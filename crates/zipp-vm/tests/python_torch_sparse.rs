//! torch.sparse checked against CPU PyTorch 2.11.
//! `fixtures/torch_sparse/sparse_cases.py` runs unchanged under both;
//! `gen.py` writes its PyTorch results to `sparse_expected.json` and
//! `text_expected.txt` (and the checkpoint `sparse.pt`). Values and
//! gradients are compared for coalescing, conversions, sparse-dense and
//! sparse-sparse products, sums, softmax, arithmetic and shape ops in
//! float32 and float64, for nn.Embedding(sparse=True) trained by SGD
//! (with Nesterov momentum), Adagrad and SparseAdam, and for a GCN trained
//! with torch.sparse.mm; printing, index order and is_coalesced flags,
//! grad_fn names, sparse gradient layouts and errors are compared as text.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

fn run_with(source: &str, files: Vec<(String, Vec<u8>)>) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &files, &[], false)?;
            let state = compiled.state_mut();
            state.set_limits(4_000_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn run(source: &str) -> Result<Vec<String>, String> {
    run_with(source, Vec::new())
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Each case's values agree to 1e-12 (float64) or 2e-6 (float32) of the
/// case's largest magnitude: Zipp's sparse-dense products sum each result
/// row in a double and round once, PyTorch accumulates float32 in float.
const COMPARE: &str = r#"
got = results()
bad = []
for name in sorted(expected):
    e = expected[name]
    if name not in got:
        bad.append("%s: missing" % name)
        continue
    g = got[name]
    if len(e) != len(g):
        bad.append("%s: %d values, expected %d" % (name, len(g), len(e)))
        continue
    tol = 2e-6 if name.startswith("f32") else 1e-12
    scale = max([1.0] + [abs(v) for v in e])
    worst = 0.0
    at = -1
    for i in range(len(e)):
        d = abs(g[i] - e[i]) / scale
        if d > worst:
            worst = d
            at = i
    if not worst <= tol:
        bad.append("%s: off by %.3g at %d (%r vs %r)" % (name, worst, at, g[at], e[at]))
extra = sorted(set(got) - set(expected))
print("compared", len(expected), bad, extra)
"#;

#[test]
fn sparse_values_and_gradients_match_pytorch() {
    let source = format!(
        "{}\nimport json\nexpected = json.loads(r'''{}''')\n{}",
        include_str!("fixtures/torch_sparse/sparse_cases.py"),
        include_str!("fixtures/torch_sparse/sparse_expected.json"),
        COMPARE
    );
    let out = run(&source).unwrap();
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(out[0].starts_with("compared ") && out[0].ends_with(" [] []"), "{}", out[0]);
}

#[test]
fn sparse_printing_structure_and_errors_match_pytorch() {
    let source = format!(
        "{}\nfor line in text():\n    print(line)\n",
        include_str!("fixtures/torch_sparse/sparse_cases.py")
    );
    let out = lines(&run(&source).unwrap().join("\n"));
    let expected = lines(include_str!("fixtures/torch_sparse/text_expected.txt"));
    for (i, (g, e)) in out.iter().zip(&expected).enumerate() {
        assert_eq!(g, e, "line {i}");
    }
    assert_eq!(out.len(), expected.len(), "{out:#?}");
}

/// Checkpoints: torch.load reads PyTorch's sparse records
/// (`torch._utils._rebuild_sparse_tensor` with the layout from
/// `torch.serialization._get_layout`; a CSR/CSC tensor's two index tensors
/// share one storage at different offsets) under weights_only, and
/// torch.save writes the same records, so a Zipp checkpoint round-trips
/// (PyTorch 2.11 loads it back as well).
#[test]
fn sparse_checkpoints_round_trip_with_pytorch() {
    let files = vec![(
        "sparse.pt".to_owned(),
        include_bytes!("fixtures/torch_sparse/sparse.pt").to_vec(),
    )];
    let out = run_with(
        r#"
import torch
import zipfile

def show(d):
    for k in sorted(d):
        t = d[k]
        if t.layout == torch.sparse_coo:
            print(k, t.layout, t.dtype, tuple(t.shape), t.is_coalesced(), t._indices().tolist(), t._values().tolist())
        elif t.layout == torch.strided:
            print(k, t.layout, t.dtype, t.tolist())
        else:
            c, p = (t.crow_indices(), t.col_indices()) if t.layout == torch.sparse_csr else (t.ccol_indices(), t.row_indices())
            print(k, t.layout, t.dtype, tuple(t.shape), c.tolist(), p.tolist(), t.values().tolist())

d = torch.load("sparse.pt")
show(d)
torch.save(d, "rt.pt")
with zipfile.ZipFile("rt.pt") as f:
    pkl = f.read([n for n in f.namelist() if n.endswith("data.pkl")][0])
    print(b"_rebuild_sparse_tensor" in pkl, b"_get_layout" in pkl, b"torch.sparse_csc" in pkl)
show(torch.load("rt.pt"))
"#,
        files,
    )
    .unwrap();
    let expected = [
        "c torch.sparse_csr torch.float32 (2, 3) [0, 2, 3] [0, 2, 2] [1.0, 2.0, 3.0]",
        "d torch.sparse_csc torch.float32 (2, 3) [0, 1, 1, 3] [0, 0, 1] [1.0, 2.0, 3.0]",
        "h torch.sparse_coo torch.float64 (3, 2) True [[0, 2]] [[1.0, 2.0], [3.0, 4.0]]",
        "s torch.sparse_coo torch.float32 (2, 3) False [[0, 1, 1, 0], [2, 0, 2, 2]] [3.0, 4.0, 5.0, 6.0]",
        "x torch.strided torch.float32 [0.0, 1.0, 2.0]",
    ];
    assert_eq!(out[..5], expected);
    assert_eq!(out[5], "True True True");
    assert_eq!(out[6..], expected);
}

/// What has no PyTorch counterpart to compare with: gradients PyTorch
/// cannot take (through a nonlinear unary op or index_select of a sparse
/// tensor, which it refuses for want of a sparse kernel), coalescing and
/// products at a size where the kernels' loops matter, checked against the
/// dense computation, and a CSR matrix's products agreeing with its COO
/// form.
#[test]
fn sparse_zipp_specific_behaviour() {
    let out = run(r#"
import torch
i = torch.tensor([[0, 1, 1, 0, 2], [2, 0, 2, 2, 1]])
v = torch.tensor([1.0, 2.0, 3.0, 4.0, 5.0], dtype=torch.float64, requires_grad=True)
s = torch.sparse_coo_tensor(i, v, (3, 3))
(torch.sin(s).to_dense() * torch.arange(9.0, dtype=torch.float64).reshape(3, 3)).sum().backward()
print([round(x, 12) for x in v.grad.tolist()])
v.grad = None
(s.index_select(0, torch.tensor([2, 0, 0])).to_dense() * torch.arange(9.0, dtype=torch.float64).reshape(3, 3)).sum().backward()
print(v.grad.tolist())
n, m, nnz = 400, 300, 20000
rows = torch.tensor([(k * 7919) % n for k in range(nnz)])
cols = torch.tensor([(k * 104729 + k // 3) % m for k in range(nnz)])
vals = torch.tensor([((k * 37) % 101) / 17.0 - 3.0 for k in range(nnz)], dtype=torch.float64)
a = torch.sparse_coo_tensor(torch.stack([rows, cols]), vals, (n, m))
dense = a.to_dense()
c = a.coalesce()
print(c._nnz() <= nnz, c.is_coalesced(), torch.equal(c.to_dense(), dense))
keys = c.indices()[0] * m + c.indices()[1]
print(bool((keys[1:] > keys[:-1]).all()))
x = torch.tensor([[((r * 13 + q * 7) % 29) / 7.0 for q in range(16)] for r in range(m)], dtype=torch.float64)
print(torch.allclose(torch.sparse.mm(a, x), dense @ x, rtol=1e-12, atol=1e-10))
print(torch.allclose(a.to_sparse_csr() @ x, dense @ x, rtol=1e-12, atol=1e-10))
print(torch.allclose(x.t() @ a.t().coalesce(), x.t() @ dense.t(), rtol=1e-12, atol=1e-10))
print(torch.allclose((a + a.t().coalesce().t()).to_dense(), 2 * dense), torch.allclose((a * a).to_dense(), dense * dense))
"#)
    .unwrap();
    assert_eq!(
        out,
        [
            "[0.567324370926, -1.248440509641, -4.949962483002, 0.567324370926, 1.985635298243]",
            "[13.0, 0.0, 0.0, 13.0, 1.0]",
            "True True True",
            "True",
            "True",
            "True",
            "True",
            "True True",
        ]
    );
}

/// The native loops behind the sparse kernels (keys, coalesce, merge,
/// sparse @ dense, scatter-add) and `index_select` store exactly the bytes
/// the JavaScript loops store, for every dtype they take (the others
/// decline to the JavaScript loop), and decline an index out of range so
/// the JavaScript loop reports it.
#[test]
fn sparse_native_loops_match_javascript() {
    let out = run(r#"
import torch
import _zipp_tensor as _k
BAD, CASES = [], [0]

def image(t):
    if isinstance(t, (list, tuple)):
        return [image(x) for x in t]
    if t.layout != torch.strided:
        return [image(t._indices()), image(t._values()), t.is_coalesced()] if t.layout == torch.sparse_coo else [image(t.crow_indices()), image(t.col_indices()), image(t.values())]
    return (str(t.dtype), _k.tobytes(t._s) if not t.dtype.is_complex else _k.tobytes(torch.view_as_real(t)._s))

def both(label, fn):
    CASES[0] += 1
    results = []
    for on in (False, True):
        _k._native(on)
        try:
            results.append(("ok", image(fn())))
        except Exception as e:
            results.append(("err", type(e).__name__, str(e)))
    if results[0] != results[1]:
        BAD.append(label)

def rand_sparse(n, m, nnz, dt, seed, block=()):
    rows = torch.tensor([(k * 7919 + seed) % n for k in range(nnz)])
    cols = torch.tensor([(k * 104729 + seed * 3 + k // 5) % m for k in range(nnz)])
    count = nnz
    for b in block:
        count *= b
    vals = torch.tensor([(((k + seed) * 37) % 101) / 7.0 - 7.0 for k in range(count)], dtype=torch.float64).reshape((nnz,) + tuple(block)).to(dt)
    return torch.sparse_coo_tensor(torch.stack([rows, cols]), vals, (n, m) + tuple(block))

for dt in (torch.float32, torch.float64, torch.float16, torch.bfloat16, torch.int64, torch.int32, torch.uint8, torch.bool, torch.complex64):
    s = rand_sparse(40, 30, 700, dt, 1)
    h = rand_sparse(40, 30, 300, dt, 2, (3,))
    both("coalesce %s" % dt, lambda: s.coalesce())
    both("coalesce hybrid %s" % dt, lambda: h.coalesce())
    both("to_dense %s" % dt, lambda: s.to_dense())
    both("add %s" % dt, lambda: s.coalesce() + rand_sparse(40, 30, 500, dt, 3).coalesce())
    both("add hybrid %s" % dt, lambda: h + rand_sparse(40, 30, 200, dt, 4, (3,)))
    both("index_select %s" % dt, lambda: torch.index_select(s.to_dense(), 1, torch.tensor([3, 0, -1, 7, 7])))
    if dt not in (torch.bool, torch.uint8):
        both("add alpha %s" % dt, lambda: torch.add(s, rand_sparse(40, 30, 500, dt, 3), alpha=3))
        both("dense add_ %s" % dt, lambda: torch.ones(40, 30, dtype=dt).add_(s))
    if dt not in (torch.bool,):
        both("spmm %s" % dt, lambda: torch.sparse.mm(s, torch.arange(90.0).reshape(30, 3).to(dt)))
        both("dense @ sparse %s" % dt, lambda: torch.arange(80.0).reshape(2, 40).to(dt) @ s)
both("spmm bad index", lambda: torch.sparse.mm(torch.sparse_coo_tensor(torch.tensor([[0, 5], [1, 1]]), torch.ones(2), (3, 3)), torch.ones(3, 40)))
both("to_dense bad index", lambda: torch.sparse_coo_tensor(torch.tensor([[0, 5], [1, 1]]), torch.ones(2, 40), (3, 3, 40)).to_dense())
both("index_select bad index", lambda: torch.index_select(torch.ones(5, 40), 0, torch.tensor([1, 5])))
_k._native(True)
print(CASES[0], BAD)
"#)
    .unwrap();
    assert_eq!(out, ["87 []"]);
}

