//! AST から共通IRへの変換
//!
//! 型検査でエラーがなかったモジュールだけを受け取る前提。
//!
//! Union（ADR-0009）: 型が変わる地点（代入、引数、戻り値、絞り込まれた変数の読み出し）で
//! `coerce` が値を変換する。基本型 → Union は `MakeUnion`、Union → 基本型は `UnionPayload`、
//! Union → Union はタグで分岐して詰め直す。

use std::collections::HashMap;
use wys_ast::{self as ast, ExprKind, PatternKind, StmtKind, UnaryOp};
use wys_ir::*;
use wys_typeck::{Ty as TTy, TypeckResult};

fn ir_ty(t: TTy) -> Option<Ty> {
    match t {
        TTy::Int => Some(Ty::Int),
        TTy::Bool => Some(Ty::Bool),
        TTy::Str => Some(Ty::Str),
        TTy::None => None,
        TTy::Error => panic!("lowering received a type error; run the type checker first"),
        TTy::Union(u) => Some(Ty::Union(UnionId(u.0))),
    }
}

pub fn lower(module: &ast::Module, tc: &TypeckResult) -> Program {
    let func_ids: HashMap<&str, FuncId> = module
        .funcs
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.name.as_str(), FuncId(i as u32)))
        .collect();
    let unions = tc
        .types
        .unions()
        .iter()
        .map(|members| members.iter().map(|m| ir_ty(*m)).collect())
        .collect();
    let mut strings = Vec::new();
    let funcs = module
        .funcs
        .iter()
        .enumerate()
        .map(|(i, f)| FnLower::run(f, i, tc, &func_ids, &mut strings))
        .collect();
    Program {
        strings,
        unions,
        funcs,
        entry: func_ids["main"],
    }
}

struct Bb {
    insts: Vec<Inst>,
    term: Option<Terminator>,
}

struct FnLower<'a> {
    tc: &'a TypeckResult,
    ret: TTy,
    func_ids: &'a HashMap<&'a str, FuncId>,
    strings: &'a mut Vec<String>,
    locals: Vec<Ty>,
    local_ids: HashMap<String, LocalId>,
    /// ローカル変数の宣言された型（絞り込み前）。
    local_decl: HashMap<String, TTy>,
    temps: Vec<Ty>,
    blocks: Vec<Bb>,
    cur: usize,
}

impl<'a> FnLower<'a> {
    fn run(
        f: &ast::FuncDef,
        index: usize,
        tc: &'a TypeckResult,
        func_ids: &'a HashMap<&'a str, FuncId>,
        strings: &'a mut Vec<String>,
    ) -> Function {
        let info = &tc.funcs[index];
        let sig = &tc.sigs[index];
        let mut l = FnLower {
            tc,
            ret: sig.ret,
            func_ids,
            strings,
            locals: Vec::new(),
            local_ids: HashMap::new(),
            local_decl: HashMap::new(),
            temps: Vec::new(),
            blocks: vec![Bb {
                insts: Vec::new(),
                term: None,
            }],
            cur: 0,
        };
        for (name, ty) in &info.locals {
            let id = LocalId(l.locals.len() as u32);
            l.locals
                .push(ir_ty(*ty).expect("locals never have type None"));
            l.local_ids.insert(name.clone(), id);
            l.local_decl.insert(name.clone(), *ty);
        }
        l.stmts(&f.body);
        let ret = ir_ty(sig.ret);
        let blocks = l
            .blocks
            .into_iter()
            .map(|b| Block {
                insts: b.insts,
                term: b.term.unwrap_or(if ret.is_none() {
                    Terminator::Return(None)
                } else {
                    // 型検査で「すべての経路が return する」ことを確認済み。到達しない。
                    Terminator::Unreachable
                }),
            })
            .collect();
        Function {
            name: f.name.name.clone(),
            param_count: sig.params.len() as u32,
            locals: l.locals,
            ret,
            temps: l.temps,
            blocks,
        }
    }

    // ---- builder helpers ----

    fn new_block(&mut self) -> BlockId {
        self.blocks.push(Bb {
            insts: Vec::new(),
            term: None,
        });
        BlockId(self.blocks.len() as u32 - 1)
    }

    fn switch_to(&mut self, b: BlockId) {
        self.cur = b.0 as usize;
    }

    /// 終端済みのブロックに命令を追加しようとしたら、新しい（到達しない）ブロックに切り替える。
    fn ensure_open(&mut self) {
        if self.blocks[self.cur].term.is_some() {
            let b = self.new_block();
            self.switch_to(b);
        }
    }

    fn emit(&mut self, inst: Inst) {
        self.ensure_open();
        self.blocks[self.cur].insts.push(inst);
    }

    fn terminate(&mut self, term: Terminator) {
        self.ensure_open();
        self.blocks[self.cur].term = Some(term);
    }

    fn temp(&mut self, ty: Ty) -> TempId {
        self.temps.push(ty);
        TempId(self.temps.len() as u32 - 1)
    }

    fn hidden_local(&mut self, ty: Ty) -> LocalId {
        self.locals.push(ty);
        LocalId(self.locals.len() as u32 - 1)
    }

    fn konst(&mut self, value: Const, ty: Ty) -> TempId {
        let dst = self.temp(ty);
        self.emit(Inst::Const { dst, value });
        dst
    }

    // ---- statements ----

    fn stmts(&mut self, stmts: &[ast::Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &ast::Stmt) {
        match &s.kind {
            StmtKind::Pass => {}
            StmtKind::Expr(e) => {
                self.expr(e);
            }
            StmtKind::Assign { target, value, .. } => {
                let v = self.expr(value);
                let decl = self.local_decl[&target.name];
                let v = self
                    .coerce(v, self.tc.ty(value), decl)
                    .expect("variables never have type None");
                let dst = self.local_ids[&target.name];
                self.emit(Inst::Store { dst, src: v });
            }
            StmtKind::Return(value) => {
                let v = value.as_ref().and_then(|e| {
                    let v = self.expr(e);
                    if self.ret == TTy::None {
                        None
                    } else {
                        self.coerce(v, self.tc.ty(e), self.ret)
                    }
                });
                self.terminate(Terminator::Return(v));
            }
            StmtKind::If {
                branches,
                else_body,
            } => {
                let join = self.new_block();
                for (cond, body) in branches {
                    let c = self.expr(cond).expect("conditions are bool");
                    let then_b = self.new_block();
                    let next_b = self.new_block();
                    self.terminate(Terminator::Branch {
                        cond: c,
                        then_block: then_b,
                        else_block: next_b,
                    });
                    self.switch_to(then_b);
                    self.stmts(body);
                    if self.blocks[self.cur].term.is_none() {
                        self.terminate(Terminator::Jump(join));
                    }
                    self.switch_to(next_b);
                }
                if let Some(body) = else_body {
                    self.stmts(body);
                }
                if self.blocks[self.cur].term.is_none() {
                    self.terminate(Terminator::Jump(join));
                }
                self.switch_to(join);
            }
            StmtKind::Match { subject, arms } => self.match_stmt(subject, arms),
            StmtKind::While { cond, body } => {
                let head = self.new_block();
                let body_b = self.new_block();
                let exit = self.new_block();
                self.terminate(Terminator::Jump(head));
                self.switch_to(head);
                let c = self.expr(cond).expect("conditions are bool");
                self.terminate(Terminator::Branch {
                    cond: c,
                    then_block: body_b,
                    else_block: exit,
                });
                self.switch_to(body_b);
                self.stmts(body);
                if self.blocks[self.cur].term.is_none() {
                    self.terminate(Terminator::Jump(head));
                }
                self.switch_to(exit);
            }
        }
    }

    /// `match`: 被検査値を一度だけ評価してローカルに置き、`case` を上から順にタグで判定する。
    /// 型パターンが被検査値の型そのもの（Union でない）なら常に一致する。
    fn match_stmt(&mut self, subject: &ast::Expr, arms: &[ast::MatchArm]) {
        let st = self.tc.ty(subject);
        let v = self.expr(subject);
        let slot = v.map(|t| {
            let ty = self.temps[t.0 as usize];
            let l = self.hidden_local(ty);
            self.emit(Inst::Store { dst: l, src: t });
            l
        });
        let join = self.new_block();
        for arm in arms {
            let pattern_ty = match &arm.pattern.kind {
                PatternKind::Wildcard => None,
                PatternKind::None => Some(TTy::None),
                PatternKind::Type(name) => Some(match name.as_str() {
                    "int" => TTy::Int,
                    "bool" => TTy::Bool,
                    "str" => TTy::Str,
                    other => panic!("unknown pattern type `{other}` passed type checking"),
                }),
            };
            let next = self.new_block();
            if let (Some(pt), TTy::Union(u), Some(slot)) = (pattern_ty, st, slot) {
                let val = self.temp(self.locals[slot.0 as usize]);
                self.emit(Inst::Load {
                    dst: val,
                    src: slot,
                });
                let c = self.temp(Ty::Bool);
                let variant = self.variant(u, pt);
                self.emit(Inst::UnionIs {
                    dst: c,
                    src: val,
                    variant,
                });
                let body = self.new_block();
                self.terminate(Terminator::Branch {
                    cond: c,
                    then_block: body,
                    else_block: next,
                });
                self.switch_to(body);
            }
            self.stmts(&arm.body);
            if self.blocks[self.cur].term.is_none() {
                self.terminate(Terminator::Jump(join));
            }
            self.switch_to(next);
        }
        // 型検査で網羅性を確認済みなので、ここには到達しない
        self.terminate(Terminator::Unreachable);
        self.switch_to(join);
    }

    // ---- expressions ----

    /// 式を評価し、値があればその一時値を返す（`None` 型なら `None`）。
    fn expr(&mut self, e: &ast::Expr) -> Option<TempId> {
        match &e.kind {
            ExprKind::Int(v) => Some(self.konst(Const::Int(*v), Ty::Int)),
            ExprKind::Bool(v) => Some(self.konst(Const::Bool(*v), Ty::Bool)),
            ExprKind::Str(s) => {
                let id = StrId(self.strings.len() as u32);
                self.strings.push(s.clone());
                Some(self.konst(Const::Str(id), Ty::Str))
            }
            ExprKind::None => None,
            ExprKind::Name(name) => {
                let narrowed = self.tc.ty(e);
                if narrowed == TTy::None {
                    return None; // 絞り込みで None と確定した変数
                }
                let src = self.local_ids[name];
                let ty = self.locals[src.0 as usize];
                let dst = self.temp(ty);
                self.emit(Inst::Load { dst, src });
                self.coerce(Some(dst), self.local_decl[name], narrowed)
            }
            ExprKind::Is { lhs, negated, .. } => {
                let lt = self.tc.ty(lhs);
                let v = self.expr(lhs);
                let is_none = match lt {
                    TTy::None => self.konst(Const::Bool(true), Ty::Bool),
                    TTy::Union(u) => {
                        let dst = self.temp(Ty::Bool);
                        let variant = self.variant(u, TTy::None);
                        self.emit(Inst::UnionIs {
                            dst,
                            src: v.expect("union values exist"),
                            variant,
                        });
                        dst
                    }
                    other => panic!("`is None` on {other:?} passed type checking"),
                };
                if *negated {
                    let dst = self.temp(Ty::Bool);
                    self.emit(Inst::Not { dst, src: is_none });
                    Some(dst)
                } else {
                    Some(is_none)
                }
            }
            ExprKind::Unary { op, operand } => {
                let v = self.expr(operand).expect("unary operands have values");
                Some(match op {
                    UnaryOp::Neg => {
                        let zero = self.konst(Const::Int(0), Ty::Int);
                        let dst = self.temp(Ty::Int);
                        self.emit(Inst::Binary {
                            dst,
                            op: BinOp::Sub,
                            lhs: zero,
                            rhs: v,
                        });
                        dst
                    }
                    UnaryOp::Not => {
                        let dst = self.temp(Ty::Bool);
                        self.emit(Inst::Not { dst, src: v });
                        dst
                    }
                })
            }
            ExprKind::Binary { op, lhs, rhs } => Some(self.binary(*op, lhs, rhs)),
            ExprKind::Call { callee, args } => {
                if callee.name == "print" {
                    let v = self.expr(&args[0]).expect("print arguments have values");
                    self.print(v, self.tc.ty(&args[0]));
                    return None;
                }
                let params: Vec<TTy> = match callee.name.as_str() {
                    "exit" => vec![TTy::Int],
                    name => self.tc.sigs[self.func_ids[name].0 as usize].params.clone(),
                };
                let mut arg_vals = Vec::new();
                for (a, pt) in args.iter().zip(params) {
                    let v = self.expr(a);
                    let v = self.coerce(v, self.tc.ty(a), pt);
                    arg_vals.push(v.expect("arguments have values"));
                }
                let (callee, ret) = match callee.name.as_str() {
                    "exit" => (Callee::Builtin(Builtin::Exit), None),
                    name => {
                        let id = self.func_ids[name];
                        (Callee::Func(id), ir_ty(self.tc.sigs[id.0 as usize].ret))
                    }
                };
                let dst = ret.map(|t| self.temp(t));
                self.emit(Inst::Call {
                    dst,
                    callee,
                    args: arg_vals,
                });
                dst
            }
        }
    }

    // ---- unions ----

    fn members(&self, u: wys_typeck::UnionId) -> Vec<TTy> {
        self.tc.types.members(TTy::Union(u))
    }

    fn variant(&self, u: wys_typeck::UnionId, m: TTy) -> u32 {
        self.members(u)
            .iter()
            .position(|x| *x == m)
            .unwrap_or_else(|| panic!("{m:?} is not a member of union {u:?}")) as u32
    }

    fn make_union(&mut self, u: wys_typeck::UnionId, m: TTy, payload: Option<TempId>) -> TempId {
        let dst = self.temp(Ty::Union(UnionId(u.0)));
        let variant = self.variant(u, m);
        self.emit(Inst::MakeUnion {
            dst,
            union: UnionId(u.0),
            variant,
            payload,
        });
        dst
    }

    fn payload(&mut self, v: TempId, u: wys_typeck::UnionId, m: TTy) -> Option<TempId> {
        let ty = ir_ty(m)?;
        let dst = self.temp(ty);
        let variant = self.variant(u, m);
        self.emit(Inst::UnionPayload {
            dst,
            src: v,
            variant,
        });
        Some(dst)
    }

    /// 値 `v`（型 `from`）を型 `to` の値に変換する。型検査で `from` の値が `to` に収まることは保証済み
    /// （絞り込みの結果、`to` が `from` より狭い場合を含む）。
    fn coerce(&mut self, v: Option<TempId>, from: TTy, to: TTy) -> Option<TempId> {
        if from == to {
            return v;
        }
        match (from, to) {
            (TTy::Union(fu), TTy::Union(tu)) => {
                let v = v.expect("union values exist");
                Some(self.union_to_union(v, fu, tu))
            }
            (TTy::Union(fu), base) => self.payload(v.expect("union values exist"), fu, base),
            (base, TTy::Union(tu)) => Some(self.make_union(tu, base, v)),
            _ => panic!("no conversion from {from:?} to {to:?}"),
        }
    }

    /// Union から Union へ: タグで分岐し、共通の構成要素ごとに詰め直す。
    fn union_to_union(
        &mut self,
        v: TempId,
        fu: wys_typeck::UnionId,
        tu: wys_typeck::UnionId,
    ) -> TempId {
        let target = self.members(tu);
        let common: Vec<TTy> = self
            .members(fu)
            .into_iter()
            .filter(|m| target.contains(m))
            .collect();
        let src = self.hidden_local(Ty::Union(UnionId(fu.0)));
        self.emit(Inst::Store { dst: src, src: v });
        let out = self.hidden_local(Ty::Union(UnionId(tu.0)));
        self.for_each_variant(src, fu, &common, |l, val, m| {
            let p = l.payload(val, fu, m);
            let u = l.make_union(tu, m, p);
            l.emit(Inst::Store { dst: out, src: u });
        });
        let dst = self.temp(Ty::Union(UnionId(tu.0)));
        self.emit(Inst::Load { dst, src: out });
        dst
    }

    /// ローカル `src` に入った Union 値について、`variants` のどれに当たるかで分岐し、それぞれで `f` を実行する。
    /// 最後の構成要素は検査せずに実行する（型検査で、値がこのどれかであることは保証済み）。
    fn for_each_variant(
        &mut self,
        src: LocalId,
        u: wys_typeck::UnionId,
        variants: &[TTy],
        mut f: impl FnMut(&mut Self, TempId, TTy),
    ) {
        let src_ty = self.locals[src.0 as usize];
        let join = self.new_block();
        for (i, &m) in variants.iter().enumerate() {
            let last = i + 1 == variants.len();
            let next = if last {
                None
            } else {
                let val = self.temp(src_ty);
                self.emit(Inst::Load { dst: val, src });
                let c = self.temp(Ty::Bool);
                let variant = self.variant(u, m);
                self.emit(Inst::UnionIs {
                    dst: c,
                    src: val,
                    variant,
                });
                let then_b = self.new_block();
                let next_b = self.new_block();
                self.terminate(Terminator::Branch {
                    cond: c,
                    then_block: then_b,
                    else_block: next_b,
                });
                self.switch_to(then_b);
                Some(next_b)
            };
            let val = self.temp(src_ty);
            self.emit(Inst::Load { dst: val, src });
            f(self, val, m);
            self.terminate(Terminator::Jump(join));
            if let Some(n) = next {
                self.switch_to(n);
            }
        }
        self.switch_to(join);
    }

    fn print(&mut self, v: TempId, ty: TTy) {
        fn builtin(t: TTy) -> Builtin {
            match t {
                TTy::Int => Builtin::PrintInt,
                TTy::Bool => Builtin::PrintBool,
                TTy::Str => Builtin::PrintStr,
                other => panic!("print of {other:?} passed type checking"),
            }
        }
        match ty {
            TTy::Union(u) => {
                let src = self.hidden_local(Ty::Union(UnionId(u.0)));
                self.emit(Inst::Store { dst: src, src: v });
                let members = self.members(u);
                self.for_each_variant(src, u, &members, |l, val, m| {
                    let p = l
                        .payload(val, u, m)
                        .expect("printable members have payloads");
                    l.emit(Inst::Call {
                        dst: None,
                        callee: Callee::Builtin(builtin(m)),
                        args: vec![p],
                    });
                });
            }
            base => self.emit(Inst::Call {
                dst: None,
                callee: Callee::Builtin(builtin(base)),
                args: vec![v],
            }),
        }
    }

    fn binary(&mut self, op: ast::BinOp, lhs: &ast::Expr, rhs: &ast::Expr) -> TempId {
        use ast::BinOp as A;
        if matches!(op, A::And | A::Or) {
            return self.short_circuit(op == A::And, lhs, rhs);
        }
        let l = self.expr(lhs).expect("operands have values");
        let r = self.expr(rhs).expect("operands have values");
        let (iop, ty) = match op {
            A::Add => (BinOp::Add, Ty::Int),
            A::Sub => (BinOp::Sub, Ty::Int),
            A::Mul => (BinOp::Mul, Ty::Int),
            A::FloorDiv => (BinOp::FloorDiv, Ty::Int),
            A::Mod => (BinOp::Mod, Ty::Int),
            A::Eq => (BinOp::Eq, Ty::Bool),
            A::Ne => (BinOp::Ne, Ty::Bool),
            A::Lt => (BinOp::Lt, Ty::Bool),
            A::Le => (BinOp::Le, Ty::Bool),
            A::Gt => (BinOp::Gt, Ty::Bool),
            A::Ge => (BinOp::Ge, Ty::Bool),
            A::Div | A::And | A::Or => {
                unreachable!("rejected by the type checker or handled above")
            }
        };
        let dst = self.temp(ty);
        self.emit(Inst::Binary {
            dst,
            op: iop,
            lhs: l,
            rhs: r,
        });
        dst
    }

    /// `a and b` / `a or b` を、結果を隠しローカルに書く分岐に変換する。
    fn short_circuit(&mut self, is_and: bool, lhs: &ast::Expr, rhs: &ast::Expr) -> TempId {
        let slot = self.hidden_local(Ty::Bool);
        let l = self.expr(lhs).expect("operands have values");
        self.emit(Inst::Store { dst: slot, src: l });
        let rhs_b = self.new_block();
        let end_b = self.new_block();
        let (then_block, else_block) = if is_and {
            (rhs_b, end_b)
        } else {
            (end_b, rhs_b)
        };
        self.terminate(Terminator::Branch {
            cond: l,
            then_block,
            else_block,
        });
        self.switch_to(rhs_b);
        let r = self.expr(rhs).expect("operands have values");
        self.emit(Inst::Store { dst: slot, src: r });
        self.terminate(Terminator::Jump(end_b));
        self.switch_to(end_b);
        let dst = self.temp(Ty::Bool);
        self.emit(Inst::Load { dst, src: slot });
        dst
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower_src(src: &str) -> Program {
        let (m, d) = wys_parser::parse_source(src);
        assert!(d.is_empty(), "{d:?}");
        let (tc, d) = wys_typeck::check(&m);
        assert!(d.is_empty(), "{d:?}");
        let p = lower(&m, &tc);
        verify(&p).unwrap_or_else(|e| panic!("{e}\n{p}"));
        p
    }

    #[test]
    fn lowers_and_verifies_control_flow() {
        let p = lower_src(
            "def fib(n: int) -> int:\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\n\ndef main() -> None:\n    i = 0\n    while i < 10 and not False:\n        print(fib(i))\n        i = i + 1\n    if i == 10 or i == 11:\n        print(\"done\")\n    elif i > 100:\n        return\n    else:\n        print(-i)\n",
        );
        assert_eq!(p.funcs.len(), 2);
        assert_eq!(p.entry, FuncId(1));
    }

    #[test]
    fn dead_code_after_return_still_verifies() {
        lower_src(
            "def f() -> int:\n    return 1\n    print(2)\n\ndef main() -> None:\n    print(f())\n",
        );
    }

    #[test]
    fn ir_dump_is_stable() {
        let p = lower_src("def main() -> None:\n    x = 1 + 2\n    print(x)\n");
        assert_eq!(
            p.to_string(),
            "entry f0\n\nfn f0 main() {\n  local l0: int\nbb0:\n  t0 = const 1\n  t1 = const 2\n  t2 = add t0, t1\n  store l0, t2\n  t3 = load l0\n  call @print_int(t3)\n  return\n}\n"
        );
    }
}
