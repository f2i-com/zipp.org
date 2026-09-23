//! `torch.compile` with graph protocol version 3: comparisons as masks,
//! `where`/`masked_fill`, clamp (scalar and tensor bounds), hardtanh, relu6,
//! leaky_relu, maximum/minimum, logical mask algebra, dropout drawn on the
//! device, and the prepared-step rule for constants built inside a step.
//! Compared with PyTorch 2.11 (fixtures/torch_gpu2), with eager Zipp and
//! between per-call and prepared execution; dropout by its statistics and by
//! the gradient carrying the forward's own mask, since its random stream is
//! Zipp's own counter-based one, not PyTorch's.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

fn run(source: &str) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &[], &[], false)?;
            let state = compiled.state_mut();
            state.set_limits(400_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

/// The fixture's cases (see fixtures/torch_gpu2/masks.py).
const MASK_CASES: [&str; 9] = [
    "masks",
    "where",
    "clamp",
    "activations",
    "maxmin",
    "edges",
    "tensorclamp",
    "logical",
    "evaldropout",
];

/// Runs one fixture case three ways and prints its deviations from PyTorch.
const TAIL: &str = r#"
compiled = torch.compile(train_step, training=True)
initial = [p.detach().clone() for p in params]
def reset():
    for p, value in zip(params, initial):
        p.data = value.clone()
        p.grad = None
    optimizer.state.clear()
def worst(actual, wanted):
    assert len(actual) == len(wanted), (len(actual), len(wanted))
    return max(abs(a - b) / (1.0 + abs(b)) for a, b in zip(actual, wanted))
chained, chained_losses = [], []
def took(loss):
    chained_losses.append(loss.item())
    chained.append(state(loss))
for x, y in batches:
    compiled(x, y).submit(took)
dc = max(worst(s, e) for s, e in zip(chained, expected))
reset()
eager = [state(train_step(x, y)) for x, y in batches]
de = max(worst(s, e) for s, e in zip(eager, expected))
reset()
prepared = compiled.prepare(*batches[0])
resident = []
prepared.step(lambda loss: resident.append(loss.item()), *batches[0])
prepared.steps(lambda results: resident.extend(r.item() for r in results), batches[1:])
prepared.sync(lambda p: None)
final = state(torch.tensor(resident[-1]))
prepared.dispose()
dp = worst(final, expected[-1])
print(case, "compiled", repr(dc), "eager", repr(de), "prepared", repr(dp), resident == chained_losses, final == chained[-1])
"#;

#[test]
fn compiled_masks_clamps_and_selections_match_pytorch_per_call_prepared_and_eager() {
    // Golden values from CPU PyTorch 2.11 (`fixtures/torch_gpu2/gen.py`): one
    // state vector (loss, gradients, weights, optimizer buffers) per eager
    // step over four batches. Each case runs as chained compiled calls, as
    // eager Zipp and as one prepared session (one step, then three in one
    // submission, then `sync()`); the prepared losses and final state must
    // equal the chained calls' bit for bit. The "edges" case's first batch
    // sits on every boundary (clamp and hardtanh bounds, relu6's 0 and 6, a
    // leaky_relu/relu 0, a maximum tie), so its first gradients check that
    // clamp passes the gradient at a bound, hardtanh and relu6 do not and a
    // tie splits it. Measured deviations from PyTorch are below 6e-7
    // (relative to 1 + |value|).
    let fixture = include_str!("fixtures/torch_gpu2/masks.py");
    let expected = include_str!("fixtures/torch_gpu2/masks_expected.json");
    for case in MASK_CASES {
        let source = format!("case = {case:?}\n{fixture}\nexpected = {expected}\nexpected = expected[case]\n{TAIL}");
        let out = run(&source).unwrap();
        assert_eq!(out.len(), 1, "{case}: {out:?}");
        let fields: Vec<&str> = out[0].split(' ').collect();
        assert_eq!(fields[0], case);
        for (label, value) in [("compiled", fields[2]), ("eager", fields[4]), ("prepared", fields[6])] {
            let deviation: f64 = value.parse().unwrap();
            assert!(deviation < 1e-6, "{case}: {label} deviates from PyTorch by {deviation}");
        }
        assert_eq!(&fields[7..], ["True", "True"], "{case}: prepared differs from chained compiled calls");
    }
}

#[test]
fn dropout_draws_on_the_device_with_pytorch_statistics_and_a_matching_gradient() {
    // Kept values are exactly x * float32(1 / float32(1 - p)), the keep
    // fraction is 1 - p, the gradient is masked and scaled exactly as the
    // forward was, every call and every prepared step draws a fresh mask, a
    // manual_seed repeats them, eval mode is the identity and draws nothing,
    // dropout2d drops whole channels, and rand_like/bernoulli of graph
    // tensors draw on the device too. A prepared session with dropout is
    // accepted and trains.
    let out = run(r#"
import torch
from torch import nn
import torch.nn.functional as F
def compiled_dropout(p, shape=(64, 32), feature=False):
    w = nn.Parameter(torch.ones(shape[-1]))
    optimizer = torch.optim.SGD([w], lr=0.0)
    def step(x):
        optimizer.zero_grad()
        out = F.dropout2d((x * w).reshape(8, 6, 3, 3), p) if feature else F.dropout(x * w, p)
        out.sum().backward()
        optimizer.step()
        return out
    return w, torch.compile(step, training=True)
# Per call: values are 0 or float32(1 / float32(1 - p)); the gradient carries the same mask and scale.
torch.manual_seed(1)
w, step = compiled_dropout(0.5)
got = []
step(torch.ones(64, 32)).submit(got.append)
out = got[0]
kept = (out != 0).float()
print("p=0.5 values", sorted(set(out.flatten().tolist())), "grad = 2 * kept per column", torch.equal(w.grad, kept.sum(0) * 2.0))
for p in (0.1, 0.25, 0.9):
    w, step = compiled_dropout(p, (256, 256))
    got = []
    step(torch.ones(256, 256)).submit(got.append)
    scale = torch.tensor([1.0]).div(torch.tensor([1.0 - p])).item()
    values = set(got[0].flatten().tolist())
    frac = (got[0] != 0).float().mean().item()
    print("p=%s" % p, values == {0.0, scale}, abs(frac - (1 - p)) < 0.01)
# Fresh masks every call; the same masks again after the same manual_seed.
w, step = compiled_dropout(0.5)
def masks(seed, calls):
    torch.manual_seed(seed)
    seen = []
    for _ in range(calls):
        step(torch.ones(64, 32)).submit(lambda o: seen.append(tuple((o != 0).flatten().tolist())))
    return seen
first, again = masks(7, 3), masks(7, 3)
print("fresh per call", len(set(first)) == 3, "reproducible under manual_seed", first == again)
# The seed is one word of torch's generator; eval mode draws nothing and is the identity.
torch.manual_seed(3); step(torch.ones(64, 32)).submit(lambda o: None); after_train = torch.rand(1).item()
torch.manual_seed(3); expected_next = torch.rand(1).item()
model = nn.Sequential(nn.Linear(8, 8), nn.Dropout(0.5))
model.eval()
x = torch.randn(4, 8)
torch.manual_seed(3); got = []; torch.compile(model)(x).submit(got.append); after_eval = torch.rand(1).item()
print("train call draws a seed", after_train != expected_next, "eval draws none", after_eval == expected_next,
      "eval is identity", torch.allclose(got[0], model(x), atol=1e-6))
# Feature dropout zeroes whole channels.
w, step = compiled_dropout(0.5, (48, 9), feature=True)
got = []
step(torch.ones(48, 9)).submit(got.append)
channels = got[0].reshape(48, 9).tolist()
print("dropout2d whole channels", all(len(set(c)) == 1 and c[0] in (0.0, 2.0) for c in channels), 10 < sum(1 for c in channels if c[0]) < 38)
# rand_like and bernoulli of graph tensors draw on the device.
got = []
torch.compile(lambda x: torch.rand_like(x) + torch.bernoulli(x * 0.0 + 0.3) * 10.0)(torch.zeros(128, 128)).submit(got.append)
u = got[0] - (got[0] >= 10.0).float() * 10.0
print("rand_like in [0, 1)", 0.0 <= u.min().item() and u.max().item() < 1.0, abs(u.mean().item() - 0.5) < 0.01,
      "bernoulli(0.3)", abs((got[0] >= 10.0).float().mean().item() - 0.3) < 0.02)
# Prepared: dropout is accepted, each step draws a fresh mask, the gradient matches its step's mask.
w, step = compiled_dropout(0.5)
prepared = step.prepare(torch.ones(64, 32))
outs = []
prepared.steps(lambda results: outs.extend(results), [(torch.ones(64, 32),)] * 5)
prepared.sync(lambda p: None)
prepared.dispose()
masks_seen = set(tuple((o != 0).flatten().tolist()) for o in outs)
frac = sum((o != 0).float().mean().item() for o in outs) / 5
print("prepared fresh masks", len(masks_seen) == 5, abs(frac - 0.5) < 0.03,
      "synced gradient is the last step's mask", torch.equal(w.grad, (outs[-1] != 0).float().sum(0) * 2.0))
# An nn.Dropout model trains prepared; eval mode stays deterministic.
torch.manual_seed(0)
model = nn.Sequential(nn.Linear(6, 16), nn.ReLU(), nn.Dropout(0.2), nn.Linear(16, 3))
optimizer = torch.optim.Adam(model.parameters(), lr=0.05)
def train(x, y):
    optimizer.zero_grad()
    loss = F.cross_entropy(model(x), y)
    loss.backward()
    optimizer.step()
    return loss
xs = torch.randn(32, 6)
ys = (xs[:, 0] > 0).long() + (xs[:, 1] > 0).long()
session = torch.compile(train, training=True).prepare(xs, ys)
losses = []
session.steps(lambda r: losses.extend(v.item() for v in r), [(xs, ys)] * 40)
session.sync(lambda p: None)
session.dispose()
print("dropout MLP trains", losses[-1] < losses[0] * 0.6, round(losses[0], 3) > 0)
"#)
    .unwrap();
    assert_eq!(
        out,
        [
            "p=0.5 values [0.0, 2.0] grad = 2 * kept per column True",
            "p=0.1 True True",
            "p=0.25 True True",
            "p=0.9 True True",
            "fresh per call True reproducible under manual_seed True",
            "train call draws a seed True eval draws none True eval is identity True",
            "dropout2d whole channels True True",
            "rand_like in [0, 1) True True bernoulli(0.3) True",
            "prepared fresh masks True True synced gradient is the last step's mask True",
            "dropout MLP trains True True",
        ]
    );
}

#[test]
fn prepared_steps_upload_constants_once_and_refuse_values_a_step_would_change() {
    // Constants built inside the step (torch.ones, arange, scalar tensors, a
    // causal tril mask compared and used by masked_fill) are uploaded once
    // and the session equals per-call compilation bit for bit; a bool mask
    // argument is fed every step. A value computed from a parameter outside
    // autograd or one derived from a step argument would be frozen at its
    // prepare() value and is refused with the reason. A CPU random draw is
    // drawn again on the host and fed every step (python_torch_gpu3.rs), so
    // the session draws what per-call compilation draws.
    let out = run(r#"
import torch
from torch import nn
import torch.nn.functional as F
torch.manual_seed(0)
T = 4
xs = [torch.randn(T, 4) for _ in range(3)]
ys = [torch.tensor([0, 1, 2, 3]), torch.tensor([3, 2, 1, 0]), torch.tensor([1, 1, 2, 2])]
ms = [torch.tensor([[True, False, False, True]] * T), torch.tensor([[False, True, True, False]] * T), torch.tensor([[True, True, False, False]] * T)]
def build(kind):
    torch.manual_seed(1)
    layer = nn.Linear(4, 4)
    optimizer = torch.optim.SGD(layer.parameters(), lr=0.1)
    def step(x, y, m=None):
        optimizer.zero_grad()
        h = layer(x)
        if kind == "ones":
            h = h + torch.ones(4)
        elif kind == "arange":
            h = h * (torch.arange(4).float() / 4 + 1)
        elif kind == "scalar tensor":
            h = h * torch.tensor(0.5) + torch.tensor([0.25])
        elif kind == "causal mask":
            scores = (h @ h.T) / 2.0
            causal = torch.tril(torch.ones(T, T))
            att = F.softmax(scores.masked_fill(causal == 0, -1e9), dim=-1)
            h = att @ h
        elif kind == "mask argument":
            h = h.masked_fill(m, 0.0)
        elif kind == "no_grad from a parameter":
            with torch.no_grad():
                s = layer.weight.abs().sum()
            h = h * s
        elif kind == "detach arithmetic":
            h = h * (layer.weight.detach() * 2.0).sum(0)
        elif kind == "one_hot of an argument":
            h = h + F.one_hot(y, 4).float()
        elif kind == "randn":
            h = h + torch.randn(T, 4)
        loss = F.cross_entropy(h, y)
        loss.backward()
        optimizer.step()
        return loss
    return layer, step
for kind in ("ones", "arange", "scalar tensor", "causal mask", "mask argument", "no_grad from a parameter",
             "detach arithmetic", "one_hot of an argument", "randn"):
    args = lambda i: (xs[i], ys[i], ms[i]) if kind == "mask argument" else (xs[i], ys[i])
    layer, step = build(kind)
    eager = [step(*args(i)).item() for i in range(3)]
    layer, step = build(kind)
    compiled = torch.compile(step, training=True)
    chained = []
    for i in range(3):
        compiled(*args(i)).submit(lambda l: chained.append(l.item()))
    layer, step = build(kind)
    try:
        session = torch.compile(step, training=True).prepare(*args(0))
    except NotImplementedError as error:
        print(kind, "->", error)
        continue
    got = []
    session.steps(lambda r: got.extend(v.item() for v in r), [args(i) for i in range(3)])
    session.dispose()
    print(kind, "prepared == per-call", got == chained, "eager within 1e-5", max(abs(a - b) for a, b in zip(got, eager)) < 1e-5)
"#)
    .unwrap();
    let parameter = "prepare(): the step reads a tensor computed from a parameter outside autograd (under no_grad, or from \
                     .detach()/.data) as a graph input; the session would keep its prepare() value at every step. Compute it from \
                     the parameter with gradients enabled (it is then recorded on the device from the resident weights) or outside the step";
    let argument = "prepare(): the step reads a tensor that an eager operation derived from a step argument (a one-hot, a cast, \
                    arithmetic on class targets) as a graph input; the session would keep its prepare() value at every step. \
                    Pass the derived tensor as the argument instead";
    let fine = "prepared == per-call True eager within 1e-5 True";
    assert_eq!(
        out,
        [
            format!("ones {fine}"),
            format!("arange {fine}"),
            format!("scalar tensor {fine}"),
            format!("causal mask {fine}"),
            format!("mask argument {fine}"),
            format!("no_grad from a parameter -> {parameter}"),
            format!("detach arithmetic -> {parameter}"),
            format!("one_hot of an argument -> {argument}"),
            format!("randn {fine}"),
        ]
    );
}

#[test]
fn version_three_graphs_refuse_what_pytorch_refuses_and_the_native_kernels_equal_the_reference() {
    // PyTorch's own errors for a float condition or mask; non-finite fill
    // values and in-place dropout are refused; mask algebra, an eager tensor
    // on the left of a comparison and masked_fill with a 0-d tensor record.
    // A graph is labelled version 3 only when it uses a version-3 operation,
    // and Zipp's tensor-kernel evaluator equals zipp_gpu's pure-Python
    // reference bit for bit on every new operation (the uniform draw's known
    // answers included).
    let out = run(r#"
import torch
from torch import nn
import torch.nn.functional as F
import zipp_gpu
x = torch.tensor([[1.0, -2.0, 0.5], [0.0, 3.0, -1.0]])
def attempt(name, fn):
    try:
        torch.compile(fn)(x).submit(lambda r: print(name, "accepted", r.tolist()))
    except Exception as error:
        print(name, "->", type(error).__name__, error)
attempt("float condition", lambda x: torch.where(x, x, 0.0))
attempt("float mask", lambda x: x.masked_fill(x, 0.0))
attempt("infinite fill", lambda x: x.masked_fill(x > 0, float("-inf")))
attempt("in-place dropout", lambda x: F.dropout(x, 0.5, True, True))
attempt("hardtanh bounds", lambda x: F.hardtanh(x, 1.0, -1.0))
attempt("invert a float", lambda x: ~x)
attempt("bool arithmetic", lambda x: (x > 0).float() * 2 + (~(x > 0)).float() + ((x > 0) & (x < 2)).float() * 10)
attempt("eager left comparison", lambda x: (torch.zeros(2, 3) < x).float() + torch.zeros(3).maximum(x))
attempt("masked_fill value tensor", lambda x: x.masked_fill(x < 0, torch.tensor(9.0)))
attempt("mask shapes", lambda x: x.masked_fill(torch.tensor([True, False, True]), 5.0))
for name, fn in [("clamp", lambda x: x.clamp(0, 1)), ("scale", lambda x: x * 2.0), ("exp", lambda x: x.exp())]:
    pending = torch.compile(fn)(x)
    print(name, "program version", pending._value._capture.graph._version)
# The native evaluator (tensor kernels) and the pure-Python reference agree bit for bit.
g = zipp_gpu.Graph()
a = g.tensor([[0.5, -1.0, 2.0], [2.0, 0.0, -0.5]]); b = g.tensor([2.0, 0.0, -1.0]); c = g.tensor([[1.0], [0.0]])
nan = g.full((3,), 0) / g.full((3,), 0)
outs = {op: getattr(a, op)(b) for op in ("eq", "ne", "lt", "le", "gt", "ge", "maximum", "minimum")}
outs.update(rev=b.maximum(a), where=g.where(c, a, b), pick=g.where(nan, b, b * 2), u=g.uniform((3, 257), 2 ** 31 + 5, 7),
            u2=g.uniform((2, 5), 12345, 3), nanmax=b.maximum(nan).ne(b.maximum(nan)), nanmin=nan.minimum(b).ne(nan.minimum(b)))
program = g.program(**outs)
reference = zipp_gpu.execute_locally(program)["outputs"]
kernels = zipp_gpu._execute_kernels(program, False)["outputs"]
print("kernels == reference", all(kernels[k]["data"] == reference[k]["data"] and kernels[k]["shape"] == reference[k]["shape"] for k in reference))
print("known answers", kernels["u2"]["data"][:3])
"#)
    .unwrap();
    assert_eq!(
        out,
        [
            "float condition -> RuntimeError where expected condition to be a boolean tensor, but got a tensor with dtype Float",
            "float mask -> RuntimeError masked_fill_ only supports boolean masks, but got mask with dtype float",
            "infinite fill -> NotImplementedError torch.compile records finite float32 constants only; -inf cannot be a graph value \
             (for a masked_fill before softmax use a large finite value such as -1e9)",
            "in-place dropout -> NotImplementedError torch.compile does not record in-place dropout on a graph tensor; use inplace=False",
            "hardtanh bounds -> ValueError min_val cannot be greater than max_val",
            "invert a float -> TypeError ~ (bitwise not) of a float compiled graph tensor is not supported; it applies to comparison masks",
            "bool arithmetic accepted [[12.0, 1.0, 12.0], [1.0, 2.0, 1.0]]",
            "eager left comparison accepted [[2.0, 0.0, 1.5], [0.0, 4.0, 0.0]]",
            "masked_fill value tensor accepted [[1.0, 9.0, 0.5], [0.0, 3.0, 9.0]]",
            "mask shapes accepted [[5.0, -2.0, 5.0], [5.0, 3.0, 5.0]]",
            "clamp program version 3",
            "scale program version 1",
            "exp program version 2",
            "kernels == reference True",
            "known answers [0.7625014185905457, 0.13164889812469482, 0.7715473175048828]",
        ]
    );
}
