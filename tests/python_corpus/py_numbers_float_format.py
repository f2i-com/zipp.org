# Float and int formatting: an empty spec is repr, '+' without a type,
# 'f' on huge floats, '%' on big ints, and printf-style edge cases.

# An empty format spec formats a float like repr(), exponent form included.
for v in [1234567890123456789.0, 5e-324, 2.2250738585072014e-308, 1.7976931348623157e308, 1.2345678e-10,
          123456789012345678.0, 1e16, 1e15, 0.0001, 0.00001, -0.0, 1.5, 100.0]:
    print("fstr-nospec", f"{v}", format(v), format(v, ""), "{}".format(v), f"{v:}", str(v) == format(v))
# The '+' / ' ' sign flags and a width with no presentation type are repr-like too.
print("plus-nospec", f"{1.7976931348623157e308:+}", f"{1234567.125:+}", f"{0.1:+}", f"{1e-7:+}", f"{123456789.123:+}", f"{-2.5: }", f"{3.0: }")
print("width-nospec", f"{1e16:>25}|", f"{0.1:<8}|", f"{-1e-7:^15}|", f"{2.5:010}|", f"{1e22:,}", f"{1234567.0:_}")
print("precision-nospec", f"{1234567.891:.3}", f"{0.000012345:.2}", f"{1e16:.3}", f"{123.0:.10}", f"{9.999:.2}", f"{0.5:.0}", f"{5.0:.1}", f"{15.0:.2}", f"{99.5:.2}", f"{0.0:.3}", f"{1e-5:.2}")
# The 'f' type on a huge float prints every integer digit.
print("f-huge", f"{1.7976931348623157e308:f}"[:40], f"{1e22:f}", f"{2.0**70:.1f}", "%f" % 1e22, f"{1e21:.2f}", f"{-1e23:,.0f}")
print("f-small", f"{1e-7:f}", f"{5e-324:.3f}", f"{0.125:.2f}", f"{0.375:.2f}", f"{2.675:.2f}", f"{-0.0:.1f}")
# The float types on an int convert exactly, not through repr of the float.
print("int-percent", f"{10**20:%}", f"{10**17:%}", f"{2**60:.1%}", format(10**16, "%"), f"{7:e}", f"{2**80:g}", f"{123:.2f}")
try:
    format(10**400, "f")
except OverflowError as e:
    print("OverflowError", e)
# printf-style formatting of floats and ints.
print("printf", "%s %r %g %e %.3f %5.1f|%-6.2f|" % (1e16, 1e-5, 1e16, 12345.678, 2.0005, 3.14159, 2.5),
      "%d %i %x %o %X" % (3.9, -2.5, 255, 8, 3054), "%+d % d %05d" % (5, 5, -42), "%.0f %.0f %.0f" % (0.5, 1.5, 2.5))
print("printf-big", "%d" % 10**30, "%x" % 2**70, "%.2f" % 10**17, "%g" % 10**20, "%e" % 2**64)
print("repr-str", repr(1e16), repr(1e-5), repr(0.1 + 0.2), str(2.0**62), repr(-1e300 * 10), str(float("nan")), repr(1e300 * 1e10))
