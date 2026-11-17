#!/bin/sh
# 英訳（ADR-0031）が、元の日本語の文書より古くなっていないかを確かめる。
# 英訳の先頭の「... translation of [<元>](...) as of commit `<hash>`」を読み、
# その後に元の文書が変わっていたら警告する（失敗にはしない。--strict で失敗にする）。
# コミットが手元にない（浅い clone）と確かめられないので、「最新」とは区別して報告する（--strict では失敗）。
set -eu
strict=0
[ "${1:-}" = "--strict" ] && strict=1
stale=0
unchecked=0
for en in docs/en/*.md; do
  line=$(grep -m1 'translation of \[' "$en" || true)
  src=$(printf '%s' "$line" | sed -n 's/.*translation of \[\([^]]*\)\].*/\1/p')
  hash=$(printf '%s' "$line" | sed -n 's/.*as of commit `\([0-9a-f]*\)`.*/\1/p')
  if [ -z "$src" ] || [ -z "$hash" ]; then
    echo "error: $en has no translation header (ADR-0031)" >&2
    exit 1
  fi
  [ -f "$src" ] || { echo "error: $en: source $src does not exist" >&2; exit 1; }
  if ! git cat-file -e "$hash^{commit}" 2>/dev/null; then
    echo "warning: $en: commit $hash is not in this checkout (cannot check; fetch the full history)" >&2
    unchecked=$((unchecked + 1))
    continue
  fi
  if [ -n "$(git log --oneline "$hash..HEAD" -- "$src")" ] || ! git diff --quiet HEAD -- "$src"; then
    echo "warning: $en may be stale: $src changed after $hash" >&2
    stale=1
  fi
done
if [ "$stale" = 1 ]; then
  echo "translations: some may be stale (see the warnings)"
elif [ "$unchecked" -gt 0 ]; then
  echo "translations: $unchecked could not be checked"
else
  echo "translations: up to date"
fi
if [ "$strict" = 1 ] && { [ "$stale" = 1 ] || [ "$unchecked" -gt 0 ]; }; then
  exit 1
fi
exit 0
