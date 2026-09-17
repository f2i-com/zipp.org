#!/usr/bin/env python3
"""Train a deliberately tiny character LM on this file's original toy sentences.
Development only: torch and safetensors are NOT runtime dependencies of ZIPP.
The result demonstrates loading/inference, not useful general language ability.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import sys
import time
import torch
from torch import nn
from torch.nn import functional as F
from safetensors.torch import save_file

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cpython_host import write_json

ROOT = Path(__file__).resolve().parents[1]
CORPUS = [
    "hello zipp!", "hello alice!", "hello lance!", "python runs locally.",
    "a tiny model lives here.", "the little duck likes tea.", "the little cat likes naps.",
    "the little bird can sing.", "one app, one little model.", "green tea and python code.",
    "bring your own model.", "small models can stay offline.", "the duck writes python.",
    "a local model needs no cloud.", "zipp runs a tiny model.", "alice likes green tea.",
]
CONFIG = dict(vocab_size=0, context_length=48, hidden_size=32, num_heads=4,
              num_layers=1, intermediate_size=64, layer_norm_epsilon=1e-5)

class Block(nn.Module):
    def __init__(self, c):
        super().__init__(); h = c['hidden_size']; self.c = c
        self.ln1 = nn.LayerNorm(h, eps=c['layer_norm_epsilon']); self.ln2 = nn.LayerNorm(h, eps=c['layer_norm_epsilon'])
        self.q = nn.Linear(h, h); self.k = nn.Linear(h, h); self.v = nn.Linear(h, h); self.out = nn.Linear(h, h)
        self.up = nn.Linear(h, c['intermediate_size']); self.down = nn.Linear(c['intermediate_size'], h)
    def forward(self, x):
        b, t, h = x.shape; heads = self.c['num_heads']; d = h // heads; n = self.ln1(x)
        q, k, v = [layer(n).reshape(b, t, heads, d).transpose(1, 2) for layer in (self.q, self.k, self.v)]
        mask = torch.triu(torch.full((t, t), -1e9, device=x.device), diagonal=1)
        scores = q @ k.transpose(-1, -2) * (1 / math.sqrt(d)) + mask
        context = (scores.softmax(dim=-1) @ v).transpose(1, 2).contiguous().reshape(b, t, h)
        x = x + self.out(context)
        return x + self.down(F.gelu(self.up(self.ln2(x)), approximate='none'))

class TinyModel(nn.Module):
    def __init__(self, c):
        super().__init__(); self.c = c; h = c['hidden_size']
        self.token = nn.Embedding(c['vocab_size'], h); self.position = nn.Embedding(c['context_length'], h)
        self.blocks = nn.ModuleList([Block(c) for _ in range(c['num_layers'])])
        self.final = nn.LayerNorm(h, eps=c['layer_norm_epsilon']); self.head = nn.Linear(h, c['vocab_size'])
    def forward(self, ids):
        x = self.token(ids) + self.position(torch.arange(ids.shape[1], device=ids.device))
        for block in self.blocks: x = block(x)
        return self.head(self.final(x))
    def export(self):
        out = {'token_embedding': self.token.weight, 'position_embedding': self.position.weight}
        def norm(prefix, layer): out[prefix+'.weight'] = layer.weight; out[prefix+'.bias'] = layer.bias
        def linear(prefix, layer): out[prefix+'.weight'] = layer.weight.T; out[prefix+'.bias'] = layer.bias
        for i, b in enumerate(self.blocks):
            p = 'blocks.'+str(i); norm(p+'.ln1', b.ln1); norm(p+'.ln2', b.ln2)
            for name, layer in [('q',b.q),('k',b.k),('v',b.v),('out',b.out)]: linear(p+'.attn.'+name, layer)
            linear(p+'.ff.up', b.up); linear(p+'.ff.down', b.down)
        norm('final_norm', self.final); linear('lm_head', self.head)
        return {name: value.detach().cpu().contiguous() for name, value in out.items()}

def main():
    parser = argparse.ArgumentParser(); parser.add_argument('--steps', type=int, default=1000)
    args = parser.parse_args(); torch.set_num_threads(2); torch.manual_seed(20260917)
    vocab = ['<bos>', '<eos>', '<unk>'] + sorted(set(''.join(CORPUS)))
    mapping = {ch:i for i,ch in enumerate(vocab)}; c = dict(CONFIG, vocab_size=len(vocab)); model = TinyModel(c)
    sequences = [torch.tensor([0]+[mapping[ch] for ch in line]+[1]) for line in CORPUS]
    x = torch.full((len(sequences), c['context_length']), 1, dtype=torch.long)
    y = torch.full_like(x, -100)
    for i, ids in enumerate(sequences): x[i,:len(ids)-1] = ids[:-1]; y[i,:len(ids)-1] = ids[1:]
    optim = torch.optim.AdamW(model.parameters(), lr=0.003, weight_decay=0.01)
    start = time.time()
    for step in range(args.steps):
        optim.zero_grad(); logits = model(x); loss = F.cross_entropy(logits.reshape(-1,c['vocab_size']), y.reshape(-1))
        loss.backward(); torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0); optim.step()
    model.eval(); folder = ROOT/'examples/tiny-char'; folder.mkdir(parents=True,exist_ok=True)
    save_file(model.export(), str(folder/'weights.safetensors'), metadata={'purpose':'toy integration fixture; original 16-sentence corpus'})
    # The manifest decides the identity, checkpoint family and tokenizer this
    # checkpoint claims: a model trained by this script is a zipp.tiny-causal-v1
    # checkpoint because this script emits that layout, not because it is small.
    identity = json.loads((ROOT/'plugins/tiny-causal/plugin.json').read_text())
    plugin_hash = hashlib.sha256((ROOT/'plugins/tiny-causal/plugin.json').read_bytes()).hexdigest()
    tokenizer = dict(type=identity['tokenizer_formats'][0], vocab=vocab, bos_token_id=0,eos_token_id=1,unk_token_id=2)
    manifest = dict(format='zipp.local-model', version=1, architecture=dict(id=identity['id'],version=identity['plugin_version'],sha256=plugin_hash),
                    checkpoint_format=identity['checkpoint_format'],
                    config=c,tokenizer=tokenizer,weights=[dict(path='weights.safetensors',sha256=hashlib.sha256((folder/'weights.safetensors').read_bytes()).hexdigest())],
                    license='CC0-1.0; original toy training corpus included in tools/train_demo.py')
    write_json(folder/'model.json', manifest)
    cases = []
    with torch.no_grad():
        for prompt in ['', 'hello ', 'the little ', 'zipp runs ', 'alice likes ']:
            ids = [0]+[mapping.get(ch,2) for ch in prompt]
            expected = model(torch.tensor([ids]))[0].tolist()
            current = list(ids); generated = []
            for _ in range(32):
                if len(current)>c['context_length']: break
                token = int(model(torch.tensor([current]))[0,-1].argmax())
                if token == 1: break
                generated.append(token);current.append(token)
            text = ''.join(vocab[t] if t>=3 else '\ufffd' if t==2 else '' for t in generated)
            cases.append(dict(prompt=prompt,tokens=ids,logits=expected,greedy_tokens=generated,greedy_text=text))
    write_json(folder/'oracle.json', dict(source='CPython/PyTorch CPU reference; not a ZIPP execution result',cases=cases))
    report = dict(seed=20260917,steps=args.steps,torch_version=torch.__version__,training_seconds=time.time()-start,
                  final_training_loss=float(loss.detach()),parameters=sum(p.numel() for p in model.parameters()),
                  checkpoint_bytes=(folder/'weights.safetensors').stat().st_size,corpus_sentences=len(CORPUS),
                  note='Training-set examples only. No held-out language-quality benchmark.',samples=[{k:v for k,v in case.items() if k in ['prompt','greedy_text']} for case in cases])
    write_json(folder/'training-report.json', report);print(json.dumps(report,indent=2))

if __name__ == '__main__': main()
