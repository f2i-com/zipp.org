# Reading GGUF, including quantized checkpoints

`src/gguf.mjs` opens a GGUF file as a weight source beside Safetensors. The
reader and the block dequantizers are not reimplemented here: they are the
`gguf` and `ggml-quants` crates from the `llm` repository, compiled to
WebAssembly by its `gguf-wasm` crate. Both were already pure computation and
needed no change to target `wasm32`; the module is 132 KB and understands
F32, F16, BF16, Q4_0/1, Q5_0/1, Q8_0, Q2_K, Q3_K, **Q4_K**, Q5_K, Q6_K,
IQ4_NL and IQ4_XS.

```sh
# in the llm repository
cargo build --release --target wasm32-unknown-unknown -p gguf-wasm
wasm-bindgen --target web --out-dir crates/gguf-wasm/pkg \
  target/wasm32-unknown-unknown/release/gguf_wasm.wasm

# here
ZIPP_GGUF_WASM=<…/gguf-wasm/pkg-node/gguf_wasm.js> \
ZIPP_GGUF_MODEL=<…/model.gguf> node --test tests/gguf.test.mjs
```

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

What it does not buy is any of that automatically. This reader still decodes
every tensor to float32 on the way in, because `dequantize` is what it calls.
Keeping blocks is a second path, described below, and the two ends exist without
being joined yet.

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

Two things are not done. Only the JavaScript reference backend reads blocks; the
C kernels, WebGPU and WebGL2 still want float32, and porting the decode into
each is three ports of one settled design rather than three designs. And this
reader does not yet bind a tensor as blocks -- `readTensor` dequantizes, so
reaching the 7.11x through a plugin needs a binding that passes blocks straight
through. Both are mechanical now that the protocol and the reference exist.

## A reader is not a model

This gives tensors, metadata and a vocabulary. Running a checkpoint also needs a
plugin for its architecture — `gemma3`, `qwen3`, whatever `general.architecture`
says — because the tensor names, the attention shape and the tokenizer are that
family's, not the format's. `../plugins/gpt-neo/` is what one of those looks
like, and `interop/` is the other way of writing one.
