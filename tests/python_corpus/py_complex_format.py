# complex formatting: format()/f-string/str.format specs (complex.__format__:
# no type, e/E/f/F/g/G/n, precision, width, alignment and fill, sign, '#',
# 'z', grouping, special values) and its errors, plus %-formatting.
inf = float("inf")
nan = float("nan")
values = [0j, -0j, 1j, -1j, 1 + 2j, -1.5 - 2.25j, complex(-0.0, 0.0), complex(0.0, -0.0), 1234567.891 + 0.000123j, 1e16 + 1e-5j,
          complex(1e300, -1e-300), 0.5 + 2.5j, 1.005 - 0.125j, complex(inf, nan), complex(-inf, -inf), complex(nan, -0.0),
          123456789 + 987654321j, complex(-0.001, 0.0004), 1 / 3 + 2 / 3j]
specs = ["", "g", ".3", ".3g", ".0g", "e", ".2e", "E", "f", ".1f", ".0f", "F", "G", ".10G", "n", ".4n", "10", "<30", ">30", "^30",
         "*^32.3f", "+", "+g", " ", " .2f", "-.3e", "#", "#g", "#.0f", "#.0e", "#.3g", "z", "z.1f", "z.0f", "+z.2e", ",", ",.2f", "_",
         "_.3f", "30,.2f", "x>40,g", "=<25", "\u00e9^20.1e", ".17g", ".20f", ".25e", "#.1", "12.4"]
for v in values:
    print(repr(v))
    for s in specs:
        print("  %-8s|%s|" % (s, format(v, s)))
print(f"{1 + 2j}", f"{1 + 2j!r}", f"{1 + 2j!s:>12}", f"{-3j:+.1f}", f"{2.5j:{'>'}{10}.{2}f}", "{:.2f}|{:>9}|{!r}".format(1j, 2j, 3j))
print("{0.real} {0.imag} {0:e}".format(1.5 + 2j), "%s %r %5s %-8r|" % (1j, 1 + 1j, 2j, -1j), str(1e-7 + 1e22j))
for s in ["010", "05.2f", "0>10", "0<10", "<010", "=10", "=^10", "0=10", "d", "x", "%", "s", "c", "b", "o", "X", ".2%", "10d",
          ",d", ".1s", "z", "Z", "ee", ".f", "-+"]:
    try:
        print(repr(s), format(1 + 2j, s))
    except (ValueError, TypeError) as e:
        print(repr(s), type(e).__name__, e)
for f in [lambda: "%d" % 1j, lambda: "%i" % (1 + 1j), lambda: "%f" % 1j, lambda: "%e" % 1j, lambda: "%g" % 1j, lambda: "%x" % 1j,
          lambda: "%o" % 1j, lambda: "%c" % 1j, lambda: "%.2f" % (1j,), lambda: format(1j, 5)]:
    try:
        print(f())
    except (ValueError, TypeError) as e:
        print(type(e).__name__, e)
