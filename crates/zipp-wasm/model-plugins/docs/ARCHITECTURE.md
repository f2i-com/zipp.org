# Architecture, formats and trust boundaries

## 1. Separate support from model data

`plugin.json` describes executable support; `model.json` describes a particular
checkpoint. Installing a plugin never means downloading weights. A model's
`architecture` pins an already approved plugin identity. A mismatch is an error,
not an instruction to resolve a remote repository or run remote code.

A plugin also declares **which checkpoint family it implements** and which
tokenizers it can read. A model names the same two things. The host compares them
for exact equality before it constructs an engine, so an incompatible checkpoint
is refused rather than partly loaded. This is deliberately not inference from the
operations a graph uses: two plugins can emit identical attention and still
disagree about tensor names, weight orientation, tied embeddings, attention
scaling, epsilon placement or tokenization, which is precisely what a checkpoint
depends on.

The installed snapshot contains only Python source, namespaced under
`zipp_plugin/`, plus a fixed generated bootstrap. It is supplied to
`Engine.initPythonProject(files, entry, [])`. Architecture and tokenizer hooks run
through `pythonCall`; the host renews a finite instruction budget per hook.

This uses existing project compilation rather than a second interpreter or a
compiler hot-patch. All model/plugin/session state is isolated by a fresh engine;
the outer Worker is the hard cancellation boundary. The plugin shares its model
session's authority, not other applications' virtual files or models.

## 2. Plugin manifest version 1

The checked-in `plugins/*/plugin.json` files are complete, valid examples:

```json
{
  "format": "zipp.python-model-plugin",
  "version": 1,
  "id": "org.example.my-architecture",
  "plugin_version": "0.2.0",
  "entry": "architecture",
  "capabilities": ["graph-v2"],
  "checkpoint_format": "org.example.my-architecture-v1",
  "tokenizer_formats": ["character-v1"],
  "sources": {"architecture.py": "<64 lowercase SHA-256 hex characters>"}
}
```

`checkpoint_format` and each `tokenizer_formats` entry are lowercase identifiers
(`[a-z]` then `[a-z0-9]`, `.` and `-`), compared only for equality: the host never
parses a family name for meaning. They are pinned by the manifest hash like the
source is, which is what lets a host refuse a model before compiling any Python.
The installed source must agree — `describe` reports the same two values, and a
session that finds them different fails with `PLUGIN` rather than continuing.

The placeholder above is documentation, not a valid digest. `tools/build_manifests.py`
generates the included examples. An unchanged id/version cannot be overwritten
with different source content in the same registry. Side-by-side versions are
allowed. Removing a registry entry does not mutate an already running session.

A multi-file plugin imports its own modules by absolute name under `zipp_plugin`:
`from zipp_plugin.graph import Graph`, never `from .graph import Graph`. ZIPP's
Python frontend rejects relative imports outright (`relative imports are not
supported`), and the fixed namespace also stops a plugin's own `json.py` or
`math.py` shadowing what the bootstrap imports. `tools/cpython_host.py` mirrors
the same layout so the CPython tests and fixture tools import what ZIPP compiles.

The entry module implements:

```python
def describe(config):
    # Validate the supported configuration; currently only causal-lm task drivers.
    # The two format fields are the plugin's compatibility claim, and must match
    # plugin.json: one plugin implements one checkpoint family.
    return {"task": "causal-lm", "checkpoint_format": "org.example.my-architecture-v1",
            "tokenizer_formats": ["character-v1"], "vocab_size": 30, "max_context": 48}

def encode(text, tokenizer_config):
    # Return nonempty, bounded integer IDs, including any required BOS.
    return token_ids

def decode(token_ids, tokenizer_config):
    # Return text for the complete generated prefix, not just one byte piece.
    return text

def build_graph(config, token_ids):
    # Return Graph v2 nodes and external tensor bindings, never bulk weights.
    return {"version": 1, "graph": graph, "bindings": bindings}
```

These signatures are an ABI sketch. `architecture.py` in either example is a
complete implementation. Hooks must fail visibly on unsupported configurations;
recognizing an architecture label is not enough to prove numerical compatibility.
This version embeds tokenizer configuration in `model.json`. Large tokenizer
assets need a future explicit, bounded local-asset interface or host policy
adjustment; arbitrary tokenizer downloads are not supported.

## 3. Model manifest version 1

`examples/tiny-char/model.json` and `examples/bigram/model.json` are complete
examples. Required fields are `format: zipp.local-model`, `version: 1`,
`architecture: {id, version, sha256}`, `checkpoint_format`, `config`,
`tokenizer` (including its `type`), and `weights`.

`checkpoint_format` must equal the plugin's, and `tokenizer.type` must be one the
plugin lists; otherwise `ModelSession.open` throws `CHECKPOINT` or `TOKENIZER`
before an engine exists. Editing that field to satisfy the check does not convert
a checkpoint — it only replaces a clear refusal with a plugin reading the wrong
bytes. A checkpoint from another project needs a plugin written for it.

The `sha256` in `architecture` hashes the exact UTF-8 bytes of `plugin.json`, which
in turn pins every Python source file. Each weights entry has a canonical relative
`.safetensors` path and may include a content `sha256`. Both supplied examples pin
weights as well. Configuration can describe different sizes of the same compatible
architecture. It cannot create support for different tensor naming/layout,
attention semantics or tokenization on its own.

Safetensors support is deliberately a subset: little-endian F32, F16 and BF16,
rank zero through four, bounded shapes, exact byte ranges, and no unindexed gaps,
overlaps or trailing data. Duplicate JSON keys, unsupported dtypes and malformed
metadata fail. Scalar/empty tensor containers are accepted; the existing compute
graph's positive-dimension rules still apply when used as model inputs.
Safetensors itself permits non-finite numbers; this loader rejects them for ZIPP's
finite-F32 graph protocol. This is not a claim that such files violate Safetensors.

## 4. Binary bindings

A graph input has a normal id/op/shape but omits `data`. One binding resolves it:

```json
{"node": 0, "kind": "tensor", "tensor": "blocks.0.attn.q.weight"}
{"node": 1, "kind": "rows", "tensor": "token_embedding", "indices": [0, 14]}
{"node": 2, "kind": "causal", "length": 2}
```

`tensor` requires exact shape equality. `rows` gathers indexed matrix rows on the
host CPU for embeddings or a bigram table. `causal` supplies a finite additive
mask and optionally a local `window`. The finite sentinel assumes ordinary
attention-score magnitudes, as tested by the bundled fixture; it is not a proof
of masking for arbitrarily huge finite checkpoint values. Robust wider checkpoint
support should validate score bounds or add a proper masked-softmax primitive.
We use -1e9, not -Infinity: the current graph
validator accepts finite F32 input values only. Binding and shape validation
happen before large weight reads, then the ordinary runtime graph validator runs
again on the fully bound graph. No source strings/shaders are admitted.

General tensors have a rank-four ceiling, but the inspected graph validator's
`matmul` accepts rank two or rank three. The example therefore folds attention
heads into the rank-three batch dimension `[heads, tokens, head_dim]`; it does not
assume arbitrary rank-four matmul. LayerNorm is reduced to mean/sub/mul/add/sqrt/div,
and GELU uses the erf form matching the inspected backend, not the tanh variant.

The transformer example uses `[input_width, output_width]` linear weight layout.
Its Safetensors keys are its own explicit format, `zipp.tiny-causal-v1`. A PyTorch
or Hugging Face file with different layouts must be converted or handled by a
matching plugin; renaming its architecture or checkpoint field is not a conversion.

## 5. Limits and ownership

Defaults are intentionally conservative and separate from the existing runtime's
compute policies: 128 MiB per model shard, 256 MiB across shards, 64 MiB decoded
weights, 64 MiB bound graph inputs, 512 graph nodes, 512 context tokens, 128 newly
generated tokens, 1 MiB per Python file and 4 MiB total Python source. Runtime
Graph v2 limits are NOT globally raised by this package. Both policies must admit
a request. The demo uses the existing runtime defaults.

These caps are per model, not a global host quota. The host must also bound
concurrent model sessions/workers. These individual caps are **not one total
peak-memory budget**. Peak use also
includes selected source bytes, temporary checksum/read/decode buffers, cached
F32 tensors, gathered rows/masks, validated graph copies, uploads, activations,
readbacks and the engine. Whole-shard optional SHA verification is not streaming
in this prototype. Large-model deployment needs aggregate reservation/peak tests
before increasing any limits. Do not simply turn every limit up to fit a file.

A session owns its engine and weight cache; it borrows the runtime. `dispose()` is
idempotent after work settles and refuses mid-flight disposal. Concurrent operations
on a session fail with BUSY. AbortSignal is checked between operations; synchronous
Python/native/driver work requires terminating the owning Worker. The demo sets
an outer no-progress deadline. Hardware driver failure is not fully contained by
an application-level Promise timeout.

The registry's approval callback and SHA pins are not a sandbox or a signature
scheme. The embedding host must use the non-interop Python build, no extra host
capabilities, bounded files, existing graph validation and a deadline-limited
Worker. A hash from an untrusted origin authenticates nothing by itself. Catalogue
publication/signing, trust UI and revocation are future host responsibilities.

## 6. Website and offline behavior

`downloadPluginSource` is an optional host API, restricted to explicitly supplied,
same-origin URLs with bounded streaming and pinned hashes. Redirects and credentials
are refused. It is never called from Python or because of a model metadata field.
Catalogue installation does not silently install dependencies or a model.

The demo's bundled-fixture button explicitly approves downloading both example
support and its tiny checkpoint. The website-plugin/local-model choice downloads only architecture source; the
fully local-folder choice downloads neither plugin source nor model weights.
JavaScript/WASM/runtime files are still required to load the lab. Package/cache
those along with models for a genuinely offline application; no durable installer
or service worker is added here. Serve `.py` support as inert source assets, not
server-executed CGI.
