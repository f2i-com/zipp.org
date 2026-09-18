"""Reference logits for a GGUF checkpoint, from an implementation that is not ours.

transformers reads GGUF itself: its own container reader, gguf-py's block
decoders, and its own Qwen3 in PyTorch. Nothing on that path is shared with
ZIPP, and both start from the same file -- which is what makes the comparison
mean something the existing tests do not already say.
"""
import hashlib
import json
import pathlib
import sys

import numpy as np
import torch
from transformers import AutoModelForCausalLM, AutoTokenizer

MODEL = sys.argv[1] if len(sys.argv) > 1 else "E:/models/qwen3-0.6b-q4_k_m.gguf"
OUT = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else ".")

PROMPTS = [
    "The capital of France is",
    "1 2 3 4",
    "def add(a, b):",
]

path = pathlib.Path(MODEL)
digest = hashlib.sha256(path.read_bytes()).hexdigest()
print(f"{path.name}\n  sha256 {digest}")

name, gguf = str(path.parent), path.name
print("  loading through transformers (dequantizes to f32)...")
model = AutoModelForCausalLM.from_pretrained(name, gguf_file=gguf, dtype=torch.float32)
model.eval()
tok = AutoTokenizer.from_pretrained(name, gguf_file=gguf)
print(f"  {model.config.model_type}, {model.config.num_hidden_layers} layers, "
      f"vocab {model.config.vocab_size}")

entries = []
for prompt in PROMPTS:
    ids = tok(prompt, return_tensors="pt").input_ids
    with torch.no_grad():
        logits = model(ids).logits[0, -1].to(torch.float32).numpy()
    top = int(np.argmax(logits))
    entries.append({
        "prompt": prompt,
        "ids": ids[0].tolist(),
        "argmax": top,
        "argmax_piece": tok.convert_ids_to_tokens([top])[0],
        "logits_sha256": hashlib.sha256(logits.astype("<f4").tobytes()).hexdigest(),
    })
    logits.astype("<f4").tofile(OUT / f"qwen3_logits_{len(entries)}.f32")
    piece = entries[-1]["argmax_piece"].encode("ascii", "backslashreplace").decode()
    print(f"  {prompt!r} -> {ids.shape[1]} ids, argmax {top} "
          f"({piece}), range [{logits.min():.4f}, {logits.max():.4f}]")

(OUT / "qwen3_oracle.json").write_text(json.dumps({
    "model": path.name,
    "model_sha256": digest,
    "reference": "transformers + gguf-py, both reading this GGUF directly",
    "vocab_size": int(model.config.vocab_size),
    "prompts": entries,
}, indent=2) + "\n", encoding="utf-8", newline="\n")
print(f"\nwrote {len(entries)} reference vectors of {model.config.vocab_size} logits")
