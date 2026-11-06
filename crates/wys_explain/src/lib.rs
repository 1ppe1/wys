//! `wys explain`: プログラムの説明レポート（DX-004、ADR-0017）
//!
//! 静的な事実だけを出す。
//! - 関数ごとのシグネチャ、ローカル変数の宣言型、絞り込み（参照の地点の型が宣言型と異なるもの）
//! - ARC パスが挿入した retain / release と、その理由（IR の関数インスタンスごと）
//!
//! 実行時の回数は `WYS_RC_STATS`（M3 T7）の役割で、ここでは扱わない。
//! JSON の形は `docs/spec/explain-schema.json`（v0）に従う。v0 の間は項目の追加だけを許す。

use serde::Serialize;
use wys_driver::{Frontend, SourceMap};
use wys_ir::{Inst, LocalId, Program, RcReason, TempId};
use wys_span::Span;
use wys_typeck::TypeckResult;

/// JSON の `schema_version`。
pub const SCHEMA_VERSION: &str = "0";

const ERROR_CODES: &str = include_str!("../../../docs/spec/error-codes.md");

// ---- JSON の表現（スキーマと1対1に対応） ----

#[derive(Serialize)]
struct FileReport<'a> {
    schema_version: &'static str,
    kind: &'static str,
    file: &'a str,
    functions: Vec<FunctionJson<'a>>,
}

#[derive(Serialize)]
struct FunctionJson<'a> {
    name: &'a str,
    /// `async def`（ADR-0020 論点 19）
    #[serde(rename = "async")]
    is_async: bool,
    span: SpanJson<'a>,
    signature: SignatureJson<'a>,
    locals: Vec<LocalJson<'a>>,
    narrowing: Vec<NarrowingJson<'a>>,
    arc: Vec<InstanceJson<'a>>,
    effects: EffectsJson<'a>,
    /// `@tool(...)` の属性（ADR-0022 論点 16）。付いていなければ `null`
    tool: Option<ToolJson<'a>>,
    /// `@agent(...)` の属性（ADR-0022 論点 16）。付いていなければ `null`
    agent: Option<AgentJson<'a>>,
}

#[derive(Serialize)]
struct ToolJson<'a> {
    description: &'a str,
    /// `"none"` か `"required"`
    approval: &'static str,
}

#[derive(Serialize)]
struct AgentJson<'a> {
    tools: &'a [String],
    capabilities: Vec<&'static str>,
    model: &'a str,
    max_llm_calls: i64,
    max_tool_calls: i64,
    max_tokens: i64,
    timeout_ms: i64,
}

/// Effect（ADR-0014 論点 10）。推論ではない: `required` は組み込み関数と注釈のある関数の分だけ。
#[derive(Serialize)]
struct EffectsJson<'a> {
    /// `uses` 節で宣言した Effect。注釈がなければ `null`。
    declared: Option<Vec<&'static str>>,
    required: Vec<&'static str>,
    sites: Vec<EffectSiteJson<'a>>,
}

#[derive(Serialize)]
struct EffectSiteJson<'a> {
    callee: &'a str,
    span: SpanJson<'a>,
    /// 呼び出し先の Effect（注釈のない関数なら空で、`annotated` が偽）。
    effects: Vec<&'static str>,
    annotated: bool,
}

#[derive(Serialize)]
struct SignatureJson<'a> {
    type_params: Vec<String>,
    params: Vec<ParamJson<'a>>,
    #[serde(rename = "return")]
    ret: String,
}

#[derive(Serialize)]
struct ParamJson<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Serialize)]
struct LocalJson<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    ty: String,
    kind: &'static str,
    span: SpanJson<'a>,
}

#[derive(Serialize)]
struct NarrowingJson<'a> {
    variable: &'a str,
    span: SpanJson<'a>,
    declared: String,
    narrowed: String,
}

#[derive(Serialize)]
struct InstanceJson<'a> {
    instance: &'a str,
    retains: usize,
    releases: usize,
    ops: Vec<OpJson<'a>>,
    /// Move 最適化が取り除いた操作（M4 T3）。項目の追加なので版は上げない（ADR-0017 論点 1）。なければ出さない
    #[serde(skip_serializing_if = "Vec::is_empty")]
    elided: Vec<ElidedJson<'a>>,
}

#[derive(Serialize)]
struct ElidedJson<'a> {
    op: &'static str,
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    site: Option<&'static str>,
    block: u32,
    index: usize,
    temp: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    local: Option<&'a str>,
    elision: &'static str,
    message: &'static str,
}

#[derive(Serialize)]
struct OpJson<'a> {
    op: &'static str,
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    site: Option<&'static str>,
    block: usize,
    index: usize,
    temp: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    local: Option<&'a str>,
    message: String,
}

#[derive(Serialize)]
struct SpanJson<'a> {
    file: &'a str,
    start: PositionJson,
    end: PositionJson,
}

#[derive(Serialize)]
struct PositionJson {
    line: u32,
    column: u32,
    byte: u32,
}

fn span_json(map: &SourceMap, span: Span) -> SpanJson<'_> {
    let pos = |b| {
        let p = map.position(b);
        PositionJson {
            line: p.line,
            column: p.column,
            byte: p.byte,
        }
    };
    SpanJson {
        file: map.file_name(span.start),
        start: pos(span.start),
        end: pos(span.end),
    }
}

/// エラーのないフロントエンドの結果から、説明レポートの JSON を作る。
/// 型検査の結果か IR がなければ（エラーがある場合）`None`。
pub fn explain_file(fe: &Frontend) -> Option<String> {
    let tc = fe.typeck.as_ref()?;
    let program = fe.program.as_ref()?;
    let report = FileReport {
        schema_version: SCHEMA_VERSION,
        kind: "file",
        file: &fe.file.entry().name,
        functions: tc
            .sigs
            .iter()
            .enumerate()
            .map(|(i, _)| function_json(&fe.file, tc, program, i))
            .collect(),
    };
    Some(serde_json::to_string_pretty(&report).expect("report serializes"))
}

fn function_json<'a>(
    map: &'a SourceMap,
    tc: &'a TypeckResult,
    program: &'a Program,
    index: usize,
) -> FunctionJson<'a> {
    let sig = &tc.sigs[index];
    let info = &tc.funcs[index];
    let ty = |t| tc.types.name(t);
    let params = info
        .locals
        .iter()
        .zip(&sig.params)
        .map(|((name, _), t)| ParamJson { name, ty: ty(*t) })
        .collect();
    let locals = info
        .locals
        .iter()
        .zip(&info.local_spans)
        .enumerate()
        .map(|(i, ((name, t), span))| LocalJson {
            name,
            ty: ty(*t),
            kind: if i < sig.params.len() {
                "param"
            } else {
                "local"
            },
            span: span_json(map, *span),
        })
        .collect();
    let narrowing = info
        .narrowings
        .iter()
        .map(|n| NarrowingJson {
            variable: &n.name,
            span: span_json(map, n.span),
            declared: ty(n.declared),
            narrowed: ty(n.narrowed),
        })
        .collect();
    let arc = program
        .funcs
        .iter()
        .filter(|f| is_instance_of(&f.name, &sig.name, !sig.type_params.is_empty()))
        .map(|f| instance_json(f, &info.locals))
        .collect();
    let required = info
        .effect_sites
        .iter()
        .filter_map(|site| site.effects)
        .fold(wys_typeck::EffectSet::default(), |a, b| a.union(b));
    let effects = EffectsJson {
        declared: sig.effects.map(|e| e.names()),
        required: required.names(),
        sites: info
            .effect_sites
            .iter()
            .map(|site| EffectSiteJson {
                callee: &site.callee,
                span: span_json(map, site.span),
                effects: site.effects.map(|e| e.names()).unwrap_or_default(),
                annotated: site.effects.is_some(),
            })
            .collect(),
    };
    FunctionJson {
        name: &sig.name,
        is_async: sig.is_async,
        span: span_json(map, sig.span),
        signature: SignatureJson {
            type_params: sig
                .type_params
                .iter()
                .map(|p| ty(wys_typeck::Ty::Param(*p)))
                .collect(),
            params,
            ret: ty(sig.ret),
        },
        locals,
        narrowing,
        arc,
        effects,
        tool: sig.tool.as_ref().map(|t| ToolJson {
            description: &t.description,
            approval: if t.approval { "required" } else { "none" },
        }),
        agent: sig.agent.as_ref().map(|a| AgentJson {
            tools: &a.tools,
            capabilities: a.capabilities.names(),
            model: &a.model,
            max_llm_calls: a.max_llm_calls,
            max_tool_calls: a.max_tool_calls,
            max_tokens: a.max_tokens,
            timeout_ms: a.timeout_ms,
        }),
    }
}

/// IR の関数 `ir_name` が、ソースの関数 `name` のインスタンスか。Generic 関数のインスタンスは `name__N`。
/// Generic でない関数は名前の完全一致だけ（ユーザーが `f__1` という関数を定義していても `f` に数えない）。
fn is_instance_of(ir_name: &str, name: &str, generic: bool) -> bool {
    ir_name == name
        || generic
            && ir_name
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix("__"))
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

fn instance_json<'a>(
    f: &'a wys_ir::Function,
    locals: &'a [(String, wys_typeck::Ty)],
) -> InstanceJson<'a> {
    let mut ops = Vec::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        // スロットから読んだ一時値 → ローカル変数の名前（lowering が作った内部のスロットは名前を持たない）
        let mut loaded: std::collections::HashMap<TempId, LocalId> = Default::default();
        for (k, inst) in b.insts.iter().enumerate() {
            let (op, src, reason): (&'static str, TempId, RcReason) = match inst {
                Inst::Load { dst, src } | Inst::Take { dst, src } => {
                    loaded.insert(*dst, *src);
                    continue;
                }
                Inst::Retain { src, reason } => ("retain", *src, *reason),
                Inst::Release { src, reason } => ("release", *src, *reason),
                _ => continue,
            };
            let local = loaded
                .get(&src)
                .and_then(|l| locals.get(l.0 as usize))
                .map(|(name, _)| name.as_str());
            ops.push(OpJson {
                op,
                reason: reason.id(),
                site: reason.site().map(|s| s.id()),
                block: bi,
                index: k,
                temp: src.0,
                local,
                message: reason.message(),
            });
        }
    }
    let elided = f
        .elided_rc
        .iter()
        .map(|e| ElidedJson {
            op: if e.retain { "retain" } else { "release" },
            reason: e.reason.id(),
            site: e.reason.site().map(|s| s.id()),
            block: e.block,
            index: e.index,
            temp: e.src.0,
            local: e
                .local
                .and_then(|l| locals.get(l.0 as usize))
                .map(|(name, _)| name.as_str()),
            elision: e.elision.id(),
            message: e.elision.message(),
        })
        .collect();
    InstanceJson {
        instance: &f.name,
        retains: ops.iter().filter(|o| o.op == "retain").count(),
        releases: ops.iter().filter(|o| o.op == "release").count(),
        ops,
        elided,
    }
}

// ---- エラーコードの説明 ----

/// `docs/spec/error-codes.md` の表の1行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeEntry {
    pub code: String,
    pub stage: String,
    pub summary: String,
}

#[derive(Serialize)]
struct CodeReport<'a> {
    schema_version: &'static str,
    kind: &'static str,
    #[serde(flatten)]
    entry: &'a CodeEntry,
}

/// `docs/spec/error-codes.md` に載っているエラーコードの一覧（表の順。同じコードが複数の表にあれば最初の行）。
pub fn error_codes() -> Vec<CodeEntry> {
    let mut out: Vec<CodeEntry> = Vec::new();
    for line in ERROR_CODES.lines() {
        let Some(rest) = line.strip_prefix("| ") else {
            continue;
        };
        // セルの中の `\|` は区切りではない
        let cells: Vec<String> = rest
            .replace("\\|", "\u{0}")
            .trim_end()
            .trim_end_matches('|')
            .split('|')
            .map(|c| c.trim().replace('\u{0}', "|"))
            .collect();
        let [code, stage, summary, ..] = cells.as_slice() else {
            continue;
        };
        let is_code = code.len() == 5
            && (code.starts_with('E') || code.starts_with('W'))
            && code[1..].chars().all(|c| c.is_ascii_digit());
        if is_code && !out.iter().any(|e| &e.code == code) {
            out.push(CodeEntry {
                code: code.clone(),
                stage: stage.clone(),
                summary: summary.clone(),
            });
        }
    }
    out
}

/// エラーコードの説明。載っていなければ `None`。
pub fn explain_code(code: &str) -> Option<CodeEntry> {
    error_codes().into_iter().find(|e| e.code == code)
}

/// エラーコードの説明の JSON。
pub fn code_json(entry: &CodeEntry) -> String {
    serde_json::to_string_pretty(&CodeReport {
        schema_version: SCHEMA_VERSION,
        kind: "code",
        entry,
    })
    .expect("report serializes")
}

/// 引数がエラーコードの形（`E0201` / `W0301`）か。
pub fn looks_like_code(arg: &str) -> bool {
    arg.len() == 5
        && (arg.starts_with('E') || arg.starts_with('W'))
        && arg[1..].chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = include_str!("../../../docs/spec/explain-schema.json");

    fn validator() -> jsonschema::Validator {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).expect("schema is valid JSON");
        jsonschema::validator_for(&schema).expect("schema compiles")
    }

    fn report(src: &str) -> serde_json::Value {
        let fe = wys_driver::frontend("t.wys", src);
        assert!(!fe.has_errors(), "{:?}", fe.diagnostics);
        let json = explain_file(&fe).expect("no errors");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let errors: Vec<String> = validator().iter_errors(&v).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "schema violations: {errors:?}\n{json}");
        v
    }

    fn func<'a>(v: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
        v["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap()
    }

    #[test]
    fn reports_signatures_locals_and_narrowing() {
        let v = report(
            "def f(x: int | None) -> int:\n    y = 1\n    if x is not None:\n        return x + y\n    return y\n\ndef main() -> None:\n    print(f(2))\n",
        );
        assert_eq!(v["schema_version"], "0");
        assert_eq!(v["kind"], "file");
        let f = func(&v, "f");
        assert_eq!(f["signature"]["params"][0]["name"], "x");
        assert_eq!(f["signature"]["params"][0]["type"], "int | None");
        assert_eq!(f["signature"]["return"], "int");
        assert_eq!(f["locals"][0]["kind"], "param");
        assert_eq!(f["locals"][1]["name"], "y");
        assert_eq!(f["locals"][1]["kind"], "local");
        assert_eq!(f["locals"][1]["span"]["start"]["line"], 2);
        let n = f["narrowing"].as_array().unwrap();
        assert_eq!(n.len(), 1, "{n:?}");
        assert_eq!(n[0]["variable"], "x");
        assert_eq!(n[0]["declared"], "int | None");
        assert_eq!(n[0]["narrowed"], "int");
        assert_eq!(n[0]["span"]["start"]["line"], 4);
        // int だけの関数には ARC の操作がない
        assert_eq!(f["arc"][0]["instance"], "f");
        assert_eq!(f["arc"][0]["retains"], 0);
    }

    #[test]
    fn reports_arc_reasons_with_local_names() {
        let v = report(
            "def first(xs: list[str]) -> str:\n    return xs[0]\n\ndef main() -> None:\n    names = [\"a\", \"b\"]\n    print(first(names))\n",
        );
        let arc = &func(&v, "first")["arc"][0];
        let ops = arc["ops"].as_array().unwrap().clone();
        let reasons: Vec<&str> = ops.iter().map(|o| o["reason"].as_str().unwrap()).collect();
        // `xs[0]` は return で消費されるので、可変な場所からの借用の保持は残る
        assert!(
            reasons.contains(&"borrow_from_mutable_place"),
            "{reasons:?}"
        );
        // `xs` は代入されないので、入口と出口の組は Move 最適化が取り除く（M4 T3）
        let elided = arc["elided"].as_array().unwrap();
        let entry = elided
            .iter()
            .find(|o| o["reason"] == "param_slot_entry")
            .unwrap();
        assert_eq!(entry["local"], "xs");
        assert_eq!(entry["elision"], "param_never_reassigned");
        assert!(
            elided
                .iter()
                .any(|o| o["reason"] == "slot_at_function_exit")
        );
        let main = func(&v, "main");
        let store = main["arc"][0]["ops"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["reason"] == "store_overwrites_slot")
            .unwrap();
        assert_eq!(store["local"], "names");
    }

    #[test]
    fn generic_functions_have_one_entry_per_instance() {
        let v = report(
            "def id[T](x: T) -> T:\n    return x\n\ndef unused[T](x: T) -> T:\n    return x\n\ndef main() -> None:\n    print(id(1))\n    print(id(\"s\"))\n",
        );
        let id = func(&v, "id");
        assert_eq!(id["signature"]["type_params"][0], "T");
        assert_eq!(id["arc"].as_array().unwrap().len(), 2);
        assert_eq!(func(&v, "unused")["arc"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn reports_declared_and_required_effects() {
        let v = report(
            "def log(x: int) -> None uses io:\n    print(x)\n\ndef helper() -> None:\n    pass\n\ndef main() -> None:\n    log(1)\n    helper()\n    exit(0)\n",
        );
        let log = &func(&v, "log")["effects"];
        assert_eq!(log["declared"], serde_json::json!(["io"]));
        assert_eq!(log["required"], serde_json::json!(["io"]));
        let main = &func(&v, "main")["effects"];
        assert!(main["declared"].is_null());
        // 注釈のない関数の分は含まない（推論ではない）
        assert_eq!(main["required"], serde_json::json!(["io", "exit"]));
        let sites = main["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 3);
        assert_eq!(sites[1]["callee"], "helper");
        assert_eq!(sites[1]["annotated"], false);
        assert_eq!(sites[2]["effects"], serde_json::json!(["exit"]));
        assert_eq!(
            func(&v, "helper")["effects"]["required"],
            serde_json::json!([])
        );
    }

    #[test]
    fn instance_names_match_only_numeric_suffixes() {
        assert!(is_instance_of("f", "f", false));
        assert!(is_instance_of("f__3", "f", true));
        assert!(!is_instance_of("f__3", "f", false));
        assert!(!is_instance_of("f__x", "f", true));
        assert!(!is_instance_of("ff", "f", true));
    }

    #[test]
    fn error_codes_are_read_from_the_spec() {
        let codes = error_codes();
        assert!(codes.len() > 10, "{codes:?}");
        let e = explain_code("E0201").unwrap();
        assert_eq!(e.stage, "型");
        assert!(e.summary.contains("型が一致しない"), "{e:?}");
        // セルの中の `\|` は区切りとして扱わない
        let e = explain_code("E0215").unwrap();
        assert!(e.summary.contains("int | None"), "{e:?}");
        assert!(explain_code("E9999").is_none());
        let v: serde_json::Value =
            serde_json::from_str(&code_json(&explain_code("E0201").unwrap())).unwrap();
        assert!(validator().is_valid(&v), "{v}");
        assert_eq!(v["kind"], "code");
        assert_eq!(v["code"], "E0201");
    }

    #[test]
    fn codes_are_recognized_by_shape() {
        assert!(looks_like_code("E0201"));
        assert!(looks_like_code("W0301"));
        assert!(!looks_like_code("e0201"));
        assert!(!looks_like_code("main.wys"));
    }
}
