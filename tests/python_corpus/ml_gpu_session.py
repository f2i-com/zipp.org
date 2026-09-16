# A training loop through a prepared session: `Graph.prepare` validates one
# MLP training step once, keeps the parameters and Adam moments as carried
# tensors, and each `run` feeds only that step's batch. The same steps as
# separate graphs (`ml_gpu_training.py`'s way) must give the same float32
# numbers, bit for bit. Without a GPU host the float32 reference runs both.
from zipp_gpu import Graph, GraphError, ComputeError

state = [11]


def uniform(lo, hi):
    state[0] = (state[0] * 1103515245 + 12345) % 2147483648
    return lo + (hi - lo) * state[0] / 2147483648

CLASSES, FEATURES, HIDDEN, BATCH, STEPS = 3, 4, 6, 9, 8
SHAPES = [(FEATURES, HIDDEN), (HIDDEN,), (HIDDEN, CLASSES), (CLASSES,)]


def init(rows, cols, scale):
    return [round(uniform(-scale, scale), 4) for _ in range(rows * cols)]

params = [init(FEATURES, HIDDEN, 0.5), [0.0] * HIDDEN, init(HIDDEN, CLASSES, 0.5), [0.0] * CLASSES]
batches = []
for _ in range(STEPS):
    xs, ys = [], []
    for i in range(BATCH):
        c = i % CLASSES
        xs.append([round(uniform(-0.3, 0.3) + (1.0 if j % CLASSES == c else 0.0), 4) for j in range(FEATURES)])
        ys.append(c)
    batches.append((xs, ys))


def step_outputs(g, x, y, weights, moments, step):
    w1, b1, w2, b2 = weights
    pre = x @ w1 + b1
    hidden = pre.relu()
    logits = hidden @ w2 + b2
    delta = logits.cross_entropy_grad(y)
    back = (delta @ w2.T) * pre.positive()
    grads = [x.T @ back, back.sum(0), hidden.T @ delta, delta.sum(0)]
    outputs = {"loss": logits.cross_entropy(y), "predicted": logits.softmax()}
    for i, (p, grad, (m, v)) in enumerate(zip(weights, grads, moments)):
        outputs["p%d" % i], outputs["m%d" % i], outputs["v%d" % i] = g.adam(p, grad, m, v, lr=0.05, step=step)
    return outputs


# ---- the reference: one graph per step, fed by the previous step's outputs ----------------
flat, moments = list(params), [([0.0] * len(p), [0.0] * len(p)) for p in params]
chained_losses = []
for step, (xs, ys) in enumerate(batches, 1):
    g = Graph()
    weights = [g.tensor(p, shape=s) for p, s in zip(flat, SHAPES)]
    ms = [(g.tensor(m, shape=s), g.tensor(v, shape=s)) for (m, v), s in zip(moments, SHAPES)]
    seen = []
    g.submit(seen.append, **step_outputs(g, g.tensor(xs), g.tensor(ys), weights, ms, step))
    out = seen[0]["outputs"]
    chained_losses.append(out["loss"]["data"][0])
    flat = [out["p%d" % i]["data"] for i in range(4)]
    moments = [(out["m%d" % i]["data"], out["v%d" % i]["data"]) for i in range(4)]
    chained_predicted = out["predicted"]["data"]

# ---- the session: the step prepared once, parameters and moments carried ------------------
g = Graph()
x, y = g.tensor([[0.0] * FEATURES] * BATCH), g.tensor([0] * BATCH)
weights = [g.tensor(p, shape=s) for p, s in zip(params, SHAPES)]
ms = [(g.tensor([0.0] * len(p), shape=s), g.tensor([0.0] * len(p), shape=s)) for p, s in zip(params, SHAPES)]
outputs = step_outputs(g, x, y, weights, ms, 1)
carry = {}
for i in range(4):
    carry[weights[i]] = outputs["p%d" % i]
    carry[ms[i][0]] = outputs["m%d" % i]
    carry[ms[i][1]] = outputs["v%d" % i]
resident = [outputs[k] for k in outputs if k not in ("loss", "predicted")]
ready = []
session = g.prepare(feeds={"x": x, "y": y}, carry=carry, resident=resident, on_ready=ready.append, **outputs)
print("session", session.backend, "ready", len(ready), "feeds", session.feeds, "resident", len(session.resident), "step", session.step)
program = session._program
print("fed inputs without data", [n["id"] for n in program["nodes"] if n["op"] == "input" and "data" not in n],
      "carried", sum(1 for n in program["nodes"] if "carry" in n), "nodes", len(program["nodes"]))

results = []
# Three steps one run at a time (the loss is all that comes back)...
for xs, ys in batches[:3]:
    session.run(results.append, x=xs, y=ys)
print("per-run outputs", sorted(results[0]["outputs"]), "steps", [r["steps"][0]["step"] for r in results])
# ...then the remaining five in one run, with the predictions of each step.
session.run_steps(results.append, [{"x": xs, "y": ys} for xs, ys in batches[3:]], readback=["loss", "predicted"])
batched = results[3]
print("batched steps", [s["step"] for s in batched["steps"]], "next step", session.step, batched["step"])
session_losses = [r["outputs"]["loss"]["data"][0] for r in results[:3]] + [s["outputs"]["loss"]["data"][0] for s in batched["steps"]]
print("loss x1000", [round(v * 1000) for v in session_losses])
print("session equals chained, bit for bit:", session_losses == chained_losses, batched["outputs"]["predicted"]["data"] == chained_predicted)

downloaded = []
session.download(downloaded.append, "p0", "p1", "p2", "p3", "m0", "v3")
got = downloaded[0]["outputs"]
print("downloaded", sorted(got), [got["p%d" % i]["shape"] for i in range(4)])
print("weights equal chained:", all(got["p%d" % i]["data"] == flat[i] for i in range(4)), got["m0"]["data"] == moments[0][0], got["v3"]["data"] == moments[3][1])
correct = sum(1 for i in range(BATCH) if max(range(CLASSES), key=lambda c: chained_predicted[i * CLASSES + c]) == batches[-1][1][i])
print("fell", session_losses[-1] < session_losses[0] * 0.8, "correct", correct, "of", BATCH)

# Restarting the step count re-applies step 1's bias correction: a different update.
restart = []
session.run(restart.append, x=batches[0][0], y=batches[0][1], step=1, readback=["loss"])
print("restarted at", restart[0]["steps"][0]["step"], "next", session.step)

# What a session refuses.
for label, call in [
        ("unknown feed", lambda: session.run(print, z=batches[0][0])),
        ("wrong length", lambda: session.run(print, x=batches[0][0][1:], y=batches[0][1])),
        ("class index out of range", lambda: session.run(print, x=batches[0][0], y=[5] * BATCH)),
        ("missing feed", lambda: session.run(print, x=batches[0][0])),
        ("download of a non-resident", lambda: session.download(print, "loss")),
        ("carry into the targets", lambda: g.prepare(feeds={"x": x}, carry={y: outputs["p0"]}, **outputs)),
        ("carry shape", lambda: g.prepare(carry={weights[0]: outputs["p1"]}, **outputs)),
        ("resident that is not an output", lambda: g.prepare(resident=[x], **outputs)),
        ("a backend the reference is not", lambda: g.prepare(backend="webgpu", **outputs))]:
    try:
        call()
        print(label, "accepted")
    except GraphError as error:
        print(label, "-> GraphError:", error)
    except ComputeError as error:
        print(label, "-> ComputeError:", error.code)
session.dispose()
try:
    session.run(print, x=batches[0][0], y=batches[0][1])
except ComputeError as error:
    print("after dispose ->", error.code)
