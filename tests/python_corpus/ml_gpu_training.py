# A small MLP classifier trained with zipp_gpu graphs: every step's forward
# pass, mean cross-entropy, backward pass and Adam update is one graph, and
# the updated parameters and moments feed the next step. Without a GPU host
# the float32 reference evaluates it (CLI and CPython alike).
from zipp_gpu import Graph

state = [7]


def uniform(lo, hi):
    # A fixed LCG, so the data is identical under every Python implementation.
    state[0] = (state[0] * 1103515245 + 12345) % 2147483648
    return lo + (hi - lo) * state[0] / 2147483648

CLASSES, FEATURES, HIDDEN, BATCH = 3, 4, 6, 9
xs, ys = [], []
for i in range(BATCH):
    c = i % CLASSES
    xs.append([round(uniform(-0.3, 0.3) + (1.0 if j % CLASSES == c else 0.0), 4) for j in range(FEATURES)])
    ys.append(c)


def init(rows, cols, scale):
    return [[round(uniform(-scale, scale), 4) for _ in range(cols)] for _ in range(rows)]


params = [init(FEATURES, HIDDEN, 0.5), [0.0] * HIDDEN, init(HIDDEN, CLASSES, 0.5), [0.0] * CLASSES]
moments = None


def step_graph(step, activation):
    g = Graph()
    x, y = g.tensor(xs), g.tensor(ys)
    w1, b1, w2, b2 = [g.tensor(p) for p in params]
    pre = x @ w1 + b1
    hidden = pre.gelu() if activation == "gelu" else pre.relu()
    logits = hidden @ w2 + b2
    delta = logits.cross_entropy_grad(y)
    back = delta @ w2.T
    back = back * (pre.gelu_grad() if activation == "gelu" else pre.positive())
    grads = [x.T @ back, back.sum(0), hidden.T @ delta, delta.sum(0)]
    outputs = {"loss": logits.cross_entropy(y), "predicted": logits.softmax()}
    for i, (p, grad) in enumerate(zip([w1, b1, w2, b2], grads)):
        if moments is None:
            m, v = g.zeros(p.shape), g.zeros(p.shape)
        else:
            m, v = g.tensor(moments[0][i], shape=p.shape), g.tensor(moments[1][i], shape=p.shape)
        new_p, new_m, new_v = g.adam(p, grad, m, v, lr=0.05, step=step)
        outputs["p%d" % i], outputs["m%d" % i], outputs["v%d" % i] = new_p, new_m, new_v
    print("step", step, "nodes", len(g.program(**outputs)["nodes"]), "outputs", len(outputs))
    return g, outputs


def rows(flat, shape):
    return [flat[r * shape[1]:(r + 1) * shape[1]] for r in range(shape[0])] if len(shape) == 2 else flat


for activation in ["relu", "gelu"]:
    params = [init(FEATURES, HIDDEN, 0.5), [0.0] * HIDDEN, init(HIDDEN, CLASSES, 0.5), [0.0] * CLASSES]
    moments = None
    losses = []
    for step in range(1, 9):
        graph, outputs = step_graph(step, activation)
        seen = []
        graph.submit(seen.append, **outputs)
        out = seen[0]["outputs"]
        losses.append(out["loss"]["data"][0])
        shapes = [(FEATURES, HIDDEN), (HIDDEN,), (HIDDEN, CLASSES), (CLASSES,)]
        params = [rows(out["p%d" % i]["data"], shapes[i]) for i in range(4)]
        moments = ([out["m%d" % i]["data"] for i in range(4)], [out["v%d" % i]["data"] for i in range(4)])
        probs = out["predicted"]["data"]
    correct = sum(1 for i in range(BATCH) if max(range(CLASSES), key=lambda c: probs[i * CLASSES + c]) == ys[i])
    print(activation, "loss x1000", [round(v * 1000) for v in losses], "fell", losses[-1] < losses[0] * 0.8, "correct", correct, "of", BATCH)
