# 変更履歴 / Changelog

この文書の形式は [Keep a Changelog](https://keepachangelog.com/ja/1.1.0/) に、版は [SemVer](https://semver.org/lang/ja/) の `0.y.z` に従う（ADR-0029）。`0.y` の間は、`y` を上げるときに言語の構文・組み込み・CLI が変わり得る。安定性の段階は [docs/stability.md](docs/stability.md)。

## [0.1.1] - 2026-11-17

言語・コンパイラ・ランタイムの挙動は変えていない。試す障壁を下げるための版。

### 追加

- **英語の文書**: `README.md` を英語にし、日本語は `README.ja.md` へ。入門・AI SDK・セキュリティ・安定性の英訳（`docs/en/`）。日本語が正（ADR-0031、Proposed）。英訳と README のコード例もテストで実行する。
- **VS Code の拡張**（`editors/vscode/`、Release に `.vsix` を添付）: `.wys` と Markdown の `wys` のコードブロックの色付け、`wys lsp` による診断・ホバー・定義ジャンプ・quickfix・整形。
- **Homebrew**: `brew install 1ppe1/wys/wys`（tap `1ppe1/homebrew-wys`。ADR-0032、Proposed）。
- **1 行でのインストール**: `curl -fsSL https://raw.githubusercontent.com/1ppe1/wys/main/scripts/install.sh | sh`（`install.sh` 自体は 0.1.0 と同じ）。
- **agent の例**（`examples/agents/`）: 型付きの抽出（`extract`）と、予算でランタイムが止める例（`budget`）。実際のモデル（`WYS_LLM_PROVIDER=anthropic`）でも、同梱の記録の再生でも動く。
- **README**: 承認のないツールの呼び出しをランタイムが止める、`agent_demo` の実際の出力。

### 変更

- `scripts/release.sh --vsix` で拡張も作り、`SHA256SUMS` に含める。拡張の版がコンパイラの版と違えば止める。
- CI は全履歴をチェックアウトし、英訳が元の文書より古くなっていないかを警告する（`scripts/check-translations.sh`）。

## [0.1.0] - 2026-11-12

最初の公開版。Phase 1（言語・Native の MVP）と Phase 2（AI ネイティブ MVP）の範囲。

### 追加

- **言語**: Python に近いインデントの構文。`int` / `float` / `bool` / `str` / `None`、Union（`T | None`）と絞り込み、レコード（構造的・不変）、Generics（単相化）、`match` と網羅性の検査、`Result[T, E]` と `?`、`list` / `dict`、`for`、モジュール（`import`、`pub`）、`async` / `await` と `spawn`、Effect の注釈（`uses`）と推論。
- **コンパイラ**: LLVM IR を生成して clang でリンクし、ランタイムを静的リンクした単独の実行ファイルを作る。ARC（参照カウント）と Move 最適化。Debug / Release（`--release` で -O2）、AddressSanitizer のビルド。
- **外部の値**: `Unknown`、`parse_json` / `to_json`、`validate[T]`（型に合うかを検査し、どこが違うかを返す）。
- **Python 連携**: CPython 3.14 の埋め込み、例外を `Result` に変換する。
- **AI SDK**: 型付きの `llm_generate[T]`、`@tool` / `@agent`、Capability・承認・予算（トークン・費用・時間）のランタイム強制、監査ログ（`WYS_AUDIT`）、トレース（`WYS_TRACE`）、記録と再生（`WYS_LLM_RECORD` / `replay:`）、評価ケース（`wys eval`）、Anthropic のプロバイダと `wys llm-proxy`（API キーを proxy だけが持つ）。
- **道具**: `wys check`（機械向けの JSON 診断）、`build`、`run`、`test`、`fmt`、`explain`（エラーコードと、型・絞り込み・ARC の理由）、`lsp`（診断・ホバー・定義ジャンプ・quickfix・整形）、`--version` / `version --format json`。
- **配布**: `scripts/release.sh`（`bin/` + `lib/` のアーカイブと `SHA256SUMS`）、`scripts/install.sh`（SHA-256 を確かめてから入れる）。対象は macOS arm64（Tier 1）と Linux x86_64（Tier 2）。
- **文書**: 入門、AI SDK、Python 連携、リファレンス、セキュリティ、ベンチマーク、安定性。

[0.1.1]: https://github.com/1ppe1/wys/releases/tag/v0.1.1
[0.1.0]: https://github.com/1ppe1/wys/releases/tag/v0.1.0
