#!/usr/bin/env bash
# pgo-build.sh TARGET: build the release CLI for TARGET with profile-guided
# optimization, as release-build.yml ships the CLIs of the hosts that can run
# their own binaries.
#
#   1. an instrumented build of the release CLI (the release feature set:
#      JavaScript, Python and the GPU path, as the plain build has);
#   2. the PGO training corpus, bench/pgo-training, run by that binary through
#      tools/pgo_training.py: the inputs and the exact output each must print
#      are bench/pgo-training/expected-output.json's, staged read-only first.
#      It runs twice: as the CLI runs it, then interpreted (ZIPP_NOJIT=1), as
#      a large program, a sandboxed one and any loop the JIT declines runs.
#      The corpus is small programs whose hot loops the JIT compiles, so the
#      first pass alone left the interpreter's paths for those loops cold;
#   3. the profiles of both passes merged by the selected toolchain's
#      llvm-profdata;
#   4. the release build with that profile, where
#      `cargo build --release --target TARGET` leaves the binary.
#
# This is the corpus tools/pgo.sh trains the canonical benchmark build on;
# that script adds the provenance contract and the structural comparison of
# the corpus with every scored program, which a release build does not repeat.
# RUSTFLAGS, when set (the workflow's -Dwarnings), applies to both builds.
# Needs the toolchain's llvm-tools component, Python 3 and sha256sum.
set -euo pipefail
cd "$(dirname "$0")/../.."

if [[ $# -ne 1 ]]; then
  echo "usage: bash tools/release/pgo-build.sh TARGET" >&2
  exit 2
fi
target=$1
exe=zipp
[[ $target == *windows* ]] && exe=zipp.exe

# Git Bash on Windows: rustc and llvm-profdata take Windows paths, the shell
# takes POSIX ones.
native_path() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s\n' "$1"; fi
}
shell_path() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -u "$1"; else printf '%s\n' "$1"; fi
}

python=$(command -v python3 || command -v python)
work="$PWD/target/pgo-release"
# A previous run left its stage read-only (tools/pgo_training.py seals it).
if [[ -e $work ]]; then chmod -R u+w "$work"; rm -rf "$work"; fi
mkdir -p "$work/profiles" "$work/profiles-interp" "$work/stage"
# The same directory as cargo and Python (native programs) must be told it.
root_n=$(native_path "$PWD")
work_n=$(native_path "$work")
manifest=bench/pgo-training/expected-output.json
mapfile -t inputs < <("$python" -I -c '
import json, sys
for case in json.load(open(sys.argv[1], encoding="utf-8"))["cases"]:
    print(case["path"])
' "$manifest" | tr -d '\r')
if [[ ${#inputs[@]} -eq 0 ]]; then
  echo "error: $manifest names no training inputs" >&2
  exit 1
fi

sysroot=$(shell_path "$(rustc --print sysroot | tr -d '\r')")
host=$(rustc -vV | tr -d '\r' | awk '$1 == "host:" { print $2 }')
profdata="$sysroot/lib/rustlib/$host/bin/llvm-profdata"
[[ -x $profdata ]] || profdata="$profdata.exe"
if [[ ! -x $profdata ]]; then
  echo "error: llvm-profdata not found in $sysroot (rustup component add llvm-tools)" >&2
  exit 1
fi

echo "== staging ${#inputs[@]} training inputs =="
stage_args=(stage --root "$root_n" --destination "$work_n/stage" --file "$manifest")
for input in "${inputs[@]}"; do stage_args+=(--file "$input"); done
"$python" -I tools/pgo_training.py "${stage_args[@]}"

base_flags=${RUSTFLAGS:-}
echo "== stage 1/4: instrumented build =="
CARGO_TARGET_DIR="$work_n/target" \
  RUSTFLAGS="$base_flags -Cprofile-generate=$work_n/profiles" \
  cargo build --locked --release --package zipp-cli --bin zipp --target "$target"

# train DIR LIST: the corpus once, its profiles in DIR and their list in LIST.
train() {
  local run_args=(run --root "$work_n/stage" --binary "$work_n/target/$target/release/$exe"
    --manifest "$work_n/stage/$manifest" --profile-dir "$1" --profile-list "$2")
  for input in "${inputs[@]}"; do run_args+=(--input "$input"); done
  "$python" -I tools/pgo_training.py "${run_args[@]}"
}
echo "== stage 2/4: training, as the CLI runs it =="
train "$work_n/profiles" "$work_n/profile-list"
echo "== stage 2/4: training, interpreted =="
(export ZIPP_NOJIT=1; train "$work_n/profiles-interp" "$work_n/profile-list-interp")

echo "== stage 3/4: merge =="
profiles=()
for pass in "" -interp; do
  "$python" -I tools/pgo_training.py verify-profiles --profile-dir "$work_n/profiles$pass" \
    --profile-list "$work_n/profile-list$pass" > "$work/verified$pass"
  mapfile -d '' -t pass_profiles < "$work/verified$pass"
  if [[ ${#pass_profiles[@]} -ne ${#inputs[@]} ]]; then
    echo "error: ${#pass_profiles[@]} profiles for ${#inputs[@]} training inputs (pass '$pass')" >&2
    exit 1
  fi
  profiles+=("${pass_profiles[@]}")
done
merged="$work/merged.profdata"
"$profdata" merge -o "$work_n/merged.profdata" "${profiles[@]}"
profile_sha256=$(sha256sum "$merged" | awk '{ print $1 }')
# The corpus as trained: the passes, then the manifest and each input, in
# order.
recipe_sha256=$({
  printf 'passes: default, ZIPP_NOJIT=1\n'
  for f in "$manifest" "${inputs[@]}"; do
    printf '%s %s\n' "$(sha256sum "$f" | awk '{ print $1 }')" "$f"
  done
} | sha256sum | awk '{ print $1 }')
echo "merged profile sha256 $profile_sha256, training corpus sha256 $recipe_sha256"

echo "== stage 4/4: profile-guided release build =="
ZIPP_PGO_PROFILE_SHA256=$profile_sha256 \
  ZIPP_PGO_TRAINING_RECIPE_SHA256=$recipe_sha256 \
  RUSTFLAGS="$base_flags -Cprofile-use=$work_n/merged.profdata" \
  cargo build --locked --release --package zipp-cli --bin zipp --target "$target"
binary="target/$target/release/$exe"
"$binary" --version | grep -E '^(zipp |pgo profile sha256)'
if ! "$binary" --version | grep -q "pgo profile sha256: $profile_sha256"; then
  echo "error: $binary does not carry the merged profile's digest" >&2
  exit 1
fi
