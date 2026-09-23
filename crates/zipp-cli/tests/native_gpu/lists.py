# zipp_gpu itself (no torch): list data comes back as lists, and a prepared
# session carries its value on the device.
from zipp_gpu import Graph

gpu = Graph()
a = gpu.tensor([1, 2, 3, 4])
b = gpu.tensor([10, 20, 30, 40])
c = (a * b + 4).relu()
m = gpu.matmul(gpu.tensor([[1, 2], [3, 4]]), gpu.tensor([[5, 6], [7, 8]]))
result = []
gpu.submit(result.append, result=c, total=c.sum(), m=m)
out = result[0]["outputs"]
print(result[0]["backend"], out["result"]["data"], out["total"]["data"], out["m"]["data"], out["m"]["shape"],
      type(out["result"]["data"]).__name__)
g = Graph()
x = g.tensor([0.0, 1.0])
y = x + 1
session = g.prepare(carry={x: y}, resident=[y], y=y)
runs = []
session.run_steps(runs.append, [{}, {}, {}], readback=[y])
print("session", [s["outputs"]["y"]["data"] for s in runs[0]["steps"]], "step", runs[0]["step"], "backend", session.backend)
session.download(lambda r: print("resident", r["outputs"]["y"]["data"]), y)
session.dispose()
# A run longer than a host takes at once (64 steps) and more live sessions
# than a host keeps (16): the same results as on the CPU evaluator.
session = g.prepare(carry={x: y}, resident=[y], y=y)
runs = []
session.run_steps(runs.append, [{}] * 100, readback=[y])
print("long run", len(runs[0]["steps"]), runs[0]["steps"][-1]["outputs"]["y"]["data"], "step", runs[0]["step"])
session.dispose()
live = [g.prepare(carry={x: y}, y=y) for _ in range(20)]
outs = []
for s in live:
    s.run_steps(lambda r: outs.append(r["outputs"]["y"]["data"]), [{}, {}])
print("sessions", len(outs), outs[0], outs[-1])
for s in live:
    s.dispose()
