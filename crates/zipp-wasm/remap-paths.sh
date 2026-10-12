#!/usr/bin/env bash
# Print a CARGO_ENCODED_RUSTFLAGS value: the rustc flags given as arguments,
# followed by the --remap-path-prefix flags that keep build-machine paths out
# of a WebAssembly module.
#
#   CARGO_ENCODED_RUSTFLAGS="$(./remap-paths.sh [+toolchain] [rustflag...])" \
#     cargo build --locked --release --target wasm32-unknown-unknown
#
# rustc records, for every source file whose code can panic, the path it was
# compiled from. Without remapping those are absolute paths on the machine
# that ran the build: the checkout, the cargo registry under the builder's
# home directory, and (where rust-src is installed) the toolchain's standard
# library sources. A JavaScript module carried 157 of them, 11 KB, and the
# same commit built in a longer directory came out 2 KB larger. Remapped, a
# panic still names its file and line -- `heap.rs`, `vm/dispatch.rs`,
# `regress-fork/parse.rs`, `num-bigint-0.5.1/src/biguint.rs`,
# `library/core/src/slice/index.rs` -- and the module no longer depends on
# where, or as whom, it was built.
#
# The value is separated with 0x1F, cargo's CARGO_ENCODED_RUSTFLAGS encoding,
# so a path containing spaces stays one argument. cargo prefers that variable
# to RUSTFLAGS, so pass the flags RUSTFLAGS would have carried as arguments.
set -euo pipefail
cd "$(dirname "$0")"

toolchain=()
if [[ ${1-} == +* ]]; then
  toolchain=("$1")
  shift
fi

# The form rustc itself sees: a Windows path under Git Bash, unchanged elsewhere.
native() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}
sep=/
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) sep='\' ;;
esac

flags=("$@")

# The workspace crates: zipp-vm, zipp-wasm, regress-fork, zipp-pyparse,
# zipp-numparse. rustc applies the last prefix that matches, so the
# per-crate ones below win over this one for the files they cover.
repo="$(native "$(cd ../.. && pwd)")"
flags+=("--remap-path-prefix=${repo}${sep}crates${sep}=")
# Each crate's `src` directory loses the `src`, and the engine's loses its
# crate name too: its files are most of the panicking ones (about 170 of a
# module's 190), and each one's path is kept in the module once for its
# panic records. `vm/dispatch.rs` names the engine's file as well as
# `zipp-vm/src/vm/dispatch.rs` did, in 2 KB less of every module.
for crate in ../*/; do
  crate="$(basename "$crate")"
  [ -d "../$crate/src" ] || continue
  if [ "$crate" = zipp-vm ]; then
    flags+=("--remap-path-prefix=${repo}${sep}crates${sep}${crate}${sep}src${sep}=")
  else
    flags+=("--remap-path-prefix=${repo}${sep}crates${sep}${crate}${sep}src${sep}=${crate}${sep}")
  fi
done

# Registry sources. The index directory's name is a hash cargo derives from
# the registry, so it is read from disk; fetch first, or a cold cache would
# leave these paths unmapped and the module larger than a warm one's.
cargo ${toolchain[@]+"${toolchain[@]}"} fetch --locked >/dev/null 2>&1 || true
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
for index in "$cargo_home"/registry/src/*/; do
  [ -d "$index" ] || continue
  flags+=("--remap-path-prefix=$(native "${index%/}")${sep}=")
done

# The standard library: the local rust-src copy where that component is
# installed (rustc substitutes it for the virtual name), the virtual
# `/rustc/<commit>/` name everywhere else.
sysroot="$(rustc ${toolchain[@]+"${toolchain[@]}"} --print sysroot | tr -d '\r')"
commit="$(rustc ${toolchain[@]+"${toolchain[@]}"} -vV | tr -d '\r' | sed -n 's/^commit-hash: //p')"
flags+=("--remap-path-prefix=${sysroot}${sep}lib/rustlib/src/rust${sep}=")
if [ -n "$commit" ]; then
  flags+=("--remap-path-prefix=/rustc/${commit}/=")
fi

encoded=""
for flag in "${flags[@]}"; do
  encoded+="${encoded:+$'\037'}${flag}"
done
printf '%s' "$encoded"
