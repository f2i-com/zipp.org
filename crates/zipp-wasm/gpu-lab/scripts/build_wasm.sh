#!/usr/bin/env sh
# Build wasm/kernels.wasm from the `zipp-kernels` crate.
#
# Cargo links this itself, so there is no separate compile-and-link dance and
# no `wasm-opt` post-link step to be tripped up by an npm shim on PATH. The
# module is freestanding: no allocator, no libc, no imports. The host reads
# `memory` and `__heap_base` and runs its own arena over them.
#
# The 2 GiB ceiling is a ceiling, not a reservation: memory grows on demand.
# It is what a real checkpoint needs -- a 0.6B Qwen3 is 373 MB of blocks, and
# the arena is a bump allocator that does not reclaim within one execution.
#
# `+simd128` is what makes the matmul panels vectorise. Float determinism needs
# no flag here: Rust never contracts a multiply and an add into an FMA, so
# every operation rounds on its own, which is what keeps this bit-for-bit with
# the JavaScript reference. (The C this replaced needed -ffp-contract=off to
# promise the same thing.)
set -eu
cd "$(dirname "$0")/.."
CRATE="../rust"
OUT="wasm/kernels.wasm"

# The compiler is an input like the source is: another rustc lays the same code
# out differently, and the committed binary is what 1.92.0 makes of it -- the
# toolchain CI tests with, and the one its rebuild check runs. A caller who
# names another toolchain gets that one, and a binary that will not match.
: "${RUSTUP_TOOLCHAIN:=1.92.0}"
export RUSTUP_TOOLCHAIN

# Where this was built must not be part of what was built. `gguf-quants` comes
# from a git dependency, so rustc records a panic location inside the cargo
# checkout -- an absolute path naming whoever ran this. That is 40-odd bytes
# nobody else can reproduce, in a binary that is committed and meant to be
# checkable by rebuilding it. Remapped to a fixed stand-in instead.
#
# The prefix has to match the form rustc records, which on Windows is
# `C:\Users\...` and not the `/c/Users/...` this shell uses, hence cygpath.
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
posix_home="$cargo_home"
if command -v cygpath >/dev/null 2>&1; then
  cargo_home=$(cygpath -w "$cargo_home")
fi

# And the separators have to match too. What follows the prefix -- the
# checkout's own directories, and this workspace's relative paths -- keeps the
# separator of the machine that built it, so a Windows build recorded
# `crates\gguf-quants\src\q4_k.rs` where Linux records the same path with
# slashes, and the two binaries differed in exactly those bytes. On Windows
# every source file that can be recorded is remapped to its slash form; rustc
# lets the later of two matching prefixes win, so these take precedence over
# the prefix above. Elsewhere the paths are already in that form.
separators=""
if command -v cygpath >/dev/null 2>&1; then
  for f in $(cd "$CRATE" && find zipp-kernels -name '*.rs'); do
    separators="$separators --remap-path-prefix=$(printf '%s' "$f" | tr '/' '\\')=$f"
  done
  for f in $(find "$posix_home/git/checkouts" -path '*/crates/gguf-quants/src/*.rs' 2>/dev/null); do
    separators="$separators --remap-path-prefix=$(cygpath -w "$f")=/cargo/git/checkouts/${f#"$posix_home/git/checkouts/"}"
  done
fi

# Nor where the workspace sits. Symbol names carry a hash of each crate's
# identity, which for a path dependency includes its directory, so two
# checkouts named every internal function differently in the debug `name`
# section. The host reads only the exports -- `memory`, `__heap_base` and the
# kernels, all by fixed name -- so the names are stripped rather than chased.
RUSTFLAGS="-C target-feature=+simd128 \
 -C strip=symbols \
 --remap-path-prefix=$cargo_home=/cargo$separators \
 -C link-arg=--export-memory \
 -C link-arg=--export=__heap_base \
 -C link-arg=--initial-memory=2097152 \
 -C link-arg=--max-memory=2147483648" \
  cargo build --locked --release --target wasm32-unknown-unknown \
    --manifest-path "$CRATE/Cargo.toml" -p zipp-kernels

cp "$CRATE/target/wasm32-unknown-unknown/release/zipp_kernels.wasm" "$OUT"
echo "$OUT: $(wc -c < "$OUT") bytes"
