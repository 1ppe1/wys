//! `wys --version` に載せる、ビルドの情報（公開準備 T6）: コミット、target、rustc の版。
//!
//! コミットは `WYS_BUILD_COMMIT`（リリースの手順が設定する）、なければ `git rev-parse --short HEAD`、
//! git がなければ `unknown`。

use std::process::Command;

fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn main() {
    let commit = std::env::var("WYS_BUILD_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| output("git", &["rev-parse", "--short", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc_version = output(&rustc, &["--version"]).unwrap_or_else(|| "rustc unknown".into());
    println!("cargo:rustc-env=WYS_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=WYS_BUILD_TARGET={target}");
    println!("cargo:rustc-env=WYS_BUILD_RUSTC={rustc_version}");
    println!("cargo:rerun-if-env-changed=WYS_BUILD_COMMIT");
    // コミットが進んだら作り直す（ブランチの切り替えと新しいコミット）
    if let Some(git_dir) = output("git", &["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        if let Some(head_ref) = output("git", &["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={git_dir}/{head_ref}");
        }
    }
}
