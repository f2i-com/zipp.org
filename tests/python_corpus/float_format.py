# repr/str/format/round of floats: shortest round-trip repr, exponent switch points,
# half-to-even rounding, format specs on ints and floats.
samples = [0.0, -0.0, 1.0, -1.5, 0.1, 0.2, 0.3, 1 / 3, 2 / 3, 1e15, 1e16, 1e17, 123456789.0, 1234567890123456789.0,
           1e-4, 1e-5, 0.00012345, 5e-324, 2.2250738585072014e-308, 1.7976931348623157e308, 9007199254740993.0,
           float("inf"), float("-inf"), float("nan"), 3.141592653589793, 2.718281828459045, 100.0, 1e22, 1e23, 0.5, 2.5e-7]
for v in samples:
    print("repr", repr(v), str(v), f"{v!r}", f"{v!s}", [v], (v,), "{!r}".format(v))
for v in [0.0, -0.0, 1.0, -1.5, 0.1, 1 / 3, 1e15, 1e16, 1e22, 123456789.0, 1e-4, 1e-5, 0.00012345, 3.141592653589793, float("inf"), float("nan"), 2.5e-7]:
    print("fstr", f"{v}", f"[{v}]", f"{v}{v}", "{}".format(v))

for v in [0.0, 1.0, -1.5, 0.1, 1 / 3, 1e16, 123456.789, 1e-7, 5e-324, float("inf"), float("nan"), -2.5]:
    print("spec", f"{v:.0f}|{v:.3f}|{v:e}|{v:.2e}|{v:g}|{v:.3g}|{v:10.4f}|{v:<12.2f}|{v:^12.1f}|{v:+.1f}|{v:%}" if v == v and abs(v) != float("inf") and abs(v) < 1e300 else f"{v:f}|{v:e}|{v:g}|{v:+}")

for v in [0, 1, -1, 42, -42, 255, 2**31, -2**63, 10**15, 123456789]:
    print("intspec", f"{v:d}|{v:5d}|{v:<6}|{v:^7}|{v:+d}|{v: d}|{v:x}|{v:X}|{v:#x}|{v:o}|{v:#o}|{v:b}|{v:#b}|{v:,}|{v:_}|{v:08d}|{v:e}|{v:.2f}|{v:%}")

halves = [0.5, 1.5, 2.5, 3.5, -0.5, -1.5, -2.5, 0.125, 0.375, 2.675, 1.005, 0.285, 1234.5, 1e16 + 2]
print("round0", [round(h) for h in halves])
print("round1", [round(h, 1) for h in halves])
print("round2", [round(h, 2) for h in halves])
print("round-neg", round(1234.5678, -1), round(1234.5678, -2), round(-1250.0, -2), round(1350, -2), round(1250, -2), round(-1250, -2), round(15, -1), round(25, -1))
print("round-types", type(round(2.5)).__name__, type(round(2.5, 0)).__name__, type(round(7, 1)).__name__, round(7, 1), round(2**70 + 0.0), round(True))
print("format()", format(1.5), format(1.5, ""), format(2**64, ","), format(-0.0, ".1f"), format(1e100, ".3g"), format(12.0, "g"), format(0.0001, "g"), format(0.00001, "g"))
print("str-format", "{:.2f}".format(1.005), "{:.1%}".format(0.12345), "{:e}".format(0.0), "{:.10g}".format(1 / 7), "{:>10.3e}".format(-12345.678))
print("percent", "%.2f" % 2.675, "%e" % 1e-10, "%g" % 1e20, "%g" % 123456.0, "%G" % 1e-20, "%10.3f|" % 3.14159, "%-10.2e|" % 314.159, "%+.0f" % 0.5, "%.0f" % 1.5, "%r" % 0.1, "%s" % 1e16)
vals = [(i * 37 % 17) / 8 - 1 for i in range(12)]
print("table", " ".join(f"{v:6.3f}" for v in vals))
print("sum-repr", sum(vals), sum(vals) / len(vals), [round(v * 3, 2) for v in vals])
print("float-int-mix", 3 * 0.1, 0.1 * 3, 1.1 * 1.1, 1e16 + 1, 1e16 + 2, 2**0.5 * 2**0.5, 10 / 3 * 3, 4.35 * 100, 0.57 * 100)
