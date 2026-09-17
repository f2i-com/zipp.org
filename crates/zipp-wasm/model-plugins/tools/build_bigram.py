#!/usr/bin/env python3
"""Regenerate the tiny deterministic table fixture after manifest updates.

Run `build_manifests.py` first: this reads the plugin manifest it produces, so
the model's identity pin, checkpoint format and weights stay consistent.
"""
from pathlib import Path
import hashlib
import json
import struct
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cpython_host import write_json

ROOT = Path(__file__).resolve().parents[1]
folder = ROOT/'examples/bigram'
folder.mkdir(exist_ok=True)
vocab = ['<bos>', '<eos>', '<unk>', 'a', 'b', 'c', '.']
v = len(vocab)
w = [-8.0]*(v*v)
for a, b in [(0, 3), (3, 4), (4, 5), (5, 6), (6, 1), (1, 1), (2, 1)]:
    w[a*v + b] = 8.0
header = json.dumps({'transition_logits': {'dtype': 'F32', 'shape': [v, v], 'data_offsets': [0, v*v*4]}},
                    separators=(',', ':')).encode()
header += b' '*((-len(header)) % 8)
raw = struct.pack('<Q', len(header)) + header + struct.pack('<' + 'f'*len(w), *w)
(folder/'weights.safetensors').write_bytes(raw)
manifest_path = ROOT/'plugins/bigram/plugin.json'
plugin = json.loads(manifest_path.read_text())
model = {
    'format': 'zipp.local-model', 'version': 1,
    'architecture': {'id': plugin['id'], 'version': plugin['plugin_version'],
                     'sha256': hashlib.sha256(manifest_path.read_bytes()).hexdigest()},
    'checkpoint_format': plugin['checkpoint_format'],
    'config': {'vocab_size': v, 'context_length': 32},
    'tokenizer': {'type': plugin['tokenizer_formats'][0], 'vocab': vocab,
                  'bos_token_id': 0, 'eos_token_id': 1, 'unk_token_id': 2},
    'weights': [{'path': 'weights.safetensors', 'sha256': hashlib.sha256(raw).hexdigest()}],
    'license': 'CC0-1.0; hand-authored deterministic integration fixture, not a trained LLM',
}
write_json(folder/'model.json', model)
print('Bigram:', len(raw), 'bytes; expected generation abc.')
