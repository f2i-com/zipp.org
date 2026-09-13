# Compute graphs with the bundled zipp_gpu library. Without a GPU host (the
# CLI, CPython) `submit` evaluates the graph with the float32 reference
# implementation and calls back before returning.
import struct
from zipp_gpu import Graph, GraphError, ComputeError, execute_locally

print(struct.pack("<f", 0.1).hex(), struct.unpack("<f", struct.pack("<f", 0.1))[0], struct.calcsize("<ihd"), struct.pack(">I", 258), struct.unpack("<2h?", b"\x01\x00\xff\xff\x01"))

g = Graph()
a = g.tensor([1, 2, 3, 4])
b = g.tensor([10, 20, 30, 40])
c = (a * b + 4).relu()
print(a, c.shape, c.dtype)
program = g.program(result=c, total=c.sum())
print(program["version"], len(program["nodes"]), [n["op"] for n in program["nodes"]], program["outputs"])


def show(result):
    print("backend:", result["backend"])
    for name in sorted(result["outputs"]):
        out = result["outputs"][name]
        print(name, out["shape"], out["data"])


g.submit(show, result=c, total=c.sum())

m = Graph()
x = m.tensor([[1, 2, 3], [4, 5, 6]])
y = m.tensor([[7, 8], [9, 10], [11, 12]])
m.submit(show, result=x @ y)

n = Graph()
x = n.tensor([[1, -2, 3], [0, 1, 2]])
w1 = n.tensor([[0.5, -1, 2, 0], [1, 0, -0.5, 1], [-1, 1, 0, 0.5]])
hidden = (x @ w1 + 0.25).relu()
w2 = n.tensor([[1, 0], [0.5, -0.5], [1, 1], [-1, 2]])
n.submit(show, result=hidden @ w2)

life = Graph()
seed = [0] * (8 * 8)
for yy, xx in [(1, 2), (2, 3), (3, 1), (3, 2), (3, 3)]:
    seed[yy * 8 + xx] = 1
state = life.tensor(seed, shape=(8, 8))
for _ in range(4):
    state = state.life()


def grid(result):
    data = result["outputs"]["result"]["data"]
    for row in range(8):
        print("".join("#" if data[row * 8 + col] else "." for col in range(8)))
    print("alive:", result["outputs"]["alive"]["data"][0])


life.submit(grid, result=state, alive=state.sum())

for bad in [lambda: Graph().tensor([]), lambda: Graph().tensor([[1, 2], [3]]), lambda: g.tensor([1, 2]) + m.tensor([1, 2]),
            lambda: g.tensor([1, 2]) + g.tensor([1, 2, 3]), lambda: g.tensor([1, 2]) @ g.tensor([1, 2]), lambda: g.program(),
            lambda: g.tensor(float("inf")), lambda: g.tensor([True]), lambda: bool(g.tensor([1]))]:
    try:
        bad()
        print("no error")
    except (GraphError, TypeError) as e:
        print(type(e).__name__, e)
try:
    execute_locally({"version": 2})
except ComputeError as e:
    print(e, e.code)
print(1e-7 * 3, 0.1 + 0.2, sum(range(5)))
