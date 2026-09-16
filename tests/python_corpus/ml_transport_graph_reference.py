# zipp_gpu graphs submitted without a host. CPython evaluates them with the
# module's pure-Python float32 reference (`execute_locally`); Zipp evaluates
# them with its tensor kernels. Every number is printed with repr, so the two
# must agree bit for bit: rounding of each matmul partial sum, the pairwise
# sum's odd tails, signed zeros, broadcasting of scalars, aliased outputs.
from zipp_gpu import Graph, GraphError, ComputeError, execute_locally


def lcg(seed, n, scale):
    out = []
    for _ in range(n):
        seed = (seed * 1103515245 + 12345) % 2147483648
        out.append((seed / 2147483648.0 - 0.5) * scale)
    return out


def show(label):
    def callback(result):
        print(label, result["backend"], result["stats"])
        for name in sorted(result["outputs"]):
            out = result["outputs"][name]
            print(" ", name, out["shape"], out["dtype"], type(out["data"]).__name__, out["data"])
    return callback


# Elementwise ops, scalar broadcasting on either side, signed zeros.
g = Graph()
a = g.tensor([0.1, -2.5, 3.25, 1e-3, -0.0, 7.0])
b = g.tensor([1 / 3, 7, -0.2, 5, -0.0, -7.0])
s = g.tensor(0.7)
g.submit(show("elementwise"), None,
         add=a + b, sub=a - b, mul=a * b, lscalar=a * s, rscalar=1.5 - a,
         both=s * 3 + s, relu=(a - b).relu(), positive=(a * b).positive())

# Matmul accumulates in float32; transpose; sums with odd tails.
g = Graph()
m = g.tensor([lcg(1, 5, 4.0), lcg(2, 5, 4.0), lcg(3, 5, 4.0)])
n = g.tensor([lcg(4, 4, 2.0), lcg(5, 4, 2.0), lcg(6, 4, 2.0), lcg(7, 4, 2.0), lcg(8, 4, 2.0)])
p = m @ n
g.submit(show("matmul"), None, product=p, transposed=p.transpose(), total=p.sum(),
         seven=g.tensor(lcg(9, 7, 100.0)).sum(), one=g.tensor([2.5]).sum())

# A larger product whose rounding differs from double accumulation.
g = Graph()
x = g.tensor([lcg(10 + r, 24, 1.0) for r in range(8)])
w = g.tensor([lcg(40 + r, 6, 1.0) for r in range(24)])
h = (x @ w).relu()
g.submit(show("mlp"), None, hidden=h, loss=(h * h).sum())

# The same output requested twice, an input requested as an output, full/zeros.
g = Graph()
v = g.tensor([[1, 2], [3, 4]])
twice = v * 2
g.submit(show("aliases"), None, first=twice, second=twice, raw=v,
         filled=g.full((2, 3), 0.1), nothing=g.zeros(3))

# One step of the toroidal life rule.
g = Graph()
grid = g.tensor([[0, 1, 0, 0, 0], [0, 0, 1, 0, 0], [1, 1, 1, 0, 0], [0, 0, 0, 0, 0], [0, 0, 0, 0, 0]])
g.submit(show("life"), None, next=grid.life(), after=grid.life().life())

# The reference itself, called directly, agrees with submit.
g = Graph()
c = g.tensor(lcg(77, 9, 3.0)) * g.tensor(lcg(78, 9, 3.0))
t = c.sum()
seen = []
g.submit(seen.append, None, result=c, total=t)
print("reference agrees", execute_locally(g.program(result=c, total=t)) == seen[0])

# An overflow is refused at readback.
g = Graph()
big = g.tensor([3e38, 1.0])
try:
    g.submit(show("overflow"), None, result=big * big)
except ComputeError as error:
    print("overflow", error.code, error)

# Recording errors.
for label, action in [
    ("empty", lambda: Graph().tensor([])),
    ("ragged", lambda: Graph().tensor([[1, 2], [3]])),
    ("bool", lambda: Graph().tensor([True])),
    ("nan", lambda: Graph().tensor([float("nan")])),
    ("inf", lambda: Graph().tensor([1e39])),
    ("rank", lambda: Graph().tensor([[[1]]])),
    ("shape", lambda: Graph().tensor([1, 2, 3], (2, 2))),
    ("mismatch", lambda: (lambda q: q.tensor([1, 2]) + q.tensor([1, 2, 3]))(Graph())),
    ("matmul", lambda: (lambda q: q.tensor([[1, 2]]) @ q.tensor([[1, 2]]))(Graph())),
    ("transpose", lambda: Graph().tensor([1, 2]).transpose()),
    ("foreign", lambda: Graph().tensor([1]) + Graph().tensor([1])),
    ("name", lambda: (lambda q: q.program(**{"bad-name": q.tensor([1])}))(Graph())),
    ("none", lambda: Graph().program()),
    ("many", lambda: (lambda q: q.program(**{"o%d" % i: q.tensor([i]) for i in range(17)}))(Graph())),
]:
    try:
        action()
        print(label, "accepted")
    except GraphError as error:
        print(label, type(error).__name__, error)
