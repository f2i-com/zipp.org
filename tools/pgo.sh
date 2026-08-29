#!/usr/bin/env bash
# Profile-guided release build — the published-headline build since B114.
#
# Measured 2026-08-03 (B114): headline geomean 0.8672x [0.863, 0.870] against
# the identical-source non-PGO build, 21 counterbalanced pairs — the largest
# single lever ever measured in this repo. The gains sit exactly where PGO can
# reach: AOT Rust (interpreter dispatch, MEM-tier helpers, regress exec, JSON,
# GC). The two >=85%-jit-native rows measured flat, which is the expected blind
# spot (runtime-emitted dynasm pages are untouched by construction).
#
# The binary is fully portable (no ISA raise; x86-64-v3 measured NULL, same
# entry). Cost is two builds plus a short training pass. The digest-named merged
# profile lives under target/pgo-profiles and MUST be retrained after any
# nontrivial engine change —
# a stale profile silently degrades toward the non-PGO number, so a headline
# capture should always run this script first, from a clean tree.
#
# Usage (Windows PowerShell; native Git Bash is required, not WSL):
#   & 'C:\Program Files\Git\bin\bash.exe' tools/pgo.sh
# This leaves the binary at target/x86_64-pc-windows-msvc/release/zipp.exe.
# (The explicit --target keeps RUSTFLAGS off host proc-macros — dynasm.)
set -euo pipefail
cd "$(dirname "$0")/.."
export LC_ALL=C

if [[ $# -gt 1 || ($# -eq 1 && "$1" != "--validate-only") ]]; then
  echo "usage: bash tools/pgo.sh [--validate-only]" >&2
  exit 2
fi
VALIDATE_ONLY=0
if [[ ${1-} == "--validate-only" ]]; then
  VALIDATE_ONLY=1
fi

if ! command -v sha256sum >/dev/null 2>&1; then
  echo "error: sha256sum is required to stamp PGO provenance" >&2
  exit 1
fi

sha256_file() {
  sha256sum -- "$1" | awk '{print $1}'
}

sha256_stream() {
  sha256sum | awk '{print $1}'
}

CHECKOUT_ROOT="$(pwd)"
ROOT="$CHECKOUT_ROOT"
TRIPLE=x86_64-pc-windows-msvc
PUBLISHED_BIN="$CHECKOUT_ROOT/target/$TRIPLE/release/zipp.exe"

# Select the actual stable toolchain binaries before clearing the build
# environment. This bypasses aliases and CARGO/RUSTC wrapper variables while
# retaining the project's documented stable-toolchain policy. The executable
# bytes and verbose identities are committed to the build recipe below.
RUSTUP_BIN=
for candidate in "$HOME/.cargo/bin/rustup" "$HOME/.cargo/bin/rustup.exe"; do
  if [[ -x "$candidate" ]]; then
    RUSTUP_BIN="$candidate"
    break
  fi
done
if [[ -z "$RUSTUP_BIN" ]]; then
  RUSTUP_BIN="$(command -v rustup || command -v rustup.exe || true)"
fi
if [[ -z "$RUSTUP_BIN" ]]; then
  echo "error: rustup is required to select the canonical stable toolchain" >&2
  exit 1
fi
PYTHON_BIN="$(command -v python3 || command -v python || true)"
if [[ -z "$PYTHON_BIN" ]]; then
  echo "error: Python 3 is required to validate the scored corpus" >&2
  exit 1
fi
shell_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1"
  else
    printf '%s\n' "$1"
  fi
}
cargo_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -m "$1"
  else
    printf '%s\n' "$1"
  fi
}
RUSTUP_HOME_CANONICAL="$HOME/.rustup"
CARGO_BIN="$(shell_path "$(env -u RUSTUP_TOOLCHAIN RUSTUP_HOME="$RUSTUP_HOME_CANONICAL" "$RUSTUP_BIN" which --toolchain stable cargo)")"
RUSTC_BIN="$(shell_path "$(env -u RUSTUP_TOOLCHAIN RUSTUP_HOME="$RUSTUP_HOME_CANONICAL" "$RUSTUP_BIN" which --toolchain stable rustc)")"
for tool in "$CARGO_BIN" "$RUSTC_BIN"; do
  if [[ ! -f "$tool" ]]; then
    echo "error: rustup returned a missing toolchain executable: $tool" >&2
    exit 1
  fi
done
CARGO_BIN_BUILD="$(cargo_path "$CARGO_BIN")"
CARGO_SHA256="$(sha256_file "$CARGO_BIN")"
RUSTC_SHA256="$(sha256_file "$RUSTC_BIN")"
CARGO_IDENTITY="$("$CARGO_BIN" -Vv | tr '\r\n' '  ' | awk '{$1=$1};1')"
RUSTC_VERBOSE_RAW="$("$RUSTC_BIN" -vV)"
RUSTC_IDENTITY="$(tr '\r\n' '  ' <<< "$RUSTC_VERBOSE_RAW" | awk '{$1=$1};1')"
RUSTC_HOST="$(awk '$1 == "host:" { print $2; exit }' <<< "$RUSTC_VERBOSE_RAW")"
if [[ "$RUSTC_HOST" != "$TRIPLE" ]]; then
  echo "error: PGO requires native $TRIPLE Rust under Git Bash; selected host is ${RUSTC_HOST:-unknown}" >&2
  echo "       run from PowerShell with: & 'C:\Program Files\Git\bin\bash.exe' tools/pgo.sh" >&2
  exit 1
fi
RUSTC_SYSROOT="$(shell_path "$("$RUSTC_BIN" --print sysroot)")"
RUSTC_TARGET_LIBDIR="$(shell_path "$("$RUSTC_BIN" --print target-libdir --target "$TRIPLE")")"
if [[ ! -d "$RUSTC_TARGET_LIBDIR" ]]; then
  echo "error: Rust standard libraries for $TRIPLE are not installed in the selected stable toolchain" >&2
  exit 1
fi
for required_build_env in VCToolsInstallDir WindowsSdkDir LIB INCLUDE; do
  if [[ -z ${!required_build_env-} ]]; then
    echo "error: PGO requires an x64 Visual Studio developer environment ($required_build_env is unset)" >&2
    echo "       start an x64 Developer PowerShell, then invoke native Git Bash" >&2
    exit 1
  fi
done
MSVC_TOOLS_ROOT="$(shell_path "$VCToolsInstallDir")"
MSVC_TOOLS_ROOT="${MSVC_TOOLS_ROOT%/}/bin/Hostx64/x64"
MSVC_CL="$MSVC_TOOLS_ROOT/cl.exe"
MSVC_LIB="$MSVC_TOOLS_ROOT/lib.exe"
for tool in "$MSVC_CL" "$MSVC_LIB"; do
  if [[ ! -f "$tool" || -L "$tool" ]]; then
    echo "error: canonical MSVC tool is missing or redirected: $tool" >&2
    exit 1
  fi
done
MSVC_CL_BUILD="$(cargo_path "$MSVC_CL")"
MSVC_LIB_BUILD="$(cargo_path "$MSVC_LIB")"
MSVC_CL_SHA256="$(sha256_file "$MSVC_CL")"
MSVC_LIB_SHA256="$(sha256_file "$MSVC_LIB")"
MSVC_CL_IDENTITY="$({ "$MSVC_CL" 2>&1 || true; } | tr -d '\r' | awk 'NF {print; exit}' | awk '{$1=$1};1')"
MSVC_LIB_IDENTITY="$({ "$MSVC_LIB" 2>&1 || true; } | tr -d '\r' | awk 'NF {print; exit}' | awk '{$1=$1};1')"
if [[ -z "$MSVC_CL_IDENTITY" || -z "$MSVC_LIB_IDENTITY" ]]; then
  echo "error: canonical MSVC tools did not report identities" >&2
  exit 1
fi
LLVM_PROFDATA=
for candidate in \
  "$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/llvm-profdata" \
  "$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/llvm-profdata.exe"; do
  if [[ -f "$candidate" ]]; then
    LLVM_PROFDATA="$candidate"
    break
  fi
done
if [[ -n "$LLVM_PROFDATA" ]]; then
  LLVM_PROFDATA_SHA256="$(sha256_file "$LLVM_PROFDATA")"
else
  LLVM_PROFDATA_SHA256=
fi
RUST_LLD=
for candidate in \
  "$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/rust-lld" \
  "$RUSTC_SYSROOT/lib/rustlib/$RUSTC_HOST/bin/rust-lld.exe"; do
  if [[ -f "$candidate" ]]; then
    RUST_LLD="$candidate"
    break
  fi
done
if [[ -z "$RUST_LLD" ]]; then
  echo "error: rust-lld for the selected stable compiler was not found" >&2
  exit 1
fi
RUST_LLD_CARGO="$(cargo_path "$RUST_LLD")"
RUST_LLD_SHA256="$(sha256_file "$RUST_LLD")"
RUST_LLD_IDENTITY="$("$RUST_LLD" -flavor link --version | tr '\r\n' '  ' | awk '{$1=$1};1')"

PGO_INPUTS=(
  bench/pgo-training/runtime-mix.js
  bench/pgo-training/text-data-mix.js
  bench/pgo-training/csv-tuple-mix.js
  bench/pgo-training/template-uri-mix.js
  bench/pgo-training/async-dag-mix.js
  bench/pgo-training/memory-shapes-mix.js
  bench/pgo-training/dictionary-mix.js
)
TRAINING_INPUTS=("${PGO_INPUTS[@]}")
CORPUS_VALIDATOR=tools/pgo_corpus.py
TRAINING_RUNNER=tools/pgo_training.py
EXPECTED_OUTPUT_MANIFEST=bench/pgo-training/expected-output.json
PGO_SIMILARITY_POLICY='zipp-pgo-structural-similarity-v1;normalized-js-tokens;10gram;ngram-evidence>=16;function-containment<0.78;whole-containment<0.66;window=96/24@0.82;absolute-run<72;short-run<36-or-0.90;training-source=ascii-lf;training-template-literal=deny;training-unicode-escape=deny;training-html-comment=deny;training-hashbang=deny;training-fnv1a=deny;training-distinctive-numbers=disjoint;training-numeric-tuples=disjoint;training-cooked-strings+regex-bodies=disjoint;training-ambiguous-slash=deny;private-id=atomic'
PGO_RUNNER_POLICY='zipp-pgo-runner-v1;exclusive-readonly-stage;external-code-off;timeout=30s;stdout<=4096;combined-output<=8192;output=manifest;one-profraw-per-input;explicit-hashed-profile-merge;atomic-publish'
PGO_RECIPE_VERSION='zipp-pgo-training-recipe-v7-immutable-source-staged-bounded-external-code-off'
PGO_RECIPE_COMMAND='build both Cargo stages from one private read-only clean-HEAD source snapshot; stage ordered corpus and scored provenance into an exclusive read-only tree; validate staged bytes; run each ordered training input once as zipp js --pgo-training STAGED_INPUT under zipp-pgo-training-env-allowlist-v1; enforce timeout, output caps, exact manifest stdout, and one explicitly hashed profraw per input; merge only enumerated profiles'

# Derive the excluded scored-source set from the same definitions as the two
# publication harnesses: every tracked bench/**/*.js|mjs|cjs outside the training
# directory, plus every hostile input declared by the strictly validated
# manifest, including non-code provenance inputs. A temporary
# file preserves the producer's exit status; `mapfile < <(python ...)` would
# fail open if Python
# rejected a malformed manifest.
if [[ ! -d /tmp ]]; then
  echo "error: canonical PGO builds require a system /tmp directory" >&2
  exit 1
fi
PUBLICATION_LIST=
PUBLICATION_RECHECK=
BUILD_CWD=
BUILD_TARGET_DIR=
CORPUS_STAGE=
PGO_WORK_DIR=
SOURCE_SNAPSHOT_OWNER=
SOURCE_COMMIT=
CLEANUP_RUNNER="$CHECKOUT_ROOT/$TRAINING_RUNNER"

safe_cleanup_file() {
  local path=$1
  if [[ -e "$path" || -L "$path" ]]; then
    if "$PYTHON_BIN" -I "$CLEANUP_RUNNER" check --path "$path" --kind file >/dev/null 2>&1; then
      rm -f -- "$path"
    else
      echo "warning: refusing to clean a replaced PGO temporary file: $path" >&2
    fi
  fi
}

safe_cleanup_dir() {
  local path=$1
  if [[ -e "$path" || -L "$path" ]]; then
    if ! "$PYTHON_BIN" -I "$CLEANUP_RUNNER" remove-tree --path "$path" >/dev/null 2>&1; then
      echo "warning: refusing to clean a replaced PGO temporary directory: $path" >&2
    fi
  fi
}

cleanup() {
  case "$PUBLICATION_RECHECK" in
    /tmp/zipp-pgo-publication-recheck.*) safe_cleanup_file "$PUBLICATION_RECHECK" ;;
  esac
  case "$PUBLICATION_LIST" in
    /tmp/zipp-pgo-publication.*) safe_cleanup_file "$PUBLICATION_LIST" ;;
  esac
  case "$CORPUS_STAGE" in
    /tmp/zipp-pgo-stage.*) safe_cleanup_dir "$CORPUS_STAGE" ;;
  esac
  case "$BUILD_CWD" in
    /tmp/zipp-pgo-cargo.*) safe_cleanup_dir "$BUILD_CWD" ;;
  esac
  case "$BUILD_TARGET_DIR" in
    "$CHECKOUT_ROOT"/target/pgo-cargo-target.*) safe_cleanup_dir "$BUILD_TARGET_DIR" ;;
  esac
  case "$PGO_WORK_DIR" in
    "$CHECKOUT_ROOT"/target/pgo-work.*) safe_cleanup_dir "$PGO_WORK_DIR" ;;
  esac
  case "$SOURCE_SNAPSHOT_OWNER" in
    /tmp/zipp-pgo-source.*) safe_cleanup_dir "$SOURCE_SNAPSHOT_OWNER" ;;
  esac
}
trap cleanup EXIT

"$PYTHON_BIN" -I "$TRAINING_RUNNER" check --path /tmp --kind directory

checkout_is_clean_at_commit() {
  local current_head
  current_head="$(git -C "$CHECKOUT_ROOT" rev-parse --verify HEAD 2>/dev/null || true)"
  [[ -n "$SOURCE_COMMIT" && "$current_head" == "$SOURCE_COMMIT" ]] || return 1
  [[ -z "$(git -C "$CHECKOUT_ROOT" status --porcelain=v1 --untracked-files=all --ignore-submodules=none)" ]]
}

if [[ $VALIDATE_ONLY -eq 0 ]]; then
  if ! command -v git >/dev/null 2>&1; then
    echo "error: canonical PGO builds require Git to materialize the clean source commit" >&2
    exit 1
  fi
  SOURCE_COMMIT="$(git -C "$CHECKOUT_ROOT" rev-parse --verify HEAD)"
  if ! checkout_is_clean_at_commit; then
    echo "error: canonical PGO publication requires the original checkout at an exact clean HEAD" >&2
    echo "       use --validate-only for dirty development-tree corpus validation" >&2
    exit 1
  fi
  SOURCE_SNAPSHOT_OWNER="$(mktemp -d /tmp/zipp-pgo-source.XXXXXX)"
  "$PYTHON_BIN" -I "$CLEANUP_RUNNER" check --path "$SOURCE_SNAPSHOT_OWNER" --kind directory --empty
  SOURCE_ROOT="$SOURCE_SNAPSHOT_OWNER/repository"
  git -c core.hooksPath=/dev/null clone --quiet --no-hardlinks --no-checkout \
    -- "$CHECKOUT_ROOT" "$SOURCE_ROOT"
  git -C "$SOURCE_ROOT" -c core.hooksPath=/dev/null -c core.autocrlf=false \
    checkout --quiet --detach --force "$SOURCE_COMMIT"
  if [[ "$(git -C "$SOURCE_ROOT" rev-parse --verify HEAD)" != "$SOURCE_COMMIT" \
     || -n "$(git -C "$SOURCE_ROOT" status --porcelain=v1 --untracked-files=all --ignore-submodules=none)" ]]; then
    echo "error: private PGO source snapshot is not the requested clean commit" >&2
    exit 1
  fi
  if find "$SOURCE_ROOT" -path "$SOURCE_ROOT/.git" -prune -o -type l -print -quit | grep -q .; then
    echo "error: private PGO source snapshot contains a source-tree symlink" >&2
    exit 1
  fi
  find "$SOURCE_ROOT" -path "$SOURCE_ROOT/.git" -prune -o -type f -exec chmod a-w -- {} +
  find "$SOURCE_ROOT" -path "$SOURCE_ROOT/.git" -prune -o -type d -exec chmod a-w -- {} +
  ROOT="$SOURCE_ROOT"
  CLEANUP_RUNNER="$SOURCE_ROOT/$TRAINING_RUNNER"
  cd "$ROOT"
else
  echo "note: --validate-only checks the current development tree and cannot publish a PGO binary"
fi

PUBLICATION_LIST="$(mktemp /tmp/zipp-pgo-publication.XXXXXX)"
"$PYTHON_BIN" -I "$TRAINING_RUNNER" check --path "$PUBLICATION_LIST" --kind file
BUILD_CWD="$(mktemp -d /tmp/zipp-pgo-cargo.XXXXXX)"
"$PYTHON_BIN" -I "$TRAINING_RUNNER" check --path "$BUILD_CWD" --kind directory --empty
CORPUS_STAGE="$(mktemp -d /tmp/zipp-pgo-stage.XXXXXX)"
"$PYTHON_BIN" -I "$TRAINING_RUNNER" check --path "$CORPUS_STAGE" --kind directory --empty
mkdir -p "$CHECKOUT_ROOT/target"
"$PYTHON_BIN" -I "$TRAINING_RUNNER" check --path "$CHECKOUT_ROOT/target" --kind directory
BUILD_TARGET_DIR="$(mktemp -d "$CHECKOUT_ROOT/target/pgo-cargo-target.XXXXXX")"
"$PYTHON_BIN" -I "$TRAINING_RUNNER" check --path "$BUILD_TARGET_DIR" --kind directory --empty
BIN="$BUILD_TARGET_DIR/$TRIPLE/release/zipp.exe"

# Cargo walks every ancestor of its invocation directory looking for .cargo
# config. Fail closed even on a machine whose system temporary hierarchy has
# been customized.
config_search="$BUILD_CWD"
while :; do
  for cargo_config in "$config_search/.cargo/config" "$config_search/.cargo/config.toml"; do
    if [[ -e "$cargo_config" || -L "$cargo_config" ]]; then
      echo "error: canonical PGO builds reject discovered Cargo config: $cargo_config" >&2
      exit 1
    fi
  done
  parent="$(dirname "$config_search")"
  [[ "$parent" == "$config_search" ]] && break
  config_search="$parent"
done
derive_publication_inputs() {
  "$PYTHON_BIN" -I - "$ROOT" <<'PY'
import importlib.util
import os
import subprocess
import sys
from pathlib import Path, PurePosixPath

root = Path(sys.argv[1]).resolve(strict=True)
bench_root = (root / "bench").resolve(strict=True)
listed = subprocess.run(
    ["git", "-C", str(root), "ls-files", "-z", "--", "bench"],
    check=True,
    stdout=subprocess.PIPE,
).stdout
paths = set()
for encoded in (name for name in listed.split(b"\0") if name):
    relative = os.fsdecode(encoded)
    pure = PurePosixPath(relative)
    if (
        len(pure.parts) < 2
        or pure.parts[0] != "bench"
        or pure.parts[1] == "pgo-training"
        or pure.suffix.lower() not in (".js", ".mjs", ".cjs")
    ):
        continue
    path = root.joinpath(*pure.parts)
    resolved = path.resolve(strict=True)
    try:
        resolved.relative_to(bench_root)
    except ValueError as exc:
        raise SystemExit(f"error: benchmark escapes bench/: {path}") from exc
    if path.is_symlink() or not resolved.is_file():
        raise SystemExit(f"error: benchmark is not a regular file: {path}")
    paths.add(resolved)
if not paths:
    raise SystemExit("error: canonical tracked JavaScript benchmark set is empty")

harness_path = root / "tools" / "bench_hostile.py"
spec = importlib.util.spec_from_file_location("zipp_pgo_hostile_manifest", harness_path)
if spec is None or spec.loader is None:
    raise SystemExit("error: cannot load the hostile manifest validator")
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
manifest = module.load_manifest(root / "bench" / "hostile" / "manifest.json")

for case in manifest.cases:
    paths.update(path.resolve(strict=True) for path in case.inputs)
if not paths:
    raise SystemExit("error: canonical scored input set is empty")
for path in sorted(paths, key=lambda item: item.relative_to(root).as_posix()):
    try:
        relative = path.relative_to(root).as_posix()
    except ValueError as exc:
        raise SystemExit(f"error: scored input escapes repository: {path}") from exc
    if any(ch in relative for ch in "\r\n\0"):
        raise SystemExit(f"error: scored input has a non-portable name: {relative!r}")
    sys.stdout.buffer.write(relative.encode("utf-8") + b"\0")
PY
}
if ! derive_publication_inputs > "$PUBLICATION_LIST"
then
  echo "error: could not derive the canonical scored input set" >&2
  exit 1
fi
mapfile -d '' -t PUBLICATION_INPUTS < "$PUBLICATION_LIST"
if [[ ${#PUBLICATION_INPUTS[@]} -eq 0 ]]; then
  echo "error: canonical scored input set is empty" >&2
  exit 1
fi

# Copy every recipe-relevant source into an exclusive tree before validation.
# Training subsequently opens only these read-only staged bytes, preventing a
# mutable checkout path from being swapped for launch and restored afterward.
STAGE_FILES=()
declare -A STAGE_SEEN=()
add_stage_file() {
  local path=$1
  if [[ -z ${STAGE_SEEN[$path]+x} ]]; then
    STAGE_FILES+=("$path")
    STAGE_SEEN[$path]=1
  fi
}
for input in "${TRAINING_INPUTS[@]}"; do add_stage_file "$input"; done
for input in "${PUBLICATION_INPUTS[@]}"; do add_stage_file "$input"; done
add_stage_file bench/hostile/manifest.json
add_stage_file "$EXPECTED_OUTPUT_MANIFEST"
add_stage_file "$CORPUS_VALIDATOR"
add_stage_file "$TRAINING_RUNNER"
add_stage_file tools/pgo.sh
STAGE_ARGS=(stage --root "$ROOT" --destination "$CORPUS_STAGE")
for input in "${STAGE_FILES[@]}"; do STAGE_ARGS+=(--file "$input"); done
"$PYTHON_BIN" -I "$TRAINING_RUNNER" "${STAGE_ARGS[@]}"

STAGED_CORPUS_VALIDATOR="$CORPUS_STAGE/$CORPUS_VALIDATOR"
STAGED_TRAINING_RUNNER="$CORPUS_STAGE/$TRAINING_RUNNER"
STAGED_EXPECTED_OUTPUT_MANIFEST="$CORPUS_STAGE/$EXPECTED_OUTPUT_MANIFEST"

# Refuse literal reuse, normalized structural clones, and ambiguous source
# spellings before any training process runs.
CORPUS_VALIDATE_ARGS=(validate --root "$CORPUS_STAGE")
for input in "${TRAINING_INPUTS[@]}"; do
  CORPUS_VALIDATE_ARGS+=(--training "$input")
done
for input in "${PUBLICATION_INPUTS[@]}"; do
  CORPUS_VALIDATE_ARGS+=(--scored "$input")
done
if ! "$PYTHON_BIN" -I "$STAGED_CORPUS_VALIDATOR" "${CORPUS_VALIDATE_ARGS[@]}"; then
  echo "error: PGO training corpus failed anti-leakage validation" >&2
  exit 1
fi

staged_snapshot_sha256() {
  {
    for input in "${STAGE_FILES[@]}"; do
      printf '%s\0%s\0' "$input" "$(sha256_file "$CORPUS_STAGE/$input")"
    done
  } | sha256_stream
}
STAGED_SNAPSHOT_SHA256="$(staged_snapshot_sha256)"

verify_staged_sources_unchanged() {
  local input
  if [[ "$(staged_snapshot_sha256)" != "$STAGED_SNAPSHOT_SHA256" ]]; then
    echo "error: read-only PGO staging bytes changed during the build" >&2
    return 1
  fi
  for input in "${STAGE_FILES[@]}"; do
    if [[ "$(sha256_file "$input")" != "$(sha256_file "$CORPUS_STAGE/$input")" ]]; then
      echo "error: repository input diverged from the staged PGO bytes: $input" >&2
      return 1
    fi
  done
}

# Hash the recipe implementation, stable relative input names, and every input
# digest. Absolute checkout paths never enter this identity. The merged profile
# gets its own hash below, so two runs also remain distinguishable if runtime
# conditions produce different counters from the same recipe.
training_recipe_sha256() {
  {
    printf '%s\0' "$PGO_RECIPE_VERSION"
    printf '%s\0' "$PGO_RECIPE_COMMAND"
    printf '%s\0%s\0' 'tools/pgo.sh' "$(sha256_file "$CORPUS_STAGE/tools/pgo.sh")"
    printf '%s\0' "$PGO_SIMILARITY_POLICY"
    printf '%s\0%s\0' "$CORPUS_VALIDATOR" "$(sha256_file "$STAGED_CORPUS_VALIDATOR")"
    printf '%s\0' "$PGO_RUNNER_POLICY"
    printf '%s\0%s\0' "$TRAINING_RUNNER" "$(sha256_file "$STAGED_TRAINING_RUNNER")"
    printf '%s\0%s\0' "$EXPECTED_OUTPUT_MANIFEST" "$(sha256_file "$STAGED_EXPECTED_OUTPUT_MANIFEST")"
    for input in "${TRAINING_INPUTS[@]}"; do
      printf '%s\0%s\0' "$input" "$(sha256_file "$CORPUS_STAGE/$input")"
    done
    printf '%s\0%s\0' 'bench/hostile/manifest.json' "$(sha256_file "$CORPUS_STAGE/bench/hostile/manifest.json")"
    printf '%s\0' 'excluded-publication-inputs'
    for input in "${PUBLICATION_INPUTS[@]}"; do
      printf '%s\0%s\0' "$input" "$(sha256_file "$CORPUS_STAGE/$input")"
    done
  } | sha256_stream
}
TRAINING_RECIPE_SHA256="$(training_recipe_sha256)"

verify_training_recipe_unchanged() {
  local current_recipe
  PUBLICATION_RECHECK="$(mktemp /tmp/zipp-pgo-publication-recheck.XXXXXX)"
  "$PYTHON_BIN" -I "$TRAINING_RUNNER" check --path "$PUBLICATION_RECHECK" --kind file
  if ! derive_publication_inputs > "$PUBLICATION_RECHECK"; then
    safe_cleanup_file "$PUBLICATION_RECHECK"
    PUBLICATION_RECHECK=
    echo "error: scored corpus became invalid during the PGO build" >&2
    return 1
  fi
  if [[ "$(sha256_file "$PUBLICATION_RECHECK")" != "$(sha256_file "$PUBLICATION_LIST")" ]]; then
    safe_cleanup_file "$PUBLICATION_RECHECK"
    PUBLICATION_RECHECK=
    echo "error: scored corpus membership changed during the PGO build" >&2
    return 1
  fi
  safe_cleanup_file "$PUBLICATION_RECHECK"
  PUBLICATION_RECHECK=
  verify_staged_sources_unchanged
  current_recipe="$(training_recipe_sha256)"
  if [[ "$current_recipe" != "$TRAINING_RECIPE_SHA256" ]]; then
    echo "error: PGO recipe, training input, manifest, or excluded scored input changed during the build" >&2
    return 1
  fi
}

repository_snapshot_sha256() {
  local snapshot_root=${1:-$ROOT}
  "$PYTHON_BIN" -I - "$snapshot_root" <<'PY'
import hashlib
import os
import subprocess
import sys
from pathlib import Path, PurePosixPath

root = Path(sys.argv[1]).resolve(strict=True)
listed = subprocess.run(
    ["git", "-C", str(root), "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
    check=True,
    stdout=subprocess.PIPE,
).stdout
names = sorted(name for name in listed.split(b"\0") if name)
digest = hashlib.sha256(b"zipp-pgo-repository-snapshot-v1\0")
for name in names:
    relative = os.fsdecode(name)
    pure = PurePosixPath(relative)
    path = root.joinpath(*pure.parts)
    digest.update(name)
    digest.update(b"\0")
    if path.is_symlink():
        raise SystemExit(f"error: canonical PGO source snapshot rejects symlink: {relative}")
    if path.is_file():
        digest.update(b"file\0")
        content = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                content.update(chunk)
        digest.update(content.digest())
    else:
        digest.update(b"missing-or-special")
    digest.update(b"\0")
sys.stdout.buffer.write(digest.hexdigest().encode("ascii"))
PY
}

SOURCE_SNAPSHOT_SHA256="$(repository_snapshot_sha256 "$ROOT")"
verify_source_and_tools_unchanged() {
  if [[ "$(repository_snapshot_sha256 "$ROOT")" != "$SOURCE_SNAPSHOT_SHA256" ]]; then
    echo "error: private PGO source snapshot changed during the build" >&2
    return 1
  fi
  if [[ $VALIDATE_ONLY -eq 0 ]]; then
    if ! checkout_is_clean_at_commit; then
      echo "error: original checkout HEAD or cleanliness changed during the PGO build" >&2
      return 1
    fi
    if [[ "$(git -C "$ROOT" rev-parse --verify HEAD)" != "$SOURCE_COMMIT" \
       || -n "$(git -C "$ROOT" status --porcelain=v1 --untracked-files=all --ignore-submodules=none)" ]]; then
      echo "error: private PGO source snapshot no longer represents the clean source commit" >&2
      return 1
    fi
  fi
  if [[ "$(sha256_file "$CARGO_BIN")" != "$CARGO_SHA256" \
     || "$(sha256_file "$RUSTC_BIN")" != "$RUSTC_SHA256" \
     || "$(sha256_file "$RUST_LLD")" != "$RUST_LLD_SHA256" \
     || "$(sha256_file "$MSVC_CL")" != "$MSVC_CL_SHA256" \
     || "$(sha256_file "$MSVC_LIB")" != "$MSVC_LIB_SHA256" ]]; then
    echo "error: selected Cargo/rustc/linker/MSVC tool bytes changed during the PGO build" >&2
    return 1
  fi
  if [[ -n "$LLVM_PROFDATA" \
     && "$(sha256_file "$LLVM_PROFDATA")" != "$LLVM_PROFDATA_SHA256" ]]; then
    echo "error: selected llvm-profdata bytes changed during the PGO build" >&2
    return 1
  fi
}

# Cargo discovers config from its invocation directory rather than from
# --manifest-path. Run it in an empty directory outside the checkout and reject
# config in the canonical Cargo home, so a user-level linker/wrapper/rustflags
# setting cannot silently alter a publication build. Workspace .cargo config is
# intentionally not discovered; every applicable setting is explicit below.
CARGO_HOME_CANONICAL="$HOME/.cargo"
for cargo_config in "$CARGO_HOME_CANONICAL/config" "$CARGO_HOME_CANONICAL/config.toml"; do
  if [[ -e "$cargo_config" || -L "$cargo_config" ]]; then
    echo "error: canonical PGO builds reject Cargo home config: $cargo_config" >&2
    exit 1
  fi
done

PGO_BUILD_CONTRACT='zipp-pgo-build-v2;cargo=build --locked --release --target=x86_64-pc-windows-msvc --package=zipp-cli --bin=zipp --no-default-features;profile=opt-level=3,lto=fat,codegen-units=1,panic=abort,incremental=false,debug=false,strip=none,debug-assertions=false,overflow-checks=false;rustflags=target-cpu=x86-64,linker-flavor=lld-link,profile-use=<verified-profile>;linker=selected-rustc-rust-lld;cc-rs=target-specific-selected-msvc-cl+lib;source=private-readonly-clean-head-snapshot-v1;cargo-config=controlled-cwd+no-home-config;target-dir=fresh;sdk=validated-environment-paths-not-byte-manifested;env=allowlist-v2'
PGO_BUILD_ENV_POLICY='zipp-pgo-build-env-allowlist-v2'
FORWARDED_ENV_NAMES=(
  PATH HOME USERPROFILE SystemRoot SYSTEMROOT WINDIR COMSPEC PATHEXT TEMP TMP TMPDIR
  LOCALAPPDATA APPDATA PROGRAMFILES PROGRAMDATA
  INCLUDE LIB LIBPATH VCToolsInstallDir VCINSTALLDIR WindowsSdkDir
  WindowsSDKVersion UniversalCRTSdkDir UCRTVersion VisualStudioVersion
)
BUILD_ENV_ARGS=()
for name in "${FORWARDED_ENV_NAMES[@]}"; do
  if [[ -v $name ]]; then
    BUILD_ENV_ARGS+=("$name=${!name}")
  fi
done
MSVC_TARGET_ENV_ARGS=(
  "CC_x86_64_pc_windows_msvc=$MSVC_CL_BUILD"
  "CXX_x86_64_pc_windows_msvc=$MSVC_CL_BUILD"
  "AR_x86_64_pc_windows_msvc=$MSVC_LIB_BUILD"
)
BUILD_ENV_SHA256="$(
  {
    printf '%s\0' "$PGO_BUILD_ENV_POLICY"
    for assignment in "${BUILD_ENV_ARGS[@]}"; do
      printf '%s\0' "$assignment"
    done
    for assignment in "${MSVC_TARGET_ENV_ARGS[@]}"; do
      printf '%s\0' "$assignment"
    done
    printf '%s\0%s\0' CARGO_HOME "$CARGO_HOME_CANONICAL"
    printf '%s\0%s\0' CARGO_TARGET_DIR '<fresh-isolated-target>'
    printf '%s\0%s\0' CARGO_INCREMENTAL 0
    printf '%s\0%s\0' CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER "$RUST_LLD_CARGO"
    printf '%s\0%s\0' LC_ALL C
    printf '%s\0%s\0' LANG C
    printf '%s\0%s\0' TZ UTC
  } | sha256_stream
)"

CARGO_BUILD_ARGS=(
  build
  --manifest-path "$ROOT/Cargo.toml"
  --locked
  --release
  --target "$TRIPLE"
  --package zipp-cli
  --bin zipp
  --no-default-features
  --config 'profile.release.opt-level=3'
  --config 'profile.release.lto="fat"'
  --config 'profile.release.codegen-units=1'
  --config 'profile.release.panic="abort"'
  --config 'profile.release.incremental=false'
  --config 'profile.release.debug=false'
  --config 'profile.release.strip="none"'
  --config 'profile.release.debug-assertions=false'
  --config 'profile.release.overflow-checks=false'
)
BUILD_DEFINITION_FILES=(
  Cargo.toml
  Cargo.lock
  crates/zipp-cli/Cargo.toml
  crates/zipp-cli/build.rs
  crates/zipp-vm/Cargo.toml
  crates/regress-fork/Cargo.toml
)
for definition in "${BUILD_DEFINITION_FILES[@]}"; do
  if [[ ! -f "$definition" ]]; then
    echo "error: missing canonical build definition: $definition" >&2
    exit 1
  fi
done

run_cargo_build() {
  local encoded_rustflags=$1
  local profile_sha256=$2
  local build_recipe_sha256=$3
  (
    cd "$BUILD_CWD"
    env -i \
      "${BUILD_ENV_ARGS[@]}" \
      "${MSVC_TARGET_ENV_ARGS[@]}" \
      CARGO_HOME="$CARGO_HOME_CANONICAL" \
      CARGO_TARGET_DIR="$BUILD_TARGET_DIR" \
      CARGO_INCREMENTAL=0 \
      CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER="$RUST_LLD_CARGO" \
      LC_ALL=C LANG=C TZ=UTC \
      RUSTC="$RUSTC_BIN" \
      CARGO_ENCODED_RUSTFLAGS="$encoded_rustflags" \
      ZIPP_PGO_PROFILE_SHA256="$profile_sha256" \
      ZIPP_PGO_TRAINING_RECIPE_SHA256="$TRAINING_RECIPE_SHA256" \
      ZIPP_PGO_BUILD_RECIPE_SHA256="$build_recipe_sha256" \
      ZIPP_PGO_BUILD_CONTRACT="$PGO_BUILD_CONTRACT" \
      ZIPP_PGO_BUILD_ENV_POLICY="$PGO_BUILD_ENV_POLICY" \
      ZIPP_PGO_BUILD_ENV_SHA256="$BUILD_ENV_SHA256" \
      ZIPP_PGO_SOURCE_SNAPSHOT_SHA256="$SOURCE_SNAPSHOT_SHA256" \
      ZIPP_PGO_CARGO_IDENTITY="$CARGO_IDENTITY" \
      ZIPP_PGO_CARGO_PATH="$CARGO_BIN_BUILD" \
      ZIPP_PGO_CARGO_SHA256="$CARGO_SHA256" \
      ZIPP_PGO_RUSTC_SHA256="$RUSTC_SHA256" \
      ZIPP_PGO_LINKER_PATH="$RUST_LLD_CARGO" \
      ZIPP_PGO_LINKER_IDENTITY="$RUST_LLD_IDENTITY" \
      ZIPP_PGO_LINKER_SHA256="$RUST_LLD_SHA256" \
      ZIPP_PGO_MSVC_CL_PATH="$MSVC_CL_BUILD" \
      ZIPP_PGO_MSVC_CL_IDENTITY="$MSVC_CL_IDENTITY" \
      ZIPP_PGO_MSVC_CL_SHA256="$MSVC_CL_SHA256" \
      ZIPP_PGO_MSVC_LIB_PATH="$MSVC_LIB_BUILD" \
      ZIPP_PGO_MSVC_LIB_IDENTITY="$MSVC_LIB_IDENTITY" \
      ZIPP_PGO_MSVC_LIB_SHA256="$MSVC_LIB_SHA256" \
      "$CARGO_BIN" "${CARGO_BUILD_ARGS[@]}"
  )
}

echo "excluded scored inputs: ${#PUBLICATION_INPUTS[@]}"
echo "training recipe/input sha256: $TRAINING_RECIPE_SHA256"
echo "canonical cargo sha256: $CARGO_SHA256"
echo "canonical rustc sha256: $RUSTC_SHA256"
echo "canonical linker sha256: $RUST_LLD_SHA256"
echo "canonical MSVC cl sha256: $MSVC_CL_SHA256"
echo "canonical MSVC lib sha256: $MSVC_LIB_SHA256"
echo "canonical build environment sha256: $BUILD_ENV_SHA256"
echo "repository source snapshot sha256: $SOURCE_SNAPSHOT_SHA256"
if [[ $VALIDATE_ONLY -eq 1 ]]; then
  echo "PGO provenance validation passed (build not run)"
  exit 0
fi
if [[ -z "$LLVM_PROFDATA" ]]; then
  echo "error: llvm-profdata for the selected stable compiler was not found; install the llvm-tools-preview component" >&2
  exit 1
fi

PGO_WORK_DIR="$(mktemp -d "$CHECKOUT_ROOT/target/pgo-work.XXXXXX")"
"$PYTHON_BIN" -I "$STAGED_TRAINING_RUNNER" check --path "$PGO_WORK_DIR" --kind directory --empty
PGODIR="$PGO_WORK_DIR/raw"
PROFDATA="$PGO_WORK_DIR/merged.profdata"
PROFILE_LIST="$PGO_WORK_DIR/profile-list"
VERIFIED_PROFILE_PATHS="$PGO_WORK_DIR/verified-profile-paths"
mkdir "$PGODIR"
"$PYTHON_BIN" -I "$STAGED_TRAINING_RUNNER" check --path "$PGODIR" --kind directory --empty

echo "== stage 1/4: instrumented build =="
PGODIR_RUSTC="$(cargo_path "$PGODIR")"
GENERATE_RUSTFLAGS="-Cprofile-generate=$PGODIR_RUSTC"$'\x1f''-Ctarget-cpu=x86-64'$'\x1f''-Clinker-flavor=lld-link'
run_cargo_build "$GENERATE_RUSTFLAGS" "" ""
verify_training_recipe_unchanged
verify_source_and_tools_unchanged

echo "== stage 2/4: training (7 structural-similarity-guarded workloads) =="
# Exact publication inputs are never launched here. The runner invokes only
# immutable staged programs with runtime compilation/module loading disabled,
# bounded output/time, and byte-exact expected stdout.
TRAINING_RUN_ARGS=(
  run
  --root "$CORPUS_STAGE"
  --binary "$BIN"
  --manifest "$STAGED_EXPECTED_OUTPUT_MANIFEST"
  --profile-dir "$PGODIR"
  --profile-list "$PROFILE_LIST"
)
for input in "${TRAINING_INPUTS[@]}"; do
  TRAINING_RUN_ARGS+=(--input "$input")
done
env -i \
  "${BUILD_ENV_ARGS[@]}" \
  LC_ALL=C LANG=C TZ=UTC \
  "$PYTHON_BIN" -I "$STAGED_TRAINING_RUNNER" "${TRAINING_RUN_ARGS[@]}"
verify_training_recipe_unchanged
verify_source_and_tools_unchanged

echo "== stage 3/4: merge profiles =="
if ! "$PYTHON_BIN" -I "$STAGED_TRAINING_RUNNER" verify-profiles \
  --profile-dir "$PGODIR" --profile-list "$PROFILE_LIST" > "$VERIFIED_PROFILE_PATHS"; then
  echo "error: generated PGO profile set failed enumeration/hash validation" >&2
  exit 1
fi
mapfile -d '' -t PROFILE_PATHS < "$VERIFIED_PROFILE_PATHS"
if [[ ${#PROFILE_PATHS[@]} -ne ${#TRAINING_INPUTS[@]} ]]; then
  echo "error: generated profile count does not match the ordered training corpus" >&2
  exit 1
fi
"$LLVM_PROFDATA" merge -o "$PROFDATA" "${PROFILE_PATHS[@]}"
"$PYTHON_BIN" -I "$STAGED_TRAINING_RUNNER" verify-profiles \
  --profile-dir "$PGODIR" --profile-list "$PROFILE_LIST" > /dev/null
"$PYTHON_BIN" -I "$STAGED_TRAINING_RUNNER" check --path "$PROFDATA" --kind file
PROFILE_SHA256="$(sha256_file "$PROFDATA")"
PROFILE_SNAPSHOT_DIR="$CHECKOUT_ROOT/target/pgo-profiles"
PROFILE_SNAPSHOT="$PROFILE_SNAPSHOT_DIR/$PROFILE_SHA256.profdata"
verify_training_recipe_unchanged
verify_source_and_tools_unchanged
mkdir -p "$PROFILE_SNAPSHOT_DIR"
"$PYTHON_BIN" -I "$STAGED_TRAINING_RUNNER" check --path "$PROFILE_SNAPSHOT_DIR" --kind directory
"$PYTHON_BIN" -I "$STAGED_TRAINING_RUNNER" publish \
  --source "$PROFDATA" --destination "$PROFILE_SNAPSHOT" \
  --readonly --reuse-identical
if [[ "$(sha256_file "$PROFILE_SNAPSHOT")" != "$PROFILE_SHA256" ]]; then
  echo "error: atomically published PGO profile failed digest verification" >&2
  exit 1
fi
PROFDATA_RUSTC="$(cargo_path "$PROFILE_SNAPSHOT")"
echo "merged profile sha256: $PROFILE_SHA256"
echo "training recipe/input sha256: $TRAINING_RECIPE_SHA256"
verify_training_recipe_unchanged
verify_source_and_tools_unchanged

echo "== stage 4/4: optimized build =="
build_recipe_sha256() {
  {
    printf '%s\0' 'zipp-pgo-build-recipe-v2'
    printf '%s\0' "$PGO_BUILD_CONTRACT"
    printf '%s\0%s\0' 'tools/pgo.sh' "$(sha256_file tools/pgo.sh)"
    printf '%s\0%s\0' 'pgo-training-recipe-sha256' "$TRAINING_RECIPE_SHA256"
    printf '%s\0%s\0' 'pgo-profile-sha256' "$PROFILE_SHA256"
    printf '%s\0%s\0' 'cargo-identity' "$CARGO_IDENTITY"
    printf '%s\0%s\0' 'cargo-sha256' "$CARGO_SHA256"
    printf '%s\0%s\0' 'rustc-identity' "$RUSTC_IDENTITY"
    printf '%s\0%s\0' 'rustc-sha256' "$RUSTC_SHA256"
    printf '%s\0%s\0' 'linker-identity' "$RUST_LLD_IDENTITY"
    printf '%s\0%s\0' 'linker-sha256' "$RUST_LLD_SHA256"
    printf '%s\0%s\0' 'msvc-cl-identity' "$MSVC_CL_IDENTITY"
    printf '%s\0%s\0' 'msvc-cl-sha256' "$MSVC_CL_SHA256"
    printf '%s\0%s\0' 'msvc-lib-identity' "$MSVC_LIB_IDENTITY"
    printf '%s\0%s\0' 'msvc-lib-sha256' "$MSVC_LIB_SHA256"
    printf '%s\0%s\0' 'source-snapshot-sha256' "$SOURCE_SNAPSHOT_SHA256"
    printf '%s\0%s\0' 'build-environment-policy' "$PGO_BUILD_ENV_POLICY"
    printf '%s\0%s\0' 'build-environment-sha256' "$BUILD_ENV_SHA256"
    for definition in "${BUILD_DEFINITION_FILES[@]}"; do
      printf '%s\0%s\0' "$definition" "$(sha256_file "$definition")"
    done
  } | sha256_stream
}
BUILD_RECIPE_SHA256="$(build_recipe_sha256)"
USE_RUSTFLAGS="-Cprofile-use=$PROFDATA_RUSTC"$'\x1f''-Ctarget-cpu=x86-64'$'\x1f''-Clinker-flavor=lld-link'
run_cargo_build "$USE_RUSTFLAGS" "$PROFILE_SHA256" "$BUILD_RECIPE_SHA256"
verify_training_recipe_unchanged
verify_source_and_tools_unchanged
if [[ "$(sha256_file "$PROFILE_SNAPSHOT")" != "$PROFILE_SHA256" ]]; then
  echo "error: digest-named PGO profile changed during the optimized build" >&2
  exit 1
fi
if [[ "$(build_recipe_sha256)" != "$BUILD_RECIPE_SHA256" ]]; then
  echo "error: canonical Cargo/build definition changed during the optimized build" >&2
  exit 1
fi

ZIPP_PGO_IDENTITY_JSON="$("$BIN" --version --json)" \
ZIPP_PGO_EXPECTED_PROFILE_SHA256="$PROFILE_SHA256" \
ZIPP_PGO_EXPECTED_RECIPE_SHA256="$TRAINING_RECIPE_SHA256" \
ZIPP_PGO_EXPECTED_BUILD_RECIPE_SHA256="$BUILD_RECIPE_SHA256" \
ZIPP_PGO_EXPECTED_BUILD_CONTRACT="$PGO_BUILD_CONTRACT" \
ZIPP_PGO_EXPECTED_BUILD_ENV_POLICY="$PGO_BUILD_ENV_POLICY" \
ZIPP_PGO_EXPECTED_BUILD_ENV_SHA256="$BUILD_ENV_SHA256" \
ZIPP_PGO_EXPECTED_CARGO_IDENTITY="$CARGO_IDENTITY" \
ZIPP_PGO_EXPECTED_CARGO_SHA256="$CARGO_SHA256" \
ZIPP_PGO_EXPECTED_RUSTC_SHA256="$RUSTC_SHA256" \
ZIPP_PGO_EXPECTED_LINKER_SHA256="$RUST_LLD_SHA256" \
ZIPP_PGO_EXPECTED_LINKER_IDENTITY="$RUST_LLD_IDENTITY" \
ZIPP_PGO_EXPECTED_MSVC_CL_SHA256="$MSVC_CL_SHA256" \
ZIPP_PGO_EXPECTED_MSVC_CL_IDENTITY="$MSVC_CL_IDENTITY" \
ZIPP_PGO_EXPECTED_MSVC_LIB_SHA256="$MSVC_LIB_SHA256" \
ZIPP_PGO_EXPECTED_MSVC_LIB_IDENTITY="$MSVC_LIB_IDENTITY" \
ZIPP_PGO_EXPECTED_SOURCE_SNAPSHOT_SHA256="$SOURCE_SNAPSHOT_SHA256" \
"$PYTHON_BIN" -I - <<'PY'
import json
import os
import re

identity = json.loads(os.environ["ZIPP_PGO_IDENTITY_JSON"])
expected_profile = os.environ["ZIPP_PGO_EXPECTED_PROFILE_SHA256"]
expected_recipe = os.environ["ZIPP_PGO_EXPECTED_RECIPE_SHA256"]
expected_build_recipe = os.environ["ZIPP_PGO_EXPECTED_BUILD_RECIPE_SHA256"]
expected_contract = os.environ["ZIPP_PGO_EXPECTED_BUILD_CONTRACT"]
expected_env_policy = os.environ["ZIPP_PGO_EXPECTED_BUILD_ENV_POLICY"]
expected_env = os.environ["ZIPP_PGO_EXPECTED_BUILD_ENV_SHA256"]
expected_cargo_identity = os.environ["ZIPP_PGO_EXPECTED_CARGO_IDENTITY"]
expected_cargo = os.environ["ZIPP_PGO_EXPECTED_CARGO_SHA256"]
expected_rustc = os.environ["ZIPP_PGO_EXPECTED_RUSTC_SHA256"]
expected_linker = os.environ["ZIPP_PGO_EXPECTED_LINKER_SHA256"]
expected_linker_identity = os.environ["ZIPP_PGO_EXPECTED_LINKER_IDENTITY"]
expected_msvc_cl = os.environ["ZIPP_PGO_EXPECTED_MSVC_CL_SHA256"]
expected_msvc_cl_identity = os.environ["ZIPP_PGO_EXPECTED_MSVC_CL_IDENTITY"]
expected_msvc_lib = os.environ["ZIPP_PGO_EXPECTED_MSVC_LIB_SHA256"]
expected_msvc_lib_identity = os.environ["ZIPP_PGO_EXPECTED_MSVC_LIB_IDENTITY"]
expected_source_snapshot = os.environ["ZIPP_PGO_EXPECTED_SOURCE_SNAPSHOT_SHA256"]
digest = re.compile(r"[0-9a-f]{64}")
checks = {
    "rustflags source": identity.get("rustflags_source") == "CARGO_ENCODED_RUSTFLAGS",
    "canonical rustflags": identity.get("rustflags") == "-Cprofile-use=<redacted-path> -Ctarget-cpu=x86-64 -Clinker-flavor=lld-link",
    "target": identity.get("target") == "x86_64-pc-windows-msvc",
    "profile": identity.get("profile") == "release",
    "opt level": identity.get("opt_level") == "3",
    "features": identity.get("features") == "",
    "jit enabled": identity.get("jit") is True,
    "profile hash format": bool(digest.fullmatch(identity.get("pgo_profile_sha256", ""))),
    "recipe hash format": bool(digest.fullmatch(identity.get("pgo_training_recipe_sha256", ""))),
    "build recipe hash format": bool(digest.fullmatch(identity.get("pgo_build_recipe_sha256", ""))),
    "profile hash match": identity.get("pgo_profile_sha256") == expected_profile,
    "recipe hash match": identity.get("pgo_training_recipe_sha256") == expected_recipe,
    "build recipe hash match": identity.get("pgo_build_recipe_sha256") == expected_build_recipe,
    "build contract match": identity.get("pgo_build_contract") == expected_contract,
    "build environment policy match": identity.get("pgo_build_environment_policy") == expected_env_policy,
    "build environment hash match": identity.get("pgo_build_environment_sha256") == expected_env,
    "cargo identity match": identity.get("pgo_cargo_identity") == expected_cargo_identity,
    "cargo hash match": identity.get("pgo_cargo_sha256") == expected_cargo,
    "rustc hash match": identity.get("pgo_rustc_sha256") == expected_rustc,
    "linker hash match": identity.get("pgo_linker_sha256") == expected_linker,
    "linker identity match": identity.get("pgo_linker_identity") == expected_linker_identity,
    "MSVC cl hash match": identity.get("pgo_msvc_cl_sha256") == expected_msvc_cl,
    "MSVC cl identity match": identity.get("pgo_msvc_cl_identity") == expected_msvc_cl_identity,
    "MSVC lib hash match": identity.get("pgo_msvc_lib_sha256") == expected_msvc_lib,
    "MSVC lib identity match": identity.get("pgo_msvc_lib_identity") == expected_msvc_lib_identity,
    "source snapshot match": identity.get("pgo_source_snapshot_sha256") == expected_source_snapshot,
}
failed = [name for name, passed in checks.items() if not passed]
if failed:
    raise SystemExit("PGO provenance self-check failed: " + ", ".join(failed))
PY

echo "PGO binary ready: $BIN"
"$BIN" --version

verify_training_recipe_unchanged
verify_source_and_tools_unchanged
mkdir -p "$(dirname "$PUBLISHED_BIN")"
"$PYTHON_BIN" -I "$STAGED_TRAINING_RUNNER" check \
  --path "$(dirname "$PUBLISHED_BIN")" --kind directory
"$PYTHON_BIN" -I "$STAGED_TRAINING_RUNNER" publish \
  --source "$BIN" --destination "$PUBLISHED_BIN"
if [[ "$(sha256_file "$PUBLISHED_BIN")" != "$(sha256_file "$BIN")" ]]; then
  echo "error: atomically published binary failed digest verification" >&2
  exit 1
fi
echo "PGO binary published: $PUBLISHED_BIN"

PROFILE_TAG="${PROFILE_SHA256:0:12}"
echo
echo "Exact benchmark commands (run from this repository root):"
echo "python tools/bench.py --zipp $PUBLISHED_BIN --reps 15 --bootstrap-samples 10000 --json target/bench-results/pgo-real-$PROFILE_TAG.json"
echo "python tools/bench_hostile.py --zipp $PUBLISHED_BIN --reps 15 --bootstrap-samples 10000 --json target/bench-results/pgo-hostile-$PROFILE_TAG.json"
