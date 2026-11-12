#!/bin/bash
# Build the Ubuntu .deb in a Docker container, without GitHub Actions.
#
#   scripts/build-deb-docker.sh          -> dist/dwl-timer_<version>_amd64.deb
#
# Builds on Ubuntu 22.04 for amd64 (installs on 22.04 and 24.04), same as the CI workflow.
# On an Apple Silicon Mac the container runs under emulation: the first build takes 30-60
# minutes, later ones are faster (Rust and npm caches are kept in Docker volumes
# dwl-timer-cargo / -rustup / -target / -npm). Rust's own linker (lld) crashes under
# that emulation, so the system linker is used. Unit tests run with `cargo test` outside.
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(node -p 'require("./package.json").version')
mkdir -p dist

docker run --rm --platform linux/amd64 \
    -v "$PWD":/src:ro \
    -v "$PWD/dist":/dist \
    -v dwl-timer-cargo:/root/.cargo \
    -v dwl-timer-rustup:/root/.rustup \
    -v dwl-timer-target:/build/src-tauri/target \
    -v dwl-timer-npm:/root/.npm \
    -e VERSION="$version" \
    -e CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-Clinker-features=-lld" \
    ubuntu:22.04 bash -euo pipefail -c '
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev \
    build-essential curl file pkg-config xz-utils ca-certificates >/dev/null
if [ ! -x /root/.cargo/bin/cargo ]; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal
fi
. /root/.cargo/env
rustup default stable >/dev/null 2>&1 || rustup toolchain install stable --profile minimal
if [ ! -x /opt/node/bin/node ]; then
    mkdir -p /opt/node
    curl -sSfL https://nodejs.org/dist/v20.18.0/node-v20.18.0-linux-x64.tar.xz | tar -xJ -C /opt/node --strip-components=1
fi
export PATH=/opt/node/bin:$PATH
# Copy the sources (the mount is read-only); keep the cached target dir.
mkdir -p /build
tar -C /src --exclude=./node_modules --exclude=./src-tauri/target --exclude=./dist --exclude=./.git -cf - . | tar -C /build -xf -
cd /build
npm ci --no-audit --no-fund
npx tauri build --bundles deb
# The cached target dir also holds earlier versions: take this one only.
cp src-tauri/target/release/bundle/deb/*_${VERSION}_amd64.deb "/dist/dwl-timer_${VERSION}_amd64.deb"
'
ls -l "dist/dwl-timer_${version}_amd64.deb"
