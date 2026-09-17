# Reading GGUF, including quantized checkpoints

`src/gguf.mjs` opens a GGUF file as a weight source beside Safetensors. Nothing
about the format is implemented here. The container, the block dequantizers and
the tokenizer are their own project --
[**gguf-wasm**](https://github.com/f2i-com/gguf-wasm) -- and this consumes a
tagged release of it. The module understands F32, F16, BF16, Q4_0/1, Q5_0/1,
Q8_0, Q2_K, Q3_K, **Q4_K**, Q5_K, **Q6_K**, IQ4_NL and IQ4_XS.

```sh
sh scripts/fetch_gguf_wasm.sh        # the pinned tag, checksum-verified
cat wasm/VERSION                     # which one is in the tree

ZIPP_GGUF_MODEL=<…/model.gguf> node --test tests/gguf.test.mjs
```

## Where the boundary runs

That project knows about the file format and nothing about how a model is
executed. This one knows how ZIPP executes a model and nothing about GGUF
beyond what it is handed. Concretely:

> **gguf-wasm says** -- here is `blk.12.attn_q.weight`, shape `[2048, 1024]`,
> format Q4_K, and here are its original packed bytes.
>
> **ZIPP says** -- keep those bytes resident; graph node 273 will consume them
> through a quantized transposed matmul.

The rule that a packed weight may appear only as the right-hand side of a
transposed matmul is a *ZIPP compute-protocol* rule, and belongs on this side.
So does the Qwen3 plugin, the decode plan, the carried KV cache, the prepared
session and the four backends.

ZIPP pins that project twice and the two move together: `../rust/Cargo.toml`
takes `gguf-quants` from the tag, because a matmul that reads a quantized
weight must decode it exactly as the reader does, and `scripts/fetch_gguf_wasm.sh`
takes the same tag's WebAssembly build. `gpu-lab`'s suite then checks its own
JavaScript decoder against that module and the compiled kernels against the
JavaScript one, so all three agree or it names which does not.

## Neither side holds the file

A quantized checkpoint is routinely gigabytes; the two used to develop this are
6.8 GB and 15.7 GB. So the header is parsed from the first few megabytes, and
every tensor after that is a byte range read through the same bounded `source`
interface a Safetensors model uses — a `File`, a `Blob`, a ranged request.

Measured on a 6.8 GB Q4_K_M Gemma 3 12B:

| | |
| --- | --- |
| Opened | 35 ms, from a 16 MiB header read |
| Tensors found | 626 — 289 F32, 288 Q4_K, 49 Q6_K |
| Embedding table | `token_embd.weight`, 788 MiB, Q6_K |
| Five rows of it | 3 ms |

That last row is the point of the arrangement. Every block format packs a whole
number of blocks per row, so a row is individually addressable: reading the
tokens a prompt actually uses costs kilobytes where the table costs a gigabyte.
`WeightStore.rows` uses that path when the index offers one, and a `rows`
binding in a graph gets it for free.

GGUF writes shapes fastest-varying first, the mirror of the convention used
everywhere here. They are reversed once, on the way in, so a plugin sees
`[rows, width]` and never learns which file format it came from.

## What quantization buys, and what it does not

It buys a smaller file, a smaller read, a vocabulary-sized table that can be
touched a row at a time, and -- since Graph v2 learned a quantized input -- a
smaller tensor on the device.

What it does not buy is any of that automatically, and which path a tensor
takes is a decision somebody has to make. `residentDtype(name)` answers whether
a backend can hold this one as blocks; `readBlocks(name)` hands over the file's
own bytes if so, and `readTensor(name)` decodes to float32 if not. The two are
budgeted separately -- blocks against the model budget, because blocks cost what
the file costs, and decoded tensors against the decoded-weight budget -- so a
model that keeps most of itself quantized is charged for what it actually holds.

The qwen3 plugin makes that decision the ordinary one: `matrix` binds a weight
as blocks where the format allows it and expands it where it does not, which is
what takes the 0.6B from 2,274 MB resident to 373 MB.

## Blocks on the device

Graph v2 takes `dtype: 'q4_k'` on an `input` node, whose `data` is the blocks
themselves: 144 bytes per 256 values. A backend decodes as it multiplies, so
what the device holds is the file's own bytes and what it computes is unchanged
-- the tests hold a quantized matmul to bit-for-bit equality with the same
matmul over decoded values, and hold the decoder to equality with
`ggml-quants` rather than with itself.

Because blocks run along a row, a quantized weight can only be read `[N, K]`,
which is the layout every checkpoint stores it in anyway. `matmul` therefore
takes `transposed: true`, and a quantized `b` without it is refused rather than
quietly transposed.

Measured on `blk.0.attn_q.weight` of a 6.8 GB Q4_K_M Gemma 3 12B:

| | |
| --- | --- |
| Shape | `[4096, 3840]`, Q4_K |
| Resident, quantized | 8.4 MiB |
| Resident, float32 | 60.0 MiB |
| Ratio | **7.11x** |
| Output | bit-for-bit identical |

7.11x is the number that decides whether a model fits. A 0.8B model whose
matrices are Q4_K is roughly 450 MB of weights instead of 3.2 GB, which is the
difference between a browser tab and a wall.

All four backends read blocks: the JavaScript reference, the compiled SIMD
kernels, WebGPU and WebGL2. Each decodes inside its own matmul rather than
before it, and each is checked two ways -- the quantized product against that
same backend's product over decoded values, and the whole thing against the
reference. The first of those is **exact on every backend**, because decoding
is integer arithmetic and two half-precision scales and there is nothing to
round differently; `gpu-lab/scripts/check-gpu-matmul.cjs` is what measures it in
a real browser.

| Backend | decoder | product vs cpu-js |
| --- | --- | --- |
| cpu-js | exact | reference |
| wasm (SIMD) | exact | **exact** |
| WebGPU | exact | **exact** |
| WebGL2 | exact | 6.1e-5 |

WebGL2's difference is a float32 GPU summing in its own order, not a decoder:
its transposed f32 matmul differs from cpu-js by exactly the same amount.

Both Q4_K and Q6_K, which between them are what a `Q4_K_M` checkpoint is made
of. Q6_K blocks are 210 bytes, so they are not word-aligned and the shaders
address the buffer by byte rather than by word.

## A model, end to end

`plugins/qwen3/` is the first architecture that runs this way. Measured on a
Qwen3-0.6B Q4_K_M, through the real engine running the plugin's Python:

| | |
| --- | --- |
| Opened | 37 ms, from a 16 MiB header read |
| Bound | 0.4 s, 310 tensors |
| Resident | **373 MB** -- 372 MB of blocks, 1 MB of float32 norms |
| The same weights as float32 | 2,274 MB |
| Forward pass | 0.9 s (5 tokens, compiled SIMD kernels) |
| "The capital of France is" | predicts " Paris" |

Nothing was decoded: every matrix in that family is Q4_K or Q6_K, so 99.7% of
what the device holds is the file's own bytes. That last row is the point --
a model that loads and produces finite logits can still be wrong in ways that
look like plausible nonsense, so the test asks a question with one answer.

Two bindings make this work, and a plugin picks between them by intent rather
than by dtype:

* `matrix` -- a weight matrix in the checkpoint's own `[out, in]` layout. The
  host keeps it as blocks where a backend can decode them and expands it where
  one cannot; the graph multiplies it `transposed` either way, so the plugin
  does not change when the answer does.
* `blocks` -- the same, but it refuses rather than expanding, for a caller that
  means it.

A GGUF file describes itself, so there is no `config.json` to read: the plugin's
`config_from_gguf` maps the metadata under its own architecture prefix and
refuses a file that is not its family.

## The tokenizer comes out of the file too

A GGUF checkpoint carries its own vocabulary, merge table and -- importantly --
the name of the pre-tokenizer it was trained with. `index.tokenizer()` builds
one inside the module and keeps it there: 151,936 token strings and 151,387
merges are expensive to move across the boundary and pointless to move, since
the encoder is the only thing that reads them. Building it takes about 180 ms,
against the minutes the same tables took in guest Python.

The pattern matters more than it looks. Qwen2 splits digits **one at a time**
where Llama-3 takes them three at a time and GPT-2 takes a run with its leading
space. Using the wrong one produces ids that are individually valid and
collectively wrong, so `tokenizer.ggml.pre` is read and a name this build does
not recognise is refused rather than guessed.

    "The capital of France is"  ->  785, 6722, 315, 9625, 374
    "12345"                     ->  220, 16, 17, 18, 19, 20   (qwen2: one per digit)

## Cached decoding

`build_graph` recomputes the whole prompt for every token and is the oracle.
`build_decode_graph` is the shape that makes a checkpoint usable: the weights
are uploaded once as blocks, the keys and values stay on the device as carried
inputs, and a token costs one position rather than the whole context.

Rotary models need one thing the eager path does not. A prefill knows every
position when it is built and carries its cosines and sines as constants; a
decode graph is built once and run at every position, so the host feeds them per
step through a `rope` slot. They are trigonometry over a position and an index,
never over the data, which is why they belong on the host side rather than in a
kernel.

Measured on the 0.6B, cache included, compiled SIMD kernels:

| | |
| --- | --- |
| Prepared | 0.4 s, 56 caches (two per layer) |
| Resident | 401 MB at context 128 -- 372 MB of blocks, 28 MB of cache |
| Throughput | ~3.9 tokens a second |
| "The capital of France is" | " Paris. The capital of France is also the capital of the Republic" |

## A reader is not a model

This gives tensors, metadata and a vocabulary. Running a checkpoint also needs a
plugin for its architecture — `gemma3`, `qwen3`, whatever `general.architecture`
says — because the tensor names, the attention shape and the tokenizer are that
family's, not the format's. `../plugins/gpt-neo/` is what one of those looks
like, and `interop/` is the other way of writing one.
