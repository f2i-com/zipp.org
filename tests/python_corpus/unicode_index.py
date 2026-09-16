# len/index/slice/iterate/search over non-ASCII and astral (UTF-16 surrogate-pair) text:
# every result is in code points, which an O(1) length or index fast path must preserve.
samples = ["", "ascii", "héllo", "naïve café", "日本語テキスト", "😀", "a😀b", "😀😃😄", "x🎉y🎊z", "𝄞 music", "e\u0301", "🇳🇴 flag", "mixé😀日"]
for s in samples:
    print("len", repr(s), len(s), [ord(c) for c in s][:6], list(s)[:4])

s = "a😀bé日𝄞c"
print("index", [s[i] for i in range(len(s))], [s[-i] for i in range(1, len(s) + 1)])
print("slice", s[1:3], s[2:], s[:-2], s[::2], s[::-1], s[1:-1:2], s[-3:], s[5:100], s[100:], repr(s[3:3]))
print("find", s.find("b"), s.find("日"), s.index("𝄞"), s.rfind("c"), s.find("😀b"), s.count("😀"), "😀" in s, "😃" in s)
print("split", "a😀b😀c".split("😀"), "日本 語".split(), "x🎉y".partition("🎉"), "😀,😃".replace(",", "|"))
print("methods", "héllo".upper(), "ÉCOLE".lower(), "straße".upper(), "😀abc".upper(), "日本".center(6, "*"), "é".ljust(3, "·") + "|", "😀".rjust(3, "-"))
print("strip", "  😀 ".strip(), "😀😀x😀".strip("😀"), "ééxé".lstrip("é"), "a😀".endswith("😀"), "😀a".startswith("😀"))
print("enumerate", list(enumerate("é😀")), [(i, c) for i, c in enumerate("a𝄞b") if ord(c) > 127])
print("reversed", "".join(reversed("a😀b")), list(reversed("日本")))
print("ord-chr", ord("😀"), chr(128512), chr(0x10FFFF) == "\U0010ffff", len(chr(0x10FFFF)), ord("\U0001d11e"), hex(ord("é")))
print("encode", "é😀".encode("utf-8"), len("é😀".encode("utf-8")), "é😀".encode("utf-8").decode("utf-8") == "é😀", "日".encode("utf-16-le") if False else "skip")
print("compare", "é" < "😀", "😀" < "\uffff", "\uffff" < "𐀀", sorted(["😀", "z", "é", "\ufb01", "𐀀", "日"]), max("a😀b"), min("😀é"))
print("isX", "é".isalpha(), "😀".isalpha(), "日".isalpha(), "٣".isdigit(), "Ⅻ".isnumeric(), " ".isspace(), "\u3000".isspace())
text = "😀" * 50 + "end" + "é" * 50
print("long", len(text), text[50:53], text[49], text[53], text[-1], text.index("end"), text[::17])
total = 0
for i in range(len(text)):
    if text[i] == "e":
        total += i
print("loop-index", total)
count = 0
for ch in text:
    if ord(ch) > 0xFFFF:
        count += 1
print("loop-iter", count)
chunks = [text[i:i + 13] for i in range(0, len(text), 13)]
print("chunks", len(chunks), [len(c) for c in chunks], chunks[3])
built = ""
for i in range(20):
    built += "😀" if i % 3 == 0 else "é"
print("concat", len(built), built[::5], built.count("😀"))
print("fstring-width", f"[{'😀':>4}]", f"[{'日本':<5}]", f"[{'é':^5}]", "{:*^7}".format("a😀"))
print("join-len", len("".join(["😀", "é", "x"])), len("-".join("😀😀")), "😀".join(["a", "b"]))
print("mult", "😀é" * 3, len("😀é" * 3), ("😀" * 4)[1:3])
print("in-list", "😀" in ["😀"], {"😀": 1}["😀"], {"é": 2}.get("e\u0301", "decomposed-differs"))
print("zfill-expand", "é".zfill(4), "a\t😀".expandtabs(4), len("a\t😀".expandtabs(4)))
words = "naïve 😀 café 日本 x".split()
print("words", [(w, len(w)) for w in words], max(words, key=len), sorted(words, key=len))
print("splitlines", "a😀\nb\r\nc".splitlines(), "é\rf".splitlines())
print("title", "héllo wörld".title(), "émile".capitalize(), "😀a b".title())
print("casefold", "Straße".casefold() == "strasse", "ÉÈ".casefold())
print("bytes-index", "é😀".encode()[1], list("é😀".encode()), bytes("é", "utf-8")[::-1])
