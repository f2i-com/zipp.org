"""GPT-Neo: the first externally sourced checkpoint family this host can read.

This is a separate plugin, not a configuration of the custom tiny-causal
fixture. Sharing attention, LayerNorm and an MLP with that fixture says nothing
about compatibility; what makes a checkpoint loadable is the set of decisions
below, each of which differs from the custom fixture's:

  * No 1/sqrt(head_dim) attention scaling. GPT-Neo omits it (transformers'
    GPTNeoSelfAttention matmuls q@k directly, and its flash path passes
    softmax_scale=1.0). Applying the usual scale changes every logit.
  * `gelu_new`, the tanh approximation, not ZIPP's erf `gelu` kernel.
  * q/k/v projections without bias; out_proj and both MLP projections with one.
  * Layers alternate global and windowed local attention.
  * Learned position embeddings added to the token embedding, no rotary.
  * The output projection is tied to the token embedding.
  * A GPT-2 byte-level BPE tokenizer, supplied as bounded host assets, rather
    than a character vocabulary that fits in a manifest.

The tensor names below are the checkpoint's own. Nothing is renamed and nothing
is rewritten on disk: a PyTorch [out, in] projection is read through a
transposing binding, and the tied output projection is the embedding read the
other way round. `native_manifest` reads the checkpoint's `config.json`
directly, so a Hugging Face folder needs no conversion to be loaded -- only a
repack out of pickle, which this host refuses to execute.
"""
import math
# Siblings are imported absolutely under the fixed package the host installs
# this source into. Relative imports also work now, but the absolute form says
# where the code will actually live, which is the useful thing here.
from zipp_plugin.graph import Graph
from zipp_plugin.tokenizer import encode, decode, load_asset

CHECKPOINT_FORMAT = "hf.gpt-neo-v1"
TOKENIZER_FORMATS = ["gpt2-byte-bpe-v1"]
# What this plugin needs from a checkpoint folder laid out the way the project
# that published it did: the config file it reads, and the tokenizer files that
# checkpoint was trained with. The host mounts exactly these and nothing else.
NATIVE_LAYOUT = {
    "config": "config.json",
    # `form` is how the host hands the file over, not what it contains: the
    # vocabulary as the pairs of a flat JSON object, the merge table as lines.
    "assets": {"vocab": {"path": "vocab.json", "form": "json-pairs"},
               "merges": {"path": "merges.txt", "form": "lines"}},
}

REQUIRED = {"vocab_size", "context_length", "hidden_size", "num_heads", "num_layers",
            "intermediate_size", "layer_norm_epsilon", "window_size", "attention_types",
            "activation"}
LIMITS = [("vocab_size", 2, 262144), ("context_length", 1, 2048), ("hidden_size", 1, 4096),
          ("num_heads", 1, 64), ("num_layers", 1, 48), ("intermediate_size", 1, 16384),
          ("window_size", 1, 2048)]
# Fields of a Hugging Face gpt_neo config that would change the arithmetic if
# they were not at these values. Reading a checkpoint that sets one of them
# differently would produce confident, wrong output, so it is refused instead.
NATIVE_DEFAULTS = {"attention_dropout": 0.0, "embed_dropout": 0.0, "resid_dropout": 0.0,
                   "classifier_dropout": 0.1, "use_cache": True, "scale_attn_weights": True}


def describe(config):
    if set(config) != REQUIRED:
        raise ValueError("Unsupported gpt-neo configuration fields")
    for key, low, high in LIMITS:
        if type(config[key]) is not int or not low <= config[key] <= high:
            raise ValueError("Invalid model dimension: " + key)
    if config["hidden_size"] % config["num_heads"] != 0:
        raise ValueError("hidden_size must be divisible by num_heads")
    epsilon = config["layer_norm_epsilon"]
    if not isinstance(epsilon, (int, float)) or not math.isfinite(epsilon) or not 0 < epsilon <= 1:
        raise ValueError("Invalid LayerNorm epsilon")
    # The activation is named, never guessed: a checkpoint trained with the tanh
    # approximation is not interchangeable with one trained on the erf form.
    if config["activation"] not in ("gelu_new", "gelu_pytorch_tanh"):
        raise ValueError("This plugin implements the tanh GELU approximation only, not "
                         + str(config["activation"]))
    kinds = config["attention_types"]
    if not isinstance(kinds, list) or len(kinds) != config["num_layers"]:
        raise ValueError("attention_types must name every layer")
    if any(kind not in ("global", "local") for kind in kinds):
        raise ValueError("Unsupported attention type")
    return {"task": "causal-lm", "checkpoint_format": CHECKPOINT_FORMAT,
            "tokenizer_formats": list(TOKENIZER_FORMATS),
            # This plugin also builds a cached decode graph, so a host that can
            # prepare one need not recompute the context for every token.
            "decode": True,
            "vocab_size": config["vocab_size"], "max_context": config["context_length"]}


def native_manifest(native, assets, limits):
    """Read a Hugging Face gpt_neo config.json into what the host needs.

    `native` is the checkpoint's own config, `assets` maps each tokenizer asset
    name to the path the host mounted it at, and `limits` carries the host's
    ceilings. Returning a configuration here is this plugin asserting it can
    read this checkpoint; a field it does not understand is an error, never
    something to ignore and hope for.
    """
    if not isinstance(native, dict):
        raise ValueError("Expected a checkpoint configuration object")
    if native.get("model_type") != "gpt_neo":
        raise ValueError("This plugin reads gpt_neo checkpoints, not "
                         + str(native.get("model_type")))
    architectures = native.get("architectures") or ["GPTNeoForCausalLM"]
    if "GPTNeoForCausalLM" not in architectures:
        raise ValueError("This plugin implements GPTNeoForCausalLM only")
    for key, expected in NATIVE_DEFAULTS.items():
        if key in native and native[key] != expected:
            raise ValueError("Unsupported " + key + ": this plugin implements only " + str(expected))
    # transformers writes the layer pattern as [[["global", "local"], 4]].
    kinds = []
    for entry in native["attention_types"]:
        pattern, repeats = entry
        if type(repeats) is not int or not 0 < repeats <= 64:
            raise ValueError("Invalid attention_types repeat count")
        for _ in range(repeats):
            kinds.extend(pattern)
    layers = native["num_layers"]
    if len(kinds) != layers:
        raise ValueError("attention_types does not describe every layer")
    context = native["max_position_embeddings"]
    # Every token is recomputed from the whole context here, so the checkpoint's
    # own maximum is usually far past what is usable. The host's ceiling wins,
    # and the difference is reported rather than silently clamped to a number
    # nobody chose.
    usable = min(context, limits["max_context"])
    config = {
        "vocab_size": native["vocab_size"],
        "context_length": usable,
        "hidden_size": native["hidden_size"],
        "num_heads": native["num_heads"],
        "num_layers": layers,
        "intermediate_size": native.get("intermediate_size") or 4*native["hidden_size"],
        "layer_norm_epsilon": native["layer_norm_epsilon"],
        "window_size": native["window_size"],
        "attention_types": kinds,
        "activation": native["activation_function"],
    }
    describe(config)
    tokenizer = {
        "type": TOKENIZER_FORMATS[0],
        "eos_token_id": native.get("eos_token_id", 50256),
        "bos_token_id": native.get("bos_token_id", 50256),
        "assets": assets,
    }
    return {"config": config, "tokenizer": tokenizer,
            "notes": ["checkpoint context " + str(context) + " limited to " + str(usable)]
                     if usable < context else []}


def build_decode_graph(config, context=None):
    """The same model as one cached step: read one token, attend over the cache.

    `build_graph` recomputes the whole context for every token and is the
    correctness oracle. This is the shape that makes a real checkpoint usable.
    The host prepares it once, so every weight is uploaded once; the key and
    value caches are carried on the device and never cross the boundary; and a
    token costs one position rather than the whole prompt.

    The arithmetic is identical to `build_graph` at the last position. What
    differs is only where the keys and values come from: the cache, with this
    token's written into it first.
    """
    description = describe(config)
    context = description["max_context"] if context is None else int(context)
    if not 1 <= context <= description["max_context"]:
        raise ValueError("Invalid decode context")
    hidden, heads = config["hidden_size"], config["num_heads"]
    dim, intermediate = hidden // heads, config["intermediate_size"]
    epsilon = config["layer_norm_epsilon"]
    g = Graph()
    x = g.op("add", a=g.step_rows("transformer.wte.weight", "token", hidden),
             b=g.step_rows("transformer.wpe.weight", "position", hidden))
    masks = {"global": g.step_mask(context)}
    if "local" in config["attention_types"]:
        masks["local"] = g.step_mask(context, config["window_size"])
    write = g.step_write(context)
    keep = g.op("sub", a=g.scalar(1.0), b=write)
    for layer in range(config["num_layers"]):
        block = "transformer.h." + str(layer)
        attn = block + ".attn.attention."
        n = g.layer_norm(x, block + ".ln_1.weight", block + ".ln_1.bias", hidden, epsilon)

        def head_view(projected):
            return g.op("permute", a=g.op("reshape", a=projected, shape=[1, heads, dim]), dims=[1, 0, 2])

        q = head_view(g.linear(n, attn + "q_proj.weight", hidden, hidden))
        k = head_view(g.linear(n, attn + "k_proj.weight", hidden, hidden))
        v = head_view(g.linear(n, attn + "v_proj.weight", hidden, hidden))
        keys = g.write_cache("k_cache_" + str(layer), g.cache("k_cache_" + str(layer), [heads, context, dim]),
                             keep, write, k)
        values = g.write_cache("v_cache_" + str(layer), g.cache("v_cache_" + str(layer), [heads, context, dim]),
                               keep, write, v)
        # No scaling, exactly as in the full-context path.
        scores = g.op("matmul", a=q, b=g.op("permute", a=keys, dims=[0, 2, 1]))
        scores = g.op("add", a=scores, b=masks[config["attention_types"][layer]])
        context_vectors = g.op("matmul", a=g.op("softmax", a=scores, axis=-1), b=values)
        merged = g.op("reshape", a=g.op("permute", a=context_vectors, dims=[1, 0, 2]), shape=[1, hidden])
        x = g.op("add", a=x, b=g.linear(merged, attn + "out_proj.weight", hidden, hidden,
                                        bias=attn + "out_proj.bias"))
        n = g.layer_norm(x, block + ".ln_2.weight", block + ".ln_2.bias", hidden, epsilon)
        ff = g.gelu_new(g.linear(n, block + ".mlp.c_fc.weight", hidden, intermediate,
                                 bias=block + ".mlp.c_fc.bias"))
        x = g.op("add", a=x, b=g.linear(ff, block + ".mlp.c_proj.weight", intermediate, hidden,
                                        bias=block + ".mlp.c_proj.bias"))
    x = g.layer_norm(x, "transformer.ln_f.weight", "transformer.ln_f.bias", hidden, epsilon)
    logits = g.linear(x, "transformer.wte.weight", hidden, config["vocab_size"])
    return g.finish_decode(logits, context)


def build_graph(config, tokens):
    description = describe(config)
    if not isinstance(tokens, list) or not 1 <= len(tokens) <= description["max_context"]:
        raise ValueError("Invalid context length")
    if any(type(token) is not int or not 0 <= token < config["vocab_size"] for token in tokens):
        raise ValueError("Invalid token id")
    length = len(tokens)
    hidden, heads = config["hidden_size"], config["num_heads"]
    dim, intermediate = hidden // heads, config["intermediate_size"]
    epsilon = config["layer_norm_epsilon"]
    g = Graph()
    x = g.op("add", a=g.rows("transformer.wte.weight", tokens, hidden),
             b=g.rows("transformer.wpe.weight", list(range(length)), hidden))
    masks = {"global": g.causal_mask(length)}
    if "local" in config["attention_types"]:
        # transformers builds the local mask as tril XOR tril(-window_size), so a
        # token sees the window_size positions ending at itself. The host's
        # `causal` binding masks col <= row - window, which is the same set.
        masks["local"] = g.causal_mask(length, config["window_size"])
    for layer in range(config["num_layers"]):
        block = "transformer.h." + str(layer)
        attn = block + ".attn.attention."
        n = g.layer_norm(x, block + ".ln_1.weight", block + ".ln_1.bias", hidden, epsilon)
        q = g.linear(n, attn + "q_proj.weight", hidden, hidden)
        k = g.linear(n, attn + "k_proj.weight", hidden, hidden)
        v = g.linear(n, attn + "v_proj.weight", hidden, hidden)
        q = g.op("permute", a=g.op("reshape", a=q, shape=[length, heads, dim]), dims=[1, 0, 2])
        k = g.op("permute", a=g.op("reshape", a=k, shape=[length, heads, dim]), dims=[1, 2, 0])
        v = g.op("permute", a=g.op("reshape", a=v, shape=[length, heads, dim]), dims=[1, 0, 2])
        # No scaling: GPT-Neo multiplies q by k and masks, nothing else.
        scores = g.op("add", a=g.op("matmul", a=q, b=k), b=masks[config["attention_types"][layer]])
        context = g.op("matmul", a=g.op("softmax", a=scores, axis=-1), b=v)
        context = g.op("reshape", a=g.op("permute", a=context, dims=[1, 0, 2]), shape=[length, hidden])
        x = g.op("add", a=x, b=g.linear(context, attn + "out_proj.weight", hidden, hidden,
                                        bias=attn + "out_proj.bias"))
        n = g.layer_norm(x, block + ".ln_2.weight", block + ".ln_2.bias", hidden, epsilon)
        ff = g.gelu_new(g.linear(n, block + ".mlp.c_fc.weight", hidden, intermediate,
                                 bias=block + ".mlp.c_fc.bias"))
        x = g.op("add", a=x, b=g.linear(ff, block + ".mlp.c_proj.weight", intermediate, hidden,
                                        bias=block + ".mlp.c_proj.bias"))
    x = g.layer_norm(x, "transformer.ln_f.weight", "transformer.ln_f.bias", hidden, epsilon)
    # Only the last row is ever sampled. Selecting it before the vocabulary
    # projection keeps the output at [1, vocab_size] instead of [tokens,
    # vocab_size], which a real vocabulary would push past the graph's element
    # limit long before the context limit was reached.
    last = g.op("matmul", a=g.one_hot_row(length, length - 1), b=x)
    # Tied embeddings: the output projection is the token embedding read the
    # other way round, so the checkpoint needs no second copy of it.
    logits = g.linear(last, "transformer.wte.weight", hidden, config["vocab_size"])
    return g.finish(logits)
