# zipp-bench: zipp-only
"""Eager torch transformer block training: token and position Embedding,
pre-LayerNorm causal self-attention through F.scaled_dot_product_attention,
a GELU MLP, a vocabulary head, cross-entropy backward and an AdamW step,
repeated. Runs on Zipp's bundled torch subset; the checksum is validated
against the baseline Zipp. Parameters and inputs come from arange formulas and
the printed values are rounded."""
import sys
import time

import torch
import torch.nn.functional as F
from torch import nn

STEPS = 4
BATCH = 2
SEQ = 16
VOCAB = 32
DIM = 32
HEADS = 2


def patterned(shape, count, mul, scale):
    values = (torch.arange(count, dtype=torch.float32) * mul) % 13 - 6.0
    return (values / scale).reshape(shape)


class Block(nn.Module):
    def __init__(self):
        super().__init__()
        self.tok = nn.Embedding(VOCAB, DIM)
        self.pos = nn.Embedding(SEQ, DIM)
        self.ln1 = nn.LayerNorm(DIM)
        self.qkv = nn.Linear(DIM, 3 * DIM)
        self.proj = nn.Linear(DIM, DIM)
        self.ln2 = nn.LayerNorm(DIM)
        self.fc = nn.Linear(DIM, 4 * DIM)
        self.out = nn.Linear(4 * DIM, DIM)
        self.lnf = nn.LayerNorm(DIM)
        self.head = nn.Linear(DIM, VOCAB)

    def forward(self, idx):
        b, t = idx.shape
        x = self.tok(idx) + self.pos(torch.arange(t))
        q, k, v = self.qkv(self.ln1(x)).split(DIM, dim=2)
        q = q.view(b, t, HEADS, DIM // HEADS).transpose(1, 2)
        k = k.view(b, t, HEADS, DIM // HEADS).transpose(1, 2)
        v = v.view(b, t, HEADS, DIM // HEADS).transpose(1, 2)
        y = F.scaled_dot_product_attention(q, k, v, is_causal=True)
        y = y.transpose(1, 2).reshape(b, t, DIM)
        x = x + self.proj(y)
        x = x + self.out(F.gelu(self.fc(self.ln2(x))))
        return self.head(self.lnf(x))


def build():
    model = Block()
    with torch.no_grad():
        for index, param in enumerate(model.parameters()):
            param.copy_(patterned(tuple(param.shape), param.numel(), 7 + 2 * index, 40.0))
    idx = (torch.arange(BATCH * SEQ) * 7 % VOCAB).reshape(BATCH, SEQ)
    target = (torch.arange(BATCH * SEQ) * 5 % VOCAB).reshape(BATCH, SEQ)
    return model, idx, target


def bench(steps):
    model, idx, target = build()
    opt = torch.optim.AdamW(model.parameters(), lr=0.01)
    first = None
    loss = None
    for step in range(steps):
        opt.zero_grad()
        logits = model(idx)
        loss = F.cross_entropy(logits.reshape(BATCH * SEQ, VOCAB), target.reshape(BATCH * SEQ))
        loss.backward()
        opt.step()
        if first is None:
            first = loss.item()
    weight_sum = 0.0
    for param in model.parameters():
        weight_sum += param.detach().abs().sum().item()
    return first, loss.item(), weight_sum


t0 = time.perf_counter()
result = bench(STEPS)
elapsed = time.perf_counter() - t0
print("torch_transformer", STEPS, BATCH, SEQ, "%.4f" % result[0], "%.4f" % result[1], "%.3f" % result[2])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
