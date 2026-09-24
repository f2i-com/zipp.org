# ZIPP's GPU path against PyTorch, same models, same batches, same weights.
#
#   py -3.11 bench_vs_torch.py [--zipp PATH] [--zipp-base PATH] [--browser JSON] [--out report.md]
#                              [--quick] [--only NAME,...]
#       The driver (CPython with PyTorch): runs the PyTorch rows in-process
#       (CUDA eager, a captured CUDA graph, torch.compile when it works, CPU
#       eager), then this same file under `zipp py` once per case on the
#       native GPU and, for the small cases, on the CPU evaluator
#       (ZIPP_GPU=0), and prints one markdown table. --zipp-base runs an
#       earlier build right before it (rows `zipp-gpu-base`); --browser merges
#       bench_browser.py's records.
#   zipp py bench_vs_torch.py CASE [CALLS STEPS RUNS]
#       One case on ZIPP: per-call torch.compile (CALLS, default 10),
#       prepared.step (STEPS, 15) and prepared.steps (RUNS of 8, 5); prints
#       `RESULT {json}` lines. ZIPP_GPU=0 runs it on the CPU evaluator.
#
# Weights and batches come from an integer formula (exact in float32 on both
# sides), so the first eight losses of ZIPP's prepared session and of
# PyTorch's eager steps (TF32 off) are compared value for value. Times are
# warm medians in milliseconds; every PyTorch CUDA time includes the
# synchronisation that makes the result available on the host (loss.item()),
# as a ZIPP step's callback receives its loss.
import sys
import os
import time
import json

IS_ZIPP = sys.platform == "zipp"

import torch
from torch import nn
import torch.nn.functional as F

MLPS = {
    "mlp_s": ([784, 256, 10], 64),
    "mlp_m": ([784, 1024, 1024, 10], 256),
    "mlp_l": ([784, 2048, 2048, 10], 1024),
}
MATMULS = {"mm_1024": 1024, "mm_2048": 2048, "mm_4096": 4096}
CHAIN = 4  # matmuls per step in the matmul cases: ((X @ W) @ W ...)
ELEMENTWISE = ("ew_4m", 2048)  # a [2048, 2048] tensor through 36 elementwise ops
EMBEDDING = ("emb", 8192, 128, 256, 16)  # vocab, dim, batch, tokens per row
CASES = list(MLPS) + list(MATMULS) + [ELEMENTWISE[0], EMBEDDING[0], "cnn"]


def median(xs):
    xs = sorted(xs)
    return xs[len(xs) // 2] if xs else float("nan")


def ramp(n, seed, modulus=2003):
    """n float32 values in [-0.5, 0.5): an integer formula, exact on both sides."""
    i = torch.arange(n, dtype=torch.int64)
    return ((i * 7919 + seed * 104729) % modulus).float() / modulus - 0.5


def init_linear(layer, seed):
    with torch.no_grad():
        out_f, in_f = layer.weight.shape
        layer.weight.copy_((ramp(out_f * in_f, seed) * (2.0 / in_f ** 0.5)).reshape(out_f, in_f))
        layer.bias.copy_(ramp(out_f, seed + 1) * 0.1)


def mlp(sizes):
    layers = []
    for a, b in zip(sizes[:-1], sizes[1:]):
        layers += [nn.Linear(a, b), nn.ReLU()]
    model = nn.Sequential(*layers[:-1])
    k = 0
    for m in model:
        if isinstance(m, nn.Linear):
            init_linear(m, 10 + k)
            k += 2
    return model


def mlp_batches(sizes, batch, count=8):
    xs = [(ramp(batch * sizes[0], 100 + i) + 0.5).reshape(batch, sizes[0]) for i in range(count)]
    ys = [(torch.arange(batch, dtype=torch.int64) * 37 + i * 11) % sizes[-1] for i in range(count)]
    return xs, ys


def square(n, seed):
    return (ramp(n * n, seed) * (3.4641016 / n ** 0.5)).reshape(n, n)


def emb_model():
    _, vocab, dim, batch, tokens = EMBEDDING
    table = nn.Embedding(vocab, dim)
    head = nn.Linear(dim, 10)
    with torch.no_grad():
        table.weight.copy_(ramp(vocab * dim, 3).reshape(vocab, dim))
    init_linear(head, 4)
    return table, head


def emb_batches(count=8):
    _, vocab, dim, batch, tokens = EMBEDDING
    toks = [((torch.arange(batch * tokens, dtype=torch.int64) * 7919 + i * 131) % vocab).reshape(batch, tokens) for i in range(count)]
    ys = [(torch.arange(batch, dtype=torch.int64) * 37 + i * 11) % 10 for i in range(count)]
    return toks, ys


def elementwise(y):
    for _ in range(4):
        y = torch.tanh(y * 1.5 + 0.25)
        y = torch.sigmoid(y) * y
        y = torch.exp(-(y * y)) + y
    return y


class Weights(nn.Module):
    """The inference cases: (((X * s) @ W) @ W ...).sum(), or the elementwise chain of X * s."""

    def __init__(self, name):
        super().__init__()
        if name in MATMULS:
            n = MATMULS[name]
            self.X, self.W = nn.Parameter(square(n, 1)), nn.Parameter(square(n, 2))
        else:
            self.X = nn.Parameter((ramp(ELEMENTWISE[1] ** 2, 5) * 4).reshape(ELEMENTWISE[1], ELEMENTWISE[1]))
            self.W = None

    def forward(self, s):
        # s (a fed [1] of ones) enters first, so everything after it is graph
        # work: ZIPP evaluates what does not depend on a feed while recording.
        r = self.X * s
        if self.W is None:
            return elementwise(r).sum()
        for _ in range(CHAIN):
            r = r @ self.W
        return r.sum()


def emit(record):
    print("RESULT " + json.dumps(record), flush=True)


# ---------------------------------------------------------------- ZIPP side
def zipp_case(name):
    wall0 = time.perf_counter()
    # A ZIPP program sees no environment: counts come as arguments.
    calls, reps, runs = [int(a) for a in (sys.argv[2:5] + ["10", "15", "5"][len(sys.argv[2:5]):])]
    rec = {"case": name, "impl": "zipp"}
    if name == "cnn":
        model = nn.Sequential(nn.Conv2d(1, 8, 3, padding=1), nn.ReLU(), nn.Flatten(), nn.Linear(8 * 28 * 28, 10))
        opt = torch.optim.Adam(model.parameters(), lr=1e-3)

        def step(x, y):
            opt.zero_grad()
            loss = F.cross_entropy(model(x), y)
            loss.backward()
            opt.step()
            return loss
        try:
            compiled = torch.compile(step, training=True)
            p = compiled.prepare(torch.rand(32, 1, 28, 28), torch.randint(0, 10, (32,)))
            rec["backend"] = p.backend
            p.dispose()
        except Exception as e:  # the graph protocol has no convolution
            rec["error"] = "%s: %s" % (type(e).__name__, str(e)[:200])
        emit(rec)
        return
    if name in MLPS:
        sizes, batch = MLPS[name]
        model = mlp(sizes)
        opt = torch.optim.Adam(model.parameters(), lr=1e-3)
        xs, ys = mlp_batches(sizes, batch)

        def step(x, y):
            opt.zero_grad()
            loss = F.cross_entropy(model(x), y)
            loss.backward()
            opt.step()
            return loss
        compiled = torch.compile(step, training=True)
        feeds = list(zip(xs, ys))
    elif name == EMBEDDING[0]:
        table, head = emb_model()
        opt = torch.optim.Adam(list(table.parameters()) + list(head.parameters()), lr=1e-3)
        toks, ys = emb_batches()

        def step(tok, y):
            opt.zero_grad()
            logp = F.log_softmax(head(table(tok).mean(1)), 1)
            loss = -logp.gather(1, y.view(-1, 1)).mean()
            loss.backward()
            opt.step()
            return loss
        compiled = torch.compile(step, training=True)
        feeds = list(zip(toks, ys))
    else:
        # Parameters, so the graph reads them from the device (a plain
        # tensor constant would be folded on the CPU while recording).
        model = Weights(name)
        compiled = torch.compile(model)
        feeds = [(torch.ones(1),) for _ in range(8)]
    losses = []
    t = time.perf_counter()
    session = compiled.prepare(*feeds[0])
    rec["prepare_ms"] = (time.perf_counter() - t) * 1000
    rec["backend"] = session.backend
    rec["impl"] = "zipp-gpu" if session.backend == "webgpu" else "zipp-cpu"
    t = time.perf_counter()
    session.step(lambda loss: losses.append(loss.item()), *feeds[0])
    rec["first_step_ms"] = (time.perf_counter() - t) * 1000
    session.steps(lambda out: losses.extend(l.item() for l in out), feeds[1:8])
    rec["losses"] = losses[:8]
    one = []
    for i in range(reps + 2):
        t = time.perf_counter()
        session.step(lambda loss: None, *feeds[i % 8])
        one.append((time.perf_counter() - t) * 1000)
    rec["step_ms"] = median(one[2:])
    eight = []
    for i in range(runs + 1):
        t = time.perf_counter()
        session.steps(lambda out: None, feeds)
        eight.append((time.perf_counter() - t) * 1000 / 8)
    rec["steps8_ms"] = median(eight[1:])
    t = time.perf_counter()
    session.sync(lambda s: None)
    rec["sync_ms"] = (time.perf_counter() - t) * 1000
    session.dispose()
    if calls > 0:
        times = []
        for i in range(calls + 1):
            t = time.perf_counter()
            compiled(*feeds[i % 8]).submit(lambda loss: None)
            times.append((time.perf_counter() - t) * 1000)
        rec["first_call_ms"] = times[0]
        rec["call_ms"] = median(times[1:])
    rec["process_ms"] = (time.perf_counter() - wall0) * 1000
    emit(rec)


# ------------------------------------------------------------- PyTorch side
def torch_case(name, device, quick=False):
    """PyTorch eager on `device`, and on CUDA a captured CUDA graph."""
    cuda = device == "cuda"
    sync = torch.cuda.synchronize if cuda else (lambda: None)
    out = []
    reps = 5 if (quick or not cuda) else 30
    if name == "cnn":
        model = nn.Sequential(nn.Conv2d(1, 8, 3, padding=1), nn.ReLU(), nn.Flatten(), nn.Linear(8 * 28 * 28, 10)).to(device)
        opt = torch.optim.Adam(model.parameters(), lr=1e-3)
        x, y = torch.rand(32, 1, 28, 28, device=device), torch.randint(0, 10, (32,), device=device)

        def step():
            opt.zero_grad()
            loss = F.cross_entropy(model(x), y)
            loss.backward()
            opt.step()
            return loss
        times = []
        for i in range(reps + 3):
            t = time.perf_counter()
            step().item()
            times.append((time.perf_counter() - t) * 1000)
        out.append({"case": name, "impl": "torch-" + device, "step_ms": median(times[3:])})
        return out
    training = name in MLPS or name == EMBEDDING[0]
    if training:
        params, loss_of, xs, ys = build_training(name, device)
        if cuda and name == "mlp_l" and quick:
            reps = 10
        opt = torch.optim.Adam(params, lr=1e-3)

        def step(x, y):
            opt.zero_grad()
            loss = loss_of(x, y)
            loss.backward()
            opt.step()
            return loss
        sync()
        t = time.perf_counter()
        losses = [step(xs[0], ys[0]).item()]
        first = (time.perf_counter() - t) * 1000
        losses += [step(x, y).item() for x, y in zip(xs[1:], ys[1:])]
        one = []
        for i in range(reps + 3):
            t = time.perf_counter()
            step(xs[i % 8], ys[i % 8]).item()
            one.append((time.perf_counter() - t) * 1000)
        eight = []
        for i in range(max(3, reps // 4) + 1):
            t = time.perf_counter()
            ls = [step(x, y) for x, y in zip(xs, ys)]
            torch.stack(ls).tolist()
            eight.append((time.perf_counter() - t) * 1000 / 8)
        rec = {"case": name, "impl": "torch-" + device, "first_step_ms": first, "losses": losses,
               "step_ms": median(one[3:]), "steps8_ms": median(eight[1:])}
        out.append(rec)
        if cuda:
            try:
                out.append(cuda_graph_training(name, reps))
            except Exception as e:
                out.append({"case": name, "impl": "torch-cuda-graph", "error": "%s: %s" % (type(e).__name__, str(e)[:160])})
        return out
    # Inference graphs: matmul chains and the elementwise chain.
    infer = Weights(name).to(device)
    if name in MATMULS:
        n = MATMULS[name]
        flops = 2.0 * n ** 3 * CHAIN
        variants = [("fp32", False)] + ([("tf32", True)] if cuda else [])
        if not cuda and n >= 4096:
            reps = 1
    else:
        flops = None
        variants = [("fp32", False)]
    s = torch.ones(1, device=device)
    for label, tf32 in variants:
      with torch.no_grad():
        torch.backends.cuda.matmul.allow_tf32 = tf32
        sync()
        t = time.perf_counter()
        value = infer(s).item()
        first = (time.perf_counter() - t) * 1000
        one = []
        for i in range(reps + 2):
            t = time.perf_counter()
            infer(s).item()
            one.append((time.perf_counter() - t) * 1000)
        eight = []
        for i in range(max(3, reps // 4) + 1):
            t = time.perf_counter()
            torch.stack([infer(s) for _ in range(8)]).tolist()
            eight.append((time.perf_counter() - t) * 1000 / 8)
        rec = {"case": name, "impl": "torch-%s%s" % (device, "-tf32" if tf32 else ""), "first_step_ms": first,
               "losses": [value], "step_ms": median(one[2:]), "steps8_ms": median(eight[1:])}
        if flops:
            rec["gflops"] = flops / (min(rec["step_ms"], rec["steps8_ms"]) * 1e6)
        out.append(rec)
    torch.backends.cuda.matmul.allow_tf32 = False
    return out


def build_training(name, device):
    """A fresh model on `device`: (parameters, loss function, batches, targets)."""
    if name in MLPS:
        sizes, batch = MLPS[name]
        model = mlp(sizes).to(device)
        xs, ys = mlp_batches(sizes, batch)
        loss_of = lambda x, y: F.cross_entropy(model(x), y)
        params = list(model.parameters())
    else:
        table, head = emb_model()
        table, head = table.to(device), head.to(device)
        xs, ys = emb_batches()
        loss_of = lambda tok, y: -F.log_softmax(head(table(tok).mean(1)), 1).gather(1, y.view(-1, 1)).mean()
        params = list(table.parameters()) + list(head.parameters())
    return params, loss_of, [x.to(device) for x in xs], [y.to(device) for y in ys]


def cuda_graph_training(name, reps):
    """The whole step (forward, backward, capturable Adam) replayed as one CUDA graph."""
    params, loss_of, xs, ys = build_training(name, "cuda")
    opt = torch.optim.Adam(params, lr=1e-3, capturable=True)
    sx, sy = xs[0].clone(), ys[0].clone()
    side = torch.cuda.Stream()
    side.wait_stream(torch.cuda.current_stream())
    with torch.cuda.stream(side):
        for _ in range(3):  # warm up on a side stream, as capture requires
            opt.zero_grad(set_to_none=False)
            loss_of(sx, sy).backward()
            opt.step()
    torch.cuda.current_stream().wait_stream(side)
    graph = torch.cuda.CUDAGraph()
    opt.zero_grad(set_to_none=False)
    with torch.cuda.graph(graph):
        static_loss = loss_of(sx, sy)
        static_loss.backward()
        opt.step()
    one = []
    for i in range(reps + 3):
        t = time.perf_counter()
        sx.copy_(xs[i % 8])
        sy.copy_(ys[i % 8])
        graph.replay()
        static_loss.item()
        one.append((time.perf_counter() - t) * 1000)
    eight = []
    for i in range(max(3, reps // 4) + 1):
        t = time.perf_counter()
        ls = []
        for x, y in zip(xs, ys):
            sx.copy_(x)
            sy.copy_(y)
            graph.replay()
            ls.append(static_loss.clone())
        torch.stack(ls).tolist()
        eight.append((time.perf_counter() - t) * 1000 / 8)
    return {"case": name, "impl": "torch-cuda-graph", "step_ms": median(one[3:]), "steps8_ms": median(eight[1:])}


def torch_compile_probe():
    """Whether torch.compile's default (inductor) backend works here."""
    try:
        f = torch.compile(lambda a: (a * 2).sum())
        f(torch.ones(4, device="cuda")).item()
        return "works"
    except Exception as e:
        return "%s: %s" % (type(e).__name__, str(e).strip().splitlines()[0][:160])


def torch_cudagraphs_backend(name, quick):
    """torch.compile(backend='cudagraphs') of the forward+loss (no triton needed); Adam eager (foreach)."""
    if name not in MLPS:
        return None
    sizes, batch = MLPS[name]
    model = mlp(sizes).cuda()
    opt = torch.optim.Adam(model.parameters(), lr=1e-3)
    xs, ys = mlp_batches(sizes, batch)
    xs = [x.cuda() for x in xs]
    ys = [y.cuda() for y in ys]
    fwd = torch.compile(lambda x, y: F.cross_entropy(model(x), y), backend="cudagraphs")

    def step(x, y):
        opt.zero_grad()
        loss = fwd(x, y)
        loss.backward()
        opt.step()
        return loss
    try:
        for i in range(3):
            step(xs[i], ys[i]).item()
    except Exception as e:
        return {"case": name, "impl": "torch-compile-cudagraphs", "error": "%s: %s" % (type(e).__name__, str(e)[:160])}
    reps = 10 if quick else 30
    one = []
    for i in range(reps + 3):
        t = time.perf_counter()
        step(xs[i % 8], ys[i % 8]).item()
        one.append((time.perf_counter() - t) * 1000)
    return {"case": name, "impl": "torch-compile-cudagraphs", "step_ms": median(one[3:])}


# ------------------------------------------------------------------ driver
def run_zipp(zipp, case, env_extra, timeout=1800, counts=()):
    import subprocess
    env = dict(os.environ)
    env.update(env_extra)
    here = os.path.dirname(os.path.abspath(__file__))
    t = time.perf_counter()
    try:
        done = subprocess.run([zipp, "py", os.path.basename(__file__), case] + [str(c) for c in counts], cwd=here, env=env,
                              capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return [{"case": case, "impl": "zipp-cpu" if env_extra.get("ZIPP_GPU") == "0" else "zipp-gpu", "error": "timeout %ds" % timeout}]
    wall = (time.perf_counter() - t) * 1000
    recs = [json.loads(line[7:]) for line in done.stdout.splitlines() if line.startswith("RESULT ")]
    if not recs:
        recs = [{"case": case, "impl": "zipp", "error": (done.stderr or done.stdout)[-400:]}]
    for r in recs:
        r["wall_ms"] = wall
    return recs


def fmt(v, unit=""):
    if v is None:
        return "-"
    if isinstance(v, str):
        return v
    if v >= 100:
        return "%.0f%s" % (v, unit)
    if v >= 10:
        return "%.1f%s" % (v, unit)
    return "%.2f%s" % (v, unit)


def driver(argv):
    zipp = None
    base = None
    out_path = None
    quick = "--quick" in argv
    only = None
    browser_json = None
    for i, a in enumerate(argv):
        if a == "--zipp":
            zipp = argv[i + 1]
        if a == "--out":
            out_path = argv[i + 1]
        if a == "--only":
            only = argv[i + 1].split(",")
        if a == "--browser":
            browser_json = argv[i + 1]
        if a == "--zipp-base":
            base = argv[i + 1]
    cases = only or CASES
    results = []
    info = {"torch": torch.__version__, "cuda": torch.version.cuda, "gpu": torch.cuda.get_device_name(0) if torch.cuda.is_available() else None,
            "compile": None}
    if torch.cuda.is_available():
        t = time.perf_counter()
        torch.zeros(1, device="cuda").sum().item()
        info["cuda_init_ms"] = (time.perf_counter() - t) * 1000
        info["compile"] = torch_compile_probe()
    for case in cases:
        if torch.cuda.is_available():
            results += torch_case(case, "cuda", quick)
            r = torch_cudagraphs_backend(case, quick)
            if r:
                results.append(r)
        if case not in ("mm_4096",) or not quick:
            results += torch_case(case, "cpu", quick)
        if base:
            # An earlier build, run right before this one: a before/after row pair.
            for r in run_zipp(base, case, {}):
                r["impl"] += "-base"
                results.append(r)
        if zipp:
            results += run_zipp(zipp, case, {})
            if case == "mlp_s":  # the CPU evaluator runs graphs in pure Python
                results += run_zipp(zipp, case, {"ZIPP_GPU": "0"}, timeout=900, counts=(2, 3, 1))
        print("done", case, file=sys.stderr, flush=True)
    if browser_json and os.path.exists(browser_json):
        results += json.load(open(browser_json))
    text = report(results, info)
    print(text)
    if out_path:
        with open(out_path, "w", encoding="utf-8") as f:
            f.write(text)
        with open(out_path + ".json", "w", encoding="utf-8") as f:
            json.dump({"info": info, "results": results}, f, indent=1)


def report(results, info):
    lines = ["PyTorch %s (CUDA %s) on %s; torch.compile (inductor): %s; CUDA init %s ms" %
             (info["torch"], info["cuda"], info["gpu"], info["compile"], fmt(info.get("cuda_init_ms"))), ""]
    lines.append("| Case | Implementation | step (ms) | 8 per run, per step (ms) | per-call compiled (ms) | first step (ms) | GFLOP/s | loss dev vs torch-cuda |")
    lines.append("|---|---|---|---|---|---|---|---|")
    ref, based = {}, {}
    for r in results:
        if r.get("impl") == "torch-cuda" and r.get("losses"):
            ref[r["case"]] = r["losses"]
        if r.get("impl") == "zipp-gpu-base" and r.get("losses"):
            based[r["case"]] = r["losses"]
    for r in results:
        if "error" in r:
            lines.append("| %s | %s | %s | | | | | |" % (r["case"], r["impl"], r["error"].replace("|", "/").replace("\n", " ")[:120]))
            continue
        dev = None
        if r.get("losses") and r["case"] in ref and r["impl"] != "torch-cuda":
            a, b = r["losses"], ref[r["case"]]
            dev = max(abs(x - y) / max(1.0, abs(y)) for x, y in zip(a, b))
        gf = r.get("gflops")
        if gf is None and r["case"] in MATMULS and r.get("step_ms"):
            n = MATMULS[r["case"]]
            gf = 2.0 * n ** 3 * CHAIN / (min(r["step_ms"], r.get("steps8_ms") or r["step_ms"]) * 1e6)
        first = r.get("first_step_ms")
        if r["impl"].startswith("zipp") and r.get("prepare_ms") is not None:
            first = "%s + %s" % (fmt(r["prepare_ms"]), fmt(r["first_step_ms"]))
        note = "-" if dev is None else "%.1e" % dev
        if r["impl"] == "zipp-gpu" and r["case"] in based:
            # The same eight losses, compared as float32 bits (repr of a float32 widened exactly).
            note += "; bits %s base" % ("=" if r["losses"] == based[r["case"]] else "DIFFER from")
        lines.append("| %s | %s | %s | %s | %s | %s | %s | %s |" % (
            r["case"], r["impl"], fmt(r.get("step_ms")), fmt(r.get("steps8_ms")), fmt(r.get("call_ms")),
            first if isinstance(first, str) else fmt(first), fmt(gf), note))
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    if IS_ZIPP:
        zipp_case(sys.argv[1] if len(sys.argv) > 1 else "mlp_s")
    else:
        driver(sys.argv[1:])
