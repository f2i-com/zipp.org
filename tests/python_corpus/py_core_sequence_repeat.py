# Sequence repetition: bytearray keeps its type from either side and in
# place, and only a non-int operand gets "can't multiply sequence by non-int".
cases = [
    lambda: bytearray(b"x") * 3,
    lambda: 3 * bytearray(b"ab"),
    lambda: bytearray(b"q") * 0,
    lambda: bytearray(b"q") * -2,
    lambda: bytearray(b"ab") * True,
    lambda: b"x" * 3,
    lambda: [1] * [2],
    lambda: "a" * 2.0,
    lambda: (1,) * "x",
    lambda: b"a" * b"b",
    lambda: bytearray(b"a") * 2.5,
    lambda: 2.5 * bytearray(b"a"),
    lambda: [1] * True,
    lambda: "ab" * 2,
    lambda: None * [1],
]
for i, f in enumerate(cases):
    try:
        r = f()
        print(i, type(r).__name__, r)
    except TypeError as e:
        print(i, "TypeError", e)
b = bytearray(b"ab")
alias = b
b *= 3
print(b, alias is b, len(b))
try:
    b *= "x"
except TypeError as e:
    print("TypeError", e)
