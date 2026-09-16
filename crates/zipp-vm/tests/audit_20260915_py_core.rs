//! Python runtime-core audit (2026-09-15): file handles left open reach the
//! virtual filesystem at exit, byte buffers past the engine's dense-array
//! limit keep their contents, the pickle loader rejects negative lengths and
//! state on shared classes, and the bundled torch keeps autograd history
//! through in-place ops and promotes integer sums like PyTorch.
//!
//! Python programs run with the VM JIT disabled (`compile_python_program`),
//! so there is a single execution tier to cover. The torch golden values
//! come from CPU PyTorch 2.11; the rest from CPython 3.13.
#![cfg(feature = "python")]
use zipp_vm::embed::JsValue;
use zipp_vm::frontend::compile_python_program;

struct Run {
    outcome: Result<(), String>,
    output: Vec<String>,
    /// The `__zipp_py_vfs_changed` listing the hosts write back.
    changes: String,
}

fn run(main: &str, files: &[(&str, &[u8])]) -> Run {
    let modules = vec![("main".to_owned(), main.to_owned())];
    let files: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(n, b)| (n.to_string(), b.to_vec()))
        .collect();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let mut compiled =
                compile_python_program("main", &modules, &files, &[], false).expect("compile");
            let state = compiled.state_mut();
            state.set_limits(2_000_000_000, None);
            let outcome = state.run_init().map(|_| ());
            let output = state.take_output();
            let changes = match state.call_global("__zipp_py_vfs_changed", &[]) {
                Ok(JsValue::String(s)) => s,
                other => panic!("vfs listing: {other:?}"),
            };
            Run {
                outcome,
                output,
                changes,
            }
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn base64(bytes: &[u8]) -> String {
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        out.push(B64[(b[0] >> 2) as usize] as char);
        out.push(B64[(((b[0] & 3) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[(((b[1] & 15) << 2) | (b[2] >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(b[2] & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn assert_written(changes: &str, path: &str, content: &[u8]) {
    let entry = format!("\"path\":\"{path}\",\"base64\":\"{}\"", base64(content));
    assert!(changes.contains(&entry), "{path}: {entry} not in {changes}");
}

/// CPython flushes a file when its last reference goes and at exit; every
/// write through an unclosed handle used to reach the host as an empty file,
/// truncating what was there.
#[test]
fn unclosed_file_handles_are_flushed_for_reads_and_at_exit() {
    let main = r#"
import json
open("out.txt", "w").write("hello\n")
print(repr(open("out.txt").read()))
f = open("log.txt", "a")
f.write("line\n")
json.dump({"a": 1}, open("cfg.json", "w"))
def save(p, t):
    fh = open(p, "w")
    fh.write(t)
save("saved.txt", "saved\n")
open("data.txt", "w").write("lower")
with open("pf.txt", "w") as h:
    print("x", 1, file=h)
print(repr(open("pf.txt").read()))
w = open("seek.bin", "wb")
w.seek(3)
w.write(b"ab")
w.close()
print(open("seek.bin", "rb").read())
"#;
    let r = run(main, &[("data.txt", b"ORIGINAL")]);
    r.outcome.unwrap();
    assert_eq!(r.output, ["'hello\\n'", "'x 1\\n'", "b'\\x00\\x00\\x00ab'"]);
    assert_written(&r.changes, "out.txt", b"hello\n");
    assert_written(&r.changes, "log.txt", b"line\n");
    assert_written(&r.changes, "cfg.json", b"{\"a\": 1}");
    assert_written(&r.changes, "saved.txt", b"saved\n");
    assert_written(&r.changes, "data.txt", b"lower");
    assert_written(&r.changes, "pf.txt", b"x 1\n");

    // An uncaught exception still leaves the handle's writes behind.
    let r = run(
        "g = open('err.txt', 'w')\ng.write('partial')\nraise ValueError('boom')\n",
        &[],
    );
    assert!(r.outcome.unwrap_err().contains("ValueError"));
    assert_written(&r.changes, "err.txt", b"partial");
}

/// `bytes(n)` was `new Array(n).fill(0)` and file reads, `struct` and
/// `os.urandom` went through `Array.from`: past 2^20 elements they came back
/// empty or raised. A file larger than the hosts load (8 MiB) is refused on
/// write.
#[test]
fn byte_buffers_past_the_dense_array_limit_keep_their_contents() {
    let big: Vec<u8> = (0..1_100_000u32).map(|i| (i % 251) as u8).collect();
    let main = r#"
n = 1_100_000
b = bytearray(n)
z = bytes(n)
print(len(b), z[5], len(z[:10]), z[-1], len(bytes(-0)))
data = open("big.bin", "rb").read()
print(len(data), data[-1], data[1048576])
open("copy.bin", "wb").write(data)
print(len(open("copy.bin", "rb").read()))
try:
    bytes(-1)
except ValueError as e:
    print("ValueError", e)
import os, struct
packed = struct.pack("%ds" % n, data)
print(len(packed), packed[-1] == data[-1], len(struct.unpack("%ds" % n, packed)[0]), len(os.urandom(n)))
f = open("huge.bin", "wb")
try:
    f.truncate(9 * 1024 * 1024)
except OSError as e:
    print("OSError", e)
f.seek(8 * 1024 * 1024)
try:
    f.write(b"x")
except OSError as e:
    print("OSError", e)
"#;
    let r = run(main, &[("big.bin", &big)]);
    r.outcome.unwrap();
    assert_eq!(
        r.output,
        [
            "1100000 0 10 0 0",
            &format!("1100000 {} {}", big[big.len() - 1], big[1_048_576]),
            "1100000",
            "ValueError negative count",
            "1100000 True 1100000 1100000",
            "OSError [Errno 27] File too large",
            "OSError [Errno 27] File too large",
        ]
    );
}

/// A negative LONG4/BINSTRING length moved the read position backwards and
/// the loader looped forever; BUILD patched classes `find_class` handed out.
#[test]
fn pickle_rejects_negative_lengths_and_state_on_shared_classes() {
    let main = r#"
import pickle
from collections import OrderedDict
cases = [
    b'\x80\x02\x8b\xfb\xff\xff\xff.',
    b'\x80\x02T\xfb\xff\xff\xff.',
    b'\x80\x02',
    b'\x80\x02\x8b\x05\x00\x00\x00ab',
    b'\x80\x02ccollections\nOrderedDict\n}X\x04\x00\x00\x00keyscbuiltins\nlist\nsb.',
    b'\x80\x02\x8b\x01\x00\x00\x00\x05.',
]
for data in cases:
    try:
        print("ok", pickle.loads(data))
    except Exception as e:
        print(type(e).__name__, e)
print(list(OrderedDict(a=1).keys()), pickle.loads(b'\x80\x02cbuiltins\nbytearray\n.') is bytearray)
od = OrderedDict(a=1)
print(pickle.loads(pickle.dumps(od)) == od)
"#;
    let r = run(main, &[]);
    r.outcome.unwrap();
    assert_eq!(
        r.output,
        [
            "UnpicklingError LONG pickle has negative byte count",
            "UnpicklingError BINSTRING pickle has negative byte count",
            "EOFError Ran out of input",
            "UnpicklingError pickle data was truncated",
            "TypeError 'mappingproxy' object does not support item assignment",
            "ok 5",
            "['a'] True",
            "True",
        ]
    );
}

/// A checkpoint whose data.pkl sets `torch.FloatStorage.dtype` through
/// BUILD used to corrupt every later honest load; saving a tensor past
/// 2^18 elements overflowed the byte conversion.
#[test]
fn torch_checkpoints_resist_class_patching_and_save_large_tensors() {
    let main = r#"
import torch, zipfile
evil = b'\x80\x02ctorch\nFloatStorage\n}X\x05\x00\x00\x00dtypectorch\nuint8\nsb0}.'
with zipfile.ZipFile('evil.pt', 'w') as z:
    z.writestr('evil/data.pkl', evil)
torch.save({'w': torch.tensor([1.0, 2.0, 3.0, 4.0])}, 'good.pt')
try:
    torch.load('evil.pt')
    print("loaded")
except Exception as e:
    print(type(e).__name__)
with zipfile.ZipFile('hang.pt', 'w') as z:
    z.writestr('hang/data.pkl', b'\x80\x02\x8b\xfb\xff\xff\xff.')
try:
    torch.load('hang.pt')
except Exception as e:
    print(type(e).__name__, e)
print(torch.load('good.pt')['w'].tolist())
torch.save({'w': torch.arange(300000, dtype=torch.float32)}, 'ck.pt')
print(torch.load('ck.pt')['w'][299999].item())
print(hex(zipfile._crc32(b"hello world")), hex(zipfile._crc32(bytes(4000))))
"#;
    let r = run(main, &[]);
    r.outcome.unwrap();
    assert_eq!(
        r.output,
        [
            "TypeError",
            "UnpicklingError LONG pickle has negative byte count",
            "[1.0, 2.0, 3.0, 4.0]",
            "299999.0",
            "0xd4a1185 0x3a8b93be",
        ]
    );
}

/// In-place ops rebound the tensor to a node whose parent was the tensor
/// itself, so `h += x` lost the path to `h`'s producer (the residual
/// `out += identity` left Linear gradients None).
#[test]
fn torch_inplace_ops_keep_the_autograd_history() {
    let main = r#"
import torch
import torch.nn as nn
import torch.nn.functional as F
def show(label, t):
    print(label, None if t is None else [round(v, 4) for v in t.flatten().tolist()])
x = torch.tensor([1., 2.], requires_grad=True)
h = x * 3
h += x
(h * h).sum().backward()
show("A", x.grad)
x = torch.tensor([1., 2.], requires_grad=True)
h = x + 0
h.mul_(5)
h.sum().backward()
show("B", x.grad)
x = torch.tensor([1., 2., 3.], requires_grad=True)
h = x * 2
h[0] = 0
h.sum().backward()
show("C", x.grad)
x = torch.tensor([1., 2., 3.], requires_grad=True)
v = torch.tensor(5., requires_grad=True)
h = x * 2
h[1] = v * 3
(h * h).sum().backward()
show("C2x", x.grad); show("C2v", v.grad)
x = torch.tensor([[1., 2.]], requires_grad=True)
w = torch.tensor([[1., 0.], [0., 1.]], requires_grad=True)
out = x @ w
out += x
F.relu(out).sum().backward()
show("Dw", w.grad); show("Dx", x.grad)
class Block(nn.Module):
    def __init__(self):
        super().__init__()
        self.fc = nn.Linear(2, 2)
    def forward(self, x):
        identity = x
        out = self.fc(x)
        out += identity
        return F.relu(out)
b = Block()
with torch.no_grad():
    b.fc.weight.copy_(torch.tensor([[0.5, -0.5], [0.25, 0.75]]))
    b.fc.bias.copy_(torch.tensor([0.1, -0.2]))
b(torch.tensor([[1., 3.]])).sum().backward()
show("Ew", b.fc.weight.grad); show("Eb", b.fc.bias.grad)
x = torch.tensor([1., 2.], requires_grad=True)
h = x * 1
y = h * 2
h.mul_(10)
(y.sum() + h.sum()).backward()
show("F", x.grad)
x = torch.tensor([1., 2.], requires_grad=True)
h = torch.zeros(2)
h += x
(h * 2).sum().backward()
show("G", x.grad)
x = torch.tensor([-1., 0.5, 2.], requires_grad=True)
h = x * 1
h.clamp_(0, 1)
h.sum().backward()
show("H", x.grad)
w = torch.tensor([1., 2.], requires_grad=True)
for _ in range(3):
    (w * w).sum().backward()
    with torch.no_grad():
        w -= 0.1 * w.grad
    w.grad = None
print("I", w.requires_grad, w.is_leaf, [round(v, 4) for v in w.tolist()])
"#;
    let r = run(main, &[]);
    r.outcome.unwrap();
    assert_eq!(
        r.output,
        [
            "A [32.0, 64.0]",
            "B [5.0, 5.0]",
            "C [0.0, 2.0, 2.0]",
            "C2x [8.0, 0.0, 24.0]",
            "C2v [90.0]",
            "Dw [1.0, 1.0, 2.0, 2.0]",
            "Dx [2.0, 2.0]",
            "Ew [1.0, 3.0, 1.0, 3.0]",
            "Eb [1.0, 1.0]",
            "F [12.0, 12.0]",
            "G [2.0, 2.0]",
            "H [0.0, 1.0, 0.0]",
            "I True True [0.512, 1.024]",
        ]
    );
}

/// Bool and integer reductions accumulate in int64; eye(n, m) for n != m,
/// tied max/min gradients, max(dim) backward and BCE at exact 0/1.
#[test]
fn torch_reductions_and_numerics_match_pytorch() {
    let main = r#"
import torch
import torch.nn.functional as F
def show(label, t):
    print(label, [round(v, 4) for v in t.flatten().tolist()])
pred = torch.tensor([1, 0, 3, 3]); target = torch.tensor([1, 2, 3, 3])
c = (pred == target).sum()
print("S1", c.item(), c.dtype, c.item() / len(target))
print("S2", torch.tensor([200, 100], dtype=torch.uint8).sum().item())
print("S3", torch.tensor([True, True, False]).sum().item(), (pred == target).sum(dim=0).item())
print("S4", torch.count_nonzero(pred).item(), torch.tensor([2, 3]).prod().item(), torch.tensor([True, False, True]).cumsum(0).tolist())
print("S5", torch.tensor([1., 2.]).sum().dtype, torch.tensor([1, 2], dtype=torch.int32).sum().dtype)
print("N1", torch.eye(2, 3).tolist(), torch.eye(3, 2).tolist())
x = torch.tensor([1., 3., 3.], requires_grad=True); x.max().backward(); show("N2", x.grad)
x = torch.tensor([2., 1., 1.], requires_grad=True); x.min().backward(); show("N3", x.grad)
y = torch.tensor([[1., 2., 3.], [6., 5., 4.]], requires_grad=True)
v, i = y.max(dim=1); v.sum().backward(); show("N4", y.grad)
y = torch.tensor([[1., 2., 3.], [6., 5., 4.]], requires_grad=True)
v, i = torch.max(y, 0); v.sum().backward(); show("N5", y.grad)
y = torch.tensor([[1., 2., 3.]], requires_grad=True)
v, i = y.min(dim=1, keepdim=True); (v * 2).sum().backward(); show("N6", y.grad)
p = torch.tensor([0., 1.], requires_grad=True)
l = F.binary_cross_entropy(p, torch.tensor([0., 1.])); print("N7", round(l.item(), 4))
l.backward(); show("N7g", p.grad)
print("N8", round(F.binary_cross_entropy(torch.tensor([0., 1., 0.3]), torch.tensor([1., 0., 1.])).item(), 4))
p = torch.tensor([0.2, 0.9], requires_grad=True)
l = F.binary_cross_entropy(p, torch.tensor([0., 1.]), weight=torch.tensor([2., 1.]))
print("N9", round(l.item(), 4)); l.backward(); show("N9g", p.grad)
# One layout for the whole tensor, as PyTorch prints it.
print("R1", torch.tensor([0., 0.5, 0.5]))
print("R2", torch.tensor([1., 2.]), torch.tensor(0.5))
print("R3", torch.tensor([1., float("inf")]), torch.tensor([0., float("nan")]))
print("R4", repr(torch.tensor([[1., 0.], [0., 0.25]])).replace("\n", "|"))
"#;
    let r = run(main, &[]);
    r.outcome.unwrap();
    assert_eq!(
        r.output,
        [
            "S1 3 torch.int64 0.75",
            "S2 300",
            "S3 2 3",
            "S4 3 6 [1, 1, 2]",
            "S5 torch.float32 torch.int64",
            "N1 [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] [[1.0, 0.0], [0.0, 1.0], [0.0, 0.0]]",
            "N2 [0.0, 0.5, 0.5]",
            "N3 [0.0, 0.5, 0.5]",
            "N4 [0.0, 0.0, 1.0, 1.0, 0.0, 0.0]",
            "N5 [0.0, 0.0, 0.0, 1.0, 1.0, 1.0]",
            "N6 [2.0, 0.0, 0.0]",
            "N7 0.0",
            "N7g [0.0, 0.0]",
            "N8 67.068",
            "N9 0.2758",
            "N9g [1.25, -0.5556]",
            "R1 tensor([0.0000, 0.5000, 0.5000])",
            "R2 tensor([1., 2.]) tensor(0.5000)",
            "R3 tensor([1., inf]) tensor([0., nan])",
            "R4 tensor([[1.0000, 0.0000],|        [0.0000, 0.2500]])",
        ]
    );
}
