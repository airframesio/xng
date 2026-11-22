#!/bin/bash
# Build the Linux release binary and .deb for one architecture, inside an
# Ubuntu 22.04 container (glibc 2.35), so the result runs on Ubuntu 22.04+,
# Debian 12 and Raspberry Pi OS Bookworm. The release workflow runs it; to
# try it locally:
#
#   docker run --rm -v "$PWD:/src" -w /src ubuntu:22.04 \
#       packaging/build-linux-release.sh armhf
#
# Argument: amd64 | arm64 (native builds, run on a host of that
# architecture) or armhf (cross-compiled on amd64).
# Output: dist/xng-<tag>-<rust target>.tar.gz and dist/xng-<version>-<arch>.deb.
#
# Environment: XNG_TAG names the tarball (default: v<Cargo.toml version>),
# PROTOC points at a protoc binary (otherwise one is downloaded: Ubuntu
# 22.04's protoc 3.12 can't compile proto3 `optional` fields).
set -euo pipefail

ARCH=${1:?usage: build-linux-release.sh amd64|arm64|armhf}
case "$ARCH" in
  amd64) TARGET=x86_64-unknown-linux-gnu;        FEATURES=airspy,airspyhf,sherpa ;;
  arm64) TARGET=aarch64-unknown-linux-gnu;       FEATURES=airspy,airspyhf,sherpa ;;
  # sherpa-onnx has no 32-bit ARM library: armhf transcribes with --whisper-url.
  armhf) TARGET=armv7-unknown-linux-gnueabihf;   FEATURES=airspy,airspyhf ;;
  *) echo "unknown architecture: $ARCH" >&2; exit 2 ;;
esac

VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
TAG=${XNG_TAG:-v$VERSION}
SUDO=; [ "$(id -u)" -ne 0 ] && SUDO=sudo
export DEBIAN_FRONTEND=noninteractive

HOST_ARCH=$(dpkg --print-architecture)
LIBS="libsoapysdr-dev libairspy-dev libairspyhf-dev"
$SUDO apt-get update
$SUDO apt-get install -y --no-install-recommends \
  build-essential ca-certificates curl unzip git pkg-config clang libclang-dev cmake dpkg-dev file

if [ "$ARCH" = "$HOST_ARCH" ]; then
  $SUDO apt-get install -y --no-install-recommends $LIBS
else
  # Cross build from amd64: armhf packages come from the ports mirror, so
  # pin the existing sources to amd64 first.
  [ "$ARCH" = armhf ] && [ "$HOST_ARCH" = amd64 ] || { echo "only armhf cross-builds, from amd64" >&2; exit 2; }
  CODENAME=$(. /etc/os-release && echo "$VERSION_CODENAME")
  $SUDO sed -i -E 's/^deb (http|\[)/deb [arch=amd64] \1/; s/^deb \[arch=amd64\] \[/deb [/' /etc/apt/sources.list
  for s in "" -updates -security; do
    echo "deb [arch=armhf] http://ports.ubuntu.com/ubuntu-ports ${CODENAME}$s main universe"
  done | $SUDO tee /etc/apt/sources.list.d/armhf.list >/dev/null
  $SUDO dpkg --add-architecture armhf
  $SUDO apt-get update
  # 32-bit ARM can't build the Opus source bundled with audiopus_sys (its
  # NEON files need -mfpu=neon), so link Ubuntu's static libopus.a instead,
  # as the armv7 Docker image does.
  $SUDO apt-get install -y --no-install-recommends gcc-arm-linux-gnueabihf g++-arm-linux-gnueabihf \
    libc6-dev:armhf $(for l in $LIBS libopus-dev; do echo "$l:armhf"; done)
  mkdir -p target/libopus-armhf/lib
  ln -sf /usr/lib/arm-linux-gnueabihf/libopus.a target/libopus-armhf/lib/
  export LIBOPUS_LIB_DIR=$PWD/target/libopus-armhf
  export CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
  export CXX_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-g++
  export AR_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-ar
  export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
  export PKG_CONFIG_ALLOW_CROSS=1
  export PKG_CONFIG_LIBDIR=/usr/lib/arm-linux-gnueabihf/pkgconfig:/usr/share/pkgconfig
  export BINDGEN_EXTRA_CLANG_ARGS_armv7_unknown_linux_gnueabihf="--target=arm-linux-gnueabihf -I/usr/include/arm-linux-gnueabihf"
fi

if [ -z "${PROTOC:-}" ]; then
  PROTOC_VERSION=28.3
  case "$HOST_ARCH" in amd64) P=x86_64 ;; arm64) P=aarch_64 ;; esac
  curl -fsSL -o /tmp/protoc.zip \
    "https://github.com/protocolbuffers/protobuf/releases/download/v${PROTOC_VERSION}/protoc-${PROTOC_VERSION}-linux-${P}.zip"
  mkdir -p /tmp/protoc && unzip -qo /tmp/protoc.zip -d /tmp/protoc
  export PROTOC=/tmp/protoc/bin/protoc
fi

if ! command -v cargo >/dev/null; then
  [ -x "$HOME/.cargo/bin/cargo" ] || \
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
  export PATH=$HOME/.cargo/bin:$PATH
fi
rustup target add "$TARGET"

cargo build --release --locked --bin xng --target "$TARGET" --features "$FEATURES"
BIN=target/$TARGET/release/xng
file "$BIN"

# The newest glibc symbol the binary needs: must stay at or below 2.35
# (Ubuntu 22.04; Debian 12 and Pi OS Bookworm have 2.36).
NEED=$(objdump -T "$BIN" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)
echo "$BIN needs $NEED"
if [ "$(printf '%s\nGLIBC_2.35\n' "$NEED" | sort -V | tail -1)" != GLIBC_2.35 ]; then
  echo "$BIN needs $NEED, newer than Ubuntu 22.04's GLIBC_2.35" >&2
  exit 1
fi

mkdir -p dist
PKG=xng-${TAG}-${TARGET}
rm -rf "dist/$PKG" && mkdir -p "dist/$PKG"
cp "$BIN" README.md LICENSE-MIT LICENSE-APACHE "dist/$PKG/"
tar -C dist -czf "dist/$PKG.tar.gz" "$PKG"
rm -r "dist/$PKG"

XNG_BIN=$BIN DEB_ARCH=$ARCH ./build_deb_package.sh
cp "build/xng-${VERSION}-${ARCH}.deb" dist/
ls -l dist
