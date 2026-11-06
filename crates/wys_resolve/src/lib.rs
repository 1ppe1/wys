//! モジュールの読み込みと名前解決（ADR-0010）
//!
//! - 1ファイル = 1モジュール。`import a.b` はエントリファイルのディレクトリからの相対パス `a/b.wys` を読む。
//! - 各モジュールのトップレベルの定義を内部名に付け替える（`a/b.wys` の `f` は `a.b.f`）。
//!   エントリファイルの定義は元の名前のままにする（`main` を含む）。
//! - 参照（関数の呼び出し、型名、パターン）を内部名に解決し、全モジュールを1つの `Module` にまとめる。
//!   以降の型検査・コード生成は、まとめた `Module` だけを扱う。
//! - `pub` が付いていない定義は、ほかのモジュールから使えない（E0702）。循環 import は禁止（E0703）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use wys_ast::*;
use wys_diagnostics::Diagnostic;
use wys_span::{SourceFile, SourceMap, Span};

pub struct Loaded {
    pub module: Module,
    pub sources: SourceMap,
    pub diagnostics: Vec<Diagnostic>,
}

struct ModInfo {
    /// `a.b` の形の名前（エントリはファイル名の拡張子を除いたもの）
    name: String,
    is_entry: bool,
    ast: Module,
}

#[derive(Clone)]
struct Def {
    internal: String,
    public: bool,
    span: Span,
}

/// エントリファイルとその import を読み込み、名前を解決して1つのモジュールにまとめる。
/// `read` はファイルの内容を返す（見つからなければ `None`）。
pub fn load_program(
    entry_name: &str,
    entry_text: &str,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Loaded {
    let root = Path::new(entry_name)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut loader = Loader {
        root,
        read,
        sources: SourceMap::default(),
        diags: Vec::new(),
        mods: Vec::new(),
        index: HashMap::new(),
        next_expr: 0,
    };
    let stem = Path::new(entry_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main")
        .to_string();
    loader.add(
        stem.clone(),
        entry_name.to_string(),
        entry_text.to_string(),
        true,
    );
    loader.visit(0, &mut vec![stem]);
    let Loader {
        sources,
        mut diags,
        mods,
        next_expr,
        ..
    } = loader;
    if diags.iter().any(Diagnostic::is_error) {
        return Loaded {
            module: Module {
                imports: Vec::new(),
                types: Vec::new(),
                funcs: Vec::new(),
                expr_count: next_expr,
            },
            sources,
            diagnostics: diags,
        };
    }
    let module = resolve(mods, &mut diags, next_expr);
    Loaded {
        module,
        sources,
        diagnostics: diags,
    }
}

struct Loader<'r> {
    root: PathBuf,
    read: &'r dyn Fn(&Path) -> Option<String>,
    sources: SourceMap,
    diags: Vec<Diagnostic>,
    mods: Vec<ModInfo>,
    index: HashMap<String, usize>,
    next_expr: u32,
}

impl Loader<'_> {
    fn add(&mut self, name: String, file_name: String, text: String, is_entry: bool) -> usize {
        let base = self.sources.add(SourceFile::new(file_name, text.clone()));
        let (ast, diags) = wys_parser::parse_source_at(&text, base, self.next_expr);
        self.next_expr = ast.expr_count;
        self.diags.extend(diags);
        self.index.insert(name.clone(), self.mods.len());
        self.mods.push(ModInfo {
            name,
            is_entry,
            ast,
        });
        self.mods.len() - 1
    }

    /// 深さ優先で import をたどる。`stack` は現在たどっている経路（循環の検出用）。
    fn visit(&mut self, idx: usize, stack: &mut Vec<String>) {
        let imports = self.mods[idx].ast.imports.clone();
        for imp in imports {
            let name = imp.module_name();
            if let Some(pos) = stack.iter().position(|s| *s == name) {
                let mut cycle = stack[pos..].to_vec();
                cycle.push(name.clone());
                self.diags.push(
                    Diagnostic::error(
                        "E0703",
                        format!("circular import: {}", cycle.join(" -> ")),
                        imp.span,
                    )
                    .with_hint(
                        "move the shared definitions into a separate module that both can import",
                    ),
                );
                continue;
            }
            if self.index.contains_key(&name) {
                continue;
            }
            let mut path = self.root.clone();
            for part in &imp.module {
                path.push(&part.name);
            }
            path.set_extension("wys");
            let Some(text) = (self.read)(&path) else {
                let span = Span::new(
                    imp.module[0].span.start,
                    imp.module.last().expect("non-empty").span.end,
                );
                self.diags.push(
                    Diagnostic::error("E0701", format!("module `{name}` not found"), span)
                        .with_cause(format!("looked for `{}`", path.display())),
                );
                continue;
            };
            let child = self.add(name.clone(), path.display().to_string(), text, false);
            stack.push(name);
            self.visit(child, stack);
            stack.pop();
        }
    }
}

/// 名前の付け替えと参照の解決。
fn resolve(mut mods: Vec<ModInfo>, diags: &mut Vec<Diagnostic>, expr_count: u32) -> Module {
    // 各モジュールの定義（関数と型は同じ名前空間）
    let mut defs: Vec<HashMap<String, Def>> = Vec::new();
    for m in &mods {
        let mut d = HashMap::new();
        let internal = |n: &str| {
            if m.is_entry {
                n.to_string()
            } else {
                format!("{}.{n}", m.name)
            }
        };
        for f in &m.ast.funcs {
            d.entry(f.name.name.clone()).or_insert(Def {
                internal: internal(&f.name.name),
                public: f.public,
                span: f.name.span,
            });
        }
        for t in &m.ast.types {
            d.entry(t.name.name.clone()).or_insert(Def {
                internal: internal(&t.name.name),
                public: t.public,
                span: t.name.span,
            });
        }
        defs.push(d);
    }
    let index: HashMap<String, usize> = mods
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name.clone(), i))
        .collect();

    let mut out = Module {
        imports: Vec::new(),
        types: Vec::new(),
        funcs: Vec::new(),
        expr_count,
    };
    for (mi, m) in mods.iter_mut().enumerate() {
        let errors_before = diags.iter().filter(|d| d.is_error()).count();
        // このモジュールで見える名前: 自分の定義 + `from ... import` した名前
        let mut scope: HashMap<String, String> = defs[mi]
            .iter()
            .map(|(k, d)| (k.clone(), d.internal.clone()))
            .collect();
        // `import a.b` → `a.b.f`、`import a.b as c` → `c.f`（ADR-0016 論点 19）。修飾の接頭辞 → モジュール名
        let mut modules: HashMap<String, String> = HashMap::new();
        // import で導入した名前と、その場所（別の import との衝突を E0704 にする）
        let mut bound: HashMap<String, Bound> = HashMap::new();
        for imp in &m.ast.imports {
            let target = imp.module_name();
            let Some(&ti) = index.get(&target) else {
                continue;
            };
            match &imp.names {
                None => {
                    let (local, span) = match &imp.alias {
                        Some(a) => (a.name.clone(), a.span),
                        None => (target.clone(), imp.span),
                    };
                    let b = Bound {
                        span,
                        module: target.clone(),
                        identity: format!("module {target}"),
                    };
                    let own = if imp.alias.is_some() {
                        &defs[mi]
                    } else {
                        &HashMap::new()
                    };
                    if !import_conflict(&local, &b, own, &bound, diags) {
                        continue;
                    }
                    bound.insert(local.clone(), b);
                    modules.insert(local, target);
                }
                Some(names) => {
                    for n in names {
                        let Some(def) =
                            lookup(&defs[ti], &target, &n.name.name, n.name.span, diags)
                        else {
                            continue;
                        };
                        let local = n.local();
                        let b = Bound {
                            span: local.span,
                            module: target.clone(),
                            identity: def.clone(),
                        };
                        if !import_conflict(&local.name, &b, &defs[mi], &bound, diags) {
                            continue;
                        }
                        bound.insert(local.name.clone(), b);
                        scope.insert(local.name.clone(), def);
                    }
                }
            }
        }
        let mut r = Resolver {
            scope: &scope,
            modules: &modules,
            defs: &defs,
            index: &index,
            diags,
            hidden: HashSet::new(),
            own_name: &m.name,
            own: &defs[mi],
            locals: HashMap::new(),
            used: HashSet::new(),
        };
        for t in &mut m.ast.types {
            t.name.name = defs[mi][&t.name.name].internal.clone();
            r.hidden = t.params.iter().map(|p| p.name.clone()).collect();
            match &mut t.value {
                TypeDefValue::Alias(te) => r.type_expr(te),
                TypeDefValue::Record(fields) => {
                    for (_, te) in fields {
                        r.type_expr(te);
                    }
                }
            }
        }
        for f in &mut m.ast.funcs {
            f.name.name = defs[mi][&f.name.name].internal.clone();
            r.hidden = f.type_params.iter().map(|p| p.name.clone()).collect();
            r.locals = f.params.iter().map(|p| (p.name.name.clone(), 0)).collect();
            collect_locals(&f.body, &mut r.locals);
            for p in &mut f.params {
                r.type_expr(&mut p.ty);
            }
            r.type_expr(&mut f.ret);
            r.stmts(&mut f.body);
        }
        // 使われない import（W0202、ADR-0016 論点 21）。書いた順に報告する
        // 名前解決のエラーがあるモジュールでは出さない（`lib.f` の E0706 なら、別名は使うつもりだった）
        let clean = r.diags.iter().filter(|d| d.is_error()).count() == errors_before;
        let mut unused: Vec<(&String, &Bound)> = bound
            .iter()
            .filter(|(local, _)| clean && !r.used.contains(local.as_str()))
            .collect();
        unused.sort_by_key(|(_, b)| b.span.start);
        for (local, b) in unused {
            r.diags.push(
                Diagnostic::warning(
                    "W0202",
                    format!("`{local}` is imported but never used"),
                    b.span,
                )
                .with_hint("remove the import"),
            );
        }
        out.types.append(&mut m.ast.types);
        out.funcs.append(&mut m.ast.funcs);
    }
    out
}

/// import で導入する名前 `local` が、このモジュールの定義か、先に import した名前と衝突していれば E0704 を報告して
/// `false` を返す（ADR-0010、ADR-0016 論点 19）。同じモジュールの同じ名前を二度 import するのは衝突ではない。
fn import_conflict(
    local: &str,
    new: &Bound,
    own: &HashMap<String, Def>,
    bound: &HashMap<String, Bound>,
    diags: &mut Vec<Diagnostic>,
) -> bool {
    let (span, target) = (new.span, new.module.as_str());
    if BUILTINS.contains(&local) {
        diags.push(
            Diagnostic::error(
                "E0704",
                format!("`{local}` is a built-in function and cannot be used as an import name"),
                span,
            )
            .with_hint("choose another name with `as`"),
        );
        return false;
    }
    if let Some(def) = own.get(local) {
        diags.push(
            Diagnostic::error(
                "E0704",
                format!("`{local}` is imported from `{target}` but is also defined in this module"),
                span,
            )
            .with_hint("rename it with `as`, e.g. `from m import f as m_f`")
            .with_related(
                wys_diagnostics::RelatedKind::Function,
                local,
                Some(def.span),
            ),
        );
        return false;
    }
    if let Some(prev) = bound.get(local)
        && prev.identity != new.identity
    {
        diags.push(
            Diagnostic::error(
                "E0704",
                format!(
                    "`{local}` refers to {} but was already imported as {}",
                    new.describe(),
                    prev.describe()
                ),
                span,
            )
            .with_hint("rename one of them with `as`")
            .with_related(
                wys_diagnostics::RelatedKind::Function,
                local,
                Some(prev.span),
            ),
        );
        return false;
    }
    true
}

/// import で導入した名前（衝突の検査用）。
struct Bound {
    span: Span,
    module: String,
    /// 指すもの（モジュールなら `module a.b`、定義なら内部名）。同じものを指す import は衝突しない。
    identity: String,
}

impl Bound {
    fn describe(&self) -> String {
        match self.identity.strip_prefix("module ") {
            Some(m) => format!("module `{m}`"),
            None => format!(
                "`{}` from `{}`",
                self.identity.rsplit('.').next().unwrap_or_default(),
                self.module
            ),
        }
    }
}

/// モジュール `target` の定義 `name` を探す。なければ E0705、公開されていなければ E0702。
fn lookup(
    defs: &HashMap<String, Def>,
    target: &str,
    name: &str,
    span: Span,
    diags: &mut Vec<Diagnostic>,
) -> Option<String> {
    let Some(def) = defs.get(name) else {
        diags.push(Diagnostic::error(
            "E0705",
            format!("module `{target}` has no definition named `{name}`"),
            span,
        ));
        return None;
    };
    if !def.public {
        diags.push(
            Diagnostic::error(
                "E0702",
                format!("`{name}` is private to module `{target}`"),
                span,
            )
            .with_hint(format!(
                "add `pub` to the definition of `{name}` in `{target}`"
            ))
            .with_related(wys_diagnostics::RelatedKind::Function, name, Some(def.span)),
        );
        return None;
    }
    Some(def.internal.clone())
}

/// 名前の並びの式（`a`、`a.b.c`）を `a.b.c` の文字列にする。それ以外は `None`。
fn dotted_path(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Name(n) => Some(n.clone()),
        ExprKind::Field { base, name } => dotted_path(base).map(|p| format!("{p}.{}", name.name)),
        _ => None,
    }
}

/// 関数の本体で束縛する名前（代入先、`for` の変数、`case Ok(x)` の束縛）と、最初に束縛する位置。
/// スコープは区別しない。型検査はローカル変数を出てきた順に登録するので、それより前の使用はモジュールの参照になる。
fn collect_locals(stmts: &[Stmt], out: &mut HashMap<String, u32>) {
    let bind = |id: &Ident, out: &mut HashMap<String, u32>| {
        let at = out.entry(id.name.clone()).or_insert(id.span.start);
        *at = (*at).min(id.span.start);
    };
    for s in stmts {
        match &s.kind {
            StmtKind::Assign { target, .. } => bind(target, out),
            StmtKind::For { var, body, .. } => {
                bind(var, out);
                collect_locals(body, out);
            }
            StmtKind::If {
                branches,
                else_body,
            } => {
                for (_, b) in branches {
                    collect_locals(b, out);
                }
                if let Some(b) = else_body {
                    collect_locals(b, out);
                }
            }
            StmtKind::While { body, .. } => collect_locals(body, out),
            StmtKind::Match { arms, .. } => {
                for a in arms {
                    if let PatternKind::Bind(_, Some(v)) = &a.pattern.kind {
                        bind(v, out);
                    }
                    collect_locals(&a.body, out);
                }
            }
            StmtKind::Expr(_)
            | StmtKind::Return(_)
            | StmtKind::IndexAssign { .. }
            | StmtKind::Pass => {}
        }
    }
}

struct Resolver<'a> {
    scope: &'a HashMap<String, String>,
    modules: &'a HashMap<String, String>,
    defs: &'a [HashMap<String, Def>],
    index: &'a HashMap<String, usize>,
    diags: &'a mut Vec<Diagnostic>,
    /// 型パラメータの名前（付け替えない）
    hidden: HashSet<String>,
    /// このモジュールの名前と定義（自分自身への修飾参照 `lib.f` を許す）
    own_name: &'a str,
    own: &'a HashMap<String, Def>,
    /// いま解決している関数のローカル変数と引数、最初に束縛する位置（引数は 0）。
    /// `xs.append(1)` はメソッド呼び出しで、モジュールの参照ではない
    locals: HashMap<String, u32>,
    /// 参照した import の名前（使われない import の警告 W0202 用）
    used: HashSet<String>,
}

impl Resolver<'_> {
    /// 名前を内部名にする。`m.f` は `import m` したモジュールの公開定義。見つからなければそのまま（型検査が報告する）。
    fn name(&mut self, name: &mut String, span: Span) {
        if self.hidden.contains(name.as_str()) {
            return;
        }
        if let Some((prefix, last)) = name.rsplit_once('.') {
            if prefix == self.own_name {
                if let Some(d) = self.own.get(last) {
                    *name = d.internal.clone();
                }
                return;
            }
            if let Some(target) = self.modules.get(prefix) {
                self.used.insert(prefix.to_string());
                let ti = self.index[target.as_str()];
                if let Some(internal) = lookup(&self.defs[ti], target, last, span, self.diags) {
                    *name = internal;
                }
            } else if self.index.contains_key(prefix) {
                // 読み込まれたモジュールでも、このモジュールで import していなければ使えない（E0706）。
                // 内部名と同じ綴りなので、ここで止めないと `pub` の検査（E0702）も素通りする
                let mut aliases: Vec<&String> = self
                    .modules
                    .iter()
                    .filter(|(_, t)| t.as_str() == prefix)
                    .map(|(a, _)| a)
                    .collect();
                aliases.sort();
                let alias = aliases.first().map(|a| a.to_string());
                let d = Diagnostic::error(
                    "E0706",
                    format!("module `{prefix}` is not imported in this module"),
                    span,
                );
                self.diags.push(match alias {
                    Some(a) => d.with_hint(format!("it is imported as `{a}`: write `{a}.{last}`")),
                    None => d.with_hint(format!("add `import {prefix}` at the top of this file")),
                });
            } else if let Some(full) = self.longer_module(prefix) {
                // `import a.b` の後の `a.f()`: `a` はモジュールの経路の途中で、モジュールとして import していない
                self.diags.push(
                    Diagnostic::error(
                        "E0706",
                        format!("module `{prefix}` is not imported in this module"),
                        span,
                    )
                    .with_hint(format!("`{full}` is imported: write `{full}.{last}`")),
                );
            }
            return;
        }
        if let Some(internal) = self.scope.get(name.as_str()) {
            self.used.insert(name.clone());
            *name = internal.clone();
        }
    }

    /// `prefix.` で始まる、import したモジュールの修飾の接頭辞（`import a.b` なら `a` に対する `a.b`）。決定的にするため名前の順で最初のもの。
    fn longer_module(&self, prefix: &str) -> Option<String> {
        let start = format!("{prefix}.");
        self.modules
            .keys()
            .filter(|k| k.starts_with(&start))
            .min()
            .cloned()
    }

    fn type_expr(&mut self, t: &mut TypeExpr) {
        let span = t.span;
        match &mut t.kind {
            TypeExprKind::Name(n) => self.name(n, span),
            TypeExprKind::Apply(n, args) => {
                self.name(n, span);
                for a in args {
                    self.type_expr(a);
                }
            }
            TypeExprKind::Union(items) => {
                for i in items {
                    self.type_expr(i);
                }
            }
        }
    }

    fn stmts(&mut self, stmts: &mut [Stmt]) {
        for s in stmts {
            match &mut s.kind {
                StmtKind::Assign { ann, value, .. } => {
                    if let Some(a) = ann {
                        self.type_expr(a);
                    }
                    self.expr(value);
                }
                StmtKind::Expr(e) => self.expr(e),
                StmtKind::Return(v) => {
                    if let Some(e) = v {
                        self.expr(e);
                    }
                }
                StmtKind::If {
                    branches,
                    else_body,
                } => {
                    for (c, b) in branches {
                        self.expr(c);
                        self.stmts(b);
                    }
                    if let Some(b) = else_body {
                        self.stmts(b);
                    }
                }
                StmtKind::While { cond, body } => {
                    self.expr(cond);
                    self.stmts(body);
                }
                StmtKind::For { iter, body, .. } => {
                    self.expr(iter);
                    self.stmts(body);
                }
                StmtKind::IndexAssign { target, value } => {
                    self.expr(target);
                    self.expr(value);
                }
                StmtKind::Match { subject, arms } => {
                    self.expr(subject);
                    for a in arms {
                        if let PatternKind::Type(n) = &mut a.pattern.kind {
                            let span = a.pattern.span;
                            self.name(n, span);
                        }
                        self.stmts(&mut a.body);
                    }
                }
                StmtKind::Pass => {}
            }
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        // `m.f(...)` / `a.b.f(...)`: 受け手が名前の並びで、その位置で見えるローカル変数で始まらず、
        // モジュール（別名、自分自身、読み込まれたモジュール）か、どのローカル変数でもない名前で始まるなら、
        // メソッド呼び出しではなく修飾付きの呼び出し（M4 T4・T6）。ローカル変数が同じ名前のモジュールを隠す。
        // フィールドの値の受け手（`box.handle.to_int()`）はメソッド呼び出しのまま
        if let ExprKind::MethodCall { receiver, .. } = &e.kind
            && let Some(path) = dotted_path(receiver)
            && let first = path.split('.').next().unwrap_or_default()
            && !self
                .locals
                .get(first)
                .is_some_and(|&at| at < receiver.span.start)
            && (self.modules.contains_key(&path)
                || path == self.own_name
                || self.index.contains_key(&path)
                || self.longer_module(&path).is_some()
                || !self.locals.contains_key(first))
        {
            let ExprKind::MethodCall {
                receiver,
                method,
                type_args,
                args,
                kwargs,
            } = std::mem::replace(&mut e.kind, ExprKind::None)
            else {
                unreachable!("checked above")
            };
            e.kind = ExprKind::Call {
                callee: Ident {
                    name: format!(
                        "{}.{}",
                        dotted_path(&receiver).expect("checked above"),
                        method.name
                    ),
                    span: Span::new(receiver.span.start, method.span.end),
                },
                type_args,
                args,
                kwargs,
            };
        }
        match &mut e.kind {
            ExprKind::MethodCall {
                receiver,
                method,
                type_args,
                args,
                kwargs,
            } => {
                // 組み込みのメソッドに Generic なものはない（ADR-0021 論点 9）
                if let Some(t) = type_args.first() {
                    self.diags.push(Diagnostic::error(
                        "E0101",
                        format!(
                            "type arguments cannot be given to the method `{}`",
                            method.name
                        ),
                        t.span,
                    ));
                }
                self.expr(receiver);
                for a in args {
                    self.expr(a);
                }
                for (_, v) in kwargs {
                    self.expr(v);
                }
            }
            ExprKind::Call {
                callee,
                type_args,
                args,
                kwargs,
            } => {
                let span = callee.span;
                self.name(&mut callee.name, span);
                for t in type_args {
                    self.type_expr(t);
                }
                for a in args {
                    self.expr(a);
                }
                for (_, v) in kwargs {
                    self.expr(v);
                }
            }
            ExprKind::Unary { operand, .. } => self.expr(operand),
            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Is { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Field { base, .. } => self.expr(base),
            ExprKind::Try(inner) | ExprKind::Await(inner) => self.expr(inner),
            ExprKind::List(items) => {
                for i in items {
                    self.expr(i);
                }
            }
            ExprKind::Dict(entries) => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            ExprKind::Index { base, index } => {
                self.expr(base);
                self.expr(index);
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::None
            | ExprKind::Name(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(files: &[(&str, &str)]) -> Loaded {
        let map: HashMap<PathBuf, String> = files
            .iter()
            .map(|(p, s)| (PathBuf::from(p), s.to_string()))
            .collect();
        let read = |p: &Path| map.get(p).cloned();
        load_program(files[0].0, files[0].1, &read)
    }

    fn codes(l: &Loaded) -> Vec<&'static str> {
        l.diagnostics.iter().map(|d| d.code).collect()
    }

    #[test]
    fn resolves_imports_to_internal_names() {
        let l = load(&[
            (
                "app/main.wys",
                "import util.text\nfrom shapes import area\n\ndef main() -> None:\n    print(area(util.text.width(2)))\n",
            ),
            (
                "app/util/text.wys",
                "pub def width(n: int) -> int:\n    return helper(n)\n\ndef helper(n: int) -> int:\n    return n\n",
            ),
            (
                "app/shapes.wys",
                "pub def area(n: int) -> int:\n    return n * n\n",
            ),
        ]);
        assert!(l.diagnostics.is_empty(), "{:?}", l.diagnostics);
        let names: Vec<_> = l
            .module
            .funcs
            .iter()
            .map(|f| f.name.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["main", "util.text.width", "util.text.helper", "shapes.area"]
        );
        let StmtKind::Expr(Expr {
            kind: ExprKind::Call { args, .. },
            ..
        }) = &l.module.funcs[0].body[0].kind
        else {
            panic!()
        };
        let ExprKind::Call {
            callee,
            args: inner,
            ..
        } = &args[0].kind
        else {
            panic!()
        };
        assert_eq!(callee.name, "shapes.area");
        let ExprKind::Call { callee, .. } = &inner[0].kind else {
            panic!()
        };
        assert_eq!(callee.name, "util.text.width");
        // モジュール内の非公開関数への参照も付け替わる
        let StmtKind::Return(Some(Expr {
            kind: ExprKind::Call { callee, .. },
            ..
        })) = &l.module.funcs[1].body[0].kind
        else {
            panic!()
        };
        assert_eq!(callee.name, "util.text.helper");
    }

    #[test]
    fn module_errors() {
        assert_eq!(
            codes(&load(&[("main.wys", "import missing\n")])),
            vec!["E0701"]
        );
        assert_eq!(
            codes(&load(&[
                ("main.wys", "from m import secret\n"),
                ("m.wys", "def secret() -> None:\n    pass\n")
            ])),
            vec!["E0702"]
        );
        assert_eq!(
            codes(&load(&[
                ("main.wys", "import a\n"),
                ("a.wys", "import b\n"),
                ("b.wys", "import a\n")
            ])),
            vec!["E0703"]
        );
        assert_eq!(
            codes(&load(&[
                (
                    "main.wys",
                    "from m import f\n\ndef f() -> None:\n    pass\n"
                ),
                ("m.wys", "pub def f() -> None:\n    pass\n")
            ])),
            vec!["E0704"]
        );
        assert_eq!(
            codes(&load(&[
                ("main.wys", "from m import g\n"),
                ("m.wys", "pub def f() -> None:\n    pass\n")
            ])),
            vec!["E0705"]
        );
        let l = load(&[
            ("main.wys", "import a\n"),
            ("a.wys", "import b\n"),
            ("b.wys", "import a\n"),
        ]);
        assert!(
            l.diagnostics[0].message.contains("a -> b -> a"),
            "{}",
            l.diagnostics[0].message
        );
    }

    #[test]
    fn diagnostics_point_into_the_right_file() {
        let l = load(&[
            ("main.wys", "import m\n"),
            ("m.wys", "def f() -> None:\n    x = 1 $\n"),
        ]);
        assert_eq!(codes(&l), vec!["E0001"]);
        let span = l.diagnostics[0].span;
        assert_eq!(l.sources.file_name(span.start), "m.wys");
        assert_eq!(l.sources.position(span.start).line, 2);
    }

    #[test]
    fn type_parameters_are_not_renamed() {
        let l = load(&[
            (
                "main.wys",
                "from m import id\n\ndef main() -> None:\n    print(id(1))\n",
            ),
            (
                "m.wys",
                "type T = {x: int}\n\npub def id[T](x: T) -> T:\n    return x\n",
            ),
        ]);
        assert!(l.diagnostics.is_empty());
        let f = l
            .module
            .funcs
            .iter()
            .find(|f| f.name.name == "m.id")
            .unwrap();
        assert!(matches!(&f.params[0].ty.kind, TypeExprKind::Name(n) if n == "T"));
    }
}
