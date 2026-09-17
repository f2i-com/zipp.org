# Running a transformers modelling file on ZIPP

`transformers/` here is **not** the Hugging Face package. It is the handful of
base classes, decorators and type shims that package's *modelling files* import
— 21 modules, 341 lines — so that one of those files can be read by ZIPP's
Python exactly as published, with nothing rewritten.

It works, and not only for one model. Each file below was copied byte for byte
from an installed transformers 4.57, built inside the engine, and compared with
what transformers itself produced for the same weights:

| Modelling file | max abs error vs transformers |
| --- | --- |
| `modeling_qwen3.py` | 7.5e-08 |
| `modeling_qwen2.py` | 6.0e-08 |
| `modeling_llama.py` | 1.8e-07 |
| `modeling_mistral.py` | 1.8e-07 |

The test hashes each file against the digest recorded when the reference was
made, so "unmodified" is checked rather than asserted, and it runs every model
staged under `models/transformers-*`, not one.

```sh
python tools/stage_transformers_model.py --model qwen3   # copies the two files + a reference
node --test tests/transformers-shim.test.mjs
```

## What this does and does not mean

The **package** does not run here, and cannot. Importing one modelling file
under CPython pulls in over three thousand modules — numpy, PyTorch,
tokenizers, safetensors, scipy, pyarrow, PIL — most of them C or Rust
extensions. This VM has no native extension loading and no installer, so none
of that is reachable.

But a modelling file does not use them. `modeling_qwen3.py` is 528 lines, imports
`torch` and fourteen names from transformers itself, and **never touches numpy**.
Those fourteen names are what this directory provides. The model's arithmetic
runs on ZIPP's own `torch` subset.

So: a numpy implementation is not what stands between ZIPP and a published
model. What stood there was four tensor operations (`sin`, `cos`, `rsqrt`,
`repeat_interleave`), `torch.autocast`, relative imports, and these 341 lines.

## Where it stops

Older modelling files do not load yet, and the test records that rather than
skipping it: `gpt_neo` is asserted to fail for the reason below, so if it ever
starts working, the list is what changes. `modeling_gpt_neo.py` reaches for
`torch.nn.attention.flex_attention` behind an availability check; CPython never
runs that branch, but ZIPP resolves every import while compiling, so a guarded
import of a module that does not exist fails before the guard can decide. That
is a real difference in import semantics, not a missing shim, and changing it
would mean deferring import errors to runtime for every program.

The four files above are the current generation and share one shape. A file
that wants a fused kernel, an ONNX export path or a tokenizer will want more
than this directory has.

## What is deliberately missing

The shim covers a forward pass and nothing else. It has no `from_pretrained`,
no hub access, no tokenizer, no `generate`, no device placement, no
quantisation, no sharding. Weights are handed in by the host; a sampling loop
belongs to the host too. Each shim module says in its own text what the real one
does that this one does not.

`transformers/masking_utils.py` and `cache_utils.py` are real implementations
rather than stubs, because a forward pass genuinely needs a causal mask and a
key/value cache. They are simple, and they are checked by the same comparison
against transformers.

## How this relates to the plugins

A Graph v2 plugin (`../plugins/`) emits nodes the host validates, binds and
submits; weights never enter Python, and the compute runs on whichever backend
the host chose, including the GPU. This path is the opposite trade: the guest
holds the tensors and the arithmetic runs on the engine's CPU kernels, but a new
architecture is a file you copy rather than a graph you write.

`torch.compile` records supported operations into a Graph v2 program and submits
them to the GPU. A rotary model is not recordable that way today: Graph v2 has
no `sin`, `cos` or `rsqrt`, so a compiled rotary block fails when the graph is
recorded, not when it runs. Either those three reach the protocol and its four
backends, or a model passes precomputed rotary tables in as data — which is what
the Graph v2 plugins already do, and why they already run on the GPU.

## Licence

The shim modules here are part of ZIPP and carry its licence. The modelling
files they are made to read belong to Hugging Face and are Apache-2.0; none of
them are checked in. `tools/stage_transformers_model.py` copies them out of a
transformers you installed yourself, into a gitignored directory.
