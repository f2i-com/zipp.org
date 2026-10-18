#!/usr/bin/env bash
# embed-kit/package-variant.sh <variant> <cargo.wasm> <tested.wasm> <out-dir> [optimize-wasm.sh flags]
#
# One engine variant's release module, the same way locally (build-release.sh)
# and in CI (.github/workflows/release-build.yml):
#   1. wasm-bindgen's web bindings for the cargo output, with the
#      target-features strip and the Binaryen pass the boundary tests' copy
#      had, and a byte compare with that copy (<tested.wasm>): the shipped
#      code is exactly the code those tests ran;
#   2. the glue, memory-surface and type-surface checks;
#   3. the bindings and the zipp.meta record embedded (package.mjs embed),
#      proved by stripping them off again and comparing;
#   4. the module loaded and exercised through zipp.mjs (test-kit.mjs).
# Leaves <out-dir>/zipp-<version>-<variant>.wasm, the release asset (its path
# is the last line printed), and <out-dir>/glue-<variant>/ with the generated
# zipp_wasm.js, the .d.ts files and PROFILE.json.
set -euo pipefail
variant=$1
built=$2
tested=$3
out=$4
shift 4
here="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
built="$(cd "$(dirname "$built")" && pwd)/$(basename "$built")"
tested="$(cd "$(dirname "$tested")" && pwd)/$(basename "$tested")"
glue="$out/glue-$variant"
rm -rf "$glue"
cd "$here/.."

wasm-bindgen --target web --out-dir "$glue" \
  --remove-name-section --remove-producers-section "$built"
node tests/node/strip-target-features.cjs "$glue/zipp_wasm_bg.wasm" "$glue/stripped.wasm"
mv "$glue/stripped.wasm" "$glue/zipp_wasm_bg.wasm"
bash optimize-wasm.sh "$glue/zipp_wasm_bg.wasm" "$@"
# wasm-bindgen's Node and web glue use the same module.
cmp "$tested" "$glue/zipp_wasm_bg.wasm"

node --input-type=module --check < "$glue/zipp_wasm.js"
node tests/node/check-wasm-memory.cjs "$glue/zipp_wasm_bg.wasm"
# A mutually stale glue and module can run while silently missing an API
# the tagged source added; pin the generated type surfaces too.
grep -Fq 'getGlobalsFingerprint(indices: any): any;' "$glue/zipp_wasm.d.ts"
grep -Fq 'readonly engine_getGlobalsFingerprint:' "$glue/zipp_wasm.d.ts"
grep -Fq 'export const engine_getGlobalsFingerprint:' "$glue/zipp_wasm_bg.wasm.d.ts"

asset="$(node "$here/package.mjs" embed "$variant" "$glue/zipp_wasm.js" "$glue/zipp_wasm_bg.wasm" "$out")"
node "$here/package.mjs" strip "$asset" "$glue/unembedded.wasm"
cmp "$glue/zipp_wasm_bg.wasm" "$glue/unembedded.wasm"
rm "$glue/unembedded.wasm"

node "$here/test-kit.mjs" "$here" "$out" "$variant"
echo "$asset"
