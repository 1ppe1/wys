# CLAUDE.md

このリポジトリで作業するAIエージェント向けの指示。

## プロジェクトの現状

- AIネイティブ次世代プログラミング言語。**Phase 0**（PoC1 で最小言語 → Native 実行まで動作）。進捗は `docs/roadmap.md`。
- 正となる文書は `docs/ai_native_language_requirements.md`（要件定義書）。迷ったらここに戻る。
- 言語名・拡張子は未定。文書中では「本言語」と呼ぶ。勝手に名前を付けない。
- 内部コードネームは `wys`（ユーザーが決定）。Rust クレートは `crates/wys_*`。これは正式名称ではない。

## ドキュメントのルール

- 文書は日本語で書く。技術用語は `docs/glossary.md` の表記に揃え、新しい用語を導入したら用語集にも追加する。
- **要件ID（LANG-001, AI-004, AC-001 など）は採番済みのものを変更・再利用しない。** 廃止する場合は「廃止」と明記して残す。新規要件は各カテゴリの末尾の番号に続けて追加する。
- 要件定義書の「12. 構文イメージ」は**非規範的**。確定構文として扱ったり、それを前提に仕様を書いたりしない。
- 設計判断は `docs/adr/` にADRとして記録する。AIは `Proposed` で起草するだけで、`Accepted` にするのはユーザー（手順は `docs/adr/README.md`）。要件定義書「10. 未決定事項」の項目を決めたら、ADRを作成し、ADR一覧の対応欄を更新する。
- 新しい文書を追加したら `docs/README.md` の索引を更新する。
- 性能について根拠のない倍率（「Python比○倍」など）を書かない（要件定義書 5.1 の方針）。

## 開発コマンド

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

変更を終える前に、この3つがすべて通ることを確認する（`./scripts/ci.sh` でまとめて実行できる）。

コンパイラを手で動かす場合（`cargo build` でランタイムの `libwys_runtime.a` も作られる）:

```sh
cargo build
target/debug/wys_cli run examples/hello.wys
target/debug/wys_cli check --format json examples/type_error.wys
target/debug/wys_cli dump-ir examples/fib.wys
```

- 新しいエラーコードを追加したら `docs/spec/error-codes.md` に追記する。コードは再利用しない。
- 言語機能を追加したら、`crates/wys_cli/tests/e2e.rs` に実行結果のテストを追加する。

## 設計上の前提（要件定義書より）

- 優先順位は「言語の意味論・型・所有権・Effect・FFI境界」の確定が最優先。GPU機能やAI向け糖衣構文は後回し。
- AI関連機能（`agent`/`tool`/`llm`）はまず標準SDK・デコレータとして設計し、専用構文にはしない。
- セキュリティはRuntime強制が前提。LLM出力・ツール戻り値は信頼しない。プロンプトでの権限制御に依存しない。
- 実装言語はRust（ADR-0002 で決定）。ツールチェーンは `rust-toolchain.toml` で固定。
- 実行計画（マイルストーン・直近タスク）は `docs/roadmap.md`。
