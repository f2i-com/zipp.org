"""A table-based next-character model: proof the host is not transformer-specific.

One file carries the architecture and its tokenizer, so it needs no sibling
imports. The tokenizer below is the explicit character scheme the bundled
fixtures use: NOT GPT-2 BPE, SentencePiece, or a generic tokenizer.json reader.
Another architecture package supplies its own implementation of these hooks.
"""
# The checkpoint family this source implements, and the tokenizers it can read;
# `plugin.json` repeats both so a host can refuse a mismatched model before it
# compiles any Python. A transition table is not a transformer checkpoint, and
# neither format name is a claim about anyone else's checkpoints.
CHECKPOINT_FORMAT = "zipp.bigram-v1"
TOKENIZER_FORMATS = ["character-v1"]

def _validate(config):
    if set(config) != {"type", "vocab", "bos_token_id", "eos_token_id", "unk_token_id"}:
        raise ValueError("Unexpected character tokenizer fields")
    if config["type"] != "character-v1":
        raise ValueError("This plugin needs the character-v1 tokenizer")
    vocab = config["vocab"]
    if not isinstance(vocab, list) or not 4 <= len(vocab) <= 4096:
        raise ValueError("Invalid character vocabulary")
    if any(not isinstance(x, str) or not 1 <= len(x) <= 16 for x in vocab) or len(set(vocab)) != len(vocab):
        raise ValueError("Vocabulary entries must be unique bounded strings")
    special = [config["bos_token_id"], config["eos_token_id"], config["unk_token_id"]]
    if any(type(i) is not int or i < 0 or i >= len(vocab) for i in special) or len(set(special)) != 3:
        raise ValueError("Invalid special tokens")
    if any(len(vocab[i]) != 1 for i in range(len(vocab)) if i not in special):
        raise ValueError("Ordinary vocabulary entries must be single characters")
    return vocab, special

def encode(text, config):
    vocab, special = _validate(config)
    mapping = {value: i for i, value in enumerate(vocab) if i not in special}
    return [config["bos_token_id"]] + [mapping.get(ch, config["unk_token_id"]) for ch in text]

def decode(tokens, config):
    vocab, special = _validate(config)
    result = []
    for token in tokens:
        if type(token) is not int or not 0 <= token < len(vocab):
            raise ValueError("Invalid token id")
        if token == config["unk_token_id"]:
            result.append("\ufffd")
        elif token not in special:
            result.append(vocab[token])
    return "".join(result)


def describe(config):
    if set(config) != {"vocab_size", "context_length"}:
        raise ValueError("Unexpected bigram configuration")
    for key in ["vocab_size", "context_length"]:
        if type(config[key]) is not int or not 1 <= config[key] <= 512:
            raise ValueError("Invalid bigram configuration")
    return {"task": "causal-lm", "checkpoint_format": CHECKPOINT_FORMAT,
            "tokenizer_formats": list(TOKENIZER_FORMATS),
            "vocab_size": config["vocab_size"], "max_context": config["context_length"]}

def build_graph(config, tokens):
    describe(config)
    if not isinstance(tokens, list) or not 1 <= len(tokens) <= config["context_length"]:
        raise ValueError("Invalid token context")
    if any(type(t) is not int or not 0 <= t < config["vocab_size"] for t in tokens):
        raise ValueError("Invalid token id")
    return {"version": 1, "graph": {"version": 2,
        "nodes": [{"id": 0, "op": "input", "shape": [1, config["vocab_size"]]}],
        "outputs": [{"name": "logits", "id": 0}]},
        "bindings": [{"node": 0, "kind": "rows", "tensor": "transition_logits", "indices": [tokens[-1]]}]}
