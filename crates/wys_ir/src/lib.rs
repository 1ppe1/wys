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

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Bool,
    Str,
    Union(UnionId),
    Record(RecordId),
}

impl Ty {
    pub fn name(self) -> String {
        match self {
            Ty::Int => "int".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "str".into(),
            Ty::Union(u) => format!("u{}", u.0),
            Ty::Record(r) => format!("r{}", r.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnionId(pub u32);

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
    /// Union 型の構成要素。`Ty::Union(UnionId(i))` は `unions[i]` を指す。構成要素は Union を含まない。
    pub unions: Vec<Vec<Member>>,
    /// レコード型のフィールド（名前と型、この順で配置する）。`Ty::Record(RecordId(i))` は `records[i]` を指す。
    pub records: Vec<Vec<(String, Ty)>>,
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
}

impl Builtin {
    pub fn name(self) -> &'static str {
        match self {
            Builtin::PrintInt => "print_int",
            Builtin::PrintBool => "print_bool",
            Builtin::PrintStr => "print_str",
            Builtin::Exit => "exit",
        }
    }

    fn signature(self) -> &'static [Ty] {
        match self {
            Builtin::PrintInt | Builtin::Exit => &[Ty::Int],
            Builtin::PrintBool => &[Ty::Bool],
            Builtin::PrintStr => &[Ty::Str],
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
        if members.len() < 2 || members.iter().any(|m| matches!(m, Some(Ty::Union(_)))) {
            return Err(format!(
                "union u{i} must have at least two non-union members"
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

fn verify_types(p: &Program, f: &Function) -> Result<(), String> {
    for t in f.locals.iter().chain(&f.temps).chain(f.ret.iter()) {
        match t {
            Ty::Union(u) if u.0 as usize >= p.unions.len() => {
                return Err(format!("unknown union u{}", u.0));
            }
            Ty::Record(r) if r.0 as usize >= p.records.len() => {
                return Err(format!("unknown record r{}", r.0));
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
                        Callee::Builtin(b) => (b.signature().to_vec(), None),
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
