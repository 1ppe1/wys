#!/bin/sh
# 配布物を入れる（公開準備 T7、ADR-0030）。アーカイブの SHA-256 を SHA256SUMS で確かめてから展開し、
# <prefix>/bin/<名前> と <prefix>/lib/ に置く（コンパイラは ../lib のランタイムを使う）。
#
# 使い方:
#   install.sh [--version <版>] [--prefix <dir>]          GitHub Releases から取ってくる（既定は最新）
#   install.sh --from-archive <file.tar.gz> [--sums <SHA256SUMS>] [--prefix <dir>]
#                                                        手元のアーカイブから（オフラインで検証できる）
# 既定の prefix は $HOME/.local。前提: clang（必須）、Python 3.14 と開発用ヘッダ（Python を使うときだけ）。
set -eu

name="${INSTALL_NAME:-wys}"          # ADR-0027 で名前が決まったら変える
repo="${INSTALL_REPO:-1ppe1/wys}"
prefix="$HOME/.local"
version=latest
archive=
sums=

while [ $# -gt 0 ]; do
  case "$1" in
    --version) version="$2"; shift 2 ;;
    --prefix) prefix="$2"; shift 2 ;;
    --from-archive) archive="$2"; shift 2 ;;
    --sums) sums="$2"; shift 2 ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

fail() { echo "install.sh: $*" >&2; exit 1; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

if [ -z "$archive" ]; then
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    *) fail "この環境（$(uname -s) $(uname -m)）向けの配布物はない（ADR-0029 の Tier を参照）" ;;
  esac
  command -v curl >/dev/null 2>&1 || fail "curl が要る"
  if [ "$version" = latest ]; then
    version=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
      | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$version" ] || fail "最新の版を取得できない"
  fi
  file="$name-$version-$target.tar.gz"
  url="https://github.com/$repo/releases/download/v$version"
  curl -fsSL -o "$tmp/$file" "$url/$file" || fail "$url/$file を取得できない"
  curl -fsSL -o "$tmp/SHA256SUMS" "$url/SHA256SUMS" || fail "$url/SHA256SUMS を取得できない"
  archive="$tmp/$file"
  sums="$tmp/SHA256SUMS"
else
  [ -f "$archive" ] || fail "$archive がない"
  [ -n "$sums" ] || sums="$(dirname "$archive")/SHA256SUMS"
fi

[ -f "$sums" ] || fail "$sums がない（SHA-256 を確かめられないので入れない）"
file=$(basename "$archive")
want=$(awk -v f="$file" '$2 == f || $2 == "*" f { print $1 }' "$sums" | head -n 1)
[ -n "$want" ] || fail "$sums に $file の行がない"
got=$(sha256 "$archive")
[ "$want" = "$got" ] || fail "SHA-256 が一致しない（$file: 期待 ${want}、実際 ${got}）"

tar -C "$tmp" -xzf "$archive"
dir="$tmp/${file%.tar.gz}"
[ -x "$dir/bin/$name" ] || fail "アーカイブに bin/$name がない"
mkdir -p "$prefix/bin" "$prefix/lib"
# 同じディレクトリの一時の名前に置いてから rename で入れ替える（動いているコンパイラ、たとえば `wys lsp` を
# 上書きしようとして Linux の `Text file busy` で止まらないように。Codex のレビュー）
place() {
  cp "$1" "$2.new.$$"
  mv -f "$2.new.$$" "$2"
}
place "$dir/bin/$name" "$prefix/bin/$name"
for lib in "$dir"/lib/*.a; do
  place "$lib" "$prefix/lib/$(basename "$lib")"
done
echo "install.sh: $prefix/bin/$name を入れた（$("$prefix/bin/$name" --version)）"
case ":$PATH:" in
  *":$prefix/bin:"*) ;;
  *) echo "install.sh: PATH に $prefix/bin を足してください" ;;
esac
command -v clang >/dev/null 2>&1 || echo "install.sh: clang が見つからない（プログラムのビルドに要る）"
