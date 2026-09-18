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

| package | library | |
| --- | --- | --- |
| `f2i-gguf` | `gguf` | the container: magic, version, metadata, tensor shapes, the byte range of every tensor. With `std`, opening a file and reading tensors and rows out of it. |
| `f2i-gguf-quants` | `gguf_quants` | the block formats, decoded exactly: F32, F16, BF16, Q4_0/1, Q5_0/1, Q8_0, Q2_K, Q3_K, Q4_K, Q5_K, Q6_K, IQ4_NL, IQ4_XS. |
| `f2i-gguf-tokenizer` | `gguf_tokenizer` | byte-level BPE over the file's own vocabulary, with the pre-tokenizer that vocabulary was trained with. |
| `f2i-gguf-wasm` | `gguf_wasm` | the `wasm-bindgen` surface: the three above, in a browser. |

The published names carry an `f2i-` prefix because `gguf` on crates.io is an
unrelated crate. The *library* names do not, so a consumer aliases the package
once and `use gguf::...` reads the way you would expect:

```toml
gguf = { package = "f2i-gguf", version = "0.0.4", features = ["std"] }
```

Everything except the wasm surface builds without `std`, which is what lets the
same code serve a browser, a server and a `wasm32` target. That is `no_std`,
not allocation-free: `gguf` and `gguf-tokenizer` use `alloc` — `Vec`, `String`,
`BTreeMap` — because parsing metadata means building collections. Only
`gguf-quants` allocates nothing at all: every routine decodes into a slice its
caller already owns, which is what lets a freestanding module with no allocator
use it.

## Reading something you did not write

A parser takes numbers out of a file and then allocates according to them,
which is the whole attack surface of a format like this. So:

* lengths are `u64` in the format and `usize` on the machine, and on `wasm32`
  that is a narrowing — checked, never cast;
* a count is refused when the remaining bytes could not hold that many elements
  even at their minimum size, which bounds every allocation by the file's own
  length before a single byte is reserved;
* additions and products that could overflow are checked;
* duplicate metadata keys and duplicate tensor names are refused, because a
  file that makes a reader choose which one wins is malformed;
* `ParseLimits` bounds the rest, and a caller that knows its inputs can raise
  or lower it;
* a refusal says whether reading more could change it. `GgufError::
  needs_more_bytes` is true only for truncation, so the header read can double
  when a file is merely short and stop at once when it is not a GGUF file --
  without that distinction, opening the wrong file costs the whole 64 MiB
  schedule before it fails.

The JavaScript wrapper checks the same things on its own side, because a range
crosses that boundary as JSON. A GGUF length is a `u64` and a JSON number is a
double, so past 2^53 a value arrives *near* what the file said rather than equal
to it; and an offset past the end of a file is not an error in any source here,
since `Blob.slice` clamps and the read simply comes back short. Both are refused
when the model is opened, naming the tensor, rather than surfacing later as a
tensor that decoded into nonsense.

`rows()` checks its own arguments for a different reason: those come from the
caller, not the file. A row index crosses into the module as a `u32`, and
JavaScript hands `2**32 + 1`, `-1` or `1.5` to that conversion without
complaint -- each of which reads some other row and returns it as though it
were the one asked for. An index outside the tensor's own row count is refused.

### One `u32` ceiling, made explicit

The parser is `u64` throughout, and a tensor's offset and length stay that wide
because reading a range never materialises anything inside the module. Two
entry points are narrower: `dequantize` and `row_range` take a `u32`, since what
they produce lands in wasm32 memory, which is four gigabytes in total. A wider
parameter would move the failure rather than remove it.

What is worth refusing is the silent conversion, and `js/index.mjs` does:
`rows()` rejects an index past `0xffffffff` and `floats()` rejects a tensor with
more values than that, each saying so rather than truncating. `bytes()` has no
such ceiling and is the way past it -- the range is read outside the module and
never crosses that boundary, which is what a caller holding a tensor too large
to decode in one go should use.

None of this is about Rust memory safety, which is not in question. It is about
a malformed or hostile file producing an error rather than a panic, a silently
truncated length, or an allocation that takes the process down. `crates/gguf/
tests/hostile.rs` is where that is checked, including every prefix of a
plausible header and a couple of thousand rounds of arbitrary bytes.

## Rust

```toml
[dependencies]
gguf = { package = "f2i-gguf", version = "0.0.4", features = ["std"] }
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

Without the `std` feature there is no file and no filesystem. You hand
`GgufHeader::from_bytes` a header you read yourself, and it tells you where a
tensor is:

```rust
let header = gguf::GgufHeader::from_bytes(&first_megabyte)?;
let range = header.range_of("blk.0.attn_q.weight")?;   // offset and length
```

A header holds no file bytes and cannot hand you a tensor body — reading that
range is the caller's job, because only the caller knows whether the file is a
`File`, a `Blob`, an HTTP resource or a peer.

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

Three sources: `fromBlob(file)`, `fromFileHandle(handle, size)`, and
`fromURL(url)`.

### Reading one object, not several

`fromURL` reads a model over many requests, and the failure that matters is not
a request that fails — it is a *successful* one against a different object. A
header parsed from version A and a weight fetched from version B are both 206,
both the right length, and the model is quietly wrong. So identity is pinned
when the model is opened and carried afterwards:

* the open is a one-byte range request, not a HEAD. A server that honours Range
  answers with a `Content-Range` stating the total, which is proof rather than a
  promise — and some perfectly good servers and CDNs do not expose
  `Accept-Ranges` or `Content-Length` to a cross-origin HEAD at all;
* whatever validator comes back (`ETag`, else `Last-Modified`) is sent as
  `If-Range` on every read after that, so a changed object answers 200 with the
  whole entity and is refused;
* every response's `Content-Range` must be the range that was asked for, out of
  a total that has not changed, and its numbers must be exact JavaScript
  integers -- they are decimal digits from a header, and `Number` will take more
  of them than it can hold and return something merely close;
* a *weak* `ETag` is passed over for `Last-Modified`. `If-Range` requires a
  strong validator and a server must ignore a weak one, so sending `W/"..."`
  would look like a guarantee while being none.

That does not make an HTTP source trustworthy. It makes it *consistent*: given
a validator, what is read is all from one object or it is an error.

A server that offers neither `ETag` nor `Last-Modified` cannot support that,
and `fromURL` refuses it rather than quietly falling back to the size check --
an object replaced by a different one of the same length is exactly the failure
the rest of this is for. `allowUnvalidated: true` reads it anyway, and
`identity()` returns `null` so a caller can tell which it got.

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

Which is a claim, so it is checked against someone else's decoder rather than
only against `ggml-quants.c` by eye. `crates/gguf-quants/tests/golden/` holds
vectors decoded by [gguf-py](https://github.com/ggml-org/llama.cpp/tree/master/gguf-py),
the llama.cpp project's own GGUF library, written in numpy by other people from
the same specification. The comparison is equality, not a tolerance: **all
fifteen formats, bit for bit.**

The blocks are synthetic, and no model weights are in this repository. A decoder
does not need a quantizer: every bit pattern is a legal block, since the quants
are fixed-width indices and the scales are f16, so pseudo-random bytes are a
valid block of any format. The one correction is that the f16 scale fields are
overwritten with finite values, because a random f16 is NaN or infinity about
one time in 128 and a NaN's payload is not something two implementations owe
each other.

That is also how the K-quants get covered at all — gguf-py decodes them without
being able to produce them — and it is better coverage than real weights, which
cluster where an off-by-one in a shift does not show.
`scripts/golden-vectors.py` regenerates them.

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
cargo test --workspace                                # the format itself,
                                                      # including the golden
                                                      # vectors from gguf-py
cargo test -p f2i-gguf --features std                 # and the file reader
node --test js/index.test.mjs                         # the source adapters
sh scripts/build-wasm.sh && node scripts/smoke.mjs    # the built module
GGUF_MODEL=model.gguf cargo test -p f2i-gguf --features std   # against a real file
```

A checkpoint is not redistributed here, so the tests that need one skip without
`GGUF_MODEL`. Everything else runs anywhere, including the hostile-file suite
and a smoke test that writes its own GGUF.

## Provenance and licence

The reader and the block formats began in a private Rust reimplementation of
llama.cpp and were rebuilt here to work without `std`. The block layouts must
stay byte-for-byte compatible with upstream `ggml-quants.c`, and the decoders
are written to be read against that source rather than trusted.

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at
your option.
