# Small text-processing workloads: tokenizing, counting, run-length coding, a tiny
# expression evaluator, string building with f-strings, and a JSON-like pretty printer.
TEXT = """It was the best of times, it was the worst of times, it was the age of wisdom,
it was the age of foolishness, it was the epoch of belief, it was the epoch of incredulity,
it was the season of Light, it was the season of Darkness, it was the spring of hope."""

words = [w.strip(",.").lower() for w in TEXT.split()]
freq = {}
for w in words:
    freq[w] = freq.get(w, 0) + 1
top = sorted(freq.items(), key=lambda kv: (-kv[1], kv[0]))[:6]
print("freq", len(words), len(freq), top)
print("lengths", {n: sorted({w for w in words if len(w) == n}) for n in range(5, 8)})
lines = TEXT.splitlines()
print("lines", [len(ln) for ln in lines], [ln.count("it was") for ln in lines], max(lines, key=len)[:20])


def rle(s):
    out = []
    i = 0
    while i < len(s):
        j = i
        while j < len(s) and s[j] == s[i]:
            j += 1
        out.append(f"{j - i}{s[i]}")
        i = j
    return "".join(out)


def unrle(s):
    out, num = [], ""
    for ch in s:
        if ch.isdigit():
            num += ch
        else:
            out.append(ch * int(num))
            num = ""
    return "".join(out)


samples = ["aaabccddddde", "abc", "", "zzzzzzzzzzzz", "aabbaa"]
print("rle", [rle(s) for s in samples], all(unrle(rle(s)) == s for s in samples))


def caesar(s, k):
    res = []
    for ch in s:
        if "a" <= ch <= "z":
            res.append(chr((ord(ch) - 97 + k) % 26 + 97))
        elif "A" <= ch <= "Z":
            res.append(chr((ord(ch) - 65 + k) % 26 + 65))
        else:
            res.append(ch)
    return "".join(res)


enc = caesar(lines[0], 13)
print("caesar", enc[:30], caesar(enc, 13) == lines[0], caesar("xyz ABC", -3))


def tokenize(expr):
    tokens, num = [], ""
    for ch in expr:
        if ch.isdigit() or ch == ".":
            num += ch
            continue
        if num:
            tokens.append(float(num) if "." in num else int(num))
            num = ""
        if ch in "+-*/()":
            tokens.append(ch)
    if num:
        tokens.append(float(num) if "." in num else int(num))
    return tokens


def evaluate(tokens):
    pos = 0

    def peek():
        return tokens[pos] if pos < len(tokens) else None

    def take():
        nonlocal pos
        tok = tokens[pos]
        pos += 1
        return tok

    def factor():
        tok = take()
        if tok == "(":
            v = expr()
            take()
            return v
        if tok == "-":
            return -factor()
        return tok

    def term():
        v = factor()
        while peek() in ("*", "/"):
            op = take()
            rhs = factor()
            v = v * rhs if op == "*" else v / rhs
        return v

    def expr():
        v = term()
        while peek() in ("+", "-"):
            op = take()
            rhs = term()
            v = v + rhs if op == "+" else v - rhs
        return v

    return expr()


for e in ["1 + 2 * 3", "(1 + 2) * 3", "10 / 4 - 1", "2 * (3 + 4) * -1", "1.5 * 4 + 0.25", "((7))", "100 - 99 - 1"]:
    print("eval", e, "=", evaluate(tokenize(e)))


def pretty(value, indent=0):
    pad = "  " * indent
    if isinstance(value, dict):
        if not value:
            return "{}"
        inner = ",\n".join(f"{pad}  {k!r}: {pretty(v, indent + 1)}" for k, v in value.items())
        return "{\n" + inner + "\n" + pad + "}"
    if isinstance(value, list):
        return "[" + ", ".join(pretty(v, indent) for v in value) + "]"
    if isinstance(value, str):
        return '"' + value.replace('"', '\\"') + '"'
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    return repr(value)


doc = {"name": "zipp", "tags": ["py", "js"], "meta": {"stars": 42, "ratio": 0.75, "ok": True, "none": None, "empty": {}}, "quote": 'say "hi"'}
print(pretty(doc))
table = [("alpha", 1, 0.5), ("beta", 22, 12.25), ("gamma", 333, -3.125)]
widths = [max(len(str(row[i])) for row in table) for i in range(3)]
for name, n, x in table:
    print(f"| {name:<{widths[0]}} | {n:>{widths[1]}} | {x:>{widths[2] + 2}.3f} |")
acrostic = "".join(ln.split()[1][0] for ln in lines)
print("acrostic", acrostic, "-".join(w[::-1] for w in words[:5]), " ".join(w.capitalize() for w in words[-4:]))
vowels = sum(1 for ch in TEXT.lower() if ch in "aeiou")
print("vowels", vowels, round(vowels / len(TEXT), 4), sorted(set(TEXT.lower()) - set("abcdefghijklmnopqrstuvwxyz")))
builder = []
for i, w in enumerate(words[:12]):
    builder.append(f"{i}:{w[:3]}")
print("builder", ",".join(builder), len(",".join(builder)))
anagrams = {}
for w in ["listen", "silent", "enlist", "google", "gooegl", "cat", "act", "tac", "dog"]:
    anagrams.setdefault("".join(sorted(w)), []).append(w)
print("anagrams", sorted(v for v in anagrams.values() if len(v) > 1))
