//! PoC2: `T | None` の絞り込み（narrowing）と `match` の網羅性検査の実験。
//!
//! 本物の AST ではなく、検証したい制御フローだけを持つ小さな文の言語で試す。
//! 確かめたいこと:
//! 1. `if x is None: return` の後で `x` が `T` に絞り込まれること（早期 return の合流）
//! 2. 分岐の合流で型が和集合に戻ること
//! 3. 再代入で絞り込みが解除されること
//! 4. Union 型に対する `match` の網羅性検査と、到達しない分岐の検出
//! 5. 構造的なレコード型と名目的な型が同じ Union に入るときの扱い

use std::collections::{BTreeMap, BTreeSet};

/// Union の構成要素。`Record` は構造的（フィールド集合で同一性を判定）、`Nominal` は名目的（名前で判定）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Base {
    Int,
    Str,
    Bool,
    None,
    /// 構造的レコード型。フィールド名と型の組（名前順）。
    Record(Vec<(String, Base)>),
    /// 名目的な型（`class` / `enum` 相当）。
    Nominal(String),
}

/// Union 型（構成要素の集合）。要素1つなら通常の型。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Union(pub BTreeSet<Base>);

impl Union {
    pub fn of(items: impl IntoIterator<Item = Base>) -> Self {
        Union(items.into_iter().collect())
    }

    pub fn join(&self, other: &Union) -> Union {
        Union(self.0.union(&other.0).cloned().collect())
    }

    pub fn without(&self, b: &Base) -> Union {
        Union(self.0.iter().filter(|x| *x != b).cloned().collect())
    }

    /// `self` の値がすべて `required` に含まれるか（代入・引数の互換性）。
    pub fn fits(&self, required: &Union) -> bool {
        self.0
            .iter()
            .all(|b| required.0.iter().any(|r| subtype(b, r)))
    }
}

/// 構造的部分型: レコードは、要求されるフィールドをすべて同じ型で持てば互換（幅部分型）。
/// 名目的な型は名前が同じときだけ互換。
fn subtype(a: &Base, b: &Base) -> bool {
    match (a, b) {
        (Base::Record(fa), Base::Record(fb)) => fb.iter().all(|f| fa.contains(f)),
        _ => a == b,
    }
}

/// 実験用の文。
#[derive(Debug, Clone)]
pub enum Stmt {
    Assign {
        var: String,
        ty: Union,
    },
    /// `var` を `required` 型の値として使う（関数呼び出しの引数など）。
    Use {
        var: String,
        required: Union,
    },
    IfIsNone {
        var: String,
        negated: bool,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    },
    Return,
}

pub type Env = BTreeMap<String, Union>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseError {
    pub var: String,
    pub actual: Union,
}

/// 文の列を流れ解析する。戻り値は `(到達する場合の環境, エラー)`。`None` の環境は、必ず return すること。
pub fn check(stmts: &[Stmt], env: Env) -> (Option<Env>, Vec<UseError>) {
    let mut errors = Vec::new();
    let out = block(stmts, env, &mut errors);
    (out, errors)
}

fn block(stmts: &[Stmt], mut env: Env, errors: &mut Vec<UseError>) -> Option<Env> {
    for s in stmts {
        env = stmt(s, env, errors)?;
    }
    Some(env)
}

fn stmt(s: &Stmt, mut env: Env, errors: &mut Vec<UseError>) -> Option<Env> {
    match s {
        Stmt::Assign { var, ty } => {
            env.insert(var.clone(), ty.clone());
            Some(env)
        }
        Stmt::Use { var, required } => {
            let actual = env.get(var).cloned().unwrap_or_default();
            if !actual.fits(required) {
                errors.push(UseError {
                    var: var.clone(),
                    actual,
                });
            }
            Some(env)
        }
        Stmt::Return => None,
        Stmt::IfIsNone {
            var,
            negated,
            then_body,
            else_body,
        } => {
            let cur = env.get(var).cloned().unwrap_or_default();
            let none_part = Union::of(cur.0.contains(&Base::None).then_some(Base::None));
            let some_part = cur.without(&Base::None);
            let (then_ty, else_ty) = if *negated {
                (some_part, none_part)
            } else {
                (none_part, some_part)
            };
            let mut then_env = env.clone();
            then_env.insert(var.clone(), then_ty);
            let mut else_env = env;
            else_env.insert(var.clone(), else_ty);
            let a = block(then_body, then_env, errors);
            let b = block(else_body, else_env, errors);
            match (a, b) {
                (None, None) => None,
                (Some(e), None) | (None, Some(e)) => Some(e),
                (Some(x), Some(y)) => Some(join_env(&x, &y)),
            }
        }
    }
}

fn join_env(a: &Env, b: &Env) -> Env {
    let mut out = a.clone();
    for (k, v) in b {
        out.entry(k.clone())
            .and_modify(|u| *u = u.join(v))
            .or_insert_with(|| v.clone());
    }
    out
}

/// `match` の分岐パターン。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    Is(Base),
    Wildcard,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MatchReport {
    /// どの分岐にも当てはまらない構成要素。
    pub missing: Vec<Base>,
    /// それより前の分岐で網羅済みのため、到達しない分岐の番号。
    pub unreachable_arms: Vec<usize>,
}

pub fn check_match(scrutinee: &Union, arms: &[Pattern]) -> MatchReport {
    let mut remaining: BTreeSet<Base> = scrutinee.0.clone();
    let mut report = MatchReport::default();
    for (i, arm) in arms.iter().enumerate() {
        let covered: Vec<Base> = match arm {
            Pattern::Wildcard => remaining.iter().cloned().collect(),
            Pattern::Is(b) => remaining
                .iter()
                .filter(|r| subtype(r, b))
                .cloned()
                .collect(),
        };
        if covered.is_empty() {
            report.unreachable_arms.push(i);
        }
        for c in covered {
            remaining.remove(&c);
        }
    }
    report.missing = remaining.into_iter().collect();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt_str() -> Union {
        Union::of([Base::Str, Base::None])
    }
    fn str_only() -> Union {
        Union::of([Base::Str])
    }
    fn env(var: &str, ty: Union) -> Env {
        Env::from([(var.to_string(), ty)])
    }
    fn use_str(var: &str) -> Stmt {
        Stmt::Use {
            var: var.into(),
            required: str_only(),
        }
    }

    #[test]
    fn unchecked_optional_use_is_rejected() {
        // AC-002: None の可能性がある値を未検証で使うと拒否される
        let (_, errs) = check(&[use_str("x")], env("x", opt_str()));
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn early_return_narrows_the_rest_of_the_block() {
        let prog = [
            Stmt::IfIsNone {
                var: "x".into(),
                negated: false,
                then_body: vec![Stmt::Return],
                else_body: vec![],
            },
            use_str("x"),
        ];
        let (_, errs) = check(&prog, env("x", opt_str()));
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn narrowing_inside_is_not_none_branch() {
        let prog = [Stmt::IfIsNone {
            var: "x".into(),
            negated: true,
            then_body: vec![use_str("x")],
            else_body: vec![],
        }];
        assert!(check(&prog, env("x", opt_str())).1.is_empty());
    }

    #[test]
    fn join_restores_the_union_after_branches() {
        let prog = [
            Stmt::IfIsNone {
                var: "x".into(),
                negated: true,
                then_body: vec![use_str("x")],
                else_body: vec![],
            },
            use_str("x"),
        ];
        let (out, errs) = check(&prog, env("x", opt_str()));
        assert_eq!(errs.len(), 1);
        assert_eq!(out.unwrap()["x"], opt_str());
    }

    #[test]
    fn reassignment_resets_narrowing() {
        let prog = [
            Stmt::IfIsNone {
                var: "x".into(),
                negated: false,
                then_body: vec![Stmt::Return],
                else_body: vec![],
            },
            Stmt::Assign {
                var: "x".into(),
                ty: opt_str(),
            },
            use_str("x"),
        ];
        assert_eq!(check(&prog, env("x", opt_str())).1.len(), 1);
    }

    #[test]
    fn all_paths_return() {
        let prog = [Stmt::IfIsNone {
            var: "x".into(),
            negated: false,
            then_body: vec![Stmt::Return],
            else_body: vec![Stmt::Return],
        }];
        assert_eq!(check(&prog, env("x", opt_str())).0, None);
    }

    #[test]
    fn match_exhaustiveness_and_unreachable_arms() {
        let u = Union::of([Base::Int, Base::Str, Base::None]);
        let r = check_match(&u, &[Pattern::Is(Base::Int), Pattern::Is(Base::Str)]);
        assert_eq!(r.missing, vec![Base::None]);
        let r = check_match(
            &u,
            &[
                Pattern::Is(Base::Int),
                Pattern::Wildcard,
                Pattern::Is(Base::Str),
            ],
        );
        assert!(r.missing.is_empty());
        assert_eq!(r.unreachable_arms, vec![2]);
    }

    #[test]
    fn structural_records_and_nominal_types() {
        let point = Base::Record(vec![("x".into(), Base::Int), ("y".into(), Base::Int)]);
        let has_x = Base::Record(vec![("x".into(), Base::Int)]);
        // 幅部分型: {x, y} は {x} を要求する場所で使える
        assert!(Union::of([point.clone()]).fits(&Union::of([has_x.clone()])));
        assert!(!Union::of([has_x.clone()]).fits(&Union::of([point.clone()])));
        // 名目的な型は形が同じでも名前が違えば互換でない
        let user = Base::Nominal("UserId".into());
        let order = Base::Nominal("OrderId".into());
        assert!(!Union::of([user.clone()]).fits(&Union::of([order.clone()])));
        // 構造的なパターンは、そのフィールドを持つすべてのレコードに当てはまる。
        // 2つのレコードが1つのパターンで網羅されるので、レコードを区別したいなら名目的な型が要る。
        let u = Union::of([point, has_x.clone(), user]);
        let r = check_match(&u, &[Pattern::Is(has_x)]);
        assert_eq!(r.missing, vec![Base::Nominal("UserId".into())]);
    }
}
