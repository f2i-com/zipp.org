"""Qwen3: the first checkpoint family this host runs without expanding it.

A separate plugin, not a configuration of the GPT-Neo one. They share the words
"attention" and "MLP" and agree on nothing else that matters:

  * RMSNorm, no bias anywhere, and a second norm applied **per head** to the
    query and key projections before rotation -- the thing that distinguishes
    Qwen3 from Qwen2.
  * Rotary position embeddings with a base of 1,000,000, not learned position
    embeddings.
  * Grouped-query attention: sixteen query heads over eight key/value heads.
  * A gated SwiGLU MLP, three matrices, not two.
  * Attention scaled by 1/sqrt(head_dim), which GPT-Neo omits entirely.
  * The output projection tied to the token embedding.

The configuration comes from the checkpoint itself. A GGUF file carries its own
hyperparameters under its architecture's prefix, so unlike a Hugging Face folder
there is no `config.json` to read and nothing to convert: `config_from_gguf`
maps the metadata this plugin needs and refuses anything it does not recognise.

The weights are read where they lie. Every projection is bound with `matrix`,
which hands the host the checkpoint's own `[out, in]` tensor and lets it keep
the block format where a backend can decode it. For this family that is most of
the file -- a 0.6B Qwen3 is 373 MB resident where float32 would be 2,274 MB --
and the graph is identical either way, because the multiply is transposed
regardless.
"""
from zipp_plugin.graph import Graph, rope_tables

CHECKPOINT_FORMAT = "gguf.qwen3-v1"
TOKENIZER_FORMATS = ["qwen2-byte-bpe-v1"]

REQUIRED = {"vocab_size", "context_length", "hidden_size", "num_heads", "num_kv_heads",
            "head_dim", "num_layers", "intermediate_size", "rms_norm_epsilon", "rope_base"}
LIMITS = [("vocab_size", 2, 262144), ("context_length", 1, 262144), ("hidden_size", 1, 8192),
          ("num_heads", 1, 128), ("num_kv_heads", 1, 128), ("head_dim", 2, 512),
          ("num_layers", 1, 64), ("intermediate_size", 1, 32768)]

# GGUF metadata keys, under the architecture's own prefix.
GGUF_KEYS = {
    "hidden_size": "embedding_length", "num_heads": "attention.head_count",
    "num_kv_heads": "attention.head_count_kv", "num_layers": "block_count",
    "intermediate_size": "feed_forward_length", "context_length": "context_length",
    "rms_norm_epsilon": "attention.layer_norm_rms_epsilon", "rope_base": "rope.freq_base",
    "head_dim": "attention.key_length",
}


def config_from_gguf(metadata, vocab_size):
    """The plugin's own configuration, out of the file's own metadata."""
    architecture = metadata.get("general.architecture")
    if architecture != "qwen3":
        raise ValueError("Not a qwen3 checkpoint: " + str(architecture))
    config = {"vocab_size": int(vocab_size)}
    for name, suffix in GGUF_KEYS.items():
        key = "qwen3." + suffix
        if key not in metadata:
            raise ValueError("Checkpoint metadata is missing " + key)
        value = metadata[key]
        config[name] = float(value) if name == "rms_norm_epsilon" else int(value)
    return config


def describe(config):
    if set(config) != REQUIRED:
        raise ValueError("Unexpected configuration fields")
    for name, low, high in LIMITS:
        value = config[name]
        if type(value) is not int or not low <= value <= high:
            raise ValueError("Configuration out of range: " + name)
    if config["num_heads"] % config["num_kv_heads"] != 0:
        raise ValueError("Query heads must be a multiple of key/value heads")
    if config["head_dim"] % 2 != 0:
        raise ValueError("Rotary embeddings need an even head dimension")
    epsilon = config["rms_norm_epsilon"]
    if not isinstance(epsilon, float) or not 0.0 < epsilon < 1.0:
        raise ValueError("Configuration out of range: rms_norm_epsilon")
    return {
        "family": "qwen3",
        "checkpoint_format": CHECKPOINT_FORMAT,
        "tokenizer_formats": list(TOKENIZER_FORMATS),
        "vocab_size": config["vocab_size"],
        # The graph is built for one prompt at a time, and every tensor in it is
        # sized by the prompt, so the host's context ceiling is what binds.
        "max_context": config["context_length"],
        "tied_embeddings": True,
    }


def tensor_names(config):
    """Every tensor this graph binds, in the checkpoint's own naming. Listed so
    a host can check a file before building anything."""
    names = ["token_embd.weight", "output_norm.weight"]
    for layer in range(config["num_layers"]):
        block = "blk." + str(layer) + "."
        names.extend([block + "attn_norm.weight", block + "attn_q.weight",
                      block + "attn_k.weight", block + "attn_v.weight",
                      block + "attn_q_norm.weight", block + "attn_k_norm.weight",
                      block + "attn_output.weight", block + "ffn_norm.weight",
                      block + "ffn_gate.weight", block + "ffn_up.weight",
                      block + "ffn_down.weight"])
    return names


def build_decode_graph(config, context=None):
    """The same model as one cached step: read one token, attend over the cache.

    `build_graph` recomputes the whole prompt for every token and is the
    correctness oracle. This is the shape that makes a checkpoint usable: the
    host prepares it once, so every weight -- 372 MB of blocks for the 0.6B --
    is uploaded once instead of per token; the key and value caches live on the
    device and never cross the boundary; and a token costs one position rather
    than the whole context.

    The arithmetic is identical to `build_graph` at the last position. Two
    things differ. The keys and values come from the caches, with this token's
    written in first -- by arithmetic, because the protocol has no scatter. And
    the rotary tables are fed per step rather than built in, because this graph
    is built once and run at every position.
    """
    description = describe(config)
    context = description["max_context"] if context is None else int(context)
    if not 1 <= context <= description["max_context"]:
        raise ValueError("Invalid decode context")

    hidden = config["hidden_size"]
    heads, kv_heads = config["num_heads"], config["num_kv_heads"]
    dim = config["head_dim"]
    repeats = heads // kv_heads
    epsilon = config["rms_norm_epsilon"]
    intermediate = config["intermediate_size"]
    q_width, kv_width = heads*dim, kv_heads*dim

    g = Graph()
    x = g.step_rows("token_embd.weight", "token", hidden)
    mask = g.step_mask(context)
    write = g.step_write(context)
    keep = g.op("sub", a=g.scalar(1.0), b=write)
    cos = g.step_rope("cos", dim, config["rope_base"])
    sin = g.step_rope("sin", dim, config["rope_base"])
    scale = g.scalar(1.0 / (dim ** 0.5))

    for layer in range(config["num_layers"]):
        block = "blk." + str(layer) + "."
        n = g.rms_norm(x, block + "attn_norm.weight", hidden, epsilon)
        q = g.op("reshape", a=g.linear(n, block + "attn_q.weight", hidden, q_width),
                 shape=[1, heads, dim])
        k = g.op("reshape", a=g.linear(n, block + "attn_k.weight", hidden, kv_width),
                 shape=[1, kv_heads, dim])
        v = g.op("reshape", a=g.linear(n, block + "attn_v.weight", hidden, kv_width),
                 shape=[1, kv_heads, dim])
        q = g.rms_norm(q, block + "attn_q_norm.weight", dim, epsilon)
        k = g.rms_norm(k, block + "attn_k_norm.weight", dim, epsilon)
        q = g.rope(q, 1, heads, dim, cos, sin)
        k = g.rope(k, 1, kv_heads, dim, cos, sin)

        # Into the caches, which are [context, kv_width] and carried.
        keys = g.write_cache("k" + str(layer), g.cache("k" + str(layer), [context, kv_width]),
                             keep, write, g.op("reshape", a=k, shape=[1, kv_width]))
        values = g.write_cache("v" + str(layer), g.cache("v" + str(layer), [context, kv_width]),
                               keep, write, g.op("reshape", a=v, shape=[1, kv_width]))
        # Grouped-query attention without repeating anything.
        #
        # The prefill path copies each key head out to the query heads it
        # serves, because its mask is [tokens, tokens] and cannot broadcast
        # across a grouped batch. One token needs no such thing: viewing the
        # queries as [kv_heads, repeats, dim] makes the batch dimension the
        # key head itself, so the cache is read where it lies. That removes a
        # [context, heads, dim] copy of both caches every step -- 29 million
        # elements written per token on this model -- and halves what the
        # permutes move.
        qg = g.op("reshape", a=q, shape=[kv_heads, repeats, dim])
        kh = g.op("permute", a=g.op("reshape", a=keys, shape=[context, kv_heads, dim]),
                  dims=[1, 2, 0])                              # [kv_heads, dim, context]
        vh = g.op("permute", a=g.op("reshape", a=values, shape=[context, kv_heads, dim]),
                  dims=[1, 0, 2])                              # [kv_heads, context, dim]
        scores = g.op("mul", a=g.op("matmul", a=qg, b=kh), b=scale)
        scores = g.op("add", a=scores, b=mask)                 # [1, context] broadcasts
        attended = g.op("matmul", a=g.op("softmax", a=scores, axis=-1), b=vh)
        # [kv_heads, repeats, dim] is already head order, so this is a view.
        attended = g.op("reshape", a=attended, shape=[1, q_width])
        x = g.op("add", a=x, b=g.linear(attended, block + "attn_output.weight", q_width, hidden))

        n = g.rms_norm(x, block + "ffn_norm.weight", hidden, epsilon)
        gate = g.silu(g.linear(n, block + "ffn_gate.weight", hidden, intermediate))
        up = g.linear(n, block + "ffn_up.weight", hidden, intermediate)
        x = g.op("add", a=x, b=g.linear(g.op("mul", a=gate, b=up),
                                        block + "ffn_down.weight", intermediate, hidden))

    x = g.rms_norm(x, "output_norm.weight", hidden, epsilon)
    logits = g.linear(x, "token_embd.weight", hidden, config["vocab_size"])
    return g.finish_decode(logits, context)


def build_graph(config, tokens):
    description = describe(config)
    if not isinstance(tokens, list) or not 1 <= len(tokens) <= description["max_context"]:
        raise ValueError("Invalid context length")
    if any(type(token) is not int or not 0 <= token < config["vocab_size"] for token in tokens):
        raise ValueError("Invalid token id")

    length = len(tokens)
    hidden = config["hidden_size"]
    heads, kv_heads = config["num_heads"], config["num_kv_heads"]
    dim = config["head_dim"]
    repeats = heads // kv_heads
    epsilon = config["rms_norm_epsilon"]
    intermediate = config["intermediate_size"]
    # Qwen3 sizes its attention projections by head_dim * heads, which need not
    # equal hidden_size -- for the 0.6B it is 2048 against a hidden of 1024.
    q_width, kv_width = heads*dim, kv_heads*dim

    g = Graph()
    x = g.rows("token_embd.weight", tokens, hidden)
    mask = g.causal_mask(length)
    cos_data, sin_data = rope_tables(length, dim, float(config["rope_base"]))
    cos = g.literal("rope_cos", [length, 1, dim], cos_data)
    sin = g.literal("rope_sin", [length, 1, dim], sin_data)
    scale = g.scalar(1.0 / (dim ** 0.5))

    for layer in range(config["num_layers"]):
        block = "blk." + str(layer) + "."
        n = g.rms_norm(x, block + "attn_norm.weight", hidden, epsilon)
        q = g.op("reshape", a=g.linear(n, block + "attn_q.weight", hidden, q_width),
                 shape=[length, heads, dim])
        k = g.op("reshape", a=g.linear(n, block + "attn_k.weight", hidden, kv_width),
                 shape=[length, kv_heads, dim])
        v = g.op("reshape", a=g.linear(n, block + "attn_v.weight", hidden, kv_width),
                 shape=[length, kv_heads, dim])
        # Per-head RMSNorm before rotation. This is Qwen3's addition, and the
        # weights are head_dim wide rather than hidden wide.
        q = g.rms_norm(q, block + "attn_q_norm.weight", dim, epsilon)
        k = g.rms_norm(k, block + "attn_k_norm.weight", dim, epsilon)
        q = g.rope(q, length, heads, dim, cos, sin)
        k = g.rope(k, length, kv_heads, dim, cos, sin)
        k = g.repeat_heads(k, length, kv_heads, repeats, dim)
        v = g.repeat_heads(v, length, kv_heads, repeats, dim)

        qh = g.op("permute", a=q, dims=[1, 0, 2])            # [heads, length, dim]
        kh = g.op("permute", a=k, dims=[1, 2, 0])            # [heads, dim, length]
        vh = g.op("permute", a=v, dims=[1, 0, 2])            # [heads, length, dim]
        scores = g.op("mul", a=g.op("matmul", a=qh, b=kh), b=scale)
        scores = g.op("add", a=scores, b=mask)
        context = g.op("matmul", a=g.op("softmax", a=scores, axis=-1), b=vh)
        context = g.op("reshape", a=g.op("permute", a=context, dims=[1, 0, 2]),
                       shape=[length, q_width])
        x = g.op("add", a=x, b=g.linear(context, block + "attn_output.weight", q_width, hidden))

        n = g.rms_norm(x, block + "ffn_norm.weight", hidden, epsilon)
        gate = g.silu(g.linear(n, block + "ffn_gate.weight", hidden, intermediate))
        up = g.linear(n, block + "ffn_up.weight", hidden, intermediate)
        x = g.op("add", a=x, b=g.linear(g.op("mul", a=gate, b=up),
                                        block + "ffn_down.weight", intermediate, hidden))

    x = g.rms_norm(x, "output_norm.weight", hidden, epsilon)
    # Only the last row is sampled, and this vocabulary is 151,936 wide: taking
    # the row before the projection is the difference between logits of
    # [1, 151936] and [tokens, 151936].
    last = g.op("matmul", a=g.one_hot_row(length, length - 1), b=x)
    # Tied: the output projection is the token embedding read the other way
    # round, so the checkpoint carries no second copy of it.
    logits = g.linear(last, "token_embd.weight", hidden, config["vocab_size"])
    return g.finish(logits)
