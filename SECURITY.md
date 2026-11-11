# セキュリティ / Security Policy

## 対象の版 / Supported versions

最新の `0.y` だけを対象にする。修正は最新の `0.y.z` に出し、古い `0.y` には持ち帰らない（ADR-0029）。

Only the latest `0.y` release receives fixes.

## 報告の方法 / Reporting a vulnerability

**公開の issue には書かないでください。** GitHub の private vulnerability reporting を使ってください: このリポジトリの **Security** タブ → **Report a vulnerability**。メールの窓口はない（ADR-0028）。

**Please do not open a public issue.** Use GitHub private vulnerability reporting: **Security** tab → **Report a vulnerability**. There is no email channel.

報告には、再現の手順（`.wys` のコード、環境変数、`wys --version` の出力）と、何が破られるかを書いてください。日本語でも英語でもよい。

## 対応 / Response

- 受領の返答は 7 日以内を目標にする。修正の期限は約束しない（1 人で運営している）。
- 修正を出したら、GitHub Security Advisory で公表する。報告者の名前を載せるかは報告者が選べる。

## 範囲 / Scope

範囲内（コンパイラ・ランタイム・`wys llm-proxy`・`wys lsp`）:

- Capability・承認・予算の迂回（AC-007）
- 秘密情報（API キーなど）の漏れ（NFR-005）
- メモリ安全性の保証範囲（`docs/design/memory-model.md` §3）の破れ
- 記録・トレース・監査のファイルの扱いの不備（symlink の追従など）

範囲外:

- 利用者のプログラムの誤り
- Python のコード（Python はサンドボックスではない。ADR-0018）
- 利用者が指定した `cmd:` のプロバイダ
- LLM の出力そのもの（出力は信頼しない前提で、型の検証と Capability で守る）

保証すること・しないことの詳細は [docs/security.md](docs/security.md)。
