# Plain pickle: loads CPython 3.11's pickles of the same data at protocols
# 0-5 (plain_p*.pkl, written by gen_expected.py) and round-trips this
# pickle's own output. Prints lines that must equal CPython's
# (pickle_expected.txt, from `python pickle_cases.py`).
import pickle


def show(value):
    if isinstance(value, dict):
        return "{" + ", ".join("%r: %s" % (k, show(value[k])) for k in sorted(value)) + "}"
    if isinstance(value, (set, frozenset)):
        return "%s(%s)" % (type(value).__name__, sorted(value))
    return repr(value)


for proto in range(6):
    with open("plain_p%d.pkl" % proto, "rb") as f:
        data = pickle.load(f)
    print(proto, show(data))
    print(proto, "types", sorted((k, type(v).__name__) for k, v in data.items()))

value = {"b": b"\x00\xff\x80", "empty": b"", "ba": bytearray(b"xy"), "s": {1, 2}, "fs": frozenset([3]), "big": -(2 ** 80), "f": -0.25,
         "t": ((), (1,), (1, 2), (1, 2, 3), (1, 2, 3, 4)), "shared": None}
shared = [1, 2]
value["shared"] = [shared, shared]
for label, proto in (("0", 0), ("2", 2), ("highest", pickle.HIGHEST_PROTOCOL)):
    out = pickle.dumps(value, protocol=proto)
    back = pickle.loads(out)
    print("round trip", label, show(back) == show(value), back["shared"][0] is back["shared"][1], type(back["ba"]).__name__)
print("bytearray", pickle.loads(pickle.dumps(bytearray(b"ab"))))
print("bytes", pickle.loads(pickle.dumps(b"\x00\x01\xfe")), pickle.loads(pickle.dumps(b"")))


class Target:
    def __init__(self):
        self.written = b""

    def write(self, data):
        self.written += data


t = Target()
pickle.dump([1, "two"], t, protocol=2)
print("dump", pickle.loads(t.written))

