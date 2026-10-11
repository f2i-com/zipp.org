# Python's whitespace is what str.isspace() says: it has the separators
# U+001C..U+001F and U+0085 (NEL), and it does not have U+FEFF. strip(),
# split() and isspace() all use that one set, on every path: a plain str, a
# str with characters beyond ASCII, a str holding a lone surrogate, a str
# subclass, an explicit None, a call through the type, and a bound method.
# Everything is printed through ascii() so the expected output stays ASCII,
# and every character beyond Latin-1 is written by its code point.

WS = [chr(c) for c in (0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x85, 0xA0, 0x1680,
                       0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200A,
                       0x2028, 0x2029, 0x202F, 0x205F, 0x3000)]
NOT_WS = [chr(c) for c in (0x00, 0x1B, 0x21, 0x7F, 0x84, 0x86, 0x180E, 0x200B, 0x200C, 0x200D, 0x2060, 0xFEFF, 0x61)]
BOM, EM, IDEO, LSEP = chr(0xFEFF), chr(0x2003), chr(0x3000), chr(0x2028)
FACE, GRIN = chr(0x1F600), chr(0x1F601)
HI, LO = chr(0xD800), chr(0xDE00)


class Text(str):
    pass


def show(label, value):
    print(label, ascii(value))


print("== every whitespace character, one at a time")
for ch in WS:
    s = ch + "x" + ch
    print(ascii(ch), ch.isspace(), ascii(s.strip()), ascii(s.lstrip()), ascii(s.rstrip()), ascii(s.split()),
          ascii(("a" + ch + "b").split()), ascii(s.strip(None)), ascii((ch * 3).strip()), (ch * 3).split())

print("== characters that are not whitespace")
for ch in NOT_WS:
    s = ch + "x" + ch
    print(ascii(ch), ch.isspace(), ascii(s.strip()), ascii(s.lstrip()), ascii(s.rstrip()), ascii(s.split()),
          ascii(("a" + ch + "b").split()), ascii(s.strip(None)))

print("== ASCII receivers")
plain = "\x1c\t  Alpha beta\x1dGAMMA \x1e\x1f\n"
show("strip", plain.strip())
show("lstrip", plain.lstrip())
show("rstrip", plain.rstrip())
show("split", plain.split())
show("unchanged", ("abc".strip(), "abc".lstrip(), "abc".rstrip()))
show("all white", " \t\x1c\x1f ".strip())
show("all white l", " \t\x1c\x1f ".lstrip())
show("all white r", " \t\x1c\x1f ".rstrip())
show("empty", ("".strip(), "".lstrip(), "".rstrip()))
show("empty split", "".split())
show("white split", " \x1c\x1d ".split())
show("inner kept", "a \x1c b".strip())
show("one side", "\x1fabc".rstrip())
show("other side", "abc\x1f".lstrip())
show("one char", (" ".strip(), "\x1c".strip(), "a".strip(), " a".strip(), "a ".strip()))

print("== receivers beyond ASCII")
wide = "\x85" + IDEO + "\xa0caf\xe9 " + LSEP + "na\xefve" + BOM + EM + "\x85"
show("strip", wide.strip())
show("lstrip", wide.lstrip())
show("rstrip", wide.rstrip())
show("split", wide.split())
show("bom kept", (BOM + "x" + BOM).strip())
show("bom split", ("a" + BOM + "b c").split())
show("nel", "\x85x\x85".strip())
show("nel split", " a\x85b ".split())
show("astral", (EM + FACE + " " + GRIN + EM).strip())
show("astral split", (EM + FACE + "\x1c" + GRIN + EM).split())
show("unchanged", ("caf\xe9".strip(), "caf\xe9".lstrip(), "caf\xe9".rstrip()))
show("all white", ((IDEO + "\x85" + EM).strip(), (IDEO + "\x85" + EM).split()))

print("== a lone surrogate in the receiver")
lone = "\x1c " + HI + " x\x85\x1f"
show("strip", lone.strip())
show("lstrip", lone.lstrip())
show("rstrip", lone.rstrip())
show("split", lone.split())
show("isspace", (HI.isspace(), (" " + HI).isspace()))

print("== explicit None, and character sets")
show("strip(None)", "\x1c a \x1f".strip(None))
show("lstrip(None)", "\x1c a \x1f".lstrip(None))
show("rstrip(None)", "\x1c a \x1f".rstrip(None))
show("strip(chars)", "\x1c a \x1f".strip(" "))
show("strip(sep)", "\x1c a \x1f".strip("\x1c\x1f"))
show("lstrip(chars)", "xxabcxx".lstrip("x"))
show("rstrip(chars)", "xxabcxx".rstrip("x"))
show("strip(wide)", "\xe9\xe9abc\xe9".strip("\xe9"))

print("== split with a count, and rsplit")
text = "\x1ca\x1db \x85 c\x1ed\x1f"
for n in (-1, 0, 1, 2, 3, 4, 5):
    print(n, ascii(text.split(None, n)), ascii(text.rsplit(None, n)), ascii(text.split(maxsplit=n)), ascii(text.rsplit(maxsplit=n)))
show("rsplit", "\x1ca b\x1f".rsplit())
show("rsplit bom", ("a" + BOM + "b c").rsplit())
show("split sep", "a\x1cb\x1cc".split("\x1c"))
show("split space", "a\x1cb c".split(" "))
show("bom count", (BOM + " a " + BOM + " b").split(None, 1))
show("bom rcount", (BOM + " a " + BOM + " b").rsplit(None, 1))
# With a count, what is left over is kept as it is: split() keeps the
# whitespace that ends it, rsplit() the whitespace that starts it.
for s in (" a b ", "a b", " a ", "a  ", "  a", "   ", "", "  a  b  c  ", "a b c", "\x1c a\x85b \x1f", Text(" a b ")):
    print(ascii(s), ascii([s.split(None, n) for n in (0, 1, 2, 3)]), ascii([s.rsplit(None, n) for n in (0, 1, 2, 3)]))

print("== isspace")
show("mixed", "\x1c\x1d\x1e\x1f\x85 \t".isspace())
show("with bom", (" " + BOM).isspace())
show("empty", "".isspace())
show("letters", " a ".isspace())
show("all", "".join(WS).isspace())

print("== a str subclass, the type's method, a bound method")
sub = Text("\x1c\x85 sub \x1f")
show("strip", sub.strip())
show("lstrip", sub.lstrip())
show("rstrip", sub.rstrip())
show("split", sub.split())
show("isspace", Text("\x1c\x85").isspace())
show("type", type(sub.strip()).__name__)
show("str.strip", str.strip("\x1c a \x1f"))
show("str.lstrip", str.lstrip("\x1c a \x1f"))
show("str.rstrip", str.rstrip("\x1c a \x1f"))
show("str.split", str.split("\x1ca\x1fb"))
bound = "\x1c bound \x1f".strip
show("bound", bound())
show("map", list(map(str.strip, ["\x1ca\x1f", " b ", "\x85c\x85"])))
strip = "strip"
show("getattr", getattr("\x1e dyn \x1e", strip)())

print("== startswith and endswith")
s = "Alpha,beta"
show("starts", (s.startswith("Al"), s.startswith("al"), s.startswith(""), s.startswith(s), s.startswith(s + "x")))
show("ends", (s.endswith("ta"), s.endswith("Ta"), s.endswith(""), s.endswith(s), s.endswith("x" + s)))
show("tuple", (s.startswith(("x", "Al")), s.startswith(("x", "y")), s.startswith(()), s.endswith(("ta", "x")), s.endswith(())))
show("bounds", (s.startswith("ph", 2), s.startswith("ph", 3), s.startswith("Al", 0, 1), s.startswith("", 99), s.endswith("be", 0, 8), s.endswith("ta", -2), s.endswith("ta", -1)))
show("none bounds", (s.startswith("Al", None), s.startswith("Al", None, None), s.endswith("ta", None, None)))
show("empty", ("".startswith(""), "".endswith(""), "".startswith("a"), "".endswith("a")))
w = "caf\xe9" + FACE + "x"
show("wide", (w.startswith("caf\xe9"), w.startswith("caf\xe8"), w.endswith(FACE + "x"), w.endswith("\xe9" + FACE + "x"), w.startswith("\xe9", 3), w.endswith(FACE, 0, 5)))
show("wide in ascii", ("cafe".startswith("caf\xe9"), "cafe".endswith("\xe9"), "caf\xe9".startswith("caf"), "caf\xe9".endswith("f\xe9")))
show("lone", ((HI + "x").startswith(HI), ("x" + HI).endswith(HI), (HI + "x").startswith("x"), ("x" + LO).endswith("x" + LO)))
show("subclass", (Text("abc").startswith("ab"), Text("abc").endswith(Text("bc")), "abc".startswith(Text("ab"))))
show("str.startswith", (str.startswith("abc", "a"), str.endswith("abc", "c")))
for bad in (1, None, b"a", ["a"]):
    for name in ("startswith", "endswith"):
        try:
            print(name, ascii(getattr("abc", name)(bad)))
        except TypeError:
            print(name, "TypeError")
        try:
            if name == "startswith":
                print(name, "abc".startswith(bad))
            else:
                print(name, "abc".endswith(bad))
        except TypeError:
            print(name, "TypeError")


def hot(lines):
    # The same sites, run often enough to be compiled.
    total = 0
    for line in lines:
        t = line.strip()
        if t.startswith("k") and t.endswith("v"):
            total += len(t)
        total += len(line.lstrip()) + len(line.rstrip()) + len(line.split())
    return total


lines = ["\x1c key=v \x1f", " k\x85v ", BOM + "kv" + BOM, "plain", "", "  ", "\x1e\x1e", "k v", " kv" + HI + " ", Text(" kv "),
         IDEO + "k" + EM + "v\x85"]
print("hot", [hot(lines) for _ in range(5)], hot(lines * 400))

print("== the library functions that split as str.split() does")
import string
import textwrap

for text in ("  hello  world  ", "a\x1cb \x85 c", "hello world", " x ", "", BOM + "one two", "one two three four five six seven"):
    print(ascii(string.capwords(text)), ascii(textwrap.shorten(text, 20)), ascii(textwrap.shorten(text, 12)))
