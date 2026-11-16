#!/bin/sh
# Write the licences of the crates med is built from (and of the Catppuccin
# palette) to target/THIRD-PARTY-LICENSES.txt, the file the packages carry.
# cargo packager runs this before packaging (see crates/med/Cargo.toml).
# Usage: scripts/third-party-licenses.sh   (needs cargo-about)
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$root/target"
cargo about generate --locked \
  --manifest-path "$root/Cargo.toml" \
  --config "$root/about.toml" \
  --output-file "$root/target/THIRD-PARTY-LICENSES.txt" \
  "$root/about.hbs"
