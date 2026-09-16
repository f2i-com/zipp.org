# str repr/ascii escaping of non-printable and astral code points, OSError
# errno mapping, Unicode case predicates, zfill/splitlines/titlecase, and
# the str.replace/find/count edge cases.

# repr() escapes non-printable code points (categories Cc, Cf, Cs, Co, Cn, Zl, Zp, and Zs other than space).
print("repr-nonprintable", repr("\u200b"), repr("a\xadb"), repr("\u2028"), repr("\ufeff"), repr("\U000e0001"), repr("\xa0"),
      repr("\x85"), repr("\u3000"), repr("\x7f"), repr("\x9f"), repr("é ü"), repr("日本"))
print("repr-noncharacter", ["\uffff"], repr("\ufffe"), ["\x80"], repr("\u0378"), repr("\U0010ffff"), repr("\ue000"))
print("repr-quotes", repr("it's"), repr('say "hi"'), repr("both ' and \""), repr("tab\there\n"), repr("back\\slash"))
print("printable", "\u200b".isprintable(), "abc".isprintable(), "\xa0".isprintable(), " ".isprintable(), "é".isprintable(), "\n".isprintable())

# ascii() escapes astral code points as \U0001xxxx, not as surrogate pairs.
print("ascii-astral", ascii("😀"), ascii("a\U0010ffffb"), ascii(["𝄞"]), ascii("é\u200b"), ascii({"k": "ñ"}), "%a" % "ü")

# OSError(errno, strerror) maps to the errno subclass and formats "[Errno N] msg".
for args in [(2, "nofile"), (13, "denied"), (17, "exists"), (99999, "other")]:
    e = OSError(*args)
    print("oserror", type(e).__name__, str(e), repr(e), e.errno, e.strerror, e.filename)
e = OSError(2, "No such file", "a.txt")
print("oserror-file", type(e).__name__, str(e), e.args, e.filename, isinstance(e, OSError))
print("oserror-plain", str(OSError("just text")), OSError().errno, repr(OSError("a", "b", "c", "d", "e", "f")))
try:
    raise FileNotFoundError(2, "missing")
except OSError as err:
    print("oserror-catch", type(err).__name__, err.errno, err)

# Case predicates on non-ASCII letters.
print("case-pred", "é".islower(), "É".isupper(), "日本".islower(), "ÉCOLE".isupper(), "ǅ".istitle(), "straße".islower(),
      "Σσ".isupper(), "ÀB".isupper(), "a1".islower(), "1".islower(), "Ǆ".isupper(), "Hello World".istitle(), "Hello world".istitle())
# zfill pads to a width in code points (an astral character is one).
print("zfill-astral", "😀".zfill(4), "-😀".zfill(4), len("😀".zfill(4)), "𝄞".rjust(3, "0"), "😀abc😀d".zfill(8), "+7".zfill(4), "".zfill(3))
# splitlines splits on every Unicode line boundary.
print("splitlines-unicode", "é\u2028f\u2029g\x85h\x0bi\x0cj\x1ck\x1dl\x1em".splitlines(), "a\r\nb\rc\n".splitlines(True), "x\u2028".splitlines(True))
# capitalize/title use titlecase mappings; ß titlecases to "Ss".
print("titlecase", "ǆemo".capitalize(), "ǆemo".title(), "ßx".title(), "ßx".capitalize(), "ŉ".title(), "hello wORLD".title(), "o'neil 3rd".title(), "éCOLE ÉTÉ".title())

# str.replace with an empty pattern, and find/index/count bounds.
print("replace-empty", "abc".replace("", "-"), "abc".replace("", "-", 2), "".replace("", "x"), "😀a".replace("", "."), "aaa".replace("a", "b", 2), "ab".replace("", "-", 0))
print("find-bounds", "abc".find("", 5), "abc".find("", 3), "abc".rfind("", 2), "abc".count("", 4), "abc".count(""), "abcabc".count("bc", 2), "abcabc".find("c", -2), "a😀b😀".find("b"), "a😀b😀".rfind("😀"))
try:
    "abc".index("", 4)
except ValueError as err:
    print("ValueError", err)
