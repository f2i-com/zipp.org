# `str in str`, which the engine answers natively when both are ASCII:
# found, absent, empty and longer needles, needles either side of the
# searcher's short-needle length, almost-matches at every offset (once a
# compare per start, quadratic in the needle), and the non-ASCII path.

def show(label, needle, hay):
    print(label, needle in hay, needle not in hay)

show("found", "b", "abc")
show("absent", "abd", "abc")
show("empty in empty", "", "")
show("empty in text", "", "abc")
show("longer", "abcd", "abc")
show("whole", "abc", "abc")
show("at end", "bc", "abc")
show("at start", "ab", "abc")
for n in range(1, 12):
    needle = "a" * (n - 1) + "b"
    show("short %d last" % n, needle, "a" * 40 + needle)
    show("short %d absent" % n, needle, "a" * 40)
    show("short %d first" % n, needle, needle + "a" * 40)
for size in (1024, 4096, 20000):
    hay = "a" * size
    needle = "a" * (size // 2 - 1) + "b"
    show("near %d absent" % size, needle, hay)
    show("near %d at end" % size, needle, hay + "b")
    show("near %d in middle" % size, needle, "a" * (size // 3) + needle + "a" * size)
show("periodic", "abab" * 50 + "abac", "abab" * 400)
show("periodic found", "abab" * 50 + "abac", "abab" * 400 + "abac")
show("non-ascii", "é", "café")
show("non-ascii absent", "é", "café")
show("non-ascii hay", "f", "café")
show("non-ascii needle", "éx", "café")
show("astral", "\U0001f600", "a\U0001f600b")
