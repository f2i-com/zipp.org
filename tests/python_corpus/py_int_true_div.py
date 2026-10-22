# int / int beyond 2^53: correctly rounded (half to even), subnormal
# results kept, the overflow boundary and signed zeros as CPython has them.
# repr is the shortest round-trip form, so equal output is equal bits.

def show(label, a, b):
    try:
        print(label, repr(a / b))
    except (OverflowError, ZeroDivisionError) as e:
        print(label, type(e).__name__, e)

# Representable results the scaled 70-bit quotient flushed to zero.
show("tiny_normal", 1, 1 << 1020)
show("tiny_normal_neg", -1, 1 << 1020)
show("min_normal", 1, 1 << 1022)
show("subnormal", 1, 1 << 1030)
show("min_subnormal", 1, 1 << 1074)
show("min_subnormal_neg", 1, -(1 << 1074))
show("half_min_subnormal", 1, 1 << 1075)
show("three_quarter_min_subnormal", 3, 1 << 1076)
show("below_half_min_subnormal", 1, (1 << 1075) + 1)
show("above_half_min_subnormal", (1 << 200) + 1, 1 << 1275)
show("far_below", 1, 1 << 2000)
show("far_below_neg", -1, 1 << 2000)

# Halfway cases: 1 + 2^-53 is a tie (to even, down); anything past it rounds up.
show("tie_down", (1 << 80) + (1 << 27), 1 << 80)
show("above_tie", (1 << 80) + (1 << 27) + 1, 1 << 80)
show("below_tie", (1 << 80) + (1 << 27) - 1, 1 << 80)
show("tie_up", (1 << 80) + 3 * (1 << 27), 1 << 80)
for k in range(54, 140, 9):
    tie = (1 << k) + (1 << (k - 53))
    show("tie_%d" % k, tie, 1)
    show("tie_%d_up" % k, tie + 1, 1)
    show("tie_%d_down" % k, tie - 1, 1)
    show("tie_%d_neg" % k, -tie - 1, 1)

# Zero, equal and huge operands.
show("zero_by_huge", 0, 1 << 100)
show("zero_by_huge_neg", 0, -(1 << 100))
show("equal_huge", 7 ** 400, 7 ** 400)
show("thirds", 10 ** 60, 3 * 10 ** 59)
show("by_zero", 1 << 100, 0)

# The overflow boundary: 2^1024 - 2^970 is the tie between the largest
# double and 2^1024, which rounds to even (2^1024) and overflows.
show("max_double", (1 << 1024) - (1 << 971), 1)
show("below_overflow_tie", (1 << 1024) - (1 << 970) - 1, 1)
show("overflow_tie", (1 << 1024) - (1 << 970), 1)
show("overflow", 1 << 1024, 1)
show("overflow_neg", -(1 << 1100), 3)
show("near_max", (1 << 2048) - 1, (1 << 1024) + 1)
show("big_ratio", 10 ** 400, 10 ** 100)

# A seeded spread of sizes, signs and ratios.
state = 2463534242
def rand(bits):
    global state
    out = 0
    for _ in range((bits + 31) // 32):
        state = (state * 1103515245 + 12345) % (1 << 31)
        out = (out << 31) | state
    return out % (1 << bits) if bits else 0

for i in range(400):
    a = rand(rand(11) % 1300 + 1) | 1
    b = rand(rand(11) % 1300 + 1) | 1
    if rand(1):
        a = -a
    if rand(1):
        b = -b
    show("r%d" % i, a, b)
