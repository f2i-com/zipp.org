# The compute kernels, as WebAssembly

One crate. `zipp-kernels` builds `../gpu-lab/wasm/kernels.wasm`: freestanding
float32 kernels for Graph v2, with no `std`, no allocator, no libc and no
imports. The host instantiates the module, reads `memory` and `__heap_base`,
and drives an arena itself.

```sh
../gpu-lab/scripts/build_wasm.sh   # cargo alone, no separate link step
cargo test                          # the elementary functions, on the host
```

## The contract is bit-for-bit

`gpu-lab` holds this backend to exact equality with its JavaScript reference
over whole training steps, not to a tolerance. Two things keep that true and
must survive any edit:

* Rust never contracts `a * b + c` into an FMA, so each operation rounds on its
  own — which is what the reference does. (The C this replaced needed
  `-ffp-contract=off` to promise the same thing.)
* Every output accumulates its terms in index order. SIMD therefore goes
  *across outputs*, never along the reduced axis.

The elementary functions in `math.rs` are ported term for term from that C
rather than taken from a libm, because "agrees with `Math.exp` to a double ulp"
is what survives the round to float32 and a different polynomial would not
necessarily. They are pure `f64` arithmetic, so they are unit-tested on the
host as well as exercised through the kernels.

## Reading GGUF is somewhere else now

The container, the block formats and the tokenizer used to live here. They are
generic — useful to anyone reading a GGUF file, with no opinion about how a
model runs — so they became their own project:

**https://github.com/f2i-com/gguf-wasm**

ZIPP consumes it twice, and the two pins move together:

| | |
| --- | --- |
| `Cargo.toml` here | takes `gguf-quants` from the tag, because a matmul that reads a quantized weight must decode it exactly as the reader does. |
| `../model-plugins/scripts/fetch_gguf_wasm.sh` | fetches that tag's WebAssembly build, checksum-verified, into `../model-plugins/wasm/`. |

Using the same decoder rather than a copy of it is the point: `gpu-lab`'s suite
checks its JavaScript decoder against that module and checks these kernels
against the JavaScript one, so all three agree or the suite names which does
not.

What stayed here is what is only meaningful inside ZIPP — kernels held to a
compute protocol this repository defines. See
`../model-plugins/interop/GGUF.md` for where the boundary runs and why.

## What is committed

`kernels.wasm` is, the way it always has been: a browser needs it and not
everyone who clones this has a Rust toolchain. `.gitattributes` marks it
binary.
