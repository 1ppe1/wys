# ドキュメント索引

## 既存ドキュメント

| ドキュメント | 状態 | 内容 |
|---|---|---|
| [ai_native_language_requirements.md](ai_native_language_requirements.md) | Draft v0.1 | 要件定義書。全ての設計成果物の起点 |
| [plans/m3.md](plans/m3.md) | 完了 | M3 のタスク分解（T0〜T11）、ヒープと ARC の方針、新しい ADR と診断コードの帯、Phase 1 の出口判定 |
| [plans/m4.md](plans/m4.md) | 承認済み計画 | M4 のタスク分解（T0〜T12）、CPython FFI（埋め込み）と `async`、PoC3 の判定基準、M4a / M4b の分割基準 |
| [plans/m2.md](plans/m2.md) | 完了 | M2 のタスク分解（T0〜T10）、暫定値表現、新しい ADR と診断コードの帯 |
| [roadmap.md](roadmap.md) | 承認済み計画 | Phase 0〜3 のマイルストーン、決定順、直近タスク |
| [spec/diagnostics-schema.json](spec/diagnostics-schema.json) | v0（草案） | 構造化診断JSONのスキーマ（DX-002, AC-005）。実装は `crates/wys_diagnostics` で、テストでスキーマ検証する |
| [spec/explain-schema.json](spec/explain-schema.json) | v0（草案） | `wys explain` の説明レポートのスキーマ（DX-004、ADR-0017）。実装は `crates/wys_explain` で、全 Conformance ケースの出力をスキーマ検証する |
| [spec/grammar.ebnf](spec/grammar.ebnf) | v0.1 | 文法（PoC1 / M1 サブセット）とレイアウト規則 |
| [spec/error-codes.md](spec/error-codes.md) | PoC1 | 診断のエラーコード一覧と実行時エラー |
| [design/ir.md](design/ir.md) | 草案 | 共通IRの構造・値のルール・テキスト形式 |
| [design/memory-model.md](design/memory-model.md) | 草案 | メモリモデル（ARC + Move）、保証範囲、Union の値表現 |
| [poc/poc5-results.md](poc/poc5-results.md) | 準備済み・未計測 | PoC5: AI 修正ループの成功率（DX-005） |
| [glossary.md](glossary.md) | Draft | 用語集。文書間で表記を揃えるための基準 |
| [adr/README.md](adr/README.md) | 運用中 | ADRの書き方、一覧、未決定事項との対応 |

## 作成予定の設計成果物

要件定義書「13. 次の設計成果物」に対応する。作成したら状態と置き場所を更新する。

| # | 成果物 | 予定パス | 主な関連要件 | 前提 | 状態 |
|---|---|---|---|---|---|
| 1 | 言語仕様 v0.1（EBNF、型規則、エラー処理、モジュール） | `docs/spec/` | LANG-001〜009 | 未決定事項 2, 3 のADR | 未着手 |
| 2 | メモリ/Effect ADR | `docs/adr/` | MEM-001〜006, AI-009 | 型/所有権PoC | メモリは ADR-0005・ADR-0012、Effect は ADR-0014（Phase 1 は注釈）で Accepted |
| 3 | コンパイラ設計（AST/IR、診断JSONスキーマ、バックエンド境界） | `docs/design/compiler.md` | COMP-001〜008, DX-002 | コンパイラPoC | 未着手 |
| 4 | AI SDK/API仕様（型付き入出力、Capability、承認、監査イベント） | `docs/design/ai-sdk.md` | AI-001〜010 | 未決定事項 9, 10 のADR | 未着手 |
| 5 | MVPテスト計画（Conformance、セキュリティ、ベンチマーク） | [`docs/test-plan.md`](test-plan.md) | AC-001〜010, NFR-002 | 1〜4 | v0.1（M1 の受け入れ確認まで） |

## 推奨する作成順

1. 未決定事項のうちPhase 1に必要なもの（言語の意味論・型・所有権・Effect）をADRで決める。
2. 言語仕様 v0.1 とメモリ/Effect ADR を並行して作る。
3. コンパイラPoCの結果を踏まえてコンパイラ設計を書く。
4. Phase 2 の前に AI SDK/API仕様を書く。
5. 受け入れ基準（AC-xxx）をテスト計画に落とし込む。
