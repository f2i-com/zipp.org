# Rust that becomes WebAssembly

Four crates, two build outputs, one rule: nothing here allocates a checkpoint
and nothing here is a second copy of something that already exists.

| Crate | `std`? | Becomes |
| --- | --- | --- |
| `zipp-quants` | no | — (used by the two below) |
| `zipp-gguf` | no | — |
| `zipp-kernels` | no | `../gpu-lab/wasm/kernels.wasm` |
| `zipp-model-wasm` | yes | `../model-plugins/wasm/gguf{,-node}/` |

```sh
../gpu-lab/scripts/build_wasm.sh              # the Graph v2 kernels
../model-plugins/scripts/build_gguf_wasm.sh   # the GGUF reader, via wasm-bindgen
cargo test                                    # all four, on the host
```

## Where this came from

`zipp-quants`, `zipp-gguf` and the `wasm-bindgen` surface in `zipp-model-wasm`
are vendored from the private `llm` repository — a Rust reimplementation of
llama.cpp — at commit `e1b9d48`, from its `ggml-quants`, `gguf` and `gguf-wasm`
crates. Keeping a diff possible is the reason that commit is written down.

What changed, and only this:

* **No `std`.** `thiserror` is a procedural macro over `std::error::Error`, so
  the two error enums spell their `Display` out by hand. `std::collections`,
  `std::sync::Arc` and `std::string` become their `alloc` equivalents.
* **No files.** The reader's memory-mapped backing and `GgufFile::open` are
  gone; only `from_bytes` remains. In a browser the header arrives as bytes a
  host read out of a `File`, a `Blob` or a ranged request, and a tensor body is
  an offset and a length the host reads for itself. Nothing holds a checkpoint.
* **Renamed** with a `zipp-` prefix, so a workspace can depend on both these and
  the originals without a collision.

`ggml-rs`, `ggml-rs-cuda` and `llama-rs` are deliberately **not** vendored.
Graph v2 is this repository's inference engine; those are the other repository's,
and a CUDA backend has no meaning in a browser.

## `zipp-kernels` is a port, not a vendor

It replaces `gpu-lab/wasm/kernels.c`, which was freestanding C built with clang
and `wasm-ld`. Rust reaches the same place with `cargo` alone — no separate
link step, and no `wasm-opt` post-link stage for an npm shim on `PATH` to break
on Windows. Two things made the swap safe rather than brave:

* `gpu-lab` holds the compiled backend to **bit-for-bit** equality with its
  JavaScript reference over whole training steps, so a port either reproduces
  every float exactly or the suite says which one it did not. It does.
* Rust never contracts `a * b + c` into an FMA, so each operation rounds on its
  own. The C needed `-ffp-contract=off` to promise the same thing.

The elementary functions in `math.rs` are ported term for term from that C
rather than taken from a libm, because "agrees with `Math.exp` to a double ulp"
is what survives the round to float32, and a different polynomial would not
necessarily agree. They are pure `f64` arithmetic, so they are unit-tested on
the host as well as exercised through the kernels.

`zipp-kernels` depends on `zipp-quants` for Q4_K. That is the point of the
arrangement: the decoder that reads a weight inside a matmul is the same one the
GGUF reader uses, and there is no third implementation to drift. The JavaScript
reference backend has its own, in `gpu-lab/src/quant.mjs`, and the suite holds
the two to equality.

## What is committed

The build outputs are, the way `kernels.wasm` always has been: a browser needs
them and not everyone who clones this has a Rust toolchain. `.gitattributes`
marks `*.wasm` binary.
