# The graph pieces torch.compile(training=True) records for SGD with momentum
# (dampening, Nesterov) and for softmax, log_softmax and kept-axis
# reductions, evaluated by zipp_gpu's float32 reference. The optimizer steps
# are compared bit for bit with a float32 transcription of torch.optim's
# loop; the composed gradients against central finite differences.
# Transcendental results are rounded so libm last-bit differences cannot show.
import math
import struct
from zipp_gpu import Graph, execute_locally


def f32(value):
    return struct.unpack("<f", struct.pack("<f", value))[0]


def run(graph, **outputs):
    return {name: out["data"] for name, out in execute_locally(graph.program(**outputs))["outputs"].items()}


# ---- SGD with momentum: buffer = grad on the first step, then
# momentum * buffer + (1 - dampening) * grad; Nesterov steps along grad + momentum * buffer.
LR, MOMENTUM, DAMPENING = 0.05, 0.9, 0.1
# Inputs and scalars enter the graph as float32, so the transcription starts there too.
grads = [[f32(v) for v in row] for row in [[0.5, -1.25, 2.0, 0.125], [-0.75, 0.5, 1.5, -2.0], [1.0, 1.0, -1.0, 0.25], [0.3, -0.6, 0.9, -1.2]]]
LR32, MU32, W32 = f32(LR), f32(MOMENTUM), f32(1 - DAMPENING)
for nesterov in (False, True):
    params, buffer = [1.0, -2.0, 0.5, 4.0], None
    mine, my_buffer = list(params), None
    agree = True
    for step, grad in enumerate(grads, 1):
        g = Graph()
        p, d = g.tensor(params), g.tensor(grad)
        buf = d if buffer is None else g.momentum_update(g.tensor(buffer), d, MOMENTUM, DAMPENING)
        direction = d + buf * MOMENTUM if nesterov else buf
        out = run(g, p=g.sgd_update(p, direction, LR), buf=buf)
        params, buffer = out["p"], out["buf"]
        # torch.optim.SGD's single-tensor loop in float32.
        my_buffer = list(grad) if my_buffer is None else [f32(f32(MU32 * b) + f32(W32 * v)) for b, v in zip(my_buffer, grad)]
        my_direction = [f32(v + f32(MU32 * b)) for v, b in zip(grad, my_buffer)] if nesterov else my_buffer
        mine = [f32(v - f32(LR32 * m)) for v, m in zip(mine, my_direction)]
        agree = agree and mine == params and my_buffer == buffer
    print("nesterov" if nesterov else "momentum", "matches torch loop", agree, "params", [round(v, 6) for v in params], "buffer", [round(v, 6) for v in buffer])

# ---- composed gradients: the pullbacks torch.compile uses, against finite differences.
ROWS, COLS = 2, 3
logits = [[0.5, -1.0, 2.0], [1.5, 0.25, -0.75]]
upstream = [[0.3, -0.2, 0.6], [-0.4, 0.1, 0.5]]


def loss_of(kind, values):
    """sum(upstream * f(values)) for the op under test, in double precision."""
    total = 0.0
    for r in range(ROWS):
        row = values[r]
        m = max(row)
        z = sum(math.exp(v - m) for v in row)
        for c in range(COLS):
            if kind == "softmax":
                f = math.exp(row[c] - m) / z
            elif kind == "log_softmax":
                f = row[c] - m - math.log(z)
            elif kind == "mean_keepdim":
                f = sum(row) / COLS
            else:  # centered: value minus its row mean
                f = row[c] - sum(row) / COLS
            total += upstream[r][c] * f
    return total


for kind in ("softmax", "log_softmax", "mean_keepdim", "centered"):
    g = Graph()
    x, up = g.tensor(logits), g.tensor(upstream)
    if kind == "softmax":
        s = x.softmax()
        grad = s * (up - (up * s).sum(-1, keepdim=True))
    elif kind == "log_softmax":
        ls = x.log_softmax()
        grad = up - ls.exp() * up.sum(-1, keepdim=True)
    elif kind == "mean_keepdim":
        # d/dx of a kept-axis mean broadcast back over the row.
        kept = up.sum(-1, keepdim=True)
        grad = g.full((ROWS, COLS), 1.0 / COLS) * kept
    else:
        # x - x.mean(1, keepdim=True): the upstream minus its row mean spread back.
        grad = up - g.full((ROWS, COLS), 1.0 / COLS) * up.sum(-1, keepdim=True)
    got = run(g, grad=grad)["grad"]
    worst = 0.0
    for r in range(ROWS):
        for c in range(COLS):
            h = 1e-4
            plus = [list(row) for row in logits]
            minus = [list(row) for row in logits]
            plus[r][c] += h
            minus[r][c] -= h
            fd = (loss_of(kind, plus) - loss_of(kind, minus)) / (2 * h)
            worst = max(worst, abs(fd - got[r * COLS + c]))
    print(kind, "grad x1000", [round(v * 1000) for v in got], "within 1e-4 of finite differences", worst < 1e-4)

# ---- keepdim shapes and the reshape a dropped axis needs on the way back.
g = Graph()
m = g.tensor([[1, 2, 3], [4, 5, 6]])
dropped = m.sum(1)
print(dropped.shape, m.sum(1, keepdim=True).shape, m.mean(0, keepdim=True).shape, m.sum(keepdim=True).shape)
out = run(g, back=g.full((2, 3), 1.0) * dropped.reshape(2, 1), whole=g.full((2, 3), 0.5) * m.sum(keepdim=True))
print("back", out["back"], "whole", out["whole"])
