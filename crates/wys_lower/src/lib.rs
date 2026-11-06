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
    display: wys_ir::DisplayInfo,
    list_ids: HashMap<wys_typeck::ListId, ListId>,
    lists: Vec<Ty>,
    /// `Result[T, E]` → 構成要素が2つの Union（0 = `Ok`、1 = `Err`。ADR-0013）
    result_ids: HashMap<wys_typeck::ResultId, UnionId>,
    dict_ids: HashMap<wys_typeck::DictId, DictId>,
    dicts: Vec<(Ty, Ty)>,
    /// `Task[T]`（ADR-0020）
    task_ids: HashMap<wys_typeck::TaskId, TaskId>,
    tasks: Vec<Option<Ty>>,
    /// `Unknown` のコンテナの種類（型検査の `list[Unknown]` と `dict[str, Unknown]`。ADR-0021 論点 1）
    unknown_containers: (TTy, TTy),
    /// IR に登録した `Unknown` のコンテナの型（`Unknown` を使うときだけ）。`None` の間に登録中なら `registering_unknown`
    unknown_ir: Option<(ListId, DictId)>,
    registering_unknown: bool,
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
            TTy::Float => Some(Ty::Float),
            TTy::PyObject => Some(Ty::Py),
            TTy::None => None,
            TTy::Union(u) => Some(Ty::Union(self.ir_union(u))),
            TTy::Record(r) => Some(Ty::Record(self.ir_record(r))),
            TTy::List(lst) => Some(Ty::List(self.ir_list(lst))),
            TTy::Result(r) => Some(Ty::Union(self.ir_result(r))),
            TTy::Dict(d) => Some(Ty::Dict(self.ir_dict(d))),
            TTy::Param(_) => {
                panic!("type parameter reached code generation; monomorphization missed it")
            }
            // `Unknown` は IR の独自の型（ADR-0021 論点 3）。最初に使ったときにコンテナの種類の型を登録する。
            // `list[Unknown]` の要素の登録でここに戻るので、登録中は印だけを見て返す
            TTy::Unknown => {
                if self.unknown_ir.is_none() && !self.registering_unknown {
                    self.registering_unknown = true;
                    let (TTy::List(l), TTy::Dict(d)) = self.unknown_containers else {
                        unreachable!("the containers are a list and a dict")
                    };
                    let l = self.ir_list(l);
                    let d = self.ir_dict(d);
                    self.unknown_ir = Some((l, d));
                }
                Some(Ty::Unknown)
            }
            TTy::Error => panic!("lowering received a type error; run the type checker first"),
            TTy::Task(k) => Some(Ty::Task(self.ir_task(k))),
        }
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

    fn ir_result(&mut self, r: wys_typeck::ResultId) -> UnionId {
        if let Some(&id) = self.result_ids.get(&r) {
            return id;
        }
        let (ok, err) = self.types.result_args(r);
        let members = vec![self.ir_ty(ok), self.ir_ty(err)];
        let id = UnionId(self.unions.len() as u32);
        self.unions.push(members);
        self.display.results.push(id);
        self.result_ids.insert(r, id);
        id
    }

    fn ir_dict(&mut self, d: wys_typeck::DictId) -> DictId {
        if let Some(&id) = self.dict_ids.get(&d) {
            return id;
        }
        let (k, v) = self.types.dict_args(d);
        let k = self.ir_ty(k).expect("dict keys are int, bool or str");
        let v = self.ir_ty(v).expect("dict values are never None");
        if let Some(&id) = self.dict_ids.get(&d) {
            return id; // `ir_list` と同じ（`dict[str, Unknown]`）
        }
        let id = DictId(self.dicts.len() as u32);
        self.dicts.push((k, v));
        self.dict_ids.insert(d, id);
        id
    }

    fn ir_task(&mut self, k: wys_typeck::TaskId) -> TaskId {
        if let Some(&id) = self.task_ids.get(&k) {
            return id;
        }
        let r = self.types.task_elem(k);
        let r = self.ir_ty(r);
        let id = TaskId(self.tasks.len() as u32);
        self.tasks.push(r);
        self.task_ids.insert(k, id);
        id
    }

    fn ir_list(&mut self, lst: wys_typeck::ListId) -> ListId {
        if let Some(&id) = self.list_ids.get(&lst) {
            return id;
        }
        let elem = self.types.list_elem(lst);
        let elem = self.ir_ty(elem).expect("list elements are never None");
        // 要素の型を登録する間に、同じ型が登録されていることがある（`Unknown` を初めて使うと
        // `list[Unknown]` を登録する。ADR-0021 論点 3）。二重に登録しない
        if let Some(&id) = self.list_ids.get(&lst) {
            return id;
        }
        let id = ListId(self.lists.len() as u32);
        self.lists.push(elem);
        self.list_ids.insert(lst, id);
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
        // 表示名は定義に書いた名前（ほかのモジュールの型の内部名 `util.text.Tag` は `Tag`）
        let name = self.types.record_def(r).name.clone().unwrap_or_default();
        let name = name.rsplit('.').next().unwrap_or_default().to_string();
        self.display.record_names.push(name);
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
    let mut types = tc.types.clone();
    let unknown_containers = (types.list(TTy::Unknown), types.dict(TTy::Str, TTy::Unknown));
    let mut sh = Shared {
        tc,
        types,
        unknown_containers,
        unknown_ir: None,
        registering_unknown: false,
        union_ids: HashMap::new(),
        unions: Vec::new(),
        record_ids: HashMap::new(),
        records: Vec::new(),
        display: Default::default(),
        list_ids: HashMap::new(),
        result_ids: HashMap::new(),
        dict_ids: HashMap::new(),
        dicts: Vec::new(),
        task_ids: HashMap::new(),
        tasks: Vec::new(),
        lists: Vec::new(),
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
    // `@tool` の関数ごとにトランポリンを合成する（ADR-0022 論点 15）。ツールは Generic でない（E1403）
    for (i, sig) in tc.sigs.iter().enumerate() {
        if sig.tool.is_none() {
            continue;
        }
        let Some(&tool_id) = sh.instances.get(&(i, Vec::new())) else {
            continue;
        };
        let entry = FuncId(funcs.len() as u32);
        let (f, arg_record) = tool_trampoline(&mut sh, &module.funcs[i], i, tool_id);
        funcs.push(f);
        if let Some(t) = funcs[tool_id.0 as usize].tool.as_mut() {
            t.entry = Some(entry);
            t.arg_record = Some(arg_record);
        }
    }
    let entry = if opts.tests.is_empty() {
        sh.instances[&(sh.func_index["main"], Vec::new())]
    } else {
        let ids: Vec<FuncId> = opts
            .tests
            .iter()
            .map(|i| sh.instances[&(*i, Vec::new())])
            .collect();
        // async のテストがあれば、エントリも async にする（スケジューラの上で動かす。ADR-0020 論点 7）
        let any_async = opts.tests.iter().any(|i| tc.sigs[*i].is_async);
        funcs.push(test_dispatcher(&ids, any_async));
        FuncId(funcs.len() as u32 - 1)
    };
    Ok(Program {
        strings: sh.strings,
        unions: sh.unions,
        records: sh.records,
        lists: sh.lists,
        dicts: sh.dicts,
        tasks: sh.tasks,
        unknown: sh.unknown_ir,
        display: sh.display,
        funcs,
        entry,
    })
}

/// `wys test` のエントリ: 環境変数 `WYS_TEST_INDEX` の番号のテスト関数を1つだけ呼ぶ。
/// ツールのトランポリン `__wys_tool_entry_<名前>(args: str) -> str`（ADR-0022 論点 15）: JSON の引数を
/// `parse_json` → `validate[引数のレコード]` で型付きにしてツールを呼び、戻り値を `to_json` で返す。
/// 引数が検証を通らなければ `tool_args_rejected` を呼んで空の文字列を返す（ランタイムが `Err(schema)` にする）。
/// 戻り値が `Result` の `Err` なら `tool_err` を呼ぶ（LLM には失敗として返す）。ツールが `async def` なら
/// トランポリンも `async`（ランタイムはタスクのスタックの上で呼ぶので、ツールの中で中断してよい）
fn tool_trampoline(
    sh: &mut Shared<'_>,
    f: &ast::FuncDef,
    index: usize,
    tool: FuncId,
) -> (Function, RecordId) {
    let sig = sh.tc.sigs[index].clone();
    let fields: Vec<(String, TTy)> = f
        .params
        .iter()
        .zip(&sig.params)
        .map(|(p, t)| (p.name.name.clone(), *t))
        .collect();
    let TTy::Record(rid) = sh.types.record(fields, None) else {
        unreachable!("records are records")
    };
    let arg_record = sh.ir_record(rid);
    let mut l = FnLower {
        sh,
        subst: Subst::new(),
        ret: TTy::Str,
        locals: vec![Ty::Str],
        local_ids: HashMap::new(),
        local_decl: HashMap::new(),
        temps: Vec::new(),
        blocks: vec![Bb {
            insts: Vec::new(),
            term: None,
        }],
        cur: 0,
        agent: false,
    };
    let reject = |l: &mut FnLower, schema: bool| {
        let c = l.konst(Const::Bool(schema), Ty::Bool);
        l.emit(Inst::Call {
            dst: None,
            callee: Callee::Builtin(Builtin::ToolArgsRejected),
            args: vec![c],
        });
        let e = l.string_const("");
        l.terminate(Terminator::Return(Some(e)));
    };
    // 引数の JSON → Unknown
    let a = l.temp(Ty::Str);
    l.emit(Inst::Load {
        dst: a,
        src: LocalId(0),
    });
    let u = l.temp(Ty::Unknown);
    l.emit(Inst::Call {
        dst: Some(u),
        callee: Callee::Builtin(Builtin::JsonParse),
        args: vec![a],
    });
    let u_slot = l.hidden_local(Ty::Unknown);
    l.emit(Inst::Store {
        dst: u_slot,
        src: u,
    });
    let jf = l.builtin(Builtin::JsonFailed, vec![]);
    let (parsed, bad_json) = (l.new_block(), l.new_block());
    l.terminate(Terminator::Branch {
        cond: jf,
        then_block: bad_json,
        else_block: parsed,
    });
    l.switch_to(bad_json);
    reject(&mut l, false);
    // Unknown → 引数のレコード
    l.switch_to(parsed);
    let lu = l.temp(Ty::Unknown);
    l.emit(Inst::Load {
        dst: lu,
        src: u_slot,
    });
    let rv = l.temp(Ty::Record(arg_record));
    l.emit(Inst::Call {
        dst: Some(rv),
        callee: Callee::Builtin(Builtin::Validate),
        args: vec![lu],
    });
    let r_slot = l.hidden_local(Ty::Record(arg_record));
    l.emit(Inst::Store {
        dst: r_slot,
        src: rv,
    });
    let sf = l.builtin(Builtin::SchemaFailed, vec![]);
    let (valid, bad_args) = (l.new_block(), l.new_block());
    l.terminate(Terminator::Branch {
        cond: sf,
        then_block: bad_args,
        else_block: valid,
    });
    l.switch_to(bad_args);
    reject(&mut l, true);
    // ツールを呼ぶ（引数はレコードのフィールド。フィールドは名前の順に並ぶので、名前で引く）
    l.switch_to(valid);
    let rec = l.temp(Ty::Record(arg_record));
    l.emit(Inst::Load {
        dst: rec,
        src: r_slot,
    });
    let mut args = Vec::new();
    for (p, &pt) in f.params.iter().zip(&sig.params) {
        let (k, _) =
            l.sh.types
                .record_def(rid)
                .field(&p.name.name)
                .expect("the record has every parameter");
        let ir = l.sh.ir_ty(pt).expect("tool arguments have values");
        let d = l.temp(ir);
        l.emit(Inst::Field {
            dst: d,
            src: rec,
            index: k as u32,
        });
        args.push(d);
    }
    let ret_ir = l.sh.ir_ty(sig.ret);
    let rv = ret_ir.map(|t| l.temp(t));
    l.emit(Inst::Call {
        dst: rv,
        callee: Callee::Func(tool),
        args,
    });
    match (sig.ret, rv) {
        (TTy::Result(r), Some(v)) => {
            let (ok_t, err_t) = l.sh.types.result_args(r);
            let res_slot = l.hidden_local(ret_ir.expect("results have values"));
            l.emit(Inst::Store {
                dst: res_slot,
                src: v,
            });
            let lv = l.temp(ret_ir.expect("results have values"));
            l.emit(Inst::Load {
                dst: lv,
                src: res_slot,
            });
            let is_err = l.temp(Ty::Bool);
            l.emit(Inst::UnionIs {
                dst: is_err,
                src: lv,
                variant: 1,
            });
            let (ok_b, err_b) = (l.new_block(), l.new_block());
            l.terminate(Terminator::Branch {
                cond: is_err,
                then_block: err_b,
                else_block: ok_b,
            });
            for (blk, t, variant, err) in [(ok_b, ok_t, 0, false), (err_b, err_t, 1, true)] {
                l.switch_to(blk);
                if err {
                    l.emit(Inst::Call {
                        dst: None,
                        callee: Callee::Builtin(Builtin::ToolErr),
                        args: vec![],
                    });
                }
                let j = match l.sh.ir_ty(t) {
                    None => l.string_const("null"),
                    Some(pt) => {
                        let lv = l.temp(ret_ir.expect("results have values"));
                        l.emit(Inst::Load {
                            dst: lv,
                            src: res_slot,
                        });
                        let p = l.temp(pt);
                        l.emit(Inst::UnionPayload {
                            dst: p,
                            src: lv,
                            variant,
                        });
                        l.builtin(Builtin::ToJson, vec![p])
                    }
                };
                l.terminate(Terminator::Return(Some(j)));
            }
        }
        (_, Some(v)) => {
            let j = l.builtin(Builtin::ToJson, vec![v]);
            l.terminate(Terminator::Return(Some(j)));
        }
        (_, None) => {
            let j = l.string_const("null");
            l.terminate(Terminator::Return(Some(j)));
        }
    }
    let func = Function {
        name: format!("__wys_tool_entry_{}", f.name.name.replace('.', "_")),
        param_count: 1,
        locals: l.locals,
        ret: Some(Ty::Str),
        temps: l.temps,
        blocks: l
            .blocks
            .into_iter()
            .map(|b| Block {
                insts: b.insts,
                term: b.term.unwrap_or(Terminator::Unreachable),
            })
            .collect(),
        elided_rc: Vec::new(),
        is_async: sig.is_async,
        effects: None,
        tool: None,
        agent: None,
    };
    (func, arg_record)
}

fn test_dispatcher(tests: &[FuncId], is_async: bool) -> Function {
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
        elided_rc: Vec::new(),
        effects: None,
        tool: None,
        agent: None,
        is_async,
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
    /// `@agent` の関数（入口で agent コンテキストに入り、すべての `return` の前で出る。ADR-0022 論点 14）
    agent: bool,
}

impl<'s, 't> FnLower<'s, 't> {
    /// 関数から戻る。`@agent` の関数なら、先に agent コンテキストを出る
    fn return_(&mut self, v: Option<TempId>) {
        if self.agent {
            self.emit(Inst::Call {
                dst: None,
                callee: Callee::Builtin(Builtin::AgentExit),
                args: vec![],
            });
        }
        self.terminate(Terminator::Return(v));
    }

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
            agent: sig.agent.is_some(),
        };
        if l.agent {
            l.emit(Inst::Call {
                dst: None,
                callee: Callee::Builtin(Builtin::AgentEnter),
                args: vec![],
            });
        }
        for (lname, ty) in &info.locals {
            let lid = LocalId(l.locals.len() as u32);
            let decl = l.sh.types.subst(*ty, &l.subst);

            let ir = l.sh.ir_ty(decl).expect("locals never have type None");
            l.locals.push(ir);
            l.local_ids.insert(lname.clone(), lid);
            l.local_decl.insert(lname.clone(), decl);
        }
        l.stmts(&f.body);
        let ret = l.sh.ir_ty(l.ret);
        // 本体の終わりに落ちる経路（`-> None` の関数）も、agent コンテキストを出る
        if l.agent && ret.is_none() {
            for b in &mut l.blocks {
                if b.term.is_none() {
                    b.insts.push(Inst::Call {
                        dst: None,
                        callee: Callee::Builtin(Builtin::AgentExit),
                        args: vec![],
                    });
                }
            }
        }
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
            elided_rc: Vec::new(),
            is_async: sig.is_async,
            effects: sig.effects.map(|es| {
                es.iter()
                    .into_iter()
                    .map(|e| match e {
                        wys_typeck::Effect::Io => Effect::Io,
                        wys_typeck::Effect::Exit => Effect::Exit,
                        wys_typeck::Effect::Python => Effect::Python,
                        wys_typeck::Effect::Llm => Effect::Llm,
                    })
                    .collect()
            }),
            tool: sig.tool.as_ref().map(|t| wys_ir::ToolAttr {
                description: t.description.clone(),
                approval: t.approval,
                entry: None,
                arg_record: None,
            }),
            agent: sig.agent.as_ref().map(|a| wys_ir::AgentAttr {
                tools: a.tools.clone(),
                capabilities: a
                    .capabilities
                    .iter()
                    .into_iter()
                    .map(|e| match e {
                        wys_typeck::Effect::Io => Effect::Io,
                        wys_typeck::Effect::Exit => Effect::Exit,
                        wys_typeck::Effect::Python => Effect::Python,
                        wys_typeck::Effect::Llm => Effect::Llm,
                    })
                    .collect(),
                model: a.model.clone(),
                max_llm_calls: a.max_llm_calls,
                max_tool_calls: a.max_tool_calls,
                max_tokens: a.max_tokens,
                timeout_ms: a.timeout_ms,
            }),
        }
    }

    /// 式の型（この関数インスタンスの型引数で置き換えた後）。
    fn ty(&mut self, e: &ast::Expr) -> TTy {
        let t = self.sh.tc.ty(e);
        self.sh.types.subst(t, &self.subst)
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
            .map(|t| self.sh.types.subst(*t, &map))
            .collect();
        let ret = self.sh.types.subst(ret, &map);

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

    /// `Unknown` の値を作るときは、コンテナの種類の型を必ず登録する（型を書かずに `parse_scalar` の結果を
    /// `match` するだけのプログラムでも `Program::unknown` が決まるように）
    fn register_unknown(&mut self, ty: Ty) {
        if ty == Ty::Unknown && self.sh.unknown_ir.is_none() {
            self.sh.ir_ty(TTy::Unknown);
        }
    }

    fn temp(&mut self, ty: Ty) -> TempId {
        self.register_unknown(ty);
        self.temps.push(ty);
        TempId(self.temps.len() as u32 - 1)
    }

    fn hidden_local(&mut self, ty: Ty) -> LocalId {
        self.register_unknown(ty);
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

    /// 文字列の定数（不死の `str`）。
    fn string_const(&mut self, text: &str) -> TempId {
        let id = StrId(self.sh.strings.len() as u32);
        self.sh.strings.push(text.to_string());
        self.konst(Const::Str(id), Ty::Str)
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
                self.return_(v);
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
            StmtKind::IndexAssign { target, value } => {
                let ExprKind::Index { base, index } = &target.kind else {
                    unreachable!("IndexAssign always has an index target")
                };
                let bt = self.ty(base);
                self.violates_unknown_rules(bt, target.span, false);
                if let TTy::Dict(d) = self.ty(base) {
                    // d[k] = v（ADR-0015）
                    let (kt, vt) = self.sh.types.dict_args(d);
                    let b = self.expr(base).expect("dicts have values");
                    let bb = self.cur;
                    let k = self.expr(index);
                    let it = self.ty(index);
                    let k = self.coerce(k, it, kt).expect("keys have values");
                    let kb = self.cur;
                    let v = self.expr(value);
                    let valt = self.ty(value);
                    let v = self.coerce(v, valt, vt).expect("dict values have values");
                    let b = self.materialize(b, bb);
                    let k = self.materialize(k, kb);
                    self.emit(Inst::DictSet {
                        dict: b,
                        key: k,
                        value: v,
                    });
                    return;
                }
                let TTy::List(l) = self.ty(base) else {
                    panic!("index assignment on a non-list passed type checking")
                };
                let elem = self.sh.types.list_elem(l);
                let b = self.expr(base).expect("lists have values");
                let bb = self.cur;
                let i = self.expr(index).expect("indexes have values");
                let ib = self.cur;
                let v = self.expr(value);
                let vt = self.ty(value);
                let v = self.coerce(v, vt, elem).expect("list elements have values");
                let b = self.materialize(b, bb);
                let i = self.materialize(i, ib);
                self.emit(Inst::ListSet {
                    list: b,
                    index: i,
                    value: v,
                });
            }
            StmtKind::For { var, iter, body } => self.for_stmt(var, iter, body),
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
        if let (TTy::Result(r), Some(slot)) = (st, slot) {
            self.result_arms(r, slot, arms, join);
            self.terminate(Terminator::Unreachable);
            self.switch_to(join);
            return;
        }
        for arm in arms {
            let pattern_ty = match &arm.pattern.kind {
                PatternKind::Wildcard => None,
                PatternKind::None => Some(TTy::None),
                PatternKind::Type(name) => Some(match name.as_str() {
                    "int" => TTy::Int,
                    "bool" => TTy::Bool,
                    "str" => TTy::Str,
                    "float" => TTy::Float,
                    "Result" => match st {
                        TTy::Union(u) => self
                            .members(u)
                            .into_iter()
                            .find(|m| matches!(m, TTy::Result(_)))
                            .expect("type checking found a Result member"),
                        other => other,
                    },
                    // 型の別名（`type Id = int`、ほかのモジュールの `ids.Id` を含む）は、型検査で解決した型
                    other => *self
                        .sh
                        .tc
                        .pattern_types
                        .get(&arm.pattern.span.start)
                        .unwrap_or_else(|| {
                            panic!("unknown pattern type `{other}` passed type checking")
                        }),
                }),
                PatternKind::Bind(..) => {
                    panic!("binding pattern on a non-Result passed type checking")
                }
            };
            let next = self.new_block();
            // 一致しない `case`（絞り込みで被検査値の型から外れた型。型検査は W0301 を出す）は生成しない
            let is_sum = matches!(st, TTy::Union(_) | TTy::Unknown);
            let never = match (pattern_ty, st) {
                (None, _) => false,
                (Some(pt), st) if is_sum => !self.sum_members(st).contains(&pt),
                (Some(pt), st) => pt != st,
            };
            if never {
                continue;
            }
            if let (Some(pt), true, Some(slot)) = (pattern_ty, is_sum, slot) {
                let val = self.temp(self.locals[slot.0 as usize]);
                self.emit(Inst::Load {
                    dst: val,
                    src: slot,
                });
                let c = self.temp(Ty::Bool);
                let variant = self.variant(st, pt);
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

    /// `operand?`: 値をローカルに置き、`Err` なら中身を関数の `Result` の `Err` に詰め直して返す。
    /// `Ok` なら次のブロックで中身を取り出す（一時値はブロック内でのみ使えるので、ローカルから読み直す）。
    fn try_expr(&mut self, inner: &ast::Expr) -> Option<TempId> {
        let TTy::Result(r) = self.ty(inner) else {
            panic!("`?` on a non-Result passed type checking")
        };
        let TTy::Result(fr) = self.ret else {
            panic!("`?` in a function that does not return a Result passed type checking")
        };
        let (ok, err) = self.sh.types.result_args(r);
        let (_, ferr) = self.sh.types.result_args(fr);
        let v = self.expr(inner).expect("Result values exist");
        let union_ty = self.temps[v.0 as usize];
        let slot = self.hidden_local(union_ty);
        self.emit(Inst::Store { dst: slot, src: v });
        let val = self.temp(union_ty);
        self.emit(Inst::Load {
            dst: val,
            src: slot,
        });
        let is_err = self.temp(Ty::Bool);
        self.emit(Inst::UnionIs {
            dst: is_err,
            src: val,
            variant: 1,
        });
        let (err_b, ok_b) = (self.new_block(), self.new_block());
        self.terminate(Terminator::Branch {
            cond: is_err,
            then_block: err_b,
            else_block: ok_b,
        });
        // Err: 関数の Result の Err として返す
        self.switch_to(err_b);
        let e = self.sh.ir_ty(err).map(|ety| {
            let val = self.temp(union_ty);
            self.emit(Inst::Load {
                dst: val,
                src: slot,
            });
            let p = self.temp(ety);
            self.emit(Inst::UnionPayload {
                dst: p,
                src: val,
                variant: 1,
            });
            p
        });
        let e = self.coerce(e, err, ferr);
        let fu = self.sh.ir_result(fr);
        let out = self.temp(Ty::Union(fu));
        self.emit(Inst::MakeUnion {
            dst: out,
            union: fu,
            variant: 1,
            payload: e,
        });
        self.return_(Some(out));
        // Ok: 中身を取り出す
        self.switch_to(ok_b);
        let oty = self.sh.ir_ty(ok)?;
        let val = self.temp(union_ty);
        self.emit(Inst::Load {
            dst: val,
            src: slot,
        });
        let p = self.temp(oty);
        self.emit(Inst::UnionPayload {
            dst: p,
            src: val,
            variant: 0,
        });
        Some(p)
    }

    /// `Result` に対する `case`: タグで判定し、束縛変数は `case` の本体だけで使えるローカルにする（ADR-0013）。
    /// 中身は借用で取り出し、束縛のスロットに格納する（ARC パスが retain し、スロットが +1 で所有する）。
    fn result_arms(
        &mut self,
        r: wys_typeck::ResultId,
        slot: LocalId,
        arms: &[ast::MatchArm],
        join: BlockId,
    ) {
        let (ok, err) = self.sh.types.result_args(r);
        let mut remaining = [true, true];
        for arm in arms {
            let (pieces, binding): (Vec<u32>, Option<&ast::Ident>) = match &arm.pattern.kind {
                PatternKind::Bind(name, b) => (vec![u32::from(name == "Err")], b.as_ref()),
                PatternKind::Wildcard => (vec![0, 1], None),
                PatternKind::Type(n) if n == "Result" => (vec![0, 1], None),
                // 絞り込みで外れた構成要素の case（W0301）は一致しない
                _ => continue,
            };
            let pieces: Vec<u32> = pieces
                .into_iter()
                .filter(|&p| remaining[p as usize])
                .collect();
            if pieces.is_empty() {
                continue; // 到達しない case（W0301）
            }
            for &p in &pieces {
                remaining[p as usize] = false;
            }
            let next = self.new_block();
            let union_ty = self.locals[slot.0 as usize];
            if let [variant] = pieces.as_slice() {
                let val = self.temp(union_ty);
                self.emit(Inst::Load {
                    dst: val,
                    src: slot,
                });
                let c = self.temp(Ty::Bool);
                self.emit(Inst::UnionIs {
                    dst: c,
                    src: val,
                    variant: *variant,
                });
                let body = self.new_block();
                self.terminate(Terminator::Branch {
                    cond: c,
                    then_block: body,
                    else_block: next,
                });
                self.switch_to(body);
            }
            let mut saved = None;
            if let (Some(ident), [variant]) = (binding, pieces.as_slice()) {
                let bty = if *variant == 0 { ok } else { err };
                if let Some(ir) = self.sh.ir_ty(bty) {
                    let val = self.temp(union_ty);
                    self.emit(Inst::Load {
                        dst: val,
                        src: slot,
                    });
                    let p = self.temp(ir);
                    self.emit(Inst::UnionPayload {
                        dst: p,
                        src: val,
                        variant: *variant,
                    });
                    let local = self.hidden_local(ir);
                    self.emit(Inst::Store { dst: local, src: p });
                    let name = ident.name.clone();
                    saved = Some((
                        name.clone(),
                        self.local_ids.insert(name.clone(), local),
                        self.local_decl.insert(name, bty),
                    ));
                }
            }
            self.stmts(&arm.body);
            if let Some((name, id, decl)) = saved {
                match id {
                    Some(id) => self.local_ids.insert(name.clone(), id),
                    None => self.local_ids.remove(&name),
                };
                match decl {
                    Some(d) => self.local_decl.insert(name, d),
                    None => self.local_decl.remove(&name),
                };
            }
            if self.blocks[self.cur].term.is_none() {
                self.terminate(Terminator::Jump(join));
            }
            self.switch_to(next);
        }
    }

    // ---- expressions ----

    /// 式を評価し、値があればその一時値を返す（`None` 型なら `None`）。
    fn expr(&mut self, e: &ast::Expr) -> Option<TempId> {
        match &e.kind {
            ExprKind::Int(v) => Some(self.konst(Const::Int(*v), Ty::Int)),
            ExprKind::Float(v) => Some(self.konst(Const::Float(v.to_bits()), Ty::Float)),
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
                    TTy::Union(_) | TTy::Unknown => {
                        let dst = self.temp(Ty::Bool);
                        let variant = self.variant(lt, TTy::None);
                        self.emit(Inst::UnionIs {
                            dst,
                            src: v.expect("union values exist"),
                            variant,
                        });
                        dst
                    }
                    // 絞り込みで None を含まなくなった値（宣言は Optional）。常に偽
                    _ => self.konst(Const::Bool(false), Ty::Bool),
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
                    // float は `-0.0 - x`（`0.0 - x` だと `-0.0` の符号を失う。ADR-0019）
                    UnaryOp::Neg if self.ty(operand) == TTy::Float => {
                        let zero = self.konst(Const::Float((-0.0f64).to_bits()), Ty::Float);
                        let dst = self.temp(Ty::Float);
                        self.emit(Inst::Binary {
                            dst,
                            op: BinOp::Sub,
                            lhs: zero,
                            rhs: v,
                        });
                        dst
                    }
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
            ExprKind::Try(inner) => self.try_expr(inner),
            ExprKind::Await(inner) => self.await_expr(inner, e),
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
                // 絞り込んだフィールド（ADR-0016）は、変数と同じく絞り込んだ型に変換する
                let narrowed = self.ty(e);
                if narrowed == TTy::None {
                    return None;
                }
                let dst = self.temp(fty);
                self.emit(Inst::Field {
                    dst,
                    src: v,
                    index: index as u32,
                });
                self.coerce(Some(dst), ft, narrowed)
            }
            ExprKind::Call { callee, args, .. } if callee.name == "spawn" => {
                Some(self.spawn(&args[0], e))
            }
            ExprKind::Call { callee, kwargs, .. } if self.is_record_constructor(&callee.name) => {
                let TTy::Record(r) = self.ty(e) else {
                    panic!("record construction without a record type")
                };
                Some(self.construct(r, kwargs))
            }
            // 数値の変換（ADR-0019 論点 3）
            ExprKind::Call { callee, args, .. }
                if matches!(callee.name.as_str(), "float" | "int") =>
            {
                let v = self.expr(&args[0]).expect("numbers have values");
                let b = if callee.name == "float" {
                    Builtin::FloatFromInt
                } else {
                    Builtin::IntFromFloat
                };
                Some(self.builtin(b, vec![v]))
            }
            // Python（ADR-0018 論点 9・10）
            ExprKind::Call { callee, args, .. } if callee.name == "py_import" => {
                let name = self.expr(&args[0]).expect("names have values");
                Some(self.py_op(Builtin::PyImport, vec![name], e))
            }
            ExprKind::Call { callee, args, .. } if callee.name == "py_value" => {
                let at = self.ty(&args[0]);
                let v = self.expr(&args[0]);
                Some(match at {
                    TTy::PyObject => v.expect("handles have values"),
                    TTy::None => self.builtin(Builtin::PyNone, vec![]),
                    t => {
                        let b = match t {
                            TTy::Int => Builtin::PyFromInt,
                            TTy::Bool => Builtin::PyFromBool,
                            TTy::Str => Builtin::PyFromStr,
                            TTy::Float => Builtin::PyFromFloat,
                            // list / dict / Union は型ごとの変換関数（M4 T7）
                            _ => Builtin::PyFromValue,
                        };
                        self.builtin(b, vec![v.expect("convertible values have values")])
                    }
                })
            }
            ExprKind::Call { callee, args, .. } if callee.name == "len" => {
                let at = self.ty(&args[0]);
                let s = self.expr(&args[0]).expect("len takes a value");
                if matches!(at, TTy::List(_)) {
                    let d = self.temp(Ty::Int);
                    self.emit(Inst::ListLen { dst: d, src: s });
                    return Some(d);
                }
                if matches!(at, TTy::Dict(_)) {
                    let d = self.temp(Ty::Int);
                    self.emit(Inst::DictLen { dst: d, src: s });
                    return Some(d);
                }
                Some(self.builtin(Builtin::StrLen, vec![s]))
            }
            // Python のオブジェクトのメソッド（受け手は式。ADR-0018 論点 9）
            ExprKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } if self.ty(receiver) == TTy::PyObject => {
                Some(self.py_method(receiver, &method.name, args, e))
            }
            // list / dict のメソッド（受け手はローカル変数、型検査で確認済み。M4 T4 で AST に印を付けた）
            ExprKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } if method.name == "pop" => {
                let recv = receiver
                    .as_name()
                    .expect("list and dict methods take a variable");
                let (dv, dt, direct) = self.receiver(recv, |t| matches!(t, TTy::Dict(_)));
                let db = self.cur;
                self.violates_unknown_rules(dt, e.span, false);
                let TTy::Dict(d) = dt else {
                    unreachable!("receiver returns the requested type")
                };
                let (kt, vt) = self.sh.types.dict_args(d);
                let k = self.expr(&args[0]);
                let it = self.ty(&args[0]);
                let k = self.coerce(k, it, kt).expect("keys have values");
                let dv = match direct {
                    Some(slot) => self.materialize_from_local(dv, slot),
                    None => self.materialize(dv, db),
                };
                let ir = self.sh.ir_ty(vt).expect("dict values are never None");
                let dst = self.temp(ir);
                self.emit(Inst::DictPop {
                    dst,
                    src: dv,
                    key: k,
                });
                Some(dst)
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } if method.name == "get" => Some(
                self.dict_get(
                    receiver
                        .as_name()
                        .expect("list and dict methods take a variable"),
                    &args[0],
                    e,
                ),
            ),
            ExprKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                // xs.append(v)
                debug_assert_eq!(
                    method.name, "append",
                    "the type checker accepts only these methods"
                );
                let recv = receiver
                    .as_name()
                    .expect("list and dict methods take a variable");
                let (list, rt, direct) = self.receiver(recv, |t| matches!(t, TTy::List(_)));
                let lb = self.cur;
                self.violates_unknown_rules(rt, e.span, false);
                let TTy::List(l) = rt else {
                    unreachable!("receiver returns the requested type")
                };
                let elem = self.sh.types.list_elem(l);
                let v = self.expr(&args[0]);
                let vt = self.ty(&args[0]);
                let v = self.coerce(v, vt, elem).expect("list elements have values");
                let list = match direct {
                    Some(slot) => self.materialize_from_local(list, slot),
                    None => self.materialize(list, lb),
                };
                self.emit(Inst::ListPush { list, value: v });
                None
            }
            ExprKind::List(items) => {
                let TTy::List(l) = self.ty(e) else {
                    panic!("list literal without a list type")
                };
                let elem = self.sh.types.list_elem(l);
                let mut evaluated = Vec::new();
                for it in items {
                    let v = self.expr(it);
                    let vt = self.ty(it);
                    let v = self.coerce(v, vt, elem).expect("list elements have values");
                    evaluated.push((v, self.cur));
                }
                let vals: Vec<TempId> = evaluated
                    .into_iter()
                    .map(|(t, b)| self.materialize(t, b))
                    .collect();
                let il = self.sh.ir_list(l);
                let d = self.temp(Ty::List(il));
                self.emit(Inst::ListNew {
                    dst: d,
                    list: il,
                    items: vals,
                });
                Some(d)
            }
            ExprKind::Dict(entries) => {
                let TTy::Dict(d) = self.ty(e) else {
                    panic!("dict literal without a dict type")
                };
                let (kt, vt) = self.sh.types.dict_args(d);
                let mut evaluated = Vec::new();
                for (k, v) in entries {
                    let kv = self.expr(k);
                    let ktt = self.ty(k);
                    let kv = self.coerce(kv, ktt, kt).expect("keys have values");
                    evaluated.push((kv, self.cur));
                    let vv = self.expr(v);
                    let vtt = self.ty(v);
                    let vv = self.coerce(vv, vtt, vt).expect("dict values have values");
                    evaluated.push((vv, self.cur));
                }
                let vals: Vec<TempId> = evaluated
                    .into_iter()
                    .map(|(t, b)| self.materialize(t, b))
                    .collect();
                let (keys, vals): (Vec<TempId>, Vec<TempId>) = (
                    vals.iter().step_by(2).copied().collect(),
                    vals.iter().skip(1).step_by(2).copied().collect(),
                );
                let id = self.sh.ir_dict(d);
                let dst = self.temp(Ty::Dict(id));
                self.emit(Inst::DictNew {
                    dst,
                    dict: id,
                    keys,
                    vals,
                });
                Some(dst)
            }
            ExprKind::Index { base, index } if matches!(self.ty(base), TTy::Dict(_)) => {
                let TTy::Dict(d) = self.ty(base) else {
                    unreachable!("checked by the guard")
                };
                let (kt, _) = self.sh.types.dict_args(d);
                let b = self.expr(base).expect("dicts have values");
                let bb = self.cur;
                let k = self.expr(index);
                let it = self.ty(index);
                let k = self.coerce(k, it, kt).expect("keys have values");
                let b = self.materialize(b, bb);
                let et = self.ty(e);
                let ir = self.sh.ir_ty(et).expect("dict values are never None");
                let dst = self.temp(ir);
                self.emit(Inst::DictGet {
                    dst,
                    src: b,
                    key: k,
                });
                Some(dst)
            }
            ExprKind::Index { base, index } => {
                let b = self.expr(base).expect("lists have values");
                let bb = self.cur;
                let i = self.expr(index).expect("indexes have values");
                let b = self.materialize(b, bb);
                let et = self.ty(e);
                let ir = self.sh.ir_ty(et).expect("list elements are never None");
                let d = self.temp(ir);
                self.emit(Inst::ListGet {
                    dst: d,
                    src: b,
                    index: i,
                });
                Some(d)
            }
            ExprKind::Call { callee, args, .. }
                if matches!(
                    callee.name.as_str(),
                    "arg_count"
                        | "arg"
                        | "parse_scalar"
                        | "parse_json"
                        | "validate"
                        | "to_json"
                        | "llm_generate"
                        | "llm_mock_push"
                        | "audit_events"
                ) =>
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
                    "parse_json" => self.lower_parse_json(&args[0], e),
                    "validate" => self.lower_validate(&args[0], e),
                    "llm_generate" => self.lower_llm_generate(&args[0], e),
                    "llm_mock_push" => {
                        let mr = self.sh.types.mock_response();
                        let j = self.json_of(&args[0], mr);
                        self.emit(Inst::Call {
                            dst: None,
                            callee: Callee::Builtin(Builtin::LlmMockPush),
                            args: vec![j],
                        });
                        return None;
                    }
                    "audit_events" => self.lower_audit_events(e),
                    "to_json" => {
                        let v = self.expr(&args[0]).expect("schema values exist");
                        self.builtin(Builtin::ToJson, vec![v])
                    }
                    _ => self.lower_parse_scalar(&args[0], e),
                })
            }
            ExprKind::Call { callee, args, .. } if matches!(callee.name.as_str(), "Ok" | "Err") => {
                let TTy::Result(r) = self.ty(e) else {
                    panic!("`{}` was not typed as a Result", callee.name)
                };
                let (ok, err) = self.sh.types.result_args(r);
                let variant = u32::from(callee.name == "Err");
                let want = if variant == 0 { ok } else { err };
                let at = self.ty(&args[0]);
                let v = self.expr(&args[0]);
                let payload = self.coerce(v, at, want);
                let union = self.sh.ir_result(r);
                let dst = self.temp(Ty::Union(union));
                self.emit(Inst::MakeUnion {
                    dst,
                    union,
                    variant,
                    payload,
                });
                Some(dst)
            }
            ExprKind::Call { callee, args, .. } if callee.name == "panic" => {
                let m = self.expr(&args[0]).expect("panic messages are str");
                let loc = (self.sh.locate)(e.span.start);
                let id = StrId(self.sh.strings.len() as u32);
                self.sh.strings.push(loc);
                let l = self.konst(Const::Str(id), Ty::Str);
                self.emit(Inst::Call {
                    dst: None,
                    callee: Callee::Builtin(Builtin::Panic),
                    args: vec![m, l],
                });
                // panic() は戻らない。後続の文は到達しないブロックに入る
                self.terminate(Terminator::Unreachable);
                None
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
                    "sleep_ms" => (Callee::Builtin(Builtin::SleepMs), vec![TTy::Int], TTy::None),
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
                let noreturn = matches!(target, Callee::Builtin(Builtin::Exit));
                self.emit(Inst::Call {
                    dst,
                    callee: target,
                    args: arg_vals,
                });
                if noreturn {
                    // exit() は戻らない。後続の文は到達しないブロックに入る
                    self.terminate(Terminator::Unreachable);
                }
                dst
            }
        }
    }

    /// `await operand`（ADR-0020 論点 5）。`async def` の呼び出しはその場で呼ぶ（タスクを作らない）。
    /// `Task` の値なら、終わるまで待って結果を受け取る。
    fn await_expr(&mut self, inner: &ast::Expr, e: &ast::Expr) -> Option<TempId> {
        // 型検査（`is_async_call`）と同じ判定: async の関数の呼び出しなら、戻り値が `Task` でもその場で呼ぶだけ
        let direct = match &inner.kind {
            ExprKind::Call { callee, .. } => {
                matches!(callee.name.as_str(), "sleep_ms" | "llm_generate")
                    || self
                        .sh
                        .func_index
                        .get(&callee.name)
                        .is_some_and(|&i| self.sh.tc.sigs[i].is_async)
            }
            _ => false,
        };
        if direct {
            return self.expr(inner);
        }
        let task = self.expr(inner).expect("tasks have values");
        let rt = self.ty(e);
        let dst = self.sh.ir_ty(rt).map(|t| self.temp(t));
        self.emit(Inst::Await { dst, task });
        dst
    }

    /// `spawn(f(args))`（ADR-0020 論点 8）。引数をその場で評価して、タスクに渡す。
    fn spawn(&mut self, call: &ast::Expr, e: &ast::Expr) -> TempId {
        let ExprKind::Call { callee, args, .. } = &call.kind else {
            panic!("spawn takes a call (checked by the type checker)")
        };
        let (func, params, _) = self.callee(&callee.name, call);
        let mut evaluated = Vec::new();
        for (a, pt) in args.iter().zip(params) {
            let v = self.expr(a);
            let at = self.ty(a);
            let v = self.coerce(v, at, pt);
            evaluated.push((v.expect("arguments have values"), self.cur));
        }
        let args: Vec<TempId> = evaluated
            .into_iter()
            .map(|(t, b)| self.materialize(t, b))
            .collect();
        let tt = self.ty(e);
        let Some(Ty::Task(task)) = self.sh.ir_ty(tt) else {
            panic!("spawn has a Task type")
        };
        let dst = self.temp(Ty::Task(task));
        self.emit(Inst::Spawn {
            dst,
            task,
            func,
            args,
        });
        dst
    }

    /// ローカル `slot` から読んだ一時値 `t` を、現在のブロックで使える形にする（ブロックが変わっていれば読み直す）。
    fn materialize_from_local(&mut self, t: TempId, slot: LocalId) -> TempId {
        if self.blocks[self.cur]
            .insts
            .iter()
            .any(|i| matches!(i, Inst::Load { dst, .. } if *dst == t))
        {
            return t;
        }
        let d = self.temp(self.locals[slot.0 as usize]);
        self.emit(Inst::Load { dst: d, src: slot });
        d
    }

    /// `for var in iter:`。`range(a, b)` は整数で数え、`list[T]` は添字で要素を順に取り出す（M3 T4a）。
    /// 反復対象のリストと位置は隠しローカルに置く（ループの各ブロックで読み直すため）。
    fn for_stmt(&mut self, var: &ast::Ident, iter: &ast::Expr, body: &[ast::Stmt]) {
        if matches!(self.ty(iter), TTy::Dict(_)) {
            return self.dict_for(var, iter, body);
        }
        let var_slot = self.local_ids[&var.name];
        let var_decl = self.local_decl[&var.name];
        let pos = self.hidden_local(Ty::Int);
        let end = self.hidden_local(Ty::Int);
        let mut list_slot = None;
        let mut elem_ty = TTy::Int;
        if let ExprKind::Call { callee, args, .. } = &iter.kind
            && callee.name == "range"
        {
            let (start, stop) = if args.len() == 2 {
                let a = self.expr(&args[0]).expect("range bounds are ints");
                let ab = self.cur;
                let b = self.expr(&args[1]).expect("range bounds are ints");
                (self.materialize(a, ab), b)
            } else {
                let z = self.konst(Const::Int(0), Ty::Int);
                let b = self.expr(&args[0]).expect("range bounds are ints");
                let z = self.materialize(z, self.cur);
                (z, b)
            };
            let stop_b = self.cur;
            self.emit(Inst::Store {
                dst: pos,
                src: start,
            });
            let stop = self.materialize(stop, stop_b);
            self.emit(Inst::Store {
                dst: end,
                src: stop,
            });
        } else {
            let TTy::List(l) = self.ty(iter) else {
                panic!("for over a non-list passed type checking")
            };
            elem_ty = self.sh.types.list_elem(l);
            let ls = match self.iterated_local(iter, body) {
                // ローカル変数をそのまま反復する（複製すると retain と release が1組増える。M4 T3）
                Some(slot) => slot,
                None => {
                    let v = self.expr(iter).expect("lists have values");
                    let lt = self.temp_ty(v);
                    let ls = self.hidden_local(lt);
                    self.emit(Inst::Store { dst: ls, src: v });
                    ls
                }
            };
            let z = self.konst(Const::Int(0), Ty::Int);
            self.emit(Inst::Store { dst: pos, src: z });
            list_slot = Some(ls);
        }
        let (head, body_b, exit) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Terminator::Jump(head));
        self.switch_to(head);
        let p = self.temp(Ty::Int);
        self.emit(Inst::Load { dst: p, src: pos });
        let limit = match list_slot {
            Some(ls) => {
                let lv = self.temp(self.locals[ls.0 as usize]);
                self.emit(Inst::Load { dst: lv, src: ls });
                let n = self.temp(Ty::Int);
                self.emit(Inst::ListLen { dst: n, src: lv });
                n
            }
            None => {
                let n = self.temp(Ty::Int);
                self.emit(Inst::Load { dst: n, src: end });
                n
            }
        };
        let more = self.temp(Ty::Bool);
        self.emit(Inst::Binary {
            dst: more,
            op: BinOp::Lt,
            lhs: p,
            rhs: limit,
        });
        self.terminate(Terminator::Branch {
            cond: more,
            then_block: body_b,
            else_block: exit,
        });
        self.switch_to(body_b);
        let p = self.temp(Ty::Int);
        self.emit(Inst::Load { dst: p, src: pos });
        let value = match list_slot {
            Some(ls) => {
                let lv = self.temp(self.locals[ls.0 as usize]);
                self.emit(Inst::Load { dst: lv, src: ls });
                let ir = self
                    .sh
                    .ir_ty(elem_ty)
                    .expect("list elements are never None");
                let x = self.temp(ir);
                self.emit(Inst::ListGet {
                    dst: x,
                    src: lv,
                    index: p,
                });
                self.coerce(Some(x), elem_ty, var_decl)
            }
            None => self.coerce(Some(p), TTy::Int, var_decl),
        };
        self.emit(Inst::Store {
            dst: var_slot,
            src: value.expect("loop variables have values"),
        });
        // 位置を先に進めてから本体を実行する（本体の中で return してもよいように、進め方を本体に依存させない）
        let p = self.temp(Ty::Int);
        self.emit(Inst::Load { dst: p, src: pos });
        let one = self.konst(Const::Int(1), Ty::Int);
        let next = self.temp(Ty::Int);
        self.emit(Inst::Binary {
            dst: next,
            op: BinOp::Add,
            lhs: p,
            rhs: one,
        });
        self.emit(Inst::Store {
            dst: pos,
            src: next,
        });
        self.stmts(body);
        if self.blocks[self.cur].term.is_none() {
            self.terminate(Terminator::Jump(head));
        }
        self.switch_to(exit);
    }

    /// メソッド呼び出しの受け手（ローカル変数）の値。宣言が Union なら、型検査が絞り込んだ構成要素（`want` を満たすもの）を取り出す。
    /// 3つ目は、値をローカルからそのまま読んだ場合のスロット。
    fn receiver(
        &mut self,
        recv: &str,
        want: impl Fn(TTy) -> bool,
    ) -> (TempId, TTy, Option<LocalId>) {
        let slot = self.local_ids[recv];
        let decl = self.local_decl[recv];
        let v = self.temp(self.locals[slot.0 as usize]);
        self.emit(Inst::Load { dst: v, src: slot });
        if want(decl) {
            return (v, decl, Some(slot));
        }
        if !matches!(decl, TTy::Union(_) | TTy::Unknown) {
            panic!("method receiver of type {decl:?} passed type checking")
        }
        let m = self
            .sum_members(decl)
            .into_iter()
            .find(|m| want(*m))
            .expect("type checking narrowed the receiver");
        let p = self.payload(v, decl, m).expect("containers have values");
        (p, m, None)
    }

    /// `d.get(k)`: キーがあれば値、なければ `None` の `V | None`（ADR-0015）。
    fn dict_get(&mut self, recv: &str, key: &ast::Expr, e: &ast::Expr) -> TempId {
        if let Some(t) = self.local_decl.get(recv).copied() {
            let t = self.sh.types.subst(t, &self.subst);
            let dict = self
                .sh
                .types
                .members(t)
                .into_iter()
                .find(|m| matches!(m, TTy::Dict(_)))
                .unwrap_or(t);
            if self.violates_unknown_rules(dict, e.span, true) {
                // `T | None` は `Unknown` に平坦化されている。IR は捨てられるので、型の合う値を返すだけ
                return self.make_sum(TTy::Unknown, TTy::None, None);
            }
        }
        let (u, out) = self.result_union(e);
        let (dv, dt, direct) = self.receiver(recv, |t| matches!(t, TTy::Dict(_)));
        let TTy::Dict(d) = dt else {
            unreachable!("receiver returns the requested type")
        };
        let (kt, vt) = self.sh.types.dict_args(d);
        let dty = self.temp_ty(dv);
        // 受け手が素のローカル変数なら、そのスロットを後のブロックで読み直す（複製すると、反復ごとに retain と
        // release が1組増える。キーの評価の間にローカル変数は変わらない: 代入の式はなく、呼ばれた関数からは届かない。
        // M4 T3 の Move 最適化）。絞り込んだ Union の中身は一時値なので、これまでどおり隠しスロットに置く
        let d_slot = match direct {
            Some(slot) => slot,
            None => {
                let d_slot = self.hidden_local(dty);
                self.emit(Inst::Store {
                    dst: d_slot,
                    src: dv,
                });
                d_slot
            }
        };
        let k = self.expr(key);
        let it = self.ty(key);
        let k = self.coerce(k, it, kt).expect("keys have values");
        let kty = self.temp_ty(k);
        let k_slot = self.hidden_local(kty);
        self.emit(Inst::Store {
            dst: k_slot,
            src: k,
        });
        let (dl, kl) = (self.temp(dty), self.temp(kty));
        self.emit(Inst::Load {
            dst: dl,
            src: d_slot,
        });
        self.emit(Inst::Load {
            dst: kl,
            src: k_slot,
        });
        let c = self.temp(Ty::Bool);
        self.emit(Inst::DictContains {
            dst: c,
            src: dl,
            key: kl,
        });
        let (some_b, none_b, join) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Terminator::Branch {
            cond: c,
            then_block: some_b,
            else_block: none_b,
        });
        self.switch_to(some_b);
        let (dl, kl) = (self.temp(dty), self.temp(kty));
        self.emit(Inst::Load {
            dst: dl,
            src: d_slot,
        });
        self.emit(Inst::Load {
            dst: kl,
            src: k_slot,
        });
        let vir = self.sh.ir_ty(vt).expect("dict values are never None");
        let v = self.temp(vir);
        self.emit(Inst::DictGet {
            dst: v,
            src: dl,
            key: kl,
        });
        let ut = TTy::Union(u);
        let w = self.coerce(Some(v), vt, ut).expect("unions have values");
        self.emit(Inst::Store { dst: out, src: w });
        self.terminate(Terminator::Jump(join));
        self.switch_to(none_b);
        let n = self.make_union(u, TTy::None, None);
        self.emit(Inst::Store { dst: out, src: n });
        self.terminate(Terminator::Jump(join));
        self.switch_to(join);
        let r = self.temp(Ty::Union(self.locals[out.0 as usize].union_id()));
        self.emit(Inst::Load { dst: r, src: out });
        r
    }

    /// `for k in d:` キーを挿入順に反復する。開始時のキーの数を覚えておき、変わればランタイムがパニックにする（ADR-0015）。
    /// `for x in xs:` の `xs` が、型を絞り込まずに使うローカル変数で、ループの本体（入れ子を含む）で代入しないなら、
    /// そのスロット（反復の対象を複製しなくてよい）。本体で代入するなら、Python と同じく元のコンテナを反復し続ける
    /// ために複製する。呼ばれた関数はローカル変数に届かないので、本体の代入文だけを調べればよい。
    fn iterated_local(&mut self, iter: &ast::Expr, body: &[ast::Stmt]) -> Option<LocalId> {
        fn assigns(stmts: &[ast::Stmt], name: &str) -> bool {
            stmts.iter().any(|s| match &s.kind {
                StmtKind::Assign { target, .. } => target.name == name,
                StmtKind::For { var, body, .. } => var.name == name || assigns(body, name),
                StmtKind::If {
                    branches,
                    else_body,
                } => {
                    branches.iter().any(|(_, b)| assigns(b, name))
                        || else_body.as_ref().is_some_and(|b| assigns(b, name))
                }
                StmtKind::While { body, .. } => assigns(body, name),
                StmtKind::Match { arms, .. } => arms.iter().any(|a| {
                    matches!(&a.pattern.kind, PatternKind::Bind(_, Some(v)) if v.name == name)
                        || assigns(&a.body, name)
                }),
                _ => false,
            })
        }
        let name = iter.as_name()?;
        let slot = *self.local_ids.get(name)?;
        // 絞り込んだ値（`list[int] | None` の中身など）は、スロットの型と違うので複製する
        let it = self.ty(iter);
        (self.local_decl.get(name) == Some(&it) && !assigns(body, name)).then_some(slot)
    }

    fn dict_for(&mut self, var: &ast::Ident, iter: &ast::Expr, body: &[ast::Stmt]) {
        let var_slot = self.local_ids[&var.name];
        let var_decl = self.local_decl[&var.name];
        let TTy::Dict(d) = self.ty(iter) else {
            unreachable!("checked by the caller")
        };
        let (kt, _) = self.sh.types.dict_args(d);
        let dty = Ty::Dict(self.sh.ir_dict(d));
        let d_slot = match self.iterated_local(iter, body) {
            // ローカル変数をそのまま反復する（M4 T3）
            Some(slot) => slot,
            None => {
                let dv = self.expr(iter).expect("dicts have values");
                let d_slot = self.hidden_local(dty);
                self.emit(Inst::Store {
                    dst: d_slot,
                    src: dv,
                });
                d_slot
            }
        };
        let (pos, len0, cur) = (
            self.hidden_local(Ty::Int),
            self.hidden_local(Ty::Int),
            self.hidden_local(Ty::Int),
        );
        let dl = self.temp(dty);
        self.emit(Inst::Load {
            dst: dl,
            src: d_slot,
        });
        let n = self.temp(Ty::Int);
        self.emit(Inst::DictLen { dst: n, src: dl });
        self.emit(Inst::Store { dst: len0, src: n });
        let z = self.konst(Const::Int(0), Ty::Int);
        self.emit(Inst::Store { dst: pos, src: z });
        let (head, body_b, exit) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Terminator::Jump(head));
        self.switch_to(head);
        let (dl, p, l0) = (self.temp(dty), self.temp(Ty::Int), self.temp(Ty::Int));
        self.emit(Inst::Load {
            dst: dl,
            src: d_slot,
        });
        self.emit(Inst::Load { dst: p, src: pos });
        self.emit(Inst::Load { dst: l0, src: len0 });
        let ent = self.temp(Ty::Int);
        self.emit(Inst::DictIterNext {
            dst: ent,
            src: dl,
            pos: p,
            len0: l0,
        });
        self.emit(Inst::Store { dst: cur, src: ent });
        let z = self.konst(Const::Int(0), Ty::Int);
        let more = self.temp(Ty::Bool);
        self.emit(Inst::Binary {
            dst: more,
            op: BinOp::Ge,
            lhs: ent,
            rhs: z,
        });
        self.terminate(Terminator::Branch {
            cond: more,
            then_block: body_b,
            else_block: exit,
        });
        self.switch_to(body_b);
        let (dl, c) = (self.temp(dty), self.temp(Ty::Int));
        self.emit(Inst::Load {
            dst: dl,
            src: d_slot,
        });
        self.emit(Inst::Load { dst: c, src: cur });
        let kir = self.sh.ir_ty(kt).expect("dict keys have values");
        let k = self.temp(kir);
        self.emit(Inst::DictKeyAt {
            dst: k,
            src: dl,
            index: c,
        });
        let kv = self.coerce(Some(k), kt, var_decl);
        self.emit(Inst::Store {
            dst: var_slot,
            src: kv.expect("loop variables have values"),
        });
        // 位置を先に進めてから本体を実行する（list の for と同じ）
        let one = self.konst(Const::Int(1), Ty::Int);
        let next = self.temp(Ty::Int);
        self.emit(Inst::Binary {
            dst: next,
            op: BinOp::Add,
            lhs: c,
            rhs: one,
        });
        self.emit(Inst::Store {
            dst: pos,
            src: next,
        });
        self.stmts(body);
        if self.blocks[self.cur].term.is_none() {
            self.terminate(Terminator::Jump(head));
        }
        self.switch_to(exit);
    }

    fn temp_ty(&self, t: TempId) -> Ty {
        self.temps[t.0 as usize]
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

    /// Generic な関数の本体は型パラメータのまま検査するので、`T = Unknown` で具体化したときの規則
    /// （ADR-0021 論点 4: 要素が `Unknown` のコンテナは読み取り専用、論点 7: `dict[str, Unknown]` の `.get` は使えない）は
    /// ここで確かめる。破っていれば診断を記録して真を返す（IR は捨てられる）
    fn violates_unknown_rules(&mut self, container: TTy, span: wys_span::Span, get: bool) -> bool {
        let inner = match container {
            TTy::List(l) => self.sh.types.list_elem(l),
            TTy::Dict(d) => self.sh.types.dict_args(d).1,
            _ => return false,
        };
        if !self.sh.types.contains_unknown(inner) || self.sh.error.is_some() {
            return false;
        }
        let ty = self.sh.types.name(container);
        let d = if get {
            wys_diagnostics::Diagnostic::error(
                "E0908",
                format!("`get` cannot be used on `{ty}`, whose values may be `None`"),
                span,
            )
            .with_cause("a generic function was instantiated with `Unknown`, which holds JSON `null` (ADR-0021 point 7)")
            .with_hint("check the key with `in` and then read `d[k]`")
        } else {
            wys_diagnostics::Diagnostic::error(
                "E1503",
                format!("cannot modify `{ty}`: a list or dict whose elements are `Unknown` is read-only"),
                span,
            )
            .with_cause("a generic function that modifies its argument was instantiated with `Unknown`; data reached from `Unknown` is an immutable tree (ADR-0021 point 4)")
            .with_hint("validate the data into a concrete type before passing it to a function that changes it")
        };
        self.sh.error = Some(d);
        true
    }

    fn result_union(&mut self, e: &ast::Expr) -> (wys_typeck::UnionId, LocalId) {
        let TTy::Union(u) = self.ty(e) else {
            panic!("input builtins return unions")
        };
        let iu = self.sh.ir_union(u);
        (u, self.hidden_local(Ty::Union(iu)))
    }

    /// `PyObject` のメソッド（ADR-0018 論点 9）。受け手を先に評価し、引数の評価でブロックが変わっても使えるようにする。
    fn py_method(
        &mut self,
        receiver: &ast::Expr,
        method: &str,
        args: &[ast::Expr],
        e: &ast::Expr,
    ) -> TempId {
        let h = self.expr(receiver).expect("handles have values");
        let hb = self.cur;
        let mut evaluated = Vec::new();
        for a in args {
            let v = self.expr(a).expect("arguments have values");
            evaluated.push((v, self.cur));
        }
        let h = self.materialize(h, hb);
        let mut vals = vec![h];
        vals.extend(evaluated.into_iter().map(|(t, b)| self.materialize(t, b)));
        let b = match method {
            "attr" => Builtin::PyGetAttr,
            "call" => Builtin::PyCall,
            "to_int" => Builtin::PyToInt,
            "to_bool" => Builtin::PyToBool,
            "to_str" => Builtin::PyToStr,
            "to_float" => Builtin::PyToFloat,
            "to_list" => Builtin::PyToList,
            "to_dict" => Builtin::PyToDict,
            "call_kw" => Builtin::PyCallKw,
            "is_none" => return self.builtin(Builtin::PyIsNone, vals),
            other => panic!("PyObject method `{other}` passed type checking"),
        };
        self.py_op(b, vals, e)
    }

    /// 失敗し得る Python の操作 `b(args)` を呼び、`Result[T, PyError]`（`e` の型）にする（ADR-0018 論点 8）。
    /// 成功なら値を `Ok` に、失敗なら例外の型名・メッセージ・トレースバックの `PyError` を `Err` に入れる。
    fn py_op(&mut self, b: Builtin, args: Vec<TempId>, e: &ast::Expr) -> TempId {
        let TTy::Result(r) = self.ty(e) else {
            panic!("Python operations return a Result")
        };
        let (ok_t, err) = self.sh.types.result_args(r);
        let union = self.sh.ir_result(r);
        let out = self.hidden_local(Ty::Union(union));
        // 値は失敗したときも（使わない値として）返るので、いったんローカルに置き、成功の側でだけ読む。
        // 値の型は `Ok` の型から決める（`py_to_list` / `py_to_dict` は型の ID が呼び出しごとに違う）
        let vt = self.sh.ir_ty(ok_t).expect("Python results have values");
        let v = self.temp(vt);
        self.emit(Inst::Call {
            dst: Some(v),
            callee: Callee::Builtin(b),
            args,
        });
        let slot = self.hidden_local(vt);
        self.emit(Inst::Store { dst: slot, src: v });
        let failed = self.builtin(Builtin::PyFailed, vec![]);
        let (ok_b, err_b, join) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Terminator::Branch {
            cond: failed,
            then_block: err_b,
            else_block: ok_b,
        });
        self.switch_to(ok_b);
        let lv = self.temp(vt);
        self.emit(Inst::Load { dst: lv, src: slot });
        let okv = self.temp(Ty::Union(union));
        self.emit(Inst::MakeUnion {
            dst: okv,
            union,
            variant: 0,
            payload: Some(lv),
        });
        self.emit(Inst::Store { dst: out, src: okv });
        self.terminate(Terminator::Jump(join));
        self.switch_to(err_b);
        let TTy::Record(rid) = err else {
            panic!("PyError is a record")
        };
        let record = self.sh.ir_record(rid);
        // フィールドは名前の順（kind, message, traceback）に並んでいる
        let fields: Vec<TempId> = [
            Builtin::PyErrKind,
            Builtin::PyErrMessage,
            Builtin::PyErrTraceback,
        ]
        .into_iter()
        .map(|b| self.builtin(b, vec![]))
        .collect();
        let rec = self.temp(Ty::Record(record));
        self.emit(Inst::MakeRecord {
            dst: rec,
            record,
            fields,
        });
        let errv = self.temp(Ty::Union(union));
        self.emit(Inst::MakeUnion {
            dst: errv,
            union,
            variant: 1,
            payload: Some(rec),
        });
        self.emit(Inst::Store {
            dst: out,
            src: errv,
        });
        self.terminate(Terminator::Jump(join));
        self.switch_to(join);
        let d = self.temp(Ty::Union(union));
        self.emit(Inst::Load { dst: d, src: out });
        d
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
    fn lower_parse_scalar(&mut self, text: &ast::Expr, _e: &ast::Expr) -> TempId {
        let u = TTy::Unknown;
        let out = self.hidden_local(Ty::Unknown);
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
        for (code, member) in [
            (0, TTy::Int),
            (1, TTy::Bool),
            (3, TTy::None),
            (4, TTy::Float),
            (2, TTy::Str),
        ] {
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
                    TTy::Float => self.builtin(Builtin::ScalarFloat, vec![ls]),
                    _ => ls,
                })
            };
            let v = self.make_sum(u, member, payload);
            self.emit(Inst::Store { dst: out, src: v });
            self.terminate(Terminator::Jump(join));
            if let Some(n) = next {
                self.switch_to(n);
            }
        }
        self.switch_to(join);
        let d = self.temp(Ty::Unknown);
        self.emit(Inst::Load { dst: d, src: out });
        d
    }

    /// 値を型 `to`（組み込みのレコード）にそろえてから JSON の文字列にする（ランタイムはレコードの配置を知らない）
    fn json_of(&mut self, arg: &ast::Expr, to: TTy) -> TempId {
        let v = self.expr(arg);
        let at = self.ty(arg);
        let v = self.coerce(v, at, to).expect("records have values");
        self.builtin(Builtin::ToJson, vec![v])
    }

    /// `LlmError { kind, message }`（フィールドは名前の順）を `Err` に入れて `out` に置く
    fn store_llm_error(
        &mut self,
        out: LocalId,
        union: UnionId,
        record: RecordId,
        kind: TempId,
        message: TempId,
    ) {
        let ev = self.temp(Ty::Record(record));
        self.emit(Inst::MakeRecord {
            dst: ev,
            record,
            fields: vec![kind, message],
        });
        let errv = self.temp(Ty::Union(union));
        self.emit(Inst::MakeUnion {
            dst: errv,
            union,
            variant: 1,
            payload: Some(ev),
        });
        self.emit(Inst::Store {
            dst: out,
            src: errv,
        });
    }

    /// `await llm_generate[T](req)`（ADR-0022 論点 8）: ランタイムが LLM を呼び、本文を返す。`T = str` ならそのまま、
    /// それ以外は `parse_json` → `validate[T]`（T6 の glue）。失敗はどれも `Err(LlmError)`
    fn lower_llm_generate(&mut self, arg: &ast::Expr, e: &ast::Expr) -> TempId {
        let TTy::Result(r) = self.ty(e) else {
            panic!("llm_generate returns a Result")
        };
        let (ok_t, err) = self.sh.types.result_args(r);
        let union = self.sh.ir_result(r);
        let TTy::Record(erid) = err else {
            panic!("LlmError is a record")
        };
        let record = self.sh.ir_record(erid);
        let out = self.hidden_local(Ty::Union(union));
        let req = self.sh.types.llm_request();
        let j = self.json_of(arg, req);
        let vt = self.sh.ir_ty(ok_t).expect("schema values exist");
        let schema = if ok_t == TTy::Str {
            self.string_const("")
        } else {
            self.konst(Const::Schema(vt), Ty::Str)
        };
        let text = self.temp(Ty::Str);
        self.emit(Inst::Call {
            dst: Some(text),
            callee: Callee::Builtin(Builtin::LlmGenerate),
            args: vec![j, schema],
        });
        let text_slot = self.hidden_local(Ty::Str);
        self.emit(Inst::Store {
            dst: text_slot,
            src: text,
        });
        let failed = self.builtin(Builtin::LlmFailed, vec![]);
        let (ok_b, err_b, join) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Terminator::Branch {
            cond: failed,
            then_block: err_b,
            else_block: ok_b,
        });
        self.switch_to(err_b);
        let kind = self.builtin(Builtin::LlmErrKind, vec![]);
        let msg = self.builtin(Builtin::LlmErrMessage, vec![]);
        self.store_llm_error(out, union, record, kind, msg);
        self.terminate(Terminator::Jump(join));
        self.switch_to(ok_b);
        let lt = self.temp(Ty::Str);
        self.emit(Inst::Load {
            dst: lt,
            src: text_slot,
        });
        if ok_t == TTy::Str {
            let okv = self.temp(Ty::Union(union));
            self.emit(Inst::MakeUnion {
                dst: okv,
                union,
                variant: 0,
                payload: Some(lt),
            });
            self.emit(Inst::Store { dst: out, src: okv });
            self.terminate(Terminator::Jump(join));
        } else {
            // 本文 → Unknown（壊れていれば invalid JSON）→ 型 T
            let u = self.temp(Ty::Unknown);
            self.emit(Inst::Call {
                dst: Some(u),
                callee: Callee::Builtin(Builtin::JsonParse),
                args: vec![lt],
            });
            let u_slot = self.hidden_local(Ty::Unknown);
            self.emit(Inst::Store {
                dst: u_slot,
                src: u,
            });
            let jfailed = self.builtin(Builtin::JsonFailed, vec![]);
            let (parsed_b, bad_json_b) = (self.new_block(), self.new_block());
            self.terminate(Terminator::Branch {
                cond: jfailed,
                then_block: bad_json_b,
                else_block: parsed_b,
            });
            self.switch_to(bad_json_b);
            let kind = self.string_const("schema");
            let msg = self.builtin(Builtin::LlmJsonRejected, vec![]);
            self.store_llm_error(out, union, record, kind, msg);
            self.terminate(Terminator::Jump(join));
            self.switch_to(parsed_b);
            let lu = self.temp(Ty::Unknown);
            self.emit(Inst::Load {
                dst: lu,
                src: u_slot,
            });
            let v = self.temp(vt);
            self.emit(Inst::Call {
                dst: Some(v),
                callee: Callee::Builtin(Builtin::Validate),
                args: vec![lu],
            });
            let v_slot = self.hidden_local(vt);
            self.emit(Inst::Store {
                dst: v_slot,
                src: v,
            });
            let sfailed = self.builtin(Builtin::SchemaFailed, vec![]);
            let (valid_b, invalid_b) = (self.new_block(), self.new_block());
            self.terminate(Terminator::Branch {
                cond: sfailed,
                then_block: invalid_b,
                else_block: valid_b,
            });
            self.switch_to(invalid_b);
            let kind = self.string_const("schema");
            let msg = self.builtin(Builtin::LlmSchemaRejected, vec![]);
            self.store_llm_error(out, union, record, kind, msg);
            self.terminate(Terminator::Jump(join));
            self.switch_to(valid_b);
            let lv = self.temp(vt);
            self.emit(Inst::Load {
                dst: lv,
                src: v_slot,
            });
            let okv = self.temp(Ty::Union(union));
            self.emit(Inst::MakeUnion {
                dst: okv,
                union,
                variant: 0,
                payload: Some(lv),
            });
            self.emit(Inst::Store { dst: out, src: okv });
            self.terminate(Terminator::Jump(join));
        }
        self.switch_to(join);
        let d = self.temp(Ty::Union(union));
        self.emit(Inst::Load { dst: d, src: out });
        d
    }

    /// `audit_events()`: ランタイムが JSON の配列を返し、T6 の glue で `list[AuditEvent]` にする。壊れていればコンパイラかランタイムの誤り
    fn lower_audit_events(&mut self, e: &ast::Expr) -> TempId {
        let lt = self.ty(e);
        let vt = self.sh.ir_ty(lt).expect("lists have values");
        let j = self.builtin(Builtin::AuditJson, vec![]);
        let u = self.temp(Ty::Unknown);
        self.emit(Inst::Call {
            dst: Some(u),
            callee: Callee::Builtin(Builtin::JsonParse),
            args: vec![j],
        });
        let v = self.temp(vt);
        self.emit(Inst::Call {
            dst: Some(v),
            callee: Callee::Builtin(Builtin::Validate),
            args: vec![u],
        });
        let slot = self.hidden_local(vt);
        self.emit(Inst::Store { dst: slot, src: v });
        let failed = self.builtin(Builtin::SchemaFailed, vec![]);
        let (ok_b, bad_b) = (self.new_block(), self.new_block());
        self.terminate(Terminator::Branch {
            cond: failed,
            then_block: bad_b,
            else_block: ok_b,
        });
        self.switch_to(bad_b);
        let m = self.string_const("audit events are malformed (this is a compiler bug)");
        let at = self.string_const("audit_events");
        self.emit(Inst::Call {
            dst: None,
            callee: Callee::Builtin(Builtin::Panic),
            args: vec![m, at],
        });
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok_b);
        let d = self.temp(vt);
        self.emit(Inst::Load { dst: d, src: slot });
        d
    }

    /// `validate[T](u)`（ADR-0022 論点 6）: 型ごとの glue が木を検証する。失敗したら `SchemaError` の `Err`。
    fn lower_validate(&mut self, arg: &ast::Expr, e: &ast::Expr) -> TempId {
        let TTy::Result(r) = self.ty(e) else {
            panic!("validate returns a Result")
        };
        let (ok_t, err) = self.sh.types.result_args(r);
        let vt = self.sh.ir_ty(ok_t).expect("schema values exist");
        let union = self.sh.ir_result(r);
        let out = self.hidden_local(Ty::Union(union));
        // 引数は `Unknown` に置ける値（絞り込んだ `Unknown`、`int | None` など）。`Unknown` に広げてから渡す
        let u = self.expr(arg);
        let at = self.ty(arg);
        let u = self
            .coerce(u, at, TTy::Unknown)
            .expect("Unknown values exist");
        let v = self.temp(vt);
        self.emit(Inst::Call {
            dst: Some(v),
            callee: Callee::Builtin(Builtin::Validate),
            args: vec![u],
        });
        // 失敗したときの値はゼロ（解放しても何もしない）
        let slot = self.hidden_local(vt);
        self.emit(Inst::Store { dst: slot, src: v });
        let failed = self.builtin(Builtin::SchemaFailed, vec![]);
        let (ok_b, err_b, join) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Terminator::Branch {
            cond: failed,
            then_block: err_b,
            else_block: ok_b,
        });
        self.switch_to(ok_b);
        let lv = self.temp(vt);
        self.emit(Inst::Load { dst: lv, src: slot });
        let okv = self.temp(Ty::Union(union));
        self.emit(Inst::MakeUnion {
            dst: okv,
            union,
            variant: 0,
            payload: Some(lv),
        });
        self.emit(Inst::Store { dst: out, src: okv });
        self.terminate(Terminator::Jump(join));
        self.switch_to(err_b);
        let TTy::Record(rid) = err else {
            panic!("SchemaError is a record")
        };
        let record = self.sh.ir_record(rid);
        // フィールドは名前の順（expected, found, path）
        let expected = self.builtin(Builtin::SchemaErrExpected, vec![]);
        let found = self.builtin(Builtin::SchemaErrFound, vec![]);
        let path = self.builtin(Builtin::SchemaErrPath, vec![]);
        let ev = self.temp(Ty::Record(record));
        self.emit(Inst::MakeRecord {
            dst: ev,
            record,
            fields: vec![expected, found, path],
        });
        let errv = self.temp(Ty::Union(union));
        self.emit(Inst::MakeUnion {
            dst: errv,
            union,
            variant: 1,
            payload: Some(ev),
        });
        self.emit(Inst::Store {
            dst: out,
            src: errv,
        });
        self.terminate(Terminator::Jump(join));
        self.switch_to(join);
        let d = self.temp(Ty::Union(union));
        self.emit(Inst::Load { dst: d, src: out });
        d
    }

    /// `parse_json(s)`（ADR-0021 論点 5）: ランタイムが木を作る。失敗したら `JsonError` の `Err`。
    fn lower_parse_json(&mut self, text: &ast::Expr, e: &ast::Expr) -> TempId {
        let TTy::Result(r) = self.ty(e) else {
            panic!("parse_json returns a Result")
        };
        let (_, err) = self.sh.types.result_args(r);
        let union = self.sh.ir_result(r);
        let out = self.hidden_local(Ty::Union(union));
        let s = self.expr(text).expect("the text has a value");
        let v = self.temp(Ty::Unknown);
        self.emit(Inst::Call {
            dst: Some(v),
            callee: Callee::Builtin(Builtin::JsonParse),
            args: vec![s],
        });
        // 失敗したときの値は `None` の種類なので、置いたまま捨ててよい
        let slot = self.hidden_local(Ty::Unknown);
        self.emit(Inst::Store { dst: slot, src: v });
        let failed = self.builtin(Builtin::JsonFailed, vec![]);
        let (ok_b, err_b, join) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Terminator::Branch {
            cond: failed,
            then_block: err_b,
            else_block: ok_b,
        });
        self.switch_to(ok_b);
        let lv = self.temp(Ty::Unknown);
        self.emit(Inst::Load { dst: lv, src: slot });
        let okv = self.temp(Ty::Union(union));
        self.emit(Inst::MakeUnion {
            dst: okv,
            union,
            variant: 0,
            payload: Some(lv),
        });
        self.emit(Inst::Store { dst: out, src: okv });
        self.terminate(Terminator::Jump(join));
        self.switch_to(err_b);
        let TTy::Record(rid) = err else {
            panic!("JsonError is a record")
        };
        let record = self.sh.ir_record(rid);
        // フィールドは名前の順（message, offset）に並んでいる
        let msg = self.builtin(Builtin::JsonErrMessage, vec![]);
        let off = self.builtin(Builtin::JsonErrOffset, vec![]);
        let ev = self.temp(Ty::Record(record));
        self.emit(Inst::MakeRecord {
            dst: ev,
            record,
            fields: vec![msg, off],
        });
        let errv = self.temp(Ty::Union(union));
        self.emit(Inst::MakeUnion {
            dst: errv,
            union,
            variant: 1,
            payload: Some(ev),
        });
        self.emit(Inst::Store {
            dst: out,
            src: errv,
        });
        self.terminate(Terminator::Jump(join));
        self.switch_to(join);
        let d = self.temp(Ty::Union(union));
        self.emit(Inst::Load { dst: d, src: out });
        d
    }

    // ---- unions ----

    fn members(&self, u: wys_typeck::UnionId) -> Vec<TTy> {
        self.sh.types.members(TTy::Union(u))
    }

    /// Union と `Unknown`（ADR-0021 論点 3）の構成要素。`Unknown` は `wys_ir::UNKNOWN_KINDS` の順
    fn sum_members(&self, t: TTy) -> Vec<TTy> {
        match t {
            TTy::Union(u) => self.members(u),
            TTy::Unknown => {
                let (l, d) = self.sh.unknown_containers;
                vec![TTy::Int, TTy::Bool, TTy::Str, TTy::Float, TTy::None, l, d]
            }
            other => panic!("{other:?} is not a union"),
        }
    }

    fn variant(&self, t: TTy, m: TTy) -> u32 {
        self.sum_members(t)
            .iter()
            .position(|x| *x == m)
            .unwrap_or_else(|| panic!("{m:?} is not a member of {t:?}")) as u32
    }

    /// Union か `Unknown` の値を作る。
    fn make_sum(&mut self, t: TTy, m: TTy, payload: Option<TempId>) -> TempId {
        match t {
            TTy::Union(u) => self.make_union(u, m, payload),
            _ => {
                let kind = self.variant(t, m);
                let dst = self.temp(Ty::Unknown);
                self.emit(Inst::MakeUnknown { dst, kind, payload });
                dst
            }
        }
    }

    fn make_union(&mut self, u: wys_typeck::UnionId, m: TTy, payload: Option<TempId>) -> TempId {
        let dst_iu = self.sh.ir_union(u);
        let dst = self.temp(Ty::Union(dst_iu));
        let variant = self.variant(TTy::Union(u), m);
        self.emit(Inst::MakeUnion {
            dst,
            union: dst_iu,
            variant,
            payload,
        });
        dst
    }

    fn payload(&mut self, v: TempId, u: TTy, m: TTy) -> Option<TempId> {
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
        if from == to {
            return v;
        }
        let is_sum = |t: TTy| matches!(t, TTy::Union(_) | TTy::Unknown);
        match (from, to) {
            // 値の型そのものが変換先の構成要素（`Unknown` を `Result[Unknown, E]` などに入れる）
            (_, TTy::Union(tu)) if self.members(tu).contains(&from) => {
                Some(self.make_union(tu, from, v))
            }
            (f, t) if is_sum(f) && is_sum(t) => {
                let v = v.expect("union values exist");
                Some(self.sum_to_sum(v, f, t))
            }
            (f, base) if is_sum(f) => self.payload(v.expect("union values exist"), f, base),
            (base, t) if is_sum(t) => {
                let tm = self.target_member(t, base);
                let p = self.coerce(v, base, tm);
                Some(self.make_sum(t, tm, p))
            }
            (TTy::Record(a), TTy::Record(b)) => {
                Some(self.record_to_record(v.expect("records have values"), a, b))
            }
            _ => panic!("no conversion from {from:?} to {to:?}"),
        }
    }

    /// Union `tu` の中で、値の型 `m` を受け取る構成要素。同じ型がなければ、`m` を幅部分型として受け取るレコード
    /// （型検査により、1つの Union に入るレコード型は1つまで）。
    fn target_member(&self, tu: TTy, m: TTy) -> TTy {
        let members = self.sum_members(tu);
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
                "print"
                    | "spawn"
                    | "sleep_ms"
                    | "exit"
                    | "panic"
                    | "arg_count"
                    | "arg"
                    | "parse_scalar"
                    | "parse_json"
                    | "validate"
                    | "to_json"
                    | "llm_generate"
                    | "llm_mock_push"
                    | "audit_events"
                    | "assert"
                    | "Ok"
                    | "Err"
                    | "len"
                    | "float"
                    | "int"
                    | "py_import"
                    | "py_value"
                    | "range"
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
    /// Union / `Unknown` から Union / `Unknown` へ: タグで分岐し、共通の構成要素ごとに詰め直す（ADR-0021 論点 3）。
    fn sum_to_sum(&mut self, v: TempId, fu: TTy, tu: TTy) -> TempId {
        // 変換元の各構成要素が、変換先のどの構成要素に入るか（同じ型か、レコードの幅部分型）。変換先そのもの
        // （`Unknown | R` の `Unknown` を `Unknown` に）なら、中身をそのまま使う（`None` で表す）。
        // 変換先に入らない構成要素は、型検査（絞り込み）によりこの地点には来ない。
        let target = self.sum_members(tu);
        let pairs: Vec<(TTy, Option<TTy>)> = self
            .sum_members(fu)
            .into_iter()
            .filter_map(|m| {
                if m == tu {
                    return Some((m, None));
                }
                target
                    .iter()
                    .find(|t| **t == m)
                    .or_else(|| target.iter().find(|t| self.sh.types.member_fits(m, **t)))
                    .map(|t| (m, Some(*t)))
            })
            .collect();
        let common: Vec<TTy> = pairs.iter().map(|(m, _)| *m).collect();
        let src_ty = self.sh.ir_ty(fu).expect("unions have values");
        let src = self.hidden_local(src_ty);
        self.emit(Inst::Store { dst: src, src: v });
        let out_ty = self.sh.ir_ty(tu).expect("unions have values");
        let out = self.hidden_local(out_ty);
        self.for_each_variant(src, fu, &common, |l, val, m| {
            let tm = pairs
                .iter()
                .find(|(f, _)| *f == m)
                .map(|(_, t)| *t)
                .expect("paired above");
            let p = l.payload(val, fu, m);
            let u = match tm {
                None => p.expect("the target type has values"),
                Some(tm) => {
                    let p = l.coerce(p, m, tm);
                    l.make_sum(tu, tm, p)
                }
            };
            l.emit(Inst::Store { dst: out, src: u });
        });
        let dst = self.temp(out_ty);
        self.emit(Inst::Load { dst, src: out });
        dst
    }

    /// ローカル `src` に入った Union 値について、`variants` のどれに当たるかで分岐し、それぞれで `f` を実行する。
    /// 最後の構成要素は検査せずに実行する（型検査で、値がこのどれかであることは保証済み）。
    fn for_each_variant(
        &mut self,
        src: LocalId,
        u: TTy,
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
        // `int` / `bool` / `str` は専用の組み込み関数、構造を持つ値は型ごとの表示関数（ADR-0016 論点 17）
        fn builtin(t: TTy) -> Builtin {
            match t {
                TTy::Int => Builtin::PrintInt,
                TTy::Bool => Builtin::PrintBool,
                TTy::Str => Builtin::PrintStr,
                TTy::Float => Builtin::PrintFloat,
                _ => Builtin::PrintValue,
            }
        }
        match ty {
            // トップレベルの Union は構成要素ごとに表示する（`None` の構成要素は型検査で除いている）
            TTy::Union(u) => {
                let src_iu = self.sh.ir_union(u);
                let src = self.hidden_local(Ty::Union(src_iu));
                self.emit(Inst::Store { dst: src, src: v });
                let members = self.members(u);
                self.for_each_variant(src, TTy::Union(u), &members, |l, val, m| {
                    let p = l
                        .payload(val, TTy::Union(u), m)
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
        if matches!(op, A::In | A::NotIn) {
            // k in d（ADR-0015）: 左辺から評価する
            let TTy::Dict(d) = self.ty(rhs) else {
                panic!("`in` on a non-dict passed type checking")
            };
            let (kt, _) = self.sh.types.dict_args(d);
            let k = self.expr(lhs);
            let lt = self.ty(lhs);
            let k = self.coerce(k, lt, kt).expect("keys have values");
            let kb = self.cur;
            let dv = self.expr(rhs).expect("dicts have values");
            let k = self.materialize(k, kb);
            let c = self.temp(Ty::Bool);
            self.emit(Inst::DictContains {
                dst: c,
                src: dv,
                key: k,
            });
            if op == A::NotIn {
                let n = self.temp(Ty::Bool);
                self.emit(Inst::Not { dst: n, src: c });
                return n;
            }
            return c;
        }
        let (lt, rt) = (self.ty(lhs), self.ty(rhs));
        if matches!(op, A::Eq | A::Ne)
            && (lt != rt || !matches!(lt, TTy::Int | TTy::Bool | TTy::Str))
        {
            // 構造を持つ値の比較（ADR-0016）。型が違えば（`o == 1`、`1 == o`）、広い方の Union に揃える
            let wide = if self.sh.types.members(lt).len() >= self.sh.types.members(rt).len() {
                lt
            } else {
                rt
            };
            let l = self.expr(lhs);
            let l = self.coerce(l, lt, wide).expect("compared values exist");
            let l_block = self.cur;
            let r = self.expr(rhs);
            let r = self.coerce(r, rt, wide).expect("compared values exist");
            let l = self.materialize(l, l_block);
            let (iop, dst) = (
                if op == A::Eq { BinOp::Eq } else { BinOp::Ne },
                self.temp(Ty::Bool),
            );
            self.emit(Inst::Binary {
                dst,
                op: iop,
                lhs: l,
                rhs: r,
            });
            return dst;
        }
        let l = self.expr(lhs).expect("operands have values");
        let l_block = self.cur;
        let r = self.expr(rhs).expect("operands have values");
        let l = self.materialize(l, l_block);
        if self.ty(lhs) == TTy::Str {
            // 文字列の連結と比較はランタイムの関数（M3 T2b）
            let b = match op {
                A::Add => Builtin::StrConcat,
                A::Eq | A::Ne => Builtin::StrEq,
                _ => unreachable!("only +, == and != are allowed on str"),
            };
            let d = self.builtin(b, vec![l, r]);
            if op == A::Ne {
                let n = self.temp(Ty::Bool);
                self.emit(Inst::Not { dst: n, src: d });
                return n;
            }
            return d;
        }
        // 四則の結果は被演算子と同じ型（int か float。ADR-0019）
        let num = if self.ty(lhs) == TTy::Float {
            Ty::Float
        } else {
            Ty::Int
        };
        let (iop, ty) = match op {
            A::Add => (BinOp::Add, num),
            A::Sub => (BinOp::Sub, num),
            A::Mul => (BinOp::Mul, num),
            A::Div => (BinOp::Div, Ty::Float),
            A::FloorDiv => (BinOp::FloorDiv, Ty::Int),
            A::Mod => (BinOp::Mod, Ty::Int),
            A::Eq => (BinOp::Eq, Ty::Bool),
            A::Ne => (BinOp::Ne, Ty::Bool),
            A::Lt => (BinOp::Lt, Ty::Bool),
            A::Le => (BinOp::Le, Ty::Bool),
            A::Gt => (BinOp::Gt, Ty::Bool),
            A::Ge => (BinOp::Ge, Ty::Bool),
            A::And | A::Or | A::In | A::NotIn => {
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
