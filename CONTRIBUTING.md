# 貢献の手引き / Contributing

`wys` への貢献を歓迎する。文書と議論は日本語が正だが、英語の issue と PR も受け付ける。

Contributions are welcome. Japanese is the primary language of the docs, but issues and PRs in English are fine.

## 進め方

1. **先に issue を立てる。** 不具合なら再現の手順（`.wys` のコード、`wys --version` の出力）、機能なら動機を書く。脆弱性は issue にせず [SECURITY.md](SECURITY.md) の手順で報告する。
2. **PR は issue に紐づける。** 本文に `Closes #N`（その issue を完了する）か `Refs #N`（一部）を書く。
3. **テストを先に書く。** 言語の挙動は `tests/conformance/` に `.wys` のケースとして足し、失敗することを確かめてから実装する（書き方は `crates/wys_testharness/src/lib.rs` の先頭のコメント）。新しいエラーコードは `docs/spec/error-codes.md` に追記し、ケースを必ず作る。
4. **`./scripts/ci.sh` を通す。** fmt・clippy・テスト・Conformance（Debug / Release / AddressSanitizer）など、CI と同じ検査をまとめて実行する。前提は Rust（`rust-toolchain.toml` で固定）、clang、Python 3.14。
5. **コミットは Conventional Commits**（`feat(typeck): ...`、`fix(runtime): ...`、`docs: ...` など）。

## 設計の変更

後から変えにくい判断（構文・型・メモリ・Effect・FFI・セキュリティの境界）は ADR（`docs/adr/`）で決める。誰でも `Proposed` の ADR を PR で提案できる。`Accepted` にするのはメンテナ（ADR-0028）。手順は [docs/adr/README.md](docs/adr/README.md)。

迷ったときは要件定義書 [docs/ai_native_language_requirements.md](docs/ai_native_language_requirements.md) に戻る。用語は [docs/glossary.md](docs/glossary.md) に揃える。

## 貢献の権利（DCO）

すべてのコミットに `Signed-off-by:` の行を付ける（`git commit -s`）。これは [Developer Certificate of Origin 1.1](https://developercertificate.org/) に同意し、貢献を MIT OR Apache-2.0 の条件で提供する、という意思表示になる。

Every commit must carry a `Signed-off-by:` line (`git commit -s`), certifying the [Developer Certificate of Origin 1.1](https://developercertificate.org/). Contributions are licensed under MIT OR Apache-2.0.

## ライセンス

特に明記しない限り、あなたが意図して提出した貢献は、追加の条件なしに MIT OR Apache-2.0 の二重ライセンスで提供されたものとする。

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project shall be dual licensed as above, without any additional terms or conditions.

## 行動規範

[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)（Contributor Covenant 2.1）に従う。
