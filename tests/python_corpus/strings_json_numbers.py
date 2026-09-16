# Float repr and string searching as the engine's shared builtins do
# them for Python (15 September 2026 audit, strings-and-JSON track).

# repr/str pick the shortest digits, and an exact tie between two equally
# short candidates goes to the even one.
x = 9007199254741024 / 15
print(x, repr(x), str(-600479950316035.25), 1286742750677258.25, f"{x}")
print([9007199254741024 / d for d in (3, 7, 11, 13, 15, 17, 19)])
print(0.1, 1 / 3, 2 / 3, 1e16, 1.5e-07, 5e-324, 1.7976931348623157e308, 123.456)
print(sum(1 for d in range(1, 2000) if repr(9007199254740991 / d) != str(9007199254740991 / d)))

# A lone surrogate made at run time is itself, not U+FFFD, to the searching,
# splitting and case-mapping builtins.
L, R = chr(0xD800), chr(0xFFFD)
s = "a" + L + "b" + R + "c" + L
print(s.count(L), s.count(R), L in "a" + R + "b", R in "a" + L + "b")
print(s.startswith("a" + L), s.endswith("c" + L), s.startswith("a" + R))
print([len(p) for p in s.split(L)], [ord(c) for c in s.replace(L, "")])
print([ord(c) for c in s.upper()], [ord(c) for c in s.lower()])
