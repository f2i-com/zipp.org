"""A Qwen3 decoder layer written in ZIPP's torch subset, not as a graph.

The question: can transformers-style modelling code run on ZIPP's Python,
instead of a plugin hand-building Graph v2 nodes? Everything below is ordinary
torch idiom -- matmul, softmax, silu, rsqrt, cos/sin, repeat_interleave -- and
the weights and reference output come from real PyTorch's Qwen3DecoderLayer.
"""
import json
import math

import torch
import torch.nn.functional as F

with open("assets/case.json") as handle:
    CASE = json.load(handle)
C = CASE["config"]


def rms_norm(x, weight, eps):
    return x * (x.pow(2).mean(-1, keepdim=True) + eps).rsqrt() * weight


def rope_tables(length, dim, theta):
    inverse = 1.0 / (theta ** (torch.arange(0, dim, 2).float() / dim))
    freqs = torch.outer(torch.arange(length).float(), inverse)
    emb = torch.cat([freqs, freqs], dim=-1)
    return emb.cos(), emb.sin()


def rotate_half(x):
    half = x.shape[-1] // 2
    return torch.cat([-x.narrow(-1, half, half), x.narrow(-1, 0, half)], dim=-1)


def load(name, shape):
    return torch.tensor(CASE["weights"][name]).reshape(shape)


def forward():
    hidden, heads, kv_heads = C["hidden_size"], C["num_attention_heads"], C["num_key_value_heads"]
    head_dim, intermediate, eps = C["head_dim"], C["intermediate_size"], C["rms_norm_eps"]
    length = C["length"]
    x = torch.tensor(CASE["input"]).reshape(length, hidden)

    residual = x
    h = rms_norm(x, load("input_layernorm.weight", [hidden]), eps)
    q = h @ load("self_attn.q_proj.weight", [heads * head_dim, hidden]).transpose(0, 1)
    k = h @ load("self_attn.k_proj.weight", [kv_heads * head_dim, hidden]).transpose(0, 1)
    v = h @ load("self_attn.v_proj.weight", [kv_heads * head_dim, hidden]).transpose(0, 1)
    q = q.reshape(length, heads, head_dim).transpose(0, 1)
    k = k.reshape(length, kv_heads, head_dim).transpose(0, 1)
    v = v.reshape(length, kv_heads, head_dim).transpose(0, 1)
    # Qwen3 normalises every head of q and k before the rotation.
    q = rms_norm(q, load("self_attn.q_norm.weight", [head_dim]), eps)
    k = rms_norm(k, load("self_attn.k_norm.weight", [head_dim]), eps)
    cos, sin = rope_tables(length, head_dim, C["rope_theta"])
    q = q * cos + rotate_half(q) * sin
    k = k * cos + rotate_half(k) * sin
    repeats = heads // kv_heads
    k = k.repeat_interleave(repeats, dim=0)
    v = v.repeat_interleave(repeats, dim=0)
    scores = (q @ k.transpose(-1, -2)) * (1.0 / math.sqrt(head_dim))
    mask = [[0.0 if column <= row else -1e9 for column in range(length)] for row in range(length)]
    attention = (scores + torch.tensor(mask)).softmax(-1) @ v
    attention = attention.transpose(0, 1).reshape(length, heads * head_dim)
    x = residual + attention @ load("self_attn.o_proj.weight", [hidden, heads * head_dim]).transpose(0, 1)

    residual = x
    h = rms_norm(x, load("post_attention_layernorm.weight", [hidden]), eps)
    gate = h @ load("mlp.gate_proj.weight", [intermediate, hidden]).transpose(0, 1)
    up = h @ load("mlp.up_proj.weight", [intermediate, hidden]).transpose(0, 1)
    down = (F.silu(gate) * up) @ load("mlp.down_proj.weight", [hidden, intermediate]).transpose(0, 1)
    return residual + down


def probe():
    return json.dumps(forward().reshape(-1).tolist())
