"""Configurable pre-norm causal decoder, supplied as ordinary Python source.
This custom checkpoint schema is intentionally NOT advertised as GPT-Neo/Llama.
Graph execution, weight reading and resource policy belong to the host.
"""
import math
from .graph import Graph
from .tokenizer import encode, decode

def describe(config):
    required = {"vocab_size", "context_length", "hidden_size", "num_heads", "num_layers", "intermediate_size", "layer_norm_epsilon"}
    if set(config) != required:
        raise ValueError("Unsupported tiny-causal configuration fields")
    for key, maximum in [("vocab_size", 65536), ("context_length", 512), ("hidden_size", 512),
                         ("num_heads", 32), ("num_layers", 8), ("intermediate_size", 2048)]:
        if type(config[key]) is not int or not 1 <= config[key] <= maximum:
            raise ValueError("Invalid model dimension: " + key)
    if config["hidden_size"] % config["num_heads"] != 0:
        raise ValueError("hidden_size must be divisible by num_heads")
    epsilon = config["layer_norm_epsilon"]
    if not isinstance(epsilon, (int, float)) or not math.isfinite(epsilon) or not 0 < epsilon <= 1:
        raise ValueError("Invalid LayerNorm epsilon")
    return {"task": "causal-lm", "vocab_size": config["vocab_size"], "max_context": config["context_length"]}

def build_graph(config, tokens):
    description = describe(config)
    if not isinstance(tokens, list) or not 1 <= len(tokens) <= description["max_context"]:
        raise ValueError("Invalid context length")
    if any(type(token) is not int or not 0 <= token < config["vocab_size"] for token in tokens):
        raise ValueError("Invalid token id")
    length = len(tokens)
    hidden, heads = config["hidden_size"], config["num_heads"]
    dim, intermediate = hidden // heads, config["intermediate_size"]
    g = Graph()
    x = g.rows("token_embedding", tokens, hidden)
    x = g.op("add", a=x, b=g.rows("position_embedding", list(range(length)), hidden))
    mask = g.causal_mask(length)
    for layer in range(config["num_layers"]):
        prefix = "blocks." + str(layer)
        n = g.layer_norm(x, prefix + ".ln1", hidden, config["layer_norm_epsilon"])
        q = g.linear(n, prefix + ".attn.q", hidden, hidden)
        k = g.linear(n, prefix + ".attn.k", hidden, hidden)
        v = g.linear(n, prefix + ".attn.v", hidden, hidden)
        q = g.op("permute", a=g.op("reshape", a=q, shape=[length, heads, dim]), dims=[1, 0, 2])
        k = g.op("permute", a=g.op("reshape", a=k, shape=[length, heads, dim]), dims=[1, 2, 0])
        v = g.op("permute", a=g.op("reshape", a=v, shape=[length, heads, dim]), dims=[1, 0, 2])
        scores = g.op("mul", a=g.op("matmul", a=q, b=k), b=g.scalar(1.0 / math.sqrt(dim)))
        scores = g.op("add", a=scores, b=mask)
        probabilities = g.op("softmax", a=scores, axis=-1)
        context = g.op("matmul", a=probabilities, b=v)
        context = g.op("reshape", a=g.op("permute", a=context, dims=[1, 0, 2]), shape=[length, hidden])
        x = g.op("add", a=x, b=g.linear(context, prefix + ".attn.out", hidden, hidden))
        n = g.layer_norm(x, prefix + ".ln2", hidden, config["layer_norm_epsilon"])
        # ZIPP's graph GELU is the erf form, not GPT-2's tanh approximation.
        ff = g.op("gelu", a=g.linear(n, prefix + ".ff.up", hidden, intermediate))
        x = g.op("add", a=x, b=g.linear(ff, prefix + ".ff.down", intermediate, hidden))
    x = g.layer_norm(x, "final_norm", hidden, config["layer_norm_epsilon"])
    logits = g.linear(x, "lm_head", hidden, config["vocab_size"])
    return g.finish(logits)
