#!/usr/bin/env bash
# Fetch the off-air bench fixtures into bench/data/ and check their sha256
# against bench/fixtures.sha256 (columns: sha256, file, corpus mode folder).
#
# Sources, in order:
#   1. XNG_CORPUS=/path/to/xng-corpus  a local clone of the private
#      airframesio/xng-corpus repo (Git LFS), the durable copy of every
#      capture: files are copied from $XNG_CORPUS/iq/<mode>/.
#   2. otherwise the public xng release assets (tag bench-fixtures-v1),
#      which is what CI uses, so CI spends no LFS bandwidth.
#
# Usage: bench/fetch-fixtures.sh [file ...]   (default: every listed fixture)
set -euo pipefail
cd "$(dirname "$0")/.."

TAG="${XNG_FIXTURES_TAG:-bench-fixtures-v1}"
REPO="${XNG_FIXTURES_REPO:-airframesio/xng}"
LIST=bench/fixtures.sha256
mkdir -p bench/data

want=("$@")
fail=0
while read -r sum file mode; do
  [ -z "$sum" ] && continue
  if [ ${#want[@]} -gt 0 ] && [[ ! " ${want[*]} " == *" $file "* ]]; then continue; fi
  dst="bench/data/$file"
  if [ -f "$dst" ] && echo "$sum  $dst" | sha256sum -c --status 2>/dev/null; then
    echo "ok (cached): $file"; continue
  fi
  if [ -n "${XNG_CORPUS:-}" ]; then
    src="$XNG_CORPUS/iq/$mode/$file"
    if [ ! -f "$src" ]; then echo "missing in corpus: $src"; fail=1; continue; fi
    if head -c 40 "$src" | grep -q "^version https://git-lfs"; then
      echo "LFS pointer, not data: $src (run git lfs pull in the corpus)"; fail=1; continue
    fi
    cp "$src" "$dst"
  else
    curl -fsSL --retry 3 -o "$dst" "https://github.com/$REPO/releases/download/$TAG/$file"
  fi
  if echo "$sum  $dst" | sha256sum -c --status; then
    echo "ok: $file"
  else
    echo "CHECKSUM MISMATCH: $file"; rm -f "$dst"; fail=1
  fi
done < "$LIST"
exit $fail
