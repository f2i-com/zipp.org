# Source review provenance

Repository/API contracts inspected at ZIPP commit
`62da28d9bdcd66fccbf5627d887d574016501f9d`:

- `docs/PYTHON_FRONTEND_EXPERIMENT.md`: module compilation, VFS policy, Python/host hooks.
- `crates/zipp-wasm/playground/engine.worker.js`: Engine construction, project init,
  Python calls, budget renewal, Worker ownership and compute backend hookup.
- `crates/zipp-wasm/host-sdk/zipp-host.worker.mjs`: explicit capability grants.
- `crates/zipp-wasm/gpu-lab/src/runtime.mjs`: createRuntime, execute/typedOutputs,
  source validation, backend policies, lifecycle and prepared sessions.
- `crates/zipp-wasm/gpu-lab/src/graph.mjs`: finite-F32 inputs, strict Graph v2 fields,
  ranks, actual matmul shapes, node limits and output structure.
- `crates/zipp-wasm/gpu-lab/src/backends/cpu.mjs` and `src/kernel-math.mjs`:
  float32 arithmetic and erf-form GELU.
- `crates/zipp-wasm/build-variants.sh`: standard Python `all` vs trusted `interop` build.

The above source files were read, not copied over or modified by this overlay.
The original user's attached proposal is the basis for the host asset boundary,
Safetensors-first approach, eager reference path and later KV-cache rollout.
This deliverable narrows the first model milestone to a custom trained fixture;
it does not claim to have implemented that proposal's TinyStories/GPT-Neo runner.

SoftN code search at `7593d23fad86216f8f7216669f2f628024ea2c78` showed ZIP/manifest
and asset handling in `apps/softn-single-private/php/softn-serve.php` and
`scripts/softn-apps/fetch.mjs`. That informed the future package boundary only;
this was not a complete SoftN loader/security review.

External format reference: the official Safetensors repository's format description.

```text
https://github.com/f2i-com/zipp.org/tree/62da28d9bdcd66fccbf5627d887d574016501f9d
https://github.com/f2i-com/softn.com/tree/7593d23fad86216f8f7216669f2f628024ea2c78
https://github.com/huggingface/safetensors#format
```

New host/plugin code is provided under Apache-2.0. The hand-authored/generated toy
corpus and model fixture assets are marked CC0-1.0 in their manifests; no third-party
checkpoint or proprietary training corpus is included. Existing ZIPP and other
third-party code retains its original license.
