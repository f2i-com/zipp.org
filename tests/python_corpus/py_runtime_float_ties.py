# 'e', 'g' and precision-only float formats round the exact binary value with
# ties to even, like 'f' does (only exactly representable decimals tie).
print("{:.0e}".format(25), "%.1e" % 125, "{:.3g}".format(12.25), f"{0.125:.2}")
print("{:.0e}".format(35), "{:.0e}".format(-25), "{:.1e}".format(0.125), "%.2E" % 1.125, "{:.0e}".format(95))
print("{:.1e}".format(9.75), "{:.1e}".format(9.95), "{:.2e}".format(0.1), "{:.3e}".format(2.5), "{:.0e}".format(0.5))
print("%g" % 0.0000125, "%.2g" % 2.5, "%.1g" % 2.5, "%.1g" % 3.5, "%.1g" % 0.25, "%.2g" % 125)
print("{:.3g}".format(12.35), "{:.2g}".format(0.125), "{:.3}".format(1.125), f"{2.5:.1}", f"{-0.375:.2}")
print("{:.1}".format(95.0), "{:.2}".format(99.5), "{:#.3g}".format(1.25), "%.3G" % 1.0625e20, "{:.1g}".format(1e22))
print("{:.2%}".format(0.000125), "%10.1e|" % 125, "{:+.0e}".format(45), "{:.5g}".format(123455))
print(repr(0.125), str(2.5), repr(1e22), 1.25, 1e-5, repr(12.25), f"{0.125}", "{}".format(2.675))
print(round(2.675, 2), round(0.125, 2), "%.2f" % 0.125, "{:.1f}".format(0.25))
print(format(0.0, ".1"), format(-0.0, ".2"), "%#.3g" % 0.0, "{:#.2}".format(0.0), "{:.0e}".format(0.0), "%g" % -0.0, "{:.3g}".format(0.0))
