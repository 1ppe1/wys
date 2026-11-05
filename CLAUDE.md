# CLAUDE.md

このリポジトリで作業するAIエージェント向けの指示。

## プロジェクトの現状

- AIネイティブ次世代プログラミング言語。**M3 完了 = Phase 1 完了**（ARC・`str` / `list` / `dict`・`Result`・Effect の注釈・`wys explain`）。進捗は `docs/roadmap.md`、M3 の詳細は `docs/plans/m3.md`。
- 正となる文書は `docs/ai_native_language_requirements.md`（要件定義書）。迷ったらここに戻る。
- 言語名・拡張子は未定。文書中では「本言語」と呼ぶ。勝手に名前を付けない。
- 内部コードネームは `wys`（ユーザーが決定）。Rust クレートは `crates/wys_*`。これは正式名称ではない。

## ドキュメントのルール

- 文書は日本語で書く。技術用語は `docs/glossary.md` の表記に揃え、新しい用語を導入したら用語集にも追加する。
- **要件ID（LANG-001, AI-004, AC-001 など）は採番済みのものを変更・再利用しない。** 廃止する場合は「廃止」と明記して残す。新規要件は各カテゴリの末尾の番号に続けて追加する。
- 要件定義書の「12. 構文イメージ」は**非規範的**。確定構文として扱ったり、それを前提に仕様を書いたりしない。
- 設計判断は `docs/adr/` にADRとして記録する（手順は `docs/adr/README.md`。下の「レビューゲート」も参照）。要件定義書「10. 未決定事項」の項目を決めたら、ADRを作成し、ADR一覧の対応欄を更新する。
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
target/debug/wys run examples/hello.wys
target/debug/wys check --format json examples/type_error.wys
target/debug/wys dump-ir examples/fib.wys
target/debug/wys test examples/math_test.wys
target/debug/wys fmt --check examples/fib.wys
target/debug/wys build --release examples/fib.wys   # -O2
WYS_SANITIZE=address target/debug/wys run examples/fib.wys   # AddressSanitizer 付き
WYS_RC_STATS=1 target/debug/wys run examples/fib.wys         # 終了時に retain / release の回数を出す
target/debug/wys explain examples/fib.wys   # 型・絞り込み・ARC 操作の理由を JSON で（ADR-0017）
target/debug/wys explain E0201              # エラーコードの説明
benches/measure.sh                                          # ベンチ計測（倍率は書かない）
```

- ARC（retain / release）に関わる変更では、`scripts/ci.sh` の AddressSanitizer モードと生存オブジェクト数の検査（終了コード 102）が通ることを必ず確かめる。

- `examples/` の `.wys` は `wys fmt` で整形しておく（CI が `--check` で確かめる）。

- 新しいエラーコードを追加したら `docs/spec/error-codes.md` に追記する。コードは再利用しない。
- 言語機能を追加したら、`crates/wys_cli/tests/e2e.rs` に実行結果のテストを追加する。

## 開発の進め方（テスト先行の閉ループ）

1. **テストを先に書く。** 言語の挙動は `tests/conformance/` に `.wys` ケースとして追加する（書き方は `crates/wys_testharness/src/lib.rs` の先頭コメント）。
   - 実行結果: `lang/`。`# expect-stdout:` `# expect-exit:` `# expect-stderr-contains:`
   - 構文エラー: `syntax/`、型エラー: `types/`。`# expect-error: CODE 行:列`
   - 診断 JSON の形も固定したいもの: `diagnostics/`（insta スナップショット）
   - 複数ファイル（モジュール）: `modules/<ケース名>/main.wys` と、そこから import するファイル
2. 新しいケースが**失敗することを確認してから**実装する。
3. `.wys` の挙動は `scripts/ai-loop.sh <file>` で確かめる。診断があれば JSON が返るので、`code`・`span`・`hints` を読んで直す。
4. `./scripts/ci.sh` が通るまで作業を終えない。
5. スナップショットを意図して変えたときは `INSTA_UPDATE=always cargo test -p wys_testharness` で更新し、差分を確認してからコミットする。
6. 新しいエラーコードは `docs/spec/error-codes.md` に追記し、対応するケースを必ず作る（`every_error_code_has_a_case` テストが検査する）。

## レビューゲート

- ADR は AI が `Proposed` で起草し、`Accepted` にするのはユーザー。
- 作業はブランチで行い、`main` へのマージ・push・リポジトリ設定の変更はユーザーの指示を受けてから行う。
- **マージの前にコードレビューを確認する。** ユーザーにマージを依頼する前に、PR のレビュー・インラインコメント・会話のコメント（Codex などのボットを含む）を確認する。
  - 確認には `gh api repos/{owner}/{repo}/pulls/<N>/reviews`、`.../pulls/<N>/comments`、`.../issues/<N>/comments` を使う。ボットのレビューは PR を開いた後や Ready にした後に遅れて付くので、CI の完了を待ってから見る。
  - 指摘があれば、コードと照らして妥当か確かめ（できれば再現し）、妥当なものは直して push する。妥当でないものは理由を添えてユーザーに報告する。
  - 指摘とその扱いを報告してから、マージを依頼する。指摘がなければ「レビューの指摘なし」と明記する。

## 設計上の前提（要件定義書より）

- 優先順位は「言語の意味論・型・所有権・Effect・FFI境界」の確定が最優先。GPU機能やAI向け糖衣構文は後回し。
- AI関連機能（`agent`/`tool`/`llm`）はまず標準SDK・デコレータとして設計し、専用構文にはしない。
- セキュリティはRuntime強制が前提。LLM出力・ツール戻り値は信頼しない。プロンプトでの権限制御に依存しない。
- 実装言語はRust（ADR-0002 で決定）。ツールチェーンは `rust-toolchain.toml` で固定。
- 実行計画（マイルストーン・直近タスク）は `docs/roadmap.md`。
