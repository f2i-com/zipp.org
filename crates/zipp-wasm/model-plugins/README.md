# ZIPP local-model Python plugins — experimental source overlay

**Status: host/Python foundation, not a verified ZIPP release.** This is an additive
implementation designed against `f2i-com/zipp.org` commit
`62da28d9bdcd66fccbf5627d887d574016501f9d`. It adds a directory; it does not replace
Rust/VM files, modify the current landing-page build, or change SoftN.

40 standalone Node tests and 10 CPython/NumPy tests pass in the supplied evidence.
Two full-checkout integration gates were **not run**: the current upstream GPU
runtime and built Python-enabled ZIPP WASM package were not available locally.
The demo UI was checked in an offline HTML harness, not through model inference.
Do not merge or advertise working ZIPP/GPU inference until those gates pass.

## What is included

An approved architecture is ordinary Python source, compiled as a project by
ZIPP. It emits a Graph v2 template containing **named weight references**. The
JavaScript host supplies binary tensor data from local Safetensors, then submits
the graph to ZIPP's existing compute runtime. Model weights never enter Python's
project filesystem, JSON, or Python lists.

```text
plugin.json + architecture.py [+ helper modules]
                    |
              ZIPP Python Engine
                    |
          graph + named asset bindings
                    |
local Files / bundled asset Map --> host Safetensors reader
                    |
          validated Graph v2 + Float32Array inputs
                    |
       existing ZIPP compute runtime and backend
                    |
             logits / generated tokens
```

The registry, manifests, binary reader, bindings, generation/session driver and
browser Worker demo are implemented. They are not alternative tensor kernels.
No package name, model config field, or Python string becomes browser JavaScript.

`plugins/bigram/architecture.py` is a genuinely **single-file** architecture and
tokenizer plugin. `plugins/tiny-causal/` demonstrates a multi-file package with a
configurable pre-norm causal transformer. Both use exactly the same host APIs.
`plugin.json` is required even for one file: it carries identity, version, entry
module, required capability and source hashes.

## Activate without rebuilding the engine

A host installs verified Python source at runtime. Each model session then starts
a fresh ZIPP Engine with that source. A new architecture does not require changing
Rust or rebuilding WASM **when its operations are already supported**.

This is **session-time source installation**, not adding imports to an already
compiled, running Python program. An existing session retains its source snapshot;
upgrades take effect when a new session is opened. The registry is currently
in-memory, not an IndexedDB package manager. Pure-Python helper modules can travel
with the architecture, but arbitrary pip packages, native extensions, new VM
opcodes and new GPU kernels cannot be installed this way.

## Try it in a full ZIPP checkout

Apply the overlay/patch described in the bundle's `START_HERE.md`, then build the
regular Python WASM variant using the repository's documented toolchain:

```sh
bash crates/zipp-wasm/build-variants.sh all
cd crates/zipp-wasm/model-plugins
node --test
ZIPP_REQUIRE_INTEGRATION=1 node --test tests/integration.test.mjs
```

On PowerShell, set `$env:ZIPP_REQUIRE_INTEGRATION='1'` before the Node integration
command. The required mode makes missing artifacts a failure rather than a skip.
The Python differential tests additionally need NumPy and Safetensors:

```sh
python -m unittest discover -s tests -p 'test_*.py' -v
```

From the ZIPP repository root, serve the WASM tree (HTTPS in deployment; localhost
is suitable for development):

```sh
python -m http.server 8000 --bind 127.0.0.1 --directory crates/zipp-wasm
# Open http://127.0.0.1:8000/model-plugins/demo/
```

Choose the tiny fixture, download only a website plugin with your own local model,
or select both a plugin folder and a matching model folder.
The explicit approval button is the only route that starts a Worker or downloads
the bundled example. Local files are not uploaded. The demo reports the actual
backend; explicit GPU selection must fail rather than silently substitute another
backend. WASM compute kernels may require their separate existing GPU-lab build.
Use `dist/all`, **never the trusted-code `interop` variant**, for untrusted plugins.

The complete static tree must be included by the website build before deployment.
The current landing page/navigation/deployment pipeline is intentionally untouched.
No website changes have been published, and no service worker/offline installer is
included. Locally loaded weights do not, by themselves, make the website available
after an offline reload.

## Host API

Run this inside a host-owned, deadline-limited Worker. `Engine` and
`createRuntime` come from the existing ZIPP build, not this package.

```javascript
import {PluginRegistry, ModelSession, sourceFromFiles} from './src/index.mjs';

const registry = new PluginRegistry();
const plugin = await registry.install(sourceFromFiles(selectedPluginFiles), {
  approve: identity => hostApprovalFor(identity),
});
const runtime = await createRuntime({backend: 'auto'});
const model = await ModelSession.open({
  source: sourceFromFiles(selectedModelFiles),
  plugin,
  engineFactory: () => new Engine(),
  runtime,
});
try {
  const result = await model.generate('hello ', {
    maxNewTokens: 32,
    temperature: 0,
    onToken: ({text, backend}) => postMessage({text, backend}),
  });
} finally {
  model.dispose(); // after outstanding work settles
  runtime.dispose();
}
```

The complete runnable host is `demo/worker.mjs`. When passing browser-selected
files across a Worker, send a Map of explicit relative paths to File objects as
`demo/app.mjs` does; do not rely on `webkitRelativePath` surviving structured clone.

## Model support and present limits

| Implemented in this overlay | Not implemented or not verified |
| --- | --- |
| Versioned, source-hashed Python architecture plugins | Live mutation of an existing compiled VM; a general pip/plugin marketplace |
| Local Blob/File/typed-array sources and a scoped bundle-entry adapter | SoftN extraction, permissions/UI integration, production package migration |
| Safetensors F32/F16/BF16 with bounds checks and F32 conversion | Pickle checkpoints, GGUF, integer quantization, zero-copy device import |
| Custom tiny transformer and single-file bigram plugins | TinyStories/GPT-Neo, GPT-2 BPE, SentencePiece, arbitrary HF checkpoints |
| Eager full-context graph planning, greedy and temperature/top-k sampling | KV cache, resident model weights, fused decode kernels, throughput claims |
| Existing Graph v2 runtime integration code | ZIPP WASM/CLI, GPU inference and cross-backend parity verified here |

There is no new native `zipp py` model command or built-in `zipp_llm` Python
module in this overlay. The browser/embedding host owns model sessions.

The generic session driver currently serves causal language models only. A plugin
can replace architecture and tokenizer logic but must return that driver ABI.
Adding general library-only plugins or image/audio task drivers is a separate
extension, not something this patch silently claims to provide.

F16/BF16 checkpoints are expanded to Float32Array; this is not half-precision GPU
execution. CPU-host caches avoid re-reading files, but each inference call binds
and submits a full graph. The existing runtime copies/uploads inputs. There is no
claim of zero-copy weights or persistent GPU residence.

See [architecture and ABI](docs/ARCHITECTURE.md), [SoftN integration handoff](docs/SOFTN.md),
[release gates](docs/HANDOFF.md), and [validation](docs/VALIDATION.md).
