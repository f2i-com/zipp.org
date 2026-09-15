# str() and repr() of values of every builtin type, containers holding them,
# recursive containers, string escapes in repr, and class/exception/type reprs.
from collections import OrderedDict, defaultdict, deque, Counter, namedtuple


class Custom:
    def __repr__(self):
        return "Custom()"


class StrOnly:
    def __str__(self):
        return "str-only"


Pt = namedtuple("Pt", "x y")
values = [
    0, -1, 10**25, True, False, None, 0.5, -1e100, float("nan"),
    "", "plain", "it's", 'say "hi"', "both ' and \"", "tab\tnew\nline\\", "é ü ß", "日本語", "😀",
    b"", b"bytes", b"\x00\xff'\"", bytearray(b"ba"),
    [], [1, [2, [3]]], (), (1,), (1, 2), {}, {"a": [1, {"b": None}]}, set(), {1}, frozenset(), frozenset({2}),
    range(0), range(1, 10, 2), slice(1, 2), slice(None), Ellipsis if False else "no-ellipsis",
    OrderedDict(a=1), defaultdict(list, {"k": [1]}), deque([1, 2], maxlen=3), Counter("aab"), Pt(1, 2),
    Custom(), [Custom(), Custom()], {"c": Custom()},
    int, str, list, type, object, ValueError,
    ValueError("msg"), KeyError("key"), KeyError(), StopIteration(5), Exception("a", 1),
]
for v in values:
    print("str:", str(v), "| repr:", repr(v))
print("control-repr", repr("\x00\x07\x1b\x7f"), ascii("\x00\x1f"), repr(["\r\n", "\t"]), repr("\\x41"))
print("str-only", str(StrOnly()), [str(StrOnly())], f"{StrOnly()}")
lst = [1, 2]
lst.append(lst)
d = {"self": None}
d["self"] = d
print("recursive", lst, d, repr(lst))
print("nested-strs", ["a'b", 'c"d', "e\nf"], ("\\",), {"k'": "v\""})
print("print-sep", 1, "two", 3.0, None, [4], sep=" ~ ")
print("print-end", "no newline", end=" <end>\n")
print("str-of-str", str("x"), repr(repr("x")), repr(str(5)), ascii("ñé中"), ascii(["é"]))
print("int-bases", str(0b1010), repr(0o777), str(0xDEAD), str(-0), str(1_000))
print("float-strs", str(1.0), str(1e16), str(1.5e-5), str(123456789.123456789), repr(2.0 ** 0.5))
print("bool-ops-str", str(1 < 2), str(not 1), repr(True and None), str([] or {}))
print("bytes-repr", repr(bytes(range(0, 256, 37))), repr(b"\\n"), str(b"a" * 3), repr(bytes(2)))
print("type-names", [t.__name__ for t in (int, bool, float, str, bytes, list, tuple, dict, set, frozenset, range, type(None))])
