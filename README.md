# AIネイティブ次世代プログラミング言語（仮称）

人間とAIの双方が安全に開発できる高性能なプログラミング言語を構築するプロジェクト。
Pythonの簡潔さ、TypeScriptの型表現力、Nim/Codon/Mojoのネイティブ性能を参考に、
**型安全・ネイティブ実行・AIエージェントの安全な実行**を一つの言語体験として提供することを目指す。

> **現在のステータス:** M2 完了（型の拡張・モジュール・`test` / `fmt`）。次は M3（メモリ管理と標準ライブラリ）。内部コードネームは `wys`（正式名称ではない）。

## 試し方

macOS arm64 または Linux x86_64 で、Rust（`rust-toolchain.toml` で固定）と clang が必要（CI は両方で実行）。

```sh
cargo build
target/debug/wys run examples/fib.wys
target/debug/wys check --format json examples/type_error.wys
target/debug/wys test examples/math_test.wys   # test_* 関数を実行（main 不要）
target/debug/wys fmt examples/fib.wys          # 整形（--check で確認だけ）
```

現在の言語機能（M2 時点）:

- 型: `int` / `bool` / `str`（定数と引数）、`None`、Union（`int | None`）、レコード（`type P = {x: int}`、構造的・不変）、Generics（`def f[T]`、`type Pair[A, B]`）、`Unknown`（外部入力）
- 文: 関数、`if` / `elif` / `else`、`while`、`match` / `case`（網羅性検査）、`assert`
- モジュール: `import` / `from ... import`、`pub`
- ツール: `check`（JSON 診断）、`build`、`run`、`test`、`fmt`、`dump-ir` / `dump-llvm`
- まだないもの: `list` / `dict`、文字列の連結、`for`、クラス（M3 以降）

## ドキュメント

| ドキュメント | 内容 |
|---|---|
| [docs/README.md](docs/README.md) | ドキュメント全体の索引と作成予定の成果物 |
| [docs/ai_native_language_requirements.md](docs/ai_native_language_requirements.md) | 要件定義書（目的・機能要件・非機能要件・フェーズ・受け入れ基準） |
| [docs/glossary.md](docs/glossary.md) | 用語集 |
| [docs/adr/README.md](docs/adr/README.md) | 設計判断記録（ADR）の一覧と未決定事項 |

## ロードマップ（概要）

| フェーズ | 内容 | 完了条件 |
|---|---|---|
| Phase 1 | 言語・ネイティブMVP | 型エラーを事前検出でき、単独のNative実行ファイルを生成・テストできる |
| Phase 2 | AIネイティブMVP | 外部ツールを使うエージェントで、出力型エラー・権限拒否・予算超過を検証できる |
| Phase 3 | 高性能・広域展開 | 数値処理をCPU/GPU等に明示的に配置し、実アプリで性能・互換性を比較できる |

詳細は要件定義書の「7. 優先順位とフェーズ」を参照。

## ディレクトリ構成

```text
.
├── README.md          # このファイル
├── CLAUDE.md          # AIエージェント（Claude Code）向けのプロジェクト指示
├── .claude/           # Claude Code のプロジェクト設定
├── Cargo.toml         # Rust ワークスペース
├── rust-toolchain.toml
├── crates/wys_*/      # コンパイラ・ランタイム・CLI（wys は内部コードネーム）
├── examples/          # サンプルプログラム（.wys）
├── scripts/ci.sh      # CI と同じチェック
└── docs/
    ├── README.md      # ドキュメント索引
    ├── ai_native_language_requirements.md
    ├── glossary.md
    ├── roadmap.md     # 実行計画
    └── adr/           # Architecture Decision Records
```
