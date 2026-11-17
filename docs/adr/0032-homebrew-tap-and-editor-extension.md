# ADR-0032: Homebrew の tap と、VS Code の拡張の配布

- **状態:** Proposed
- **日付:** 2026-11-17
- **関連:** ADR-0030（配布の方式。論点 1 を一部置き換える）、ADR-0026（LSP）、ADR-0029（版と Tier）、0.1.x の計画（使ってもらう土台）

## 背景

ADR-0030 の論点 1 は、配布の経路を **GitHub Releases だけ**とし、Homebrew の tap は「formula の保守と bottle の二重管理」を理由に公開後に回した。見直し条件は「Homebrew や crates.io の需要が出たとき」。

0.1.0 の公開後、ユーザーは試す障壁を下げることを優先すると決めた（2026-11-17）。また、VS Code の拡張（`editors/vscode/`）ができ、その配り方を決める必要がある。

確かめたこと:

- Formula は Release の tar.gz と `SHA256SUMS` の値をそのまま指せば足り、bottle は要らない（配布物がすでにビルド済みの実行ファイル）。コンパイラはシンボリックリンクを解決してからランタイムを探す（`crates/wys_driver/src/lib.rs`）ので、`libexec` に展開して `bin/wys` だけをリンクする形で動く。
- 手元の Homebrew（7.0）で、一時的な tap から `brew install` → `brew test`（`hello` のビルドと実行、`--version`）→ `brew audit --strict` が通った。
- 拡張は esbuild で 1 ファイルにまとめ、`node_modules` を含めずに `.vsix`（約 92 KB）にできる。

## 選択肢

### 案A: GitHub Releases だけのまま

- 利点: 保守が増えない。
- 欠点: `curl | sh` 以外の入れ方がない。拡張の置き場所がない。

### 案B: tap と `.vsix` を足す（Marketplace は後）

- `1ppe1/homebrew-wys` に Formula を置く。版を出すたびに URL と SHA-256 を更新する。
- `.vsix` は GitHub Release に添付し、`SHA256SUMS` に含める。
- 利点: Homebrew の利用者は 1 行で入れられ、更新も `brew upgrade` で済む。拡張は配布物と同じ版・同じ検証で配れる。
- 欠点: 版ごとに tap の更新が要る（手作業 1 回）。

### 案C: Homebrew の本家（homebrew-core）や VS Code Marketplace に出す

- 利点: 見つけてもらいやすい。
- 欠点: homebrew-core はソースからのビルドと知名度の条件がある。Marketplace は発行者のアカウントと、公開後に取り下げにくい名前の確保が要る。0.y の間は早い。

## 決定

**案B を提案する。**

1. **Homebrew**: tap `1ppe1/homebrew-wys` の `Formula/wys.rb`。Release の tar.gz（macOS arm64 と Linux x86_64）と SHA-256 を指し、`libexec` に展開して `bin/wys` をリンクする。`brew test` は `hello` のビルドと実行。ソースからのビルドと bottle は作らない。
2. **版ごとの手順**: Release を公開した後に、tap の Formula の `url` と `sha256` を新しい版に更新する（`SHA256SUMS` の値をそのまま写す）。`brew audit --strict` と `brew test` を通してから push する。
3. **VS Code の拡張**: `scripts/release.sh --vsix` で `dist/wys-<版>.vsix` を作り、`SHA256SUMS` に含めて Release に添付する。拡張の版はコンパイラの版と揃える（`release.sh` が食い違いを拒む）。
4. **Marketplace / Open VSX と homebrew-core** は、この ADR の範囲外。需要が出たら別の ADR で決める。
5. ADR-0030 の論点 1 は「GitHub Releases を正とし、tap と `.vsix` を添える」に読み替える。ほかの論点（アーカイブの形、探索、署名、`install.sh`）は変えない。

## 影響

- 利用者は `brew install 1ppe1/wys/wys` で入れられる。拡張は Release から `.vsix` を入れる。
- 版を出す手順に、`.vsix` を作ることと、tap の更新が加わる。
- 更新が必要な文書: `README.md` と `README.ja.md`（インストール）、`docs/adr/README.md`（一覧）、ADR-0030 の状態の欄（「論点 1 は ADR-0032 で一部置き換え」。ADR-0032 が Accepted になってから）。
- 見直す条件: tap の更新を忘れる・遅れることが続いたとき（Release からの自動更新を検討する）。Marketplace への需要が出たとき。
