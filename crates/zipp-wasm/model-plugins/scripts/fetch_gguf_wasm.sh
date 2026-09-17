#!/usr/bin/env sh
# Fetch the GGUF WebAssembly module that `src/gguf.mjs` reads checkpoints with.
#
#   sh scripts/fetch_gguf_wasm.sh            # the pinned version
#   GGUF_WASM_VERSION=v0.2.0 sh scripts/...  # a different one
#
# The module is not built here any more. Reading GGUF -- the container, the
# block formats, the tokenizer -- is generic and useful without ZIPP, so it
# lives in its own repository and this consumes a tagged release of it:
#
#   https://github.com/f2i-com/gguf-wasm
#
# What comes back is committed, the way gpu-lab/wasm/kernels.wasm is: a browser
# needs it and not everyone who clones this wants a Rust toolchain. The digest
# is checked against the release's own SHA256SUMS, so what lands in the tree is
# what that tag built and not whatever a network happened to return.
#
# The Rust side of ZIPP pins the same tag: ../rust/Cargo.toml takes
# `gguf-quants` from it, because a matmul that reads a quantized weight must
# decode it exactly as the reader does. Move both together.
set -eu
cd "$(dirname "$0")/.."

VERSION="${GGUF_WASM_VERSION:-v0.1.0}"
REPO=f2i-com/gguf-wasm

command -v gh >/dev/null 2>&1 || { echo "needs the gh CLI to download a release" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
echo "fetching $REPO $VERSION"
gh release download "$VERSION" --repo "$REPO" --dir "$work" \
  --pattern "gguf-wasm-*.tar.gz" --pattern "SHA256SUMS" --clobber

( cd "$work" && sha256sum --check --ignore-missing SHA256SUMS )

for target in web node; do
  case "$target" in
    web) out=wasm/gguf ;;
    node) out=wasm/gguf-node ;;
  esac
  rm -rf "$out"
  mkdir -p "$out"
  tar -xzf "$work/gguf-wasm-$target-$VERSION.tar.gz" -C "$out"
  echo "$out: $(wc -c < "$out/gguf_wasm_bg.wasm") bytes"
done

# Record what is in the tree, so a reader of the repository can tell which
# release these files came from without going to look.
printf '%s\n' "$VERSION" > wasm/VERSION
echo "wasm/VERSION: $VERSION"
