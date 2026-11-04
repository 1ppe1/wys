//! ARC（自動参照カウント）の挿入と検証（ADR-0012）
//!
//! lowering は所有権を意識せずに IR を作る。このパスが、memory-model 2.2 の規則に従って `Retain` / `Release` を挿入する。
//!
//! - 一時値の所有の種類: **+1 所有**（関数呼び出しの結果、`MakeUnion` / `MakeRecord` で作った値）と
//!   **借用**（ローカルからの `Load`、文字列リテラル、`UnionPayload` / `Field` で取り出した値、不死の引数文字列）。
//! - 値を消費する地点（ローカルへの `Store`、`Return`、`MakeUnion` の中身、`MakeRecord` のフィールド）では
//!   +1 が必要。まだ消費していない +1 所有の値ならそのまま渡し、そうでなければ `Retain` する。
//! - 関数の引数は +0（借用）で渡す。T2a の時点では可変な場所（`list` の要素など）がないので、借用の安全条件は自動的に満たされる。
//! - ローカルのスロットは値を +1 で所有する。`Store` は古い値を `Release` し、関数を抜けるときは解放が必要なローカルをすべて `Release` する。
//!   引数は入口で `Retain` してスロットに所有させる。スロットの初期値はゼロ（null）で、`Release` は null を無視する。
//! - ブロックの終わりまで消費されなかった +1 所有の値は、終端命令の前で `Release` する。
//!
//! 冗長な組（retain と直後の release など）の除去は、正しさを確かめた後の別段階（M3b）で行う。

use crate::*;

/// 型が解放処理（参照カウントの操作）を必要とするか。Union / レコードの結果はメモ化する（深い入れ子で指数的にならないように）。
pub struct DropInfo {
    unions: Vec<Option<bool>>,
    records: Vec<Option<bool>>,
}

impl DropInfo {
    pub fn new(p: &Program) -> Self {
        let mut d = DropInfo {
            unions: vec![None; p.unions.len()],
            records: vec![None; p.records.len()],
        };
        for i in 0..p.unions.len() {
            d.needs_drop(p, Ty::Union(UnionId(i as u32)));
        }
        for i in 0..p.records.len() {
            d.needs_drop(p, Ty::Record(RecordId(i as u32)));
        }
        d
    }

    fn needs_drop(&mut self, p: &Program, t: Ty) -> bool {
        match t {
            Ty::Int | Ty::Bool => false,
            Ty::Str | Ty::List(_) => true,
            Ty::Union(u) => {
                if let Some(v) = self.unions[u.0 as usize] {
                    return v;
                }
                let v = p.unions[u.0 as usize]
                    .clone()
                    .into_iter()
                    .any(|m| m.is_some_and(|m| self.needs_drop(p, m)));
                self.unions[u.0 as usize] = Some(v);
                v
            }
            Ty::Record(r) => {
                if let Some(v) = self.records[r.0 as usize] {
                    return v;
                }
                let v = p.records[r.0 as usize]
                    .clone()
                    .into_iter()
                    .any(|(_, ft)| self.needs_drop(p, ft));
                self.records[r.0 as usize] = Some(v);
                v
            }
        }
    }

    /// `new` で全型を計算済みなので、ここでは表を引くだけ。
    pub fn get(&self, t: Ty) -> bool {
        match t {
            Ty::Int | Ty::Bool => false,
            Ty::Str | Ty::List(_) => true,
            Ty::Union(u) => self.unions[u.0 as usize].unwrap_or(false),
            Ty::Record(r) => self.records[r.0 as usize].unwrap_or(false),
        }
    }
}

/// すべての関数に `Retain` / `Release` を挿入する。
pub fn insert(p: &mut Program) {
    let drops = DropInfo::new(p);
    let callee_rets: Vec<Option<Ty>> = p.funcs.iter().map(|f| f.ret).collect();
    for f in &mut p.funcs {
        insert_function(f, &drops, &callee_rets);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Own {
    Owned,
    Borrowed,
}

fn insert_function(f: &mut Function, drops: &DropInfo, _rets: &[Option<Ty>]) {
    let drop_locals: Vec<LocalId> = (0..f.locals.len())
        .filter(|i| drops.get(f.locals[*i]))
        .map(|i| LocalId(i as u32))
        .collect();
    if drop_locals.is_empty() && !f.temps.iter().any(|t| drops.get(*t)) {
        return;
    }
    let mut temps = std::mem::take(&mut f.temps);
    let mut new_temp = |ty: Ty, temps: &mut Vec<Ty>| {
        temps.push(ty);
        TempId(temps.len() as u32 - 1)
    };
    let release_locals = |out: &mut Vec<Inst>,
                          temps: &mut Vec<Ty>,
                          new_temp: &mut dyn FnMut(Ty, &mut Vec<Ty>) -> TempId,
                          locals: &[Ty]| {
        for l in &drop_locals {
            let t = new_temp(locals[l.0 as usize], temps);
            out.push(Inst::Load { dst: t, src: *l });
            out.push(Inst::Release { src: t });
        }
    };
    let locals = f.locals.clone();
    for (bi, block) in f.blocks.iter_mut().enumerate() {
        let mut out = Vec::new();
        // 入口: 引数をスロットに所有させる
        if bi == 0 {
            for l in drop_locals.iter().filter(|l| l.0 < f.param_count) {
                let t = new_temp(locals[l.0 as usize], &mut temps);
                out.push(Inst::Load { dst: t, src: *l });
                out.push(Inst::Retain { src: t });
            }
        }
        let mut own: std::collections::HashMap<TempId, (Own, bool)> = Default::default();
        let consume = |t: TempId,
                       out: &mut Vec<Inst>,
                       own: &mut std::collections::HashMap<TempId, (Own, bool)>,
                       temps: &Vec<Ty>| {
            if !drops.get(temps[t.0 as usize]) {
                return;
            }
            match own.get_mut(&t) {
                Some((Own::Owned, consumed)) if !*consumed => *consumed = true,
                _ => out.push(Inst::Retain { src: t }),
            }
        };
        for inst in std::mem::take(&mut block.insts) {
            match &inst {
                Inst::Const { dst, .. } | Inst::Load { dst, .. } => {
                    own.insert(*dst, (Own::Borrowed, false));
                    out.push(inst);
                }
                Inst::ListNew { dst, items, .. } => {
                    for it in items.clone() {
                        consume(it, &mut out, &mut own, &temps);
                    }
                    own.insert(*dst, (Own::Owned, false));
                    out.push(inst);
                }
                Inst::ListSet { value, .. } | Inst::ListPush { value, .. } => {
                    consume(*value, &mut out, &mut own, &temps);
                    out.push(inst);
                }
                Inst::ListLen { .. } => out.push(inst),
                Inst::UnionPayload { dst, .. }
                | Inst::Field { dst, .. }
                | Inst::ListGet { dst, .. } => {
                    own.insert(*dst, (Own::Borrowed, false));
                    out.push(inst);
                }
                Inst::Call { dst, callee, .. } => {
                    if let Some(d) = dst {
                        let o = match callee {
                            Callee::Func(_) => Own::Owned,
                            Callee::Builtin(b) if b.returns_owned() => Own::Owned,
                            // ほかの組み込み関数が返す文字列は不死（コマンドライン引数）
                            Callee::Builtin(_) => Own::Borrowed,
                        };
                        own.insert(*d, (o, false));
                    }
                    out.push(inst);
                }
                Inst::MakeUnion { dst, payload, .. } => {
                    if let Some(p) = payload {
                        consume(*p, &mut out, &mut own, &temps);
                    }
                    own.insert(*dst, (Own::Owned, false));
                    out.push(inst);
                }
                Inst::MakeRecord { dst, fields, .. } => {
                    for fld in fields.clone() {
                        consume(fld, &mut out, &mut own, &temps);
                    }
                    own.insert(*dst, (Own::Owned, false));
                    out.push(inst);
                }
                Inst::Store { dst, src } => {
                    if drops.get(locals[dst.0 as usize]) {
                        consume(*src, &mut out, &mut own, &temps);
                        let old = new_temp(locals[dst.0 as usize], &mut temps);
                        out.push(Inst::Load {
                            dst: old,
                            src: *dst,
                        });
                        out.push(Inst::Release { src: old });
                    }
                    out.push(inst);
                }
                Inst::Binary { .. } | Inst::Not { .. } | Inst::UnionIs { .. } => out.push(inst),
                Inst::Retain { .. } | Inst::Release { .. } => out.push(inst),
            }
        }
        if let Terminator::Return(Some(t)) = &block.term {
            consume(*t, &mut out, &mut own, &temps);
        }
        // 消費されなかった +1 所有の値を解放する（定義の順）
        let mut leftovers: Vec<TempId> = own
            .iter()
            .filter(|(t, (o, consumed))| {
                *o == Own::Owned && !consumed && drops.get(temps[t.0 as usize])
            })
            .map(|(t, _)| *t)
            .collect();
        leftovers.sort_by_key(|t| t.0);
        for t in leftovers {
            out.push(Inst::Release { src: t });
        }
        if matches!(block.term, Terminator::Return(_)) {
            release_locals(&mut out, &mut temps, &mut new_temp, &locals);
        }
        block.insts = out;
    }
    f.temps = temps;
}

/// ARC の釣り合いの検査（各ブロック）:
/// - +1 所有で定義した値（関数呼び出しの結果、`MakeUnion` / `MakeRecord`）と、ローカル以外からの借用（リテラル、
///   取り出した値）は、「定義での +1 + Retain − Release − 消費」が 0 になること。
/// - ローカルから読み出した値は、Retain した回数より多く消費しないこと（読み出した値の Release は、
///   スロットが持つ参照を手放す操作なので数えない）。
///
/// スロット単位の釣り合いは、実行時の生存オブジェクト数と AddressSanitizer で確かめる（ADR-0012）。
pub fn verify(p: &Program) -> Result<(), String> {
    let drops = DropInfo::new(p);
    for f in &p.funcs {
        for (bi, b) in f.blocks.iter().enumerate() {
            let mut bal: std::collections::HashMap<TempId, i64> = Default::default();
            let mut loaded: std::collections::HashSet<TempId> = Default::default();
            let ty = |t: TempId| f.temps[t.0 as usize];
            let add = |t: TempId, d: i64, bal: &mut std::collections::HashMap<TempId, i64>| {
                if drops.get(ty(t)) {
                    *bal.entry(t).or_insert(0) += d;
                }
            };
            for inst in &b.insts {
                match inst {
                    Inst::Load { dst, .. } => {
                        loaded.insert(*dst);
                    }
                    Inst::Call {
                        dst: Some(d),
                        callee,
                        ..
                    } if matches!(callee, Callee::Func(_))
                        || matches!(callee, Callee::Builtin(b) if b.returns_owned()) =>
                    {
                        add(*d, 1, &mut bal)
                    }
                    Inst::MakeUnion { dst, payload, .. } => {
                        if let Some(pl) = payload {
                            add(*pl, -1, &mut bal);
                        }
                        add(*dst, 1, &mut bal);
                    }
                    Inst::MakeRecord { dst, fields, .. } => {
                        for fl in fields {
                            add(*fl, -1, &mut bal);
                        }
                        add(*dst, 1, &mut bal);
                    }
                    Inst::ListNew { dst, items, .. } => {
                        for it in items {
                            add(*it, -1, &mut bal);
                        }
                        add(*dst, 1, &mut bal);
                    }
                    Inst::ListSet { value, .. } | Inst::ListPush { value, .. } => {
                        add(*value, -1, &mut bal)
                    }
                    Inst::Retain { src } => add(*src, 1, &mut bal),
                    Inst::Release { src } if !loaded.contains(src) => add(*src, -1, &mut bal),
                    Inst::Store { dst, src } if drops.get(f.locals[dst.0 as usize]) => {
                        add(*src, -1, &mut bal)
                    }
                    _ => {}
                }
            }
            if let Terminator::Return(Some(t)) = &b.term {
                add(*t, -1, &mut bal);
            }
            for (t, n) in &bal {
                let ok = if loaded.contains(t) { *n >= 0 } else { *n == 0 };
                if !ok {
                    return Err(format!(
                        "in function `{}`, bb{bi}: reference count of t{} is unbalanced by {n}",
                        f.name, t.0
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn str_fn(
        insts: Vec<Inst>,
        temps: Vec<Ty>,
        locals: Vec<Ty>,
        params: u32,
        ret: Option<Ty>,
        term: Terminator,
    ) -> Program {
        Program {
            strings: vec!["s".into()],
            unions: vec![],
            records: vec![],
            lists: vec![],
            entry: FuncId(0),
            funcs: vec![Function {
                name: "f".into(),
                param_count: params,
                locals,
                ret,
                temps,
                blocks: vec![Block { insts, term }],
            }],
        }
    }

    #[test]
    fn store_of_a_literal_retains_and_releases_the_old_value() {
        // l0 = "s"; return
        let mut p = str_fn(
            vec![
                Inst::Const {
                    dst: TempId(0),
                    value: Const::Str(StrId(0)),
                },
                Inst::Store {
                    dst: LocalId(0),
                    src: TempId(0),
                },
            ],
            vec![Ty::Str],
            vec![Ty::Str],
            0,
            None,
            Terminator::Return(None),
        );
        insert(&mut p);
        let text = p.to_string();
        assert!(text.contains("retain t0"), "{text}");
        assert!(text.contains("release t1"), "{text}"); // 古い値（null）
        assert!(text.contains("release t2"), "{text}"); // 関数を抜けるときのローカル
        verify(&p).unwrap();
    }

    #[test]
    fn returning_a_parameter_retains_it() {
        // def f(s: str) -> str: return s
        let mut p = str_fn(
            vec![Inst::Load {
                dst: TempId(0),
                src: LocalId(0),
            }],
            vec![Ty::Str],
            vec![Ty::Str],
            1,
            Some(Ty::Str),
            Terminator::Return(Some(TempId(0))),
        );
        insert(&mut p);
        verify(&p).unwrap();
        let text = p.to_string();
        // 入口で引数を retain、戻り値を retain、出口でローカルを release
        assert_eq!(text.matches("retain").count(), 2, "{text}");
        assert_eq!(text.matches("release").count(), 1, "{text}");
    }

    #[test]
    fn verifier_detects_a_missing_release() {
        let mut p = str_fn(
            vec![Inst::Call {
                dst: Some(TempId(0)),
                callee: Callee::Func(FuncId(0)),
                args: vec![],
            }],
            vec![Ty::Str],
            vec![],
            0,
            Some(Ty::Str),
            Terminator::Unreachable,
        );
        assert!(verify(&p).is_err());
        insert(&mut p);
        verify(&p).unwrap();
    }
}
