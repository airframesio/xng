#!/usr/bin/env bash
# Build the decoders the navaid benchmark compares against, as black boxes.
# Nothing here is vendored into xng: sources are cloned into target/ and only
# their printed output is used. Licences: voracious MIT, dumpils MIT,
# vortrack GPL-2.0 (run, never copied).
#
# Needs: git, cargo, go, gcc, and for voracious librtlsdr-dev libusb-1.0-0-dev
# libairspy-dev libhackrf-dev libasound2-dev pkg-config.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${NAVAID_BENCH_TOOLS:-$ROOT/target/navaid-bench}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
SRC="$OUT/src"
mkdir -p "$SRC"
cd "$SRC"

# voracious 0.5.2 (crates.io), CLI without device features.
if [ ! -x "$OUT/voracious" ]; then
  curl -sSfL https://crates.io/api/v1/crates/voracious/0.5.2/download -o voracious.tgz
  tar xzf voracious.tgz
  (cd voracious-0.5.2 && cargo build --release --no-default-features -q)
  cp voracious-0.5.2/target/release/voracious "$OUT/"
fi

# vortrack (TLeconte), its rtl.c front end linked against a file-replay stub.
if [ ! -x "$OUT/vortrack-file" ]; then
  [ -d vortrack ] || git clone -q --depth 1 https://github.com/TLeconte/vortrack.git
  gcc -O2 -D WITH_RTL -o "$OUT/vortrack-file" vortrack/rtl.c vortrack/vor.c vortrack/vortrack.c \
    "$ROOT/bench/navaid/rtlstub.c" -lm
fi

# dumpils (asgaut): its demod package driven from a file, as cmd/dumpils does.
if [ ! -x "$OUT/dumpils-file" ]; then
  [ -d dumpils ] || git clone -q --depth 1 https://github.com/asgaut/dumpils.git
  mkdir -p dumpils-file
  cp "$ROOT/bench/navaid/dumpils-file/main.go" dumpils-file/
  cat > dumpils-file/go.mod <<EOF
module navbench/dumpilsfile

go 1.20

require github.com/asgaut/dumpils v0.0.0

replace github.com/asgaut/dumpils => $SRC/dumpils
EOF
  (cd dumpils && go mod download)
  (cd dumpils-file && GOFLAGS=-mod=mod go mod tidy && go build -o "$OUT/dumpils-file" .)
fi
echo "competitors in $OUT"
