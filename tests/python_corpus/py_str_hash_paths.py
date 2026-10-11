# A str's hash is computed in three places that must agree: the hash()
# builtin, a dict's or set's own hashing of a str key, and the hashing of a
# str inside a tuple or a frozenset. A class whose __hash__ delegates to
# hash(str) has to land where the plain str lands, whichever of them hashed
# it. No hash value is printed (CPython's str hashes differ from run to run).
class K:
    def __init__(self, s):
        self.s = s

    def __hash__(self):
        return hash(self.s)

    def __eq__(self, other):
        return (other.s if isinstance(other, K) else other) == self.s


words =["", "a", "abc", "caf" + chr(0xE9), chr(0x1F600), "x" * 1000, chr(0xD800), "it's", "\x00abc",
         chr(0x65E5) + chr(0x672C), "a" + chr(0x1F600) + "b", chr(0xDC00) + "x"]
d = {K(w): i for i, w in enumerate(words)}
print([d[w] for w in words])
print([d[K(w)] for w in words])
plain = {w: i for i, w in enumerate(words)}
print([plain[K(w)] for w in words])
s = {K(w) for w in words}
print(all(w in s for w in words), len(s), K("abc") in set(words), "abd" in s)
print([hash(K(w)) == hash(w) for w in words])
print([hash(w) == hash(str(w)) == hash("".join(list(w))) for w in words])
print(len({hash(w) for w in words}) == len(words))
print(all(isinstance(hash(w), int) for w in words))
fs = frozenset(words)
print(hash(fs) == hash(frozenset(reversed(words))), {fs: 1}[frozenset(list(words))])
t = tuple(words)
print(hash(t) == hash(tuple(list(words))), {t: 2}[tuple(words)])
print(hash(("abc", 1)) == hash(("ab" + "c", 1)), hash(("abc",)) == hash(("abd",)))


def hot(n):
    # hash() often enough for its site to be compiled.
    same = 0
    for i in range(n):
        w = words[i % len(words)]
        if hash(w) == hash(words[i % len(words)]) and hash(K(w)) == hash(w):
            same += 1
    return same


print(hot(3000))
