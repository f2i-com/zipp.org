# The Zipp side of the torch.nn parity fixtures (see gen.py). A generated
# parity_*.py (CASES, BUILDERS) follows this prelude; run_cases() rebuilds
# every case, loads the PyTorch state_dict and compares outputs, input and
# parameter gradients and buffers.
import math
import torch
import torch.nn as nn
import torch.nn.functional as F
try:
    from torch.nn.utils.rnn import pack_padded_sequence, pad_packed_sequence, pad_sequence, pack_sequence
except ImportError:
    # Lets the fixtures report per case against a build without these.
    pack_padded_sequence = pad_packed_sequence = pad_sequence = pack_sequence = None

true, false, null = True, False, None
NaN = float("nan")
Infinity = float("inf")
DTYPES = {"float32": torch.float32, "float64": torch.float64, "int64": torch.int64, "int32": torch.int32, "bool": torch.bool}


def numbers(text):
    return [float(v) for v in text.split(",")] if text else []


def tensor_of(j):
    dtype = DTYPES[j["dtype"]]
    values = numbers(j["data"])
    if not dtype.is_floating_point:
        values = [int(v) for v in values]
    t = torch.tensor(values, dtype=dtype)
    return t.reshape(*j["shape"])


def flatten(out, acc):
    if isinstance(out, torch.Tensor):
        acc.append(out)
    elif isinstance(out, (tuple, list)):
        for o in out:
            flatten(o, acc)
    return acc


def mismatch(actual, j, tol, what):
    """None when `actual` matches the fixture tensor `j`, else a description."""
    if actual is None:
        return what + ": missing"
    if list(actual.shape) != list(j["shape"]):
        return "%s: shape %s != %s" % (what, list(actual.shape), j["shape"])
    got = actual.detach().reshape(-1).tolist()
    want = numbers(j["data"])
    floating = j["dtype"].startswith("float")
    worst = 0.0
    for a, b in zip(got, want):
        a, b = float(a), float(b)
        if not floating:
            if a != b:
                return "%s: %s != %s" % (what, got, want)
            continue
        if b != b or a != a:
            if not (a != a and b != b):
                return "%s: nan mismatch %s vs %s" % (what, a, b)
            continue
        if math.isinf(b) or math.isinf(a):
            if a != b:
                return "%s: inf mismatch %s vs %s" % (what, a, b)
            continue
        err = abs(a - b) - (tol + 1e-4 * abs(b))
        worst = max(worst, err)
    if worst > 0:
        return "%s: exceeds tolerance by %g" % (what, worst)
    return None


def run_case(rec, builders):
    ctor, expr = builders
    m = ctor()
    if m is not None:
        m.load_state_dict({k: tensor_of(v) for k, v in rec["state"].items()})
        m.train(rec["train"])
    xs = []
    for j in rec["inputs"]:
        t = tensor_of(j)
        if j["grad"]:
            t.requires_grad_(True)
        xs.append(t)
    outs = flatten(expr(m, *xs), [])
    tol = rec["tol"]
    errors = []
    if len(outs) != len(rec["outputs"]):
        return ["%d outputs, expected %d" % (len(outs), len(rec["outputs"]))]
    loss = None
    for i, (o, j, w) in enumerate(zip(outs, rec["outputs"], rec["upstream"])):
        e = mismatch(o, j, tol, "output %d" % i)
        if e:
            errors.append(e)
        if w is not None:
            if not o.requires_grad:
                errors.append("output %d does not require grad" % i)
                continue
            term = (o * torch.tensor(numbers(w), dtype=o.dtype).reshape(*o.shape)).sum()
            loss = term if loss is None else loss + term
    if loss is not None:
        loss.backward()
    for i, (x, g) in enumerate(zip(xs, rec["input_grads"])):
        if g is not None:
            e = mismatch(x.grad, g, tol, "input %d grad" % i)
            if e:
                errors.append(e)
    if m is not None:
        params = dict(m.named_parameters())
        for k, g in rec["param_grads"].items():
            e = mismatch(params[k].grad if k in params else None, g, tol, "grad " + k)
            if e:
                errors.append(e)
        buffers = dict(m.named_buffers())
        for k, b in rec["buffers"].items():
            e = mismatch(buffers.get(k), b, tol, "buffer " + k)
            if e:
                errors.append(e)
    return errors


def run_cases(group):
    passed = 0
    for rec, builders in zip(CASES, BUILDERS):
        try:
            errors = run_case(rec, builders)
        except Exception as ex:
            errors = ["raised %s: %s" % (type(ex).__name__, ex)]
        if errors:
            print("FAIL %s: %s" % (rec["name"], "; ".join(errors)))
        else:
            passed += 1
    print("parity %s: %d/%d passed" % (group, passed, len(CASES)))
