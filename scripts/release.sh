#!/usr/bin/env bash
# 配布物を作る（公開準備 T7、ADR-0030）。GitHub Actions は使わず、メンテナが手元で実行する。
#
#   dist/<名前>-<版>-<target>/
#     bin/<名前>                     コンパイラ
#     lib/libwys_runtime.a           利用者のプログラムに静的リンクするランタイム
#     lib/libwys_runtime_python.a    Python を使うプログラム用（あれば）
#     LICENSE-* README.md CHANGELOG.md third-party-licenses.md（あれば）
#   dist/<名前>-<版>-<target>.tar.gz
#   dist/wys-<版>.vsix              VS Code の拡張（--vsix のとき。ADR-0032）
#   dist/SHA256SUMS
#
# 使い方: scripts/release.sh [--out <dir>] [--skip-build] [--profile-dir <target/release>] [--vsix]
#   --skip-build と --profile-dir target/debug は検査用（scripts/test-dist.sh）。
# 名前は `wys`（ADR-0027）。検査のために RELEASE_NAME で上書きできる。
set -euo pipefail
cd "$(dirname "$0")/.."

name="${RELEASE_NAME:-wys}"
out=dist
skip_build=0
profile_dir=target/release
vsix=0
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --skip-build) skip_build=1; shift ;;
    --profile-dir) profile_dir="$2"; shift 2 ;;
    --vsix) vsix=1; shift ;;
    *) echo "release.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

commit=$(git rev-parse --short HEAD)
if [ "$skip_build" = 0 ]; then
  WYS_BUILD_COMMIT="$commit" cargo build --release --locked -p wys_cli -p wys_runtime -p wys_runtime_python
fi

cli="$profile_dir/wys"
[ -x "$cli" ] || { echo "release.sh: $cli がない" >&2; exit 1; }
[ -f "$profile_dir/libwys_runtime.a" ] || { echo "release.sh: $profile_dir/libwys_runtime.a がない" >&2; exit 1; }
version=$("$cli" version --format json | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')
target=$(rustc -vV | sed -n 's/^host: //p')
base="$name-$version-$target"

mkdir -p "$out"
stage="$out/$base"
rm -rf "$stage" "$out/$base.tar.gz"
mkdir -p "$stage/bin" "$stage/lib"
cp "$cli" "$stage/bin/$name"
cp "$profile_dir/libwys_runtime.a" "$stage/lib/"
if [ -f "$profile_dir/libwys_runtime_python.a" ]; then
  cp "$profile_dir/libwys_runtime_python.a" "$stage/lib/"
fi
for f in LICENSE-MIT LICENSE-APACHE README.md CHANGELOG.md docs/third-party-licenses.md; do
  if [ -f "$f" ]; then cp "$f" "$stage/"; fi
done
# macOS は ad-hoc 署名まで（公証は公開後。ADR-0030 論点 5）
if [ "$(uname -s)" = Darwin ]; then
  codesign -s - -f "$stage/bin/$name" 2>/dev/null
fi

tar -C "$out" -czf "$out/$base.tar.gz" "$base"
if [ "$vsix" = 1 ]; then
  # 拡張の版はコンパイラの版と揃える（ADR-0032）
  ext_version=$(python3 -c 'import json; print(json.load(open("editors/vscode/package.json"))["version"])')
  [ "$ext_version" = "$version" ] || { echo "release.sh: editors/vscode/package.json の版 $ext_version がコンパイラの版 $version と違う" >&2; exit 1; }
  (cd editors/vscode && npm ci --no-audit --no-fund --silent && npm test && npm run package --silent)
  mv "editors/vscode/wys-$version.vsix" "$out/"
fi
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}
(cd "$out" && shopt -s nullglob && sha256 ./*.tar.gz ./*.vsix | sed 's# \./# #' > SHA256SUMS)
echo "release.sh: $out/$base.tar.gz（${commit}）"
cat "$out/SHA256SUMS"
