from dataclasses import dataclass
from collections import namedtuple
from enum import Enum

class Color(Enum):
    RED = 1
    GREEN = 2

@dataclass
class Point:
    x: int
    y: int

Pair = namedtuple("Pair", "a b")

def describe(v):
    match v:
        case 0 | 1 | 2:
            return "small"
        case int(n) if n < 0:
            return f"negative {n}"
        case int():
            return "int"
        case float(f):
            return f"float {f}"
        case str() as s:
            return f"str {s!r}"
        case None:
            return "none"
        case True:
            return "true"
        case False:
            return "false"
        case [x, y]:
            return f"pair {x} {y}"
        case [x, *rest]:
            return f"head {x} rest {rest}"
        case []:
            return "empty seq"
        case {"name": str(name), "age": int(age), **extra}:
            return f"person {name} {age} {sorted(extra)}"
        case {"kind": "circle", "r": r}:
            return f"circle {r}"
        case {}:
            return "mapping"
        case Point(x=0, y=0):
            return "origin"
        case Point(x, y) if x == y:
            return f"diag {x}"
        case Point(x=px, y=py):
            return f"point {px},{py}"
        case Pair(a, b):
            return f"Pair {a} {b}"
        case Color.RED:
            return "red"
        case Color():
            return "other color"
        case _:
            return f"other {type(v).__name__}"

tests = [0, 2, -5, 7, 2.5, "hi", None, True, False, [1, 2], (1, 2, 3), [], {"name": "Al", "age": 3, "z": 1},
         {"kind": "circle", "r": 2}, {"a": 1}, Point(0, 0), Point(3, 3), Point(1, 2), Pair(1, 2), Color.RED, Color.GREEN,
         range(2), b"xy", set()]
for t in tests:
    print(repr(t), "->", describe(t))

def cmd(c):
    match c.split():
        case ["go", ("north" | "south") as d]:
            return "move " + d
        case ["go", *_]:
            return "bad direction"
        case ["pick", "up", item] | ["pick", item, "up"]:
            return "pick " + item
        case ["drop", *items] if items:
            return "drop " + ",".join(items)
        case [cmd, *args] if len(args) > 2:
            return f"{cmd} many"
        case _:
            return "?"
for s in ["go north", "go west", "pick up sword", "pick shield up", "drop a b", "drop", "x 1 2 3", ""]:
    print(s, "->", cmd(s))

match (1, [2, {"k": (3, 4)}]):
    case (a, [b, {"k": (c, d)}]):
        print("nested", a, b, c, d)
x = 5
match x:
    case y:
        print("capture", y)
match [1, 2, 3]:
    case [1, *mid, 3]:
        print("mid", mid)
match {"a": 1, "b": 2}:
    case {"a": 1, **rest}:
        print("rest", rest)
class Bad:
    __match_args__ = ("q",)
    def __init__(self): self.q = 9
match Bad():
    case Bad(9): print("bad ok")
try:
    match Bad():
        case Bad(1, 2): pass
except TypeError as e:
    print("TypeError:", e)
try:
    match 3:
        case int(1, 2): pass
except TypeError as e:
    print("TypeError:", e)
try:
    match 3:
        case x() if False: pass
except TypeError as e:
    print("TypeError:", e)
match 3:
    case str() | int() as n:
        print("or-as", n)
