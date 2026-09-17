#!/usr/bin/env sh
# Fetch the GGUF WebAssembly module that `src/gguf.mjs` reads checkpoints with,
# and record exactly what landed.
#
#   sh scripts/fetch_gguf_wasm.sh            # the pinned release
#   GGUF_WASM_VERSION=v0.3.0 sh scripts/...  # a different one
#
# The module is not built here. Reading GGUF -- the container, the block
# formats, the tokenizer -- is generic and useful without ZIPP, so it lives in
# its own repository and this consumes a tagged release of it:
#
#   https://github.com/f2i-com/gguf-wasm
#
# What comes back is committed, the way gpu-lab/wasm/kernels.wasm is: a browser
# needs it and not everyone who clones this wants a Rust toolchain.
#
# ## Why there is a lock file
#
# A version number is a name, and a name can come to mean different bytes. So
# this writes `wasm/gguf-wasm.lock.json`: the tag, the source revision, the
# digest of every asset the release published, and the digest of every file
# that ended up in the tree. `tests/gguf-module.test.mjs` checks the tree
# against it, so a file that changed without this script running is a failing
# test rather than a mystery.
#
# The Rust side pins the same revision -- ../rust/Cargo.toml takes
# `gguf-quants` from it by `rev`, because a matmul that reads a quantized
# weight must decode it exactly as the reader does. Move both together.
set -eu
cd "$(dirname "$0")/.."

VERSION="${GGUF_WASM_VERSION:-v0.0.1}"
REPO=f2i-com/gguf-wasm

command -v gh >/dev/null 2>&1 || { echo "needs the gh CLI to download a release" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
echo "fetching $REPO $VERSION"
gh release download "$VERSION" --repo "$REPO" --dir "$work" \
  --pattern "gguf-wasm-*.tar.gz" --pattern "SHA256SUMS" --clobber

# The release's own digests, before anything is unpacked.
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

# The revision the tag names, so the Rust pin and this one can be compared.
commit=$(gh api "repos/$REPO/git/ref/tags/$VERSION" --jq '.object.sha' 2>/dev/null || echo unknown)
if [ "$(gh api "repos/$REPO/git/ref/tags/$VERSION" --jq '.object.type' 2>/dev/null)" = "tag" ]; then
  commit=$(gh api "repos/$REPO/git/tags/$commit" --jq '.object.sha')
fi

python - "$VERSION" "$REPO" "$commit" "$work" <<'PY' > wasm/gguf-wasm.lock.json
import hashlib, json, pathlib, sys

version, repo, commit, work = sys.argv[1:5]
digest = lambda p: hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()

assets = {p.name: digest(p) for p in sorted(pathlib.Path(work).glob('*.tar.gz'))}
files = {}
for directory in ('wasm/gguf', 'wasm/gguf-node'):
    for p in sorted(pathlib.Path(directory).rglob('*')):
        if p.is_file():
            files[p.as_posix().removeprefix('wasm/')] = digest(p)

print(json.dumps({
    "comment": "Written by scripts/fetch_gguf_wasm.sh. Checked by tests/gguf-module.test.mjs.",
    "repository": f"https://github.com/{repo}",
    "tag": version,
    "commit": commit,
    "assets": assets,
    "files": files,
}, indent=2))
PY

echo "wasm/gguf-wasm.lock.json: $VERSION at ${commit}"
