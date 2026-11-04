//! AST から共通IRへの変換
//!
//! 型検査でエラーがなかったモジュールだけを受け取る前提。
//!
//! Union（ADR-0009）: 型が変わる地点（代入、引数、戻り値、絞り込まれた変数の読み出し）で
//! `coerce` が値を変換する。基本型 → Union は `MakeUnion`、Union → 基本型は `UnionPayload`、
//! Union → Union はタグで分岐して詰め直す。

use std::collections::{HashMap, VecDeque};
use wys_ast::{self as ast, ExprKind, PatternKind, StmtKind, UnaryOp};
use wys_ir::*;
use wys_typeck::{Subst, Ty as TTy, TypeckResult, Types};

/// 関数をまたいで共有する状態: 型の登録表（単相化で増える）、IR の型 ID の対応、文字列、関数のインスタンス。
struct Shared<'t> {
    tc: &'t TypeckResult,
    types: Types,
    union_ids: HashMap<wys_typeck::UnionId, UnionId>,
    unions: Vec<Vec<Member>>,
    record_ids: HashMap<wys_typeck::RecordId, RecordId>,
    records: Vec<Vec<(String, Ty)>>,
    strings: Vec<String>,
    func_index: HashMap<String, usize>,
    /// (関数の番号, 型引数) → IR の関数。Generic でない関数は型引数が空。
    instances: HashMap<(usize, Vec<TTy>), FuncId>,
    queue: VecDeque<(usize, Vec<TTy>, FuncId)>,
    /// ソース上の位置を「ファイル:行:列」にする（`assert` の失敗メッセージ用）
    locate: &'t dyn Fn(u32) -> String,
    /// 単相化を打ち切った理由（多相再帰など）。あれば IR を作らずに診断を返す
    error: Option<wys_diagnostics::Diagnostic>,
}

/// 型引数の入れ子の深さの上限。多相再帰（`f[T]` が `f[Pair[T, T]]` を呼ぶなど）で、具体化が終わらないのを防ぐ。
const MAX_INSTANCE_TYPE_DEPTH: usize = 32;

impl Shared<'_> {
    /// 型検査の型を IR の型にする。Union とレコードは、使われたものだけを IR に登録する。
    fn ir_ty(&mut self, t: TTy) -> Option<Ty> {
        match t {
            TTy::Int => Some(Ty::Int),
            TTy::Bool => Some(Ty::Bool),
            TTy::Str => Some(Ty::Str),
            TTy::None => None,
            TTy::Union(u) => Some(Ty::Union(self.ir_union(u))),
            TTy::Record(r) => Some(Ty::Record(self.ir_record(r))),
            TTy::Param(_) => {
                panic!("type parameter reached code generation; monomorphization missed it")
            }
            TTy::Unknown => {
                let u = self.unknown_union();
                Some(Ty::Union(self.ir_union(u)))
            }
            TTy::Error => panic!("lowering received a type error; run the type checker first"),
        }
    }

    /// `Unknown` の実行時表現（ADR-0011）: `int | bool | str | None` の Union。
    fn unknown_union(&mut self) -> wys_typeck::UnionId {
        match self.types.union([TTy::Int, TTy::Bool, TTy::Str, TTy::None]) {
            TTy::Union(u) => u,
            _ => unreachable!("four members always form a union"),
        }
    }

    /// `Unknown` を実行時表現の Union に置き換える（レコードのフィールドや Union の構成要素の中も）。
    fn norm(&mut self, t: TTy) -> TTy {
        match t {
            TTy::Unknown => TTy::Union(self.unknown_union()),
            TTy::Union(_) if self.types.members(t).contains(&TTy::Unknown) => {
                let ms: Vec<TTy> = self
                    .types
                    .members(t)
                    .into_iter()
                    .map(|m| self.norm(m))
                    .collect();
                self.types.union(ms)
            }
            TTy::Record(r) => {
                let def = self.types.record_def(r).clone();
                if !def.fields.iter().any(|(_, ft)| self.has_unknown(*ft)) {
                    return t;
                }
                let fields = def
                    .fields
                    .iter()
                    .map(|(n, ft)| (n.clone(), self.norm(*ft)))
                    .collect();
                self.types.record_named(fields, def.name, def.args)
            }
            _ => t,
        }
    }

    fn has_unknown(&self, t: TTy) -> bool {
        self.types.contains_unknown(t)
    }

    fn ir_union(&mut self, u: wys_typeck::UnionId) -> UnionId {
        if let Some(&id) = self.union_ids.get(&u) {
            return id;
        }
        let members: Vec<Member> = self
            .types
            .members(TTy::Union(u))
            .into_iter()
            .map(|m| self.ir_ty(m))
            .collect();
        let id = UnionId(self.unions.len() as u32);
        self.unions.push(members);
        self.union_ids.insert(u, id);
        id
    }

    fn ir_record(&mut self, r: wys_typeck::RecordId) -> RecordId {
        if let Some(&id) = self.record_ids.get(&r) {
            return id;
        }
        let fields: Vec<(String, Ty)> = self
            .types
            .record_def(r)
            .fields
            .clone()
            .into_iter()
            .map(|(n, t)| (n, self.ir_ty(t).expect("record fields are never None")))
            .collect();
        let id = RecordId(self.records.len() as u32);
        self.records.push(fields);
        self.record_ids.insert(r, id);
        id
    }

    /// 関数のインスタンス（Generic でなければ型引数は空）を求め、まだなければ生成待ちに積む。
    fn request(&mut self, index: usize, args: Vec<TTy>, at: wys_span::Span) -> FuncId {
        let key = (index, args);
        if let Some(&id) = self.instances.get(&key) {
            return id;
        }
        if self.error.is_some() {
            return FuncId(0);
        }
        if key
            .1
            .iter()
            .any(|t| self.types.depth(*t) > MAX_INSTANCE_TYPE_DEPTH)
        {
            let name = &self.tc.sigs[index].name;
            self.error = Some(
                wys_diagnostics::Diagnostic::error(
                    "E0404",
                    format!("generic function `{name}` is instantiated with ever larger types (polymorphic recursion)"),
                    at,
                )
                .with_cause(format!(
                    "its type arguments become nested more than {MAX_INSTANCE_TYPE_DEPTH} levels deep, so instantiation would never end"
                ))
                .with_hint("call the function recursively with the same type arguments, or make it non-generic"),
            );
            return FuncId(0);
        }
        let id = FuncId(self.instances.len() as u32);
        self.instances.insert(key.clone(), id);
        self.queue.push_back((key.0, key.1, id));
        id
    }
}

/// IR を作るときの指定。
pub struct LowerOptions<'a> {
    /// 空でなければ、`main` の代わりに、これらの関数（`module.funcs` の番号）を `wys test` 用に呼び分けるエントリを作る。
    pub tests: Vec<usize>,
    pub locate: &'a dyn Fn(u32) -> String,
}

/// AST と型検査の結果から IR を作る。Generic 関数は、使われた型引数ごとに単相化する（ADR-0006）。
pub fn lower(module: &ast::Module, tc: &TypeckResult) -> Program {
    try_lower(module, tc).expect("lowering failed; use lower_with to get the diagnostic")
}

fn try_lower(
    module: &ast::Module,
    tc: &TypeckResult,
) -> Result<Program, Box<wys_diagnostics::Diagnostic>> {
    lower_with(
        module,
        tc,
        &LowerOptions {
            tests: Vec::new(),
            locate: &|b| format!("byte {b}"),
        },
    )
}

/// IR を作る。単相化を打ち切った場合（多相再帰、E0404）は診断を返す。
pub fn lower_with(
    module: &ast::Module,
    tc: &TypeckResult,
    opts: &LowerOptions<'_>,
) -> Result<Program, Box<wys_diagnostics::Diagnostic>> {
    let mut sh = Shared {
        tc,
        types: tc.types.clone(),
        union_ids: HashMap::new(),
        unions: Vec::new(),
        record_ids: HashMap::new(),
        records: Vec::new(),
        strings: Vec::new(),
        func_index: module
            .funcs
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.name.clone(), i))
            .collect(),
        instances: HashMap::new(),
        queue: VecDeque::new(),
        locate: opts.locate,
        error: None,
    };
    // Generic でない関数を先に登録する（関数番号はソースの順になる）
    for (i, sig) in tc.sigs.iter().enumerate() {
        if sig.type_params.is_empty() {
            sh.request(i, Vec::new(), wys_span::Span::new(0, 0));
        }
    }
    let mut funcs: Vec<Option<Function>> = Vec::new();
    while let Some((i, args, id)) = sh.queue.pop_front() {
        if sh.error.is_some() {
            break;
        }
        let f = FnLower::run(&module.funcs[i], i, args, id, &mut sh);
        if funcs.len() <= id.0 as usize {
            funcs.resize(id.0 as usize + 1, None);
        }
        funcs[id.0 as usize] = Some(f);
    }
    if let Some(d) = sh.error {
        return Err(Box::new(d));
    }
    let mut funcs: Vec<Function> = funcs
        .into_iter()
        .map(|f| f.expect("every instance is lowered"))
        .collect();
    let entry = if opts.tests.is_empty() {
        sh.instances[&(sh.func_index["main"], Vec::new())]
    } else {
        let ids: Vec<FuncId> = opts
            .tests
            .iter()
            .map(|i| sh.instances[&(*i, Vec::new())])
            .collect();
        funcs.push(test_dispatcher(&ids));
        FuncId(funcs.len() as u32 - 1)
    };
    Ok(Program {
        strings: sh.strings,
        unions: sh.unions,
        records: sh.records,
        funcs,
        entry,
    })
}

/// `wys test` のエントリ: 環境変数 `WYS_TEST_INDEX` の番号のテスト関数を1つだけ呼ぶ。
fn test_dispatcher(tests: &[FuncId]) -> Function {
    let mut temps = vec![Ty::Int];
    let mut blocks = vec![Block {
        insts: vec![
            Inst::Call {
                dst: Some(TempId(0)),
                callee: Callee::Builtin(Builtin::TestIndex),
                args: vec![],
            },
            Inst::Store {
                dst: LocalId(0),
                src: TempId(0),
            },
        ],
        term: Terminator::Jump(BlockId(1)),
    }];
    for (i, id) in tests.iter().enumerate() {
        let check = blocks.len() as u32;
        let (k, c, eq) = (
            temps.len() as u32,
            temps.len() as u32 + 1,
            temps.len() as u32 + 2,
        );
        temps.extend([Ty::Int, Ty::Int, Ty::Bool]);
        blocks.push(Block {
            insts: vec![
                Inst::Load {
                    dst: TempId(k),
                    src: LocalId(0),
                },
                Inst::Const {
                    dst: TempId(c),
                    value: Const::Int(i as i64),
                },
                Inst::Binary {
                    dst: TempId(eq),
                    op: BinOp::Eq,
                    lhs: TempId(k),
                    rhs: TempId(c),
                },
            ],
            term: Terminator::Branch {
                cond: TempId(eq),
                then_block: BlockId(check + 1),
                else_block: BlockId(check + 2),
            },
        });
        blocks.push(Block {
            insts: vec![Inst::Call {
                dst: None,
                callee: Callee::Func(*id),
                args: vec![],
            }],
            term: Terminator::Return(None),
        });
    }
    blocks.push(Block {
        insts: vec![],
        term: Terminator::Return(None),
    });
    Function {
        name: "__wys_tests".into(),
        param_count: 0,
        locals: vec![Ty::Int],
        ret: None,
        temps,
        blocks,
    }
}

struct Bb {
    insts: Vec<Inst>,
    term: Option<Terminator>,
}

struct FnLower<'s, 't> {
    sh: &'s mut Shared<'t>,
    /// この関数インスタンスの型パラメータの置き換え（Generic でなければ空）。
    subst: Subst,
    ret: TTy,
    locals: Vec<Ty>,
    local_ids: HashMap<String, LocalId>,
    /// ローカル変数の宣言された型（絞り込み前）。
    local_decl: HashMap<String, TTy>,
    temps: Vec<Ty>,
    blocks: Vec<Bb>,
    cur: usize,
}

impl<'s, 't> FnLower<'s, 't> {
    fn run(
        f: &ast::FuncDef,
        index: usize,
        args: Vec<TTy>,
        id: FuncId,
        sh: &'s mut Shared<'t>,
    ) -> Function {
        let tc = sh.tc;
        let info = &tc.funcs[index];
        let sig = &tc.sigs[index];
        let subst: Subst = sig
            .type_params
            .iter()
            .copied()
            .zip(args.iter().copied())
            .collect();
        let ret = sh.types.subst(sig.ret, &subst);
        let ret = sh.norm(ret);
        let name = if args.is_empty() {
            f.name.name.clone()
        } else {
            format!("{}__{}", f.name.name, id.0)
        };
        let mut l = FnLower {
            sh,
            subst,
            ret,
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
        for (lname, ty) in &info.locals {
            let lid = LocalId(l.locals.len() as u32);
            let decl = l.sh.types.subst(*ty, &l.subst);
            let decl = l.sh.norm(decl);
            let ir = l.sh.ir_ty(decl).expect("locals never have type None");
            l.locals.push(ir);
            l.local_ids.insert(lname.clone(), lid);
            l.local_decl.insert(lname.clone(), decl);
        }
        l.stmts(&f.body);
        let ret = l.sh.ir_ty(l.ret);
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
            name,
            param_count: sig.params.len() as u32,
            locals: l.locals,
            ret,
            temps: l.temps,
            blocks,
        }
    }

    /// 式の型（この関数インスタンスの型引数で置き換えた後）。
    fn ty(&mut self, e: &ast::Expr) -> TTy {
        let t = self.sh.tc.ty(e);
        let t = self.sh.types.subst(t, &self.subst);
        self.sh.norm(t)
    }

    /// 呼び出し先の関数インスタンスと、その引数・戻り値の型。
    fn callee(&mut self, name: &str, call: &ast::Expr) -> (FuncId, Vec<TTy>, TTy) {
        let index = self.sh.func_index[name];
        let sig = &self.sh.tc.sigs[index];
        let (type_params, params, ret) = (sig.type_params.clone(), sig.params.clone(), sig.ret);
        let args: Vec<TTy> = if type_params.is_empty() {
            Vec::new()
        } else {
            self.sh.tc.call_type_args[&call.id]
                .clone()
                .into_iter()
                .map(|t| self.sh.types.subst(t, &self.subst))
                .collect()
        };
        let map: Subst = type_params
            .iter()
            .copied()
            .zip(args.iter().copied())
            .collect();
        let params = params
            .iter()
            .map(|t| {
                let t = self.sh.types.subst(*t, &map);
                self.sh.norm(t)
            })
            .collect();
        let ret = self.sh.types.subst(ret, &map);
        let ret = self.sh.norm(ret);
        (self.sh.request(index, args, call.span), params, ret)
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

    /// ブロック `def_block` で定義した一時値を、現在のブロックで使える値にする。
    /// 後続の式（`and` / `or` など）でブロックが変わった場合は、定義したブロックの末尾でローカルに退避し、
    /// 現在のブロックで読み直す（一時値はブロック内でのみ使えるという IR の規則のため）。
    fn materialize(&mut self, t: TempId, def_block: usize) -> TempId {
        if def_block == self.cur {
            return t;
        }
        let ty = self.temps[t.0 as usize];
        let slot = self.hidden_local(ty);
        self.blocks[def_block]
            .insts
            .push(Inst::Store { dst: slot, src: t });
        let dst = self.temp(ty);
        self.emit(Inst::Load { dst, src: slot });
        dst
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
                let vt = self.ty(value);
                let v = self
                    .coerce(v, vt, decl)
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
                        let (vt, ret) = (self.ty(e), self.ret);
                        self.coerce(v, vt, ret)
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
        let st = self.ty(subject);
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
                let id = StrId(self.sh.strings.len() as u32);
                self.sh.strings.push(s.clone());
                Some(self.konst(Const::Str(id), Ty::Str))
            }
            ExprKind::None => None,
            ExprKind::Name(name) => {
                let narrowed = self.ty(e);
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
                let lt = self.ty(lhs);
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
            ExprKind::Field { base, name } => {
                let v = self.expr(base).expect("records have values");
                let TTy::Record(r) = self.ty(base) else {
                    panic!("field access on a non-record passed type checking")
                };
                let (index, ft) = self
                    .sh
                    .types
                    .record_def(r)
                    .field(&name.name)
                    .expect("fields exist after type checking");
                let fty = self.sh.ir_ty(ft).expect("record fields are never None");
                let dst = self.temp(fty);
                self.emit(Inst::Field {
                    dst,
                    src: v,
                    index: index as u32,
                });
                Some(dst)
            }
            ExprKind::Call { callee, kwargs, .. } if self.is_record_constructor(&callee.name) => {
                let TTy::Record(r) = self.ty(e) else {
                    panic!("record construction without a record type")
                };
                Some(self.construct(r, kwargs))
            }
            ExprKind::Call { callee, args, .. }
                if matches!(callee.name.as_str(), "arg_count" | "arg" | "parse_scalar") =>
            {
                Some(match callee.name.as_str() {
                    "arg_count" => {
                        let d = self.temp(Ty::Int);
                        self.emit(Inst::Call {
                            dst: Some(d),
                            callee: Callee::Builtin(Builtin::ArgCount),
                            args: vec![],
                        });
                        d
                    }
                    "arg" => self.lower_arg(&args[0], e),
                    _ => self.lower_parse_scalar(&args[0], e),
                })
            }
            ExprKind::Call { callee, args, .. } if callee.name == "assert" => {
                let c = self.expr(&args[0]).expect("conditions are bool");
                let (ok, fail) = (self.new_block(), self.new_block());
                self.terminate(Terminator::Branch {
                    cond: c,
                    then_block: ok,
                    else_block: fail,
                });
                self.switch_to(fail);
                let loc = (self.sh.locate)(e.span.start);
                let id = StrId(self.sh.strings.len() as u32);
                self.sh.strings.push(loc);
                let s = self.konst(Const::Str(id), Ty::Str);
                self.emit(Inst::Call {
                    dst: None,
                    callee: Callee::Builtin(Builtin::AssertFail),
                    args: vec![s],
                });
                self.terminate(Terminator::Unreachable);
                self.switch_to(ok);
                None
            }
            ExprKind::Call { callee, args, .. } => {
                if callee.name == "print" {
                    let v = self.expr(&args[0]).expect("print arguments have values");
                    let t = self.ty(&args[0]);
                    self.print(v, t);
                    return None;
                }
                let (target, params, ret) = match callee.name.as_str() {
                    "exit" => (Callee::Builtin(Builtin::Exit), vec![TTy::Int], TTy::None),
                    name => {
                        let (id, params, ret) = self.callee(name, e);
                        (Callee::Func(id), params, ret)
                    }
                };
                let mut evaluated = Vec::new();
                for (a, pt) in args.iter().zip(params) {
                    let v = self.expr(a);
                    let at = self.ty(a);
                    let v = self.coerce(v, at, pt);
                    evaluated.push((v.expect("arguments have values"), self.cur));
                }
                let arg_vals: Vec<TempId> = evaluated
                    .into_iter()
                    .map(|(t, b)| self.materialize(t, b))
                    .collect();
                let ret = self.sh.ir_ty(ret);
                let dst = ret.map(|t| self.temp(t));
                self.emit(Inst::Call {
                    dst,
                    callee: target,
                    args: arg_vals,
                });
                dst
            }
        }
    }

    // ---- external input (ADR-0011) ----

    fn builtin(&mut self, b: Builtin, args: Vec<TempId>) -> TempId {
        let d = self.temp(b.ret().expect("input builtins return a value"));
        self.emit(Inst::Call {
            dst: Some(d),
            callee: Callee::Builtin(b),
            args,
        });
        d
    }

    fn result_union(&mut self, e: &ast::Expr) -> (wys_typeck::UnionId, LocalId) {
        let TTy::Union(u) = self.ty(e) else {
            panic!("input builtins return unions")
        };
        let iu = self.sh.ir_union(u);
        (u, self.hidden_local(Ty::Union(iu)))
    }

    /// `arg(i)`: 引数があれば `str`、なければ `None` の `str | None`。
    fn lower_arg(&mut self, index: &ast::Expr, e: &ast::Expr) -> TempId {
        let (u, out) = self.result_union(e);
        let i = self.expr(index).expect("the index has a value");
        let i_slot = self.hidden_local(Ty::Int);
        self.emit(Inst::Store {
            dst: i_slot,
            src: i,
        });
        let exists = self.builtin(Builtin::ArgExists, vec![i]);
        let (some_b, none_b, join) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Terminator::Branch {
            cond: exists,
            then_block: some_b,
            else_block: none_b,
        });
        self.switch_to(some_b);
        let li = self.temp(Ty::Int);
        self.emit(Inst::Load {
            dst: li,
            src: i_slot,
        });
        let s = self.builtin(Builtin::ArgStr, vec![li]);
        let v = self.make_union(u, TTy::Str, Some(s));
        self.emit(Inst::Store { dst: out, src: v });
        self.terminate(Terminator::Jump(join));
        self.switch_to(none_b);
        let v = self.make_union(u, TTy::None, None);
        self.emit(Inst::Store { dst: out, src: v });
        self.terminate(Terminator::Jump(join));
        self.switch_to(join);
        let d = self.temp(Ty::Union(self.locals[out.0 as usize].union_id()));
        self.emit(Inst::Load { dst: d, src: out });
        d
    }

    /// `parse_scalar(s)`: ランタイムで種類を判定し（0 = int、1 = bool、2 = str、3 = null）、該当する値を詰める。
    fn lower_parse_scalar(&mut self, text: &ast::Expr, e: &ast::Expr) -> TempId {
        let (u, out) = self.result_union(e);
        let s = self.expr(text).expect("the text has a value");
        let s_slot = self.hidden_local(Ty::Str);
        self.emit(Inst::Store {
            dst: s_slot,
            src: s,
        });
        let k = self.builtin(Builtin::ScalarKind, vec![s]);
        let k_slot = self.hidden_local(Ty::Int);
        self.emit(Inst::Store {
            dst: k_slot,
            src: k,
        });
        let join = self.new_block();
        for (code, member) in [(0, TTy::Int), (1, TTy::Bool), (3, TTy::None), (2, TTy::Str)] {
            let last = member == TTy::Str;
            let next = if last {
                None
            } else {
                let lk = self.temp(Ty::Int);
                self.emit(Inst::Load {
                    dst: lk,
                    src: k_slot,
                });
                let c = self.konst(Const::Int(code), Ty::Int);
                let is = self.temp(Ty::Bool);
                self.emit(Inst::Binary {
                    dst: is,
                    op: BinOp::Eq,
                    lhs: lk,
                    rhs: c,
                });
                let (body, next) = (self.new_block(), self.new_block());
                self.terminate(Terminator::Branch {
                    cond: is,
                    then_block: body,
                    else_block: next,
                });
                self.switch_to(body);
                Some(next)
            };
            let payload = if member == TTy::None {
                None
            } else {
                let ls = self.temp(Ty::Str);
                self.emit(Inst::Load {
                    dst: ls,
                    src: s_slot,
                });
                Some(match member {
                    TTy::Int => self.builtin(Builtin::ScalarInt, vec![ls]),
                    TTy::Bool => self.builtin(Builtin::ScalarBool, vec![ls]),
                    _ => ls,
                })
            };
            let v = self.make_union(u, member, payload);
            self.emit(Inst::Store { dst: out, src: v });
            self.terminate(Terminator::Jump(join));
            if let Some(n) = next {
                self.switch_to(n);
            }
        }
        self.switch_to(join);
        let d = self.temp(Ty::Union(self.locals[out.0 as usize].union_id()));
        self.emit(Inst::Load { dst: d, src: out });
        d
    }

    // ---- unions ----

    fn members(&self, u: wys_typeck::UnionId) -> Vec<TTy> {
        self.sh.types.members(TTy::Union(u))
    }

    fn variant(&self, u: wys_typeck::UnionId, m: TTy) -> u32 {
        self.members(u)
            .iter()
            .position(|x| *x == m)
            .unwrap_or_else(|| panic!("{m:?} is not a member of union {u:?}")) as u32
    }

    fn make_union(&mut self, u: wys_typeck::UnionId, m: TTy, payload: Option<TempId>) -> TempId {
        let dst_iu = self.sh.ir_union(u);
        let dst = self.temp(Ty::Union(dst_iu));
        let variant = self.variant(u, m);
        self.emit(Inst::MakeUnion {
            dst,
            union: dst_iu,
            variant,
            payload,
        });
        dst
    }

    fn payload(&mut self, v: TempId, u: wys_typeck::UnionId, m: TTy) -> Option<TempId> {
        let ty = self.sh.ir_ty(m)?;
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
        let (from, to) = (self.sh.norm(from), self.sh.norm(to));
        if from == to {
            return v;
        }
        match (from, to) {
            (TTy::Union(fu), TTy::Union(tu)) => {
                let v = v.expect("union values exist");
                Some(self.union_to_union(v, fu, tu))
            }
            (TTy::Union(fu), base) => self.payload(v.expect("union values exist"), fu, base),
            (base, TTy::Union(tu)) => {
                let tm = self.target_member(tu, base);
                let p = self.coerce(v, base, tm);
                Some(self.make_union(tu, tm, p))
            }
            (TTy::Record(a), TTy::Record(b)) => {
                Some(self.record_to_record(v.expect("records have values"), a, b))
            }
            _ => panic!("no conversion from {from:?} to {to:?}"),
        }
    }

    /// Union `tu` の中で、値の型 `m` を受け取る構成要素。同じ型がなければ、`m` を幅部分型として受け取るレコード
    /// （型検査により、1つの Union に入るレコード型は1つまで）。
    fn target_member(&self, tu: wys_typeck::UnionId, m: TTy) -> TTy {
        let members = self.members(tu);
        if members.contains(&m) {
            return m;
        }
        members
            .into_iter()
            .find(|t| self.sh.types.member_fits(m, *t))
            .unwrap_or_else(|| panic!("{m:?} does not fit union {tu:?}"))
    }

    /// 幅部分型: 変換先のフィールドだけを取り出して、新しい配置のレコードを作る（不変なのでコピーしてよい）。
    fn record_to_record(
        &mut self,
        v: TempId,
        from: wys_typeck::RecordId,
        to: wys_typeck::RecordId,
    ) -> TempId {
        let src_def = self.sh.types.record_def(from).clone();
        let dst_fields = self.sh.types.record_def(to).fields.clone();
        let mut vals = Vec::new();
        for (name, ft) in &dst_fields {
            let (index, st) = src_def.field(name).expect("width subtyping checked");
            let fty = self.sh.ir_ty(st).expect("record fields are never None");
            let dst = self.temp(fty);
            self.emit(Inst::Field {
                dst,
                src: v,
                index: index as u32,
            });
            vals.push(self.coerce(Some(dst), st, *ft).expect("fields have values"));
        }
        let dst_ir = self.sh.ir_record(to);
        let dst = self.temp(Ty::Record(dst_ir));
        self.emit(Inst::MakeRecord {
            dst,
            record: dst_ir,
            fields: vals,
        });
        dst
    }

    /// 関数でも組み込み関数でもない名前の呼び出しは、`type` で定義したレコードの作成。
    fn is_record_constructor(&self, name: &str) -> bool {
        !self.sh.func_index.contains_key(name)
            && !matches!(
                name,
                "print" | "exit" | "arg_count" | "arg" | "parse_scalar" | "assert"
            )
    }

    /// `Point(x=..., y=...)`: キーワード引数をソースの順に評価してから、フィールドの順に並べる。
    fn construct(&mut self, r: wys_typeck::RecordId, kwargs: &[(ast::Ident, ast::Expr)]) -> TempId {
        let fields = self.sh.types.record_def(r).fields.clone();
        let mut evaluated: Vec<(String, TempId, usize)> = Vec::new();
        for (k, value) in kwargs {
            let (_, ft) = fields
                .iter()
                .find(|(n, _)| *n == k.name)
                .expect("fields exist after type checking");
            let v = self.expr(value);
            let vt = self.ty(value);
            let v = self.coerce(v, vt, *ft).expect("fields have values");
            evaluated.push((k.name.clone(), v, self.cur));
        }
        let mut vals = Vec::new();
        for (name, _) in &fields {
            let (_, t, b) = evaluated
                .iter()
                .find(|(n, _, _)| n == name)
                .cloned()
                .expect("every field is given");
            vals.push(self.materialize(t, b));
        }
        let dst_ir = self.sh.ir_record(r);
        let dst = self.temp(Ty::Record(dst_ir));
        self.emit(Inst::MakeRecord {
            dst,
            record: dst_ir,
            fields: vals,
        });
        dst
    }

    /// Union から Union へ: タグで分岐し、共通の構成要素ごとに詰め直す。
    fn union_to_union(
        &mut self,
        v: TempId,
        fu: wys_typeck::UnionId,
        tu: wys_typeck::UnionId,
    ) -> TempId {
        // 変換元の各構成要素が、変換先のどの構成要素に入るか（同じ型か、レコードの幅部分型）。
        // 変換先に入らない構成要素は、型検査（絞り込み）によりこの地点には来ない。
        let target = self.members(tu);
        let pairs: Vec<(TTy, TTy)> = self
            .members(fu)
            .into_iter()
            .filter_map(|m| {
                target
                    .iter()
                    .find(|t| **t == m)
                    .or_else(|| target.iter().find(|t| self.sh.types.member_fits(m, **t)))
                    .map(|t| (m, *t))
            })
            .collect();
        let common: Vec<TTy> = pairs.iter().map(|(m, _)| *m).collect();
        let src_iu = self.sh.ir_union(fu);
        let src = self.hidden_local(Ty::Union(src_iu));
        self.emit(Inst::Store { dst: src, src: v });
        let out_iu = self.sh.ir_union(tu);
        let out = self.hidden_local(Ty::Union(out_iu));
        self.for_each_variant(src, fu, &common, |l, val, m| {
            let tm = pairs
                .iter()
                .find(|(f, _)| *f == m)
                .map(|(_, t)| *t)
                .expect("paired above");
            let p = l.payload(val, fu, m);
            let p = l.coerce(p, m, tm);
            let u = l.make_union(tu, tm, p);
            l.emit(Inst::Store { dst: out, src: u });
        });
        let dst_iu = self.sh.ir_union(tu);
        let dst = self.temp(Ty::Union(dst_iu));
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
                let src_iu = self.sh.ir_union(u);
                let src = self.hidden_local(Ty::Union(src_iu));
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
        let l_block = self.cur;
        let r = self.expr(rhs).expect("operands have values");
        let l = self.materialize(l, l_block);
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
