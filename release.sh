#!/usr/bin/env bash
# Builds the release files for one version into dist/: a static Linux binary per architecture,
# packed with the example configuration, the systemd unit and the licences, plus SHA256SUMS.
# Needs zig and cargo-zigbuild (brew install zig; cargo install cargo-zigbuild) and the two
# rustup targets. Publishes nothing.
# Usage: release.sh <version>      e.g. release.sh 0.1.0
set -euo pipefail
VERSION=${1:?version}
HERE=$(cd "$(dirname "$0")" && pwd); cd "$HERE"
declared=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
[ "$declared" = "$VERSION" ] || { echo "Cargo.toml says $declared, not $VERSION"; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "the tree has uncommitted changes"; exit 1; }

mkdir -p dist
for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  cargo zigbuild --release --locked -p akramium-home --target "$target"
  arch=${target%%-*}
  name="akramium-home-$VERSION-linux-$arch"
  stage=$(mktemp -d)
  mkdir "$stage/$name"
  cp "target/$target/release/akramium-home" home.example.toml README.md LICENSE-MIT LICENSE-APACHE deploy/nas/akramium-home.service "$stage/$name/"
  tar -C "$stage" -czf "dist/$name.tar.gz" "$name"
  echo "dist/$name.tar.gz"
done
(cd dist && shasum -a 256 akramium-home-"$VERSION"-linux-*.tar.gz > SHA256SUMS && cat SHA256SUMS)
