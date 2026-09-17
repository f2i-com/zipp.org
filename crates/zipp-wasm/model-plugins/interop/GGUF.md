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

It buys a smaller file, a smaller read, and a vocabulary-sized table that can be
touched a row at a time. It does **not** buy a smaller tensor on the device.

The compute protocol is float32. Anything bound into a graph is dequantized on
the way in, so a Q4_K_M checkpoint that is 0.5 GB on disk is still four bytes
per parameter once it reaches a backend. For a 0.8B model that is about 3.2 GB
of float32 — the same wall described in `../docs/HANDOFF.md`, unmoved. The
decoded-weight budget is charged as tensors are actually decoded rather than at
open time, precisely because opening a file you will only read rows of should
not cost what decoding all of it would.

So this makes a quantized checkpoint **readable**, not runnable at a size that
was previously out of reach.

## What running one quantized would need

Quantized tensors in Graph v2: a dtype on an input node, blocks kept as bytes on
the device, and dequantization inside each backend's matmul rather than before
it. That is four backends — the JavaScript reference, the C kernels, WebGL2 and
WebGPU — and this repository holds them to bit-for-bit agreement, which is the
real cost. On WebGL2 it means blocks in a texture and dequantization in the
fragment shader; the `llm` repository's CUDA backend already does the equivalent
with per-quant cooperative-warp GEMV kernels, so the shape of the work is known
rather than speculative.

Until then, the honest description is: ZIPP can read any GGUF and run what fits
in float32.

## A reader is not a model

This gives tensors, metadata and a vocabulary. Running a checkpoint also needs a
plugin for its architecture — `gemma3`, `qwen3`, whatever `general.architecture`
says — because the tensor names, the attention shape and the tokenizer are that
family's, not the format's. `../plugins/gpt-neo/` is what one of those looks
like, and `interop/` is the other way of writing one.
