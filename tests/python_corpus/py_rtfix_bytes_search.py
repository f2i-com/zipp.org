# bytes find/rfind/index/rindex/count honour negative and clamped bounds,
# and join/concatenation of many parts.
h = b"abcabcXYZabc"
for needle in [b"abc", b"bc", b"Z", b"", b"zz", 97, 99]:
    print(needle, h.find(needle), h.rfind(needle), h.count(needle) if not isinstance(needle, int) else "-")
for start, end in [(0, None), (1, None), (-3, None), (-100, None), (2, 7), (2, -2), (-5, -1), (12, None), (13, None), (5, 2)]:
    print(start, end, h.find(b"abc", start, end), h.rfind(b"abc", start, end), h.find(b"", start, end), h.rfind(b"", start, end), h.count(b"a", start, end), h.count(b"", start, end))
print(b"aaaa".count(b"aa"), b"aaaa".find(b"aa", 1), b"aaaa".rfind(b"aa"), b"aaaa".rfind(b"aa", 0, 3))
print(h.index(b"XYZ"), h.rindex(b"abc"), h.rindex(b"abc", 0, 8))
for f in (lambda: h.index(b"q"), lambda: h.rindex(b"abc", 4, 6), lambda: h.find(300), lambda: h.find("a")):
    try:
        print(f())
    except (ValueError, TypeError) as e:
        print(type(e).__name__, e)
parts = [bytes([i % 256]) * (i % 5) for i in range(2000)]
blob = b"".join(parts)
print(len(blob), blob[:12], blob[-6:], sum(blob) % 9973)
print(b", ".join([b"a", bytearray(b"b"), b"c"]), b"".join([]), b"--".join([b"x"]), b"".join(iter([b"p", b"q"])))
big = b"".join([b"\x00\x01\x02\x03"] * 50000) + b"PK\x05\x06" + b"\x00" * 18
print(len(big), big.rfind(b"PK\x05\x06"), big.find(b"PK\x05\x06"), big.rfind(b"PK\x05\x06", 0, 200003), big.count(b"\x03\x00"))
try:
    b"".join([b"a", "b"])
except TypeError as e:
    print("TypeError", e)
with open("py_rtfix_bytes_search.tmp", "wb") as f:
    f.write(b"head")
    f.write(big)
    f.write(b"tail")
    f.seek(2)
    f.write(b"EA")
with open("py_rtfix_bytes_search.tmp", "rb") as f:
    back = f.read()
print(len(back), back[:6], back[-4:], back[4:4 + len(big)] == big)
import os
os.remove("py_rtfix_bytes_search.tmp")
