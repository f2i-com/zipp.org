#!/usr/bin/env python3
"""Build a tiny Qwen3-shaped GGUF, so the stage tests run without a checkpoint.

    pip install gguf numpy
    python tools/make_tiny_qwen3.py

## Why

Every staging test skips without `ZIPP_QWEN3_MODEL`, which means CI proves
nothing about layer ranges, seams, caches or worker isolation -- the parts most
likely to break and least likely to be noticed. A checkpoint cannot be
committed: 400 MB, and somebody else's weights.

So this writes one. Eight layers, a 1024-token vocabulary, random weights: it
computes nothing meaningful and is not supposed to. What it has is the *shape*
of the real thing, which is what those tests are about.

## Quantized on purpose

The weights are real Q4_K and Q6_K blocks, not F32, because a fixture that only
exercises float paths would not notice a packed matmul breaking -- and packed
matmuls are the reason any of this fits in a browser.

gguf-py cannot quantize K-quants. It does not have to: every bit pattern is a
legal block, since the quants are fixed-width indices and the scales are f16.
So the blocks are pseudo-random bytes with the f16 scale fields overwritten by
finite values, written through `add_tensor(raw_dtype=...)`. The same trick as
the golden vectors in gguf-wasm, for the same reason.

The numbers a model built this way produces are meaningless. Every test that
uses it compares ZIPP against ZIPP, or against a reference generated from this
same file -- never against a claim about language.
"""
import hashlib
import json
import pathlib
import sys

import numpy as np
from gguf import GGUFWriter
from gguf.constants import GGMLQuantizationType as Q

HERE = pathlib.Path(__file__).resolve().parent.parent
OUT = HERE / "tests" / "fixtures" / "tiny-qwen3.gguf"

# Small, but not so small that the shapes stop being the shapes. hidden must be
# a multiple of 256 for a K-quant row to be whole, which sets the floor.
HIDDEN, HEADS, KV_HEADS, HEAD_DIM = 256, 4, 2, 64
# Eight layers so it divides in halves, quarters and unevenly -- four would
# not leave enough for four peers to each hold one.
LAYERS, INTERMEDIATE, VOCAB, CONTEXT = 8, 512, 1024, 64
EPSILON, ROPE_BASE = 1e-6, 10000.0

BLOCK = {Q.Q4_K: (256, 144, [0, 2]), Q.Q6_K: (256, 210, [208])}


def blocks(rng, kind, rows, columns):
    """Valid blocks of `kind` for a [rows, columns] tensor, from noise.

    Random bytes are a legal block -- there is no encoding to get wrong -- with
    one correction: the f16 scale fields are given finite values, because a
    random f16 is NaN or infinity about one time in 128 and a tensor full of
    infinities makes every test fail for the wrong reason.
    """
    values, size, scale_at = BLOCK[kind]
    if columns % values:
        raise ValueError(f"{columns} is not a whole number of {values}-value blocks")
    count = rows * (columns // values)
    raw = bytearray(rng.integers(0, 256, count * size, dtype=np.uint8).tobytes())
    # Modest exponents: a hidden state that overflows float32 after four layers
    # would be a fixture that tests the overflow rather than the wiring.
    scales = (rng.uniform(-1.0, 1.0, count * len(scale_at)).astype(np.float32)
              * np.float32(2.0) ** rng.integers(-4, 1, count * len(scale_at)).astype(np.float32)
              ).astype(np.float16)
    k = 0
    for block in range(count):
        for offset in scale_at:
            raw[block * size + offset: block * size + offset + 2] = scales[k].tobytes()
            k += 1
    # Shaped in *bytes*, which is what GGUFWriter wants for a raw tensor: it
    # derives the logical shape back from the block size.
    return np.frombuffer(bytes(raw), dtype=np.uint8).reshape(rows, -1)


def byte_alphabet():
    """The 256 single-byte tokens a byte-level BPE actually uses.

    Not latin-1: byte-level BPE writes bytes as printable characters through
    GPT-2's mapping, so a space is "Ġ" and a newline "Ċ". A
    vocabulary built from raw latin-1 looks similar and shares almost none of
    its entries with what the tokenizer will ask for.
    """
    printable = (list(range(ord("!"), ord("~") + 1))
                 + list(range(0xa1, 0xad)) + list(range(0xae, 0x100)))
    mapping, spare = dict.fromkeys(printable), 0
    for byte in printable:
        mapping[byte] = chr(byte)
    for byte in range(256):
        if byte not in mapping:
            mapping[byte] = chr(0x100 + spare)
            spare += 1
    return [mapping[byte] for byte in range(256)]


def vocabulary():
    """A byte-level BPE that really is one: every byte, then learned pairs.

    A tokenizer with no merges cannot join anything, and the reader refuses a
    checkpoint without them -- rightly, since a byte-BPE vocabulary without a
    merge table is not a vocabulary, it is an alphabet. So pairs are built the
    way BPE builds them: each new token is two existing ones joined, and the
    merge that makes it is recorded in the order it was learned.
    """
    tokens = list(byte_alphabet())
    seen = set(tokens)
    merges = []
    # Deterministic and dull: join adjacent tokens, then join those, which
    # gives a table with real structure rather than a list of singletons.
    left = 0
    while len(tokens) < VOCAB:
        a, b = tokens[left], tokens[(left + 1) % len(tokens)]
        joined = a + b
        left += 1
        if joined in seen or len(joined) > 16:
            continue
        seen.add(joined)
        merges.append(a + " " + b)
        tokens.append(joined)
    return tokens, merges


def main():
    rng = np.random.default_rng(0x5EED)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    writer = GGUFWriter(str(OUT), "qwen3")

    writer.add_name("tiny-qwen3")
    writer.add_context_length(CONTEXT)
    writer.add_embedding_length(HIDDEN)
    writer.add_block_count(LAYERS)
    writer.add_feed_forward_length(INTERMEDIATE)
    writer.add_head_count(HEADS)
    writer.add_head_count_kv(KV_HEADS)
    writer.add_key_length(HEAD_DIM)
    writer.add_value_length(HEAD_DIM)
    writer.add_layer_norm_rms_eps(EPSILON)
    writer.add_rope_freq_base(ROPE_BASE)

    tokens, merges = vocabulary()
    writer.add_tokenizer_model("gpt2")
    writer.add_tokenizer_pre("qwen2")
    writer.add_token_list(tokens)
    writer.add_token_types([1] * len(tokens))
    writer.add_token_merges(merges)
    writer.add_eos_token_id(len(tokens) - 1)

    q_width, kv_width = HEADS * HEAD_DIM, KV_HEADS * HEAD_DIM
    norm = lambda n: rng.uniform(0.8, 1.2, n).astype(np.float32)

    # Tied: the embedding is also the output projection, so it is written once.
    embedding = blocks(rng, Q.Q6_K, VOCAB, HIDDEN)
    writer.add_tensor("token_embd.weight", embedding,
                      raw_shape=embedding.shape, raw_dtype=Q.Q6_K)
    writer.add_tensor("output_norm.weight", norm(HIDDEN))

    for layer in range(LAYERS):
        b = f"blk.{layer}."
        writer.add_tensor(b + "attn_norm.weight", norm(HIDDEN))
        writer.add_tensor(b + "ffn_norm.weight", norm(HIDDEN))
        writer.add_tensor(b + "attn_q_norm.weight", norm(HEAD_DIM))
        writer.add_tensor(b + "attn_k_norm.weight", norm(HEAD_DIM))
        for name, out, ins in [("attn_q.weight", q_width, HIDDEN),
                               ("attn_k.weight", kv_width, HIDDEN),
                               ("attn_v.weight", kv_width, HIDDEN),
                               ("attn_output.weight", HIDDEN, q_width),
                               ("ffn_gate.weight", INTERMEDIATE, HIDDEN),
                               ("ffn_up.weight", INTERMEDIATE, HIDDEN),
                               ("ffn_down.weight", HIDDEN, INTERMEDIATE)]:
            packed = blocks(rng, Q.Q4_K, out, ins)
            writer.add_tensor(b + name, packed, raw_shape=packed.shape, raw_dtype=Q.Q4_K)

    writer.write_header_to_file()
    writer.write_kv_data_to_file()
    writer.write_tensors_to_file()
    writer.close()

    digest = hashlib.sha256(OUT.read_bytes()).hexdigest()
    (OUT.parent / "tiny-qwen3.json").write_text(json.dumps({
        "generator": "tools/make_tiny_qwen3.py",
        "purpose": "Shape, not meaning: a Qwen3-shaped GGUF with random weights, "
                   "so the stage and seam tests run without a real checkpoint.",
        "sha256": digest,
        "bytes": OUT.stat().st_size,
        "config": {"hidden_size": HIDDEN, "num_heads": HEADS, "num_kv_heads": KV_HEADS,
                   "head_dim": HEAD_DIM, "num_layers": LAYERS,
                   "intermediate_size": INTERMEDIATE, "vocab_size": VOCAB,
                   "context_length": CONTEXT},
        "quantized": "token_embd is Q6_K, every projection Q4_K, so the packed "
                     "matmul is exercised rather than only the float path",
    }, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"{OUT.relative_to(HERE)}: {OUT.stat().st_size} bytes, sha256 {digest[:16]}")


if __name__ == "__main__":
    main()
