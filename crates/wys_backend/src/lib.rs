//! Backend トレイト（IR からオブジェクトへ）と、LLVM IR テキストを出力するバックエンド。
//!
//! `LlvmTextBackend` は LLVM IR をテキストで生成し、`clang -c` でオブジェクトファイルにする。
//! LLVM のライブラリには直接リンクしない（ADR-0004 で比較・決定する）。

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use wys_ir::arc::DropInfo;
use wys_ir::*;

#[derive(Debug)]
pub struct BackendError(pub String);

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BackendError {}

/// IR を、ランタイムとリンク可能なオブジェクトファイルにする。
pub trait Backend {
    fn name(&self) -> &'static str;
    fn emit_object(&self, program: &Program, object_path: &Path) -> Result<(), BackendError>;
}

pub struct LlvmTextBackend {
    /// LLVM IR を受け付ける C コンパイラ（既定は `clang`）。
    pub cc: String,
    /// `-O` に渡す最適化レベル（0〜3）。
    pub opt_level: u8,
    /// 追加のコンパイラオプション（`-fsanitize=address` など）。
    pub extra_flags: Vec<String>,
    /// 参照カウントの操作の回数を数える（ベンチ計測用、M3 T7）。
    pub rc_stats: bool,
}

impl Default for LlvmTextBackend {
    fn default() -> Self {
        Self {
            cc: "clang".to_string(),
            opt_level: 0,
            extra_flags: Vec::new(),
            rc_stats: false,
        }
    }
}

impl Backend for LlvmTextBackend {
    fn name(&self) -> &'static str {
        "llvm-text"
    }

    fn emit_object(&self, program: &Program, object_path: &Path) -> Result<(), BackendError> {
        let ll_path = object_path.with_extension("ll");
        let sanitize = self.extra_flags.iter().any(|f| f == "-fsanitize=address");
        let ir = emit_llvm_ir_with(program, sanitize);
        let ir = if self.rc_stats {
            with_rc_stats(&ir)
        } else {
            ir
        };
        std::fs::write(&ll_path, ir)
            .map_err(|e| BackendError(format!("cannot write {}: {e}", ll_path.display())))?;
        let output = Command::new(&self.cc)
            .arg("-c")
            .arg("-x")
            .arg("ir")
            .arg(format!("-O{}", self.opt_level))
            .args(&self.extra_flags)
            .arg("-Wno-override-module")
            .arg(&ll_path)
            .arg("-o")
            .arg(object_path)
            .output()
            .map_err(|e| BackendError(format!("cannot run `{}`: {e}", self.cc)))?;
        if !output.status.success() {
            return Err(BackendError(format!(
                "`{}` failed to compile the generated LLVM IR (this is a compiler bug):\n{}",
                self.cc,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }
}

/// ランタイム（`wys_runtime`）の C ABI 関数の宣言。シグネチャはランタイム側と一致させる。
/// ヒープオブジェクトの参照カウントの増減（ADR-0012）。ランタイムではなく生成コードの中に置く:
/// AddressSanitizer は計測付きでコンパイルしたコードのメモリアクセスしか検査しないため、
/// 解放済みのオブジェクトの参照カウントを読む誤り（release のしすぎ）をここで検出できるようにする。
/// 参照カウントはオブジェクトの先頭の i64。負なら不死。0 になったらランタイムの `wys_free_object` で解放する。
const RC_FUNCTIONS: &str = "
define private void @wys_rc_retain(ptr %p) {
entry:
  %isnull = icmp eq ptr %p, null
  br i1 %isnull, label %done, label %load
load:
  %rc = load i64, ptr %p, align 8
  %immortal = icmp slt i64 %rc, 0
  br i1 %immortal, label %done, label %inc
inc:
  ;STAT retain
  %n = add i64 %rc, 1
  store i64 %n, ptr %p, align 8
  br label %done
done:
  ret void
}

define private void @wys_rc_release(ptr %p) {
entry:
  %isnull = icmp eq ptr %p, null
  br i1 %isnull, label %done, label %load
load:
  %rc = load i64, ptr %p, align 8
  %immortal = icmp slt i64 %rc, 0
  br i1 %immortal, label %done, label %dec
dec:
  ;STAT release
  %n = sub i64 %rc, 1
  store i64 %n, ptr %p, align 8
  %zero = icmp eq i64 %n, 0
  br i1 %zero, label %free, label %done
free:
  call void @wys_free_object(ptr %p)
  br label %done
done:
  ret void
}
";

const PRELUDE: &str = "\
declare void @wys_print_int(i64)
declare void @wys_print_bool(i1 zeroext)
declare void @wys_print_str(ptr)
declare void @wys_write_int(i64)
declare void @wys_write_bool(i1 zeroext)
declare void @wys_write_str(ptr)
declare void @wys_write_bytes(ptr, i64)
declare void @wys_write_newline()
declare void @wys_free_object(ptr)
declare ptr @wys_list_new(i64, i64)
declare ptr @wys_list_push_slot(ptr, i64)
declare ptr @wys_list_slot(ptr, i64, i64)
declare i64 @wys_list_len(ptr)
declare void @wys_list_free(ptr, i64)
declare ptr @wys_dict_new(i64, i64)
declare i64 @wys_dict_len(ptr)
declare i64 @wys_dict_find(ptr, i64)
declare i64 @wys_dict_push(ptr, i64)
declare ptr @wys_dict_val(ptr, i64)
declare i64 @wys_dict_key(ptr, i64)
declare i64 @wys_dict_iter_next(ptr, i64, i64)
declare void @wys_dict_key_missing(ptr, i64) noreturn
declare void @wys_dict_free(ptr)
declare void @wys_dict_erase(ptr, i64)
declare void @wys_runtime_finish()
declare ptr @wys_str_concat(ptr, ptr)
declare zeroext i1 @wys_str_eq(ptr, ptr)
declare i64 @wys_str_len(ptr)
declare void @wys_exit(i64) noreturn
declare void @wys_panic_overflow() noreturn
declare i64 @wys_floordiv(i64, i64)
declare i64 @wys_mod(i64, i64)
declare void @wys_runtime_init(i32, ptr)
declare i64 @wys_arg_count()
declare zeroext i1 @wys_arg_exists(i64)
declare ptr @wys_arg(i64)
declare i64 @wys_scalar_kind(ptr)
declare i64 @wys_scalar_int(ptr)
declare zeroext i1 @wys_scalar_bool(ptr)
declare i64 @wys_test_index()
declare void @wys_assert_fail(ptr) noreturn
declare void @wys_panic(ptr, ptr) noreturn
declare { i64, i1 } @llvm.sadd.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.ssub.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.smul.with.overflow.i64(i64, i64)
";

fn llty(t: Ty) -> String {
    match t {
        Ty::Int => "i64".into(),
        Ty::Bool => "i1".into(),
        // ヒープオブジェクトへのポインタ（ADR-0012）
        Ty::Str => "ptr".into(),
        Ty::Union(u) => format!("%union.{}", u.0),
        Ty::Record(r) => format!("%record.{}", r.0),
        Ty::List(_) | Ty::Dict(_) => "ptr".into(),
    }
}

/// 値の大きさ（i64 の語数の上限）。すべての型が 8 バイト境界に並ぶので、語数の和で表せる。
/// `str` は ptr と長さで2語、Union はタグ1語 + 中身、レコードはフィールドの和。
fn words(p: &Program, t: Ty) -> u32 {
    match t {
        Ty::Int | Ty::Bool => 1,
        Ty::Str | Ty::List(_) | Ty::Dict(_) => 1,
        Ty::Union(u) => 1 + union_payload_words(p, u),
        // フィールドのないレコードは埋め草の 1 バイト（`type_defs`）を持つので 1 語
        Ty::Record(r) => p.records[r.0 as usize]
            .iter()
            .map(|(_, ft)| words(p, *ft))
            .sum::<u32>()
            .max(1),
    }
}

fn union_payload_words(p: &Program, u: UnionId) -> u32 {
    p.unions[u.0 as usize]
        .iter()
        .map(|m: &Member| m.map_or(0, |t| words(p, t)))
        .max()
        .unwrap_or(0)
        .max(1)
}

/// ADR-0009: Union は `{ タグ(i8), 中身([W x i64]) }`、レコードは LLVM の構造体。
fn type_defs(p: &Program) -> String {
    let mut out = String::new();
    for (i, fields) in p.records.iter().enumerate() {
        let mut fs: Vec<String> = fields.iter().map(|(_, t)| llty(*t)).collect();
        if fs.is_empty() {
            // フィールドのないレコードも大きさを持たせる。大きさ 0 の型の load / store を
            // AddressSanitizer が計装すると、実行時に落ちる（`list[E]` の要素など）
            fs.push("i8".into());
        }
        let _ = writeln!(out, "%record.{i} = type {{ {} }}", fs.join(", "));
    }
    for i in 0..p.unions.len() {
        let w = union_payload_words(p, UnionId(i as u32));
        let _ = writeln!(out, "%union.{i} = type {{ i8, [{w} x i64] }}");
    }
    out
}

fn fn_symbol(name: &str) -> String {
    format!("@wys_fn_{name}")
}

fn escape_bytes(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\' {
            out.push(b as char);
        } else {
            let _ = write!(out, "\\{b:02X}");
        }
    }
    out
}

/// IR を LLVM IR テキストに変換する。
pub fn emit_llvm_ir(p: &Program) -> String {
    emit_llvm_ir_with(p, false)
}

/// 参照カウントの操作の回数を数えるようにする（ベンチ計測用、M3 T7）。
/// 生成コードの `;STAT retain` / `;STAT release` の目印（LLVM のコメント）を、ランタイムの計数器への加算に置き換える。
/// 数えるのは、不死でないオブジェクトに対する実際の増減だけ（Union / レコードの操作は中の参照型の操作として数える）。
pub fn with_rc_stats(ir: &str) -> String {
    let mut out = ir
        .replace(
            "  ;STAT retain\n",
            "  %stat.r = atomicrmw add ptr @wys_stat_retains, i64 1 monotonic\n",
        )
        .replace(
            "  ;STAT release\n",
            "  %stat.d = atomicrmw add ptr @wys_stat_releases, i64 1 monotonic\n",
        );
    out.push_str(
        "@wys_stat_retains = external global i64\n@wys_stat_releases = external global i64\n",
    );
    out
}

/// `sanitize_address` が真なら、すべての関数に `sanitize_address` 属性を付ける。
/// clang は C のソースには自動で付けるが、LLVM IR を直接渡した場合は付かず、ASan の計測が入らないため。
pub fn emit_llvm_ir_with(p: &Program, sanitize_address: bool) -> String {
    let ir = emit_module(p);
    if !sanitize_address {
        return ir;
    }
    ir.lines()
        .map(|l| match l.strip_suffix(" {") {
            Some(head) if l.starts_with("define ") => format!("{head} sanitize_address {{"),
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn emit_module(p: &Program) -> String {
    let mut out = String::from("; generated by wys (llvm-text backend)\n\n");
    out.push_str(&type_defs(p));
    // 文字列リテラルは不死のヒープオブジェクト `{ 参照カウント -1, 長さ, バイト列 }`（ADR-0012）
    for (i, s) in p.strings.iter().enumerate() {
        let len = s.len();
        let bytes = if len == 0 {
            "zeroinitializer".to_string()
        } else {
            format!("c\"{}\"", escape_bytes(s))
        };
        let _ = writeln!(
            out,
            "@.str.{i} = private unnamed_addr constant {{ i64, i64, [{len} x i8] }} {{ i64 -1, i64 {len}, [{len} x i8] {bytes} }}, align 8"
        );
    }
    out.push('\n');
    out.push_str(PRELUDE);
    out.push_str(RC_FUNCTIONS);
    let drops = DropInfo::new(p);
    for f in &p.funcs {
        out.push('\n');
        emit_function(&mut out, p, f, &drops);
    }
    emit_glue(&mut out, p, &drops);
    let entry = &p.funcs[p.entry.0 as usize];
    let _ = write!(
        out,
        "\ndefine i32 @main(i32 %argc, ptr %argv) {{\n  call void @wys_runtime_init(i32 %argc, ptr %argv)\n  call void {}()\n  call void @wys_runtime_finish()\n  ret i32 0\n}}\n",
        fn_symbol(&entry.name)
    );
    out
}

/// 参照カウントの操作の関数名（`kind` は "retain" か "release"）。
fn glue(kind: &str, t: Ty) -> String {
    match t {
        Ty::Str => format!("@wys_rc_{kind}"),
        // リストの retain は参照カウントだけ。release は要素の解放とバッファの返却を含む型ごとの関数
        Ty::List(_) if kind == "retain" => "@wys_rc_retain".to_string(),
        Ty::List(l) => format!("@wys_release_list{}", l.0),
        Ty::Dict(_) if kind == "retain" => "@wys_rc_retain".to_string(),
        Ty::Dict(d) => format!("@wys_release_dict{}", d.0),
        Ty::Union(u) => format!("@wys_{kind}_u{}", u.0),
        Ty::Record(r) => format!("@wys_{kind}_r{}", r.0),
        Ty::Int | Ty::Bool => unreachable!("scalars have no reference counts"),
    }
}

/// Union とレコードの参照カウント操作（drop glue、ADR-0012）。Union はタグで分岐し、レコードはフィールドごとに操作する。
/// ランタイムは型の配置を知らないので、生成コード側で型ごとに作る。
/// 要素の大きさ（バイト）。すべての型が 8 バイト境界に並ぶので、語数 × 8 で上から見積もる。
fn elem_size(p: &Program, t: Ty) -> u32 {
    words(p, t) * 8
}

/// 辞書のキーの種類（ランタイムの `KEY_INT` / `KEY_BOOL` / `KEY_STR`）。
fn dict_key_kind(k: Ty) -> u32 {
    match k {
        Ty::Int => 0,
        Ty::Bool => 1,
        Ty::Str => 2,
        other => unreachable!("dict keys are int, bool or str, not {other:?}"),
    }
}

/// キーの値をランタイムに渡す i64 1語にする命令（`{dst} = ...`）。
fn key_to_raw(k: Ty, dst: &str, src: &str) -> String {
    match k {
        Ty::Int => format!("  {dst} = add i64 {src}, 0"),
        Ty::Bool => format!("  {dst} = zext i1 {src} to i64"),
        Ty::Str => format!("  {dst} = ptrtoint ptr {src} to i64"),
        other => unreachable!("dict keys are int, bool or str, not {other:?}"),
    }
}

/// ランタイムから受け取った i64 1語をキーの値にする命令。
fn raw_to_key(k: Ty, dst: &str, src: &str) -> String {
    match k {
        Ty::Int => format!("  {dst} = add i64 {src}, 0"),
        Ty::Bool => format!("  {dst} = icmp ne i64 {src}, 0"),
        Ty::Str => format!("  {dst} = inttoptr i64 {src} to ptr"),
        other => unreachable!("dict keys are int, bool or str, not {other:?}"),
    }
}

/// 辞書の型ごとの関数（ADR-0015）: 追加・置き換え（`set`）、読み出し（`get`）、release。
/// キーと値の参照カウントの操作は、型を知っている生成コードが行う。
fn emit_dict_glue(out: &mut String, p: &Program, drops: &DropInfo) {
    for (i, &(k, v)) in p.dicts.iter().enumerate() {
        let (kt, vt) = (llty(k), llty(v));
        let key_drop = drops.get(k);
        let val_drop = drops.get(v);
        // set: 既存のキーなら値を置き換えて古い値と新しいキーを release、なければ追加する
        let _ = writeln!(
            out,
            "\ndefine private void @wys_dict_set{i}(ptr %d, {kt} %k, {vt} %v) {{\nentry:\n{}\n  %e = call i64 @wys_dict_find(ptr %d, i64 %raw)\n  %found = icmp sge i64 %e, 0\n  br i1 %found, label %replace, label %insert\nreplace:\n  %s = call ptr @wys_dict_val(ptr %d, i64 %e)",
            key_to_raw(k, "%raw", "%k")
        );
        if val_drop {
            let _ = writeln!(
                out,
                "  %old = load {vt}, ptr %s\n  store {vt} %v, ptr %s\n  call void {}({vt} %old)",
                glue("release", v)
            );
        } else {
            let _ = writeln!(out, "  store {vt} %v, ptr %s");
        }
        if key_drop {
            let _ = writeln!(out, "  call void {}({kt} %k)", glue("release", k));
        }
        let _ = writeln!(
            out,
            "  ret void\ninsert:\n  %e2 = call i64 @wys_dict_push(ptr %d, i64 %raw)\n  %s2 = call ptr @wys_dict_val(ptr %d, i64 %e2)\n  store {vt} %v, ptr %s2\n  ret void\n}}"
        );
        // get: キーがなければパニック
        let _ = writeln!(
            out,
            "\ndefine private {vt} @wys_dict_get{i}(ptr %d, {kt} %k) {{\nentry:\n{}\n  %e = call i64 @wys_dict_find(ptr %d, i64 %raw)\n  %found = icmp sge i64 %e, 0\n  br i1 %found, label %ok, label %missing\nmissing:\n  call void @wys_dict_key_missing(ptr %d, i64 %raw)\n  unreachable\nok:\n  %s = call ptr @wys_dict_val(ptr %d, i64 %e)\n  %v = load {vt}, ptr %s\n  ret {vt} %v\n}}",
            key_to_raw(k, "%raw", "%k")
        );
        // pop: 値の所有を呼び出し側に移し、辞書が持っていたキーを release してから削除する
        let _ = writeln!(
            out,
            "\ndefine private {vt} @wys_dict_pop{i}(ptr %d, {kt} %k) {{\nentry:\n{}\n  %e = call i64 @wys_dict_find(ptr %d, i64 %raw)\n  %found = icmp sge i64 %e, 0\n  br i1 %found, label %ok, label %missing\nmissing:\n  call void @wys_dict_key_missing(ptr %d, i64 %raw)\n  unreachable\nok:\n  %s = call ptr @wys_dict_val(ptr %d, i64 %e)\n  %v = load {vt}, ptr %s",
            key_to_raw(k, "%raw", "%k")
        );
        if key_drop {
            let _ = writeln!(
                out,
                "  %kr = call i64 @wys_dict_key(ptr %d, i64 %e)\n{}\n  call void @wys_dict_erase(ptr %d, i64 %e)\n  call void {}({kt} %stored)",
                raw_to_key(k, "%stored", "%kr"),
                glue("release", k)
            );
        } else {
            let _ = writeln!(out, "  call void @wys_dict_erase(ptr %d, i64 %e)");
        }
        let _ = writeln!(out, "  ret {vt} %v\n}}");
        // release: 参照カウントが 0 になったら、キーと値を release してから辞書を返す
        let _ = writeln!(
            out,
            "\ndefine private void @wys_release_dict{i}(ptr %p) {{\nentry:\n  %isnull = icmp eq ptr %p, null\n  br i1 %isnull, label %done, label %load\nload:\n  ;STAT release\n  %rc = load i64, ptr %p, align 8\n  %n = sub i64 %rc, 1\n  store i64 %n, ptr %p, align 8\n  %zero = icmp eq i64 %n, 0\n  br i1 %zero, label %free, label %done\nfree:\n  %len = call i64 @wys_dict_len(ptr %p)\n  br label %loop\nloop:\n  %pos = phi i64 [0, %free], [%pos2, %body]\n  %e = call i64 @wys_dict_iter_next(ptr %p, i64 %pos, i64 %len)\n  %more = icmp sge i64 %e, 0\n  br i1 %more, label %body, label %fin\nbody:"
        );
        if key_drop {
            let _ = writeln!(
                out,
                "  %kr = call i64 @wys_dict_key(ptr %p, i64 %e)\n{}\n  call void {}({kt} %key)",
                raw_to_key(k, "%key", "%kr"),
                glue("release", k)
            );
        }
        if val_drop {
            let _ = writeln!(
                out,
                "  %s = call ptr @wys_dict_val(ptr %p, i64 %e)\n  %x = load {vt}, ptr %s\n  call void {}({vt} %x)",
                glue("release", v)
            );
        }
        let _ = writeln!(
            out,
            "  %pos2 = add i64 %e, 1\n  br label %loop\nfin:\n  call void @wys_dict_free(ptr %p)\n  br label %done\ndone:\n  ret void\n}}"
        );
    }
}

/// 型の値を比べる命令（`{dst} = ...`、結果は i1）。`int` / `bool` はその場で、ほかは型ごとの関数を呼ぶ（ADR-0016）。
fn eq_inst(t: Ty, dst: &str, a: &str, b: &str) -> String {
    match t {
        Ty::Int | Ty::Bool => format!("  {dst} = icmp eq {} {a}, {b}", llty(t)),
        Ty::Str => format!("  {dst} = call zeroext i1 @wys_str_eq(ptr {a}, ptr {b})"),
        Ty::List(l) => format!("  {dst} = call i1 @wys_eq_list{}(ptr {a}, ptr {b})", l.0),
        Ty::Dict(d) => format!("  {dst} = call i1 @wys_eq_dict{}(ptr {a}, ptr {b})", d.0),
        Ty::Union(u) => format!(
            "  {dst} = call i1 @wys_eq_u{}({u} {a}, {u} {b})",
            u.0,
            u = llty(t)
        ),
        Ty::Record(r) => format!(
            "  {dst} = call i1 @wys_eq_r{}({r} {a}, {r} {b})",
            r.0,
            r = llty(t)
        ),
    }
}

/// `==` / `!=` で比べる型と、その構成要素の型（比較関数を作る必要があるもの）。
fn eq_types(p: &Program) -> Vec<Ty> {
    let mut work: Vec<Ty> = Vec::new();
    for f in &p.funcs {
        for b in &f.blocks {
            for inst in &b.insts {
                if let Inst::Binary {
                    op: BinOp::Eq | BinOp::Ne,
                    lhs,
                    ..
                } = inst
                {
                    work.push(f.temps[lhs.0 as usize]);
                }
            }
        }
    }
    let mut seen: Vec<Ty> = Vec::new();
    while let Some(t) = work.pop() {
        if matches!(t, Ty::Int | Ty::Bool | Ty::Str) || seen.contains(&t) {
            continue;
        }
        seen.push(t);
        match t {
            Ty::List(l) => work.push(p.lists[l.0 as usize]),
            Ty::Dict(d) => work.push(p.dicts[d.0 as usize].1),
            Ty::Union(u) => work.extend(p.unions[u.0 as usize].iter().flatten()),
            Ty::Record(r) => work.extend(p.records[r.0 as usize].iter().map(|(_, t)| *t)),
            Ty::Int | Ty::Bool | Ty::Str => {}
        }
    }
    seen
}

/// 構造を持つ値の型ごとの比較関数（ADR-0016 論点 12・14）。被演算子は借用で読み、参照カウントは変えない。
fn emit_eq_glue(out: &mut String, p: &Program) {
    for t in eq_types(p) {
        match t {
            Ty::List(l) => {
                let elem = p.lists[l.0 as usize];
                let (size, lt) = (elem_size(p, elem), llty(elem));
                let _ = writeln!(
                    out,
                    "\ndefine private i1 @wys_eq_list{i}(ptr %a, ptr %b) {{\nentry:\n  %la = call i64 @wys_list_len(ptr %a)\n  %lb = call i64 @wys_list_len(ptr %b)\n  %same = icmp eq i64 %la, %lb\n  br i1 %same, label %loop, label %no\nloop:\n  %i = phi i64 [0, %entry], [%i2, %next]\n  %more = icmp slt i64 %i, %la\n  br i1 %more, label %body, label %yes\nbody:\n  %sa = call ptr @wys_list_slot(ptr %a, i64 %i, i64 {size})\n  %sb = call ptr @wys_list_slot(ptr %b, i64 %i, i64 {size})\n  %xa = load {lt}, ptr %sa\n  %xb = load {lt}, ptr %sb\n{eq}\n  br i1 %e, label %next, label %no\nnext:\n  %i2 = add i64 %i, 1\n  br label %loop\nyes:\n  ret i1 1\nno:\n  ret i1 0\n}}",
                    i = l.0,
                    eq = eq_inst(elem, "%e", "%xa", "%xb")
                );
            }
            Ty::Dict(d) => {
                let v = p.dicts[d.0 as usize].1;
                let vt = llty(v);
                // 同じ数のキーを持ち、a の各キーが b にあって値が等しい（順序は問わない）
                let _ = writeln!(
                    out,
                    "\ndefine private i1 @wys_eq_dict{i}(ptr %a, ptr %b) {{\nentry:\n  %la = call i64 @wys_dict_len(ptr %a)\n  %lb = call i64 @wys_dict_len(ptr %b)\n  %same = icmp eq i64 %la, %lb\n  br i1 %same, label %loop, label %no\nloop:\n  %pos = phi i64 [0, %entry], [%pos2, %next]\n  %ea = call i64 @wys_dict_iter_next(ptr %a, i64 %pos, i64 %la)\n  %more = icmp sge i64 %ea, 0\n  br i1 %more, label %body, label %yes\nbody:\n  %kr = call i64 @wys_dict_key(ptr %a, i64 %ea)\n  %eb = call i64 @wys_dict_find(ptr %b, i64 %kr)\n  %found = icmp sge i64 %eb, 0\n  br i1 %found, label %cmp, label %no\ncmp:\n  %sa = call ptr @wys_dict_val(ptr %a, i64 %ea)\n  %sb = call ptr @wys_dict_val(ptr %b, i64 %eb)\n  %xa = load {vt}, ptr %sa\n  %xb = load {vt}, ptr %sb\n{eq}\n  br i1 %e, label %next, label %no\nnext:\n  %pos2 = add i64 %ea, 1\n  br label %loop\nyes:\n  ret i1 1\nno:\n  ret i1 0\n}}",
                    i = d.0,
                    eq = eq_inst(v, "%e", "%xa", "%xb")
                );
            }
            Ty::Union(u) => {
                let ut = llty(t);
                let _ = writeln!(
                    out,
                    "\ndefine private i1 @wys_eq_u{i}({ut} %a, {ut} %b) {{\nentry:\n  %sa = alloca {ut}\n  store {ut} %a, ptr %sa\n  %sb = alloca {ut}\n  store {ut} %b, ptr %sb\n  %tpa = getelementptr inbounds {ut}, ptr %sa, i32 0, i32 0\n  %ta = load i8, ptr %tpa\n  %tpb = getelementptr inbounds {ut}, ptr %sb, i32 0, i32 0\n  %tb = load i8, ptr %tpb\n  %pa = getelementptr inbounds {ut}, ptr %sa, i32 0, i32 1\n  %pb = getelementptr inbounds {ut}, ptr %sb, i32 0, i32 1\n  %same = icmp eq i8 %ta, %tb\n  br i1 %same, label %c0, label %no",
                    i = u.0
                );
                // 同じタグなら payload を比べる（`None` の構成要素は payload がないので等しい）
                let cases: Vec<(usize, Ty)> = p.unions[u.0 as usize]
                    .iter()
                    .enumerate()
                    .filter_map(|(j, m)| m.map(|t| (j, t)))
                    .collect();
                for (k, (j, mt)) in cases.iter().enumerate() {
                    let lt = llty(*mt);
                    let _ = writeln!(
                        out,
                        "c{k}:\n  %m{k} = icmp eq i8 %ta, {j}\n  br i1 %m{k}, label %d{k}, label %c{n}\nd{k}:\n  %xa{k} = load {lt}, ptr %pa\n  %xb{k} = load {lt}, ptr %pb\n{eq}\n  ret i1 %e{k}",
                        n = k + 1,
                        eq = eq_inst(
                            *mt,
                            &format!("%e{k}"),
                            &format!("%xa{k}"),
                            &format!("%xb{k}")
                        )
                    );
                }
                let _ = writeln!(out, "c{}:\n  ret i1 1\nno:\n  ret i1 0\n}}", cases.len());
            }
            Ty::Record(r) => {
                let rt = llty(t);
                let _ = writeln!(
                    out,
                    "\ndefine private i1 @wys_eq_r{i}({rt} %a, {rt} %b) {{\nentry:",
                    i = r.0
                );
                let mut acc = "1".to_string();
                for (k, (_, ft)) in p.records[r.0 as usize].iter().enumerate() {
                    let _ = writeln!(
                        out,
                        "  %a{k} = extractvalue {rt} %a, {k}\n  %b{k} = extractvalue {rt} %b, {k}\n{eq}\n  %acc{k} = and i1 {acc}, %e{k}",
                        eq = eq_inst(*ft, &format!("%e{k}"), &format!("%a{k}"), &format!("%b{k}"))
                    );
                    acc = format!("%acc{k}");
                }
                let _ = writeln!(out, "  ret i1 {acc}\n}}");
            }
            Ty::Int | Ty::Bool | Ty::Str => {}
        }
    }
}

/// `print` で表示する構造を持つ型と、その構成要素の型（表示関数を作る必要があるもの）。
fn show_types(p: &Program) -> Vec<Ty> {
    let mut work: Vec<Ty> = Vec::new();
    for f in &p.funcs {
        for b in &f.blocks {
            for inst in &b.insts {
                if let Inst::Call {
                    callee: Callee::Builtin(Builtin::PrintValue),
                    args,
                    ..
                } = inst
                {
                    work.push(f.temps[args[0].0 as usize]);
                }
            }
        }
    }
    let mut seen: Vec<Ty> = Vec::new();
    while let Some(t) = work.pop() {
        if matches!(t, Ty::Int | Ty::Bool | Ty::Str) || seen.contains(&t) {
            continue;
        }
        seen.push(t);
        match t {
            Ty::List(l) => work.push(p.lists[l.0 as usize]),
            Ty::Dict(d) => {
                let (k, v) = p.dicts[d.0 as usize];
                work.extend([k, v]);
            }
            Ty::Union(u) => work.extend(p.unions[u.0 as usize].iter().flatten()),
            Ty::Record(r) => work.extend(p.records[r.0 as usize].iter().map(|(_, t)| *t)),
            Ty::Int | Ty::Bool | Ty::Str => {}
        }
    }
    seen
}

/// 表示関数が書く定数のバイト列（`[`、`P(`、`x=` など）。同じ内容は1つにまとめる。
#[derive(Default)]
struct Texts(Vec<String>);

impl Texts {
    /// `text` を書き出す命令。
    fn write(&mut self, text: &str) -> String {
        let i = match self.0.iter().position(|t| t == text) {
            Some(i) => i,
            None => {
                self.0.push(text.to_string());
                self.0.len() - 1
            }
        };
        format!(
            "  call void @wys_write_bytes(ptr @.txt.{i}, i64 {})",
            text.len()
        )
    }

    fn emit(&self, out: &mut String) {
        for (i, t) in self.0.iter().enumerate() {
            let _ = writeln!(
                out,
                "@.txt.{i} = private unnamed_addr constant [{} x i8] c\"{}\"",
                t.len(),
                escape_bytes(t)
            );
        }
    }
}

/// 構造の中の値を、改行せずに書き出す命令（ADR-0016 論点 16）。文字列は引用符で囲む。
fn show_inst(t: Ty, v: &str) -> String {
    match t {
        Ty::Int => format!("  call void @wys_write_int(i64 {v})"),
        Ty::Bool => format!("  call void @wys_write_bool(i1 zeroext {v})"),
        Ty::Str => format!("  call void @wys_write_str(ptr {v})"),
        Ty::List(l) => format!("  call void @wys_show_list{}(ptr {v})", l.0),
        Ty::Dict(d) => format!("  call void @wys_show_dict{}(ptr {v})", d.0),
        Ty::Union(u) => format!("  call void @wys_show_u{}({} {v})", u.0, llty(t)),
        Ty::Record(r) => format!("  call void @wys_show_r{}({} {v})", r.0, llty(t)),
    }
}

/// 構造を持つ値の型ごとの表示関数（ADR-0016 論点 16・17）。値は借用で読み、参照カウントは変えない。
fn emit_show_glue(out: &mut String, p: &Program) {
    let mut texts = Texts::default();
    for t in show_types(p) {
        match t {
            Ty::List(l) => {
                let elem = p.lists[l.0 as usize];
                let (size, lt) = (elem_size(p, elem), llty(elem));
                let _ = writeln!(
                    out,
                    "\ndefine private void @wys_show_list{i}(ptr %p) {{\nentry:\n{open}\n  %len = call i64 @wys_list_len(ptr %p)\n  br label %loop\nloop:\n  %i = phi i64 [0, %entry], [%i2, %elem]\n  %more = icmp slt i64 %i, %len\n  br i1 %more, label %body, label %fin\nbody:\n  %first = icmp eq i64 %i, 0\n  br i1 %first, label %elem, label %sep\nsep:\n{sep}\n  br label %elem\nelem:\n  %s = call ptr @wys_list_slot(ptr %p, i64 %i, i64 {size})\n  %x = load {lt}, ptr %s\n{show}\n  %i2 = add i64 %i, 1\n  br label %loop\nfin:\n{close}\n  ret void\n}}",
                    i = l.0,
                    open = texts.write("["),
                    sep = texts.write(", "),
                    show = show_inst(elem, "%x"),
                    close = texts.write("]"),
                );
            }
            Ty::Dict(d) => {
                let (k, v) = p.dicts[d.0 as usize];
                let vt = llty(v);
                // 挿入順（ADR-0015）
                let _ = writeln!(
                    out,
                    "\ndefine private void @wys_show_dict{i}(ptr %p) {{\nentry:\n{open}\n  %len = call i64 @wys_dict_len(ptr %p)\n  br label %loop\nloop:\n  %pos = phi i64 [0, %entry], [%pos2, %elem]\n  %first = phi i1 [1, %entry], [0, %elem]\n  %e = call i64 @wys_dict_iter_next(ptr %p, i64 %pos, i64 %len)\n  %more = icmp sge i64 %e, 0\n  br i1 %more, label %body, label %fin\nbody:\n  br i1 %first, label %elem, label %sep\nsep:\n{sep}\n  br label %elem\nelem:\n  %kr = call i64 @wys_dict_key(ptr %p, i64 %e)\n{key}\n{show_k}\n{colon}\n  %s = call ptr @wys_dict_val(ptr %p, i64 %e)\n  %x = load {vt}, ptr %s\n{show_v}\n  %pos2 = add i64 %e, 1\n  br label %loop\nfin:\n{close}\n  ret void\n}}",
                    i = d.0,
                    open = texts.write("{"),
                    sep = texts.write(", "),
                    key = raw_to_key(k, "%key", "%kr"),
                    show_k = show_inst(k, "%key"),
                    colon = texts.write(": "),
                    show_v = show_inst(v, "%x"),
                    close = texts.write("}"),
                );
            }
            Ty::Union(u) => {
                let ut = llty(t);
                let members = &p.unions[u.0 as usize];
                // `Result` は `Ok(...)` / `Err(...)`、ほかの Union は構成要素の値をそのまま書く
                let wrap: [(&str, &str); 2] = if p.display.results.contains(&u) {
                    [("Ok(", ")"), ("Err(", ")")]
                } else {
                    [("", ""); 2]
                };
                let _ = writeln!(
                    out,
                    "\ndefine private void @wys_show_u{i}({ut} %v) {{\nentry:\n  %sv = alloca {ut}\n  store {ut} %v, ptr %sv\n  %tp = getelementptr inbounds {ut}, ptr %sv, i32 0, i32 0\n  %tag = load i8, ptr %tp\n  %pp = getelementptr inbounds {ut}, ptr %sv, i32 0, i32 1\n  br label %c0",
                    i = u.0
                );
                for (j, m) in members.iter().enumerate() {
                    let (pre, post) = wrap.get(j).copied().unwrap_or(("", ""));
                    let mut body = Vec::new();
                    if !pre.is_empty() {
                        body.push(texts.write(pre));
                    }
                    match m {
                        Some(mt) => {
                            body.push(format!("  %x{j} = load {}, ptr %pp", llty(*mt)));
                            body.push(show_inst(*mt, &format!("%x{j}")));
                        }
                        None => body.push(texts.write("None")),
                    }
                    if !post.is_empty() {
                        body.push(texts.write(post));
                    }
                    let _ = writeln!(
                        out,
                        "c{j}:\n  %m{j} = icmp eq i8 %tag, {j}\n  br i1 %m{j}, label %d{j}, label %c{n}\nd{j}:\n{}\n  ret void",
                        body.join("\n"),
                        n = j + 1
                    );
                }
                let _ = writeln!(out, "c{}:\n  unreachable\n}}", members.len());
            }
            Ty::Record(r) => {
                let rt = llty(t);
                let name = &p.display.record_names[r.0 as usize];
                let _ = writeln!(
                    out,
                    "\ndefine private void @wys_show_r{i}({rt} %v) {{\nentry:\n{open}",
                    i = r.0,
                    open = texts.write(&format!("{name}("))
                );
                // フィールドは名前の順に並んでいる（ADR-0016 論点 16）
                for (k, (fname, ft)) in p.records[r.0 as usize].iter().enumerate() {
                    let label = if k == 0 {
                        format!("{fname}=")
                    } else {
                        format!(", {fname}=")
                    };
                    let _ = writeln!(
                        out,
                        "{}\n  %f{k} = extractvalue {rt} %v, {k}\n{}",
                        texts.write(&label),
                        show_inst(*ft, &format!("%f{k}"))
                    );
                }
                let _ = writeln!(out, "{}\n  ret void\n}}", texts.write(")"));
            }
            Ty::Int | Ty::Bool | Ty::Str => {}
        }
    }
    texts.emit(out);
}

fn emit_glue(out: &mut String, p: &Program, drops: &DropInfo) {
    emit_dict_glue(out, p, drops);
    emit_eq_glue(out, p);
    emit_show_glue(out, p);
    // リストの release: 参照カウントが 0 になったら、要素を release してからバッファを返す
    for (i, elem) in p.lists.iter().enumerate() {
        let size = elem_size(p, *elem);
        let _ = writeln!(
            out,
            "\ndefine private void @wys_release_list{i}(ptr %p) {{\nentry:\n  %isnull = icmp eq ptr %p, null\n  br i1 %isnull, label %done, label %load\nload:\n  ;STAT release\n  %rc = load i64, ptr %p, align 8\n  %n = sub i64 %rc, 1\n  store i64 %n, ptr %p, align 8\n  %zero = icmp eq i64 %n, 0\n  br i1 %zero, label %free, label %done\nfree:\n  %len = call i64 @wys_list_len(ptr %p)\n  br label %loop\nloop:\n  %i = phi i64 [0, %free], [%i2, %body]\n  %more = icmp slt i64 %i, %len\n  br i1 %more, label %body, label %fin\nbody:"
        );
        if drops.get(*elem) {
            let lt = llty(*elem);
            let _ = writeln!(
                out,
                "  %s = call ptr @wys_list_slot(ptr %p, i64 %i, i64 {size})\n  %x = load {lt}, ptr %s\n  call void {}({lt} %x)",
                glue("release", *elem)
            );
        }
        let _ = writeln!(
            out,
            "  %i2 = add i64 %i, 1\n  br label %loop\nfin:\n  call void @wys_list_free(ptr %p, i64 {size})\n  br label %done\ndone:\n  ret void\n}}"
        );
    }
    for kind in ["retain", "release"] {
        for (i, members) in p.unions.iter().enumerate() {
            let uty = Ty::Union(UnionId(i as u32));
            if !drops.get(uty) {
                continue;
            }
            let u = llty(uty);
            let _ = writeln!(
                out,
                "\ndefine private void {}({u} %v) {{\nentry:\n  %s = alloca {u}\n  store {u} %v, ptr %s\n  %tp = getelementptr inbounds {u}, ptr %s, i32 0, i32 0\n  %tag = load i8, ptr %tp\n  %pl = getelementptr inbounds {u}, ptr %s, i32 0, i32 1\n  br label %c0",
                glue(kind, uty)
            );
            let cases: Vec<(usize, Ty)> = members
                .iter()
                .enumerate()
                .filter_map(|(j, m)| m.filter(|t| drops.get(*t)).map(|t| (j, t)))
                .collect();
            for (k, (j, t)) in cases.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "c{k}:\n  %e{k} = icmp eq i8 %tag, {j}\n  br i1 %e{k}, label %d{k}, label %c{}\nd{k}:\n  %x{k} = load {lt}, ptr %pl\n  call void {g}({lt} %x{k})\n  ret void",
                    k + 1,
                    lt = llty(*t),
                    g = glue(kind, *t)
                );
            }
            let _ = writeln!(out, "c{}:\n  ret void\n}}", cases.len());
        }
        for (i, fields) in p.records.iter().enumerate() {
            let rty = Ty::Record(RecordId(i as u32));
            if !drops.get(rty) {
                continue;
            }
            let r = llty(rty);
            let _ = writeln!(out, "\ndefine private void {}({r} %v) {{", glue(kind, rty));
            for (k, (_, ft)) in fields.iter().enumerate() {
                if drops.get(*ft) {
                    let lt = llty(*ft);
                    let _ = writeln!(
                        out,
                        "  %f{k} = extractvalue {r} %v, {k}\n  call void {}({lt} %f{k})",
                        glue(kind, *ft)
                    );
                }
            }
            out.push_str("  ret void\n}\n");
        }
    }
}

fn emit_function(out: &mut String, p: &Program, f: &Function, drops: &DropInfo) {
    let ret = f.ret.map_or("void".to_string(), llty);
    let params: Vec<String> = (0..f.param_count as usize)
        .map(|i| format!("{} %p{i}", llty(f.locals[i])))
        .collect();
    let _ = writeln!(
        out,
        "define {ret} {}({}) {{",
        fn_symbol(&f.name),
        params.join(", ")
    );
    out.push_str("entry:\n");
    for (i, t) in f.locals.iter().enumerate() {
        let _ = writeln!(out, "  %l{i} = alloca {}", llty(*t));
    }
    for i in 0..f.param_count as usize {
        let _ = writeln!(out, "  store {} %p{i}, ptr %l{i}", llty(f.locals[i]));
    }
    // Union の命令が使う作業領域。ループ内でスタックが伸びないよう、入口でまとめて確保する。
    for b in &f.blocks {
        for inst in &b.insts {
            let (dst, ty) = match inst {
                Inst::MakeUnion { dst, union, .. } => (*dst, Ty::Union(*union)),
                Inst::UnionIs { dst, src, .. } | Inst::UnionPayload { dst, src, .. } => {
                    (*dst, f.temps[src.0 as usize])
                }
                _ => continue,
            };
            let _ = writeln!(out, "  %t{}.slot = alloca {}", dst.0, llty(ty));
        }
    }
    // 参照型を持つローカルはゼロ（null）で始める。ARC は store で古い値を release するため（null は無視される）
    for (i, t) in f.locals.iter().enumerate().skip(f.param_count as usize) {
        if drops.get(*t) {
            let zero = if *t == Ty::Str {
                "null"
            } else {
                "zeroinitializer"
            };
            let _ = writeln!(out, "  store {} {zero}, ptr %l{i}", llty(*t));
        }
    }
    out.push_str("  br label %bb0\n");

    let mut fresh = 0u32;
    let mut uses_overflow = false;
    let t = |id: TempId| format!("%t{}", id.0);
    let tty = |id: TempId| llty(f.temps[id.0 as usize]);

    for (bi, b) in f.blocks.iter().enumerate() {
        let _ = writeln!(out, "bb{bi}:");
        for inst in &b.insts {
            match inst {
                Inst::Const { dst, value } => match value {
                    Const::Int(v) => {
                        let _ = writeln!(out, "  {} = add i64 0, {v}", t(*dst));
                    }
                    Const::Bool(v) => {
                        let _ = writeln!(out, "  {} = or i1 false, {v}", t(*dst));
                    }
                    Const::Str(s) => {
                        let _ = writeln!(
                            out,
                            "  {} = getelementptr inbounds i8, ptr @.str.{}, i64 0",
                            t(*dst),
                            s.0
                        );
                    }
                },
                Inst::Load { dst, src } => {
                    let _ = writeln!(out, "  {} = load {}, ptr %l{}", t(*dst), tty(*dst), src.0);
                }
                Inst::Store { dst, src } => {
                    let _ = writeln!(out, "  store {} {}, ptr %l{}", tty(*src), t(*src), dst.0);
                }
                Inst::Not { dst, src } => {
                    let _ = writeln!(out, "  {} = xor i1 {}, true", t(*dst), t(*src));
                }
                Inst::MakeUnion {
                    dst,
                    union,
                    variant,
                    payload,
                } => {
                    let d = t(*dst);
                    let uty = llty(Ty::Union(*union));
                    let _ = writeln!(
                        out,
                        "  {d}.tag = getelementptr inbounds {uty}, ptr {d}.slot, i32 0, i32 0\n  store i8 {variant}, ptr {d}.tag"
                    );
                    if let Some(pv) = payload {
                        let _ = writeln!(
                            out,
                            "  {d}.pl = getelementptr inbounds {uty}, ptr {d}.slot, i32 0, i32 1\n  store {} {}, ptr {d}.pl",
                            tty(*pv),
                            t(*pv)
                        );
                    }
                    let _ = writeln!(out, "  {d} = load {uty}, ptr {d}.slot");
                }
                Inst::ListNew { dst, list, items } => {
                    let d = t(*dst);
                    let elem = p.lists[list.0 as usize];
                    let size = elem_size(p, elem);
                    let _ = writeln!(
                        out,
                        "  {d} = call ptr @wys_list_new(i64 {size}, i64 {})",
                        items.len()
                    );
                    for (k, it) in items.iter().enumerate() {
                        let _ = writeln!(
                            out,
                            "  {d}.s{k} = call ptr @wys_list_push_slot(ptr {d}, i64 {size})\n  store {} {}, ptr {d}.s{k}",
                            llty(elem),
                            t(*it)
                        );
                    }
                }
                Inst::DictNew {
                    dst,
                    dict,
                    keys,
                    vals,
                } => {
                    let d = t(*dst);
                    let (k, v) = p.dicts[dict.0 as usize];
                    let _ = writeln!(
                        out,
                        "  {d} = call ptr @wys_dict_new(i64 {}, i64 {})",
                        dict_key_kind(k),
                        elem_size(p, v)
                    );
                    for (key, val) in keys.iter().zip(vals) {
                        let _ = writeln!(
                            out,
                            "  call void @wys_dict_set{}(ptr {d}, {} {}, {} {})",
                            dict.0,
                            llty(k),
                            t(*key),
                            llty(v),
                            t(*val)
                        );
                    }
                }
                Inst::DictLen { dst, src } => {
                    let _ = writeln!(
                        out,
                        "  {} = call i64 @wys_dict_len(ptr {})",
                        t(*dst),
                        t(*src)
                    );
                }
                Inst::DictContains { dst, src, key } => {
                    let d = t(*dst);
                    let kty = f.temps[key.0 as usize];
                    let _ = writeln!(
                        out,
                        "{}\n  {d}.e = call i64 @wys_dict_find(ptr {}, i64 {d}.raw)\n  {d} = icmp sge i64 {d}.e, 0",
                        key_to_raw(kty, &format!("{d}.raw"), &t(*key)),
                        t(*src)
                    );
                }
                Inst::DictGet { dst, src, key } => {
                    let Ty::Dict(di) = f.temps[src.0 as usize] else {
                        unreachable!("verified")
                    };
                    let (k, v) = p.dicts[di.0 as usize];
                    let _ = writeln!(
                        out,
                        "  {} = call {} @wys_dict_get{}(ptr {}, {} {})",
                        t(*dst),
                        llty(v),
                        di.0,
                        t(*src),
                        llty(k),
                        t(*key)
                    );
                }
                Inst::DictPop { dst, src, key } => {
                    let Ty::Dict(di) = f.temps[src.0 as usize] else {
                        unreachable!("verified")
                    };
                    let (k, v) = p.dicts[di.0 as usize];
                    let _ = writeln!(
                        out,
                        "  {} = call {} @wys_dict_pop{}(ptr {}, {} {})",
                        t(*dst),
                        llty(v),
                        di.0,
                        t(*src),
                        llty(k),
                        t(*key)
                    );
                }
                Inst::DictSet { dict, key, value } => {
                    let Ty::Dict(di) = f.temps[dict.0 as usize] else {
                        unreachable!("verified")
                    };
                    let (k, v) = p.dicts[di.0 as usize];
                    let _ = writeln!(
                        out,
                        "  call void @wys_dict_set{}(ptr {}, {} {}, {} {})",
                        di.0,
                        t(*dict),
                        llty(k),
                        t(*key),
                        llty(v),
                        t(*value)
                    );
                }
                Inst::DictKeyAt { dst, src, index } => {
                    let d = t(*dst);
                    let kty = f.temps[dst.0 as usize];
                    let _ = writeln!(
                        out,
                        "  {d}.raw = call i64 @wys_dict_key(ptr {}, i64 {})\n{}",
                        t(*src),
                        t(*index),
                        raw_to_key(kty, &d, &format!("{d}.raw"))
                    );
                }
                Inst::DictIterNext {
                    dst,
                    src,
                    pos,
                    len0,
                } => {
                    let _ = writeln!(
                        out,
                        "  {} = call i64 @wys_dict_iter_next(ptr {}, i64 {}, i64 {})",
                        t(*dst),
                        t(*src),
                        t(*pos),
                        t(*len0)
                    );
                }
                Inst::ListLen { dst, src } => {
                    let _ = writeln!(
                        out,
                        "  {} = call i64 @wys_list_len(ptr {})",
                        t(*dst),
                        t(*src)
                    );
                }
                Inst::ListGet { dst, src, index } => {
                    let d = t(*dst);
                    let elem = f.temps[dst.0 as usize];
                    let _ = writeln!(
                        out,
                        "  {d}.s = call ptr @wys_list_slot(ptr {}, i64 {}, i64 {})\n  {d} = load {}, ptr {d}.s",
                        t(*src),
                        t(*index),
                        elem_size(p, elem),
                        llty(elem)
                    );
                }
                Inst::ListSet { list, index, value } => {
                    let elem = f.temps[value.0 as usize];
                    let lt = llty(elem);
                    let k = fresh;
                    fresh += 1;
                    let _ = writeln!(
                        out,
                        "  %ls{k} = call ptr @wys_list_slot(ptr {}, i64 {}, i64 {})",
                        t(*list),
                        t(*index),
                        elem_size(p, elem)
                    );
                    if drops.get(elem) {
                        let _ = writeln!(
                            out,
                            "  %lo{k} = load {lt}, ptr %ls{k}\n  store {lt} {}, ptr %ls{k}\n  call void {}({lt} %lo{k})",
                            t(*value),
                            glue("release", elem)
                        );
                    } else {
                        let _ = writeln!(out, "  store {lt} {}, ptr %ls{k}", t(*value));
                    }
                }
                Inst::ListPush { list, value } => {
                    let elem = f.temps[value.0 as usize];
                    let k = fresh;
                    fresh += 1;
                    let _ = writeln!(
                        out,
                        "  %lp{k} = call ptr @wys_list_push_slot(ptr {}, i64 {})\n  store {} {}, ptr %lp{k}",
                        t(*list),
                        elem_size(p, elem),
                        llty(elem),
                        t(*value)
                    );
                }
                Inst::Retain { src, .. } | Inst::Release { src, .. } => {
                    let ty = f.temps[src.0 as usize];
                    if drops.get(ty) {
                        let kind = if matches!(inst, Inst::Retain { .. }) {
                            "retain"
                        } else {
                            "release"
                        };
                        let _ = writeln!(
                            out,
                            "  call void {}({} {})",
                            glue(kind, ty),
                            llty(ty),
                            t(*src)
                        );
                    }
                }
                Inst::MakeRecord {
                    dst,
                    record,
                    fields,
                } => {
                    let d = t(*dst);
                    let rty = llty(Ty::Record(*record));
                    if fields.is_empty() {
                        let _ =
                            writeln!(out, "  {d} = extractvalue {{ {rty} }} zeroinitializer, 0");
                    }
                    let mut prev = "poison".to_string();
                    for (i, ft) in fields.iter().enumerate() {
                        let name = if i + 1 == fields.len() {
                            d.clone()
                        } else {
                            format!("{d}.f{i}")
                        };
                        let _ = writeln!(
                            out,
                            "  {name} = insertvalue {rty} {prev}, {} {}, {i}",
                            tty(*ft),
                            t(*ft)
                        );
                        prev = name;
                    }
                }
                Inst::Field { dst, src, index } => {
                    let _ = writeln!(
                        out,
                        "  {} = extractvalue {} {}, {index}",
                        t(*dst),
                        tty(*src),
                        t(*src)
                    );
                }
                Inst::UnionIs { dst, src, variant } => {
                    let d = t(*dst);
                    let uty = tty(*src);
                    let _ = writeln!(
                        out,
                        "  store {uty} {}, ptr {d}.slot\n  {d}.tag = getelementptr inbounds {uty}, ptr {d}.slot, i32 0, i32 0\n  {d}.tv = load i8, ptr {d}.tag\n  {d} = icmp eq i8 {d}.tv, {variant}",
                        t(*src)
                    );
                }
                Inst::UnionPayload { dst, src, .. } => {
                    let d = t(*dst);
                    let uty = tty(*src);
                    let _ = writeln!(
                        out,
                        "  store {uty} {}, ptr {d}.slot\n  {d}.pl = getelementptr inbounds {uty}, ptr {d}.slot, i32 0, i32 1\n  {d} = load {}, ptr {d}.pl",
                        t(*src),
                        tty(*dst)
                    );
                }
                Inst::Binary { dst, op, lhs, rhs } => {
                    let (d, a, bb) = (t(*dst), t(*lhs), t(*rhs));
                    let ty = tty(*lhs);
                    let ir_ty = f.temps[lhs.0 as usize];
                    if matches!(op, BinOp::Eq | BinOp::Ne) && !matches!(ir_ty, Ty::Int | Ty::Bool) {
                        // 構造を持つ値（と str）の比較は型ごとの関数で（ADR-0016）
                        if *op == BinOp::Eq {
                            let _ = writeln!(out, "{}", eq_inst(ir_ty, &d, &a, &bb));
                        } else {
                            let _ =
                                writeln!(out, "{}", eq_inst(ir_ty, &format!("{d}.eq"), &a, &bb));
                            let _ = writeln!(out, "  {d} = xor i1 {d}.eq, true");
                        }
                        continue;
                    }
                    match op {
                        BinOp::Add | BinOp::Sub | BinOp::Mul => {
                            uses_overflow = true;
                            let intr = match op {
                                BinOp::Add => "sadd",
                                BinOp::Sub => "ssub",
                                _ => "smul",
                            };
                            let k = fresh;
                            fresh += 1;
                            let _ = writeln!(
                                out,
                                "  {d}.r = call {{ i64, i1 }} @llvm.{intr}.with.overflow.i64(i64 {a}, i64 {bb})\n  {d} = extractvalue {{ i64, i1 }} {d}.r, 0\n  {d}.o = extractvalue {{ i64, i1 }} {d}.r, 1\n  br i1 {d}.o, label %overflow, label %ok{k}\nok{k}:"
                            );
                        }
                        BinOp::FloorDiv => {
                            let _ =
                                writeln!(out, "  {d} = call i64 @wys_floordiv(i64 {a}, i64 {bb})");
                        }
                        BinOp::Mod => {
                            let _ = writeln!(out, "  {d} = call i64 @wys_mod(i64 {a}, i64 {bb})");
                        }
                        cmp => {
                            let pred = match cmp {
                                BinOp::Eq => "eq",
                                BinOp::Ne => "ne",
                                BinOp::Lt => "slt",
                                BinOp::Le => "sle",
                                BinOp::Gt => "sgt",
                                BinOp::Ge => "sge",
                                _ => unreachable!(),
                            };
                            let _ = writeln!(out, "  {d} = icmp {pred} {ty} {a}, {bb}");
                        }
                    }
                }
                Inst::Call { dst, callee, args } => match callee {
                    Callee::Builtin(
                        b @ (Builtin::StrConcat | Builtin::StrEq | Builtin::StrLen),
                    ) => {
                        let d = t(dst.expect("string builtins return a value"));
                        let a: Vec<String> =
                            args.iter().map(|x| format!("ptr {}", t(*x))).collect();
                        let (ret, sym) = match b {
                            Builtin::StrConcat => ("ptr", "@wys_str_concat"),
                            Builtin::StrEq => ("zeroext i1", "@wys_str_eq"),
                            _ => ("i64", "@wys_str_len"),
                        };
                        let _ = writeln!(out, "  {d} = call {ret} {sym}({})", a.join(", "));
                    }
                    Callee::Builtin(Builtin::AssertFail) => {
                        let _ = writeln!(out, "  call void @wys_assert_fail(ptr {})", t(args[0]));
                    }
                    Callee::Builtin(Builtin::Panic) => {
                        let _ = writeln!(
                            out,
                            "  call void @wys_panic(ptr {}, ptr {})",
                            t(args[0]),
                            t(args[1])
                        );
                    }
                    Callee::Builtin(Builtin::PrintStr) => {
                        let _ = writeln!(out, "  call void @wys_print_str(ptr {})", t(args[0]));
                    }
                    Callee::Builtin(Builtin::PrintValue) => {
                        let _ = writeln!(
                            out,
                            "{}\n  call void @wys_write_newline()",
                            show_inst(f.temps[args[0].0 as usize], &t(args[0]))
                        );
                    }
                    Callee::Builtin(
                        b @ (Builtin::ArgCount
                        | Builtin::ArgExists
                        | Builtin::ArgStr
                        | Builtin::ScalarKind
                        | Builtin::ScalarInt
                        | Builtin::ScalarBool
                        | Builtin::TestIndex),
                    ) => {
                        let d = t(dst.expect("input builtins return a value"));
                        match b {
                            Builtin::ArgCount => {
                                let _ = writeln!(out, "  {d} = call i64 @wys_arg_count()");
                            }
                            Builtin::TestIndex => {
                                let _ = writeln!(out, "  {d} = call i64 @wys_test_index()");
                            }
                            Builtin::ArgExists => {
                                let _ = writeln!(
                                    out,
                                    "  {d} = call zeroext i1 @wys_arg_exists(i64 {})",
                                    t(args[0])
                                );
                            }
                            Builtin::ArgStr => {
                                let _ =
                                    writeln!(out, "  {d} = call ptr @wys_arg(i64 {})", t(args[0]));
                            }
                            _ => {
                                let (sym, ret) = match b {
                                    Builtin::ScalarKind => ("@wys_scalar_kind", "i64"),
                                    Builtin::ScalarInt => ("@wys_scalar_int", "i64"),
                                    _ => ("@wys_scalar_bool", "zeroext i1"),
                                };
                                let _ =
                                    writeln!(out, "  {d} = call {ret} {sym}(ptr {})", t(args[0]));
                            }
                        }
                    }
                    Callee::Builtin(b) => {
                        let (sym, arg_ty) = match b {
                            Builtin::PrintInt => ("@wys_print_int", "i64"),
                            Builtin::PrintBool => ("@wys_print_bool", "i1 zeroext"),
                            Builtin::Exit => ("@wys_exit", "i64"),
                            _ => unreachable!("handled by the arms above"),
                        };
                        let _ = writeln!(out, "  call void {sym}({arg_ty} {})", t(args[0]));
                    }
                    Callee::Func(id) => {
                        let g = &p.funcs[id.0 as usize];
                        let args: Vec<String> = args
                            .iter()
                            .map(|a| format!("{} {}", tty(*a), t(*a)))
                            .collect();
                        let call = format!(
                            "call {} {}({})",
                            g.ret.map_or("void".to_string(), llty),
                            fn_symbol(&g.name),
                            args.join(", ")
                        );
                        match dst {
                            Some(d) => {
                                let _ = writeln!(out, "  {} = {call}", t(*d));
                            }
                            None => {
                                let _ = writeln!(out, "  {call}");
                            }
                        }
                    }
                },
            }
        }
        match &b.term {
            Terminator::Return(None) => out.push_str("  ret void\n"),
            Terminator::Return(Some(v)) => {
                let _ = writeln!(out, "  ret {} {}", tty(*v), t(*v));
            }
            Terminator::Jump(b) => {
                let _ = writeln!(out, "  br label %bb{}", b.0);
            }
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let _ = writeln!(
                    out,
                    "  br i1 {}, label %bb{}, label %bb{}",
                    t(*cond),
                    then_block.0,
                    else_block.0
                );
            }
            Terminator::Unreachable => out.push_str("  unreachable\n"),
        }
    }
    if uses_overflow {
        out.push_str("overflow:\n  call void @wys_panic_overflow()\n  unreachable\n");
    }
    out.push_str("}\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_non_printable_and_quotes() {
        assert_eq!(escape_bytes("a\"b\\\n"), "a\\22b\\5C\\0A");
        assert_eq!(escape_bytes("あ"), "\\E3\\81\\82");
    }

    #[test]
    fn emits_entry_wrapper() {
        let p = Program {
            strings: vec![],
            unions: vec![],
            records: vec![],
            lists: vec![],
            dicts: vec![],
            display: Default::default(),
            entry: FuncId(0),
            funcs: vec![Function {
                name: "main".into(),
                param_count: 0,
                locals: vec![],
                ret: None,
                temps: vec![],
                blocks: vec![Block {
                    insts: vec![],
                    term: Terminator::Return(None),
                }],
                effects: None,
            }],
        };
        let ll = emit_llvm_ir(&p);
        assert!(ll.contains("define void @wys_fn_main()"), "{ll}");
        assert!(
            ll.contains("define i32 @main(i32 %argc, ptr %argv)"),
            "{ll}"
        );
        assert!(
            ll.contains("call void @wys_runtime_init(i32 %argc, ptr %argv)"),
            "{ll}"
        );
    }

    #[test]
    fn arc_reasons_and_effects_do_not_change_the_generated_code() {
        // def f(s: str) -> str: return s（ARC パスが retain / release を挿入する）
        let mut p = Program {
            strings: vec![],
            unions: vec![],
            records: vec![],
            lists: vec![],
            dicts: vec![],
            display: Default::default(),
            entry: FuncId(0),
            funcs: vec![Function {
                name: "f".into(),
                param_count: 1,
                locals: vec![Ty::Str],
                ret: Some(Ty::Str),
                temps: vec![Ty::Str],
                blocks: vec![Block {
                    insts: vec![Inst::Load {
                        dst: TempId(0),
                        src: LocalId(0),
                    }],
                    term: Terminator::Return(Some(TempId(0))),
                }],
                effects: None,
            }],
        };
        wys_ir::arc::insert(&mut p);
        let before = emit_llvm_ir(&p);
        assert!(before.contains("@wys_rc_"), "{before}");
        for b in &mut p.funcs[0].blocks {
            for i in &mut b.insts {
                if let Inst::Retain { reason, .. } | Inst::Release { reason, .. } = i {
                    *reason = RcReason::UnconsumedOwnedAtBlockEnd;
                }
            }
        }
        assert_eq!(before, emit_llvm_ir(&p));
        // Effect の属性（ADR-0014）も生成コードを変えない
        p.funcs[0].effects = Some(vec![Effect::Io, Effect::Exit]);
        assert_eq!(before, emit_llvm_ir(&p));
    }
}
