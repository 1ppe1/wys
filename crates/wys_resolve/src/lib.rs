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
        // このモジュールで見える名前: 自分の定義 + `from ... import` した名前
        let mut scope: HashMap<String, String> = defs[mi]
            .iter()
            .map(|(k, d)| (k.clone(), d.internal.clone()))
            .collect();
        let mut modules: HashSet<String> = HashSet::new();
        for imp in &m.ast.imports {
            let target = imp.module_name();
            let Some(&ti) = index.get(&target) else {
                continue;
            };
            match &imp.names {
                None => {
                    modules.insert(target);
                }
                Some(names) => {
                    for n in names {
                        let Some(def) = lookup(&defs[ti], &target, &n.name, n.span, diags) else {
                            continue;
                        };
                        if let Some(local) = defs[mi].get(&n.name) {
                            diags.push(
                                Diagnostic::error(
                                    "E0704",
                                    format!("`{}` is imported from `{target}` but is also defined in this module", n.name),
                                    n.span,
                                )
                                .with_related(wys_diagnostics::RelatedKind::Function, &n.name, Some(local.span)),
                            );
                            continue;
                        }
                        scope.insert(n.name.clone(), def);
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
            for p in &mut f.params {
                r.type_expr(&mut p.ty);
            }
            r.type_expr(&mut f.ret);
            r.stmts(&mut f.body);
        }
        out.types.append(&mut m.ast.types);
        out.funcs.append(&mut m.ast.funcs);
    }
    out
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

struct Resolver<'a> {
    scope: &'a HashMap<String, String>,
    modules: &'a HashSet<String>,
    defs: &'a [HashMap<String, Def>],
    index: &'a HashMap<String, usize>,
    diags: &'a mut Vec<Diagnostic>,
    /// 型パラメータの名前（付け替えない）
    hidden: HashSet<String>,
}

impl Resolver<'_> {
    /// 名前を内部名にする。`m.f` は `import m` したモジュールの公開定義。見つからなければそのまま（型検査が報告する）。
    fn name(&mut self, name: &mut String, span: Span) {
        if self.hidden.contains(name.as_str()) {
            return;
        }
        if let Some((prefix, last)) = name.rsplit_once('.') {
            if self.modules.contains(prefix) {
                let ti = self.index[prefix];
                if let Some(internal) = lookup(&self.defs[ti], prefix, last, span, self.diags) {
                    *name = internal;
                }
            }
            return;
        }
        if let Some(internal) = self.scope.get(name.as_str()) {
            *name = internal.clone();
        }
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
        match &mut e.kind {
            ExprKind::Call {
                callee,
                args,
                kwargs,
            } => {
                let span = callee.span;
                self.name(&mut callee.name, span);
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
            ExprKind::Try(inner) => self.expr(inner),
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
