"""Explicit character tokenizer used only by the bundled demonstration model.
A different architecture package can supply another implementation of these hooks.
This is NOT GPT-2 BPE, SentencePiece, or a generic tokenizer.json reader.
"""
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
