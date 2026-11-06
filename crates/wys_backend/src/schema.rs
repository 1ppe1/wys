//! スキーマ型の glue（ADR-0022 論点 5〜7 と訂正、M5 T6）。
//!
//! - `@wys_validate_{型}(%unknown %u, ptr %out) -> i1`: `Unknown` の木（借用）を型として検証し、成功なら `%out` に
//!   +1 所有の値を書いて 1、失敗ならゼロの値を書いて 0 を返す（ゼロの値は解放しても何もしない）。失敗した葉は
//!   `wys_schema_fail` で理由を記録し、戻りながら各段が経路の区切りを足す。作りかけのコンテナとレコードのフィールドは、
//!   失敗した段で解放する。
//! - `@wys_to_json_{型}(値)`: 値（借用）を `wys_json_*` のバッファに書く。
//! - `json_schema`: 型の JSON Schema（draft 2020-12 の部分集合）。`llm_generate` とツール表（T8 / T9）が使う。

use std::fmt::Write as _;

use wys_ir::arc::DropInfo;
use wys_ir::*;

use super::{elem_size, escape_bytes, glue, llty};

/// glue が使う定数（静的なバイト列と、不死の文字列オブジェクト）。
#[derive(Default)]
struct Consts {
    bytes: Vec<String>,
    strs: Vec<String>,
}

impl Consts {
    fn byte(&mut self, text: &str) -> (String, usize) {
        let i = match self.bytes.iter().position(|t| t == text) {
            Some(i) => i,
            None => {
                self.bytes.push(text.to_string());
                self.bytes.len() - 1
            }
        };
        (format!("@.jtxt.{i}"), text.len())
    }

    /// 不死の文字列オブジェクト（`dict` のキーとして `wys_dict_find` に渡す）。
    fn str_obj(&mut self, text: &str) -> String {
        let i = match self.strs.iter().position(|t| t == text) {
            Some(i) => i,
            None => {
                self.strs.push(text.to_string());
                self.strs.len() - 1
            }
        };
        format!("@.jkey.{i}")
    }

    fn json_bytes(&mut self, text: &str) -> String {
        let (g, n) = self.byte(text);
        format!("  call void @wys_json_bytes(ptr {g}, i64 {n})")
    }

    fn emit(&self, out: &mut String) {
        for (i, t) in self.bytes.iter().enumerate() {
            let _ = writeln!(
                out,
                "@.jtxt.{i} = private unnamed_addr constant [{} x i8] c\"{}\"",
                t.len().max(1),
                if t.is_empty() {
                    "\\00".into()
                } else {
                    escape_bytes(t)
                }
            );
        }
        for (i, t) in self.strs.iter().enumerate() {
            let n = t.len();
            let _ = writeln!(
                out,
                "@.jkey.{i} = private unnamed_addr constant {{ i64, i64, [{n} x i8] }} {{ i64 -1, i64 {n}, [{n} x i8] c\"{}\" }}, align 8",
                escape_bytes(t)
            );
        }
    }
}

/// 失敗のときの `expected` の表記（ADR-0022 の訂正。Generic なレコードは型引数なしの名前）。
pub fn type_display(p: &Program, t: Ty) -> String {
    match t {
        Ty::Int => "int".into(),
        Ty::Bool => "bool".into(),
        Ty::Str => "str".into(),
        Ty::Float => "float".into(),
        Ty::Unknown => "Unknown".into(),
        Ty::List(l) => format!("list[{}]", type_display(p, p.lists[l.0 as usize])),
        Ty::Dict(d) => {
            let (_, v) = p.dicts[d.0 as usize];
            format!("dict[str, {}]", type_display(p, v))
        }
        Ty::Record(r) => p.display.record_names[r.0 as usize].clone(),
        Ty::Union(u) => p.unions[u.0 as usize]
            .iter()
            .map(|m| m.map_or("None".into(), |t| type_display(p, t)))
            .collect::<Vec<_>>()
            .join(" | "),
        Ty::Py | Ty::Task(_) => unreachable!("not a schema type (E1501)"),
    }
}

/// 型と、その構成要素の型（glue を作るもの）。
fn closure(p: &Program, roots: Vec<Ty>) -> Vec<Ty> {
    let mut work = roots;
    let mut seen: Vec<Ty> = Vec::new();
    while let Some(t) = work.pop() {
        if seen.contains(&t) {
            continue;
        }
        seen.push(t);
        match t {
            Ty::List(l) => work.push(p.lists[l.0 as usize]),
            Ty::Dict(d) => work.push(p.dicts[d.0 as usize].1),
            Ty::Union(u) => work.extend(p.unions[u.0 as usize].iter().flatten()),
            Ty::Record(r) => work.extend(p.records[r.0 as usize].iter().map(|(_, t)| *t)),
            _ => {}
        }
    }
    seen
}

fn calls(p: &Program, b: Builtin) -> Vec<Ty> {
    let mut out = Vec::new();
    for f in &p.funcs {
        for blk in &f.blocks {
            for inst in &blk.insts {
                if let Inst::Call {
                    dst,
                    callee: Callee::Builtin(c),
                    args,
                } = inst
                    && *c == b
                {
                    let t = match b {
                        Builtin::Validate => dst.map(|d| f.temps[d.0 as usize]),
                        _ => args.first().map(|a| f.temps[a.0 as usize]),
                    };
                    out.extend(t);
                }
            }
        }
    }
    out
}

/// `validate` の glue の名前。
pub fn validate_fn(t: Ty) -> String {
    format!("@wys_validate_{}", t.name())
}

/// `to_json` で値を書く命令。
pub fn to_json_inst(t: Ty, v: &str) -> String {
    match t {
        Ty::Int => format!("  call void @wys_json_int(i64 {v})"),
        Ty::Bool => format!("  call void @wys_json_bool(i1 zeroext {v})"),
        Ty::Float => format!("  call void @wys_json_float(double {v})"),
        Ty::Str => format!("  call void @wys_json_str(ptr {v})"),
        _ => format!("  call void @wys_to_json_{}({} {v})", t.name(), llty(t)),
    }
}

/// `%out` にゼロを書いて 0 を返す（失敗の出口）。
fn fail_exit(t: Ty) -> String {
    format!("  store {} zeroinitializer, ptr %out\n  ret i1 0", llty(t))
}

/// 葉の不一致を記録する命令。`found` は i64 の値（タグか 7 = missing）。
fn record_fail(c: &mut Consts, expected: &str, found: &str) -> String {
    let (g, n) = c.byte(expected);
    format!("  call void @wys_schema_fail(ptr {g}, i64 {n}, i64 {found})")
}

/// `validate` と `to_json` の glue を書く。
pub fn emit_schema_glue(out: &mut String, p: &Program, drops: &DropInfo) {
    let mut c = Consts::default();
    let vtypes = closure(p, calls(p, Builtin::Validate));
    for t in &vtypes {
        emit_validate(out, p, drops, &mut c, *t);
    }
    let jtypes = closure(p, calls(p, Builtin::ToJson));
    for t in &jtypes {
        emit_to_json(out, p, &mut c, *t);
    }
    c.emit(out);
}

const PROLOGUE: &str = "  %us = alloca %unknown\n  store %unknown %u, ptr %us\n  %tag = load i8, ptr %us\n  %found = zext i8 %tag to i64\n  %wp = getelementptr inbounds %unknown, ptr %us, i32 0, i32 1\n  %w = load i64, ptr %wp";

fn emit_validate(out: &mut String, p: &Program, drops: &DropInfo, c: &mut Consts, t: Ty) {
    let lt = llty(t);
    let name = validate_fn(t);
    let expected = type_display(p, t);
    let _ = writeln!(
        out,
        "\ndefine private i1 {name}(%unknown %u, ptr %out) {{\nentry:"
    );
    // 一時領域は入口で確保する（ループの中で確保するとスタックが伸びる）
    let tmp = |out: &mut String, et: Ty, nm: &str| {
        let _ = writeln!(out, "  %{nm} = alloca {}", llty(et));
    };
    match t {
        Ty::Record(r) => {
            for (k, (_, ft)) in p.records[r.0 as usize].iter().enumerate() {
                tmp(out, *ft, &format!("f{k}"));
            }
        }
        Ty::List(l) => tmp(out, p.lists[l.0 as usize], "x"),
        Ty::Dict(d) => tmp(out, p.dicts[d.0 as usize].1, "x"),
        Ty::Union(u) => {
            for (j, m) in p.unions[u.0 as usize].iter().enumerate() {
                if let Some(mt) = m {
                    tmp(out, *mt, &format!("m{j}"));
                }
            }
        }
        _ => {}
    }
    let _ = writeln!(out, "{PROLOGUE}");
    let fail =
        |c: &mut Consts| format!("{}\n{}", record_fail(c, &expected, "%found"), fail_exit(t));
    match t {
        Ty::Int | Ty::Bool | Ty::Str => {
            let (tag, val) = match t {
                Ty::Int => (0, "  store i64 %w, ptr %out".to_string()),
                Ty::Bool => (1, "  %b = trunc i64 %w to i1\n  store i1 %b, ptr %out".to_string()),
                _ => (
                    2,
                    "  %s = inttoptr i64 %w to ptr\n  call void @wys_rc_retain(ptr %s)\n  store ptr %s, ptr %out".to_string(),
                ),
            };
            let _ = writeln!(
                out,
                "  %ok = icmp eq i8 %tag, {tag}\n  br i1 %ok, label %yes, label %no\nyes:\n{val}\n  ret i1 1\nno:\n{}\n}}",
                fail(c)
            );
        }
        Ty::Float => {
            // 整数も float に受け付ける（ADR-0022 論点 6）
            let _ = writeln!(
                out,
                "  %isf = icmp eq i8 %tag, 3\n  br i1 %isf, label %f, label %ni\nf:\n  %d = bitcast i64 %w to double\n  store double %d, ptr %out\n  ret i1 1\nni:\n  %isi = icmp eq i8 %tag, 0\n  br i1 %isi, label %i, label %no\ni:\n  %c = sitofp i64 %w to double\n  store double %c, ptr %out\n  ret i1 1\nno:\n{}\n}}",
                fail(c)
            );
        }
        Ty::Unknown => {
            // 木をそのまま共有する（ADR-0021 論点 4）
            let _ = writeln!(
                out,
                "  call void @wys_retain_unknown(%unknown %u)\n  store %unknown %u, ptr %out\n  ret i1 1\n}}"
            );
        }
        Ty::List(l) => {
            let e = p.lists[l.0 as usize];
            let (el, size) = (llty(e), elem_size(p, e));
            let _ = writeln!(
                out,
                "  %isl = icmp eq i8 %tag, 5\n  br i1 %isl, label %start, label %no\nstart:\n  %src = inttoptr i64 %w to ptr\n  %n = call i64 @wys_list_len(ptr %src)\n  %dst = call ptr @wys_list_new(i64 {size}, i64 %n)\n  br label %loop\nloop:\n  %i = phi i64 [0, %start], [%i2, %push]\n  %more = icmp slt i64 %i, %n\n  br i1 %more, label %body, label %done\nbody:\n  %ep = call ptr @wys_list_slot(ptr %src, i64 %i, i64 16)\n  %ev = load %unknown, ptr %ep\n  %eok = call i1 {}(%unknown %ev, ptr %x)\n  br i1 %eok, label %push, label %bad\npush:\n  %xv = load {el}, ptr %x\n  %slot = call ptr @wys_list_push_slot(ptr %dst, i64 {size})\n  store {el} %xv, ptr %slot\n  %i2 = add i64 %i, 1\n  br label %loop\nbad:\n  call void @wys_schema_path_index(i64 %i)\n  call void {}(ptr %dst)\n{}\ndone:\n  store ptr %dst, ptr %out\n  ret i1 1\nno:\n{}\n}}",
                validate_fn(e),
                glue("release", t),
                fail_exit(t),
                fail(c)
            );
        }
        Ty::Dict(d) => {
            let (_, v) = p.dicts[d.0 as usize];
            let vl = llty(v);
            let _ = writeln!(
                out,
                "  %isd = icmp eq i8 %tag, 6\n  br i1 %isd, label %start, label %no\nstart:\n  %src = inttoptr i64 %w to ptr\n  %len = call i64 @wys_dict_len(ptr %src)\n  %dst = call ptr @wys_dict_new(i64 2, i64 {})\n  br label %loop\nloop:\n  %pos = phi i64 [0, %start], [%pos2, %push]\n  %e = call i64 @wys_dict_iter_next(ptr %src, i64 %pos, i64 %len)\n  %more = icmp sge i64 %e, 0\n  br i1 %more, label %body, label %done\nbody:\n  %key = call i64 @wys_dict_key(ptr %src, i64 %e)\n  %kp = inttoptr i64 %key to ptr\n  %vp = call ptr @wys_dict_val(ptr %src, i64 %e)\n  %ev = load %unknown, ptr %vp\n  %eok = call i1 {}(%unknown %ev, ptr %x)\n  br i1 %eok, label %push, label %bad\npush:\n  %xv = load {vl}, ptr %x\n  call void @wys_rc_retain(ptr %kp)\n  %ne = call i64 @wys_dict_push(ptr %dst, i64 %key)\n  %np = call ptr @wys_dict_val(ptr %dst, i64 %ne)\n  store {vl} %xv, ptr %np\n  %pos2 = add i64 %e, 1\n  br label %loop\nbad:\n  call void @wys_schema_path_key(ptr %kp)\n  call void {}(ptr %dst)\n{}\ndone:\n  store ptr %dst, ptr %out\n  ret i1 1\nno:\n{}\n}}",
                elem_size(p, v),
                validate_fn(v),
                glue("release", t),
                fail_exit(t),
                fail(c)
            );
        }
        Ty::Record(r) => {
            let fields = p.records[r.0 as usize].clone();
            let _ = writeln!(
                out,
                "  %isd = icmp eq i8 %tag, 6\n  br i1 %isd, label %start, label %no\nstart:\n  %src = inttoptr i64 %w to ptr\n  br label %k0"
            );
            for (k, (fname, ft)) in fields.iter().enumerate() {
                let key = c.str_obj(fname);
                let (fg, fnl) = c.byte(fname);
                // 失敗したら、検証を済ませた前のフィールドを解放する
                let release_prev = |tag: &str| {
                    let mut r = String::new();
                    for (k2, (_, ft2)) in fields.iter().enumerate().take(k) {
                        if drops.get(*ft2) {
                            let _ = write!(
                                r,
                                "\n  %r{tag}{k}_{k2} = load {l}, ptr %f{k2}\n  call void {}({l} %r{tag}{k}_{k2})",
                                glue("release", *ft2),
                                l = llty(*ft2)
                            );
                        }
                    }
                    r
                };
                let (rel_miss, rel_bad) = (release_prev("m"), release_prev("b"));
                let _ = writeln!(
                    out,
                    "k{k}:\n  %e{k} = call i64 @wys_dict_find(ptr %src, i64 ptrtoint (ptr {key} to i64))\n  %has{k} = icmp sge i64 %e{k}, 0\n  br i1 %has{k}, label %v{k}, label %miss{k}\nmiss{k}:\n{}\n  call void @wys_schema_path_field(ptr {fg}, i64 {fnl}){rel_miss}\n{}\nv{k}:\n  %vp{k} = call ptr @wys_dict_val(ptr %src, i64 %e{k})\n  %ev{k} = load %unknown, ptr %vp{k}\n  %ok{k} = call i1 {}(%unknown %ev{k}, ptr %f{k})\n  br i1 %ok{k}, label %k{}, label %bad{k}\nbad{k}:\n  call void @wys_schema_path_field(ptr {fg}, i64 {fnl}){rel_bad}\n{}",
                    record_fail(c, &type_display(p, *ft), "7"),
                    fail_exit(t),
                    validate_fn(*ft),
                    k + 1,
                    fail_exit(t)
                );
            }
            // すべて通ったらレコードを組む
            let _ = writeln!(out, "k{}:", fields.len());
            let mut acc = "undef".to_string();
            for (k, (_, ft)) in fields.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "  %l{k} = load {}, ptr %f{k}\n  %a{k} = insertvalue {lt} {acc}, {} %l{k}, {k}",
                    llty(*ft),
                    llty(*ft)
                );
                acc = format!("%a{k}");
            }
            if fields.is_empty() {
                acc = "zeroinitializer".into();
            }
            let _ = writeln!(
                out,
                "  store {lt} {acc}, ptr %out\n  ret i1 1\nno:\n{}\n}}",
                fail(c)
            );
        }
        Ty::Union(u) => {
            let members = p.unions[u.0 as usize].clone();
            let _ = writeln!(
                out,
                "  store {lt} zeroinitializer, ptr %out\n  %tp = getelementptr inbounds {lt}, ptr %out, i32 0, i32 0\n  %pp = getelementptr inbounds {lt}, ptr %out, i32 0, i32 1\n  br label %t0"
            );
            // 構成要素の順（型検査がそろえた順）に試す。内側の失敗は Union の段の失敗として報告する
            for (j, m) in members.iter().enumerate() {
                match m {
                    Some(mt) => {
                        let _ = writeln!(
                            out,
                            "t{j}:\n  %ok{j} = call i1 {}(%unknown %u, ptr %m{j})\n  br i1 %ok{j}, label %y{j}, label %t{}\ny{j}:\n  %v{j} = load {ml}, ptr %m{j}\n  store i8 {j}, ptr %tp\n  store {ml} %v{j}, ptr %pp\n  ret i1 1",
                            validate_fn(*mt),
                            j + 1,
                            ml = llty(*mt)
                        );
                    }
                    None => {
                        let _ = writeln!(
                            out,
                            "t{j}:\n  %isn{j} = icmp eq i8 %tag, 4\n  br i1 %isn{j}, label %y{j}, label %t{}\ny{j}:\n  store i8 {j}, ptr %tp\n  ret i1 1",
                            j + 1
                        );
                    }
                }
            }
            let _ = writeln!(
                out,
                "t{}:\n  br label %no\nno:\n{}\n}}",
                members.len(),
                fail(c)
            );
        }
        Ty::Py | Ty::Task(_) => unreachable!("not a schema type (E1501)"),
    }
}

fn emit_to_json(out: &mut String, p: &Program, c: &mut Consts, t: Ty) {
    let lt = llty(t);
    match t {
        Ty::Int | Ty::Bool | Ty::Float | Ty::Str => {}
        Ty::Unknown => {
            let _ = writeln!(
                out,
                "\ndefine private void @wys_to_json_unknown(%unknown %v) {{\nentry:\n  %s = alloca %unknown\n  store %unknown %v, ptr %s\n  call void @wys_json_unknown(ptr %s)\n  ret void\n}}"
            );
        }
        Ty::List(l) => {
            let e = p.lists[l.0 as usize];
            let _ = writeln!(
                out,
                "\ndefine private void @wys_to_json_{n}(ptr %p) {{\nentry:\n{open}\n  %len = call i64 @wys_list_len(ptr %p)\n  br label %loop\nloop:\n  %i = phi i64 [0, %entry], [%i2, %elem]\n  %more = icmp slt i64 %i, %len\n  br i1 %more, label %body, label %fin\nbody:\n  %first = icmp eq i64 %i, 0\n  br i1 %first, label %elem, label %sep\nsep:\n{sep}\n  br label %elem\nelem:\n  %s = call ptr @wys_list_slot(ptr %p, i64 %i, i64 {size})\n  %x = load {el}, ptr %s\n{w}\n  %i2 = add i64 %i, 1\n  br label %loop\nfin:\n{close}\n  ret void\n}}",
                n = t.name(),
                open = c.json_bytes("["),
                sep = c.json_bytes(","),
                size = elem_size(p, e),
                el = llty(e),
                w = to_json_inst(e, "%x"),
                close = c.json_bytes("]"),
            );
        }
        Ty::Dict(d) => {
            let (_, v) = p.dicts[d.0 as usize];
            // 挿入順（ADR-0015）
            let _ = writeln!(
                out,
                "\ndefine private void @wys_to_json_{n}(ptr %p) {{\nentry:\n{open}\n  %len = call i64 @wys_dict_len(ptr %p)\n  br label %loop\nloop:\n  %pos = phi i64 [0, %entry], [%pos2, %elem]\n  %first = phi i1 [1, %entry], [0, %elem]\n  %e = call i64 @wys_dict_iter_next(ptr %p, i64 %pos, i64 %len)\n  %more = icmp sge i64 %e, 0\n  br i1 %more, label %body, label %fin\nbody:\n  br i1 %first, label %elem, label %sep\nsep:\n{sep}\n  br label %elem\nelem:\n  %kr = call i64 @wys_dict_key(ptr %p, i64 %e)\n  %k = inttoptr i64 %kr to ptr\n  call void @wys_json_str(ptr %k)\n{colon}\n  %s = call ptr @wys_dict_val(ptr %p, i64 %e)\n  %x = load {vl}, ptr %s\n{w}\n  %pos2 = add i64 %e, 1\n  br label %loop\nfin:\n{close}\n  ret void\n}}",
                n = t.name(),
                open = c.json_bytes("{"),
                sep = c.json_bytes(","),
                colon = c.json_bytes(":"),
                vl = llty(v),
                w = to_json_inst(v, "%x"),
                close = c.json_bytes("}"),
            );
        }
        Ty::Record(r) => {
            let _ = writeln!(
                out,
                "\ndefine private void @wys_to_json_{}({lt} %v) {{\nentry:",
                t.name()
            );
            // フィールドは名前の順（ADR-0022 の訂正）
            for (k, (fname, ft)) in p.records[r.0 as usize].iter().enumerate() {
                let key = serde_json::to_string(fname).unwrap_or_default();
                let label = if k == 0 {
                    format!("{{{key}:")
                } else {
                    format!(",{key}:")
                };
                let _ = writeln!(
                    out,
                    "{}\n  %f{k} = extractvalue {lt} %v, {k}\n{}",
                    c.json_bytes(&label),
                    to_json_inst(*ft, &format!("%f{k}"))
                );
            }
            let close = if p.records[r.0 as usize].is_empty() {
                "{}"
            } else {
                "}"
            };
            let _ = writeln!(out, "{}\n  ret void\n}}", c.json_bytes(close));
        }
        Ty::Union(u) => {
            let members = &p.unions[u.0 as usize];
            let _ = writeln!(
                out,
                "\ndefine private void @wys_to_json_{n}({lt} %v) {{\nentry:\n  %sv = alloca {lt}\n  store {lt} %v, ptr %sv\n  %tag = load i8, ptr %sv\n  %pp = getelementptr inbounds {lt}, ptr %sv, i32 0, i32 1\n  br label %c0",
                n = t.name()
            );
            for (j, m) in members.iter().enumerate() {
                let body = match m {
                    Some(mt) => format!(
                        "  %x{j} = load {}, ptr %pp\n{}",
                        llty(*mt),
                        to_json_inst(*mt, &format!("%x{j}"))
                    ),
                    None => c.json_bytes("null"),
                };
                let _ = writeln!(
                    out,
                    "c{j}:\n  %m{j} = icmp eq i8 %tag, {j}\n  br i1 %m{j}, label %d{j}, label %c{}\nd{j}:\n{body}\n  ret void",
                    j + 1
                );
            }
            let _ = writeln!(out, "c{}:\n  unreachable\n}}", members.len());
        }
        Ty::Py | Ty::Task(_) => unreachable!("not a schema type (E1501)"),
    }
}

/// 型の JSON Schema（ADR-0022 論点 5 と訂正。`properties` はフィールドの名前の順）。
pub fn json_schema(p: &Program, t: Ty) -> String {
    fn go(p: &Program, t: Option<Ty>) -> serde_json::Value {
        use serde_json::json;
        match t {
            None => json!({"type": "null"}),
            Some(Ty::Int) => json!({"type": "integer"}),
            Some(Ty::Float) => json!({"type": "number"}),
            Some(Ty::Bool) => json!({"type": "boolean"}),
            Some(Ty::Str) => json!({"type": "string"}),
            Some(Ty::Unknown) => json!({}),
            Some(Ty::List(l)) => {
                json!({"type": "array", "items": go(p, Some(p.lists[l.0 as usize]))})
            }
            Some(Ty::Dict(d)) => {
                json!({"type": "object", "additionalProperties": go(p, Some(p.dicts[d.0 as usize].1))})
            }
            Some(Ty::Record(r)) => {
                let fields = &p.records[r.0 as usize];
                let mut props = serde_json::Map::new();
                for (n, ft) in fields {
                    props.insert(n.clone(), go(p, Some(*ft)));
                }
                let required: Vec<&String> = fields.iter().map(|(n, _)| n).collect();
                json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
            }
            Some(Ty::Union(u)) => {
                let any: Vec<serde_json::Value> =
                    p.unions[u.0 as usize].iter().map(|m| go(p, *m)).collect();
                json!({"anyOf": any})
            }
            Some(Ty::Py | Ty::Task(_)) => unreachable!("not a schema type (E1501)"),
        }
    }
    go(p, Some(t)).to_string()
}
