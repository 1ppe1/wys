//! 共通IR（COMP-001）の定義・検証・ダンプ
//!
//! 設計（PoC1 時点）:
//! - 関数は基本ブロックの列。各ブロックは命令列と1つの終端命令を持つ。
//! - ローカル変数（`LocalId`）は可変のスロット。一時値（`TempId`）は一度だけ定義され、
//!   **定義したブロック内でのみ** 使える。ブロックをまたぐ値はローカルを経由する。
//! - 整数演算（Add/Sub/Mul/FloorDiv/Mod）はオーバーフローと0除算を実行時に検査する意味論を持つ。
//! - 値を持たない（`None` 型の）式は一時値を作らない。
//! - Union はタグ付き値（ADR-0009）。`Program::unions` に構成要素を登録し、構成要素の番号がタグになる。
//!   `None` の構成要素は中身（payload）を持たない。

pub mod arc;

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Bool,
    Str,
    Union(UnionId),
    Record(RecordId),
    /// `list[T]`（ヒープ上のリストへのポインタ1語、M3 T4a）
    List(ListId),
    /// `dict[K, V]`（ヒープ上の辞書へのポインタ1語、M3 T6a、ADR-0015）
    Dict(DictId),
}

impl Ty {
    /// Union 型の ID。Union でなければパニックする。
    pub fn union_id(self) -> UnionId {
        match self {
            Ty::Union(u) => u,
            other => panic!("{other:?} is not a union"),
        }
    }

    pub fn name(self) -> String {
        match self {
            Ty::Int => "int".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "str".into(),
            Ty::Union(u) => format!("u{}", u.0),
            Ty::Record(r) => format!("r{}", r.0),
            Ty::List(l) => format!("list{}", l.0),
            Ty::Dict(d) => format!("dict{}", d.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DictId(pub u32);

/// 不変のレコード（ADR-0009: 値型、受け渡しはコピー）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordId(pub u32);

/// Union の構成要素。`None` は中身を持たない構成要素。
pub type Member = Option<Ty>;

fn member_name(m: Member) -> String {
    m.map_or("None".into(), Ty::name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TempId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub strings: Vec<String>,
    /// Union 型の構成要素。`Ty::Union(UnionId(i))` は `unions[i]` を指す。構成要素の Union（`Result` の表現）は `i` より前に登録したもの。
    pub unions: Vec<Vec<Member>>,
    /// レコード型のフィールド（名前と型、この順で配置する）。`Ty::Record(RecordId(i))` は `records[i]` を指す。
    pub records: Vec<Vec<(String, Ty)>>,
    /// `list[T]` の要素の型。`Ty::List(ListId(i))` は `lists[i]` を指す。
    pub lists: Vec<Ty>,
    /// `dict[K, V]` のキーと値の型。`Ty::Dict(DictId(i))` は `dicts[i]` を指す。キーは `int` / `bool` / `str`。
    pub dicts: Vec<(Ty, Ty)>,
    pub funcs: Vec<Function>,
    /// エントリポイント（引数なし・戻り値なし）。
    pub entry: FuncId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    /// 先頭 `param_count` 個のローカルが引数。
    pub param_count: u32,
    pub locals: Vec<Ty>,
    pub ret: Option<Ty>,
    pub temps: Vec<Ty>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub term: Terminator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Const {
    Int(i64),
    Bool(bool),
    Str(StrId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    FloorDiv,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn name(self) -> &'static str {
        match self {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::FloorDiv => "floordiv",
            BinOp::Mod => "mod",
            BinOp::Eq => "eq",
            BinOp::Ne => "ne",
            BinOp::Lt => "lt",
            BinOp::Le => "le",
            BinOp::Gt => "gt",
            BinOp::Ge => "ge",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    PrintInt,
    PrintBool,
    PrintStr,
    Exit,
    /// 外部入力（ADR-0011）。`arg` と `parse_scalar` は lowering がこれらを組み合わせて作る。
    ArgCount,
    ArgExists,
    /// `ArgExists` が真の番号にだけ使う。
    ArgStr,
    /// 0 = int、1 = bool、2 = str、3 = null
    ScalarKind,
    ScalarInt,
    ScalarBool,
    /// `wys test` で実行するテストの番号
    TestIndex,
    /// `assert` の失敗（引数は場所の文字列）。戻らない。
    AssertFail,
    /// `panic(msg)`（引数はメッセージと場所の文字列）。戻らない（ADR-0013）。
    Panic,
    /// 文字列の連結。新しいオブジェクトを +1 所有で返す（M3 T2b）
    StrConcat,
    /// 文字列の中身の比較
    StrEq,
    /// 文字列の長さ（文字数 = Unicode のコードポイント数）
    StrLen,
}

impl Builtin {
    pub fn name(self) -> &'static str {
        match self {
            Builtin::PrintInt => "print_int",
            Builtin::PrintBool => "print_bool",
            Builtin::PrintStr => "print_str",
            Builtin::Exit => "exit",
            Builtin::ArgCount => "arg_count",
            Builtin::ArgExists => "arg_exists",
            Builtin::ArgStr => "arg_str",
            Builtin::ScalarKind => "scalar_kind",
            Builtin::ScalarInt => "scalar_int",
            Builtin::ScalarBool => "scalar_bool",
            Builtin::TestIndex => "test_index",
            Builtin::AssertFail => "assert_fail",
            Builtin::Panic => "panic",
            Builtin::StrConcat => "str_concat",
            Builtin::StrEq => "str_eq",
            Builtin::StrLen => "str_len",
        }
    }

    /// 戻り値を +1 所有で返すか（新しく作ったオブジェクト）。そうでなければ借用（不死の文字列など）。
    pub fn returns_owned(self) -> bool {
        matches!(self, Builtin::StrConcat)
    }

    /// 戻り値の型（なければ `None`）。
    pub fn ret(self) -> Option<Ty> {
        match self {
            Builtin::PrintInt
            | Builtin::PrintBool
            | Builtin::PrintStr
            | Builtin::Exit
            | Builtin::AssertFail
            | Builtin::Panic => None,
            Builtin::ArgCount | Builtin::ScalarKind | Builtin::ScalarInt | Builtin::TestIndex => {
                Some(Ty::Int)
            }
            Builtin::ArgExists | Builtin::ScalarBool | Builtin::StrEq => Some(Ty::Bool),
            Builtin::ArgStr | Builtin::StrConcat => Some(Ty::Str),
            Builtin::StrLen => Some(Ty::Int),
        }
    }

    fn signature(self) -> &'static [Ty] {
        match self {
            Builtin::PrintInt | Builtin::Exit => &[Ty::Int],
            Builtin::PrintBool => &[Ty::Bool],
            Builtin::PrintStr => &[Ty::Str],
            Builtin::ArgCount | Builtin::TestIndex => &[],
            Builtin::AssertFail | Builtin::StrLen => &[Ty::Str],
            Builtin::StrConcat | Builtin::StrEq | Builtin::Panic => &[Ty::Str, Ty::Str],
            Builtin::ArgExists | Builtin::ArgStr => &[Ty::Int],
            Builtin::ScalarKind | Builtin::ScalarInt | Builtin::ScalarBool => &[Ty::Str],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callee {
    Func(FuncId),
    Builtin(Builtin),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inst {
    Const {
        dst: TempId,
        value: Const,
    },
    Load {
        dst: TempId,
        src: LocalId,
    },
    Store {
        dst: LocalId,
        src: TempId,
    },
    Binary {
        dst: TempId,
        op: BinOp,
        lhs: TempId,
        rhs: TempId,
    },
    Not {
        dst: TempId,
        src: TempId,
    },
    Call {
        dst: Option<TempId>,
        callee: Callee,
        args: Vec<TempId>,
    },
    /// 構成要素 `variant` の Union 値を作る。`None` の構成要素なら `payload` は `None`。
    MakeUnion {
        dst: TempId,
        union: UnionId,
        variant: u32,
        payload: Option<TempId>,
    },
    /// 参照カウントを1増やす（`str` はオブジェクト、Union / レコードは中の参照型すべて）。ARC パスが挿入する。
    Retain {
        src: TempId,
        /// 挿入した理由（ADR-0017）。バックエンドは読まない。
        reason: RcReason,
    },
    /// 参照カウントを1減らし、0 になったら解放する。ARC パスが挿入する。
    Release {
        src: TempId,
        /// 挿入した理由（ADR-0017）。バックエンドは読まない。
        reason: RcReason,
    },
    /// リストを作る（`items` を +1 で消費する）。
    ListNew {
        dst: TempId,
        list: ListId,
        items: Vec<TempId>,
    },
    ListLen {
        dst: TempId,
        src: TempId,
    },
    /// 要素の読み出し（借用。負の添字は末尾から、範囲外はパニック）。
    ListGet {
        dst: TempId,
        src: TempId,
        index: TempId,
    },
    /// 要素の置き換え（`value` を +1 で消費し、古い要素を release する）。
    ListSet {
        list: TempId,
        index: TempId,
        value: TempId,
    },
    /// 末尾への追加（`value` を +1 で消費する）。
    ListPush {
        list: TempId,
        value: TempId,
    },
    /// 辞書を作る（`keys` と `vals` を +1 で消費する。同じキーは後のものが勝つ）。
    DictNew {
        dst: TempId,
        dict: DictId,
        keys: Vec<TempId>,
        vals: Vec<TempId>,
    },
    DictLen {
        dst: TempId,
        src: TempId,
    },
    /// キーがあるか（`bool`）。
    DictContains {
        dst: TempId,
        src: TempId,
        key: TempId,
    },
    /// 値の読み出し（借用。キーがなければパニック）。
    DictGet {
        dst: TempId,
        src: TempId,
        key: TempId,
    },
    /// 値を取り出してキーを削除する（`pop`）。値は +1 所有で返す。キーがなければパニック。
    DictPop {
        dst: TempId,
        src: TempId,
        key: TempId,
    },
    /// 追加または置き換え（`key` と `value` を +1 で消費する。既存のキーなら、新しいキーと古い値を release する）。
    DictSet {
        dict: TempId,
        key: TempId,
        value: TempId,
    },
    /// エントリ `index` のキー（借用）。
    DictKeyAt {
        dst: TempId,
        src: TempId,
        index: TempId,
    },
    /// 反復: `pos` 以降の最初のエントリの番号、なければ -1。開始時のキーの数 `len0` と違えばパニック。
    DictIterNext {
        dst: TempId,
        src: TempId,
        pos: TempId,
        len0: TempId,
    },
    /// レコード値を作る。`fields` はレコードのフィールドの順。
    MakeRecord {
        dst: TempId,
        record: RecordId,
        fields: Vec<TempId>,
    },
    /// レコード値の `index` 番目のフィールド。
    Field {
        dst: TempId,
        src: TempId,
        index: u32,
    },
    /// Union 値のタグが `variant` かどうか（`bool`）。
    UnionIs {
        dst: TempId,
        src: TempId,
        variant: u32,
    },
    /// Union 値から構成要素 `variant` の中身を取り出す。タグが違う値に使ってはならない（lowering が保証する）。
    UnionPayload {
        dst: TempId,
        src: TempId,
        variant: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Return(Option<TempId>),
    Jump(BlockId),
    Branch {
        cond: TempId,
        then_block: BlockId,
        else_block: BlockId,
    },
    Unreachable,
}

// ---------------------------------------------------------------- verifier

/// IR の整合性検査。エラーはコンパイラのバグを意味する。
pub fn verify(p: &Program) -> Result<(), String> {
    let entry = p
        .funcs
        .get(p.entry.0 as usize)
        .ok_or("entry function does not exist")?;
    if entry.param_count != 0 || entry.ret.is_some() {
        return Err("entry function must take no parameters and return nothing".into());
    }
    for (i, members) in p.unions.iter().enumerate() {
        // 構成要素の Union は、自分より前に登録したものだけ（`Result` は Union の構成要素になれる。ADR-0013）。
        // 循環した入れ子を防ぐ
        if members.len() < 2
            || members
                .iter()
                .any(|m| matches!(m, Some(Ty::Union(u)) if u.0 as usize >= i))
        {
            return Err(format!(
                "union u{i} must have at least two members, and nested unions must be registered before it"
            ));
        }
    }
    for f in &p.funcs {
        verify_function(p, f).map_err(|e| format!("in function `{}`: {e}", f.name))?;
    }
    Ok(())
}

fn union_member(p: &Program, u: UnionId, variant: u32) -> Result<Member, String> {
    let members = p
        .unions
        .get(u.0 as usize)
        .ok_or_else(|| format!("unknown union u{}", u.0))?;
    members
        .get(variant as usize)
        .copied()
        .ok_or_else(|| format!("union u{} has no variant {variant}", u.0))
}

fn dict_types(p: &Program, d: DictId) -> Result<(Ty, Ty), String> {
    p.dicts
        .get(d.0 as usize)
        .copied()
        .ok_or_else(|| format!("unknown dict type dict{}", d.0))
}

fn list_elem(p: &Program, l: ListId) -> Result<Ty, String> {
    p.lists
        .get(l.0 as usize)
        .copied()
        .ok_or_else(|| format!("unknown list type list{}", l.0))
}

fn verify_types(p: &Program, f: &Function) -> Result<(), String> {
    for t in f.locals.iter().chain(&f.temps).chain(f.ret.iter()) {
        match t {
            Ty::Union(u) if u.0 as usize >= p.unions.len() => {
                return Err(format!("unknown union u{}", u.0));
            }
            Ty::Record(r) if r.0 as usize >= p.records.len() => {
                return Err(format!("unknown record r{}", r.0));
            }
            Ty::List(l) if l.0 as usize >= p.lists.len() => {
                return Err(format!("unknown list type list{}", l.0));
            }
            Ty::Dict(d) if d.0 as usize >= p.dicts.len() => {
                return Err(format!("unknown dict type dict{}", d.0));
            }
            _ => {}
        }
    }
    Ok(())
}

fn verify_function(p: &Program, f: &Function) -> Result<(), String> {
    verify_types(p, f)?;
    if f.param_count as usize > f.locals.len() {
        return Err("param_count exceeds number of locals".into());
    }
    if f.blocks.is_empty() {
        return Err("function has no blocks".into());
    }
    let mut defined_in: Vec<Option<usize>> = vec![None; f.temps.len()];
    let temp_ty = |t: TempId| -> Result<Ty, String> {
        f.temps
            .get(t.0 as usize)
            .copied()
            .ok_or_else(|| format!("unknown temp t{}", t.0))
    };
    let local_ty = |l: LocalId| -> Result<Ty, String> {
        f.locals
            .get(l.0 as usize)
            .copied()
            .ok_or_else(|| format!("unknown local l{}", l.0))
    };
    let check_block = |b: BlockId| -> Result<(), String> {
        if (b.0 as usize) < f.blocks.len() {
            Ok(())
        } else {
            Err(format!("unknown block bb{}", b.0))
        }
    };
    for (bi, block) in f.blocks.iter().enumerate() {
        let define = |t: TempId, defined_in: &mut Vec<Option<usize>>| -> Result<(), String> {
            let slot = defined_in
                .get_mut(t.0 as usize)
                .ok_or_else(|| format!("unknown temp t{}", t.0))?;
            if slot.is_some() {
                return Err(format!("temp t{} defined more than once", t.0));
            }
            *slot = Some(bi);
            Ok(())
        };
        let use_ = |t: TempId, defined_in: &Vec<Option<usize>>| -> Result<Ty, String> {
            match defined_in.get(t.0 as usize) {
                Some(Some(b)) if *b == bi => temp_ty(t),
                Some(Some(_)) => Err(format!("temp t{} used outside its defining block", t.0)),
                _ => Err(format!("temp t{} used before definition", t.0)),
            }
        };
        let expect = |what: &str, got: Ty, want: Ty| -> Result<(), String> {
            if got == want {
                Ok(())
            } else {
                Err(format!(
                    "bb{bi}: {what}: expected {}, found {}",
                    want.name(),
                    got.name()
                ))
            }
        };
        for inst in &block.insts {
            match inst {
                Inst::Const { dst, value } => {
                    let ty = match value {
                        Const::Int(_) => Ty::Int,
                        Const::Bool(_) => Ty::Bool,
                        Const::Str(s) => {
                            if s.0 as usize >= p.strings.len() {
                                return Err(format!("unknown string s{}", s.0));
                            }
                            Ty::Str
                        }
                    };
                    expect("const", temp_ty(*dst)?, ty)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Load { dst, src } => {
                    expect("load", temp_ty(*dst)?, local_ty(*src)?)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Store { dst, src } => {
                    expect("store", use_(*src, &defined_in)?, local_ty(*dst)?)?;
                }
                Inst::Binary { dst, op, lhs, rhs } => {
                    let lt = use_(*lhs, &defined_in)?;
                    let rt = use_(*rhs, &defined_in)?;
                    expect("binary operands", rt, lt)?;
                    let operand_ok = match op {
                        BinOp::Eq | BinOp::Ne => matches!(lt, Ty::Int | Ty::Bool),
                        _ => lt == Ty::Int,
                    };
                    if !operand_ok {
                        return Err(format!("bb{bi}: {} on {}", op.name(), lt.name()));
                    }
                    let rty = if op.is_comparison() {
                        Ty::Bool
                    } else {
                        Ty::Int
                    };
                    expect("binary result", temp_ty(*dst)?, rty)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::MakeUnion {
                    dst,
                    union,
                    variant,
                    payload,
                } => {
                    let member = union_member(p, *union, *variant)?;
                    match (member, payload) {
                        (None, None) => {}
                        (Some(want), Some(t)) => {
                            expect("union payload", use_(*t, &defined_in)?, want)?
                        }
                        _ => {
                            return Err(format!(
                                "bb{bi}: union payload does not match the variant"
                            ));
                        }
                    }
                    expect("make_union result", temp_ty(*dst)?, Ty::Union(*union))?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Retain { src, .. } | Inst::Release { src, .. } => {
                    use_(*src, &defined_in)?;
                }
                Inst::ListNew { dst, list, items } => {
                    let elem = list_elem(p, *list)?;
                    for it in items {
                        expect("list item", use_(*it, &defined_in)?, elem)?;
                    }
                    expect("list_new result", temp_ty(*dst)?, Ty::List(*list))?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::ListLen { dst, src } => {
                    let Ty::List(_) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: list_len on a non-list value"));
                    };
                    expect("list_len result", temp_ty(*dst)?, Ty::Int)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::ListGet { dst, src, index } => {
                    let Ty::List(l) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: list_get on a non-list value"));
                    };
                    expect("list index", use_(*index, &defined_in)?, Ty::Int)?;
                    expect("list_get result", temp_ty(*dst)?, list_elem(p, l)?)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::ListSet { list, index, value } => {
                    let Ty::List(l) = use_(*list, &defined_in)? else {
                        return Err(format!("bb{bi}: list_set on a non-list value"));
                    };
                    expect("list index", use_(*index, &defined_in)?, Ty::Int)?;
                    expect(
                        "list_set value",
                        use_(*value, &defined_in)?,
                        list_elem(p, l)?,
                    )?;
                }
                Inst::ListPush { list, value } => {
                    let Ty::List(l) = use_(*list, &defined_in)? else {
                        return Err(format!("bb{bi}: list_push on a non-list value"));
                    };
                    expect(
                        "list_push value",
                        use_(*value, &defined_in)?,
                        list_elem(p, l)?,
                    )?;
                }
                Inst::DictNew {
                    dst,
                    dict,
                    keys,
                    vals,
                } => {
                    let (kt, vt) = dict_types(p, *dict)?;
                    if keys.len() != vals.len() {
                        return Err(format!("bb{bi}: dict_new key / value count mismatch"));
                    }
                    for (k, v) in keys.iter().zip(vals) {
                        expect("dict key", use_(*k, &defined_in)?, kt)?;
                        expect("dict value", use_(*v, &defined_in)?, vt)?;
                    }
                    expect("dict_new result", temp_ty(*dst)?, Ty::Dict(*dict))?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::DictLen { dst, src } => {
                    let Ty::Dict(_) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: dict_len on a non-dict value"));
                    };
                    expect("dict_len result", temp_ty(*dst)?, Ty::Int)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::DictContains { dst, src, key }
                | Inst::DictGet { dst, src, key }
                | Inst::DictPop { dst, src, key } => {
                    let Ty::Dict(d) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: dict lookup on a non-dict value"));
                    };
                    let (kt, vt) = dict_types(p, d)?;
                    expect("dict key", use_(*key, &defined_in)?, kt)?;
                    let want = if matches!(inst, Inst::DictGet { .. } | Inst::DictPop { .. }) {
                        vt
                    } else {
                        Ty::Bool
                    };
                    expect("dict lookup result", temp_ty(*dst)?, want)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::DictSet { dict, key, value } => {
                    let Ty::Dict(d) = use_(*dict, &defined_in)? else {
                        return Err(format!("bb{bi}: dict_set on a non-dict value"));
                    };
                    let (kt, vt) = dict_types(p, d)?;
                    expect("dict key", use_(*key, &defined_in)?, kt)?;
                    expect("dict value", use_(*value, &defined_in)?, vt)?;
                }
                Inst::DictKeyAt { dst, src, index } => {
                    let Ty::Dict(d) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: dict_key_at on a non-dict value"));
                    };
                    expect("dict entry index", use_(*index, &defined_in)?, Ty::Int)?;
                    expect("dict_key_at result", temp_ty(*dst)?, dict_types(p, d)?.0)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::DictIterNext {
                    dst,
                    src,
                    pos,
                    len0,
                } => {
                    let Ty::Dict(_) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: dict_iter_next on a non-dict value"));
                    };
                    expect("dict position", use_(*pos, &defined_in)?, Ty::Int)?;
                    expect("dict length", use_(*len0, &defined_in)?, Ty::Int)?;
                    expect("dict_iter_next result", temp_ty(*dst)?, Ty::Int)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::MakeRecord {
                    dst,
                    record,
                    fields,
                } => {
                    let def = p
                        .records
                        .get(record.0 as usize)
                        .ok_or_else(|| format!("unknown record r{}", record.0))?;
                    if def.len() != fields.len() {
                        return Err(format!("bb{bi}: make_record field count mismatch"));
                    }
                    for (t, (_, want)) in fields.iter().zip(def) {
                        expect("record field", use_(*t, &defined_in)?, *want)?;
                    }
                    expect("make_record result", temp_ty(*dst)?, Ty::Record(*record))?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Field { dst, src, index } => {
                    let Ty::Record(r) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: field access on a non-record value"));
                    };
                    let (_, want) = p.records[r.0 as usize]
                        .get(*index as usize)
                        .ok_or_else(|| format!("bb{bi}: record r{} has no field {index}", r.0))?;
                    expect("field result", temp_ty(*dst)?, *want)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::UnionIs { dst, src, variant } => {
                    let Ty::Union(u) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: union_is on a non-union value"));
                    };
                    union_member(p, u, *variant)?;
                    expect("union_is result", temp_ty(*dst)?, Ty::Bool)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::UnionPayload { dst, src, variant } => {
                    let Ty::Union(u) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: union_payload on a non-union value"));
                    };
                    let Some(want) = union_member(p, u, *variant)? else {
                        return Err(format!("bb{bi}: the None variant has no payload"));
                    };
                    expect("union_payload result", temp_ty(*dst)?, want)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Not { dst, src } => {
                    expect("not operand", use_(*src, &defined_in)?, Ty::Bool)?;
                    expect("not result", temp_ty(*dst)?, Ty::Bool)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Call { dst, callee, args } => {
                    let (params, ret): (Vec<Ty>, Option<Ty>) = match callee {
                        Callee::Builtin(b) => (b.signature().to_vec(), b.ret()),
                        Callee::Func(id) => {
                            let g = p
                                .funcs
                                .get(id.0 as usize)
                                .ok_or_else(|| format!("unknown function f{}", id.0))?;
                            (g.locals[..g.param_count as usize].to_vec(), g.ret)
                        }
                    };
                    if params.len() != args.len() {
                        return Err(format!("bb{bi}: call argument count mismatch"));
                    }
                    for (a, want) in args.iter().zip(params) {
                        expect("call argument", use_(*a, &defined_in)?, want)?;
                    }
                    match (dst, ret) {
                        (Some(d), Some(r)) => {
                            expect("call result", temp_ty(*d)?, r)?;
                            define(*d, &mut defined_in)?;
                        }
                        (None, _) => {}
                        (Some(_), None) => {
                            return Err(format!(
                                "bb{bi}: call to function without result has a destination"
                            ));
                        }
                    }
                }
            }
        }
        match &block.term {
            Terminator::Return(v) => match (v, f.ret) {
                (None, None) => {}
                (Some(t), Some(r)) => expect("return", use_(*t, &defined_in)?, r)?,
                _ => {
                    return Err(format!(
                        "bb{bi}: return does not match the function's return type"
                    ));
                }
            },
            Terminator::Jump(b) => check_block(*b)?,
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                expect("branch condition", use_(*cond, &defined_in)?, Ty::Bool)?;
                check_block(*then_block)?;
                check_block(*else_block)?;
            }
            Terminator::Unreachable => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- dump

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.strings.iter().enumerate() {
            writeln!(f, "string s{i} = {s:?}")?;
        }
        for (i, fields) in self.records.iter().enumerate() {
            let fs: Vec<String> = fields
                .iter()
                .map(|(n, t)| format!("{n}: {}", t.name()))
                .collect();
            writeln!(f, "record r{i} = {{{}}}", fs.join(", "))?;
        }
        for (i, members) in self.unions.iter().enumerate() {
            let names: Vec<String> = members.iter().map(|m| member_name(*m)).collect();
            writeln!(f, "union u{i} = {}", names.join(" | "))?;
        }
        for (i, (k, v)) in self.dicts.iter().enumerate() {
            writeln!(f, "dict dict{i} = dict[{}, {}]", k.name(), v.name())?;
        }
        writeln!(f, "entry f{}", self.entry.0)?;
        for (i, func) in self.funcs.iter().enumerate() {
            writeln!(f)?;
            write!(f, "fn f{i} {}(", func.name)?;
            for p in 0..func.param_count as usize {
                if p > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "l{p}: {}", func.locals[p].name())?;
            }
            write!(f, ")")?;
            if let Some(r) = func.ret {
                write!(f, " -> {}", r.name())?;
            }
            writeln!(f, " {{")?;
            for (l, ty) in func
                .locals
                .iter()
                .enumerate()
                .skip(func.param_count as usize)
            {
                writeln!(f, "  local l{l}: {}", ty.name())?;
            }
            for (bi, b) in func.blocks.iter().enumerate() {
                writeln!(f, "bb{bi}:")?;
                for inst in &b.insts {
                    write!(f, "  ")?;
                    match inst {
                        Inst::Const { dst, value } => match value {
                            Const::Int(v) => writeln!(f, "t{} = const {v}", dst.0)?,
                            Const::Bool(v) => writeln!(f, "t{} = const {v}", dst.0)?,
                            Const::Str(s) => writeln!(f, "t{} = const s{}", dst.0, s.0)?,
                        },
                        Inst::Load { dst, src } => writeln!(f, "t{} = load l{}", dst.0, src.0)?,
                        Inst::Store { dst, src } => writeln!(f, "store l{}, t{}", dst.0, src.0)?,
                        Inst::Binary { dst, op, lhs, rhs } => {
                            writeln!(f, "t{} = {} t{}, t{}", dst.0, op.name(), lhs.0, rhs.0)?
                        }
                        Inst::Not { dst, src } => writeln!(f, "t{} = not t{}", dst.0, src.0)?,
                        Inst::MakeUnion {
                            dst,
                            union,
                            variant,
                            payload,
                        } => match payload {
                            Some(t) => writeln!(
                                f,
                                "t{} = make_union u{}.{variant}(t{})",
                                dst.0, union.0, t.0
                            )?,
                            None => {
                                writeln!(f, "t{} = make_union u{}.{variant}()", dst.0, union.0)?
                            }
                        },
                        Inst::UnionIs { dst, src, variant } => {
                            writeln!(f, "t{} = union_is t{}, {variant}", dst.0, src.0)?
                        }
                        Inst::Retain { src, reason } => {
                            writeln!(f, "retain t{} ; {reason}", src.0)?
                        }
                        Inst::ListNew { dst, list, items } => {
                            let xs: Vec<String> =
                                items.iter().map(|t| format!("t{}", t.0)).collect();
                            writeln!(f, "t{} = list_new list{}[{}]", dst.0, list.0, xs.join(", "))?
                        }
                        Inst::ListLen { dst, src } => {
                            writeln!(f, "t{} = list_len t{}", dst.0, src.0)?
                        }
                        Inst::ListGet { dst, src, index } => {
                            writeln!(f, "t{} = list_get t{}, t{}", dst.0, src.0, index.0)?
                        }
                        Inst::ListSet { list, index, value } => {
                            writeln!(f, "list_set t{}, t{}, t{}", list.0, index.0, value.0)?
                        }
                        Inst::ListPush { list, value } => {
                            writeln!(f, "list_push t{}, t{}", list.0, value.0)?
                        }
                        Inst::Release { src, reason } => {
                            writeln!(f, "release t{} ; {reason}", src.0)?
                        }
                        Inst::DictNew {
                            dst,
                            dict,
                            keys,
                            vals,
                        } => {
                            let xs: Vec<String> = keys
                                .iter()
                                .zip(vals)
                                .map(|(k, v)| format!("t{}: t{}", k.0, v.0))
                                .collect();
                            writeln!(
                                f,
                                "t{} = dict_new dict{}{{{}}}",
                                dst.0,
                                dict.0,
                                xs.join(", ")
                            )?
                        }
                        Inst::DictLen { dst, src } => {
                            writeln!(f, "t{} = dict_len t{}", dst.0, src.0)?
                        }
                        Inst::DictContains { dst, src, key } => {
                            writeln!(f, "t{} = dict_contains t{}, t{}", dst.0, src.0, key.0)?
                        }
                        Inst::DictGet { dst, src, key } => {
                            writeln!(f, "t{} = dict_get t{}, t{}", dst.0, src.0, key.0)?
                        }
                        Inst::DictPop { dst, src, key } => {
                            writeln!(f, "t{} = dict_pop t{}, t{}", dst.0, src.0, key.0)?
                        }
                        Inst::DictSet { dict, key, value } => {
                            writeln!(f, "dict_set t{}, t{}, t{}", dict.0, key.0, value.0)?
                        }
                        Inst::DictKeyAt { dst, src, index } => {
                            writeln!(f, "t{} = dict_key_at t{}, t{}", dst.0, src.0, index.0)?
                        }
                        Inst::DictIterNext {
                            dst,
                            src,
                            pos,
                            len0,
                        } => writeln!(
                            f,
                            "t{} = dict_iter_next t{}, t{}, t{}",
                            dst.0, src.0, pos.0, len0.0
                        )?,
                        Inst::MakeRecord {
                            dst,
                            record,
                            fields,
                        } => {
                            let fs: Vec<String> =
                                fields.iter().map(|t| format!("t{}", t.0)).collect();
                            writeln!(
                                f,
                                "t{} = make_record r{}({})",
                                dst.0,
                                record.0,
                                fs.join(", ")
                            )?
                        }
                        Inst::Field { dst, src, index } => {
                            writeln!(f, "t{} = field t{}, {index}", dst.0, src.0)?
                        }
                        Inst::UnionPayload { dst, src, variant } => {
                            writeln!(f, "t{} = union_payload t{}, {variant}", dst.0, src.0)?
                        }
                        Inst::Call { dst, callee, args } => {
                            if let Some(d) = dst {
                                write!(f, "t{} = ", d.0)?;
                            }
                            match callee {
                                Callee::Func(id) => write!(f, "call f{}(", id.0)?,
                                Callee::Builtin(b) => write!(f, "call @{}(", b.name())?,
                            }
                            for (i, a) in args.iter().enumerate() {
                                if i > 0 {
                                    write!(f, ", ")?;
                                }
                                write!(f, "t{}", a.0)?;
                            }
                            writeln!(f, ")")?;
                        }
                    }
                }
                match &b.term {
                    Terminator::Return(None) => writeln!(f, "  return")?,
                    Terminator::Return(Some(t)) => writeln!(f, "  return t{}", t.0)?,
                    Terminator::Jump(b) => writeln!(f, "  jump bb{}", b.0)?,
                    Terminator::Branch {
                        cond,
                        then_block,
                        else_block,
                    } => writeln!(
                        f,
                        "  branch t{}, bb{}, bb{}",
                        cond.0, then_block.0, else_block.0
                    )?,
                    Terminator::Unreachable => writeln!(f, "  unreachable")?,
                }
            }
            writeln!(f, "}}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Program {
        Program {
            strings: vec!["hi".into()],
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
                temps: vec![Ty::Str],
                blocks: vec![Block {
                    insts: vec![
                        Inst::Const {
                            dst: TempId(0),
                            value: Const::Str(StrId(0)),
                        },
                        Inst::Call {
                            dst: None,
                            callee: Callee::Builtin(Builtin::PrintStr),
                            args: vec![TempId(0)],
                        },
                    ],
                    term: Terminator::Return(None),
                }],
            }],
        }
    }

    #[test]
    fn valid_program_verifies_and_dumps() {
        let p = hello();
        verify(&p).unwrap();
        let text = p.to_string();
        assert!(text.contains("t0 = const s0"), "{text}");
        assert!(text.contains("call @print_str(t0)"), "{text}");
    }

    #[test]
    fn rejects_cross_block_temp_use() {
        let mut p = hello();
        let f = &mut p.funcs[0];
        f.blocks[0].insts.pop();
        f.blocks[0].term = Terminator::Jump(BlockId(1));
        f.blocks.push(Block {
            insts: vec![Inst::Call {
                dst: None,
                callee: Callee::Builtin(Builtin::PrintStr),
                args: vec![TempId(0)],
            }],
            term: Terminator::Return(None),
        });
        let err = verify(&p).unwrap_err();
        assert!(err.contains("outside its defining block"), "{err}");
    }

    #[test]
    fn union_instructions_verify_and_dump() {
        let mut p = hello();
        p.unions = vec![vec![Some(Ty::Str), None]];
        let f = &mut p.funcs[0];
        f.temps.extend([Ty::Union(UnionId(0)), Ty::Bool, Ty::Str]);
        f.blocks[0].insts.insert(
            1,
            Inst::MakeUnion {
                dst: TempId(1),
                union: UnionId(0),
                variant: 0,
                payload: Some(TempId(0)),
            },
        );
        f.blocks[0].insts.insert(
            2,
            Inst::UnionIs {
                dst: TempId(2),
                src: TempId(1),
                variant: 1,
            },
        );
        f.blocks[0].insts.insert(
            3,
            Inst::UnionPayload {
                dst: TempId(3),
                src: TempId(1),
                variant: 0,
            },
        );
        verify(&p).unwrap();
        let text = p.to_string();
        assert!(text.contains("union u0 = str | None"), "{text}");
        assert!(text.contains("t1 = make_union u0.0(t0)"), "{text}");
        // None の構成要素から中身は取り出せない
        p.funcs[0].blocks[0].insts[3] = Inst::UnionPayload {
            dst: TempId(3),
            src: TempId(1),
            variant: 1,
        };
        assert!(verify(&p).is_err());
    }

    #[test]
    fn rejects_type_errors() {
        let mut p = hello();
        p.funcs[0].blocks[0].insts[1] = Inst::Call {
            dst: None,
            callee: Callee::Builtin(Builtin::PrintInt),
            args: vec![TempId(0)],
        };
        assert!(verify(&p).is_err());
    }
}

/// 値を +1 で消費する地点（ARC パスの `consume`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConsumeSite {
    Store,
    Return,
    MakeUnion,
    MakeRecord,
    ListNew,
    ListSet,
    ListPush,
    DictNew,
    DictSet,
}

impl ConsumeSite {
    /// 安定した識別子（`wys explain` の JSON に出す）。
    pub fn id(self) -> &'static str {
        match self {
            ConsumeSite::Store => "store",
            ConsumeSite::Return => "return",
            ConsumeSite::MakeUnion => "make_union",
            ConsumeSite::MakeRecord => "make_record",
            ConsumeSite::ListNew => "list_new",
            ConsumeSite::ListSet => "list_set",
            ConsumeSite::ListPush => "list_push",
            ConsumeSite::DictNew => "dict_new",
            ConsumeSite::DictSet => "dict_set",
        }
    }
}

/// ARC パスが `Retain` / `Release` を挿入した理由（ADR-0017）。ARC パスの挿入地点と1対1に対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RcReason {
    /// retain: 関数の入口で、引数をスロットに所有させる。
    ParamSlotEntry,
    /// retain: 借用した値（ローカルからの読み出し、リテラル、取り出した値、不死の文字列）を +1 で消費する。
    ConsumeBorrowed(ConsumeSite),
    /// retain: すでに消費した +1 所有の値を、もう一度消費する。
    ConsumeSharedOwned(ConsumeSite),
    /// retain: 可変な場所（リストの要素、`dict` の値やキー）から読んだ値を、使い終わるまで保持する（借用の安全条件）。
    BorrowFromMutablePlace,
    /// release: `Store` で、スロットの古い値を手放す。
    StoreOverwritesSlot,
    /// release: ブロックの終わりまで消費されなかった +1 所有の値を手放す。
    UnconsumedOwnedAtBlockEnd,
    /// release: 関数を抜けるときに、スロットが持つ値を手放す。
    SlotAtFunctionExit,
}

impl RcReason {
    /// 安定した識別子。`retain` / `release` という語を含めない（IR テキストで命令を数えるテストを壊さないため）。
    pub fn id(self) -> &'static str {
        match self {
            RcReason::ParamSlotEntry => "param_slot_entry",
            RcReason::ConsumeBorrowed(_) => "consume_borrowed",
            RcReason::ConsumeSharedOwned(_) => "consume_shared_owned",
            RcReason::BorrowFromMutablePlace => "borrow_from_mutable_place",
            RcReason::StoreOverwritesSlot => "store_overwrites_slot",
            RcReason::UnconsumedOwnedAtBlockEnd => "unconsumed_owned_at_block_end",
            RcReason::SlotAtFunctionExit => "slot_at_function_exit",
        }
    }

    /// 消費する地点（`consume_*` のときだけ）。
    pub fn site(self) -> Option<ConsumeSite> {
        match self {
            RcReason::ConsumeBorrowed(s) | RcReason::ConsumeSharedOwned(s) => Some(s),
            _ => None,
        }
    }

    /// 人間と AI 向けの説明（英語。診断のメッセージと同じ扱い）。
    pub fn message(self) -> String {
        match self {
            RcReason::ParamSlotEntry => {
                "parameters are passed borrowed (+0); the callee's slot takes its own reference on entry".into()
            }
            RcReason::ConsumeBorrowed(s) => format!(
                "a borrowed value is consumed by `{}`, which needs an owned (+1) reference",
                s.id()
            ),
            RcReason::ConsumeSharedOwned(s) => format!(
                "an owned value that was already consumed is consumed again by `{}`",
                s.id()
            ),
            RcReason::BorrowFromMutablePlace => {
                "a value read from a mutable place (list element, dict value or key) is held while it is used (borrow safety, memory-model 2.2)".into()
            }
            RcReason::StoreOverwritesSlot => {
                "storing to a local gives up the reference held by its old value".into()
            }
            RcReason::UnconsumedOwnedAtBlockEnd => {
                "an owned (+1) value was not consumed in this block; together with its retain this is a candidate for the Move optimization".into()
            }
            RcReason::SlotAtFunctionExit => {
                "leaving the function gives up the reference held by a local slot".into()
            }
        }
    }
}

impl fmt::Display for RcReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.site() {
            Some(s) => write!(f, "{}:{}", self.id(), s.id()),
            None => f.write_str(self.id()),
        }
    }
}
