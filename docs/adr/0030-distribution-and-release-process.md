# ADR-0030: 配布の方式とリリースの手順

- **状態:** Proposed
- **日付:** 2026-11-11
- **関連:** AC-004、COMP-002、NFR-004、NFR-006、ADR-0004（clang でリンク）、ADR-0008（ランタイムの静的リンク）、ADR-0029（版と Tier）、`docs/plans/release.md` T5 / T7 / T17、#74

## 背景

利用者がコンパイラを入れて使うには、コンパイラの実行ファイルと、利用者のプログラムに静的リンクするランタイム（`libwys_runtime.a`、Python を使うなら `libwys_runtime_python.a`）が要る。`wys build` はランタイムを `WYS_RUNTIME_LIB` → コンパイラの実行ファイルの隣 → その親の順で探す。リンクには `clang` が要り（ADR-0004）、Python を使うプログラムには Python 3.14 と `python3.14-config` が要る。

制約:

- GitHub Actions は使えない。リリースはメンテナが手元で作る。Linux x86_64 は Docker のエミュレーションで数時間かかる。
- Apple の Developer ID を持っていない（公証できない）。
- `llm_generate` を使うプログラムには、コンパイラの絶対パスが埋め込まれる（proxy の探索）。

## 選択肢と決定

| 論点 | 決定（推奨） | 主な代替案 | 理由 |
|---|---|---|---|
| 1. 配布の経路 | **GitHub Releases だけ** | (a) Homebrew の tap。(b) `cargo install` / crates.io | (a) は formula の保守と bottle の二重管理。(b) は 18 クレートの公開と名前の確保が要り、`cargo install` はランタイムの `.a` を置かない。どちらも公開後に需要を見て検討する |
| 2. アーカイブの形 | `<名前>-<版>-<target>.tar.gz`。中身は `bin/<名前>`、`lib/libwys_runtime.a`（と Python 用）、`LICENSE-*`、`README`、`third-party-licenses`。target は `aarch64-apple-darwin` と `x86_64-unknown-linux-gnu` | 単一ファイル（ランタイムをコンパイラに埋め込み、初回に展開） | 小さな変更で済む。単一ファイルは公開後 |
| 3. ランタイムの探索 | 探索順に `<exe>/../lib/` を足す（`WYS_RUNTIME_LIB` → 隣 → 親 → `../lib`） | 環境変数を必須にする | 展開しただけで動く |
| 4. 完全性の検証 | `SHA256SUMS` を Release に添え、git のタグに署名する | 署名なし | 改ざんの検出。`install.sh` は SHA-256 を検証してから展開する |
| 5. macOS の署名 | ad-hoc 署名（`codesign -s -`）まで。quarantine の外し方を文書に書く | Developer ID と公証 | 費用と手続き。`install.sh`（curl）経由なら quarantine は付かない。公証は公開後 |
| 6. インストールのスクリプト | `scripts/install.sh`: OS と arch の判定、版の指定、SHA-256 の検証、`--prefix`、`--from-archive <path>`（オフラインで検証できる）。手動のインストールの手順も文書に併記する | スクリプトを置かない | `curl \| sh` に不信がある利用者のために、手動の手順と検証の方法を並べる |
| 7. リリースの手順 | `scripts/release.sh`（`cargo build --release --locked`、アーカイブ、`SHA256SUMS`、ad-hoc 署名。名前は変数 1 か所）。Linux は Docker で前夜に始める。受け入れは Rust のないクリーンな環境で `install.sh` → `hello` / `fib` / `agent_demo`（モック）/ Python 連携 | Actions のワークフローを書いておく | Actions は使えない。手順をスクリプトにして再現できるようにする |
| 8. 埋め込むコンパイラのパス | 内部扱い（ADR-0029）。配布物を動かしたときは `WYS_LLM_PROXY` と `PATH` の探索で見つかることを文書に書く | 埋め込みをやめる | 既存の挙動を変えずに、動かしたときの回避策を示す |
| 9. 外部の前提 | `clang`（必須）、Python 3.14 と開発用ヘッダ（Python を使うときだけ）を利用者が入れる。配布物には含めない | clang を同梱する | 大きさと保守。前提を入門の文書に書く |

## 影響

- T6 で `wys --version`（版・コミット・target）。T7 で探索順の追加、`release.sh`、`install.sh`、e2e（配布物の `bin/` から `hello` と Python 連携をビルドする）と `install.sh --from-archive` のシェルテスト。T17 でリリース 0.1.0 と受け入れ。
- 名前が決まるまで（ADR-0027）、アーカイブ名と `bin/` の名前は変数のまま。ランタイムのファイル名は ADR-0027 の改名の対象。
- 見直し条件: Homebrew や crates.io の需要が出たとき、Apple の公証ができるようになったとき、GitHub Actions が使えるようになったとき（タグからのビルド）。
