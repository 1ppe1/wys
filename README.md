# AIネイティブ次世代プログラミング言語（仮称）

人間とAIの双方が安全に開発できる高性能なプログラミング言語を構築するプロジェクト。
Pythonの簡潔さ、TypeScriptの型表現力、Nim/Codon/Mojoのネイティブ性能を参考に、
**型安全・ネイティブ実行・AIエージェントの安全な実行**を一つの言語体験として提供することを目指す。

> **現在のステータス:** 構想・要件定義フェーズ（Draft v0.1）。実装コードはまだ存在しない。

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
└── docs/
    ├── README.md      # ドキュメント索引
    ├── ai_native_language_requirements.md
    ├── glossary.md
    └── adr/           # Architecture Decision Records
```
