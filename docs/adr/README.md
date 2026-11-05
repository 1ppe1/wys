# ADR（Architecture Decision Records）

設計上の重要な判断を、理由・代替案・影響とともに記録する。

## 運用ルール

1. [template.md](template.md) をコピーし、`NNNN-短い名前.md`（例：`0002-memory-model.md`）として作成する。番号は連番で再利用しない。
2. 作成時の状態は `Proposed`。プロジェクトオーナー（人間）がレビューして合意したら `Accepted` にする。**AIエージェントは `Accepted` に変更しない。**
3. 一度 `Accepted` になったADRは書き換えない。方針を変える場合は新しいADRを作り、古い方を `Superseded by ADR-NNNN` にする。
4. 番号はロードマップで予約した番号を使ってよい（例: 0005〜0007 は PoC2 用に予約済み）。
5. ADRを追加・変更したら、下の一覧と「未決定事項との対応」を更新する。

## ADR一覧

| ID | タイトル | 状態 | 日付 |
|---|---|---|---|
| [0001](0001-record-architecture-decisions.md) | 設計判断をADRで記録する | Accepted | 2026-11-04 |
| [0002](0002-implementation-language-rust.md) | 実装言語に Rust を採用する | Accepted | 2026-11-04 |
| [0003](0003-mvp-target-platform.md) | MVP の対象環境を macOS arm64 とする | Accepted | 2026-11-04 |
| [0004](0004-native-backend-llvm-ir-text.md) | Native バックエンドは LLVM IR テキストを clang に渡す | Accepted | 2026-11-04 |
| [0005](0005-memory-model-arc-move.md) | メモリモデルはコンパイラ挿入の ARC と Move 最適化 | Accepted | 2026-11-04 |
| [0006](0006-union-null-generics-error-syntax.md) | Union / None / Generics / エラー処理の構文 | Accepted | 2026-11-04 |
| [0007](0007-structural-vs-nominal-typing.md) | 構造的型付けと名目的型付けの境界 | Accepted | 2026-11-04 |
| [0008](0008-runtime-static-linking.md) | ランタイムは Rust staticlib として静的リンクする | Accepted | 2026-11-04 |
| [0009](0009-m2-interim-value-representation.md) | M2 の暫定値表現（ヒープを使わない） | Superseded by ADR-0012 | 2026-11-04 |
| [0010](0010-modules-and-visibility.md) | モジュールと可視性 | Accepted | 2026-11-04 |
| [0011](0011-unknown-and-validation.md) | `Unknown` の実行時表現と検証 API | Accepted | 2026-11-04 |
| [0012](0012-heap-objects-and-arc-pass.md) | ヒープオブジェクトと、IR パスによる ARC | Accepted | 2026-11-04 |
| [0013](0013-result-binding-patterns-try-panic.md) | `Result[T, E]`、束縛パターン、`?`、パニックの仕様 | Accepted | 2026-11-04 |
| [0014](0014-effects-phase1-annotations.md) | Effect の基礎（Phase 1 は注釈だけ） | Accepted | 2026-11-04 |
| [0015](0015-dict-keys-order-mutation.md) | `dict[K, V]` のキー、反復の順序、反復中の変更 | Accepted | 2026-11-04 |
| [0016](0016-record-field-narrowing-and-structural-values.md) | 不変レコードのフィールドの絞り込みと、構造を持つ値の `print` / `==` | Accepted | 2026-11-04 |
| [0017](0017-explain-report-and-arc-reasons.md) | `wys explain` の報告と、ARC 操作の理由 | Accepted | 2026-11-04 |

## 未決定事項との対応

要件定義書「10. 未決定事項」をADRとして決めていくための管理表。「決定が必要な時期」はフェーズ計画から見た目安。

| # | 未決定事項 | 決定が必要な時期 | 関連要件 | ADR |
|---|---|---|---|---|
| 1 | 言語名、拡張子、パッケージ名、ファイル単位の可視性規則 | Phase 1（可視性規則）／公開前（名称） | LANG-001 | [0010](0010-modules-and-visibility.md)（可視性のみ） |
| 2 | 構造的型付けと名目的型付けを適用する境界 | Phase 1 | LANG-005 | [0007](0007-structural-vs-nominal-typing.md) |
| 3 | Union/Null、Generics、エラー処理の正確な構文 | Phase 1 | LANG-003, 004, 008 | [0006](0006-union-null-generics-error-syntax.md) |
| 4 | ARC/ORC相当の方式、循環参照回収、Ownership/Borrowの詳細 | Phase 1 | MEM-001〜005 | [0005](0005-memory-model-arc-move.md) |
| 5 | Effectの表現形式（型シグネチャ、注釈、推論の組合せ） | Phase 1（基礎）／Phase 2 | AI-009 | [0014](0014-effects-phase1-annotations.md)（Phase 1 の注釈。推論は M6） |
| 6 | Runtimeの構成（静的リンク、動的リンク、最小バイナリ） | Phase 1 | COMP-002, AC-004 | [0008](0008-runtime-static-linking.md) |
| 7 | MVPの対象OS/CPU、初期標準ライブラリ範囲 | Phase 1 | NFR-006 | [0003](0003-mvp-target-platform.md)（OS/CPU のみ） |
| 8 | Python連携方式と対応バージョン、GIL/スレッド境界 | Phase 2 | INTEROP-001〜003 | — |
| 9 | AI SDKを標準添付するか別パッケージとするか | Phase 2 | AI-001〜010 | — |
| 10 | モデルプロバイダ抽象化、費用上限の算定方式、秘密情報保管、トレースのプライバシー方針 | Phase 2 | AI-006, AI-007, NFR-005 | — |
| 11 | ライセンス、ガバナンス、パッケージレジストリ運用 | 公開前 | INTEROP-004 | — |
| 12 | 具体的な性能SLOとサポート期間 | ベンチマーク・利用者検証後 | NFR-002 | — |
