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

/// `import a.b`（`names` が `None`）または `from a.b import f, g`。
#[derive(Debug, Clone)]
pub struct Import {
    pub module: Vec<Ident>,
    pub names: Option<Vec<Ident>>,
    pub span: Span,
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
    /// `pub def`（ほかのモジュールから使える、ADR-0010）
    pub public: bool,
    pub name: Ident,
    /// `def f[T](...)` の型パラメータ。
    pub type_params: Vec<Ident>,
    pub params: Vec<Param>,
    pub ret: TypeExpr,
    pub body: Vec<Stmt>,
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

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
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
        args: Vec<Expr>,
        /// `name=value` のキーワード引数（レコードの作成で使う）。
        kwargs: Vec<(Ident, Expr)>,
    },
    /// `[a, b, c]`（リストのリテラル、M3 T4a）
    List(Vec<Expr>),
    /// `base[index]`
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
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
        }
    }
}
