//! PoC4（M5 T1）: `ai_poc.c`（`@tool` / `@agent` の生成コードの相当物）を `libwys_runtime.a` にリンクして、
//! Debug・-O2・AddressSanitizer で実行する。結果は `docs/poc/poc4-results.md`。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const EXPECTED: &str = "\
plan
3
denied: tool `forbidden` is not allowed for this agent
denied: tool `run_py` needs capabilities the agent does not have
schema: tool `greet`: user.age: expected int, found str
budget: max_tool_calls exceeded
schema: title: expected str, found int
timeout: timeout_ms exceeded
budget: max_tokens exceeded
runs: add=2 greet=0 forbidden=0 run_py=0
llm_called:
tool_called:add
llm_called:
llm_called:
tool_denied:forbidden
llm_called:
tool_denied:run_py
llm_called:
tool_called:greet
schema_rejected:greet
llm_called:
budget_exceeded:add
llm_called:
llm_called:
tool_called:add
budget_exceeded:
llm_called:
budget_exceeded:
";

fn runtime_lib() -> &'static PathBuf {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    LIB.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
        let mut cmd = Command::new(env!("CARGO"));
        cmd.args(["build", "-q", "-p", "wys_runtime"]);
        if profile_dir.ends_with("release") {
            cmd.arg("--release");
        }
        assert!(
            cmd.status().unwrap().success(),
            "failed to build wys_runtime"
        );
        profile_dir.join("libwys_runtime.a")
    })
}

#[test]
fn tools_are_checked_by_the_runtime_before_they_run() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ai_poc.c");
    let dir = std::env::temp_dir().join(format!("wys-poc4-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, extra) in [
        ("ai-poc", &[][..]),
        ("ai-poc-o2", &["-O2"][..]),
        ("ai-poc-asan", &["-fsanitize=address", "-g"][..]),
    ] {
        let exe = dir.join(name);
        let mut cc = Command::new(std::env::var("WYS_CC").unwrap_or_else(|_| "clang".into()));
        cc.arg(&src)
            .arg(runtime_lib())
            .args(extra)
            .arg("-o")
            .arg(&exe);
        if cfg!(target_os = "linux") {
            cc.args(["-lpthread", "-ldl", "-lm"]);
        }
        let out = cc.output().unwrap();
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = Command::new(&exe)
            .env("WYS_VIRTUAL_TIME", "1")
            .env_remove("WYS_LLM_PROVIDER")
            .env_remove("WYS_CAPABILITIES")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{name}: {stderr}");
        assert_eq!(stdout, EXPECTED, "{name}: {stderr}");
    }
}

/// 別プロセスのプロバイダ（`cmd:`、JSON 行）との往復。ツールの呼び出しの ID と、前の応答（thinking のブロックを含む）が
/// 次の要求にそのまま戻ることを、テスト用のプロバイダが記録した要求で確かめる。
#[test]
fn a_command_provider_round_trips_tool_calls_over_json_lines() {
    let dir = std::env::temp_dir().join(format!("wys-poc4-cmd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ai_proxy_poc.c");
    let provider = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_provider.sh");
    for (name, extra) in [
        ("ai-proxy", &[][..]),
        ("ai-proxy-asan", &["-fsanitize=address", "-g"][..]),
    ] {
        let exe = dir.join(name);
        let mut cc = Command::new(std::env::var("WYS_CC").unwrap_or_else(|_| "clang".into()));
        cc.arg(&src)
            .arg(runtime_lib())
            .args(extra)
            .arg("-o")
            .arg(&exe);
        if cfg!(target_os = "linux") {
            cc.args(["-lpthread", "-ldl", "-lm"]);
        }
        let out = cc.output().unwrap();
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let log = dir.join(format!("{name}.log"));
        let _ = std::fs::remove_file(&log);
        let out = Command::new(&exe)
            .env("WYS_LLM_PROVIDER", format!("cmd:{}", provider.display()))
            .env("FAKE_LOG", &log)
            .env_remove("WYS_CAPABILITIES")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{name}: {stderr}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "{\"title\":\"plan\",\"steps\":5}\n",
            "{name}: {stderr}"
        );
        let reqs: Vec<serde_json::Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0]["tools"][0]["name"], "add");
        assert_eq!(reqs[0]["schema"]["required"][0], "title");
        let m = &reqs[1]["messages"];
        assert_eq!(m[1]["raw"][0]["type"], "thinking");
        assert_eq!(m[2]["id"], "t1");
        assert_eq!(m[2]["content"], "5");
    }
}
