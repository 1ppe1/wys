# ADR-0004: Native バックエンドは LLVM IR テキストを clang に渡す方式で始める

- **状態:** Accepted
- **日付:** 2026-11-04
- **関連:** COMP-002, COMP-004, COMP-001, ADR-0003, ロードマップ「リスクと方針転換の基準」

## 背景

COMP-004 は LLVM を中心とした Native バックエンドを求めている。LLVM との接続方式によって、環境構築・CI・保守の負荷が大きく変わる。
ロードマップでは PoC1 で次の3案を比較することにしていた: (a) inkwell（LLVM の C API を Rust から使う）、(b) LLVM IR テキストを出力して clang に渡す、(c) Cranelift。

## PoC1 の結果

- (b) を実装した（`crates/wys_backend` の `LlvmTextBackend`）。開発機に最初からある Apple clang 21 だけで動き、LLVM の追加インストールは要らなかった。
- 整数・真偽値・文字列定数・関数・再帰・`if`/`while`・短絡評価・オーバーフロー検査（`llvm.s*.with.overflow`）を実装し、e2e テスト11件が通った。
- 生成した実行ファイル（Hello World、Debug ランタイム込み）は約 1.4MB で、動的リンクは OS 標準の `libSystem` だけ。
- **(a) inkwell と (c) Cranelift はまだ試していない。** (a) には `brew install llvm` が必要。

## 選択肢

### 案A: inkwell（in-process で LLVM を使う）

- 利点: 型付き API で IR を組み立てられる。clang を別プロセスで起動しない。JIT やパスの細かい制御ができる。
- 欠点: LLVM 本体（数百MB〜）が必要で、inkwell が LLVM のバージョンに追従する必要がある。CI のビルド時間が延びる。

### 案B: LLVM IR テキスト → clang（PoC1 で実装済み）

- 利点: 依存が clang だけ。生成物（`.ll`）を人間・AIが読めるので、デバッグしやすい（`dump-llvm`）。
- 欠点: テキストの文法の誤りは clang 実行時まで検出できない（IR 検証器と e2e テストで補う）。clang のバージョンによって IR の文法差が出る可能性がある。プロセスの起動コストがかかる。

### 案C: Cranelift

- 利点: 純 Rust で、外部ツールが要らない。コンパイルが速い。
- 欠点: 最適化が LLVM より弱い。将来の MLIR/GPU 経路（COMP-007）につながらない。

## 決定

**案B で MVP（Phase 1）を進める。**`Backend` トレイトで差し替えられるようにしてあるので、後から案A・案Cを追加できる。

## 影響

- ビルドには `clang`（LLVM IR を受け付けるもの）が必要。`WYS_CC` で差し替えられる。
- 見直し条件: (1) Release 最適化・JIT・コンパイル速度が問題になったとき（M3 以降）、(2) Linux 追加時（M2）に clang のバージョン差で問題が出たとき。そのときに案Aを PoC で比較する。
