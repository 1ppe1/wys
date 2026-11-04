# ドキュメント索引

## 既存ドキュメント

| ドキュメント | 状態 | 内容 |
|---|---|---|
| [ai_native_language_requirements.md](ai_native_language_requirements.md) | Draft v0.1 | 要件定義書。全ての設計成果物の起点 |
| [roadmap.md](roadmap.md) | 承認済み計画 | Phase 0〜3 のマイルストーン、決定順、直近タスク |
| [spec/diagnostics-schema.json](spec/diagnostics-schema.json) | v0（草案） | 構造化診断JSONのスキーマ（DX-002, AC-005）。実装は `crates/wys_diagnostics` で、テストでスキーマ検証する |
| [spec/error-codes.md](spec/error-codes.md) | PoC1 | 診断のエラーコード一覧と実行時エラー |
| [design/ir.md](design/ir.md) | 草案 | 共通IRの構造・値のルール・テキスト形式 |
| [glossary.md](glossary.md) | Draft | 用語集。文書間で表記を揃えるための基準 |
| [adr/README.md](adr/README.md) | 運用中 | ADRの書き方、一覧、未決定事項との対応 |

## 作成予定の設計成果物

要件定義書「13. 次の設計成果物」に対応する。作成したら状態と置き場所を更新する。

| # | 成果物 | 予定パス | 主な関連要件 | 前提 | 状態 |
|---|---|---|---|---|---|
| 1 | 言語仕様 v0.1（EBNF、型規則、エラー処理、モジュール） | `docs/spec/` | LANG-001〜009 | 未決定事項 2, 3 のADR | 未着手 |
| 2 | メモリ/Effect ADR | `docs/adr/` | MEM-001〜006, AI-009 | 型/所有権PoC | 未着手 |
| 3 | コンパイラ設計（AST/IR、診断JSONスキーマ、バックエンド境界） | `docs/design/compiler.md` | COMP-001〜008, DX-002 | コンパイラPoC | 未着手 |
| 4 | AI SDK/API仕様（型付き入出力、Capability、承認、監査イベント） | `docs/design/ai-sdk.md` | AI-001〜010 | 未決定事項 9, 10 のADR | 未着手 |
| 5 | MVPテスト計画（Conformance、セキュリティ、ベンチマーク） | `docs/test-plan.md` | AC-001〜010, NFR-002 | 1〜4 | 未着手 |

## 推奨する作成順

1. 未決定事項のうちPhase 1に必要なもの（言語の意味論・型・所有権・Effect）をADRで決める。
2. 言語仕様 v0.1 とメモリ/Effect ADR を並行して作る。
3. コンパイラPoCの結果を踏まえてコンパイラ設計を書く。
4. Phase 2 の前に AI SDK/API仕様を書く。
5. 受け入れ基準（AC-xxx）をテスト計画に落とし込む。
