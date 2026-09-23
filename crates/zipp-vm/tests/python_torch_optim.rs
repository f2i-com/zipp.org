//! The bundled `torch.optim`, `torch.optim.lr_scheduler`, `torch.utils.data`
//! and the checkpoint layer under them (`pickle`, `zipfile`), checked against
//! CPU PyTorch 2.11 / CPython 3.11. Expected values and the PyTorch-written
//! checkpoints come from `fixtures/torch_optim/gen_expected.py` and from
//! running each `*_cases.py` fixture under PyTorch (`python data_cases.py >
//! data_expected.txt`, and likewise for groups and pickle).
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

fn run_with(source: &str, files: &[(&str, &[u8])]) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    let files: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
        .collect();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &files, &[], false)?;
            let state = compiled.state_mut();
            state.set_limits(2_000_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn run(source: &str) -> Result<Vec<String>, String> {
    run_with(source, &[])
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// A program's output as lines (one print may span several).
fn output_lines(out: Vec<String>) -> Vec<String> {
    lines(&out.join("
"))
}

/// A fixture module plus `expected = <json>` and a comparison that prints
/// one line per mismatch (none when everything agrees).
fn with_expected(fixture: &str, expected: &str, compare: &str) -> String {
    format!("{fixture}\nimport json\nexpected = json.loads(r'''{expected}''')\n{compare}")
}

const COMPARE_NUMBERS: &str = r#"
got = results()
bad = []
for name in sorted(expected):
    e, g = expected[name], got[name]
    if len(e) != len(g):
        bad.append("%s: %d values, expected %d" % (name, len(g), len(e)))
        continue
    if e and not isinstance(e[0], (int, float)) or isinstance(e[0], bool):
        if list(g) != list(e):
            bad.append("%s: %r != %r" % (name, g, e))
        continue
    worst = max([abs(a - b) / max(1.0, abs(b)) for a, b in zip(g, e)] + [0.0])
    if not worst <= TOLERANCE:
        bad.append("%s: off by %.3g" % (name, worst))
print("compared", len(expected), bad)
"#;

/// Eight steps of every optimizer (and six through a closure) on a float32
/// elementwise loss, against PyTorch's trajectories: each parameter value
/// after each step. Covers SGD/Adam/AdamW options, the foreach/fused/
/// capturable/decoupled_weight_decay keywords, RMSprop centered and
/// maximize, and Adagrad, Adamax, NAdam, RAdam, Adadelta, ASGD and Rprop.
/// Closures used to fail for every optimizer (they ran under no_grad).
#[test]
fn optimizers_match_pytorch_trajectories_and_closures() {
    let source = with_expected(
        include_str!("fixtures/torch_optim/optim_cases.py"),
        include_str!("fixtures/torch_optim/optim_expected.json"),
        &format!("TOLERANCE = 2e-6\n{COMPARE_NUMBERS}"),
    );
    assert_eq!(run(&source).unwrap(), ["compared 41 []"]);
}

/// Every scheduler's get_last_lr() (and cycled momentum/beta1) over 30
/// steps, chained schedulers, a manual lr change carrying forward, the
/// closed form for step(epoch), resuming with last_epoch= and a state_dict
/// round trip. StepLR used to recompute from base_lrs, which broke chaining
/// and overwrote external changes.
#[test]
fn schedulers_match_pytorch_learning_rate_sequences() {
    let source = with_expected(
        include_str!("fixtures/torch_optim/sched_cases.py"),
        include_str!("fixtures/torch_optim/sched_expected.json"),
        &format!("TOLERANCE = 1e-12\n{COMPARE_NUMBERS}"),
    );
    assert_eq!(run(&source).unwrap(), ["compared 26 []"]);
}

/// Datasets, samplers, default_collate and DataLoader print what PyTorch
/// prints (random orders are checked for their properties).
#[test]
fn data_utilities_match_pytorch() {
    let out = output_lines(run(include_str!("fixtures/torch_optim/data_cases.py")).unwrap());
    assert_eq!(out, lines(include_str!("fixtures/torch_optim/data_expected.txt")));
}

/// Parameter groups (generators, a bare tensor, duplicates, overlap, names),
/// Optimizer(tensor)/empty-list errors, option validation, group keys,
/// defaults and repr, state dicts that validate before touching the state,
/// closures run with gradients enabled, zero_grad and step hooks.
#[test]
fn optimizer_api_matches_pytorch() {
    let out = output_lines(run(include_str!("fixtures/torch_optim/groups_cases.py")).unwrap());
    assert_eq!(out, lines(include_str!("fixtures/torch_optim/groups_expected.txt")));
}

/// CPython's pickles of the same data at protocols 0-5 (frozenset opcodes,
/// text floats, bytes through _codecs.encode, bytearray) load, and this
/// pickle's own output round-trips.
#[test]
fn plain_pickle_reads_every_protocol() {
    let files: Vec<(&str, &[u8])> = vec![
        ("plain_p0.pkl", include_bytes!("fixtures/torch_optim/plain_p0.pkl")),
        ("plain_p1.pkl", include_bytes!("fixtures/torch_optim/plain_p1.pkl")),
        ("plain_p2.pkl", include_bytes!("fixtures/torch_optim/plain_p2.pkl")),
        ("plain_p3.pkl", include_bytes!("fixtures/torch_optim/plain_p3.pkl")),
        ("plain_p4.pkl", include_bytes!("fixtures/torch_optim/plain_p4.pkl")),
        ("plain_p5.pkl", include_bytes!("fixtures/torch_optim/plain_p5.pkl")),
    ];
    let out = output_lines(run_with(include_str!("fixtures/torch_optim/pickle_cases.py"), &files).unwrap());
    assert_eq!(out, lines(include_str!("fixtures/torch_optim/pickle_expected.txt")));
}

/// A PyTorch Adam checkpoint (whose `step` is a float tensor) resumes with an
/// integer step, continues PyTorch's own trajectory, and a compiled training
/// step accepts it. PyTorch-written bytes, sets, bytearrays and OrderedDicts
/// load through torch.load.
#[test]
fn pytorch_checkpoints_resume_and_load_plain_data() {
    let source = format!(
        "import json\nexpected = json.loads(r'''{}''')\n{}",
        include_str!("fixtures/torch_optim/resume_expected.json"),
        r#"
import torch

def loss_of(w, b):
    return ((w - 0.3) ** 2 * torch.tensor([1.0, 2.0, 0.5])).sum() + (b * b).sum() * 0.7 + (w * w * w).sum() * 0.1

ck = torch.load("adam_resume.pt")
w = ck["w"].clone().requires_grad_(True)
b = ck["b"].clone().requires_grad_(True)
opt = torch.optim.Adam([w, b], lr=0.1, weight_decay=0.05)
opt.load_state_dict(ck["opt"])
print("step", [type(opt.state[p]["step"]).__name__ for p in (w, b)], [opt.state[p]["step"] for p in (w, b)])
print("groups", opt.param_groups[0]["foreach"], opt.param_groups[0]["decoupled_weight_decay"], opt.param_groups[0]["weight_decay"])
got = []
for _ in range(3):
    opt.zero_grad()
    loss_of(w, b).backward()
    opt.step()
    got.extend(w.detach().tolist() + b.detach().tolist())
print("resumed", max(abs(a - e) for a, e in zip(got, expected)) < 2e-6, opt.state[w]["step"])

# The GPU capture needs integer step counts; a float-tensor step refused it.
lin = torch.load("adam_linear.pt")
model = torch.nn.Linear(2, 1)
model.load_state_dict(lin["model"])
lin_opt = torch.optim.Adam(model.parameters(), lr=0.01)
lin_opt.load_state_dict(lin["opt"])

def train_step(x, y):
    lin_opt.zero_grad()
    loss = ((model(x) - y) ** 2).mean()
    loss.backward()
    lin_opt.step()
    return loss
torch.compile(train_step, training=True)(torch.tensor([[1.0, 2.0]]), torch.tensor([[1.0]])).submit(
    lambda loss: print("compiled after resume", [lin_opt.state[p]["step"] for p in model.parameters()]),
    lambda error: print("compiled error", error))

data = torch.load("plain_data.pt")
print("plain", sorted(data), data["tok"], data["empty"], sorted(data["s"]), list(data["od"].items()), type(data["od"]).__name__, data["bytearray"])
"#
    );
    let files: Vec<(&str, &[u8])> = vec![
        ("adam_resume.pt", include_bytes!("fixtures/torch_optim/adam_resume.pt")),
        ("plain_data.pt", include_bytes!("fixtures/torch_optim/plain_data.pt")),
        ("adam_linear.pt", include_bytes!("fixtures/torch_optim/adam_linear.pt")),
    ];
    assert_eq!(
        run_with(&source, &files).unwrap(),
        [
            "step ['int', 'int'] [3, 3]",
            "groups None False 0.05",
            "resumed True 6",
            "compiled after resume [2, 2]",
            "plain ['bytearray', 'empty', 'od', 's', 'tok'] b'\\x00\\xff\\x80abc' b'' [1, 2, 3] [('z', 1), ('a', 2)] OrderedDict bytearray(b'xy')",
        ]
    );
}

/// What Zipp writes: bytes as `_codecs.encode` (the protocol-2 form PyTorch's
/// weights-only loader accepts, not BINBYTES), parameters (a Tensor subclass)
/// pickle through the tensor reducer, `torch._utils._rebuild_parameter`
/// returns an nn.Parameter, a whole module is refused with a pointer to
/// state_dict(), and the weights-only allowlist helper resolves PyTorch's
/// globals.
#[test]
fn checkpoints_write_pytorch_compatible_globals() {
    let out = run(r#"
import zipfile
import pickle
from collections import OrderedDict
import torch
from torch import nn

torch.save({"tok": b"\x00\xff", "empty": b"", "ba": bytearray(b"z"), "s": {1}}, "b.pt")
with zipfile.ZipFile("b.pt") as z:
    data = z.read("b/data.pkl")
print("codecs", b"c_codecs\nencode\n" in data, b"cbuiltins\nbytearray\n" in data, b"cbuiltins\nset\n" in data)
print("round trip", torch.load("b.pt"))

m = nn.Linear(2, 2)
for label, value in [("parameter", m.weight), ("parameters", list(m.parameters())), ("keep_vars", m.state_dict(keep_vars=True))]:
    torch.save(value, "p.pt")
    back = torch.load("p.pt")
    first = back if isinstance(back, torch.Tensor) else (back[0] if isinstance(back, list) else back["weight"])
    print(label, first.requires_grad, torch.equal(first.detach(), m.weight.detach()))
try:
    torch.save(m, "m.pt")
except pickle.PicklingError as e:
    print("module refused", "state_dict()" in str(e))

p = torch._utils._rebuild_parameter(torch.tensor([1.0, 2.0]), True, OrderedDict())
print("rebuild", type(p).__name__, isinstance(p, nn.Parameter), p.requires_grad)
find = torch._utils._weights_only_find_class
print("allowlist", find("torch", "Size") is torch.Size, find("torch", "device") is torch.device, find("__builtin__", "set") is set,
      find("_codecs", "encode")("\xff", "latin1"), find("torch._utils", "_rebuild_parameter") is torch._utils._rebuild_parameter,
      find("torch", "float64") == torch.float64, find("collections", "OrderedDict") is OrderedDict)
for module, name in [("builtins", "eval"), ("os", "system"), ("builtins", "frozenset")]:
    try:
        find(module, name)
    except pickle.UnpicklingError as e:
        print("refused", module, name)
"#)
    .unwrap();
    assert_eq!(
        out,
        [
            "codecs True True True",
            "round trip {'tok': b'\\x00\\xff', 'empty': b'', 'ba': bytearray(b'z'), 's': {1}}",
            "parameter True True",
            "parameters True True",
            "keep_vars True True",
            "module refused True",
            "rebuild Parameter True True",
            "allowlist True True True b'\\xff' True True True",
            "refused builtins eval",
            "refused os system",
            "refused builtins frozenset",
        ]
    );
}

/// RMSprop keeps and increments `step` (PyTorch's load_state_dict needs it),
/// with centered and maximize state.
#[test]
fn rmsprop_state_has_pytorch_keys() {
    let out = run(r#"
import torch
w = torch.tensor([1.0, 2.0], requires_grad=True)
opt = torch.optim.RMSprop([w], lr=0.01, momentum=0.5, centered=True)
for _ in range(2):
    w.grad = torch.tensor([0.5, -1.0])
    opt.step()
sd = opt.state_dict()
print(sorted(sd["state"][0].keys()), sd["state"][0]["step"], sd["param_groups"][0]["centered"])
"#)
    .unwrap();
    assert_eq!(out, ["['grad_avg', 'momentum_buffer', 'square_avg', 'step'] 2 True"]);
}

/// Mode "a" appends to an existing archive (keeping its entries, compressed
/// or not, byte for byte), "x" refuses an existing file, and "a" on a
/// missing file starts one.
#[test]
fn zipfile_appends_and_refuses_to_replace() {
    let out = run_with(
        r#"
import zipfile
with zipfile.ZipFile("a.zip", "w") as z:
    z.writestr("first.txt", "1")
with zipfile.ZipFile("a.zip", "a") as z:
    print("existing", z.namelist(), z.read("first.txt"))
    z.writestr("second.txt", b"22")
with zipfile.ZipFile("a.zip") as z:
    print("after append", z.namelist(), z.read("first.txt"), z.read("second.txt"))
try:
    zipfile.ZipFile("a.zip", "x")
except FileExistsError:
    print("x refused", zipfile.ZipFile("a.zip").namelist())
with zipfile.ZipFile("new.zip", "x") as z:
    z.writestr("n", "x")
with zipfile.ZipFile("fresh.zip", "a") as z:
    z.writestr("f", "y")
print("created", zipfile.ZipFile("new.zip").namelist(), zipfile.ZipFile("fresh.zip").read("f"))
with zipfile.ZipFile("deflated.zip", "a") as z:
    z.writestr("stored.txt", "plain")
with zipfile.ZipFile("deflated.zip") as z:
    print("deflated kept", [(i.filename, i.compress_type) for i in z.infolist()], z.read("stored.txt"))
    try:
        z.read("packed.txt")
    except NotImplementedError:
        print("deflate unreadable")
with open("deflated.zip", "rb") as f:
    blob = f.read()
with open("original.zip", "rb") as f:
    original = f.read()
import struct
directory = struct.unpack("<I", original[-6:-2])[0]
print("deflated bytes kept", blob[:directory] == original[:directory])
"#,
        &[
            ("deflated.zip", include_bytes!("fixtures/torch_optim/deflated.zip")),
            ("original.zip", include_bytes!("fixtures/torch_optim/deflated.zip")),
        ],
    )
    .unwrap();
    assert_eq!(
        out,
        [
            "existing ['first.txt'] b'1'",
            "after append ['first.txt', 'second.txt'] b'1' b'22'",
            "x refused ['first.txt', 'second.txt']",
            "created ['n'] b'y'",
            "deflated kept [('packed.txt', 8), ('stored.txt', 0)] b'plain'",
            "deflate unreadable",
            "deflated bytes kept True",
        ]
    );
}
