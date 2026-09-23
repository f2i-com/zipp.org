//! `torch.compile` with graph protocol version 4 (element selection) and CPU
//! random draws inside prepared steps.
//!
//! Selection: slicing (basic, strided, integer, last-token), split/chunk/
//! unbind, narrow, flip, cat/stack, index_select (list and tensor indices,
//! F.embedding), gather and x[torch.arange(n), idx], eager slices and cats of
//! parameters, and a small transformer head, compared with PyTorch 2.11
//! (fixtures/torch_gpu3), with eager Zipp and between per-call and prepared
//! execution. Random draws: torch.randn/rand/randint/normal/bernoulli and the
//! *_like forms inside a prepared step are drawn afresh on the host every step
//! from torch's generator and fed, so a session draws what the same eager
//! steps draw.
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

/// The fixture's cases (see fixtures/torch_gpu3/selection.py).
const SELECTION_CASES: [&str; 5] = ["transformer", "gather", "cat", "strided", "params"];

/// Runs one fixture case three ways and prints its deviations from PyTorch.
const TAIL: &str = r#"
version = torch.compile(train_step, training=True)(*batches[0])._value._capture.graph._version
for p in params:
    p.grad = None
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
print(case, "compiled", repr(dc), "eager", repr(de), "prepared", repr(dp), resident == chained_losses, final == chained[-1], "version", version)
"#;

#[test]
fn compiled_selection_matches_pytorch_per_call_prepared_and_eager() {
    // Golden values from CPU PyTorch 2.11 (`fixtures/torch_gpu3/gen.py`): one
    // state vector (loss, gradients, weights, optimizer buffers) per eager
    // step over four batches. Each case runs as chained compiled calls, as
    // eager Zipp and as one prepared session (one step, then three in one
    // submission, then `sync()`); the prepared losses and final state must
    // equal the chained calls' bit for bit, and every case records a
    // version-4 graph. The transformer case feeds its token argument to an
    // embedding lookup every step and slices a positional table (a
    // parameter) eagerly; "params" also concatenates parameters eagerly and
    // looks a parameter's rows up by the targets. Measured deviations from
    // PyTorch are below 1.1e-7 (relative to 1 + |value|).
    let fixture = include_str!("fixtures/torch_gpu3/selection.py");
    let expected = include_str!("fixtures/torch_gpu3/selection_expected.json");
    for case in SELECTION_CASES {
        let source = format!("case = {case:?}\n{fixture}\nexpected = {expected}\nexpected = expected[case]\n{TAIL}");
        let out = run(&source).unwrap();
        assert_eq!(out.len(), 1, "{case}: {out:?}");
        let fields: Vec<&str> = out[0].split(' ').collect();
        assert_eq!(fields[0], case);
        for (label, value) in [("compiled", fields[2]), ("eager", fields[4]), ("prepared", fields[6])] {
            let deviation: f64 = value.parse().unwrap();
            assert!(deviation < 1e-6, "{case}: {label} deviates from PyTorch by {deviation}");
        }
        assert_eq!(&fields[7..], ["True", "True", "version", "4"], "{case}: prepared differs from chained compiled calls");
    }
}

#[test]
fn prepared_random_draws_are_fed_per_step_and_reproduce_eager_draws() {
    // Five steps under the same manual_seed three ways: eager, per-call
    // compiled and one prepared session (one step, then four in one
    // submission). Each step returns what it drew (a VAE's randn_like noise,
    // input noise from randn + rand and a torch.normal of a graph mean, a
    // randint of negative samples looked up in a table, a bernoulli mask, a
    // randint used as class targets): the prepared draws equal the eager ones
    // bit for bit, the prepared session equals per-call compilation bit for
    // bit in its results and final weights, the weights stay within 3e-8 of
    // eager training, and afterwards torch's generator stands where the eager
    // steps left it (prepare() itself draws from a copy of it).
    let out = run(r#"
import torch
from torch import nn
import torch.nn.functional as F
def build(kind):
    torch.manual_seed(11)
    enc = nn.Linear(4, 6)
    dec = nn.Linear(3, 4)
    emb = nn.Embedding(10, 4)
    params = list(enc.parameters()) + (list(dec.parameters()) if kind == "vae" else []) + (list(emb.parameters()) if kind == "negatives" else [])
    opt = torch.optim.Adam(params, lr=0.01)
    def step(x, y):
        opt.zero_grad()
        if kind == "vae":
            h = enc(x)
            mu, logvar = h.chunk(2, dim=1)
            std = torch.exp(0.5 * logvar)
            eps = torch.randn_like(std)
            z = mu + eps * std
            recon = dec(z)
            kl = -0.5 * (1 + logvar - mu * mu - logvar.exp()).sum(1).mean()
            loss = ((recon - x) ** 2).mean() + kl * 0.1 + F.cross_entropy(recon[:, :3], y) * 0.01
            out = eps + z * 0.0
        elif kind == "noise":
            noisy = x + 0.1 * torch.randn(x.shape[0], 4) + torch.rand(4) * 0.05
            h = enc(noisy)
            loss = F.cross_entropy(h[:, :3], y) + (torch.normal(h[:, 3:], 0.5) ** 2).mean() * 0.01
            out = noisy
        elif kind == "negatives":
            neg = torch.randint(0, 10, (x.shape[0],))
            h = enc(x)[:, :4]
            loss = -F.logsigmoid((h * emb(y)).sum(1)).mean() - F.logsigmoid(-(h * emb(neg)).sum(1)).mean()
            out = torch.arange(10.0).index_select(0, neg) + h.sum() * 0.0
        elif kind == "bernoulli":
            keep = torch.bernoulli(torch.full((x.shape[0], 6), 0.7))
            h = enc(x) * keep
            loss = F.cross_entropy(h[:, :3] + h[:, 3:], y)
            out = h * 0.0 + keep
        else:
            t = torch.randint(0, 3, (x.shape[0],))
            h = enc(x)
            loss = F.cross_entropy(h[:, 1:4], t) + F.cross_entropy(h[:, :3], y)
            out = torch.arange(3.0).index_select(0, t) + h.sum() * 0.0
        loss.backward()
        opt.step()
        return out * 1.0
    return params, step
torch.manual_seed(5)
xs = [torch.randn(5, 4) for _ in range(5)]
ys = [torch.randint(0, 3, (5,)) for _ in range(5)]
for kind in ("vae", "noise", "negatives", "bernoulli", "targets"):
    params, step = build(kind)
    torch.manual_seed(100)
    eager = [step(xs[i], ys[i]) for i in range(5)]
    eager_params = [p.detach().clone() for p in params]
    after_eager = torch.rand(1).item()
    params, step = build(kind)
    compiled = torch.compile(step, training=True)
    torch.manual_seed(100)
    chained = []
    for i in range(5):
        compiled(xs[i], ys[i]).submit(chained.append)
    chained_params = [p.detach().clone() for p in params]
    after_chained = torch.rand(1).item()
    params, step = build(kind)
    torch.manual_seed(100)
    prepared = torch.compile(step, training=True).prepare(xs[0], ys[0])
    got = []
    prepared.step(got.append, xs[0], ys[0])
    prepared.steps(got.extend, [(xs[i], ys[i]) for i in range(1, 5)])
    prepared.sync(lambda p: None)
    prepared.dispose()
    after_prepared = torch.rand(1).item()
    prepared_params = [p.detach().clone() for p in params]
    print(kind, "draws == eager", all(torch.equal(a, b) for a, b in zip(got, eager)), len(set(tuple(g.flatten().tolist()) for g in got)) == 5,
          "prepared == per-call", all(torch.equal(a, b) for a, b in zip(got, chained)), all(torch.equal(a, b) for a, b in zip(prepared_params, chained_params)),
          "eager weights within 3e-8", max((a - b).abs().max().item() for a, b in zip(prepared_params, eager_params)) <= 3e-8,
          "stream", after_eager == after_chained == after_prepared)
"#)
    .unwrap();
    let fine = "draws == eager True True prepared == per-call True True eager weights within 3e-8 True stream True";
    assert_eq!(
        out,
        [
            format!("vae {fine}"),
            format!("noise {fine}"),
            format!("negatives {fine}"),
            format!("bernoulli {fine}"),
            format!("targets {fine}"),
        ]
    );
}

#[test]
fn prepared_inference_feeds_token_indices_and_slices_the_last_position() {
    // An inference model (nn.Embedding, the last position of each sequence,
    // a linear head) prepared once and fed four token batches in one run:
    // the same as per-call compilation bit for bit and as eager Zipp to 1e-6.
    let out = run(r#"
import torch
from torch import nn
torch.manual_seed(3)
class Head(nn.Module):
    def __init__(self):
        super().__init__()
        self.emb = nn.Embedding(12, 6)
        self.out = nn.Linear(6, 4)
    def forward(self, idx):
        h = self.emb(idx)
        return self.out(h[:, -1] + h[:, 0] * 0.5)
model = Head()
tokens = [torch.randint(0, 12, (3, 5)) for _ in range(4)]
compiled = torch.compile(model)
session = compiled.prepare(tokens[0])
outs = []
session.steps(lambda r: outs.extend(r), [(t,) for t in tokens])
session.dispose()
per_call = []
for t in tokens:
    compiled(t).submit(per_call.append)
print("prepared == per-call", all(torch.equal(a, b) for a, b in zip(outs, per_call)),
      "eager", max((a - model(t)).abs().max().item() for a, t in zip(outs, tokens)) < 1e-6,
      "distinct", len(set(tuple(o.flatten().tolist()) for o in outs)) == 4)
try:
    session = compiled.prepare(tokens[0])
    session.step(lambda r: None, torch.tensor([[0, 1, 2, 3, 12]] * 3))
except Exception as error:
    print(type(error).__name__, error)
"#)
    .unwrap();
    assert_eq!(
        out,
        [
            "prepared == per-call True eager True distinct True",
            "GraphError i0 must hold integer indices in [0, 12)",
        ]
    );
}

#[test]
fn what_selection_and_prepared_draws_still_refuse_is_named() {
    // Negative steps are PyTorch's own error; a boolean mask, an index
    // computed on the device, a parameter indexed by a tensor, two tensor
    // indices, negative index values, an empty slice, an integer mixed with a
    // tensor index and multinomial of a graph tensor cannot be expressed. In a
    // prepared step an in-place draw, torch.poisson, a draw read in Python, an
    // integer draw only used in eager arithmetic or Python, multinomial of a
    // parameter and a draw that requires grad are refused, naming why; a
    // multinomial of constant probabilities and randint class targets record.
    let out = run(r#"
import torch
from torch import nn
import torch.nn.functional as F
torch.manual_seed(0)
model = nn.Linear(3, 4)
optimizer = torch.optim.SGD(model.parameters(), lr=0.1)
emb = nn.Embedding(5, 4)
x = torch.tensor([[1.0, 2.0, 0.5], [3.0, -1.0, 0.2]])
y = torch.tensor([0, 1])
def attempt(name, body, prepare=False):
    def step(x, y):
        optimizer.zero_grad()
        loss = body(x, y)
        loss.backward()
        optimizer.step()
        return loss
    try:
        compiled = torch.compile(step, training=True)
        if prepare:
            compiled.prepare(x, y).dispose()
        else:
            compiled(x, y)
        print(name, "accepted")
    except Exception as error:
        print(name, "->", type(error).__name__, error)
attempt("negative step", lambda x, y: F.cross_entropy(model(x)[:, ::-1], y))
attempt("mask index", lambda x, y: F.cross_entropy(model(x)[model(x) > 0].reshape(1, -1)[:, :2], y[:1]))
attempt("device index", lambda x, y: F.cross_entropy(model(x).index_select(1, model(x).sum(0)), y))
attempt("parameter by tensor", lambda x, y: F.cross_entropy(model(x) + model.weight[y].sum(), y))
attempt("two tensor indices", lambda x, y: F.cross_entropy(model(x) + model(x)[y, y].sum(), y))
attempt("negative index", lambda x, y: F.cross_entropy(model(x)[:, [-1, 0, 1, 2]], y))
attempt("empty slice", lambda x, y: F.cross_entropy(model(x)[:, 2:2], y))
attempt("int and tensor index", lambda x, y: F.cross_entropy(model(x) + model(x)[0, y].sum(), y))
attempt("multinomial of graph", lambda x, y: F.cross_entropy(model(x) + torch.multinomial(F.softmax(model(x), -1), 1).sum(), y))
attempt("in-place draw", lambda x, y: F.cross_entropy(model(x) + torch.empty(2, 4).normal_(), y), True)
attempt("poisson", lambda x, y: F.cross_entropy(model(x) + torch.poisson(torch.ones(2, 4)), y), True)
attempt("float draw item", lambda x, y: F.cross_entropy(model(x) * (2.0 if torch.rand(1).item() > 0.5 else 1.0), y), True)
attempt("int draw in python", lambda x, y: F.cross_entropy(model(x) * float(torch.randint(1, 3, (1,)).item()), y), True)
attempt("int draw cast", lambda x, y: F.cross_entropy(model(x) + torch.randint(0, 2, (2, 4)).float(), y), True)
attempt("multinomial of a parameter", lambda x, y: F.cross_entropy(model(x) + emb(torch.multinomial(model.bias.abs(), 2, True)).sum(), y), True)
attempt("multinomial constant", lambda x, y: F.cross_entropy(model(x) + model(x).index_select(1, torch.multinomial(torch.tensor([0.1, 0.2, 0.3, 0.4]), 4, True)), y), True)
attempt("randint targets", lambda x, y: F.cross_entropy(model(x), torch.randint(0, 4, (2,))) + F.cross_entropy(model(x), y), True)
attempt("draw requires grad", lambda x, y: F.cross_entropy(model(x) + torch.randn(2, 4, requires_grad=True), y), True)
"#)
    .unwrap();
    let draw_functions = "torch.rand/randn/randint/randperm/normal/multinomial";
    assert_eq!(
        out,
        [
            "negative step -> ValueError step must be greater than zero".to_owned(),
            "mask index -> NotImplementedError torch.compile cannot index with a boolean mask: it selects a data-dependent \
             number of elements, which a graph shape cannot hold"
                .to_owned(),
            "device index -> NotImplementedError torch.compile takes index_select indices from integer tensors on the CPU; \
             an index computed on the device (an argmax, a comparison, arithmetic on graph tensors) is not supported"
                .to_owned(),
            "parameter by tensor -> NotImplementedError torch.compile cannot record indexing a parameter by a tensor on the \
             CPU (W[idx]); use torch.index_select(W, 0, idx), torch.gather or F.embedding, which record on the device with \
             the parameter's gradient"
                .to_owned(),
            "two tensor indices -> NotImplementedError torch.compile records one tensor index per indexing (or \
             x[torch.arange(n), idx] of a matrix); index in separate steps, or use gather"
                .to_owned(),
            "negative index -> NotImplementedError torch.compile records non-negative index_select indices only (got -1); \
             add the dimension size to a negative index"
                .to_owned(),
            "empty slice -> NotImplementedError torch.compile cannot record a slice that selects no elements (graph \
             dimensions are positive)"
                .to_owned(),
            "int and tensor index -> NotImplementedError torch.compile does not combine integer and tensor indices in one \
             indexing; index in two steps (x[i][idx])"
                .to_owned(),
            "multinomial of graph -> NotImplementedError torch.compile cannot record torch.multinomial of a graph tensor: \
             it samples from values the host does not have until the step has run"
                .to_owned(),
            format!(
                "in-place draw -> NotImplementedError prepare() cannot record Tensor.normal_ inside the step: an in-place \
                 draw would keep its prepare() values at every step. {draw_functions}, their *_like forms and \
                 torch.bernoulli are drawn afresh on the host every step; use one of those, per-call torch.compile, or \
                 draw outside the step and pass the tensor as an argument"
            ),
            format!(
                "poisson -> NotImplementedError prepare() cannot record this CPU random draw: {draw_functions}, \
                 torch.rand_like/randn_like/randint_like and torch.bernoulli are drawn afresh on the host every step, but \
                 an in-place draw (normal_, uniform_, random_, bernoulli_, exponential_...), torch.poisson or nn.init \
                 inside the step would replay its prepare() values. Use one of those functions, per-call torch.compile, \
                 or draw outside the step and pass the tensor as an argument"
            ),
            "float draw item -> CompileUnsupportedError Tensor.item is not supported by torch.compile".to_owned(),
            "int draw in python -> NotImplementedError prepare(): the step draws torch.randint on the CPU and does not \
             read the draw as class targets, an index or a mask of the graph (it uses it in Python or in eager \
             arithmetic); the session would keep its prepare() value at every step. Use per-call torch.compile, or draw \
             outside the step and pass it as an argument"
                .to_owned(),
            "int draw cast -> NotImplementedError prepare(): the step reads a tensor that an eager operation computed from \
             a CPU random draw of integers (a cast, a one-hot, arithmetic) as a graph input; the session would keep its \
             prepare() value at every step. Read the draw itself as class targets or an index, draw a float32 tensor \
             (arithmetic on it records on the device), or draw outside the step and pass the tensor as an argument"
                .to_owned(),
            "multinomial of a parameter -> NotImplementedError prepare(): torch.multinomial inside the step samples from \
             probabilities computed from a parameter, an argument or another draw; the host would sample from their \
             prepare() values. Pass the samples as an argument, or use per-call torch.compile"
                .to_owned(),
            "multinomial constant accepted".to_owned(),
            "randint targets accepted".to_owned(),
            "draw requires grad -> NotImplementedError prepare() cannot record a random draw that requires grad (torch.randn)"
                .to_owned(),
        ]
    );
}

#[test]
fn version_four_graphs_label_themselves_and_the_native_kernels_equal_the_reference() {
    // zipp_gpu labels a graph 4 only once it selects; Zipp's tensor-kernel
    // evaluator equals the pure-Python reference bit for bit (signs of zero
    // included) on every version-4 operation, and both add an accumulation
    // in the protocol's order: the base first, then each contribution in
    // ascending index position, one float32 rounding at a time.
    let out = run(r#"
import math
import torch
import zipp_gpu
g = zipp_gpu.Graph()
vals = lambda n, k: [(-0.0 if i % 7 == 3 else ((i * k) % 13 - 6) * 0.37) for i in range(n)]
a = g.tensor(vals(60, 5), (3, 4, 5)); b = g.tensor(vals(24, 3), (2, 3, 4))
i1 = g.tensor([4.0, 0.0, 4.0, 2.0]); i3 = g.tensor([float((i * 3) % 5) for i in range(84)], (3, 4, 7))
outs = dict(sl=a[::-1, 1:, ::2], neg=a[2, ::-3, -1], put=g.slice_scatter(a, b[:, :, :1], [0, 1, 4], [2, 1, -1]),
            isel=a.index_select(2, i1), iadd=g.index_add(a, 2, i1, g.tensor(vals(48, 7), (3, 4, 4))),
            gat=a.gather(2, i3), sadd=g.scatter_add(a, 2, i3, g.tensor(vals(84, 11), (3, 4, 7))),
            cat=g.cat([a[:, :, :2], a[:, :, 4:]], 2), stack=g.stack([a[0], a[1]], 1))
program = g.program(**outs)
reference = zipp_gpu.execute_locally(program)["outputs"]
kernels = zipp_gpu._execute_kernels(program, False)["outputs"]
signs = lambda v: [math.copysign(1.0, x) for x in v]
print("version", program["version"], "kernels == reference", all(kernels[k]["data"] == reference[k]["data"] and signs(kernels[k]["data"]) == signs(reference[k]["data"])
      and kernels[k]["shape"] == reference[k]["shape"] for k in reference))
h = zipp_gpu.Graph()
e = 2.0 ** -24
base = h.tensor([1.0, -0.0, 0.0, 3.0]); src = h.tensor([e, -0.0, e, -0.0, 2.0 ** 24, -(2.0 ** 24)]); pos = h.tensor([0.0, 1.0, 0.0, 2.0, 3.0, 3.0])
p = h.program(ia=h.index_add(base, 0, pos, src), sa=h.scatter_add(base, 0, pos, src))
for run in (zipp_gpu.execute_locally(p)["outputs"], zipp_gpu._execute_kernels(p, False)["outputs"]):
    print("order", run["ia"]["data"], signs(run["ia"]["data"]), run["sa"]["data"] == run["ia"]["data"])
v = zipp_gpu.Graph(); t = v.tensor([[1.0, 2.0], [3.0, 4.0]])
print("labels", v.program(r=t * 2)["version"], v.program(r=t.exp())["version"], v.program(r=t > 1)["version"], v.program(r=(t > 1)[0])["version"], v.program(r=t[:, 1])["version"])
"#)
    .unwrap();
    assert_eq!(
        out,
        [
            "version 4 kernels == reference True",
            "order [1.0, -0.0, 0.0, 4.0] [1.0, -1.0, 1.0, 1.0] True",
            "order [1.0, -0.0, 0.0, 4.0] [1.0, -1.0, 1.0, 1.0] True",
            "labels 1 2 3 4 4",
        ]
    );
}
