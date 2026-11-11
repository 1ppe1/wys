#!/usr/bin/env bash
# 公開前の点検（公開準備 T16、docs/plans/release.md）。読み取りだけで、何も書き換えない。
#
# 1. 全履歴（全ブランチ）の差分から、秘密情報らしい文字列を探す（見つけた場所だけを出し、値は出さない）
# 2. いま追跡しているファイルに、入れてはいけないもの（.env、秘密鍵、計測の生データ、ビルドの成果物）がないか
# 3. 追跡している記録（*.jsonl）が実プロバイダの応答でないか（ヘッダの provider が mock / cmd だけ。replay は要確認）
# 4. 著者とコミッターのメールの一覧（公開の直前に noreply へ書き換えるかの判断用。書き換えはしない）
#
# 1〜3 で見つかれば終了コード 1。4 は一覧を出すだけ。
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
note() { echo "preflight: $*"; }

echo "== 1. 履歴の秘密情報らしい文字列"
# Anthropic / OpenAI の API キー、AWS のアクセスキー、秘密鍵、キーへの直接の代入
pattern='sk-ant-[A-Za-z0-9_-]{8,}|sk-[A-Za-z0-9]{32,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----|ANTHROPIC_API_KEY=[A-Za-z0-9_-]{8,}'
# `-m`: マージのコミットも各親との差分を出す（既定ではマージの差分を出さないので、マージの解決で入った文字列を
# 見落とす。Codex のレビュー）
hits=$(git log --all -p -m --no-color --format='commit %H' \
  | awk -v pat="$pattern" '
      /^commit [0-9a-f]+$/ { c = $2; next }
      /^\+\+\+ / { f = substr($0, 7); next }
      /^\+/ && $0 ~ pat { print c " " f }' \
  | sort -u || true)
if [ -n "$hits" ]; then
  note "秘密情報らしい文字列がある（コミットとファイル。値は出さない）:"
  echo "$hits" | sed 's/^/  /'
  fail=1
else
  note "見つからなかった"
fi

echo "== 2. 追跡しているファイル"
bad=$(git ls-files | grep -E '(^|/)\.env($|\.)|(^|/)\.slack_env$|service-account.*\.json$|credentials.*\.json$|\.pem$|\.key$|(^|/)id_(rsa|ed25519)$|^target/|^dist/|^benches/results/|^scripts/poc5/results/' || true)
if [ -n "$bad" ]; then
  note "入れてはいけないファイルを追跡している:"
  echo "$bad" | sed 's/^/  /'
  fail=1
else
  note "問題なし（$(git ls-files | wc -l | tr -d ' ') ファイル）"
fi

echo "== 3. 記録（*.jsonl）のプロバイダ"
real=0
while IFS= read -r f; do
  provider=$(head -n 1 "$f" | python3 -c 'import json,sys
try:
    v = json.load(sys.stdin)
    print(v.get("provider", "-") if v.get("kind") == "header" else "-")
except Exception:
    print("-")')
  case "$provider" in
    mock|cmd|-) ;;
    # 再生しながら記録し直したものは、元の記録（実プロバイダの応答かもしれない）の本文をそのまま含む（Codex のレビュー）
    replay) note "$f は再生から作った記録（元の記録の本文を含み得るので、目で確かめる）"; real=1; fail=1 ;;
    *) note "$f は実プロバイダ（$provider）の記録（本文が入っている）"; real=1; fail=1 ;;
  esac
done < <(git ls-files '*.jsonl')
[ "$real" = 1 ] || note "実プロバイダの記録はない"

echo "== 4. 著者とコミッターのメール（件数）"
git log --all --format='%ae%n%ce' | sort | uniq -c | sort -rn | sed 's/^/  /'

if [ "$fail" = 1 ]; then
  echo "preflight: 要確認の項目がある"
  exit 1
fi
echo "preflight: OK（4 の扱いはユーザーが決める）"
