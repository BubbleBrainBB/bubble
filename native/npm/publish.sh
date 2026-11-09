#!/usr/bin/env bash
# Publishes native/npm/dist (made by build.mjs plus the CI-built platform
# packages): the platform packages first, then @bubblebrain-ai/bubble, so the
# main package never points at a build that is not there yet. Versions
# already on npm are skipped, so it can be run again after a failure.
#   TAG=latest native/npm/publish.sh [extra npm publish flags, e.g. --otp=123456]
set -euo pipefail
cd "$(dirname "$0")/dist"
TAG="${TAG:-latest}"
publish() {
  local dir="$1"
  local name version
  name=$(node -p "require('./$dir/package.json').name")
  version=$(node -p "require('./$dir/package.json').version")
  if npm view "$name@$version" version >/dev/null 2>&1; then
    echo "skip $name@$version (already published)"
    return
  fi
  echo "publish $name@$version (tag $TAG)"
  npm publish "./$dir" --access public --tag "$TAG" "${@:2}"
}
for platform in darwin-arm64 darwin-x64 linux-x64 linux-arm64 win32-x64; do
  [ -d "$platform" ] || { echo "missing dist/$platform" >&2; exit 1; }
  publish "$platform" "$@"
done
publish bubble "$@"
