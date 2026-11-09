//! Language Server Protocol の初期版（M6 PoC6、DX-003、`docs/plans/m6.md` T9）。
//!
//! `wys lsp` が stdio で JSON-RPC を話す（同期・単一スレッド。`lsp-server` + `lsp-types`）。PoC6 の範囲は診断だけ:
//! 開いているバッファを `wys_driver::frontend_with` の読み込みに重ね、`didOpen` / `didChange` / `didClose` のたびに
//! フロントエンドを全部回して、`textDocument/publishDiagnostics` をファイルごとに配る（import 先の診断も、その
//! ファイルの URI に）。位置の符号化は、クライアントが UTF-8 を受け付ければ UTF-8、そうでなければ UTF-16。
//!
//! transport を含まない純粋な部分（`diagnostics`、位置の変換）は単体テストで、JSON-RPC の全体は e2e で確かめる。

pub mod query;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lsp_server::{Connection, Message, Notification};
use lsp_types::notification::Notification as _;
use lsp_types::{
    Diagnostic, DiagnosticSeverity, NumberOrString, Position, PositionEncodingKind,
    PublishDiagnosticsParams, Range, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, Uri,
};
use wys_span::SourceMap;

/// 位置の符号化（LSP 3.17 の `positionEncoding`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

/// `byte`（`SourceMap` の通しの位置）を、そのファイルの LSP の位置にする。
pub fn lsp_position(map: &SourceMap, byte: u32, enc: Encoding) -> Position {
    let file = map.file_at(byte);
    let local = map.position(byte).byte as usize;
    let line_start = file.text[..local].rfind('\n').map_or(0, |i| i + 1);
    let prefix = &file.text[line_start..local];
    let character = match enc {
        Encoding::Utf8 => prefix.len(),
        Encoding::Utf16 => prefix.encode_utf16().count(),
    };
    Position {
        line: map.position(byte).line - 1,
        character: character as u32,
    }
}

/// エントリのファイル `path`（中身は `text`）の診断を、ファイルごとに返す。`overlay` は開いているバッファ
/// （import 先の読み込みで、ディスクより優先する）。
pub fn diagnostics(
    path: &Path,
    text: &str,
    overlay: &HashMap<PathBuf, String>,
    enc: Encoding,
) -> HashMap<PathBuf, Vec<Diagnostic>> {
    analyze(path, text, overlay, enc).0
}

/// LSP の位置（交渉した符号化）を、文書の中のバイト位置にする。行末を超える `character` は行末に、文字の途中は
/// 文字の先頭に丸める（ADR-0026 論点 8）
pub fn byte_at(text: &str, pos: Position, enc: Encoding) -> u32 {
    let mut start = 0usize;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i as u32 == pos.line {
            let body = line.trim_end_matches(['\n', '\r']);
            let mut used = 0u32;
            let mut off = 0usize;
            for ch in body.chars() {
                let w = match enc {
                    Encoding::Utf8 => ch.len_utf8() as u32,
                    Encoding::Utf16 => ch.len_utf16() as u32,
                };
                if used + w > pos.character {
                    break;
                }
                used += w;
                off += ch.len_utf8();
            }
            return (start + off) as u32;
        }
        start += line.len();
    }
    text.len() as u32
}

/// エントリのファイル `path` を解析し、診断（ファイルごと）と、ホバー・定義のための解析の結果を返す
pub fn analyze(
    path: &Path,
    text: &str,
    overlay: &HashMap<PathBuf, String>,
    enc: Encoding,
) -> (HashMap<PathBuf, Vec<Diagnostic>>, wys_driver::Frontend) {
    let read = |p: &Path| {
        overlay
            .get(p)
            .or_else(|| overlay.get(&canon(p)))
            .cloned()
            .or_else(|| std::fs::read_to_string(p).ok())
    };
    // エディタで開くファイルの多くは `main` のないモジュールなので、`main` を求めない（`main` があれば検査する）。
    // 型のエラーがあっても、ホバーと定義のために型検査まで進めた結果を残す（ADR-0026 論点 6・9）
    let fe = wys_driver::frontend_for_editor(&path.display().to_string(), text, &read);
    let mut out: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();
    out.insert(path.to_path_buf(), Vec::new());
    for d in &fe.diagnostics {
        let file = PathBuf::from(fe.file.file_name(d.span.start));
        let severity = if d.is_error() {
            DiagnosticSeverity::ERROR
        } else {
            DiagnosticSeverity::WARNING
        };
        let range_of = |span: wys_span::Span| Range {
            start: lsp_position(&fe.file, span.start, enc),
            end: lsp_position(&fe.file, span.end, enc),
        };
        // 診断 JSON の `related`（位置のあるものだけ。ADR-0026 論点 7）
        let related: Vec<lsp_types::DiagnosticRelatedInformation> = d
            .related
            .iter()
            .filter_map(|r| {
                let span = r.span?;
                let uri: Uri = path_to_uri(Path::new(fe.file.file_name(span.start)))
                    .parse()
                    .ok()?;
                Some(lsp_types::DiagnosticRelatedInformation {
                    location: lsp_types::Location {
                        uri,
                        range: range_of(span),
                    },
                    message: r.name.clone(),
                })
            })
            .collect();
        // quickfix: `replacement` のある修正案を `data` に入れる。`codeAction` はクライアントが送り返す
        // `data` から編集を作る（ADR-0026 論点 12）。編集先は `replacement` の位置のファイル
        let fixes: Vec<serde_json::Value> = d
            .hints
            .iter()
            .filter_map(|h| {
                let (span, text) = h.replacement.as_ref()?;
                Some(serde_json::json!({
                    "title": h.message,
                    "path": fe.file.file_name(span.start),
                    "range": range_of(*span),
                    "text": text,
                }))
            })
            .collect();
        out.entry(file).or_default().push(Diagnostic {
            range: range_of(d.span),
            severity: Some(severity),
            code: Some(NumberOrString::String(d.code.to_string())),
            source: Some("wys".into()),
            message: d.message.clone(),
            related_information: (!related.is_empty()).then_some(related),
            data: (!fixes.is_empty()).then(|| serde_json::json!({ "fixes": fixes })),
            ..Diagnostic::default()
        });
    }
    (out, fe)
}

/// `file://` の URI のパス（host は空か `localhost` だけ。クエリとフラグメントは落とす）。ほかの URI は `None`。
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut path = percent_decode(rest);
    // Windows: `/c:/Users/...` の先頭の `/` を落とす
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        path.remove(0);
    }
    Some(PathBuf::from(path))
}

/// パスの `file://` の URI（開いていない import 先の診断を配るとき。開いている文書は受け取った URI をそのまま使う）。
pub fn path_to_uri(path: &Path) -> String {
    let mut s = String::from("file://");
    let text = path.display().to_string().replace('\\', "/");
    if !text.starts_with('/') {
        s.push('/');
    }
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                s.push(b as char)
            }
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

/// 実のパス（シンボリックリンクと macOS の `/tmp` → `/private/tmp` を解く）。解けない（まだない）ファイルはそのまま。
/// 開いている文書・import 先・編集先を同じパスで突き合わせる（ADR-0026 論点 5）
fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(&h), Some(&l)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(h), Some(l)) = (hex(h), hex(l))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// stdio で LSP のサーバーとして動く（`wys lsp`）。`shutdown` の後の `exit` か stdin の終わりで `Ok(true)`、
/// `shutdown` なしの `exit` で `Ok(false)`（仕様では終了コード 1）。
pub fn run_stdio() -> Result<bool, String> {
    let (conn, io) = Connection::stdio();
    let clean = serve(&conn);
    drop(conn);
    // 読み取りの失敗（壊れた枠など）は `io.join()` が返す。lsp-server には -32700 で続ける口がない
    let joined = io.join().map_err(|e| e.to_string());
    let clean = clean?;
    joined?;
    Ok(clean)
}

/// 1 つの接続でサーバーとして動く（transport を含まないので、`Connection::memory()` で単体テストできる）。
pub fn serve(conn: &Connection) -> Result<bool, String> {
    let (id, params) = match conn.initialize_start() {
        Ok(x) => x,
        // `initialize` の前に閉じた
        Err(_) => return Ok(true),
    };
    let params: lsp_types::InitializeParams = serde_json::from_value(params).unwrap_or_default();
    let offered = params
        .capabilities
        .general
        .and_then(|g| g.position_encodings)
        .unwrap_or_default();
    let enc = if offered.contains(&PositionEncodingKind::UTF8) {
        Encoding::Utf8
    } else {
        Encoding::Utf16
    };
    let caps = ServerCapabilities {
        position_encoding: Some(match enc {
            Encoding::Utf8 => PositionEncodingKind::UTF8,
            Encoding::Utf16 => PositionEncodingKind::UTF16,
        }),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::Options(
            lsp_types::CodeActionOptions {
                code_action_kinds: Some(vec![lsp_types::CodeActionKind::QUICKFIX]),
                ..lsp_types::CodeActionOptions::default()
            },
        )),
        document_formatting_provider: Some(lsp_types::OneOf::Left(true)),
        definition_provider: Some(lsp_types::OneOf::Left(true)),
        ..ServerCapabilities::default()
    };
    let result = serde_json::json!({
        "capabilities": caps,
        "serverInfo": {"name": "wys lsp", "version": env!("CARGO_PKG_VERSION")}
    });
    if conn.initialize_finish(id, result).is_err() {
        // `initialized` の前に閉じた
        return Ok(true);
    }
    let mut server = Server {
        docs: HashMap::new(),
        published: HashMap::new(),
        analyses: HashMap::new(),
        enc,
    };
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                match conn.handle_shutdown(&req) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(e) => return Err(e.to_string()),
                }
                let resp = server.request(req);
                send(conn, Message::Response(resp))?;
            }
            Message::Notification(n) if n.method == "exit" => return Ok(false),
            Message::Notification(n) => server.notify(conn, n)?,
            Message::Response(_) => {}
        }
    }
    Ok(true)
}

fn send(conn: &Connection, m: Message) -> Result<(), String> {
    conn.sender.send(m).map_err(|e| e.to_string())
}

/// `window/logMessage`（壊れた通知などはサーバーを止めずに知らせる）
fn log(conn: &Connection, message: String) -> Result<(), String> {
    let n = Notification::new(
        "window/logMessage".into(),
        serde_json::json!({"type": 2, "message": message}),
    );
    send(conn, Message::Notification(n))
}

struct Server {
    /// 開いている文書: 受け取った URI → (パス, 全文)
    docs: HashMap<String, (PathBuf, String)>,
    /// いま診断を配っている URI と、その診断（変わった URI だけ配り直し、消えた URI は空で配る）
    published: HashMap<String, Vec<Diagnostic>>,
    /// 開いている文書ごとの、その文書をエントリにした最後の解析の結果（ホバーと定義が使う。ADR-0026 論点 14）
    analyses: HashMap<String, wys_driver::Frontend>,
    enc: Encoding,
}

impl Server {
    /// 要求に答える（ホバー・定義。ほかは `MethodNotFound`。壊れた引数は `InvalidParams`）
    fn request(&self, req: lsp_server::Request) -> lsp_server::Response {
        let id = req.id.clone();
        let invalid = |e: serde_json::Error| {
            lsp_server::Response::new_err(
                id.clone(),
                lsp_server::ErrorCode::InvalidParams as i32,
                e.to_string(),
            )
        };
        match req.method.as_str() {
            "textDocument/hover" => {
                let p: lsp_types::HoverParams = match serde_json::from_value(req.params) {
                    Ok(p) => p,
                    Err(e) => return invalid(e),
                };
                let tp = p.text_document_position_params;
                let result = self
                    .at(tp.text_document.uri.as_str(), tp.position)
                    .and_then(|(fe, byte)| {
                        let h = query::hover_at(fe, byte)?;
                        Some(serde_json::json!({
                            "contents": {"kind": "markdown", "value": h.markdown()},
                            "range": Range {
                                start: lsp_position(&fe.file, h.span.start, self.enc),
                                end: lsp_position(&fe.file, h.span.end, self.enc),
                            },
                        }))
                    });
                lsp_server::Response::new_ok(req.id, result.unwrap_or(serde_json::Value::Null))
            }
            "textDocument/definition" => {
                let p: lsp_types::GotoDefinitionParams = match serde_json::from_value(req.params) {
                    Ok(p) => p,
                    Err(e) => return invalid(e),
                };
                let tp = p.text_document_position_params;
                let result = self
                    .at(tp.text_document.uri.as_str(), tp.position)
                    .and_then(|(fe, byte)| {
                        let span = query::definition_at(fe, byte)?;
                        let uri = self.uri_of(Path::new(fe.file.file_name(span.start)));
                        Some(serde_json::json!({
                            "uri": uri,
                            "range": Range {
                                start: lsp_position(&fe.file, span.start, self.enc),
                                end: lsp_position(&fe.file, span.end, self.enc),
                            },
                        }))
                    });
                lsp_server::Response::new_ok(req.id, result.unwrap_or(serde_json::Value::Null))
            }
            "textDocument/codeAction" => {
                let p: lsp_types::CodeActionParams = match serde_json::from_value(req.params) {
                    Ok(p) => p,
                    Err(e) => return invalid(e),
                };
                lsp_server::Response::new_ok(req.id, self.code_actions(&p.context.diagnostics))
            }
            "textDocument/formatting" => {
                let p: lsp_types::DocumentFormattingParams =
                    match serde_json::from_value(req.params) {
                        Ok(p) => p,
                        Err(e) => return invalid(e),
                    };
                let result = self
                    .docs
                    .get(p.text_document.uri.as_str())
                    .map_or(serde_json::Value::Null, |(_, text)| self.format(text));
                lsp_server::Response::new_ok(req.id, result)
            }
            _ => lsp_server::Response::new_err(
                req.id,
                lsp_server::ErrorCode::MethodNotFound as i32,
                format!("`{}` is not supported", req.method),
            ),
        }
    }

    /// 診断の `data` の修正案を quickfix の code action にする（修正案ごとに 1 つ）
    fn code_actions(&self, diagnostics: &[Diagnostic]) -> serde_json::Value {
        let mut actions = Vec::new();
        for d in diagnostics {
            let Some(fixes) = d.data.as_ref().and_then(|v| v["fixes"].as_array()) else {
                continue;
            };
            for f in fixes {
                let (Some(title), Some(path), Some(text)) =
                    (f["title"].as_str(), f["path"].as_str(), f["text"].as_str())
                else {
                    continue;
                };
                let uri = self.uri_of(Path::new(path));
                actions.push(serde_json::json!({
                    "title": title,
                    "kind": "quickfix",
                    "diagnostics": [d],
                    "edit": {"changes": {uri: [{"range": f["range"], "newText": text}]}},
                }));
            }
        }
        serde_json::Value::Array(actions)
    }

    /// 全文を整形する（`wys fmt` と同じ。ADR-0026 論点 13）。字句エラーなら `null`、変わらなければ空の配列
    fn format(&self, text: &str) -> serde_json::Value {
        let Ok(formatted) = wys_fmt::format_source(text) else {
            return serde_json::Value::Null;
        };
        if formatted == text {
            return serde_json::json!([]);
        }
        // 全文の置き換え: 終わりは最後の行の後（行の数、0 文字目）
        let lines = text.split_inclusive('\n').count() as u32;
        let end = if text.ends_with('\n') || text.is_empty() {
            Position {
                line: lines,
                character: 0,
            }
        } else {
            let last = text.rsplit('\n').next().unwrap_or("");
            let character = match self.enc {
                Encoding::Utf8 => last.len(),
                Encoding::Utf16 => last.encode_utf16().count(),
            } as u32;
            Position {
                line: lines.saturating_sub(1),
                character,
            }
        };
        serde_json::json!([{
            "range": Range { start: Position { line: 0, character: 0 }, end },
            "newText": formatted,
        }])
    }

    /// ファイル `path` の URI。同じパスで開いている文書があればその URI、なければ実のパスが同じ文書の URI
    /// （シンボリックリンクの両側を開いていればすべて）、どれもなければパスから作る（ADR-0026 論点 5）
    fn uris_of(&self, path: &Path) -> Vec<String> {
        let mut uris: Vec<String> = self
            .docs
            .iter()
            .filter(|(_, (p, _))| p == path)
            .map(|(u, _)| u.clone())
            .collect();
        if uris.is_empty() {
            let real = canon(path);
            uris = self
                .docs
                .iter()
                .filter(|(_, (p, _))| canon(p) == real)
                .map(|(u, _)| u.clone())
                .collect();
        }
        if uris.is_empty() {
            uris.push(path_to_uri(path));
        }
        uris.sort();
        uris
    }

    fn uri_of(&self, path: &Path) -> String {
        self.uris_of(path).swap_remove(0)
    }

    /// 開いている文書 `uri` の解析の結果と、位置のバイト位置（エントリのファイルは基準位置 0）
    fn at(&self, uri: &str, pos: Position) -> Option<(&wys_driver::Frontend, u32)> {
        let (_, text) = self.docs.get(uri)?;
        let fe = self.analyses.get(uri)?;
        Some((fe, byte_at(text, pos, self.enc)))
    }

    /// 通知を処理する。中身の誤りはサーバーを止めずに `window/logMessage` で知らせ、`Err` は送信の失敗だけ
    fn notify(&mut self, conn: &Connection, n: Notification) -> Result<(), String> {
        use lsp_types::notification::{
            DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument,
        };
        let changed = match n.method.as_str() {
            DidOpenTextDocument::METHOD => {
                match serde_json::from_value::<lsp_types::DidOpenTextDocumentParams>(n.params) {
                    Ok(p) => self.open(p.text_document.uri.as_str(), p.text_document.text),
                    Err(e) => return log(conn, format!("ignored a malformed didOpen: {e}")),
                }
            }
            DidChangeTextDocument::METHOD => {
                match serde_json::from_value::<lsp_types::DidChangeTextDocumentParams>(n.params) {
                    // 全体の同期（TextDocumentSyncKind::FULL）なので、最後の変更が全文
                    Ok(p) => match p.content_changes.into_iter().last() {
                        Some(c) => self.open(p.text_document.uri.as_str(), c.text),
                        None => false,
                    },
                    Err(e) => return log(conn, format!("ignored a malformed didChange: {e}")),
                }
            }
            DidCloseTextDocument::METHOD => {
                match serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(n.params) {
                    Ok(p) => self.docs.remove(p.text_document.uri.as_str()).is_some(),
                    Err(e) => return log(conn, format!("ignored a malformed didClose: {e}")),
                }
            }
            _ => false,
        };
        if changed {
            self.republish(conn)?;
        }
        Ok(())
    }

    /// 文書を開く・置き換える。`file://` でない URI（`untitled:`、`git:` など）は無視する
    fn open(&mut self, uri: &str, text: String) -> bool {
        match uri_to_path(uri) {
            Some(path) => {
                self.docs.insert(uri.to_string(), (path, text));
                true
            }
            None => false,
        }
    }

    /// 開いているすべての文書をエントリとして解析し直し、URI ごとに診断の和集合を配る（同じモジュールを import する
    /// 文書があっても消し合わない）。変わった URI だけ配り、もう診断のない URI には空を配る
    fn republish(&mut self, conn: &Connection) -> Result<(), String> {
        // 読み込みは実のパスで突き合わせる。エントリには受け取ったパスのまま渡す（import はそのディレクトリから
        // 探すので、リンクの先のディレクトリに変えない。Codex のレビュー）。解析のスレッドごとに全文を複製しないように
        // `Arc` で共有する
        // 同じパスの文書を優先し、実のパスはそれがないときだけ（リンクと実のパスの両方を開いていても、
        // どちらの全文を読むかが決まる。Codex のレビュー）
        let mut overlay: HashMap<PathBuf, String> = self
            .docs
            .values()
            .map(|(p, t)| (p.clone(), t.clone()))
            .collect();
        for (p, t) in self.docs.values() {
            overlay.entry(canon(p)).or_insert_with(|| t.clone());
        }
        let overlay = Arc::new(overlay);
        let mut now: HashMap<String, Vec<Diagnostic>> = HashMap::new();
        let mut analyses = HashMap::new();
        for (doc_uri, (path, text)) in &self.docs {
            let by_file = match isolated_analyze(path, text, Arc::clone(&overlay), self.enc) {
                Ok((d, fe)) => {
                    analyses.insert(doc_uri.clone(), fe);
                    d
                }
                Err(why) => {
                    // 解析が落ちた文書は結果を捨てる（古い結果は位置がずれる）
                    log(conn, format!("cannot analyze {}: {why}", path.display()))?;
                    continue;
                }
            };
            for (file, ds) in by_file {
                // エントリの診断はその文書の URI だけに（同じファイルを別の URI でも開いていれば、そちらは
                // そちらの全文で解析する。Codex のレビュー）
                let uris = if file == *path {
                    vec![doc_uri.clone()]
                } else {
                    self.uris_of(&file)
                };
                for uri in uris {
                    let slot = now.entry(uri).or_default();
                    for d in &ds {
                        if !slot.contains(d) {
                            slot.push(d.clone());
                        }
                    }
                }
            }
        }
        for (uri, ds) in &now {
            if self.published.get(uri) != Some(ds) {
                publish(conn, uri, ds.clone())?;
            }
        }
        for uri in self.published.keys() {
            if !now.contains_key(uri) {
                publish(conn, uri, Vec::new())?;
            }
        }
        self.published = now;
        self.analyses = analyses;
        Ok(())
    }
}

/// `diagnostics` を大きなスタックの別スレッドで回す。フロントエンドのパニックやスタックの溢れ（深い入れ子の式）で
/// サーバーが落ちないように（`catch_unwind` ではスタックの溢れは拾えない）
type Analyzed = (HashMap<PathBuf, Vec<Diagnostic>>, wys_driver::Frontend);

fn isolated_analyze(
    path: &Path,
    text: &str,
    overlay: Arc<HashMap<PathBuf, String>>,
    enc: Encoding,
) -> Result<Analyzed, String> {
    let (path, text) = (path.to_path_buf(), text.to_string());
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || analyze(&path, &text, &overlay, enc))
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|_| "the compiler panicked".to_string())
}

fn publish(conn: &Connection, uri: &str, diagnostics: Vec<Diagnostic>) -> Result<(), String> {
    let Ok(uri) = uri.parse::<Uri>() else {
        return log(conn, format!("cannot publish to the URI {uri}"));
    };
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };
    let n = Notification::new(
        lsp_types::notification::PublishDiagnostics::METHOD.into(),
        params,
    );
    send(conn, Message::Notification(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_follow_the_lsp_positions() {
        let path = PathBuf::from("/tmp/wys-lsp-unit/main.wys");
        let text = "def main() -> None:\n    x: int = \"あい\"\n";
        let ds = diagnostics(&path, text, &HashMap::new(), Encoding::Utf8);
        let d = &ds[&path][0];
        assert_eq!(d.code, Some(NumberOrString::String("E0201".into())));
        assert_eq!(
            d.range.start,
            Position {
                line: 1,
                character: 13
            }
        );
        // UTF-8 は「あい」で 6 バイト、UTF-16 は 2 単位
        assert_eq!(d.range.end.character - d.range.start.character, 8);
        let ds = diagnostics(&path, text, &HashMap::new(), Encoding::Utf16);
        let d = &ds[&path][0];
        assert_eq!(d.range.end.character - d.range.start.character, 4);
    }

    #[test]
    fn open_buffers_override_imported_files() {
        let dir = PathBuf::from("/tmp/wys-lsp-unit-overlay");
        let main = dir.join("main.wys");
        let lib = dir.join("lib.wys");
        let mut overlay = HashMap::new();
        overlay.insert(
            lib.clone(),
            "pub def f() -> int:\n    return \"x\"\n".to_string(),
        );
        let ds = diagnostics(
            &main,
            "import lib\n\ndef main() -> None:\n    print(lib.f())\n",
            &overlay,
            Encoding::Utf16,
        );
        assert!(ds[&main].is_empty(), "{:?}", ds[&main]);
        assert_eq!(
            ds[&lib][0].code,
            Some(NumberOrString::String("E0201".into()))
        );
    }

    #[test]
    fn uris_and_paths_round_trip() {
        let p = PathBuf::from("/tmp/a b/ü.wys");
        let uri = path_to_uri(&p);
        assert_eq!(uri, "file:///tmp/a%20b/%C3%BC.wys");
        assert_eq!(uri_to_path(&uri).unwrap(), p);
        assert_eq!(
            uri_to_path("file://localhost/tmp/x.wys?a#b").unwrap(),
            PathBuf::from("/tmp/x.wys")
        );
        assert_eq!(
            uri_to_path("file:///c%3A/Users/x.wys").unwrap(),
            PathBuf::from("c:/Users/x.wys")
        );
        assert_eq!(uri_to_path("untitled:Untitled-1"), None);
        assert_eq!(uri_to_path("file://server/share/x.wys"), None);
        // `%` の後が多バイト文字でもパニックしない
        assert_eq!(
            uri_to_path("file:///tmp/%あ.wys").unwrap(),
            PathBuf::from("/tmp/%あ.wys")
        );
    }

    #[test]
    fn modules_without_main_have_no_e0210() {
        let path = PathBuf::from("/tmp/wys-lsp-unit/lib.wys");
        let ds = diagnostics(
            &path,
            "pub def f() -> int:\n    return 1\n",
            &HashMap::new(),
            Encoding::Utf16,
        );
        assert!(ds[&path].is_empty(), "{:?}", ds[&path]);
    }

    /// メモリの接続でサーバーを動かし、クライアントの送受信を `f` に任せる
    fn with_server(f: impl FnOnce(&Connection)) -> bool {
        let (server, client) = Connection::memory();
        let t = std::thread::spawn(move || serve(&server));
        let req = |id: i32, method: &str, params: serde_json::Value| {
            Message::Request(lsp_server::Request::new(id.into(), method.into(), params))
        };
        client
            .sender
            .send(req(
                1,
                "initialize",
                serde_json::json!({"capabilities": {}}),
            ))
            .unwrap();
        let _ = client.receiver.recv().unwrap();
        client
            .sender
            .send(Message::Notification(Notification::new(
                "initialized".into(),
                serde_json::json!({}),
            )))
            .unwrap();
        f(&client);
        drop(client);
        t.join().unwrap().unwrap()
    }

    fn note(client: &Connection, method: &str, params: serde_json::Value) {
        client
            .sender
            .send(Message::Notification(Notification::new(
                method.into(),
                params,
            )))
            .unwrap();
    }

    /// 次の `publishDiagnostics`（URI → 診断のコード）
    fn next_publish(client: &Connection) -> (String, Vec<String>) {
        loop {
            let Message::Notification(n) = client.receiver.recv().unwrap() else {
                continue;
            };
            if n.method == "textDocument/publishDiagnostics" {
                let codes = n.params["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|d| d["code"].as_str().unwrap_or_default().to_string())
                    .collect();
                return (n.params["uri"].as_str().unwrap().to_string(), codes);
            }
        }
    }

    #[test]
    fn shared_imports_keep_their_diagnostics_and_other_uris_are_ignored() {
        with_server(|c| {
            let open = |uri: &str, text: &str| {
                note(
                    c,
                    "textDocument/didOpen",
                    serde_json::json!({"textDocument": {"uri": uri, "languageId": "wys", "version": 1, "text": text}}),
                );
            };
            // file:// でない URI と壊れた通知はサーバーを止めない
            open("untitled:Untitled-1", "x");
            note(
                c,
                "textDocument/didOpen",
                serde_json::json!({"textDocument": {"uri": 42}}),
            );
            let lib = "pub def f() -> int:\n    return \"x\"\n";
            open("file:///tmp/wys-lsp-shared/lib.wys", lib);
            let (uri, codes) = next_publish(c);
            assert_eq!(
                (uri.as_str(), codes),
                (
                    "file:///tmp/wys-lsp-shared/lib.wys",
                    vec!["E0201".to_string()]
                )
            );
            let user = "import lib\n\ndef g() -> int:\n    return lib.f()\n";
            open("file:///tmp/wys-lsp-shared/a.wys", user);
            assert_eq!(next_publish(c).1, Vec::<String>::new());
            open("file:///tmp/wys-lsp-shared/b.wys", user);
            assert_eq!(next_publish(c).1, Vec::<String>::new());
            // a を閉じても、b が import している lib の診断は残る（a の空の診断だけが配られる）
            note(
                c,
                "textDocument/didClose",
                serde_json::json!({"textDocument": {"uri": "file:///tmp/wys-lsp-shared/a.wys"}}),
            );
            let (uri, codes) = next_publish(c);
            assert_eq!(uri, "file:///tmp/wys-lsp-shared/a.wys");
            assert!(codes.is_empty());
            // lib を直すと lib の診断が消える
            note(
                c,
                "textDocument/didChange",
                serde_json::json!({"textDocument": {"uri": "file:///tmp/wys-lsp-shared/lib.wys", "version": 2}, "contentChanges": [{"text": "pub def f() -> int:\n    return 1\n"}]}),
            );
            let (uri, codes) = next_publish(c);
            assert_eq!(uri, "file:///tmp/wys-lsp-shared/lib.wys");
            assert!(codes.is_empty());
        });
    }

    /// シンボリックリンク越しに開いた文書も、実のパスで開いた import 先と重ね合わせる（ADR-0026 論点 5）
    #[cfg(unix)]
    #[test]
    fn documents_opened_through_symlinks_share_the_overlay() {
        let root = std::env::temp_dir().join(format!("wys-lsp-symlink-{}", std::process::id()));
        let real = root.join("real");
        let link = root.join("link");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // ディスクの lib には `f` がある。開いたバッファでは `g` に変えた
        std::fs::write(real.join("lib.wys"), "pub def f() -> int:\n    return 1\n").unwrap();
        std::fs::write(real.join("main.wys"), "").unwrap();
        let lib_uri = path_to_uri(&real.join("lib.wys"));
        let main_uri = path_to_uri(&link.join("main.wys"));
        with_server(|c| {
            let open = |uri: &str, text: &str| {
                note(
                    c,
                    "textDocument/didOpen",
                    serde_json::json!({"textDocument": {"uri": uri, "languageId": "wys", "version": 1, "text": text}}),
                );
            };
            open(&lib_uri, "pub def g() -> int:\n    return 1\n");
            assert_eq!(next_publish(c), (lib_uri.clone(), vec![]));
            open(
                &main_uri,
                "import lib\n\ndef h() -> int:\n    return lib.f()\n",
            );
            // バッファの lib には `f` がないので、main に名前解決のエラーが出る。lib の診断は開いた URI のまま
            let (uri, codes) = next_publish(c);
            assert_eq!(uri, main_uri);
            assert_eq!(codes.len(), 1, "{codes:?}");
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    /// エントリ自体がリンクなら、import はリンクのあるディレクトリから探す。リンクと実のパスの両方を開けば、
    /// それぞれの URI にそれぞれの全文の診断を配る（Codex のレビュー）
    #[cfg(unix)]
    #[test]
    fn symlinked_entries_keep_their_import_root_and_aliases_keep_their_diagnostics() {
        let root = std::env::temp_dir().join(format!("wys-lsp-alias-{}", std::process::id()));
        let project = root.join("project");
        let shared = root.join("shared");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(
            project.join("lib.wys"),
            "pub def f() -> int:\n    return 1\n",
        )
        .unwrap();
        std::fs::write(shared.join("main.wys"), "").unwrap();
        std::os::unix::fs::symlink(shared.join("main.wys"), project.join("main.wys")).unwrap();
        let link_uri = path_to_uri(&project.join("main.wys"));
        let real_uri = path_to_uri(&shared.join("main.wys"));
        with_server(|c| {
            let open = |uri: &str, text: &str| {
                note(
                    c,
                    "textDocument/didOpen",
                    serde_json::json!({"textDocument": {"uri": uri, "languageId": "wys", "version": 1, "text": text}}),
                );
            };
            // `project/lib.wys` が見つかる（`shared/` には lib がない）
            open(
                &link_uri,
                "import lib\n\ndef h() -> int:\n    return lib.f()\n",
            );
            assert_eq!(next_publish(c), (link_uri.clone(), vec![]));
            // 実のパスの側は別の全文（型のエラー）。リンクの側の診断は空のまま
            open(&real_uri, "def h() -> int:\n    return \"x\"\n");
            assert_eq!(
                next_publish(c),
                (real_uri.clone(), vec!["E0201".to_string()])
            );
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    /// import 先のリンクと実のパスを両方開いて全文が違えば、import したパスと同じ文書の全文を読む（Codex のレビュー。
    /// `HashMap` の順序に左右されないことを、サーバーを作り直して何度か確かめる）
    #[cfg(unix)]
    #[test]
    fn imports_read_the_buffer_opened_at_the_same_path() {
        let root = std::env::temp_dir().join(format!("wys-lsp-exact-{}", std::process::id()));
        let project = root.join("project");
        let shared = root.join("shared");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("lib.wys"), "").unwrap();
        std::os::unix::fs::symlink(shared.join("lib.wys"), project.join("lib.wys")).unwrap();
        let lib_link = path_to_uri(&project.join("lib.wys"));
        let lib_real = path_to_uri(&shared.join("lib.wys"));
        let main = path_to_uri(&project.join("main.wys"));
        for _ in 0..8 {
            with_server(|c| {
                let open = |uri: &str, text: &str| {
                    note(
                        c,
                        "textDocument/didOpen",
                        serde_json::json!({"textDocument": {"uri": uri, "languageId": "wys", "version": 1, "text": text}}),
                    );
                    let _ = next_publish(c);
                };
                open(&lib_real, "pub def g() -> int:\n    return 1\n");
                open(&lib_link, "pub def k() -> int:\n    return 1\n");
                // 実のパスの全文（`g` だけ）を読めば名前解決のエラーになる
                note(
                    c,
                    "textDocument/didOpen",
                    serde_json::json!({"textDocument": {"uri": main, "languageId": "wys", "version": 1, "text": "import lib\n\ndef h() -> int:\n    return lib.k()\n"}}),
                );
                assert_eq!(next_publish(c), (main.clone(), vec![]));
            });
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn positions_map_back_to_bytes() {
        let text = "a\nあい x\n😀y\n";
        // UTF-16: 「あい」は 2 単位、😀 は 2 単位（代理ペア）
        assert_eq!(
            byte_at(
                text,
                Position {
                    line: 1,
                    character: 3
                },
                Encoding::Utf16
            ),
            2 + 7
        );
        assert_eq!(
            byte_at(
                text,
                Position {
                    line: 2,
                    character: 2
                },
                Encoding::Utf16
            ),
            11 + 4
        );
        // 代理ペアの途中は文字の先頭に、行末を超えたら行末に丸める
        assert_eq!(
            byte_at(
                text,
                Position {
                    line: 2,
                    character: 1
                },
                Encoding::Utf16
            ),
            11
        );
        assert_eq!(
            byte_at(
                text,
                Position {
                    line: 0,
                    character: 99
                },
                Encoding::Utf16
            ),
            1
        );
        assert_eq!(
            byte_at(
                text,
                Position {
                    line: 1,
                    character: 7
                },
                Encoding::Utf8
            ),
            2 + 7
        );
        assert_eq!(
            byte_at(
                text,
                Position {
                    line: 9,
                    character: 0
                },
                Encoding::Utf8
            ),
            text.len() as u32
        );
    }

    #[test]
    fn hover_and_definition_answer_requests() {
        with_server(|c| {
            let uri = "file:///tmp/wys-lsp-hover/main.wys";
            // 型のエラーがあっても答える
            let text = "def twice(n: int) -> int:\n    return n * 2\n\ndef main() -> None:\n    k = twice(3)\n    s: str = k\n";
            note(
                c,
                "textDocument/didOpen",
                serde_json::json!({"textDocument": {"uri": uri, "languageId": "wys", "version": 1, "text": text}}),
            );
            let (_, codes) = next_publish(c);
            assert_eq!(codes, vec!["E0201".to_string()]);
            let req = |id: i32, method: &str, line: u32, character: u32| {
                Message::Request(lsp_server::Request::new(
                    id.into(),
                    method.into(),
                    serde_json::json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}}),
                ))
            };
            let response = |c: &Connection| loop {
                if let Message::Response(r) = c.receiver.recv().unwrap() {
                    return r;
                }
            };
            c.sender.send(req(7, "textDocument/hover", 4, 9)).unwrap();
            let r = response(c);
            let value = r.result.unwrap()["contents"]["value"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                value.starts_with("```wys\ndef twice(n: int) -> int uses(inferred) pure\n```"),
                "{value}"
            );
            c.sender
                .send(req(8, "textDocument/definition", 5, 13))
                .unwrap();
            let r = response(c);
            let loc = r.result.unwrap();
            assert_eq!(loc["uri"], uri);
            assert_eq!(
                loc["range"]["start"],
                serde_json::json!({"line": 4, "character": 4})
            );
            // 空白の上は null
            c.sender.send(req(9, "textDocument/hover", 2, 0)).unwrap();
            assert_eq!(response(c).result, Some(serde_json::Value::Null));
            // 知らない要求は MethodNotFound
            c.sender
                .send(req(10, "textDocument/completion", 0, 0))
                .unwrap();
            assert!(response(c).error.is_some());
        });
    }

    #[test]
    fn quickfixes_and_formatting_answer_requests() {
        with_server(|c| {
            let uri = "file:///tmp/wys-lsp-fix/main.wys";
            let text = "def say(x: int) -> None uses io:\n    print(x)\n\ndef log(x: int) -> None uses pure:\n    say(x)\n\ndef main() -> None:\n  log( 1 )\n";
            note(
                c,
                "textDocument/didOpen",
                serde_json::json!({"textDocument": {"uri": uri, "languageId": "wys", "version": 1, "text": text}}),
            );
            // 診断を受け取り、そのまま codeAction に送り返す（data に修正案が入っている）
            let diag = loop {
                let Message::Notification(n) = c.receiver.recv().unwrap() else {
                    continue;
                };
                if n.method == "textDocument/publishDiagnostics" {
                    break n.params["diagnostics"][0].clone();
                }
            };
            assert_eq!(diag["code"], "E1102");
            assert_eq!(diag["relatedInformation"][0]["message"], "say");
            let response = |c: &Connection| loop {
                if let Message::Response(r) = c.receiver.recv().unwrap() {
                    return r;
                }
            };
            c.sender
                .send(Message::Request(lsp_server::Request::new(
                    3.into(),
                    "textDocument/codeAction".into(),
                    serde_json::json!({"textDocument": {"uri": uri}, "range": diag["range"], "context": {"diagnostics": [diag]}}),
                )))
                .unwrap();
            let actions = response(c).result.unwrap();
            let action = &actions[0];
            assert_eq!(action["kind"], "quickfix");
            let edit = &action["edit"]["changes"][uri][0];
            assert_eq!(edit["newText"], "io");
            assert_eq!(
                edit["range"]["start"],
                serde_json::json!({"line": 3, "character": 29})
            );
            // 整形は全文の置き換え
            c.sender
                .send(Message::Request(lsp_server::Request::new(
                    4.into(),
                    "textDocument/formatting".into(),
                    serde_json::json!({"textDocument": {"uri": uri}, "options": {"tabSize": 4, "insertSpaces": true}}),
                )))
                .unwrap();
            let edits = response(c).result.unwrap();
            assert_eq!(
                edits[0]["newText"],
                "def say(x: int) -> None uses io:\n    print(x)\n\ndef log(x: int) -> None uses pure:\n    say(x)\n\ndef main() -> None:\n    log(1)\n"
            );
            assert_eq!(
                edits[0]["range"]["end"],
                serde_json::json!({"line": 8, "character": 0})
            );
        });
    }

    #[test]
    fn exit_without_shutdown_is_not_clean() {
        let (server, client) = Connection::memory();
        let t = std::thread::spawn(move || serve(&server));
        client
            .sender
            .send(Message::Request(lsp_server::Request::new(
                1.into(),
                "initialize".into(),
                serde_json::json!({"capabilities": {}}),
            )))
            .unwrap();
        let _ = client.receiver.recv().unwrap();
        note(&client, "initialized", serde_json::json!({}));
        note(&client, "exit", serde_json::json!(null));
        assert!(!t.join().unwrap().unwrap());
    }

    #[test]
    fn deep_nesting_does_not_crash_the_server() {
        let path = PathBuf::from("/tmp/wys-lsp-unit/deep.wys");
        let text = format!(
            "def main() -> None:\n    x = {}1{}\n    print(x)\n",
            "(".repeat(500),
            ")".repeat(500)
        );
        assert!(isolated_analyze(&path, &text, Arc::default(), Encoding::Utf16).is_ok());
    }
}
