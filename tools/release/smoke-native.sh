#!/usr/bin/env bash
# tools/release/smoke-native.sh <arch> <command...>
#
# The release smoke test for one native CLI, run as `<command...> js|py
# <file>`: a command rather than a path, so an emulated binary runs as
# `qemu-arm -L <sysroot> zipp` (CI's armv7 job, and the local Docker runs).
# JavaScript through a JIT tier and the recursion limit, then Python. The
# expected line is what `node` prints for the script. On x86_64 the JIT log
# must also show a compiled function: a bare `console.log` never leaves the
# interpreter, so without it the Win64 ABI, the unwind/stack-probe
# interaction with the 256 MiB main thread and code publication would go
# untested in the gate that ships them. The aarch64 JIT compiles only
# call-free integer functions (the script's sum) and armv7 has none, so
# there the output alone is checked.
set -euo pipefail
arch=$1
shift
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cat > "$work/smoke.js" <<'SMOKE'
let total = 0;
for (let i = 0; i < 200000; i++) total = (total + i * 3) % 1000003;
function depth(n) { return n === 0 ? 0 : depth(n - 1) + 1; }
let guarded = "no-range-error";
try { depth(1e7); } catch (e) { guarded = e.constructor.name; }
function sum(n) { let t = 0; for (let i = 0; i < n; i++) t = (t + i * 7) | 0; return t; }
let s = 0;
for (let k = 0; k < 200; k++) s = (s + sum(1000)) | 0;
console.log(`zipp-release-smoke ${total} ${depth(100)} ${guarded} ${s}`);
SMOKE
cat > "$work/smoke.py" <<'SMOKE'
print("zipp-release-smoke", sum(x * x for x in range(4)), [k for k in {"b": 1, "a": 2}])
SMOKE
expected="zipp-release-smoke 520003 100 RangeError 699300000"
# `cd` keeps a Windows path out of bash's argument translation.
cd "$work"
actual="$(ZIPP_JIT_THRESHOLD=1 "$@" js smoke.js | tr -d '\r')"
if [[ "$actual" != "$expected" ]]; then
  echo "smoke output: $actual (expected: $expected)" >&2
  exit 1
fi
if [[ "$arch" == x86_64 ]]; then
  # Captured, not piped: `| grep -q` closes the pipe at its first match, and
  # the engine's next log write then hits EPIPE, where the print macros
  # panic.
  jitlog="$(ZIPP_JIT_THRESHOLD=1 ZIPP_JITLOG=1 "$@" js smoke.js 2>&1)"
  grep -q 'compiled' <<<"$jitlog"
fi
python="$("$@" py smoke.py | tr -d '\r')"
if [[ "$python" != "zipp-release-smoke 14 ['b', 'a']" ]]; then
  echo "python smoke output: $python" >&2
  exit 1
fi
echo "smoke ok ($arch): $actual"
