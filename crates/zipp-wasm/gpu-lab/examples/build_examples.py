"""Run from any directory: python examples/build_examples.py. No pip packages."""
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT.parents[1] / "zipp-vm" / "src" / "frontend" / "python" / "lib" / "shared"))
from zipp_gpu import Graph


def examples():
    g = Graph()
    a = g.tensor([1, 2, 3, 4])
    b = g.tensor([10, 20, 30, 40])
    c = (a * b + 4).relu()
    yield "vector", g.program(result=c, total=c.sum())

    g = Graph()
    a = g.tensor([[1, 2, 3], [4, 5, 6]])
    b = g.tensor([[7, 8], [9, 10], [11, 12]])
    yield "matmul", g.program(result=a @ b)

    # A tiny inference graph with explicit weights; no training or autograd.
    g = Graph()
    x = g.tensor([[1, -2, 3], [0, 1, 2]])
    w1 = g.tensor([[0.5, -1, 2, 0], [1, 0, -0.5, 1], [-1, 1, 0, 0.5]])
    hidden = (x @ w1 + 0.25).relu()
    w2 = g.tensor([[1, 0], [0.5, -0.5], [1, 1], [-1, 2]])
    yield "tiny_mlp", g.program(result=hidden @ w2)

    g = Graph()
    state = [0] * (32 * 32)
    for y, x in [(3, 4), (4, 5), (5, 3), (5, 4), (5, 5)]:
        state[y * 32 + x] = 1
    initial = g.tensor(state, shape=(32, 32))
    current = initial
    for _ in range(24):
        current = current.life()
    yield "life", g.program(initial=initial, result=current, alive=current.sum())


if __name__ == "__main__":
    output = ROOT / "generated"
    output.mkdir(exist_ok=True)
    for name, program in examples():
        path = output / (name + ".json")
        path.write_text(json.dumps(program, separators=(",", ":"), allow_nan=False) + "\n", encoding="utf-8")
        print("Generated", path.relative_to(ROOT), "(%d nodes)" % len(program["nodes"]))
