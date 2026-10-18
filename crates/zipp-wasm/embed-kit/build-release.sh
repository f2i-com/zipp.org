#!/usr/bin/env bash
# embed-kit/build-release.sh <out-dir>
#
# Every WebAssembly release asset of this checkout, built and checked on
# this machine the way .github/workflows/release-build.yml builds them in CI:
# each engine variant with the release's features, profile, stack and
# Binaryen flags; its Node boundary and contract tests; package-variant.sh;
# then the torch package, its pairing test, the embed kit and test-kit.mjs.
# Leaves in <out-dir> the files a release publishes (five .wasm and the kit's
# .zip) plus the unpacked kit and each variant's glue and Node package.
#
# Requires what the CI jobs install: Rust 1.92.0 with wasm32-unknown-unknown,
# the pinned wasm-bindgen CLI and Binaryen 125. ZIPP_SOURCE_SHA defaults to
# HEAD; the engines report it as their source.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
crate="$(dirname "$here")"
[ $# -eq 1 ] || { echo "usage: build-release.sh <out-dir>" >&2; exit 2; }
mkdir -p "$1"
out="$(cd "$1" && pwd)"
cd "$crate"
export ZIPP_SOURCE_SHA="${ZIPP_SOURCE_SHA:-$(git rev-parse HEAD)}"
version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' ../../Cargo.toml)"
target=wasm32-unknown-unknown

declare -A FEATURES=([javascript]="" [lite]="--features lite" [javascript-python]="--features python" [javascript-python-base]="--features python-base")
declare -A PROFILE=([javascript]=release [lite]=lite [javascript-python]=release [javascript-python-base]=release)
declare -A STACK=([javascript]=1048576 [lite]=1048576 [javascript-python]=16777216 [javascript-python-base]=16777216)
declare -A OPTIMIZE=([javascript]="" [lite]="--prefer-raw-size" [javascript-python]="" [javascript-python-base]="")

for v in javascript lite javascript-python javascript-python-base; do
  echo "=== $v"
  profile="${PROFILE[$v]}"
  dir="target/release-variants/$v"
  flags="-Dwarnings -C link-arg=--max-memory=1073741824 -C link-arg=-zstack-size=${STACK[$v]}"
  # shellcheck disable=SC2086
  CARGO_ENCODED_RUSTFLAGS="$(bash ./remap-paths.sh +1.92.0 $flags)" \
    cargo +1.92.0 build --locked --profile "$profile" --target "$target" --target-dir "$dir" ${FEATURES[$v]}
  built="$dir/$target/$profile/zipp_wasm.wasm"
  pkg="$out/node-$v"
  rm -rf "$pkg"
  wasm-bindgen --target nodejs --out-dir "$pkg" --remove-name-section --remove-producers-section "$built"
  node tests/node/strip-target-features.cjs "$pkg/zipp_wasm_bg.wasm" "$pkg/stripped.wasm"
  mv "$pkg/stripped.wasm" "$pkg/zipp_wasm_bg.wasm"
  # shellcheck disable=SC2086
  bash optimize-wasm.sh "$pkg/zipp_wasm_bg.wasm" ${OPTIMIZE[$v]}
  # The Node checks CI runs on this variant; they read tests/node/pkg.
  rm -rf tests/node/pkg
  cp -R "$pkg" tests/node/pkg
  case "$v" in
    javascript) node tests/node/run-boundary-suite.cjs ;;
    lite) node tests/node/run-boundary-suite.cjs && node tests/node/lite.cjs ;;
    javascript-python)
      node tests/node/run-boundary-suite.cjs
      node tests/node/python-conv2d.cjs
      node tests/node/python-training.cjs
      node tests/node/python-javascript.cjs
      ;;
    javascript-python-base) node tests/node/python-frontend.cjs && node tests/node/sort-semantics.cjs ;;
  esac
  # shellcheck disable=SC2086
  bash "$here/package-variant.sh" "$v" "$built" "$pkg/zipp_wasm_bg.wasm" "$out" ${OPTIMIZE[$v]}
done

echo "=== torch-package"
CARGO_ENCODED_RUSTFLAGS="$(bash ./remap-paths.sh +1.92.0 -C link-arg=--strip-all)" \
  cargo +1.92.0 build --locked --release --target "$target" --manifest-path torch/Cargo.toml --target-dir torch/target
torch="$out/torch"
rm -rf "$torch"
mkdir -p "$torch"
cp "torch/target/$target/release/zipp_torch.wasm" torch/zipp_torch.js "$torch/"
node tests/node/python-plugin.cjs "$out/node-javascript-python" "$out/node-javascript-python-base" "$torch"
node "$here/package.mjs" embed-torch "$version" "$ZIPP_SOURCE_SHA" "$torch/zipp_torch.wasm" "$out"

echo "=== embed kit"
ENGINE_DTS="$out/glue-javascript-python/zipp_wasm.d.ts" node "$here/package.mjs" kit "$version" "$out" "$out"
node "$here/test-kit.mjs" "$out/zipp-$version-wasm-embed-kit" "$out"
node "$here/test-sandbox.mjs" "$out/zipp-$version-wasm-embed-kit" "$out"
ls -l "$out"/*.wasm "$out"/*.zip
