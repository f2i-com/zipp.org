#!/bin/bash
# Release build that survives Windows holding a handle on the previous zipp.exe.
#
# A finished `zipp.exe` can stay locked for a moment after its process exits (an
# antivirus scan, an Explorer preview), and cargo's link step fails outright when
# it cannot REMOVE the old file. Renaming it always succeeds — Windows allows a
# rename of an open file, just not a delete — so move it aside first and sweep
# the leftovers on the next build.
cd "$(dirname "$0")/.." || exit 1
rm -f target/release/zipp.stale*.exe 2>/dev/null
if [ -e target/release/zipp.exe ]; then
  mv target/release/zipp.exe "target/release/zipp.stale$$.exe" 2>/dev/null
fi
cargo build --release "$@" 2>&1 | grep -E "^(error|warning: unused)|Finished|^error\[" -A 6
