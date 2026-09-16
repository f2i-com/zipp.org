# Flat constructs are not nesting: elif ladders, long operator, comparison
# and boolean chains, and large literals compile and evaluate left to right.
import sys


def classify(x):
    if x == 0:
        return "zero"
    elif x == 1:
        return "one"
    elif x == 2:
        return "two"
    elif x == 3:
        return "three"
    elif x == 4:
        return "four"
    elif x == 5:
        return "five"
    elif x == 6:
        return "six"
    elif x == 7:
        return "seven"
    elif (y := x * 2) == 16:
        return "eight " + str(y)
    else:
        return "many"


print([classify(i) for i in range(10)])

total = 1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11 + 12 + 13 + 14 + 15 + 16 + 17 + 18 + 19 + 20 + 21 + 22 + 23 + 24 + 25 + 26 + 27 + 28 + 29 + 30 + 31 + 32 + 33 + 34 + 35 + 36 + 37 + 38 + 39 + 40 + 41 + 42 + 43 + 44 + 45 + 46 + 47 + 48 + 49 + 50 + 51 + 52 + 53 + 54 + 55 + 56 + 57 + 58 + 59 + 60 + 61 + 62 + 63 + 64 + 65 + 66 + 67 + 68 + 69 + 70 + 71 + 72 + 73 + 74 + 75 + 76 + 77 + 78 + 79 + 80 + 81 + 82 + 83 + 84 + 85 + 86 + 87 + 88 + 89 + 90 + 91 + 92 + 93 + 94 + 95 + 96 + 97 + 98 + 99 + 100 + 101 + 102 + 103 + 104 + 105 + 106 + 107 + 108 + 109 + 110 + 111 + 112 + 113 + 114 + 115 + 116 + 117 + 118 + 119 + 120
print(total)
mixed = 100 - 1 - 2 - 3 - 4 - 5 - 6 - 7 - 8 - 9 - 10 + 11 * 2 - 12 // 5 + 13 % 4 - 14 - 15 - 16 - 17 - 18 - 19 - 20 + 21 - 22 + 23 - 24 + 25 - 26 + 27 - 28 + 29 - 30 + 31 - 32 + 33 - 34 + 35 - 36 + 37 - 38 + 39 - 40 + 41 - 42 + 43 - 44 + 45 - 46 + 47 - 48 + 49 - 50 + 51 - 52 + 53 - 54 + 55 - 56 + 57 - 58 + 59 - 60 + 61 - 62 + 63 - 64 + 65 - 66 + 67 - 68 + 69 - 70 + 71 - 72 + 73 - 74 + 75 - 76 + 77 - 78 + 79 - 80 + 81 - 82 + 83 - 84 + 85 - 86 + 87 - 88 + 89 - 90 + 91 - 92 + 93 - 94 + 95 - 96 + 97 - 98 + 99 - 100
print(mixed)
s = "a" + "b" + "c" + "d" + "e" + "f" + "g" + "h" + "i" + "j" + "k" + "l" + "m" + "n" + "o" + "p" + "q" + "r" + "s" + "t" + "u" + "v" + "w" + "x" + "y" + "z" + "A" + "B" + "C" + "D" + "E" + "F" + "G" + "H" + "I" + "J" + "K" + "L" + "M" + "N" + "O" + "P" + "Q" + "R" + "S" + "T" + "U" + "V" + "W" + "X" + "Y" + "Z" + "0" + "1" + "2" + "3" + "4" + "5" + "6" + "7" + "8" + "9" + "a" + "b" + "c" + "d" + "e" + "f" + "g" + "h" + "i" + "j" + "k" + "l" + "m" + "n" + "o" + "p" + "q" + "r" + "s" + "t" + "u" + "v" + "w" + "x" + "y" + "z" + "A" + "B" + "C" + "D" + "E" + "F" + "G" + "H" + "I" + "J"
print(len(s), s[-5:])


def chained(a):
    return a < a + 1 < a + 2 < a + 3 < a + 4 < a + 5 < a + 6 < a + 7 < a + 8 < a + 9 < a + 10 < a + 11 < a + 12 < a + 13 < a + 14 < a + 15 < a + 16 < a + 17 < a + 18 < a + 19 < a + 20, a == a == a == a == a == a == a == a == a == a == a == a == a == a == a == a == a == a == a == 0


print(chained(0), chained(1))
flags = [1 and 2 and 3 and 4 and 5 and 6 and 7 and 8 and 9 and 10 and 11 and 12 and 13 and 14 and 15 and 16 and 17 and 18 and 19 and 20, 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or 0 or "last"]
print(flags)

big = [v * 2 for v in range(3000)]
literal = {i: i * i for i in range(300)}
print(len(big), big[-1], len(literal), literal[299])
data = b"0123456789abcdef" * 4
print(len(data), data[:6], data[-3:], list(b"\x00\x7f\x80\xff"))
nested = [[1, 2], [3, 4], *[[5, 6], [7, 8]], (9, 10)]
print(nested, [*range(3), *"ab", 0])
print(sys.version_info >= (3, 0))
