#!/bin/sh
# 英訳（ADR-0031）が、元の日本語の文書より古くなっていないかを確かめる。
# 英訳の先頭の「... translation of [<元>](...) as of commit `<hash>`」を読み、
# その後に元の文書が変わっていたら警告する（失敗にはしない。--strict で失敗にする）。
set -eu
strict=0
[ "${1:-}" = "--strict" ] && strict=1
stale=0
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
    echo "warning: $en: commit $hash is not in this checkout (cannot check)" >&2
    continue
  fi
  if [ -n "$(git log --oneline "$hash..HEAD" -- "$src")" ] || ! git diff --quiet HEAD -- "$src"; then
    echo "warning: $en may be stale: $src changed after $hash" >&2
    stale=1
  fi
done
[ "$stale" = 0 ] && echo "translations: up to date"
[ "$strict" = 1 ] && [ "$stale" = 1 ] && exit 1
exit 0
