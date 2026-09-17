# gguf-wasm

Read GGUF models from Rust, Node or a browser — metadata, tensor ranges,
quantized blocks and the tokenizer the file was trained with.

**It never holds the file.** A quantized checkpoint is routinely several
gigabytes; the two used to develop this are 6.8 GB and 15.7 GB. So the header
is parsed from the first few megabytes, and every tensor after that is a byte
range the caller reads for itself — from a `File` a person chose, a `Blob`, an
HTTP server that honours Range, or a file handle. That constraint shapes
everything else here, and it is the reason this works in a page at all.

```
E:/models/qwen3-0.6b-q4_k_m.gguf
  0.40 GB on disk, GGUF v3
  310 tensors, 0.60B parameters
    F32       113 tensors       0.2 MiB
    Q4_K      168 tensors     204.8 MiB
    Q6_K       29 tensors     167.7 MiB
  largest: token_embd.weight [1024, 151936] Q6_K, 121.7 MiB

3 rows of 1024 in 0.1 ms -- 12.0 KiB decoded, not 121.7 MiB
```

That last line is the point of the arrangement. Every block format packs a
whole number of blocks into each row, so a row is individually addressable:
reading the tokens a prompt actually uses costs kilobytes where the table costs
a hundred megabytes.

## What it does not do

It does not run a model. It turns a file into metadata, byte ranges, values and
tokens; what executes those is somebody else's concern, and keeping that line
sharp is why this is a separate thing.

The boundary, concretely:

> **gguf-wasm says** — here is `blk.12.attn_q.weight`, shape `[2048, 1024]`,
> format Q4_K, and here are its original packed bytes.
>
> **A runtime says** — keep those bytes resident, my graph will read them
> without expanding them.

[ZIPP](https://github.com/f2i-com/zipp.org) is one such runtime: it keeps Q4_K
and Q6_K weights in the file's own block format on the device and decodes them
inside the matmul, which is the difference between a 0.6B model holding at
373 MB and needing 2,274 MB. None of that lives here.

## The crates

| | |
| --- | --- |
| `gguf` | the container: magic, version, metadata, tensor shapes, the byte range of every tensor. With `std`, opening a file and reading tensors and rows out of it. |
| `gguf-quants` | the block formats, decoded exactly: F32, F16, BF16, Q4_0/1, Q5_0/1, Q8_0, Q2_K, Q3_K, Q4_K, Q5_K, Q6_K, IQ4_NL, IQ4_XS. |
| `gguf-tokenizer` | byte-level BPE over the file's own vocabulary, with the pre-tokenizer that vocabulary was trained with. |
| `gguf-wasm` | the `wasm-bindgen` surface: the three above, in a browser. |

Everything except `gguf-wasm` builds without `std`, which is what lets the same
code serve a browser, a server and a freestanding module with no allocator.

## Rust

```toml
[dependencies]
gguf = { version = "0.1", features = ["std"] }
```

```rust
use gguf::GgufReader;

let mut model = GgufReader::open("model.gguf")?;
println!("{:?}", model.header().metadata().get("general.architecture"));

// The file's own bytes: for a quantized tensor, its blocks.
let packed = model.tensor_bytes("blk.0.attn_q.weight")?;

// Or decoded, if that is what you want.
let values = model.tensor_f32("blk.0.attn_q.weight")?;

// Or one row of a vocabulary-sized table, without the rest of it.
let embedding = model.rows_f32("token_embd.weight", &[12095])?;
```

Without the `std` feature there is no file and no filesystem: you hand
`GgufFile::from_bytes` a header you read yourself, which is what a browser and
an embedded target need.

## JavaScript

```js
import init, * as gguf from './pkg/gguf_wasm.js';
import {openGGUF, fromBlob} from './js/index.mjs';

await init();
const model = await openGGUF(fromBlob(file), {module: gguf});

model.architecture;                              // 'qwen3'
model.tensors.get('blk.0.attn_q.weight').shape;  // [2048, 1024]
await model.bytes('blk.0.attn_q.weight');        // packed blocks
await model.rows('token_embd.weight', [12095]);  // one row, kilobytes

const tokenizer = model.tokenizer();
tokenizer.encode('The capital of France is');    // [785, 6722, 315, 9625, 374]
tokenizer.decode(ids);
```

`js/index.mjs` is a convenience: reading the header by doubling until it
parses, turning a tensor name into a byte range, and fetching that range. The
module underneath is smaller and you can use it directly.

Sources it can read through: `fromBlob(file)`, `fromURL(url)` (HTTP Range — a
server that ignores it is caught rather than silently read whole), and
`fromFileHandle(handle, size)` for Node.

## The tokenizer is not an afterthought

A GGUF file carries its vocabulary, its merge table, and — importantly — the
name of the pre-tokenizer it was trained with. Qwen2 splits digits **one at a
time** where Llama-3 takes them three at a time and GPT-2 takes a run with its
leading space. Using the wrong one produces token ids that are individually
valid and collectively wrong: a model that reads as slightly stupid rather than
visibly broken.

So `tokenizer.ggml.pre` is read, and a name this build does not recognise is
refused rather than guessed.

```
"The capital of France is"  ->  785, 6722, 315, 9625, 374
"12345"                     ->  220, 16, 17, 18, 19, 20   (qwen2: one per digit)
```

It is built inside the WebAssembly module from the file's own metadata and
stays there. A vocabulary is 151,936 strings and a merge table 151,387; moving
those across the boundary is expensive and pointless, since the encoder is the
only thing that reads them. Building it takes about 180 ms.

## Decoding is exact

Block decoding is integer arithmetic and one or two half-precision scales.
There is nothing to round differently, so a decoder is either right or wrong —
and every intermediate here rounds to float32 where ggml's does, including the
sub-block scale and the product before the minimum comes off. Doing that
arithmetic in double and rounding once at the end agrees most of the time and
differs in the last bit the rest of it, which over a table with millions of
values is not "most of the time" at all.

## Examples

```sh
cargo run -p gguf-examples --bin inspect  -- model.gguf
cargo run -p gguf-examples --bin tokenize -- model.gguf "Hello world"
cargo run -p gguf-examples --bin rows     -- model.gguf token_embd.weight 0 1 2
```

There is no `matmul` example, deliberately: multiplying is the thing on the
other side of the boundary.

## Building the WebAssembly

```sh
cargo install wasm-bindgen-cli --version 0.2.126   # must match the crate's pin
sh scripts/build-wasm.sh                           # -> pkg/ and pkg-node/
```

Or take a built one from a [release](https://github.com/f2i-com/gguf-wasm/releases):
each tag attaches `gguf-wasm-web.tar.gz` and `gguf-wasm-node.tar.gz`, built
from that tag's `Cargo.lock`.

## Tests

```sh
cargo test                                            # the format itself
GGUF_MODEL=model.gguf cargo test -p gguf --features std   # against a real file
```

A checkpoint is not redistributed here, so the tests that need one skip without
`GGUF_MODEL`.

## Provenance and licence

The reader and the block formats began in a private Rust reimplementation of
llama.cpp and were rebuilt here to work without `std`. The block layouts must
stay byte-for-byte compatible with upstream `ggml-quants.c`, and the decoders
are written to be read against that source rather than trusted.

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at
your option.
