#!/bin/sh
# Attach what `cargo packager --out-dir <dir>` made to the GitHub release <tag>.
# A .app is a directory, so it goes up zipped, named like the .dmg beside it.
# Usage: scripts/release-assets.sh <tag> <dir>   (needs gh and GH_TOKEN)
set -eu
tag="$1"
dir="$2"
version="${tag#v}"
arch="$(uname -m)"
[ "$arch" = arm64 ] && arch=aarch64

set --
for path in "$dir"/*; do
  case "$path" in
    *.app)
      zip="$dir/med_${version}_${arch}.app.zip"
      ditto -c -k --keepParent "$path" "$zip"
      set -- "$@" "$zip"
      ;;
    *.dmg | *.deb | *.AppImage) set -- "$@" "$path" ;;
  esac
done
[ "$#" -gt 0 ] || { echo "nothing to upload in $dir" >&2; exit 1; }
gh release upload "$tag" "$@" --clobber
