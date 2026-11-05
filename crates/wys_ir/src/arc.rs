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
//! 冗長な組の除去は、挿入の後の別段階（`optimize`、M4 T3、memory-model 2.3）で行う。取り除いた操作は
//! `Function::elided_rc` に記録し、`wys explain` が報告する。

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
            Ty::Int | Ty::Bool | Ty::Float => false,
            Ty::Str | Ty::List(_) | Ty::Dict(_) | Ty::Py => true,
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
            Ty::Int | Ty::Bool | Ty::Float => false,
            Ty::Str | Ty::List(_) | Ty::Dict(_) | Ty::Py => true,
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
            out.push(Inst::Release {
                src: t,
                reason: RcReason::SlotAtFunctionExit,
            });
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
                out.push(Inst::Retain {
                    src: t,
                    reason: RcReason::ParamSlotEntry,
                });
            }
        }
        let mut own: std::collections::HashMap<TempId, (Own, bool)> = Default::default();
        let consume = |t: TempId,
                       site: ConsumeSite,
                       out: &mut Vec<Inst>,
                       own: &mut std::collections::HashMap<TempId, (Own, bool)>,
                       temps: &Vec<Ty>| {
            if !drops.get(temps[t.0 as usize]) {
                return;
            }
            let reason = match own.get_mut(&t) {
                Some((Own::Owned, consumed)) if !*consumed => {
                    *consumed = true;
                    return;
                }
                Some((Own::Owned, _)) => RcReason::ConsumeSharedOwned(site),
                _ => RcReason::ConsumeBorrowed(site),
            };
            out.push(Inst::Retain { src: t, reason });
        };
        for inst in std::mem::take(&mut block.insts) {
            match &inst {
                Inst::Const { dst, .. } | Inst::Load { dst, .. } => {
                    own.insert(*dst, (Own::Borrowed, false));
                    out.push(inst);
                }
                // `take` は最適化が作る（挿入の後）。ここに来るのは、最適化した IR にもう一度挿入したときだけ
                Inst::Take { dst, .. } => {
                    own.insert(*dst, (Own::Owned, false));
                    out.push(inst);
                }
                Inst::ListNew { dst, items, .. } => {
                    for it in items.clone() {
                        consume(it, ConsumeSite::ListNew, &mut out, &mut own, &temps);
                    }
                    own.insert(*dst, (Own::Owned, false));
                    out.push(inst);
                }
                Inst::ListSet { value, .. } | Inst::ListPush { value, .. } => {
                    let site = if matches!(inst, Inst::ListSet { .. }) {
                        ConsumeSite::ListSet
                    } else {
                        ConsumeSite::ListPush
                    };
                    consume(*value, site, &mut out, &mut own, &temps);
                    out.push(inst);
                }
                Inst::ListLen { .. }
                | Inst::DictLen { .. }
                | Inst::DictContains { .. }
                | Inst::DictIterNext { .. } => out.push(inst),
                Inst::DictNew {
                    dst, keys, vals, ..
                } => {
                    for it in keys.iter().chain(vals).copied().collect::<Vec<_>>() {
                        consume(it, ConsumeSite::DictNew, &mut out, &mut own, &temps);
                    }
                    own.insert(*dst, (Own::Owned, false));
                    out.push(inst);
                }
                // 取り出した値の所有は呼び出し側に移る（+1）
                Inst::DictPop { dst, .. } => {
                    own.insert(*dst, (Own::Owned, false));
                    out.push(inst);
                }
                Inst::DictSet { key, value, .. } => {
                    consume(*key, ConsumeSite::DictSet, &mut out, &mut own, &temps);
                    consume(*value, ConsumeSite::DictSet, &mut out, &mut own, &temps);
                    out.push(inst);
                }
                Inst::UnionPayload { dst, .. } | Inst::Field { dst, .. } => {
                    own.insert(*dst, (Own::Borrowed, false));
                    out.push(inst);
                }
                // 借用の安全条件（memory-model 2.2）: リストの要素は可変な場所なので、読んだ値をすぐに保持する。
                // 値を使っている間にリストが書き換えられても（`xs[0] + f(xs)` の `f` が `xs[0]` を置き換えるなど）解放されない。
                // 使われなければブロックの最後で release する（冗長な組の除去は M3b の Move 最適化で行う）
                Inst::ListGet { dst, .. }
                | Inst::DictGet { dst, .. }
                | Inst::DictKeyAt { dst, .. } => {
                    let d = *dst;
                    out.push(inst);
                    if drops.get(temps[d.0 as usize]) {
                        out.push(Inst::Retain {
                            src: d,
                            reason: RcReason::BorrowFromMutablePlace,
                        });
                        own.insert(d, (Own::Owned, false));
                    } else {
                        own.insert(d, (Own::Borrowed, false));
                    }
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
                        consume(*p, ConsumeSite::MakeUnion, &mut out, &mut own, &temps);
                    }
                    own.insert(*dst, (Own::Owned, false));
                    out.push(inst);
                }
                Inst::MakeRecord { dst, fields, .. } => {
                    for fld in fields.clone() {
                        consume(fld, ConsumeSite::MakeRecord, &mut out, &mut own, &temps);
                    }
                    own.insert(*dst, (Own::Owned, false));
                    out.push(inst);
                }
                Inst::Store { dst, src } => {
                    if drops.get(locals[dst.0 as usize]) {
                        consume(*src, ConsumeSite::Store, &mut out, &mut own, &temps);
                        let old = new_temp(locals[dst.0 as usize], &mut temps);
                        out.push(Inst::Load {
                            dst: old,
                            src: *dst,
                        });
                        out.push(Inst::Release {
                            src: old,
                            reason: RcReason::StoreOverwritesSlot,
                        });
                    }
                    out.push(inst);
                }
                Inst::Binary { .. } | Inst::Not { .. } | Inst::UnionIs { .. } => out.push(inst),
                Inst::Retain { .. } | Inst::Release { .. } => out.push(inst),
            }
        }
        if let Terminator::Return(Some(t)) = &block.term {
            consume(*t, ConsumeSite::Return, &mut out, &mut own, &temps);
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
            out.push(Inst::Release {
                src: t,
                reason: RcReason::UnconsumedOwnedAtBlockEnd,
            });
        }
        if matches!(block.term, Terminator::Return(_)) {
            release_locals(&mut out, &mut temps, &mut new_temp, &locals);
        }
        block.insts = out;
    }
    f.temps = temps;
}

/// 引数のスロットの参照の釣り合い（M4 T3 の Move 最適化の検査）: 解放が必要な型の引数ごとに、入口の
/// `param_slot_entry` の retain と、すべての `return` のブロックの `slot_at_function_exit` の release が、両方あるか
/// 両方ないこと。両方ないのは、関数の中で代入しない引数だけ（呼び出し側の参照を借りる）。
/// ブロックごとの釣り合いの検査は、スロットから読んだ一時値の release を数えないので、この組の片方だけが
/// 残る誤りをここで見つける（PR #89 のレビュー）。
fn verify_param_slots(f: &Function, drops: &DropInfo) -> Result<(), String> {
    let pair = |insts: &[Inst], p: LocalId, want: RcReason, retain: bool| {
        insts.windows(2).any(|w| match (&w[0], &w[1]) {
            (Inst::Load { dst, src }, Inst::Retain { src: t, reason }) if retain => {
                *src == p && t == dst && *reason == want
            }
            (Inst::Load { dst, src }, Inst::Release { src: t, reason }) if !retain => {
                *src == p && t == dst && *reason == want
            }
            _ => false,
        })
    };
    for p in (0..f.param_count).map(LocalId) {
        if !drops.get(f.locals[p.0 as usize]) {
            continue;
        }
        let stored = f
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .any(|i| matches!(i, Inst::Store { dst, .. } if *dst == p));
        let entry = f
            .blocks
            .first()
            .is_some_and(|b| pair(&b.insts, p, RcReason::ParamSlotEntry, true));
        if stored && !entry {
            return Err(format!(
                "parameter l{} is reassigned but its slot does not own a reference on entry",
                p.0
            ));
        }
        for (bi, b) in f.blocks.iter().enumerate() {
            if !matches!(b.term, Terminator::Return(_)) {
                continue;
            }
            let exit = pair(&b.insts, p, RcReason::SlotAtFunctionExit, false);
            if exit != entry {
                return Err(format!(
                    "bb{bi}: parameter l{} has {} on entry but {} on exit",
                    p.0,
                    if entry { "a retain" } else { "no retain" },
                    if exit { "a release" } else { "no release" }
                ));
            }
        }
    }
    Ok(())
}

/// Move 最適化（M4 T3、memory-model 2.3）: `insert` が保守的に入れた retain / release のうち、冗長な組を取り除く。
/// 取り除いた操作は `Function::elided_rc` に記録する。`verify` はこの後に実行する（最適化の釣り合いも検査する）。
///
/// - **P4（`param_never_reassigned`）**: 関数の中で代入しない引数のスロットは、呼び出し側の参照を借りるだけでよい。
///   呼び出し側は、渡した値を呼び出しの間生かしておく（借用の安全条件、memory-model 2.2）。入口の
///   `param_slot_entry` の retain と、出口の `slot_at_function_exit` の release を取り除く。
/// - **P0（`borrow_guard_unneeded`）**: 可変な場所（`list` の要素、`dict` の値やキー）から読んだ値を保持する
///   `borrow_from_mutable_place` の retain と、ブロックの終わりの release の組。読んでから最後の使用までに、
///   場所を変え得る命令（関数の呼び出し、要素の書き換え、release）がなければ取り除く。
pub fn optimize(p: &mut Program) {
    for f in &mut p.funcs {
        optimize_function(f);
    }
}

/// 可変な場所から読んだ値の保持（P0）を省いてよい命令か: オブジェクトを解放せず、コンテナを書き換えない。
fn keeps_places(inst: &Inst) -> bool {
    match inst {
        Inst::Release { .. }
        | Inst::ListSet { .. }
        | Inst::ListPush { .. }
        | Inst::DictSet { .. }
        | Inst::DictPop { .. } => false,
        // 呼ばれた関数は、引数を通じてコンテナを書き換え得る（`xs[0] + replace_first(xs)`）
        Inst::Call {
            callee: Callee::Func(_),
            ..
        } => false,
        Inst::Call {
            callee: Callee::Builtin(b),
            ..
        } => matches!(
            b,
            Builtin::PrintInt
                | Builtin::PrintBool
                | Builtin::PrintStr
                | Builtin::PrintFloat
                | Builtin::PrintValue
                | Builtin::StrConcat
                | Builtin::StrEq
                | Builtin::StrLen
                | Builtin::FloatFromInt
                | Builtin::IntFromFloat
                | Builtin::ArgCount
                | Builtin::ArgExists
                | Builtin::ArgStr
                | Builtin::ScalarKind
                | Builtin::ScalarInt
                | Builtin::ScalarBool
                | Builtin::ScalarFloat
                | Builtin::TestIndex
                | Builtin::Exit
                | Builtin::Panic
                | Builtin::AssertFail
        ),
        _ => true,
    }
}

fn optimize_function(f: &mut Function) {
    let mut remove: Vec<std::collections::HashSet<usize>> =
        vec![Default::default(); f.blocks.len()];
    let mut elided = Vec::new();
    // P4: 代入しない引数のスロット
    let stored: std::collections::HashSet<LocalId> = f
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            Inst::Store { dst, .. } => Some(*dst),
            _ => None,
        })
        .collect();
    for p in (0..f.param_count).map(LocalId) {
        if stored.contains(&p) {
            continue;
        }
        for (bi, b) in f.blocks.iter().enumerate() {
            for (i, pair) in b.insts.windows(2).enumerate() {
                let [Inst::Load { dst, src }, op] = pair else {
                    continue;
                };
                if *src != p {
                    continue;
                }
                let found = match op {
                    Inst::Retain {
                        src: t,
                        reason: r @ RcReason::ParamSlotEntry,
                    } if t == dst && bi == 0 => Some((true, *r)),
                    Inst::Release {
                        src: t,
                        reason: r @ RcReason::SlotAtFunctionExit,
                    } if t == dst => Some((false, *r)),
                    _ => None,
                };
                if let Some((retain, reason)) = found {
                    remove[bi].insert(i);
                    remove[bi].insert(i + 1);
                    elided.push(ElidedRcOp {
                        retain,
                        reason,
                        src: *dst,
                        block: bi as u32,
                        index: i + 1,
                        local: Some(p),
                        elision: ElisionReason::ParamNeverReassigned,
                    });
                }
            }
        }
    }
    // P0: 可変な場所から読んだ値の保持
    for (bi, b) in f.blocks.iter().enumerate() {
        for i in 0..b.insts.len() {
            let t = match &b.insts[i] {
                Inst::ListGet { dst, .. }
                | Inst::DictGet { dst, .. }
                | Inst::DictKeyAt { dst, .. } => *dst,
                _ => continue,
            };
            let guarded = matches!(
                b.insts.get(i + 1),
                Some(Inst::Retain { src, reason: RcReason::BorrowFromMutablePlace }) if *src == t
            );
            if !guarded {
                continue;
            }
            // 消費されなかったので、ブロックの終わりで release されている（消費された値は対象外）
            let Some(k) = (i + 2..b.insts.len()).find(|&k| {
                matches!(&b.insts[k], Inst::Release { src, reason: RcReason::UnconsumedOwnedAtBlockEnd } if *src == t)
            }) else {
                continue;
            };
            // `t` から取り出した値（`Field` / `UnionPayload`。その先も）は、`t` の保持に頼って生きている。
            // それらの使用も `t` の使用として数える（`xs[0].name + replace_first(xs)`、PR #89 のレビュー）
            let mut derived: std::collections::HashSet<TempId> = [t].into();
            let mut last_use = None;
            for j in i + 2..k {
                let inst = &b.insts[j];
                if inst.operands().iter().any(|o| derived.contains(o)) {
                    last_use = Some(j);
                    if let Inst::Field { dst, .. } | Inst::UnionPayload { dst, .. } = inst {
                        derived.insert(*dst);
                    }
                }
            }
            // 終端命令（`return` / 分岐）で使う値は、ブロックの最後まで生きるので保持を残す
            let term_uses = match &b.term {
                Terminator::Return(Some(r)) => vec![*r],
                Terminator::Branch { cond, .. } => vec![*cond],
                _ => vec![],
            };
            let safe = !term_uses.iter().any(|r| derived.contains(r))
                && match last_use {
                    Some(u) => (i + 2..=u).all(|j| keeps_places(&b.insts[j])),
                    None => true,
                };
            if !safe {
                continue;
            }
            remove[bi].insert(i + 1);
            remove[bi].insert(k);
            for (retain, reason, index) in [
                (true, RcReason::BorrowFromMutablePlace, i + 1),
                (false, RcReason::UnconsumedOwnedAtBlockEnd, k),
            ] {
                elided.push(ElidedRcOp {
                    retain,
                    reason,
                    src: t,
                    block: bi as u32,
                    index,
                    local: None,
                    elision: ElisionReason::BorrowGuardUnneeded,
                });
            }
        }
    }
    for (b, rm) in f.blocks.iter_mut().zip(&remove) {
        if rm.is_empty() {
            continue;
        }
        let insts = std::mem::take(&mut b.insts);
        b.insts = insts
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !rm.contains(i))
            .map(|(_, inst)| inst)
            .collect();
    }
    // P1: ローカルの最後の使用での所有権の移動（P4 の後。借りているだけの引数のスロットは対象外）
    let borrowed_params: std::collections::HashSet<LocalId> = (0..f.param_count)
        .map(LocalId)
        .filter(|p| !stored.contains(p))
        .collect();
    move_from_slots(f, &borrowed_params, &mut elided);
    elided.sort_by_key(|e| (e.block, e.index));
    f.elided_rc = elided;
}

/// `load` がスロットの利用者の読み出しか（ARC パスが、古い値の release や入口の retain のために入れた読み出しでないか）。
fn user_load(insts: &[Inst], i: usize) -> Option<(TempId, LocalId)> {
    let Inst::Load { dst, src } = &insts[i] else {
        return None;
    };
    let arc_internal = matches!(
        insts.get(i + 1),
        Some(Inst::Release { src: t, reason: RcReason::StoreOverwritesSlot | RcReason::SlotAtFunctionExit })
            | Some(Inst::Retain { src: t, reason: RcReason::ParamSlotEntry }) if t == dst
    );
    (!arc_internal).then_some((*dst, *src))
}

/// 値を消費する命令か（`insert` の `consume` の地点）。
fn consumes(inst: &Inst) -> bool {
    matches!(
        inst,
        Inst::Store { .. }
            | Inst::MakeUnion { .. }
            | Inst::MakeRecord { .. }
            | Inst::ListNew { .. }
            | Inst::ListSet { .. }
            | Inst::ListPush { .. }
            | Inst::DictNew { .. }
            | Inst::DictSet { .. }
    )
}

/// ブロックの出口で、スロット `l` の今の値をまだ読む経路があるか（後ろ向きの生存解析。ループの戻り経路を含む）。
fn live_out(f: &Function, l: LocalId) -> Vec<bool> {
    let n = f.blocks.len();
    // ブロックの中で、store より前に利用者の読み出しがあるか（gen）、store があるか（kill）
    let (mut gen_, mut kill) = (vec![false; n], vec![false; n]);
    for (bi, b) in f.blocks.iter().enumerate() {
        for i in 0..b.insts.len() {
            match &b.insts[i] {
                Inst::Store { dst, .. } if *dst == l => {
                    kill[bi] = true;
                    break;
                }
                Inst::Take { src, .. } if *src == l => {
                    gen_[bi] = true;
                    break;
                }
                _ if user_load(&b.insts, i).is_some_and(|(_, src)| src == l) => {
                    gen_[bi] = true;
                    break;
                }
                _ => {}
            }
        }
    }
    let succs = |b: &Block| -> Vec<usize> {
        match &b.term {
            Terminator::Jump(t) => vec![t.0 as usize],
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![then_block.0 as usize, else_block.0 as usize],
            Terminator::Return(_) | Terminator::Unreachable => vec![],
        }
    };
    let mut live_in = vec![false; n];
    let mut out = vec![false; n];
    loop {
        let mut changed = false;
        for bi in (0..n).rev() {
            let o = succs(&f.blocks[bi]).into_iter().any(|s| live_in[s]);
            let i = gen_[bi] || (!kill[bi] && o);
            if o != out[bi] || i != live_in[bi] {
                out[bi] = o;
                live_in[bi] = i;
                changed = true;
            }
        }
        if !changed {
            return out;
        }
    }
}

/// P1（`moved_from_slot`）: `t = load L` の値を、借用のまま `retain` して1回だけ消費し、その後 L の今の値を
/// どの経路でも読まないなら、`t = take L`（スロットを空にして所有権を移す）にして retain を取り除く。
/// スロットの後の release（上書きと関数の出口）は空の値に対するものになり、何もしない。
fn move_from_slots(
    f: &mut Function,
    borrowed_params: &std::collections::HashSet<LocalId>,
    elided: &mut Vec<ElidedRcOp>,
) {
    let mut lives: std::collections::HashMap<LocalId, Vec<bool>> = Default::default();
    for bi in 0..f.blocks.len() {
        let mut rewrites = Vec::new();
        {
            let b = &f.blocks[bi];
            for i in 0..b.insts.len() {
                let Some((t, l)) = user_load(&b.insts, i) else {
                    continue;
                };
                if borrowed_params.contains(&l) {
                    continue;
                }
                // t の最初の retain は、借用を消費するためのもの
                let Some(j) = (i + 1..b.insts.len()).find(|&j| {
                    matches!(&b.insts[j], Inst::Retain { src, .. } | Inst::Release { src, .. } if *src == t)
                }) else {
                    continue;
                };
                let Inst::Retain {
                    reason: reason @ RcReason::ConsumeBorrowed(site),
                    ..
                } = b.insts[j]
                else {
                    continue;
                };
                // 消費する命令: retain の後で最初に t を使う命令（または return）。その後は t を使わない
                let consumer = (j + 1..b.insts.len()).find(|&c| b.insts[c].operands().contains(&t));
                let ok_consumer = match consumer {
                    Some(c) => {
                        consumes(&b.insts[c])
                            && !b.insts[c + 1..].iter().any(|x| x.operands().contains(&t))
                            && !matches!(&b.term, Terminator::Return(Some(r)) | Terminator::Branch { cond: r, .. } if *r == t)
                    }
                    None => {
                        site == ConsumeSite::Return
                            && matches!(&b.term, Terminator::Return(Some(r)) if *r == t)
                    }
                };
                if !ok_consumer {
                    continue;
                }
                // L の今の値を、後で読まない: ブロックの残りで利用者の読み出しがなく、store がないなら出口で生きていない
                let end = consumer.unwrap_or(b.insts.len());
                if (i + 1..end).any(|k| matches!(&b.insts[k], Inst::Store { dst, .. } if *dst == l))
                {
                    continue;
                }
                let mut killed = false;
                let mut read_later = false;
                for k in i + 1..b.insts.len() {
                    match &b.insts[k] {
                        Inst::Store { dst, .. } if *dst == l => {
                            killed = true;
                            break;
                        }
                        Inst::Take { src, .. } if *src == l => {
                            read_later = true;
                            break;
                        }
                        _ if user_load(&b.insts, k).is_some_and(|(_, src)| src == l) => {
                            read_later = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if read_later {
                    continue;
                }
                if !killed {
                    let live = lives.entry(l).or_insert_with(|| live_out(f, l));
                    if live[bi] {
                        continue;
                    }
                }
                rewrites.push((i, j, t, l, reason));
            }
        }
        if rewrites.is_empty() {
            continue;
        }
        let b = &mut f.blocks[bi];
        let removed: std::collections::HashSet<usize> = rewrites.iter().map(|r| r.1).collect();
        for &(i, j, t, l, reason) in &rewrites {
            b.insts[i] = Inst::Take { dst: t, src: l };
            elided.push(ElidedRcOp {
                retain: true,
                reason,
                src: t,
                block: bi as u32,
                index: j,
                local: Some(l),
                elision: ElisionReason::MovedFromSlot,
            });
        }
        let insts = std::mem::take(&mut b.insts);
        b.insts = insts
            .into_iter()
            .enumerate()
            .filter(|(k, _)| !removed.contains(k))
            .map(|(_, x)| x)
            .collect();
        // ブロックの中身が変わったので、このスロットの生存解析をやり直す
        for &(_, _, _, l, _) in &rewrites {
            lives.remove(&l);
        }
    }
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
        verify_param_slots(f, &drops).map_err(|e| format!("in function `{}`: {e}", f.name))?;
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
                    // スロットから所有権を移した値は +1 所有（ちょうど1回消費されること）
                    Inst::Take { dst, .. } => add(*dst, 1, &mut bal),
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
                    Inst::DictNew {
                        dst, keys, vals, ..
                    } => {
                        for it in keys.iter().chain(vals) {
                            add(*it, -1, &mut bal);
                        }
                        add(*dst, 1, &mut bal);
                    }
                    Inst::DictPop { dst, .. } => add(*dst, 1, &mut bal),
                    Inst::DictSet { key, value, .. } => {
                        add(*key, -1, &mut bal);
                        add(*value, -1, &mut bal);
                    }
                    Inst::Retain { src, .. } => add(*src, 1, &mut bal),
                    Inst::Release { src, .. } if !loaded.contains(src) => add(*src, -1, &mut bal),
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
            dicts: vec![],
            display: Default::default(),
            entry: FuncId(0),
            funcs: vec![Function {
                name: "f".into(),
                param_count: params,
                locals,
                ret,
                temps,
                blocks: vec![Block { insts, term }],
                elided_rc: Vec::new(),
                effects: None,
            }],
        }
    }

    /// `def f(xs: list[str]) -> None`: 本体は `body`（t0 = load xs、t1 = 0、t2 = xs[0] の後に続く命令）。
    fn list_fn(body: Vec<Inst>, extra_temps: Vec<Ty>) -> Program {
        let mut insts = vec![
            Inst::Load {
                dst: TempId(0),
                src: LocalId(0),
            },
            Inst::Const {
                dst: TempId(1),
                value: Const::Int(0),
            },
            Inst::ListGet {
                dst: TempId(2),
                src: TempId(0),
                index: TempId(1),
            },
        ];
        insts.extend(body);
        let mut temps = vec![Ty::List(ListId(0)), Ty::Int, Ty::Str];
        temps.extend(extra_temps);
        let mut p = str_fn(
            insts,
            temps,
            vec![Ty::List(ListId(0))],
            1,
            None,
            Terminator::Return(None),
        );
        p.lists = vec![Ty::Str];
        p
    }

    fn optimized(mut p: Program) -> (String, Vec<ElidedRcOp>) {
        insert(&mut p);
        optimize(&mut p);
        // 参照カウントの釣り合い（最適化の後でも成り立つこと）
        verify(&p).unwrap();
        (p.to_string(), p.funcs[0].elided_rc.clone())
    }

    #[test]
    fn a_parameter_that_is_never_reassigned_borrows_the_callers_reference() {
        // def f(s: str) -> str: return s（P4）
        let p = str_fn(
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
        let (text, elided) = optimized(p);
        assert!(!text.contains("param_slot_entry"), "{text}");
        assert!(!text.contains("slot_at_function_exit"), "{text}");
        // 戻り値として消費するための retain は残る
        assert!(text.contains("consume_borrowed:return"), "{text}");
        assert_eq!(elided.len(), 2);
        assert!(
            elided
                .iter()
                .all(|e| e.elision == ElisionReason::ParamNeverReassigned)
        );
        assert_eq!(elided[0].local, Some(LocalId(0)));
    }

    #[test]
    fn verify_rejects_a_parameter_slot_with_only_one_half_of_its_pair() {
        // P4 の組の片方（入口の retain）だけを取り除いた誤りを、検証器が見つける
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
        let insts = &mut p.funcs[0].blocks[0].insts;
        let pos = insts
            .iter()
            .position(|i| {
                matches!(
                    i,
                    Inst::Retain {
                        reason: RcReason::ParamSlotEntry,
                        ..
                    }
                )
            })
            .unwrap();
        insts.remove(pos);
        let err = verify(&p).unwrap_err();
        assert!(
            err.contains("no retain on entry but a release on exit"),
            "{err}"
        );
    }

    fn copy_then(extra: Vec<Inst>, temps: Vec<Ty>) -> Program {
        // l0 = "s"; l1 = l0; <extra>
        let mut insts = vec![
            Inst::Const {
                dst: TempId(0),
                value: Const::Str(StrId(0)),
            },
            Inst::Store {
                dst: LocalId(0),
                src: TempId(0),
            },
            Inst::Load {
                dst: TempId(1),
                src: LocalId(0),
            },
            Inst::Store {
                dst: LocalId(1),
                src: TempId(1),
            },
        ];
        insts.extend(extra);
        let mut ts = vec![Ty::Str, Ty::Str];
        ts.extend(temps);
        str_fn(
            insts,
            ts,
            vec![Ty::Str, Ty::Str],
            0,
            None,
            Terminator::Return(None),
        )
    }

    #[test]
    fn the_last_use_of_a_local_moves_out_of_its_slot() {
        // l0 = "s"; l1 = l0（l0 はもう読まない）→ take（P1）
        let (text, elided) = optimized(copy_then(vec![], vec![]));
        assert!(text.contains("t1 = take l0"), "{text}");
        // 移した値は retain しない（定数を l0 に入れる retain t0 は残る）
        assert!(!text.contains("retain t1"), "{text}");
        assert!(
            elided
                .iter()
                .any(|e| e.elision == ElisionReason::MovedFromSlot)
        );
    }

    #[test]
    fn a_local_read_again_is_not_moved() {
        // l0 = "s"; l1 = l0; l1 = l0: 最初の読み出しの後にも l0 を読むので、最初は take にしない
        let (text, _) = optimized(copy_then(
            vec![
                Inst::Load {
                    dst: TempId(2),
                    src: LocalId(0),
                },
                Inst::Store {
                    dst: LocalId(1),
                    src: TempId(2),
                },
            ],
            vec![Ty::Str],
        ));
        assert!(text.contains("t1 = load l0"), "{text}");
        // 最後の読み出しは移せる
        assert!(text.contains("t2 = take l0"), "{text}");
    }

    #[test]
    fn a_local_read_on_the_next_loop_iteration_is_not_moved() {
        // bb0: l0 = "s"; jump bb1 / bb1: l1 = l0; jump bb1（次の反復でも l0 を読む）
        let mut p = copy_then(vec![], vec![]);
        let f = &mut p.funcs[0];
        let tail = f.blocks[0].insts.split_off(2);
        f.blocks[0].term = Terminator::Jump(BlockId(1));
        f.blocks.push(Block {
            insts: tail,
            term: Terminator::Jump(BlockId(1)),
        });
        let (text, _) = optimized(p);
        assert!(!text.contains("take"), "{text}");
    }

    #[test]
    fn a_borrowed_parameter_is_never_moved() {
        // def f(s: str) -> str: return s（P4 で借りているだけのスロットから所有権は移せない）
        let p = str_fn(
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
        let (text, _) = optimized(p);
        assert!(!text.contains("take"), "{text}");
        assert!(text.contains("consume_borrowed:return"), "{text}");
    }

    #[test]
    fn a_reassigned_parameter_keeps_its_own_reference() {
        // def f(s: str) -> None: s = "s"
        let p = str_fn(
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
            1,
            None,
            Terminator::Return(None),
        );
        let (text, elided) = optimized(p);
        assert!(text.contains("param_slot_entry"), "{text}");
        assert!(text.contains("slot_at_function_exit"), "{text}");
        assert!(elided.is_empty());
    }

    #[test]
    fn a_list_element_used_only_by_print_needs_no_guard() {
        // print(xs[0])（P0）
        let p = list_fn(
            vec![Inst::Call {
                dst: None,
                callee: Callee::Builtin(Builtin::PrintStr),
                args: vec![TempId(2)],
            }],
            vec![],
        );
        let (text, elided) = optimized(p);
        assert!(!text.contains("borrow_from_mutable_place"), "{text}");
        assert!(!text.contains("unconsumed_owned_at_block_end"), "{text}");
        let guard: Vec<_> = elided
            .iter()
            .filter(|e| e.elision == ElisionReason::BorrowGuardUnneeded)
            .collect();
        assert_eq!(guard.len(), 2);
    }

    #[test]
    fn a_list_element_used_after_a_call_keeps_its_guard() {
        // y = xs[0]; f(xs); print(y): 呼ばれた関数が xs を書き換え得るので、保持は残す（借用の安全条件）
        let p = list_fn(
            vec![
                Inst::Call {
                    dst: None,
                    callee: Callee::Func(FuncId(0)),
                    args: vec![TempId(0)],
                },
                Inst::Call {
                    dst: None,
                    callee: Callee::Builtin(Builtin::PrintStr),
                    args: vec![TempId(2)],
                },
            ],
            vec![],
        );
        let (text, elided) = optimized(p);
        assert!(text.contains("borrow_from_mutable_place"), "{text}");
        assert!(text.contains("unconsumed_owned_at_block_end"), "{text}");
        assert!(
            elided
                .iter()
                .all(|e| e.elision != ElisionReason::BorrowGuardUnneeded)
        );
    }

    #[test]
    fn a_value_derived_from_a_list_element_keeps_the_guard() {
        // r = xs[0]; s = r.name（Field）; f(xs); print(s): `s` は `r` の保持に頼るので、保持は残す（PR #89 のレビュー）
        let mut p = list_fn(
            vec![
                Inst::Field {
                    dst: TempId(3),
                    src: TempId(2),
                    index: 0,
                },
                Inst::Call {
                    dst: None,
                    callee: Callee::Func(FuncId(0)),
                    args: vec![TempId(0)],
                },
                Inst::Call {
                    dst: None,
                    callee: Callee::Builtin(Builtin::PrintStr),
                    args: vec![TempId(3)],
                },
            ],
            vec![Ty::Str],
        );
        // 要素はフィールド1つのレコード
        p.records = vec![vec![("name".into(), Ty::Str)]];
        p.display.record_names = vec!["User".into()];
        p.lists = vec![Ty::Record(RecordId(0))];
        p.funcs[0].temps[2] = Ty::Record(RecordId(0));
        let (text, _) = optimized(p);
        assert!(text.contains("borrow_from_mutable_place"), "{text}");
    }

    #[test]
    fn a_list_element_passed_to_a_call_keeps_its_guard() {
        // f(xs[0]): 最後の使用が関数の呼び出しなら、保持は残す
        let mut p = list_fn(
            vec![Inst::Call {
                dst: None,
                callee: Callee::Func(FuncId(1)),
                args: vec![TempId(2)],
            }],
            vec![],
        );
        p.funcs.push(Function {
            name: "g".into(),
            param_count: 1,
            locals: vec![Ty::Str],
            ret: None,
            temps: vec![],
            blocks: vec![Block {
                insts: vec![],
                term: Terminator::Return(None),
            }],
            elided_rc: Vec::new(),
            effects: None,
        });
        let (text, _) = optimized(p);
        assert!(text.contains("borrow_from_mutable_place"), "{text}");
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
        assert!(
            text.contains("retain t0 ; consume_borrowed:store"),
            "{text}"
        );
        assert!(
            text.contains("release t1 ; store_overwrites_slot"),
            "{text}"
        ); // 古い値（null）
        assert!(
            text.contains("release t2 ; slot_at_function_exit"),
            "{text}"
        ); // 関数を抜けるときのローカル
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
        assert!(text.contains("; param_slot_entry"), "{text}");
        assert!(text.contains("; consume_borrowed:return"), "{text}");
        assert!(text.contains("; slot_at_function_exit"), "{text}");
    }

    #[test]
    fn list_element_reads_are_held_and_released_with_reasons() {
        // xs = [] の要素を読むだけで使わない: retain（借用の安全条件）と、ブロック末尾の release が対になる
        let mut p = str_fn(
            vec![
                Inst::Load {
                    dst: TempId(0),
                    src: LocalId(0),
                },
                Inst::Const {
                    dst: TempId(1),
                    value: Const::Int(0),
                },
                Inst::ListGet {
                    dst: TempId(2),
                    src: TempId(0),
                    index: TempId(1),
                },
            ],
            vec![Ty::List(ListId(0)), Ty::Int, Ty::Str],
            vec![Ty::List(ListId(0))],
            1,
            None,
            Terminator::Return(None),
        );
        p.lists.push(Ty::Str);
        insert(&mut p);
        verify(&p).unwrap();
        let reasons: Vec<RcReason> = p.funcs[0].blocks[0]
            .insts
            .iter()
            .filter_map(|i| match i {
                Inst::Retain { reason, .. } | Inst::Release { reason, .. } => Some(*reason),
                _ => None,
            })
            .collect();
        assert_eq!(
            reasons,
            vec![
                RcReason::ParamSlotEntry,
                RcReason::BorrowFromMutablePlace,
                RcReason::UnconsumedOwnedAtBlockEnd,
                RcReason::SlotAtFunctionExit,
            ]
        );
    }

    #[test]
    fn consuming_an_owned_value_twice_retains_the_second_use() {
        // r = f(); return make_union(r) と r の2回消費
        let mut p = str_fn(
            vec![
                Inst::Call {
                    dst: Some(TempId(0)),
                    callee: Callee::Func(FuncId(0)),
                    args: vec![],
                },
                Inst::Store {
                    dst: LocalId(0),
                    src: TempId(0),
                },
            ],
            vec![Ty::Str],
            vec![Ty::Str],
            0,
            Some(Ty::Str),
            Terminator::Return(Some(TempId(0))),
        );
        insert(&mut p);
        verify(&p).unwrap();
        let text = p.to_string();
        assert!(text.contains("; consume_shared_owned:return"), "{text}");
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
