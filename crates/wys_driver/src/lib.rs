//! コンパイルパイプラインの統合
//!
//! ソース → 字句/構文解析 → 型検査 → IR 変換・検証 → バックエンド → リンク。

use std::path::{Path, PathBuf};
use std::process::Command;
use wys_backend::{Backend, LlvmTextBackend};
use wys_diagnostics::Diagnostic;
use wys_ir::Program;
use wys_span::SourceFile;

pub use wys_diagnostics::{to_json, to_text};

/// フロントエンドの結果。`program` はエラーがなく、コード生成もできるときだけ `Some`。
pub struct Frontend {
    pub file: SourceFile,
    pub diagnostics: Vec<Diagnostic>,
    pub program: Option<Program>,
    /// 型検査は通ったが、このコンパイラのコード生成がまだ対応していない機能を使っている場合の説明。
    /// `check` はこのプログラムを受け付ける。`build` / `run` はこのメッセージで止まる。
    pub unsupported: Option<String>,
}

impl Frontend {
    pub fn has_errors(&self) -> bool {
        !self.diagnostics.is_empty()
    }
}

/// 字句解析から IR 生成までを行う。構文エラーがあれば型検査はしない（エラーの連鎖を防ぐ）。
pub fn frontend(name: &str, text: &str) -> Frontend {
    let file = SourceFile::new(name, text);
    let (module, diagnostics) = wys_parser::parse_source(text);
    if !diagnostics.is_empty() {
        return Frontend {
            file,
            diagnostics,
            program: None,
            unsupported: None,
        };
    }
    let (tc, diagnostics) = wys_typeck::check(&module);
    if !diagnostics.is_empty() {
        return Frontend {
            file,
            diagnostics,
            program: None,
            unsupported: None,
        };
    }
    if let Some(i) = tc.first_function_using_unions() {
        let p = file.position(module.funcs[i].name.span.start);
        return Frontend {
            unsupported: Some(format!(
                "{}:{}:{}: function `{}` uses union types (`A | B`), which the code generator does not support yet (planned for M2 T1b); `check` accepts this program",
                file.name, p.line, p.column, module.funcs[i].name.name
            )),
            file,
            diagnostics,
            program: None,
        };
    }
    let program = wys_lower::lower(&module, &tc);
    if let Err(e) = wys_ir::verify(&program) {
        panic!("internal compiler error: IR verification failed: {e}\n{program}");
    }
    Frontend {
        file,
        diagnostics,
        program: Some(program),
        unsupported: None,
    }
}

#[derive(Debug, Default, Clone)]
pub struct BuildOptions {
    /// ランタイムの静的ライブラリ。未指定なら `WYS_RUNTIME_LIB`、次に実行ファイルの隣を探す。
    pub runtime_lib: Option<PathBuf>,
    /// C コンパイラ／リンカ。未指定なら `WYS_CC`、なければ `clang`。
    pub cc: Option<String>,
}

#[derive(Debug)]
pub enum BuildError {
    Diagnostics(Box<Frontend>),
    Io(String),
    Tool(String),
}

impl std::fmt::Debug for Frontend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frontend")
            .field("file", &self.file.name)
            .field("diagnostics", &self.diagnostics.len())
            .finish()
    }
}

const RUNTIME_LIB_NAME: &str = "libwys_runtime.a";

pub fn find_runtime_lib(opts: &BuildOptions) -> Result<PathBuf, BuildError> {
    if let Some(p) = &opts.runtime_lib {
        return Ok(p.clone());
    }
    if let Some(p) = std::env::var_os("WYS_RUNTIME_LIB") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|e| BuildError::Io(e.to_string()))?;
    let mut candidates = Vec::new();
    if let Some(dir) = exe.parent() {
        candidates.push(dir.join(RUNTIME_LIB_NAME));
        if let Some(parent) = dir.parent() {
            // `target/debug/deps/` から実行されるテストバイナリ用
            candidates.push(parent.join(RUNTIME_LIB_NAME));
        }
    }
    candidates.into_iter().find(|p| p.exists()).ok_or_else(|| {
        BuildError::Tool(format!(
            "cannot find the runtime library `{RUNTIME_LIB_NAME}`; build it with `cargo build -p wys_runtime` or set WYS_RUNTIME_LIB"
        ))
    })
}

fn cc(opts: &BuildOptions) -> String {
    opts.cc
        .clone()
        .or_else(|| std::env::var("WYS_CC").ok())
        .unwrap_or_else(|| "clang".to_string())
}

/// 一時ディレクトリ（終了時に削除）。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self, BuildError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("wys-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| BuildError::Io(e.to_string()))?;
        Ok(Self(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Rust の staticlib（ランタイム）をリンクするのに必要なシステムライブラリ。
/// `cargo rustc -p wys_runtime --lib --crate-type staticlib -- --print native-static-libs` の出力に合わせる。
/// macOS では clang が既定で `libSystem` をリンクするので追加は要らない。
fn native_link_args() -> &'static [&'static str] {
    if cfg!(target_os = "linux") {
        &[
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ]
    } else {
        &[]
    }
}

/// IR から実行ファイルを作る。
pub fn link_program(
    program: &Program,
    output: &Path,
    opts: &BuildOptions,
) -> Result<(), BuildError> {
    let runtime = find_runtime_lib(opts)?;
    let cc = cc(opts);
    let tmp = TempDir::new()?;
    let obj = tmp.0.join("module.o");
    let backend = LlvmTextBackend {
        cc: cc.clone(),
        opt_level: 0,
    };
    backend
        .emit_object(program, &obj)
        .map_err(|e| BuildError::Tool(e.to_string()))?;
    let out = Command::new(&cc)
        .arg(&obj)
        .arg(&runtime)
        .args(native_link_args())
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|e| BuildError::Tool(format!("cannot run `{cc}`: {e}")))?;
    if !out.status.success() {
        return Err(BuildError::Tool(format!(
            "linking failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// ソースファイルから実行ファイルを作る。
pub fn build_file(source: &Path, output: &Path, opts: &BuildOptions) -> Result<(), BuildError> {
    let text = std::fs::read_to_string(source)
        .map_err(|e| BuildError::Io(format!("cannot read {}: {e}", source.display())))?;
    let fe = frontend(&source.display().to_string(), &text);
    if let Some(msg) = &fe.unsupported {
        return Err(BuildError::Tool(msg.clone()));
    }
    let Some(program) = &fe.program else {
        return Err(BuildError::Diagnostics(Box::new(fe)));
    };
    link_program(program, output, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_reports_parse_errors_before_type_errors() {
        let fe = frontend("t.wys", "def main() -> None:\n    x = \n    y = 1 + True\n");
        let codes: Vec<_> = fe.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec!["E0101"]);
        assert!(fe.program.is_none());
    }

    #[test]
    fn unions_pass_check_but_are_not_built_yet() {
        let fe = frontend(
            "t.wys",
            "def f(x: int | None) -> int:\n    if x is None:\n        return 0\n    return x\n\ndef main() -> None:\n    print(f(None))\n",
        );
        assert!(!fe.has_errors());
        assert!(fe.program.is_none());
        assert!(fe.unsupported.unwrap().contains("M2 T1b"));
    }

    #[test]
    fn frontend_produces_verified_ir() {
        let fe = frontend("t.wys", "def main() -> None:\n    print(\"hi\")\n");
        assert!(!fe.has_errors());
        assert!(fe.program.is_some());
    }
}
