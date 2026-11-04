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
}

impl Default for LlvmTextBackend {
    fn default() -> Self {
        Self {
            cc: "clang".to_string(),
            opt_level: 0,
            extra_flags: Vec::new(),
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
        std::fs::write(&ll_path, emit_llvm_ir_with(program, sanitize))
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
        Ty::Record(r) => p.records[r.0 as usize]
            .iter()
            .map(|(_, ft)| words(p, *ft))
            .sum(),
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
        let fs: Vec<String> = fields.iter().map(|(_, t)| llty(*t)).collect();
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
        // release: 参照カウントが 0 になったら、キーと値を release してから辞書を返す
        let _ = writeln!(
            out,
            "\ndefine private void @wys_release_dict{i}(ptr %p) {{\nentry:\n  %isnull = icmp eq ptr %p, null\n  br i1 %isnull, label %done, label %load\nload:\n  %rc = load i64, ptr %p, align 8\n  %n = sub i64 %rc, 1\n  store i64 %n, ptr %p, align 8\n  %zero = icmp eq i64 %n, 0\n  br i1 %zero, label %free, label %done\nfree:\n  %len = call i64 @wys_dict_len(ptr %p)\n  br label %loop\nloop:\n  %pos = phi i64 [0, %free], [%pos2, %body]\n  %e = call i64 @wys_dict_iter_next(ptr %p, i64 %pos, i64 %len)\n  %more = icmp sge i64 %e, 0\n  br i1 %more, label %body, label %fin\nbody:"
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

fn emit_glue(out: &mut String, p: &Program, drops: &DropInfo) {
    emit_dict_glue(out, p, drops);
    // リストの release: 参照カウントが 0 になったら、要素を release してからバッファを返す
    for (i, elem) in p.lists.iter().enumerate() {
        let size = elem_size(p, *elem);
        let _ = writeln!(
            out,
            "\ndefine private void @wys_release_list{i}(ptr %p) {{\nentry:\n  %isnull = icmp eq ptr %p, null\n  br i1 %isnull, label %done, label %load\nload:\n  %rc = load i64, ptr %p, align 8\n  %n = sub i64 %rc, 1\n  store i64 %n, ptr %p, align 8\n  %zero = icmp eq i64 %n, 0\n  br i1 %zero, label %free, label %done\nfree:\n  %len = call i64 @wys_list_len(ptr %p)\n  br label %loop\nloop:\n  %i = phi i64 [0, %free], [%i2, %body]\n  %more = icmp slt i64 %i, %len\n  br i1 %more, label %body, label %fin\nbody:"
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
                Inst::Retain { src } | Inst::Release { src } => {
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
}
