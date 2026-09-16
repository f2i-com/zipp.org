# Python orders strings by code point; JavaScript's default sort orders
# UTF-16 code units, which puts astral characters before U+E000-U+FFFF.
import json

d = {"z": 0, "！": 1, "\U0001F600": 2, "a": 3}
print(json.dumps(d, sort_keys=True))


class K:
    pass


names = ("ａ", "\U0001F600", "b")
for name in names:
    setattr(K, name, 1)
print([hex(ord(n)) for n in dir(K) if n in names])
print([hex(ord(n)) for n in sorted(names)])
print(hex(ord(min(names[:2]))), hex(ord(max(names[:2]))))
