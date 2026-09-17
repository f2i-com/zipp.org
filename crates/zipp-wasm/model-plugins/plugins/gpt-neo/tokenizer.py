"""GPT-2 byte-level BPE, reading the checkpoint's own vocabulary and merges.

The host reads those two files from the checkpoint folder, hashes them against
what it approved, and hands them over decomposed: the vocabulary as pairs, the
merge table as lines. They are far too large to embed in a manifest -- GPT-2's
vocabulary is about 800 KB and its merge table about 450 KB, against a 256 KB
manifest budget -- and far too large to parse here, because ZIPP's string
operations are regular-expression backed and scanning a megabyte inside the
guest is quadratic. Nothing is downloaded, and no tokenizer is guessed from a
model name: a checkpoint carries the tokenizer it was trained with or it cannot
be read at all.

The pre-tokenizer is written out by hand rather than as GPT-2's regular
expression, because ZIPP's `re` rejects the \p{L} and \p{N} property escapes
that expression is built from. `str.isalpha()` and `str.isnumeric()` are
Unicode-aware here and carry exactly the Unicode categories those two escapes
name, so the scanner below walks the same alternation in the same order. It is
checked against the reference tokenizer over a differential corpus in
tests/test_gpt_neo.py; agreeing on "byte-level BPE" would not be evidence.
"""
_ASSETS = {}
CONTRACTIONS = ["'s", "'t", "'re", "'ve", "'m", "'ll", "'d"]
_CACHE = {}


def _byte_encoder():
    """GPT-2's reversible byte to printable-character table."""
    printable = list(range(33, 127)) + list(range(161, 173)) + list(range(174, 256))
    table, extra = {}, 0
    for byte in range(256):
        if byte in printable:
            table[byte] = chr(byte)
        else:
            table[byte] = chr(256 + extra)
            extra += 1
    return table


def _validate(config):
    if set(config) != {"type", "eos_token_id", "bos_token_id", "assets"}:
        raise ValueError("Unexpected byte-BPE tokenizer fields")
    if config["type"] != "gpt2-byte-bpe-v1":
        raise ValueError("This plugin needs the gpt2-byte-bpe-v1 tokenizer")
    assets = config["assets"]
    if not isinstance(assets, dict) or set(assets) != {"vocab", "merges"}:
        raise ValueError("The host must supply vocab and merges assets")
    for name in ("vocab", "merges"):
        if not isinstance(assets[name], str) or not assets[name]:
            raise ValueError("Tokenizer asset paths must be strings the host mounted")
    for key in ("eos_token_id", "bos_token_id"):
        if type(config[key]) is not int or config[key] < 0:
            raise ValueError("Invalid special token id")
    return assets


def load_asset(name, form, values):
    """Receive one tokenizer asset from the host, already decomposed.

    The host reads, hashes and splits the file; this decides what the pieces
    mean. Parsing a megabyte of text inside ZIPP is quadratic -- its string
    operations are regular-expression backed -- so a 50,000-entry vocabulary
    arrives as pairs and a 50,000-line merge table as lines, and building the
    tables from those is ordinary dictionary work.
    """
    if name == "vocab":
        if form != "json-pairs":
            raise ValueError("The vocabulary must arrive as json-pairs")
        if len(values) % 2 != 0:
            raise ValueError("Vocabulary pairs are unbalanced")
        vocab = {}
        index = 0
        while index < len(values):
            token, identifier = values[index], values[index + 1]
            if not isinstance(token, str) or type(identifier) is not int or identifier < 0:
                raise ValueError("A vocabulary maps tokens to non-negative integers")
            if token in vocab:
                raise ValueError("Duplicate vocabulary token")
            vocab[token] = identifier
            index += 2
        if not vocab:
            raise ValueError("Empty vocabulary")
        _ASSETS["vocab"] = vocab
    elif name == "merges":
        if form != "lines":
            raise ValueError("The merge table must arrive as lines")
        ranks = {}
        for line in values:
            if not isinstance(line, str):
                raise ValueError("Merge lines must be text")
            if line and line[-1] == "\r":
                line = line[:-1]
            if not line or line[0] == "#":
                continue
            # Each line is short, so scanning one is cheap; it is the whole-file
            # scan that is not.
            space = line.find(" ")
            if space <= 0 or space == len(line) - 1 or line.find(" ", space + 1) >= 0:
                raise ValueError("A merge line is one pair separated by one space")
            pair = (line[:space], line[space + 1:])
            if pair not in ranks:
                ranks[pair] = len(ranks)
        if not ranks:
            raise ValueError("Empty merge table")
        _ASSETS["merges"] = ranks
    else:
        raise ValueError("This tokenizer has no asset called " + str(name))


def _load(config):
    assets = _validate(config)
    key = (assets["vocab"], assets["merges"])
    if key in _CACHE:
        return _CACHE[key]
    if "vocab" not in _ASSETS or "merges" not in _ASSETS:
        raise ValueError("The host has not supplied this checkpoint's tokenizer files")
    vocab, ranks = _ASSETS["vocab"], _ASSETS["merges"]
    encoder = _byte_encoder()
    loaded = {
        "vocab": vocab,
        "inverse": {value: token for token, value in vocab.items()},
        "ranks": ranks,
        "byte_encoder": encoder,
        "byte_decoder": {character: byte for byte, character in encoder.items()},
        "bpe": {},
    }
    _CACHE[key] = loaded
    return loaded


def _pieces(text):
    """GPT-2's pre-tokenizer alternation, in its original order.

    'contraction | ?letters | ?numbers | ?other | trailing-space-run | space-run'
    """
    out, i, size = [], 0, len(text)
    while i < size:
        character = text[i]
        if character == "'":
            match = None
            for candidate in CONTRACTIONS:
                if text.startswith(candidate, i):
                    # Longest wins: 're and 've are longer than 't's prefix.
                    if match is None or len(candidate) > len(match):
                        match = candidate
            if match is not None:
                out.append(match)
                i += len(match)
                continue
        start = i
        # An optional single leading space belongs to the following run.
        j = i + 1 if character == " " and i + 1 < size else i
        if j < size and not text[j].isspace():
            kind = "alpha" if text[j].isalpha() else "numeric" if text[j].isnumeric() else "other"
            k = j
            while k < size:
                nxt = text[k]
                if nxt.isspace():
                    break
                if kind == "alpha" and not nxt.isalpha():
                    break
                if kind == "numeric" and not nxt.isnumeric():
                    break
                if kind == "other" and (nxt.isalpha() or nxt.isnumeric()):
                    break
                k += 1
            if k > j:
                out.append(text[start:k])
                i = k
                continue
        # Whitespace run. `\s+(?!\S)` keeps the last space for the next piece
        # whenever a non-space follows, which is what gives ' word' its space.
        k = i
        while k < size and text[k].isspace():
            k += 1
        if k < size and k - i > 1:
            k -= 1
        out.append(text[i:k])
        i = k
    return out


def _merge(word, state):
    ranks, cache = state["ranks"], state["bpe"]
    if word in cache:
        return cache[word]
    tokens = list(word)
    while len(tokens) > 1:
        best, position = None, -1
        for index in range(len(tokens) - 1):
            rank = ranks.get((tokens[index], tokens[index + 1]))
            if rank is not None and (best is None or rank < best):
                best, position = rank, index
        if best is None:
            break
        first, second = tokens[position], tokens[position + 1]
        merged, index = [], 0
        while index < len(tokens):
            if index < len(tokens) - 1 and tokens[index] == first and tokens[index + 1] == second:
                merged.append(first + second)
                index += 2
            else:
                merged.append(tokens[index])
                index += 1
        tokens = merged
    cache[word] = tokens
    return tokens


def encode(text, config):
    state = _load(config)
    if not isinstance(text, str):
        raise ValueError("Expected text to encode")
    vocab, encoder = state["vocab"], state["byte_encoder"]
    ids = []
    for piece in _pieces(text):
        word = "".join(encoder[byte] for byte in piece.encode("utf-8"))
        for token in _merge(word, state):
            if token not in vocab:
                raise ValueError("Token is absent from this checkpoint's vocabulary")
            ids.append(vocab[token])
    return ids


def decode(tokens, config):
    state = _load(config)
    inverse, decoder = state["inverse"], state["byte_decoder"]
    characters = []
    for token in tokens:
        if type(token) is not int or token not in inverse:
            raise ValueError("Invalid token id")
        characters.extend(inverse[token])
    raw = bytes(decoder[character] for character in characters if character in decoder)
    if len(raw) != len(characters):
        raise ValueError("Vocabulary contains a character outside the byte alphabet")
    # A generated prefix can end mid-character, so an incomplete tail is
    # replaced rather than raising: the caller decodes the whole prefix again
    # on the next token, and the character completes itself.
    return raw.decode("utf-8", "replace")
