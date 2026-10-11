# Loops whose iterable is written as a list or tuple display, in every place
# one can stand: a for statement, each clause of a comprehension, a
# generator expression, a class body, a generator function. The display's
# elements are evaluated once, in order, before the first step; a starred
# element splices; the loop sees exactly those values.
trace = []


def note(tag, value):
    trace.append(tag)
    return value


print("== for statements")
for x in (1, 2, 3):
    print("tuple", x)
for x in [4, 5]:
    print("list", x)
for x in ():
    print("never")
for x in []:
    print("never")
else:
    print("empty list: else ran")
for (x) in (7,):
    print("single", x)
for a, b in ((1, 2), (3, 4)):
    print("pair", a, b)
for a, *rest in [(1, 2, 3), (4,)]:
    print("star target", a, rest)
for i, (a, b) in ((0, ("x", "y")), (1, ("z", "w"))):
    print("nested target", i, a, b)

print("== evaluation order")
for x in (note("a", 1), note("b", 2), note("c", 3)):
    trace.append(x)
print(trace)
trace.clear()
n = 0
for x in (n + 1, n + 2, n + 3):
    n = x * 10
    print(x, n)

print("== break, continue, else")
for x in (1, 2, 3, 4, 5):
    if x == 2:
        continue
    if x == 4:
        break
    print("kept", x)
else:
    print("not reached")
for x in (1, 2):
    pass
else:
    print("else after", x)
total = 0
for x in [10, 20, 30]:
    for y in (1, 2):
        if y == 2 and x == 20:
            break
        total += x * y
    else:
        total += 1000
print(total)

print("== starred elements")
more = [2, 3]
none = []
for x in (1, *more, 4):
    print("spliced", x)
for x in [*none]:
    print("never")
for x in (*none, *more, *none):
    print("only", x)
for x in [*"ab", *range(2), *{"k": 1}]:
    print("mixed", x)
print([x * 2 for x in (0, *more, 9)], [x for x in [*none]], {x for x in (*more, *more)})

print("== targets that are not names")


class Box:
    pass


box = Box()
slots = [0, 0]
for box.value in (1, 2):
    pass
for slots[1] in ("a", "b"):
    pass
for slots[0], box.other in ((5, 6), (7, 8)):
    pass
print(box.value, box.other, slots)

print("== the loop variable outlives the loop; an empty one leaves it alone")
last = "before"
for last in ():
    pass
print(last)
for last in ("x", "y"):
    pass
print(last)

print("== comprehensions")
print([x * x for x in (1, 2, 3)])
print([x for x in [3, 1, 2] if x != 1])
print({x % 2 for x in (1, 2, 3, 4)} == {0, 1}, {x: x + 1 for x in (1, 2)})
print([(x, y) for x in (1, 2) for y in ("a", "b")])
print([(x, y) for x in range(2) for y in (x, x + 10)])
print([y for x in [[1, 2], [3]] for y in (*x, 0)])
print([x for x in (note("p", 1), note("q", 2))], trace)
trace.clear()
print([[y * x for y in (1, 2)] for x in (3, 4)])
print([f(2) for f in (lambda v: v + 1, lambda v: v * 5)])
print([(lambda: x)() for x in (1, 2, 3)])
print([w for x in (1, 2) if (w := x * 7) > 7], w)
print([x for x in ()], [x for x in []], {x: 1 for x in ()})
print([a + b for a, b in ((1, 2), (3, 4))], [r for _, *r in [(1, 2, 3)]])

print("== a comprehension that is its own function")
print(list(x * 2 for x in (1, 2, 3)), sum(x for x in [4, 5]), tuple(y for x in (1, 2) for y in (x, -x)))
print(sorted({k: [k + i for i in (0, 1)] for k in (1, 2)}.items()))


class Table:
    rows = [r * 2 for r in (1, 2, 3)]
    cols = [c for c in ("a", "b") for _ in (0, 1)]
    for name in ("first", "second"):
        pass
    kinds = [type(v).__name__ for v in (1, "s", 2.5, None)]


print(Table.rows, Table.cols, Table.name, Table.kinds)

print("== generators")


def gen():
    for x in (1, 2, 3):
        got = yield x
        if got:
            yield got * 100
    for y in [*"ab"]:
        yield y


g = gen()
print(next(g), g.send(None), g.send(5), list(g))


def squares():
    return (x * x for x in (1, 2, 3))


print(list(squares()), [v for v in squares()])

print("== exceptions and finally")
seen = []
try:
    for x in (1, 2, 0, 4):
        try:
            seen.append(10 // x)
        finally:
            seen.append("f")
except ZeroDivisionError:
    seen.append("zero")
print(seen)
try:
    for x in (1, undefined_name, 3):
        print("never")
except NameError as e:
    print("NameError before the first step")
try:
    print([x for x in (1, 1 // 0)])
except ZeroDivisionError:
    print("ZeroDivisionError before the first step")

print("== nothing else can change what the loop sees")
src = [1, 2, 3]
out = []
for x in [*src]:
    src.append(x * 10)
    out.append(x)
print(out, src)
cell = [0]
for x in (cell, cell):
    x[0] += 1
print(cell)

print("== a long display")
big = [x for x in (
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
    32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
    64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95,
    96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121,
    122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143, 144, 145, 146,
    147, 148, 149, 150, 151, 152, 153, 154, 155, 156, 157, 158, 159, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171,
    172, 173, 174, 175, 176, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195, 196,
    197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218, 219, 220, 221,
    222, 223, 224, 225, 226, 227, 228, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242, 243, 244, 245, 246,
    247, 248, 249, 250, 251, 252, 253, 254, 255, 256, 257, 258, 259, 260, 261, 262, 263, 264, 265, 266, 267, 268, 269, 270, 271,
    272, 273, 274, 275, 276, 277, 278, 279, 280, 281, 282, 283, 284, 285, 286, 287, 288, 289, 290, 291, 292, 293, 294, 295, 296,
    297, 298, 299)]
print(len(big), sum(big), big[255:258], big[-1])
count = 0
for x in [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
    32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
    64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95,
    96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121,
    122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143, 144, 145, 146,
    147, 148, 149, 150, 151, 152, 153, 154, 155, 156, 157, 158, 159, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171,
    172, 173, 174, 175, 176, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195, 196,
    197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218, 219, 220, 221,
    222, 223, 224, 225, 226, 227, 228, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242, 243, 244, 245, 246,
    247, 248, 249, 250, 251, 252, 253, 254, 255, 256, 257, 258, 259, 260, *more]:
    count += x
print(count)


def hot(n):
    # The same shapes, run often enough to be compiled.
    total = 0
    for i in range(n):
        a, b, c = i, i + 1, i + 2
        total += len([x for x in (a, b, c)])
        for v in (a, b):
            total += v
        for v in [c, *more]:
            if v == 3:
                continue
            total += v
        total += sum([p * q for p in (1, 2) for q in (i, 1)])
    return total


print([hot(3) for _ in range(3)], hot(4000))
