"""Text processing: generate a corpus, tokenize, count words and bigrams, rank."""
import sys
import time

LINES = 1000
SYLLABLES = ["ka", "zu", "mi", "ro", "te", "shi", "no", "va", "len", "dor", "pe", "qua"]
PUNCT = [".", ",", ";", "!", "?", "", "", ""]


def lcg(state):
    return (state * 1103515245 + 12345) % 2147483648


def make_corpus(lines):
    state = 2026
    vocab = []
    for i in range(400):
        parts = []
        for j in range(1 + i % 3):
            state = lcg(state)
            parts.append(SYLLABLES[state // 65536 % len(SYLLABLES)])
        vocab.append("".join(parts))
    out = []
    for i in range(lines):
        words = []
        for j in range(12):
            state = lcg(state)
            # Squaring the draw skews the distribution toward the front of the vocabulary.
            r = state // 65536 % 400
            word = vocab[r * r // 400]
            if j == 0:
                word = word.capitalize()
            state = lcg(state)
            words.append(word + PUNCT[state // 65536 % len(PUNCT)])
        out.append(" ".join(words))
    return "\n".join(out)


def count(text):
    counts = {}
    bigrams = {}
    total = 0
    for line in text.split("\n"):
        prev = None
        for raw in line.lower().split():
            word = raw.strip(".,;!?")
            if not word:
                continue
            total += 1
            counts[word] = counts.get(word, 0) + 1
            if prev is not None:
                key = (prev, word)
                bigrams[key] = bigrams.get(key, 0) + 1
            prev = word
    return total, counts, bigrams


def bench(lines):
    text = make_corpus(lines)
    total, counts, bigrams = count(text)
    ranked = sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))
    top_bigram = sorted(bigrams.items(), key=lambda kv: (-kv[1], kv[0]))[0]
    return len(text), total, len(counts), len(bigrams), ranked[:5], top_bigram


t0 = time.perf_counter()
result = bench(LINES)
elapsed = time.perf_counter() - t0
print("word_count", LINES, result[0], result[1], result[2], result[3])
print("top", " ".join("%s=%d" % kv for kv in result[4]))
print("bigram", result[5][0][0], result[5][0][1], result[5][1])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
