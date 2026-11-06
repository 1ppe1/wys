//! 抽象構文木の定義
//!
//! 各 `Expr` はモジュール内で一意な `ExprId` を持ち、型検査の結果はこの ID で引く。

use wys_span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExprId(pub u32);

#[derive(Debug, Clone)]
pub struct Module {
    pub imports: Vec<Import>,
    pub types: Vec<TypeDef>,
    pub funcs: Vec<FuncDef>,
    /// 割り当て済みの `ExprId` の数。
    pub expr_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// 型注釈。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExprKind {
    /// `int` / `bool` / `str` / `None` などの名前。
    Name(String),
    /// `A | B | ...`（2つ以上）。
    Union(Vec<TypeExpr>),
    /// `Pair[int, str]`（型引数の適用）。
    Apply(String, Vec<TypeExpr>),
}

/// 組み込み関数と構築子の名前。利用者は定義できず（E0209）、import の別名にもできない（E0704）。
pub const BUILTINS: &[&str] = &[
    "print",
    "exit",
    "panic",
    "arg_count",
    "arg",
    "parse_scalar",
    "parse_json",
    "validate",
    "to_json",
    "llm_generate",
    "llm_mock_push",
    "audit_events",
    "assert",
    "len",
    "range",
    "Ok",
    "Err",
    // async（ADR-0020 論点 4）
    "spawn",
    "sleep_ms",
    // 数値の変換（ADR-0019 論点 3）
    "float",
    "int",
    // Python（ADR-0018 論点 9）
    "py_import",
    "py_value",
];

/// `import a.b [as c]`（`names` が `None`）または `from a.b import f [as g], ...`。
#[derive(Debug, Clone)]
pub struct Import {
    pub module: Vec<Ident>,
    /// `import a.b as c` の `c`（ADR-0016 論点 19）。`from ... import` では使わない。
    pub alias: Option<Ident>,
    /// `from a.b import f, g as h` の名前。`import a.b` なら `None`。
    pub names: Option<Vec<ImportName>>,
    pub span: Span,
}

/// `from ... import` の名前1つ（`f` か `f as g`）。
#[derive(Debug, Clone)]
pub struct ImportName {
    pub name: Ident,
    pub alias: Option<Ident>,
}

impl ImportName {
    /// このモジュールで使う名前（別名があれば別名）。
    pub fn local(&self) -> &Ident {
        self.alias.as_ref().unwrap_or(&self.name)
    }
}

impl Import {
    /// `a.b` の形のモジュール名。
    pub fn module_name(&self) -> String {
        self.module
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }
}

/// トップレベルの `type Name = ...`。
#[derive(Debug, Clone)]
pub struct TypeDef {
    /// `pub type`（ほかのモジュールから使える、ADR-0010）
    pub public: bool,
    pub name: Ident,
    /// `type Pair[A, B] = ...` の型パラメータ。
    pub params: Vec<Ident>,
    pub value: TypeDefValue,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum TypeDefValue {
    /// `{x: int, y: int}`（構造的レコード、ADR-0007）
    Record(Vec<(Ident, TypeExpr)>),
    /// 既存の型の別名（`int | None` など）
    Alias(TypeExpr),
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: Ident,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone)]
pub struct FuncDef {
    /// `def` の前の `@name(...)`（ADR-0022 論点 11。M5 ではコンパイル時の属性で、`tool` / `agent` だけを解釈する）
    pub decorators: Vec<Decorator>,
    /// `pub def`（ほかのモジュールから使える、ADR-0010）
    pub public: bool,
    /// `async def`（ADR-0020）
    pub is_async: bool,
    pub name: Ident,
    /// `def f[T](...)` の型パラメータ。
    pub type_params: Vec<Ident>,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    /// `uses io, exit` 節（ADR-0014）。書かれていなければ `None`。
    pub effects: Option<EffectClause>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

/// デコレータ `@name` / `@name(key=value, ...)`（ADR-0022 論点 11）。引数の検査は型検査で行う。
#[derive(Debug, Clone)]
pub struct Decorator {
    pub name: Ident,
    pub args: Vec<AttrArg>,
    pub span: Span,
}

/// デコレータの引数。`key` が `None` なら位置引数（E1402）。
#[derive(Debug, Clone)]
pub struct AttrArg {
    pub key: Option<Ident>,
    pub value: AttrValue,
    pub span: Span,
}

/// デコレータの引数の値: リテラルか、文字列と名前の並び。
#[derive(Debug, Clone)]
pub enum AttrValue {
    Int(i64),
    Str(String),
    Bool(bool),
    List(Vec<AttrElem>),
}

#[derive(Debug, Clone)]
pub enum AttrElem {
    Str(String, Span),
    /// 関数の名前（`add`、`m.add`）。名前解決が内部名に直す
    Name(String, Span),
}

/// 関数の `uses` 節（ADR-0014）。名前の検査（未知の名前、重複）は型検査で行う。
#[derive(Debug, Clone)]
pub struct EffectClause {
    pub names: Vec<Ident>,
    /// `uses` から最後の名前の終わりまで。
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Assign {
        target: Ident,
        ann: Option<TypeExpr>,
        value: Expr,
    },
    Expr(Expr),
    Return(Option<Expr>),
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    /// `for var in iter:`（`iter` は `list[T]` か `range(...)`、M3 T4a）
    For {
        var: Ident,
        iter: Expr,
        body: Vec<Stmt>,
    },
    /// `xs[i] = value`（`target` は `ExprKind::Index`）
    IndexAssign {
        target: Expr,
        value: Expr,
    },
    /// `match subject:` と `case` の並び（ADR-0006）。
    Match {
        subject: Expr,
        arms: Vec<MatchArm>,
    },
    Pass,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternKind {
    /// `int()` / `bool()` / `str()`: 値がその型であることに一致する。
    Type(String),
    /// `None`
    None,
    /// `_`: 何にでも一致する。
    Wildcard,
    /// `Ok(x)` / `Err(e)` / `Ok(_)`: 構築子に一致し、中身を束縛する（ADR-0013）。`None` は `_`（束縛しない）。
    /// 束縛変数のスコープは `case` の本体だけ。
    Bind(String, Option<Ident>),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub id: ExprId,
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    /// 名前だけの式（`x`）なら、その名前。
    pub fn as_name(&self) -> Option<&str> {
        match &self.kind {
            ExprKind::Name(n) => Some(n),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    /// 浮動小数点のリテラル（ADR-0019）
    Float(f64),
    Bool(bool),
    Str(String),
    None,
    Name(String),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Call {
        callee: Ident,
        /// 呼び出しに明示した型引数 `f[T](x)`（ADR-0021 論点 9、M5 T4）。書いていなければ空
        type_args: Vec<TypeExpr>,
        args: Vec<Expr>,
        /// `name=value` のキーワード引数（レコードの作成で使う）。
        kwargs: Vec<(Ident, Expr)>,
    },
    /// `recv.method(args)`: メソッド呼び出し（M4 T4）。受け手は式（`f().attr(...)` も書ける。M4 T6）。
    /// パーサは `a.b(...)`（受け手が名前1つ）もこれにし、名前解決が、`a` がローカル変数でなくモジュール
    /// （別名を含む）なら修飾付きの `Call` に直す。`a.b.c(...)`（名前の並び）は、パーサが修飾付きの `Call` にする（ADR-0010）。
    MethodCall {
        receiver: Box<Expr>,
        method: Ident,
        /// `m.f[T](x)` の型引数。名前解決が修飾付きの `Call` に直すときに移す。本当のメソッドには書けない
        type_args: Vec<TypeExpr>,
        args: Vec<Expr>,
        /// 名前解決が `Call` に直すとき（`m.Tag(name=...)`）のために持つ。メソッドはキーワード引数を取らない
        kwargs: Vec<(Ident, Expr)>,
    },
    /// `[a, b, c]`（リストのリテラル、M3 T4a）
    List(Vec<Expr>),
    /// `{k: v, ...}`（辞書のリテラル、M3 T6a）
    Dict(Vec<(Expr, Expr)>),
    /// `base[index]`
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    /// `operand?`: `Err` なら関数からそのまま返し、`Ok` なら中身（ADR-0013）
    Try(Box<Expr>),
    /// `await operand`（ADR-0020）: `async def` の呼び出しか `Task[T]`。`await f()?` は `(await f())?`
    Await(Box<Expr>),
    /// `base.name`（レコードのフィールド参照）
    Field {
        base: Box<Expr>,
        name: Ident,
    },
    /// `lhs is rhs` / `lhs is not rhs`。型検査で `rhs` は `None` に限る。
    Is {
        lhs: Box<Expr>,
        negated: bool,
        rhs: Box<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    /// `k in d`（`dict` のキーの存在、ADR-0015）
    In,
    /// `k not in d`
    NotIn,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::FloorDiv => "//",
            BinOp::Mod => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::In => "in",
            BinOp::NotIn => "not in",
        }
    }
}
