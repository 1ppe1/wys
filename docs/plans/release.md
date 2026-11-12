# 公開準備（Phase 2 リリース）実行計画（承認済み: 2026-11-11）

Fable（計画）が作成し、ユーザーが承認した。承認時の決定: 正式名称は承認時は「新名称で段階改名」としたが、2026-11-11 にユーザーが **`wys` を正式名称にする**（ADR-0027 案A。改名しない）と決め直した、ライセンスは **MIT OR Apache-2.0**、著作権者は**ユーザー個人**（個人作品）、コミット履歴の著者メールは**公開の直前に 1 回だけ noreply に書き換える方針**（実行前に対象の一覧を提示してユーザーが確認する）、ほかの論点は推奨案のとおり。範囲は未決定事項 1（名称）/ 11（ライセンス・ガバナンス。レジストリは Phase 3）/ 12 のサポート期間、配布物 0.1.0、利用者向け文書、ベンチの公開（NFR-002）、セキュリティ（NFR-001 / NFR-005）、公開前の点検と公開の判断。issue は #71（名称）、#72（ライセンス・ガバナンス・貢献）、#73（版と安定性）、#74（インストールと配布）、#75（利用者向け文書）、#76（ベンチの公開）、#77（セキュリティ）、#78（公開の判断。ユーザーが判定する）、#135（`wys check` の分岐の合流の二乗）、#59（PoC5。ユーザー実行）。

## 現状（計画の前提）

- リポジトリは private。`LICENSE` / `CONTRIBUTING.md` / `CODE_OF_CONDUCT.md` / `SECURITY.md` / `CHANGELOG.md` はない。`Cargo.toml` は `version = "0.0.0"`、`license = "UNLICENSED"`、`publish = false`。`wys --version` はない。
- CI は手元の `./scripts/ci.sh` と、Docker の Linux x86_64（エミュレーションで数時間）。GitHub Actions は使えない。
- `wys build` はランタイム（`libwys_runtime.a`、Python を使うなら `libwys_runtime_python.a`）を `WYS_RUNTIME_LIB` → 実行ファイルの隣 → その親の順で探す。リンクに `clang` が要る（ADR-0004）。`llm_generate` を使うプログラムにはコンパイラの絶対パスが埋め込まれる（proxy の探索）。
- 改名の表面積: Rust のソースに `wys` が約 1,200 回（クレート名、ランタイムの C ABI シンボル、`WYS_*` の環境変数 26 種、使い方の文面）、`.wys` ファイル 514 個、docs に約 650 回。
- ベンチは `benches/measure.sh` / `check_scaling.py` / `lsp_latency.py` があるが、公開の文書はない。`README.md` は M2 の時点のまま。
- コミット 302 件のうち 206 件の著者メールが勤務先ドメインの個人アドレス。

## 方針

- **公開は出口条件を満たしてから**（D6）。ただし LICENSE・名称・SECURITY.md・履歴の点検（T16）が済めば、配布物（T17）より前にリポジトリを公開してもよい。
- **名前に依存しない作業を先に進める。** #135、`--version`、配布の仕組み（名前は変数 1 か所）、文書の骨格（名称は「本言語」のまま）、ベンチの文書、セキュリティの見直しは名称を待たない。
- **版は SemVer 0.y.z と安定性の段階表**（D4）。最初のタグは `v0.1.0`。サポートは最新の 0.y だけ。対象環境は Tier 1 macOS arm64、Tier 2 Linux x86_64、Windows は非対応。Python は 3.14 のみ。性能 SLO は未決のまま明記する。
- **配布は GitHub Releases のみ**（D5）。アーカイブは `bin/` + `lib/` + LICENSE + README、`SHA256SUMS`、署名付きタグ、macOS は ad-hoc 署名まで、`install.sh` は SHA-256 を検証してから展開する。ランタイムの探索順に `<exe>/../lib/` を足す。Homebrew とランタイムの埋め込みは公開後。
- **ガバナンスは単独メンテナ + 既存の ADR 手続き、DCO、Contributor Covenant 2.1、脆弱性の窓口は GitHub の private vulnerability reporting**（D3。メールの窓口は置かない、2026-11-11 にユーザーが決めた）。
- **新しい言語機能・エラーコードは入れない。**
- 性能は絶対値と機種情報だけを書き、倍率は書かない（要件定義書 5.1）。CI に倍率の表現の検査を足す。

## タスク（1 タスク = 1 ブランチ = 1 PR、テストを先に書く）

「前提の決定」が空のタスクはすぐ始められる。

| # | 内容 | 規模 | 依存 | 前提の決定 | テスト・検証 |
|---|---|---|---|---|---|
| T0 | 本計画、`docs/roadmap.md` と `docs/README.md` の更新。#59 をこのマイルストーンに移す | S | — | 計画の承認 | 索引とリンク |
| T1 | **#135 分岐の合流を線形に**（issue のコメントの計画: `Env` に取り消し記録、`mark` / `delta_since` / `rollback`、合流は差分のキーだけ）。ADR 不要 | M | — | — | 先に Conformance 8 本が今の実装で通ることを確かめてから置き換える。旧 `join` を参照実装にした単体テスト。仕事量のカウンタで線形性を決定的に検査 |
| T2 | **ADR-0027（名称・拡張子・パッケージ名・改名の範囲）の起草**: 改名の表面積の目録、段階改名の手順、ユーザーが行う衝突調査（crates.io / PyPI / npm / Homebrew / GitHub / ドメイン / 既存の拡張子）の記録欄、「拡張子 = コマンド名 = パッケージ名」の原則。候補名は書かない | S | — | 承認に D1（名前） | 文書レビュー |
| T3 | **ADR-0028（ライセンス・ガバナンス・貢献・脆弱性報告）の起草** + 依存クレートのライセンスの目録（`docs/third-party-licenses.md`） | S〜M | — | 承認に D2 / D3 | 目録が `Cargo.lock` と一致する検査 |
| T4 | **ADR-0029（版と安定性・互換性・サポート期間）の起草** | S | — | 承認に D4 | 文書レビュー |
| T5 | **ADR-0030（配布とリリース手順）の起草** | S | — | 承認に D5 | 文書レビュー |
| T6 | **`wys --version`**: 版・コミット・target・ツールチェーン（`--format json` も）。LSP の `serverInfo` と同じ値 | S | — | — | e2e |
| T7 | **配布物の配置と探索、`scripts/release.sh`、`scripts/install.sh`**（名前は変数）: `find_runtime_lib` に `../lib`、アーカイブと `SHA256SUMS`、ad-hoc 署名、`install.sh --from-archive` | M | T6 | — | e2e: 配布物の `bin/wys` で `hello` と Python 連携をビルド（`WYS_RUNTIME_LIB` なし）。`install.sh` を一時 prefix に入れるシェルテストを `ci.sh` に。Linux は Docker で 1 回 |
| T8 | **利用者向け文書の骨格**（`docs/guide/`）: 入門、リファレンス v0.1（`grammar.ebnf` の見出しを現状に、型の規則の要約、組み込みの一覧、環境変数の一覧）、FFI の対応範囲。英語は README の要約だけ | L | — | 仕上げに D1 | 文書の ```wys ブロックを `check` / `run` / `fmt --check` する doctest |
| T9 | **README / CLAUDE.md / 索引の更新** | S | T8 | D1 | 相対リンクの存在の検査 |
| T10 | **ベンチの公開（NFR-002）** `docs/benchmarks.md`: コンパイル時間、Native、FFI、`check` の規模、LSP の遅延、AI（再生）。Web/API は「対象機能なし」、Linux は「未計測」。`benches/report.py` で TSV から表を作る | M | T1、T6 | — | `report.py` の単体テスト。CI に倍率の表現の検査 |
| T11 | **セキュリティの見直しと `docs/security.md`**: 脅威モデルと保証範囲・範囲外、Capability の入口の再検査、proxy、`cmd:`、承認の入力、記録・トレース・監査に秘密情報が入らないこと（NFR-005）、LSP、`unsafe` の棚卸し | M | — | SECURITY.md の窓口だけ D3 | 見つかった問題は issue にし、修正はケースを先に書く |
| T12 | **LSP の補完（最小、任意、出口条件に含めない）**: キーワード、スコープ内の名前、`.` の後のフィールド、組み込み | M | T1 | — | harness の新キー `# expect-completion:`、e2e |
| T13 | **名前の表記の整理**（ADR-0027 案A。改名はしない）: CLAUDE.md・用語集・利用者向けの文書の「正式名称は未定」「内部コードネーム」を `wys` に、用語集の名前の由来を外す | S | T2 承認 | **D1**（決定済み: `wys`） | 文書の表記の確認 |
| T14 | **公開に要るファイル**: `LICENSE-MIT` / `LICENSE-APACHE`、`Cargo.toml` の `license` / `version = 0.1.0`、CONTRIBUTING、CODE_OF_CONDUCT、SECURITY、CHANGELOG、issue / PR のテンプレート | S | T3 / T4 承認 | D2 / D3 / D4 | `ci.sh` に必須ファイルと `license` の値の検査 |
| T15 | **安定性と互換性の文書** `docs/stability.md`: 段階表、Tier（NFR-006）、Python 3.14 と「完全互換ではない」、NFR-007 の挙動、エラーコードと `schema_version` の約束 | S | T4 承認 | D4 | `docs/test-plan.md` に NFR-006 / 007 / 008 の行 |
| T16 | **公開前の点検** `scripts/preflight-public.sh`（読み取りだけ）: 全履歴の秘密情報の走査、追跡ファイルの点検、記録ファイルの本文、著者メールの一覧。**著者メールの書き換え**（`git filter-repo` の mailmap で noreply に。対象の一覧を提示してユーザーが確認してから、公開の直前に 1 回だけ。全ブランチの強制 push と PR のコミットリンクが切れることを了承の上で）。GitHub の設定の手順書 | S | — | 書き換えの実行の確認 | スクリプトの出力を出口判定に貼る |
| T17 | **リリース 0.1.0**: 署名付きタグ、macOS arm64 と Linux x86_64（Docker）のアーカイブ、`SHA256SUMS`、Release の下書き。Rust のないクリーンな環境での受け入れ（`install.sh` → `hello` / `fib` / `agent_demo`（モック）/ Python 連携） | M | T7、T13〜T16 | D1〜D5 | 受け入れの手順そのもの |
| T18 | **ユーザーが行う項目**: #59 PoC5、PoC6 基準 2（エディタ）、`scripts/llm-smoke.sh` | U | — | — | 出口条件に含めない（未実施なら「未計測」） |
| T19 | **出口判定と公開**: 判定表、roadmap / README / 用語集 / ADR 一覧の更新。ユーザーが visibility を public にし、リポジトリ名を変え、Release を公開する（#78） | S | 全部 | D6 | 判定表 |

## 新しい ADR と決める時期

| 番号 | 論点 | 時期 |
|---|---|---|
| ADR-0027 | 名称・拡張子・パッケージ名（`wys` を正式名称にする。案A） | 2026-11-11 に Accepted |
| ADR-0028 | ライセンス（MIT OR Apache-2.0）、著作権者（ユーザー個人）、ガバナンス、DCO、行動規範、脆弱性報告の窓口と範囲 | 2026-11-11 に Accepted |
| ADR-0029 | SemVer 0.y.z、安定性の段階表、対象環境の Tier、Python 3.14、サポート期間、SLO は未決 | 2026-11-11 に Accepted |
| ADR-0030 | 配布: Releases のみ、`bin/` + `lib/`、`../lib` の探索、SHA-256 と署名付きタグ、ad-hoc 署名、`install.sh`、Actions なしの手順 | 2026-11-11 に Accepted |

## 診断コードの帯

追加なし。

## 順序とクリティカルパス

クリティカルパスは **名前の決定 → T13（改名）→ T8 / T9 の仕上げ → T17（Linux のビルドに数時間）→ T19（公開）**。

- 第 1 週: T0、T2〜T5 の ADR を Proposed で出す。並行して T1、T6、T7、T11。T8 / T10 の骨格。ユーザーは名前を決める（衝突調査を含む）。
- 第 2 週: T13、T14、T15、T8 の仕上げ、T9、T10 の数値、T12（任意）。
- 第 3 週: T16、T17、T18、T19。

**分割の判断基準**: 名前が第 2 週の終わりまでに決まらない、または T11 で Capability の迂回が見つかり修正が M を超えるなら、公開準備 a = T0〜T16（リポジトリの公開に要るもの）で出口判定し、公開準備 b = T17（配布物）を切り離す。

## 出口条件

| 条件 | 根拠 | 要件 / 未決定 |
|---|---|---|
| ADR-0027〜0030 が Accepted。ADR 一覧の未決定 1 / 11（レジストリ以外）/ 12（サポート期間）が埋まる。SLO は「未決」 | T2〜T5、T19 | 未決定 1 / 11 / 12、NFR-008 |
| LICENSE などの公開に要るファイルがあり、`Cargo.toml` の `license` と `version` が一致 | T14 | 未決定 11 |
| 名前の表記が ADR-0027 どおり。公開の前に商標をユーザーが確認した | T13、T16 | — |
| macOS arm64 と Linux x86_64 の配布物と `SHA256SUMS`。Rust のないクリーンな環境で `install.sh` → `hello` / `fib` / `agent_demo`（モック）/ Python 連携が動く | T7、T17 | AC-004、NFR-006、COMP-002 |
| `wys --version` が版・コミット・target を出す | T6 | NFR-004 |
| `docs/guide/` の入門とリファレンス v0.1 があり、文書のコードが doctest で通る。README が現状を示す | T8、T9 | DX-001 / DX-002 |
| `docs/benchmarks.md` に機種情報付きの絶対値。倍率の検査が CI にある | T10 | NFR-002、5.1 |
| `docs/security.md` と `SECURITY.md`。見直しの指摘は修正済みか issue | T11、T14 | NFR-001、NFR-005、AC-007 |
| `docs/stability.md` | T15 | NFR-006〜008 |
| #135: 分岐の形で仕事量が線形 | T1 | DX-003、NFR-003 |
| 公開前の点検の走査が空で、著者メールの書き換えを済ませた | T16 | NFR-005 |
| ユーザーが visibility を public にした（#78）。または「保留」と理由を記録 | T19 | — |
| 参考（出口に含めない）: PoC5（#59）、PoC6 基準 2、補完（T12）、`llm-smoke` | T12、T18 | DX-003、DX-005 |

## 範囲外と持ち越し候補

- パッケージレジストリ、依存解決・ロックファイル（INTEROP-004、未決定 11 の後半）→ Phase 3。
- 性能 SLO の数値（未決定 12 の前半）→ ベンチと利用者の検証の後。
- Windows、GitHub Actions の復旧、Apple の公証 → 公開後。
- LSP のリネーム・参照検索・シグネチャのヘルプ・セマンティックトークン・増分の解析・VS Code の拡張 → 公開後。
- `docs/design/compiler.md`、英語の全訳、言語仕様の完全版 → 公開後。
- タスクのキャンセル、非ブロッキングの `llm_generate`、ストリーミング、ほかのプロバイダの adapter → M6 の持ち越し表のまま。

## リスク

| リスク | 対策 |
|---|---|
| 名前が決まらない | 名前に依存しない作業を先に進める。第 2 週の終わりで分割の判断基準を適用する |
| 改名の回帰（514 ファイル、スナップショット、環境変数） | 段階改名、`check-names.sh`、改名の PR では Docker の Linux まで回す |
| 履歴の書き換えは公開後に取り消せない | 公開の直前に 1 回だけ、対象の一覧をユーザーが確認してから |
| Actions がなく Linux はエミュレーション | Linux のビルドは前夜に始める。ベンチは macOS だけと明記 |
| macOS の quarantine | ad-hoc 署名、`install.sh` 経由、手動の外し方を文書に |
| ベンチが性能の主張に読まれる | 絶対値と機種情報だけ、CI の倍率の検査 |
| セキュリティの見直しで Capability の迂回が見つかる | T11 を第 1 週に始める。大きければ公開準備 a / b に分ける |
| 文書が日本語だけ | README に英語の要約。全訳は公開後 |
| `llm_generate` のプログラムに埋め込むコンパイラの絶対パス | `WYS_LLM_PROXY` と `PATH` の探索順を文書に。内部扱いと明記 |

## 出口判定の下書き（2026-11-12、T19 の準備）

最終の判定はユーザーが行う。「済」は AI が確かめたもの。

| 条件 | 状態 | 根拠 |
|---|---|---|
| ADR-0027〜0030 が Accepted、未決定 1 / 11 / 12 の欄 | 済 | `docs/adr/README.md`。SLO は未決 |
| 公開に要るファイル、`license` と `version` | 済 | LICENSE-MIT / LICENSE-APACHE、CONTRIBUTING、CODE_OF_CONDUCT、SECURITY、CHANGELOG、issue / PR のテンプレート、`version = "0.1.0"`。`scripts/ci.sh` が検査する |
| 名前の表記が ADR-0027 どおり | 済 | T13 |
| 商標の確認 | 済（ユーザーが判断） | USPTO の TSDR で「WYS?」（79334522、登録 7601757）を確かめ、ユーザーが `wys` のまま公開すると決めた（ADR-0027 の追記）。J-PlatPat は未確認 |
| 配布物と `SHA256SUMS`、クリーンな環境での受け入れ | 済 | 署名付きタグ `v0.1.0`（`ec60492`）と Release の 3 ファイル。両方の環境で CI と受け入れが合格。受け入れは PATH から Rust を外し、`install.sh --from-archive` → `hello` / `fib` / `agent_demo` / Python 連携 |
| `wys --version` | 済 | 配布物で `wys 0.1.0 (<コミット> <target>, rustc …)` |
| 利用者向け文書と doctest、README | 済 | `docs/guide/`、`guide_code_blocks_run` |
| ベンチの公開と倍率の検査 | 済 | `docs/benchmarks.md`、`scripts/ci.sh` |
| `docs/security.md` と `SECURITY.md` | 済（P3-4 の ASan / Miri の拡充は issue） | T11 |
| `docs/stability.md` | 済 | T15 |
| #135 の線形性 | 済 | `flow_analysis_work_is_linear_in_branches` |
| 公開前の点検と著者メールの書き換え | 済 | `scripts/preflight-public.sh` の 1〜3 が空。書き換えた履歴を新しい `1ppe1/wys` に置いた（`docs/publishing.md` §2）。新しいリポジトリの履歴に勤務先のメールはない |
| GitHub の設定 | public にした直後に行う | private vulnerability reporting は private のリポジトリでは有効にできない（`docs/publishing.md` §3） |
| visibility を public に | ユーザーが判定した（2026-11-12、公開する） | — |
| 参考: PoC5、PoC6 基準 2、補完、`llm-smoke` | 未実施 | 出口に含めない |

