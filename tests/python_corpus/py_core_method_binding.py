# Pending: bound method equality (split out of tests/python_corpus/method_binding.py).
class G:
    def greet(self):
        return "hi"
g = G()
m = g.greet
print("bound-eq", g.greet == g.greet, m == g.greet, g.greet != g.greet, [].append == [].append, hash(g.greet) == hash(g.greet))
l = []
print("builtin-bound-eq", l.append == l.append, {g.greet: 1}.get(g.greet, "miss"))
