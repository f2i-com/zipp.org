# json.dumps / json.loads over plain documents and their edge cases: float
# reprs, big ints, unicode and escapes, ensure_ascii, indent, separators,
# sort_keys, duplicate keys, and the documents the plain paths hand back to
# the general code (default=, hooks, errors, cycles, subclasses).
import json

floats = [0.0, -0.0, 1.0, -1.5, 0.1, 1 / 3, 2.5e-5, 1e-4, 0.0001234, 1e15, 1e16, 12345678901234567.0,
          123456789.125, 1e22, 1.7976931348623157e308, 5e-324, 2.2250738585072014e-308, 9007199254740993.0,
          600479950316068.25, 0.3, 100.0, 1e-7, 123e-20]
print(json.dumps(floats))
print(json.loads(json.dumps(floats)) == floats)
print(json.dumps([float("nan"), float("inf"), -float("inf")]))
try:
    json.dumps([1.0, float("inf")], allow_nan=False)
except ValueError as e:
    print("ValueError:", e)
ints = [0, -1, 7, 2 ** 53, 2 ** 63, -(2 ** 64), 10 ** 30, 10 ** 38 - 1, 10 ** 40, -(10 ** 45) + 3]
print(json.dumps(ints))
print(json.loads(json.dumps(ints)) == ints, [type(x).__name__ for x in json.loads("[1, 1.0, 1e2, -0, -0.0, 10000000000000000000000000000000000000000]")])
print(json.loads("[1E400, -1e400, 0.5e-400, -0, -0.0, 1.25E+2, 3e-2]"))

texts = ["plain", "", "quote\"back\\slash/", "tab\tnl\ncr\rbs\bff\f", "\x00\x01\x1f\x7f", "caf\u00e9", "\u2028\u2029",
         "emoji \U0001F600 x", "\u00ff\u0100\uffff", "mixed \u00e9\U0001F680\x7f"]
for t in texts:
    print(json.dumps(t), json.dumps(t, ensure_ascii=False).encode("utf-8", "surrogatepass"))
    print(json.loads(json.dumps(t)) == t, json.loads(json.dumps(t, ensure_ascii=False)) == t)
for t in [chr(0xd800) + " lone", "lone " + chr(0xdfff)]:
    print(json.dumps(t), json.loads(json.dumps(t)) == t)
print(json.dumps({"k\u00e9y": ["v\U0001F600"]}), json.dumps({"k\u00e9y": ["v\U0001F600"]}, ensure_ascii=False))

doc = {"b": [1, 2, {"z": None, "a": True}], "a": (False, "x"), "c": {}, "d": [], "e": {"nested": {"deep": [[], [{}]]}}, "\u00e9": 1, "Z": 2, "\U0001F600": 3, "\uffff": 4}
print(json.dumps(doc))
print(json.dumps(doc, sort_keys=True))
print(json.dumps(doc, indent=2, sort_keys=True))
print(json.dumps(doc, indent=0))
print(json.dumps(doc, indent="\t", separators=(",", ": ")))
print(json.dumps(doc, separators=(",", ":")))
print(json.dumps(doc, indent=-3))
print(json.dumps([], indent=4), json.dumps({}, indent=4), json.dumps([[]], indent=1), json.dumps(("t",), indent=1))
print(json.dumps("top"), json.dumps(5), json.dumps(None), json.dumps(True), json.dumps(2.5))

shared = [1, 2]
print(json.dumps({"x": shared, "y": shared, "z": [shared, shared]}))
cyc = [1]
cyc.append(cyc)
try:
    json.dumps(cyc)
except ValueError as e:
    print("ValueError:", e)
d = {}
d["self"] = d
try:
    json.dumps(d)
except ValueError as e:
    print("ValueError:", e)

print(json.dumps({1: "int", 2.5: "float", True: "bool", None: "none", "s": "str"}))
try:
    json.dumps({1: "a", "b": 2}, sort_keys=True)
except TypeError as e:
    print("TypeError:", e)
print(json.dumps({3: "c", 1: "a", 2: "b"}, sort_keys=True))
try:
    json.dumps({(1, 2): "tuple key"})
except TypeError as e:
    print("TypeError:", e)
print(json.dumps({(1, 2): "tuple key", "ok": 1}, skipkeys=True))


class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y


try:
    json.dumps([Point(1, 2)])
except TypeError as e:
    print("TypeError:", e)
print(json.dumps([Point(1, 2), {"p": Point(3, 4)}], default=lambda o: {"x": o.x, "y": o.y}))
print(json.dumps({"s": {1, 2}}, default=sorted))


class MyList(list):
    pass


class MyDict(dict):
    pass


print(json.dumps(MyList([1, MyDict(a=2)])), json.dumps(MyDict(b=[MyList()])))

print(json.loads('  {"a" : 1 , "b":[true,false,null] ,"c":{"d":"e"}}  '))
print(json.loads('{"dup": 1, "other": 2, "dup": 3}'), list(json.loads('{"dup": 1, "other": 2, "dup": 3}')))
print(json.loads('"\\u00e9\\ud83d\\ude00\\n\\t\\"\\\\\\/"') == "\u00e9\U0001F600\n\t\"\\/")
lone = json.loads('["\\ud800", "\\udc00x", "\\ud83d"]')
print([hex(ord(s[0])) for s in lone], lone[1][1:])
print(json.loads('[]'), json.loads('{}'), json.loads('[[[]]]'), json.loads('{"a":{}}'), json.loads(' 12 '), json.loads('"x"'))
big = {"k%d" % i: [i, str(i), i * 0.5] for i in range(60)}
print(json.loads(json.dumps(big)) == big, list(json.loads(json.dumps(big)))[:5])
many_dups = "{" + ", ".join('"k%d": %d' % (i % 20, i) for i in range(100)) + "}"
print(json.loads(many_dups))
print(json.loads("[NaN, Infinity, -Infinity]"))
print(json.loads('{"a": 1.5}', parse_float=str), json.loads('[1, 2]', parse_int=float))
print(json.loads('{"a": {"b": 1}}', object_hook=lambda d: sorted(d.items())))
print(json.loads('{"a": 1, "b": 2}', object_pairs_hook=list))
print(json.loads('[NaN]', parse_constant=lambda s: "const:" + s))

bad = ['', ' ', '{"a" 1}', '{1: 2}', '[1 2]', '"unterminated', 'tru', 'nul', '[1] x', '01', '1.', '-',
       '.5', '1e', '{"a":}', '[', '{', '"\\x"', '"\\u12"', '"\\u12G4"', '[-Infinity, -]']
for text in bad:
    try:
        print(repr(text), "->", json.loads(text))
    except json.JSONDecodeError as e:
        print(repr(text), "JSONDecodeError:", e)
    except ValueError as e:
        print(repr(text), "ValueError:", e)

records = []
for i in range(50):
    records.append({"id": i, "name": "user%d" % i, "score": i * 0.25 + 0.125, "tags": ["t%d" % (i % 7)], "ok": i % 2 == 0, "none": None})
text = json.dumps(records, sort_keys=True)
back = json.loads(text)
print(back == records, len(text), json.dumps(back) == json.dumps(records))
print(json.dumps(back[3], indent=2, sort_keys=True))
back[0]["name"] = "changed"
back[1]["tags"].append("more")
back.append({"new": 1})
print(back[0], back[1], len(back), back[-1])
