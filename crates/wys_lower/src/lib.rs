//! AST から共通IRへの変換
//!
//! 型検査でエラーがなかったモジュールだけを受け取る前提。

use std::collections::HashMap;
use wys_ast::{self as ast, ExprKind, StmtKind, UnaryOp};
use wys_ir::*;
use wys_typeck::{Ty as TTy, TypeckResult};

fn ir_ty(t: TTy) -> Option<Ty> {
    match t {
        TTy::Int => Some(Ty::Int),
        TTy::Bool => Some(Ty::Bool),
        TTy::Str => Some(Ty::Str),
        TTy::None => None,
        TTy::Error => panic!("lowering received a type error; run the type checker first"),
        TTy::Union(_) => {
            panic!("union types are not lowered yet (M2 T1b); the driver must reject them first")
        }
    }
}

pub fn lower(module: &ast::Module, tc: &TypeckResult) -> Program {
    let func_ids: HashMap<&str, FuncId> = module
        .funcs
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.name.as_str(), FuncId(i as u32)))
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
    func_ids: &'a HashMap<&'a str, FuncId>,
    strings: &'a mut Vec<String>,
    locals: Vec<Ty>,
    local_ids: HashMap<String, LocalId>,
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
            func_ids,
            strings,
            locals: Vec::new(),
            local_ids: HashMap::new(),
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
                let v = self.expr(value).expect("assigned values are never None");
                let dst = self.local_ids[&target.name];
                self.emit(Inst::Store { dst, src: v });
            }
            StmtKind::Return(value) => {
                let v = value.as_ref().and_then(|e| self.expr(e));
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
                let src = self.local_ids[name];
                let ty = self.locals[src.0 as usize];
                let dst = self.temp(ty);
                self.emit(Inst::Load { dst, src });
                Some(dst)
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
            ExprKind::Is { .. } => {
                panic!(
                    "`is` expressions are not lowered yet (M2 T1b); the driver must reject them first"
                )
            }
            ExprKind::Call { callee, args } => {
                let arg_vals: Vec<TempId> = args
                    .iter()
                    .map(|a| self.expr(a).expect("arguments have values"))
                    .collect();
                let (callee, ret) = match callee.name.as_str() {
                    "print" => {
                        let b = match self.tc.ty(&args[0]) {
                            TTy::Int => Builtin::PrintInt,
                            TTy::Bool => Builtin::PrintBool,
                            TTy::Str => Builtin::PrintStr,
                            other => panic!("print of {other:?} passed type checking"),
                        };
                        (Callee::Builtin(b), None)
                    }
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
