# ロードマップ（実行計画）: AIネイティブ次世代プログラミング言語（Phase 0〜3）

## 進捗（2026-11-05 時点）

| 項目 | 状態 |
|---|---|
| 週1（基盤・ADR-0002/0003・ワークスペース・CI・診断スキーマ） | 完了。GitHub（private）で CI 成功 |
| 週2（PoC1: ソース → Native 実行） | 完了。ADR-0004/0008 Accepted |
| 週3（Conformance 48ケース・AI ループ・PoC2 実験・ADR-0005〜0007 草案） | 完了。ADR-0005〜0007 は設計レビュー（Fable）を経て Accepted |
| 週4（文法 v0.1・用語集・ベンチ骨格・出口判定） | 完了。**Phase 0 の出口条件（ADR-0002〜0008 が Accepted、PoC1 が CI で動く）を満たした** |
| M1（最小サブセット） | **完了**。AC-001 / AC-004 / AC-005 を確認（`docs/test-plan.md`）。CLI 名は `wys` |
| M2（型拡張・モジュール） | **完了**（出口判定は [docs/plans/m2.md](plans/m2.md)）。AC-002 / AC-003、LANG-001・003〜006、DX-001 合格。DX-005 は計測の仕組みまで（計測はユーザー） |
| M3（メモリ・stdlib・Phase 1 完了） | **完了**（出口判定は [docs/plans/m3.md](plans/m3.md)）。Phase 1 の完了条件、MEM-001 / MEM-002 / MEM-005、COMP-005、NFR-006 合格。ADR-0012〜0017 Accepted。Move 最適化・`fmt` の折り返し・`list` を経由する再帰レコードは M4 に持ち越し。DX-005 の計測はユーザー待ち |
| M4（CPython FFI・`async`） | **完了**（[docs/plans/m4.md](plans/m4.md)）。AC-010、INTEROP-001 / INTEROP-002、LANG-008 合格。ADR-0018〜0020 Accepted。PoC3 成功（[docs/poc/poc3-results.md](poc/poc3-results.md)）。M3 から持ち越した Move 最適化も完了。`fmt` の折り返し（#87）と `weak`・再帰レコード・`Unknown` のコンテナは M5 へ。DX-005 の計測はユーザー待ち |
| M5（AI SDK の核） | **完了**（[docs/plans/m5.md](plans/m5.md)）。AC-006〜AC-008、AI-001〜AI-006 を満たした（費用の上限は M6）。ADR-0021〜0023 Accepted、未決定事項 9 / 10 を決定。PoC4 成功（[docs/poc/poc4-results.md](poc/poc4-results.md)）。`fmt` の折り返し（#87）は M6 へ |

## Context

要件定義書（`docs/ai_native_language_requirements.md`）とドキュメント基盤はできたが、実装コードはなく、決定済みの設計判断は ADR-0001 だけ。この計画では、要件定義書の目的（型安全・Native実行・AIエージェントの安全な実行）を、**人間1名＋AIエージェント（Claude Code）** の体制で実現するための段階、順序、仕組みを定める。
前提として確認したこと: 開発機は macOS arm64。Rust と LLVM は未インストールで、clang・Python 3.14・uv はある。git は未初期化。
ユーザーの回答: 体制は1人＋AI、Phase 1 の最初の対象環境は macOS arm64（Linux x86_64 は後でCIに追加）。

## 方針（要点）

- 範囲を極端に絞り、**AIが検証ループを回せる仕組み（診断JSON＋Conformanceテスト）を最初に作る**。
- クリティカルパスは4つ: ①実装言語（推奨 Rust）②対象環境（macOS arm64）③Nativeバックエンドの接続方式（PoC1で決める）④メモリモデル（PoC2で決める）。
- 守る制約: Python完全互換を約束しない／性能倍率を書かない／AI機能はSDK・デコレータから始める／GPU・WasmはPhase 3／要件IDは変更しない／未決定事項は「推奨」として示し、ADRで決める。

## 役割分担とレビューゲート

- **ユーザー:** ADR を Accepted にする判定、コードネームの決定、PoC 後の方針転換の判断、各MSの出口判定。
- **Claude Code:** 実装、テストを先に書くこと、ADR草案（Proposed）の起草、ドキュメント更新。**ADRを自分で Accepted にしない**（CLAUDE.md に明記する）。
- 作業単位は1タスク＝ブランチ1本: テストを先に書く → 実装 → Conformance が通る → ユーザーレビュー。ユーザーはテストとスナップショットの差分だけを見れば判断できるようにする。

## マイルストーン

| MS | 内容 | 主な完了条件（要件ID） | 依存 |
|---|---|---|---|
| **M0** 基盤・初期ADR（1〜2週） | git初期化、Rust toolchain、Cargoワークスペース骨格、CI（macOS arm64）、診断JSONスキーマ v0、Conformanceランナー骨格、ADR-0002（Rust）、ADR-0003（対象環境＝未決定7） | CIで fmt/clippy/test が通る | — |
| **PoC1** コンパイラ（2〜3週） | 整数・関数・`print` だけの最小言語で、Lexer→Parser→型検査→IR→Native。バックエンド比較 → ADR-0004、Runtimeリンク方式 → ADR-0008（未決定6） | 外部インタプリタなしでバイナリが起動する（AC-004の先行確認） | M0 |
| **PoC2** 型/所有権（2〜3週、PoC1と一部並行） | `docs/design/memory-model.md`、Union絞り込みのミニ型検査器 → ADR-0005（メモリ＝未決定4）、0006（構文＝未決定3）、0007（構造的/名目的＝未決定2） | MEM-002/005 の保証範囲を文章で定義できる | M0 |
| **Phase 0 出口** | ADR-0002〜0008 が Accepted になり、PoC1 がCIで動く | | |
| **M1** 最小サブセット（4〜6週） | `build/run/check`、診断JSON実装、Conformanceテスト、`grammar.ebnf` v0.1 | AC-001, AC-004, AC-005, LANG-001/002/007, COMP-001〜003 | PoC1, PoC2 |
| **M2** 型拡張・モジュール（6〜8週） | Union/None絞り込み、Generics（単相化）、`match`の網羅性検査、レコード型、`Unknown`、モジュール・可視性、`fmt`/`test`、PoC5（AI修正ループ）の成功率計測 、CIに Linux x86_64 を追加（M1 完了後、Conformance が安定してから） | AC-002, AC-003, LANG-003〜006, DX-005の初期版 | M1 |
| **M3** メモリ・stdlib・Phase 1 完了（6〜8週） | ARC＋Move、`str/list/dict`、Result相当、Debug/Release、`explain` 初期版、ベンチ基盤、Effect基礎ADR（未決定5） | Phase 1 完了条件、MEM-001/002/005、COMP-005、NFR-006 | M2 |
| **M4** CPython FFI・async | PoC3 → ADR（未決定8）、例外→Result変換 | AC-010, INTEROP-001/002, LANG-008 | M3 |
| **M5** AI SDK核 | PoC4 → `docs/design/ai-sdk.md`、ADR（未決定9/10）、型付きLLM API、スキーマ検証、`@tool`/`@agent`（SDK）、Capability・承認・予算Runtime | AC-006〜008, AI-001〜006 | M4 |
| **M6** Effect・トレース・LSP | Effect推論、トレース、モック/再生、評価ケース、LSP初期版 | AC-009、Phase 2 完了条件 | M5 |
| **M7〜M9** Phase 3 | SIMD・並列API → Wasm・`.d.ts`・JSブリッジ → MLIR/GPU・パッケージ管理（未決定11/12） | Phase 3 完了条件 | M6 |

## 未決定事項の決定順と推奨案（未確定。ADRで決める）

| 順 | 事項 | 決める時期 | 推奨案 |
|---|---|---|---|
| 1 | 実装言語 | M0 | Rust（pyo3や LLVM バインディングなど周辺ライブラリが成熟していて、AIも書きやすい） |
| 2 | 7. 対象OS/CPU | M0 | macOS arm64 から始め、M2 で Linux x86_64 をCIに追加（遅くとも Phase 1 の出口まで）。Windows は Phase 3 以降 |
| 3 | 6. Runtime構成 | PoC1直後 | Rust製Runtimeを staticlib として静的リンクし、単独バイナリにする |
| 4 | バックエンド接続方式 | PoC1直後 | 比較順: (b) `.ll` テキストを出力して clang に渡す → (a) inkwell → (c) Cranelift。Backend トレイトで差し替え可能にする |
| 5 | 4. メモリモデル | PoC2直後（IRを確定する前） | コンパイラが挿入するARC＋ローカルのMove。MVPでは循環参照を回収しない（弱参照で回避）と明記し、ORCはM7以降で評価 |
| 6 | 3. 構文 | PoC2直後 | `T \| None`、`match` による絞り込み、`def f[T](x: T)`、`Result[T, E]`＋伝搬糖衣 |
| 7 | 2. 構造的/名目的 | PoC2直後 | レコード型とProtocolは構造的、`class`/`enum` は名目的 |
| 8 | 1. 可視性 | M2 | 明示的な `pub` 相当。クレート接頭辞用の**内部コードネームは人間が決める**（正式名称ではないとADRに明記） |
| 9 | 5. Effect | M3（注釈）／M6（推論） | Phase 1 は注釈だけ。型とIRにEffectの置き場所を先に確保する |
| 10〜12 | 8〜12 | M4以降 | PoC3/PoC4 の結果で決める |

## リポジトリ構成（提案）

- `Cargo.toml`（workspace）、`rust-toolchain.toml`、`.github/workflows/ci.yml`
- `crates/<cn>_{span,diagnostics,lexer,ast,parser,typeck,ir,lower,backend,backend_llvm,runtime,driver,cli,testharness}`（`fmt`/`ffi_python`/`lsp`/`effects` は後から追加）
  - `ast` と `diagnostics` は独立クレートにして、fmt・LSP・テストハーネスから使えるようにする
- `tests/conformance/{lang,types,diagnostics,memory,security}`: 期待値はソース内コメントで宣言し、`check --format json` の出力と突合する。insta でスナップショットを取る
- `benches/`（M3〜。結果は機種情報付きで記録し、倍率は書かない）、`examples/`、`scripts/`
- `docs/spec/`（grammar.ebnf, diagnostics-schema.json など）、`docs/design/`（compiler, ir, memory-model, ai-sdk）、`docs/test-plan.md`

## M1 の言語サブセット

- **入れる:** インデントブロック、`int(i64)/float/bool/str`（strはリテラルのみ）、`def`（型注釈は必須）、代入とローカル推論、`if/elif/else`、`while`、`return`、再帰、`print`、`exit`、暗黙変換の禁止
- **外す:** `T|None`・Generics・レコード・`Unknown`・`import`・`fmt`/`test`（M2）、`for`・`list/dict`・ヒープ/ARC・Result（M3）、`async`（M4）、デコレータ（M5）
- 理由: ヒープを外すことで、PoC2 の結論を待たずにパイプライン全体（AC-001/004/005）を通せる

## 品質の仕組み

| 仕組み | 導入時期 |
|---|---|
| CI（fmt/clippy/test） | M0（macOS arm64）→ M2（Linux x86_64 を追加） |
| 診断JSONスキーマ（code, severity, span, message, cause, hints, related, schema_version） | M0（v0）→ M1（実装） |
| Conformanceランナー＋insta スナップショット | M0（骨格）→ M1（本稼働） |
| **AI閉ループの自家利用**: CLAUDE.md に「テストを先に書く → `check --format json` を読んで直す → `cargo test` が通るまで終わらない」を明記。`scripts/ai-loop.sh`。`.claude/settings.json` のフック（編集後に `cargo check`）は内容を提示し、承認を得てから導入する | M1から段階的に |
| パーサの Fuzz | M2（任意） |
| ベンチ基盤、メモリ境界テスト | M3 |
| セキュリティテスト（AC-006〜008） | M5 |

## リスクと方針転換の基準

| リスク | 判断時期 | 転換基準 → 代替案 |
|---|---|---|
| LLVM連携の保守負荷 | PoC1 | inkwell の環境構築とCIが2人日を超える、またはCIが15分を超える → `.ll`＋clang でMVPを進める。それも不可なら Cranelift |
| メモリモデルPoCの失敗 | PoC2 | 保証範囲を文章で定義できない、または実装に4週間以上かかる → ARCのみ（Moveは Phase 3）、または簡易GC。どちらでも「GCなし」は約束しない |
| 範囲の肥大化 | 各MSの終了時 | 目安期間の1.5倍を超えたら、そのMSの Should 要件を次のMSに繰り越す |
| CPython FFI | M4 | 往復呼び出しと例外伝搬が2週間で動かない → ゼロコピーを Phase 3 へ回し、対応する Python のバージョンを1つに固定 |
| AI SDK の強制方式 | M5 | Capability の付与がプロンプト依存になる → Runtime 強制だけで AC-007 を満たす |
| 診断JSONが頻繁に変わる | M2 | `schema_version` を入れ、code と span だけを突合する比較モードを用意する |

## 直近4週間のタスク（承認後に着手）

**週1 基盤と意思決定**
1. `git init`、`.gitignore`、初回コミット
2. rustup の導入、`rust-toolchain.toml`
3. ADR-0002（Rust）を起草 → ユーザーレビュー → Accepted
4. ADR-0003（対象環境: macOS arm64、Linux はM2）
5. **内部コードネームをユーザーが決める**（クレート接頭辞に使う）
6. Cargo ワークスペースの骨格（空クレート12個）
7. CI（GitHub Actions、macOS arm64）。リモートがなければ `scripts/ci.sh` で代用する
8. 診断JSONスキーマ v0、`Diagnostic` 構造体、スキーマ検証テスト

**週2 PoC1**
9. Lexer（INDENT/DEDENT）
10. AST と Parser
11. 型検査
12. IRとそのダンプ・検証器
13. バックエンド比較（各1日まで）→ ADR-0004
14. Runtime staticlib → ADR-0008
15. `build/run/check --format json` で `examples/hello` が動く

**週3 PoC2と品質ループ**
16. Conformance ランナーと insta
17. `scripts/ai-loop.sh`、CLAUDE.md への開発手順の追記（フックは提示して承認を得てから）
18. `memory-model.md` の草案
19. Union 絞り込みのミニ型検査器
20. ADR-0005〜0007 を起草

**週4 Phase 0 の出口判定**
21. ユーザーが ADR を Accepted にするか判定し、転換基準に当たるか確認する
22. `grammar.ebnf` v0.1 を書き、索引と用語集を更新する
23. M1 の Conformance テストを先に書き、赤から緑にする
24. ベンチ計測スクリプトの骨格

## 更新する既存ファイル

- `docs/adr/README.md`（ADR一覧と未決定事項の対応表）
- `docs/README.md`（成果物の状態とパス）
- `docs/glossary.md`（コードネーム、Backend トレイト、Conformance ランナー、閉ループなど）
- `CLAUDE.md`（開発手順とビルド手順）
- `README.md`（ディレクトリ構成）
- 新規ADRは `docs/adr/template.md` から作成する

## 検証

- 各マイルストーンの完了は、対応する AC-xxx の Conformance テストが通り、CIが成功することで判定する
- Phase 0 の出口: `cargo test` が通り、`<cli> build examples/hello` の実行ファイルが stdout と終了コードの期待値に一致し、`check --format json` の出力がスキーマ検証を通る
- ADR は Proposed → ユーザーレビュー → Accepted の順で進め、AIだけで Accepted にしない
