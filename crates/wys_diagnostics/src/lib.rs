//! 構造化診断（DX-002）: Diagnostic、エラーコード、JSON/テキスト出力
//!
//! JSON の形は `docs/spec/diagnostics-schema.json`（v0）に従う。

use serde::Serialize;
use wys_span::{SourceFile, Span};

/// 診断 JSON のスキーマバージョン。スキーマを変えたら更新する。
pub const SCHEMA_VERSION: &str = "0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RelatedKind {
    Type,
    Function,
    Variable,
    Module,
}

/// コンパイラ内部で扱う診断。位置はバイト範囲で持ち、出力時に行・列へ変換する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    pub span: Span,
    pub cause: Option<String>,
    pub hints: Vec<Hint>,
    pub related: Vec<Related>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub message: String,
    pub replacement: Option<(Span, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Related {
    pub kind: RelatedKind,
    pub name: String,
    pub span: Option<Span>,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            span,
            cause: None,
            hints: Vec::new(),
            related: Vec::new(),
        }
    }

    pub fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }

    pub fn with_hint(mut self, message: impl Into<String>) -> Self {
        self.hints.push(Hint {
            message: message.into(),
            replacement: None,
        });
        self
    }

    pub fn with_fix(
        mut self,
        message: impl Into<String>,
        span: Span,
        text: impl Into<String>,
    ) -> Self {
        self.hints.push(Hint {
            message: message.into(),
            replacement: Some((span, text.into())),
        });
        self
    }

    pub fn with_related(
        mut self,
        kind: RelatedKind,
        name: impl Into<String>,
        span: Option<Span>,
    ) -> Self {
        self.related.push(Related {
            kind,
            name: name.into(),
            span,
        });
        self
    }
}

// ---- JSON 出力用の表現（スキーマと1対1に対応） ----

#[derive(Serialize)]
struct ReportJson<'a> {
    schema_version: &'static str,
    diagnostics: Vec<DiagnosticJson<'a>>,
}

#[derive(Serialize)]
struct DiagnosticJson<'a> {
    code: &'static str,
    severity: Severity,
    message: &'a str,
    span: SpanJson<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cause: Option<&'a str>,
    hints: Vec<HintJson<'a>>,
    related: Vec<RelatedJson<'a>>,
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

#[derive(Serialize)]
struct HintJson<'a> {
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    replacement: Option<ReplacementJson<'a>>,
}

#[derive(Serialize)]
struct ReplacementJson<'a> {
    span: SpanJson<'a>,
    text: &'a str,
}

#[derive(Serialize)]
struct RelatedJson<'a> {
    kind: RelatedKind,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    span: Option<SpanJson<'a>>,
}

fn span_json<'a>(file: &'a SourceFile, span: Span) -> SpanJson<'a> {
    let pos = |b| {
        let p = file.position(b);
        PositionJson {
            line: p.line,
            column: p.column,
            byte: p.byte,
        }
    };
    SpanJson {
        file: &file.name,
        start: pos(span.start),
        end: pos(span.end),
    }
}

/// 診断の一覧をスキーマ v0 の JSON 文字列にする。M0 では単一ファイルのみを扱う。
pub fn to_json(file: &SourceFile, diagnostics: &[Diagnostic]) -> String {
    let report = ReportJson {
        schema_version: SCHEMA_VERSION,
        diagnostics: diagnostics
            .iter()
            .map(|d| DiagnosticJson {
                code: d.code,
                severity: d.severity,
                message: &d.message,
                span: span_json(file, d.span),
                cause: d.cause.as_deref(),
                hints: d
                    .hints
                    .iter()
                    .map(|h| HintJson {
                        message: &h.message,
                        replacement: h.replacement.as_ref().map(|(s, t)| ReplacementJson {
                            span: span_json(file, *s),
                            text: t,
                        }),
                    })
                    .collect(),
                related: d
                    .related
                    .iter()
                    .map(|r| RelatedJson {
                        kind: r.kind,
                        name: &r.name,
                        span: r.span.map(|s| span_json(file, s)),
                    })
                    .collect(),
            })
            .collect(),
    };
    serde_json::to_string_pretty(&report).expect("diagnostic serialization cannot fail")
}

/// 人間向けのテキスト出力（`file:line:col: error[E0001]: message`）。
pub fn to_text(file: &SourceFile, diagnostics: &[Diagnostic]) -> String {
    let mut out = String::new();
    for d in diagnostics {
        let p = file.position(d.span.start);
        let sev = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        out.push_str(&format!(
            "{}:{}:{}: {}[{}]: {}\n",
            file.name, p.line, p.column, sev, d.code, d.message
        ));
        if let Some(c) = &d.cause {
            out.push_str(&format!("  cause: {c}\n"));
        }
        for h in &d.hints {
            out.push_str(&format!("  hint: {}\n", h.message));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = include_str!("../../../docs/spec/diagnostics-schema.json");

    fn sample() -> (SourceFile, Vec<Diagnostic>) {
        let file = SourceFile::new(
            "main.wys",
            "def f(x: int) -> int:\n    return x\n\nf(\"a\")\n",
        );
        let start = file.text.find("\"a\"").unwrap() as u32;
        let arg = Span::new(start, start + 3);
        let d = Diagnostic::error(
            "E0001",
            "型が一致しません: int が必要ですが str が渡されました",
            arg,
        )
        .with_cause("関数 f の引数 x は int として宣言されています")
        .with_fix("int の値を渡してください", arg, "0")
        .with_related(RelatedKind::Function, "f", Some(Span::new(4, 5)));
        (file, vec![d])
    }

    fn validator() -> jsonschema::Validator {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).expect("schema is valid JSON");
        jsonschema::validator_for(&schema).expect("schema compiles")
    }

    #[test]
    fn json_output_matches_schema() {
        let (file, diags) = sample();
        let out: serde_json::Value = serde_json::from_str(&to_json(&file, &diags)).unwrap();
        let errors: Vec<String> = validator()
            .iter_errors(&out)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "schema violations: {errors:?}");
    }

    #[test]
    fn empty_report_matches_schema() {
        let file = SourceFile::new("empty.wys", "");
        let out: serde_json::Value = serde_json::from_str(&to_json(&file, &[])).unwrap();
        assert!(validator().is_valid(&out));
    }

    #[test]
    fn json_positions_are_line_and_column() {
        let (file, diags) = sample();
        let out: serde_json::Value = serde_json::from_str(&to_json(&file, &diags)).unwrap();
        let start = &out["diagnostics"][0]["span"]["start"];
        assert_eq!(start["line"], 4);
        assert_eq!(start["column"], 3);
    }

    #[test]
    fn schema_rejects_bad_code() {
        let bad = serde_json::json!({
            "schema_version": "0",
            "diagnostics": [{
                "code": "oops", "severity": "error", "message": "m",
                "span": {"file": "a", "start": {"line": 1, "column": 1, "byte": 0},
                          "end": {"line": 1, "column": 1, "byte": 0}},
                "hints": [], "related": []
            }]
        });
        assert!(!validator().is_valid(&bad));
    }

    #[test]
    fn text_output_has_location_and_code() {
        let (file, diags) = sample();
        let text = to_text(&file, &diags);
        assert!(text.starts_with("main.wys:4:3: error[E0001]: "), "{text}");
    }
}
