//! complex64/complex128 tensors checked against CPU PyTorch 2.11.
//! `fixtures/torch_complex/complex_cases.py` runs unchanged under both;
//! `gen.py` writes its PyTorch results to `complex_expected.json` and
//! `text_expected.txt`. Values and gradients of a real loss (PyTorch's
//! conjugate convention) are compared for arithmetic with type promotion,
//! pow, the elementwise functions, reductions, matmul, shape ops, indexed
//! assignment, parts and conversions; printing, dtype rules and errors are
//! compared as text.
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

/// complex128 cases agree to 1e-12 relative (both compute in doubles);
/// complex64 ones to 2e-6 (Zipp computes each op in doubles and rounds once,
/// PyTorch in float).
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
    tol = 2e-6 if name.startswith("c64") else 1e-12
    worst = 0.0
    at = -1
    for i in range(len(e)):
        d = abs(g[i] - e[i]) / max(1.0, abs(e[i]))
        if d > worst:
            worst = d
            at = i
    if not worst <= tol:
        bad.append("%s: off by %.3g at %d (%r vs %r)" % (name, worst, at, g[at], e[at]))
extra = sorted(set(got) - set(expected))
print("compared", len(expected), bad, extra)
"#;

#[test]
fn complex_values_and_gradients_match_pytorch() {
    let source = format!(
        "{}\nimport json\nexpected = json.loads(r'''{}''')\n{}",
        include_str!("fixtures/torch_complex/complex_cases.py"),
        include_str!("fixtures/torch_complex/complex_expected.json"),
        COMPARE
    );
    let out = run(&source).unwrap();
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(out[0].starts_with("compared ") && out[0].ends_with(" [] []"), "{}", out[0]);
}

#[test]
fn complex_printing_dtypes_and_errors_match_pytorch() {
    let source = format!(
        "{}\nfor line in text():\n    print(line)\n",
        include_str!("fixtures/torch_complex/complex_cases.py")
    );
    let out = lines(&run(&source).unwrap().join("\n"));
    let expected = lines(include_str!("fixtures/torch_complex/text_expected.txt"));
    for (i, (g, e)) in out.iter().zip(&expected).enumerate() {
        assert_eq!(g, e, "line {i}");
    }
    assert_eq!(out.len(), expected.len(), "{out:#?}");
}

/// The torch-level ATen aliases (`grid_sampler`, `affine_grid_generator`,
/// `ctc_loss` with integer mode and reduction codes), LSTM/RNN (lower
/// precision) and GRU (own dtype) under CPU autocast(bfloat16), and torch.*
/// functions refusing an uninitialized parameter with PyTorch's ValueError
/// (its `.shape` still raising RuntimeError), as PyTorch 2.11 prints them.
#[test]
fn torch_level_aliases_rnn_autocast_and_lazy_parameters_match_pytorch() {
    let out = run(include_str!("fixtures/torch_complex/followups.py")).unwrap().join("\n");
    assert_eq!(lines(&out), lines(include_str!("fixtures/torch_complex/followups_expected.txt")));
}

/// torch.poisson draws with torch.distributions' sampler: any rate (a
/// product of uniforms would underflow near rate 745), zero and infinite
/// rates exact, and a generator's stream reproducible.
#[test]
fn poisson_handles_large_rates_and_generators() {
    let out = run(r#"
import torch
lam = torch.tensor([0.5, 3.0, 50.0, 1e4, 1e7, 0.0, float("inf")], dtype=torch.float64)
torch.manual_seed(1)
p = torch.poisson(lam.repeat(200).reshape(200, 7))
mean = p[:, :5].mean(0)
print(p.dtype, p.shape, float(p[:, 5].abs().max()), float(p[0, 6]))
print(bool(((mean - lam[:5]).abs() < 4 * (lam[:5] / 200).sqrt() + 0.05).all()), bool((p[:, :5] == p[:, :5].floor()).all()))
a = torch.poisson(lam[:5].float(), generator=torch.Generator().manual_seed(3))
b = torch.poisson(lam[:5].float(), generator=torch.Generator().manual_seed(3))
print(a.dtype, torch.equal(a, b))
"#)
    .unwrap();
    assert_eq!(out, ["torch.float64 torch.Size([200, 7]) 0.0 inf", "True True", "torch.float32 True"]);
}

/// Checkpoints: torch.load reads PyTorch's ComplexFloatStorage /
/// ComplexDoubleStorage records (interleaved pairs), including strided
/// views and a Parameter; torch.save writes the same bytes and class names
/// (a file PyTorch 2.11 loads back), and a Zipp checkpoint round-trips.
#[test]
fn complex_checkpoints_round_trip_with_pytorch() {
    let files = vec![(
        "complex.pt".to_owned(),
        include_bytes!("fixtures/torch_complex/complex.pt").to_vec(),
    )];
    let out = run_with(
        r#"
import torch
import zipfile
d = torch.load("complex.pt")
for k in sorted(d):
    v = d[k]
    if isinstance(v, torch.Tensor):
        print(k, type(v).__name__, v.dtype, tuple(v.shape), v.requires_grad, torch.view_as_real(v.detach()).reshape(-1).tolist())
    else:
        print(k, v)
z = torch.complex(torch.tensor([1.5, -2.0]), torch.tensor([0.25, 3.0]))
for t in (z, z.to(torch.complex128)):
    torch.save(t, "one.pt")
    with zipfile.ZipFile("one.pt") as f:
        print(list(f.read([n for n in f.namelist() if n.endswith("/data/0")][0])))
        pkl = f.read([n for n in f.namelist() if n.endswith("data.pkl")][0])
        print(b"ComplexFloatStorage" in pkl, b"ComplexDoubleStorage" in pkl)
torch.save({"a": z, "p": torch.nn.Parameter(z.to(torch.complex128))}, "rt.pt")
back = torch.load("rt.pt")
print(back["a"].dtype, torch.equal(back["a"], z), type(back["p"]).__name__, back["p"].dtype, torch.view_as_real(back["p"].detach()).reshape(-1).tolist())
"#,
        files,
    )
    .unwrap();
    assert_eq!(
        out,
        [
            "T Tensor torch.complex64 (3, 2) False [0.0, -0.0, 3.0, -1.5, 1.0, -0.5, 4.0, -2.0, 2.0, -1.0, 5.0, -2.5]",
            "c128 Tensor torch.complex128 (2, 3) False [0.0, 0.0, 1.25, -0.625, 2.5, -1.25, 3.75, -1.875, 5.0, -2.5, 6.25, -3.125]",
            "c64 Tensor torch.complex64 (2, 3) False [0.0, -0.0, 1.0, -0.5, 2.0, -1.0, 3.0, -1.5, 4.0, -2.0, 5.0, -2.5]",
            "dt [torch.complex64, torch.complex128]",
            "p Parameter torch.complex64 (3,) True [0.0, -0.0, 1.0, -0.5, 2.0, -1.0]",
            "slice Tensor torch.complex64 (2, 2) False [1.0, -0.5, 2.0, -1.0, 4.0, -2.0, 5.0, -2.5]",
            "[0, 0, 192, 63, 0, 0, 128, 62, 0, 0, 0, 192, 0, 0, 64, 64]",
            "True False",
            "[0, 0, 0, 0, 0, 0, 248, 63, 0, 0, 0, 0, 0, 0, 208, 63, 0, 0, 0, 0, 0, 0, 0, 192, 0, 0, 0, 0, 0, 0, 8, 64]",
            "False True",
            "torch.complex64 True Parameter torch.complex128 [1.5, 0.25, -2.0, 3.0]",
        ]
    );
}

/// What has no PyTorch counterpart to compare with: complex Python scalars
/// do not exist yet (item/tolist/float refuse, 0-d complex tensors carry
/// scalars instead), view_as_real/view_as_complex share storage and
/// version counter, .real/.imag are copies (their setters write through),
/// complex randn's parts are N(0, 1/2), and ops without a complex kernel
/// refuse complex storages instead of reading them as reals.
#[test]
fn complex_zipp_specific_behaviour() {
    let out = run(r#"
import torch
z = torch.complex(torch.tensor([1.0, 2.0]), torch.tensor([3.0, -4.0]))
for f in (lambda: z[0].item(), lambda: z.tolist(), lambda: float(z[0])):
    try:
        f()
        print("no error")
    except Exception as e:
        print(type(e).__name__, e)
two = torch.complex(torch.tensor(0.0), torch.tensor(1.0))
print(torch.view_as_real(z * two).tolist(), bool(z[0]), bool(z[0] * 0))
r = torch.view_as_real(z)
r[0, 1] = 7.0
print(torch.view_as_real(z).tolist())
w = torch.view_as_complex(r)
w.mul_(2)
print(r.tolist(), torch._k.aversion(z._s) == torch._k.aversion(r._s))
re = z.real
re[0] = 100.0
print(torch.view_as_real(z)[0].tolist())
z.imag = torch.tensor([0.5, 0.5])
print(torch.view_as_real(z).tolist())
torch.manual_seed(0)
n = torch.randn(20000, dtype=torch.complex128)
p = torch.view_as_real(n)
print(abs(float(p.mean())) < 0.02, abs(float(p[:, 0].var()) - 0.5) < 0.03, abs(float(p[:, 1].var()) - 0.5) < 0.03)
for f in (lambda: z.max(), lambda: torch.softmax(z, 0), lambda: z < z, lambda: z.relu(), lambda: torch.linalg.det(torch.ones(2, 2, dtype=torch.complex64))):
    try:
        f()
        print("no error")
    except Exception as e:
        print(type(e).__name__)
"#)
    .unwrap();
    assert_eq!(
        out,
        [
            "NotImplementedError complex Python scalars are not supported yet",
            "NotImplementedError complex Python scalars are not supported yet",
            "TypeError can't convert complex to float",
            "[[-3.0, 1.0], [4.0, 2.0]] True False",
            "[[1.0, 7.0], [2.0, -4.0]]",
            "[[2.0, 14.0], [4.0, -8.0]] True",
            "[2.0, 14.0]",
            "[[2.0, 0.5], [4.0, 0.5]]",
            "True True True",
            "RuntimeError",
            "RuntimeError",
            "RuntimeError",
            "RuntimeError",
            "NotImplementedError",
        ]
    );
}
