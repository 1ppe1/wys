//! Language Server Protocol の初期版（M6 PoC6、DX-003、`docs/plans/m6.md` T9）。
//!
//! `wys lsp` が stdio で JSON-RPC を話す（同期・単一スレッド。`lsp-server` + `lsp-types`）。PoC6 の範囲は診断だけ:
//! 開いているバッファを `wys_driver::frontend_with` の読み込みに重ね、`didOpen` / `didChange` / `didClose` のたびに
//! フロントエンドを全部回して、`textDocument/publishDiagnostics` をファイルごとに配る（import 先の診断も、その
//! ファイルの URI に）。位置の符号化は、クライアントが UTF-8 を受け付ければ UTF-8、そうでなければ UTF-16。
//!
//! transport を含まない純粋な部分（`diagnostics`、位置の変換）は単体テストで、JSON-RPC の全体は e2e で確かめる。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

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
    let read = |p: &Path| {
        overlay
            .get(p)
            .cloned()
            .or_else(|| std::fs::read_to_string(p).ok())
    };
    let fe = wys_driver::frontend_with(&path.display().to_string(), text, &read);
    let mut out: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();
    out.insert(path.to_path_buf(), Vec::new());
    for d in &fe.diagnostics {
        let file = PathBuf::from(fe.file.file_name(d.span.start));
        let severity = if d.is_error() {
            DiagnosticSeverity::ERROR
        } else {
            DiagnosticSeverity::WARNING
        };
        out.entry(file).or_default().push(Diagnostic {
            range: Range {
                start: lsp_position(&fe.file, d.span.start, enc),
                end: lsp_position(&fe.file, d.span.end, enc),
            },
            severity: Some(severity),
            code: Some(NumberOrString::String(d.code.to_string())),
            source: Some("wys".into()),
            message: d.message.clone(),
            ..Diagnostic::default()
        });
    }
    out
}

fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.as_str().strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode(s)))
}

fn path_to_uri(path: &Path) -> Option<Uri> {
    let mut s = String::from("file://");
    for b in path.display().to_string().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                s.push(b as char)
            }
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s.parse().ok()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// stdio で LSP のサーバーとして動く（`wys lsp`）。stdin の終わりか `exit` で戻る。
pub fn run_stdio() -> Result<(), String> {
    let (conn, io) = Connection::stdio();
    let (id, params) = conn.initialize_start().map_err(|e| e.to_string())?;
    let params: lsp_types::InitializeParams =
        serde_json::from_value(params).map_err(|e| e.to_string())?;
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
        ..ServerCapabilities::default()
    };
    let result = serde_json::json!({
        "capabilities": caps,
        "serverInfo": {"name": "wys lsp", "version": env!("CARGO_PKG_VERSION")}
    });
    conn.initialize_finish(id, result)
        .map_err(|e| e.to_string())?;
    let mut server = Server {
        docs: HashMap::new(),
        published: HashMap::new(),
        enc,
    };
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn.handle_shutdown(&req).map_err(|e| e.to_string())? {
                    break;
                }
                // PoC6 では診断だけ。ほかの要求には「未対応」で答える
                let resp = lsp_server::Response::new_err(
                    req.id,
                    lsp_server::ErrorCode::MethodNotFound as i32,
                    format!("`{}` is not supported yet", req.method),
                );
                conn.sender
                    .send(Message::Response(resp))
                    .map_err(|e| e.to_string())?;
            }
            Message::Notification(n) => server.notify(&conn, n)?,
            Message::Response(_) => {}
        }
    }
    drop(conn);
    io.join().map_err(|e| e.to_string())
}

struct Server {
    docs: HashMap<PathBuf, String>,
    /// エントリのファイル → そのファイルの診断を配った URI（消えた診断を空で配り直すため）
    published: HashMap<PathBuf, HashSet<String>>,
    enc: Encoding,
}

impl Server {
    fn notify(&mut self, conn: &Connection, n: Notification) -> Result<(), String> {
        use lsp_types::notification::{
            DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument,
        };
        let path = match n.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let p: lsp_types::DidOpenTextDocumentParams =
                    serde_json::from_value(n.params).map_err(|e| e.to_string())?;
                let path =
                    uri_to_path(&p.text_document.uri).ok_or("only file:// URIs are supported")?;
                self.docs.insert(path.clone(), p.text_document.text);
                path
            }
            DidChangeTextDocument::METHOD => {
                let p: lsp_types::DidChangeTextDocumentParams =
                    serde_json::from_value(n.params).map_err(|e| e.to_string())?;
                let path =
                    uri_to_path(&p.text_document.uri).ok_or("only file:// URIs are supported")?;
                // 全体の同期（TextDocumentSyncKind::FULL）なので、最後の変更が全文
                if let Some(c) = p.content_changes.into_iter().last() {
                    self.docs.insert(path.clone(), c.text);
                }
                path
            }
            DidCloseTextDocument::METHOD => {
                let p: lsp_types::DidCloseTextDocumentParams =
                    serde_json::from_value(n.params).map_err(|e| e.to_string())?;
                let path =
                    uri_to_path(&p.text_document.uri).ok_or("only file:// URIs are supported")?;
                self.docs.remove(&path);
                // 閉じたファイルの診断は消す
                for uri in self.published.remove(&path).unwrap_or_default() {
                    publish(conn, &uri, Vec::new())?;
                }
                return Ok(());
            }
            _ => return Ok(()),
        };
        let text = self.docs.get(&path).cloned().unwrap_or_default();
        let by_file = diagnostics(&path, &text, &self.docs, self.enc);
        let mut now = HashSet::new();
        for (file, ds) in by_file {
            let Some(uri) = path_to_uri(&file) else {
                continue;
            };
            now.insert(uri.as_str().to_string());
            publish(conn, uri.as_str(), ds)?;
        }
        for old in self.published.get(&path).cloned().unwrap_or_default() {
            if !now.contains(&old) {
                publish(conn, &old, Vec::new())?;
            }
        }
        self.published.insert(path, now);
        Ok(())
    }
}

fn publish(conn: &Connection, uri: &str, diagnostics: Vec<Diagnostic>) -> Result<(), String> {
    let uri: Uri = uri.parse().map_err(|_| format!("bad URI {uri}"))?;
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };
    let n = Notification::new(
        lsp_types::notification::PublishDiagnostics::METHOD.into(),
        params,
    );
    conn.sender
        .send(Message::Notification(n))
        .map_err(|e| e.to_string())
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
        let uri = path_to_uri(&p).unwrap();
        assert_eq!(uri.as_str(), "file:///tmp/a%20b/%C3%BC.wys");
        assert_eq!(uri_to_path(&uri).unwrap(), p);
    }
}
