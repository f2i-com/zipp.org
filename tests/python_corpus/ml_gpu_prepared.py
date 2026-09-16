# The session flow behind `torch.compile(...).prepare()`, at the graph level:
# one training step prepared once, with the parameters and optimizer state
# carried on the device and the gradients resident; each run feeds only its
# batch; a "sync" downloads weights, moments and gradients; training then
# continues eagerly (here: one graph per step) from the downloaded values.
# Both the resident run and the per-step graphs must give the same float32
# numbers bit for bit, under CPython's reference and Zipp's kernels alike.
from zipp_gpu import Graph

state = [23]


def uniform(lo, hi):
    state[0] = (state[0] * 1103515245 + 12345) % 2147483648
    return lo + (hi - lo) * state[0] / 2147483648

CLASSES, FEATURES, HIDDEN, BATCH, RESIDENT, EAGER = 3, 4, 5, 8, 5, 3
SHAPES = [(FEATURES, HIDDEN), (HIDDEN,), (HIDDEN, CLASSES), (CLASSES,)]


def init(rows, cols, scale):
    return [round(uniform(-scale, scale), 4) for _ in range(rows * cols)]

params = [init(FEATURES, HIDDEN, 0.5), [0.0] * HIDDEN, init(HIDDEN, CLASSES, 0.5), [0.0] * CLASSES]
batches = []
for step in range(RESIDENT + EAGER):
    xs, ys = [], []
    for i in range(BATCH):
        c = (i + step) % CLASSES
        xs.append([round(uniform(-0.3, 0.3) + (1.0 if j % CLASSES == c else 0.0), 4) for j in range(FEATURES)])
        ys.append(c)
    batches.append((xs, ys))


def step_outputs(g, x, y, weights, moments, step, optimizer):
    """Forward, mean cross-entropy, backward, and one optimizer step per parameter tensor."""
    w1, b1, w2, b2 = weights
    pre = x @ w1 + b1
    hidden = pre.relu()
    logits = hidden @ w2 + b2
    delta = logits.cross_entropy_grad(y)
    back = (delta @ w2.T) * pre.positive()
    grads = [x.T @ back, back.sum(0), hidden.T @ delta, delta.sum(0)]
    outputs = {"loss": logits.cross_entropy(y)}
    for i, (p, grad, buffers) in enumerate(zip(weights, grads, moments)):
        outputs["grad%d" % i] = grad
        if optimizer == "adam":
            outputs["p%d" % i], outputs["m%d" % i], outputs["v%d" % i] = g.adam(p, grad, buffers[0], buffers[1], lr=0.05, step=step)
        else:
            # SGD with momentum 0.9 and no dampening: a zero buffer's first
            # update is the gradient itself, which is PyTorch's first step.
            buf = g.momentum_update(buffers[0], grad, 0.9)
            outputs["m%d" % i] = buf
            outputs["p%d" % i] = g.sgd_update(p, buf, 0.1)
    return outputs


def zero_state(optimizer):
    return [tuple([0.0] * len(p) for _ in range(2 if optimizer == "adam" else 1)) for p in params]


def chained(optimizer, weights, moments, first_step, steps):
    """The eager way: one graph per step, fed by the previous step's outputs."""
    losses = []
    for offset, (xs, ys) in enumerate(steps):
        g = Graph()
        ws = [g.tensor(p, shape=s) for p, s in zip(weights, SHAPES)]
        ms = [tuple(g.tensor(m, shape=s) for m in buffers) for buffers, s in zip(moments, SHAPES)]
        seen = []
        g.submit(seen.append, **step_outputs(g, g.tensor(xs), g.tensor(ys), ws, ms, first_step + offset, optimizer))
        out = seen[0]["outputs"]
        losses.append(out["loss"]["data"][0])
        weights = [out["p%d" % i]["data"] for i in range(4)]
        moments = [tuple(out[k % i]["data"] for k in (("m%d", "v%d") if optimizer == "adam" else ("m%d",))) for i in range(4)]
        grads = [out["grad%d" % i]["data"] for i in range(4)]
    return losses, weights, moments, grads


for optimizer in ("adam", "momentum"):
    print("==", optimizer)
    # The prepared step: feeds for the batch, every parameter and buffer carried, gradients resident.
    g = Graph()
    x, y = g.tensor([[0.0] * FEATURES] * BATCH), g.tensor([0] * BATCH)
    weights = [g.tensor(p, shape=s) for p, s in zip(params, SHAPES)]
    moments = [tuple(g.tensor(m, shape=s) for m in buffers) for buffers, s in zip(zero_state(optimizer), SHAPES)]
    outputs = step_outputs(g, x, y, weights, moments, 1, optimizer)
    carry = {}
    for i in range(4):
        carry[weights[i]] = outputs["p%d" % i]
        carry[moments[i][0]] = outputs["m%d" % i]
        if optimizer == "adam":
            carry[moments[i][1]] = outputs["v%d" % i]
    resident = [name for name in outputs if name != "loss"]
    session = g.prepare(feeds={"x": x, "y": y}, carry=carry, resident=resident, **outputs)
    print("prepared", session.backend, "feeds", session.feeds, "outputs", len(session.outputs), "resident", len(session.resident))

    # Two single steps, then the rest of the resident steps in one run: only the loss comes back.
    results = []
    session.run(results.append, x=batches[0][0], y=batches[0][1])
    session.run(results.append, x=batches[1][0], y=batches[1][1])
    session.run_steps(results.append, [{"x": xs, "y": ys} for xs, ys in batches[2:RESIDENT]])
    resident_losses = [s["outputs"]["loss"]["data"][0] for r in results for s in r["steps"]]
    print("resident steps", [s["step"] for r in results for s in r["steps"]], "read back", sorted(results[-1]["outputs"]), "next step", session.step)

    # Sync: download the weights, buffers and last gradients.
    downloaded = []
    session.download(downloaded.append, *resident)
    got = downloaded[0]["outputs"]
    synced_weights = [got["p%d" % i]["data"] for i in range(4)]
    synced_moments = [tuple(got[k % i]["data"] for k in (("m%d", "v%d") if optimizer == "adam" else ("m%d",))) for i in range(4)]
    synced_grads = [got["grad%d" % i]["data"] for i in range(4)]
    print("downloaded", len(got), "tensors", [got["p%d" % i]["shape"] for i in range(4)], "grad shapes", [got["grad%d" % i]["shape"] for i in range(4)])

    # The same resident steps as separate graphs, from the same start.
    losses, weights_c, moments_c, grads_c = chained(optimizer, params, zero_state(optimizer), 1, batches[:RESIDENT])
    print("loss x1000", [round(v * 1000) for v in resident_losses])
    print("resident equals chained, bit for bit:", resident_losses == losses, synced_weights == weights_c, synced_moments == moments_c, synced_grads == grads_c)

    # Continuity: eager steps from the synced values equal the session going on.
    eager_losses, eager_weights, eager_moments, eager_grads = chained(optimizer, synced_weights, synced_moments, RESIDENT + 1, batches[RESIDENT:])
    more = []
    session.run_steps(more.append, [{"x": xs, "y": ys} for xs, ys in batches[RESIDENT:]])
    session_losses = [s["outputs"]["loss"]["data"][0] for s in more[0]["steps"]]
    session.download(downloaded.append, "p0", "p3", "m1", "grad2")
    tail = downloaded[1]["outputs"]
    print("eager continuation x1000", [round(v * 1000) for v in eager_losses])
    print("session continuation equals eager continuation:", session_losses == eager_losses,
          tail["p0"]["data"] == eager_weights[0], tail["p3"]["data"] == eager_weights[3],
          tail["m1"]["data"] == eager_moments[1][0], tail["grad2"]["data"] == eager_grads[2])
    print("weights p3 x1000", [round(v * 1000) for v in eager_weights[3]], "fell", eager_losses[-1] < resident_losses[0])
    session.dispose()
