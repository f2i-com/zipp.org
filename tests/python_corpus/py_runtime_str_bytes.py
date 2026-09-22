# str/bytes/bytearray/isinstance details: startswith/endswith bounds, digit
# classes, bytes repr quoting, bytearray slices, nested isinstance tuples and
# split keywords.
print("abcd".startswith("bc", 1), "abc".endswith("ab", 0, 2), "abc".endswith("bc", 0, 2))
print("abcd".startswith("cd", -2), "abcd".endswith("ab", None, -2), "abcd".startswith("a", None))
print("abc".startswith("", 3), "abc".startswith("", 4), "abc".endswith("", 2, 1), "abc".startswith("", 1, 1))
print("abcd".startswith(("x", "bc"), 1), "abcd".endswith(("x", "c"), 0, 3), "abcd".startswith(("a", "b"), 2))
print("héllo".startswith("é", 1), "a😀b".startswith("b", 2), "a😀b".endswith("😀", 0, 2))
print([c.isdigit() for c in "7²³¹⁰⁴₉①⑴⒈⓪❶١१߀"])
print([c.isdecimal() for c in "7²①१"], [c.isnumeric() for c in "7²①१½Ⅻ一万零"])
print("²³".isdigit(), "12a".isdigit(), "".isdigit(), "一".isdigit())
print(repr(b"it's"), repr(b'say "hi"'), repr(b"both ' and \""), repr(b"plain"), repr(bytearray(b"it's")))
ba = bytearray(b"hello")
print(repr(ba[1:3]), type(ba[::-1]).__name__, ba[1], repr(ba[:0]))
print(isinstance(1, (str, (int, float))), isinstance("s", ((int,), (bytes, (str,)))), isinstance(1.5, (str, (int,))))
print(issubclass(bool, (str, (float, (int,)))), issubclass(str, ((int,),)))
print("a b c d".split(maxsplit=1), "a,b,c".split(sep=",", maxsplit=1), "a,b,c".split(",", maxsplit=1))
print("a b c d".rsplit(maxsplit=1), "a,b,c".rsplit(sep=","), "a b".split(sep=None))
