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

/// What has no PyTorch counterpart to compare with, beside item/tolist and
/// float() of complex elements (PyTorch's values: Python complex numbers,
/// and a refused nonzero imaginary part): view_as_real/view_as_complex
/// share storage and version counter, .real/.imag are copies (their setters write through),
/// complex randn's parts are N(0, 1/2), and ops without a complex kernel
/// refuse complex storages instead of reading them as reals.
#[test]
fn complex_zipp_specific_behaviour() {
    let out = run(r#"
import torch
z = torch.complex(torch.tensor([1.0, 2.0]), torch.tensor([3.0, -4.0]))
for f in (lambda: z[0].item(), lambda: z.tolist(), lambda: float(z[0])):
    try:
        print(f())
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
            "(1+3j)",
            "[(1+3j), (2-4j)]",
            "RuntimeError value cannot be converted to type double without overflow",
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

/// Python complex scalars at the tensor boundary, as PyTorch 2.11 prints
/// them: complex literals build complex tensors (dtype inference and
/// promotion with real tensors), item()/tolist()/iteration return Python
/// complex numbers (torch.fft results included), complex scalars mix into
/// tensor arithmetic, torch.full()/fill_() take them (a real dtype refuses
/// a nonzero imaginary part), complex()/float()/int() convert one-element
/// tensors with PyTorch's checked narrowing, and a 0-d complex tensor
/// formats as its value.
#[test]
fn complex_python_scalars_match_pytorch() {
    let out = lines(&run(r##"
import torch
def show(label, f):
    try:
        r = f()
        print(label, repr(r), type(r).__name__)
    except Exception as e:
        print(label, "!!", type(e).__name__, e)
t = torch.tensor([1 + 2j, -0.5j])
show("tensor", lambda: t)
show("dtype", lambda: t.dtype)
show("item", lambda: t[0].item())
show("tolist", lambda: t.tolist())
show("mul2j", lambda: t * 2j)
show("rmul", lambda: 2j * t)
show("add", lambda: t + (1 - 1j))
show("div", lambda: t / (1 + 1j))
show("pow", lambda: t ** 2)
show("complex", lambda: torch.complex(torch.tensor([1.0, 2.0]), torch.tensor([3.0, -4.0])))
show("complex_item", lambda: torch.complex(torch.tensor(1.0), torch.tensor(-2.0)).item())
show("fft", lambda: torch.fft.fft(torch.tensor([1.0, 2.0, 3.0, 4.0])))
show("fft_item", lambda: torch.fft.fft(torch.tensor([1.0, 2.0, 3.0, 4.0]))[1].item())
show("fft_tolist", lambda: torch.fft.fft(torch.tensor([1.0, 0.0, 0.0, 0.0])).tolist())
show("ifft", lambda: torch.fft.ifft(torch.tensor([1 + 1j, 2 - 1j])).tolist())
show("scalar_tensor", lambda: torch.tensor(3 - 4j))
show("scalar_abs", lambda: torch.tensor(3 - 4j).abs().item())
show("mixed_list", lambda: torch.tensor([1, 2.5, 3j]))
show("c64", lambda: torch.tensor([1j], dtype=torch.complex64).item())
show("full", lambda: torch.full((2,), 1 + 1j))
show("iter", lambda: [x.item() for x in torch.tensor([1j, 2j])])
show("float_err", lambda: float(torch.tensor(1j)))
show("complex_of_tensor", lambda: complex(torch.tensor(1 + 2j)))
show("eq", lambda: torch.tensor([1j]) == 1j)
show("sum_item", lambda: torch.tensor([1j, 2 + 1j]).sum().item())
show("real_imag", lambda: (torch.tensor([1 + 2j]).real.tolist(), torch.tensor([1 + 2j]).imag.tolist()))
show("angle", lambda: torch.tensor([1j]).angle().item())
show("polar", lambda: torch.polar(torch.tensor([2.0]), torch.tensor([0.0])).item())
show("view_as_complex", lambda: torch.view_as_complex(torch.tensor([[1.0, 2.0]])).tolist())
show("print", lambda: str(torch.tensor([[1 + 2j, 3.25 - 1j], [0j, -1j]])))
show("fill", lambda: torch.zeros(2, dtype=torch.complex128).fill_(1 - 1j))
show("setitem", lambda: (lambda z: (z.__setitem__(0, 5j), z)[1])(torch.zeros(2, dtype=torch.complex64)))
show("where", lambda: torch.where(torch.tensor([True, False]), torch.tensor([1j, 2j]), 3j))
show("default_complex", lambda: torch.tensor(1j).dtype)
show("float0", lambda: float(torch.tensor(1 + 0j)))
show("float1", lambda: float(torch.tensor(1 + 1j)))
show("int1", lambda: int(torch.tensor(1 + 1j)))
show("int0", lambda: int(torch.tensor(2 + 0j)))
show("cx_real", lambda: complex(torch.tensor(2.5)))
show("cx_int", lambda: complex(torch.tensor(3)))
show("cx_multi", lambda: complex(torch.tensor([1j, 2j])))
show("full", lambda: torch.full((2,), 1 + 1j))
show("full_dt", lambda: torch.full((2,), 1 + 1j, dtype=torch.complex128))
show("full_realdt", lambda: torch.full((2,), 1.5, dtype=torch.complex64))
show("full_cx_to_float", lambda: torch.full((2,), 1 + 1j, dtype=torch.float32))
show("fill_real_with_cx", lambda: torch.zeros(2).fill_(1j))
show("fill_cx", lambda: torch.zeros(2, dtype=torch.complex64).fill_(2 - 1j))
show("fill_cx_int", lambda: torch.zeros(2, dtype=torch.complex64).fill_(2))
show("format0d", lambda: format(torch.tensor(1 + 2j), ""))
show("add_real_cx", lambda: torch.tensor([1.0, 2.0]) + 1j)
show("int_tensor_cx", lambda: torch.tensor([1, 2]) * (1 + 1j))
show("double_cx", lambda: (torch.tensor([1.0], dtype=torch.float64) * 1j).dtype)
show("scalar_pow", lambda: 2 ** torch.tensor([1j]))
show("isclose", lambda: torch.tensor(1j).item() == 1j)
show("clamp", lambda: torch.tensor([1j]).sum())
show("tensor_from_cx_list_c128", lambda: torch.tensor([[1j, 2], [3, 4.5]], dtype=torch.complex128))
show("as_tensor", lambda: torch.as_tensor([1 + 1j, 2]))
show("mean", lambda: torch.tensor([1j, 3j]).mean().item())
"##)
    .unwrap()
    .join("\n"));
    let expected = lines(
        r##"tensor tensor([1.+2.0000j, -0.-0.5000j]) Tensor
dtype torch.complex64 dtype
item (1+2j) complex
tolist [(1+2j), (-0-0.5j)] list
mul2j tensor([-4.+2.j,  1.-0.j]) Tensor
rmul tensor([-4.+2.j,  1.-0.j]) Tensor
add tensor([2.+1.0000j, 1.-1.5000j]) Tensor
div tensor([ 1.5000+0.5000j, -0.2500-0.2500j]) Tensor
pow tensor([-3.0000+4.j, -0.2500+0.j]) Tensor
complex tensor([1.+3.j, 2.-4.j]) Tensor
complex_item (1-2j) complex
fft tensor([10.+0.j, -2.+2.j, -2.+0.j, -2.-2.j]) Tensor
fft_item (-2+2j) complex
fft_tolist [(1+0j), (1+0j), (1+0j), (1-0j)] list
ifft [(1.5+0j), (-0.5+1j)] list
scalar_tensor tensor(3.-4.j) Tensor
scalar_abs 5.0 float
mixed_list tensor([1.0000+0.j, 2.5000+0.j, 0.0000+3.j]) Tensor
c64 1j complex
full tensor([1.+1.j, 1.+1.j]) Tensor
iter [1j, 2j] list
float_err !! RuntimeError value cannot be converted to type double without overflow
complex_of_tensor (1+2j) complex
eq tensor([True]) Tensor
sum_item (2+2j) complex
real_imag ([1.0], [2.0]) tuple
angle 1.5707963705062866 float
polar (2+0j) complex
view_as_complex [(1+2j)] list
print 'tensor([[1.0000+2.j, 3.2500-1.j],\n        [0.0000+0.j, -0.0000-1.j]])' str
fill tensor([1.-1.j, 1.-1.j], dtype=torch.complex128) Tensor
setitem tensor([0.+5.j, 0.+0.j]) Tensor
where tensor([0.+1.j, 0.+3.j]) Tensor
default_complex torch.complex64 dtype
float0 1.0 float
float1 !! RuntimeError value cannot be converted to type double without overflow
int1 !! RuntimeError value cannot be converted to type int64_t without overflow
int0 2 int
cx_real (2.5+0j) complex
cx_int (3+0j) complex
cx_multi !! ValueError only one element tensors can be converted to Python scalars
full tensor([1.+1.j, 1.+1.j]) Tensor
full_dt tensor([1.+1.j, 1.+1.j], dtype=torch.complex128) Tensor
full_realdt tensor([1.5000+0.j, 1.5000+0.j]) Tensor
full_cx_to_float !! RuntimeError value cannot be converted to type float without overflow
fill_real_with_cx !! RuntimeError value cannot be converted to type float without overflow
fill_cx tensor([2.-1.j, 2.-1.j]) Tensor
fill_cx_int tensor([2.+0.j, 2.+0.j]) Tensor
format0d '(1+2j)' str
add_real_cx tensor([1.+1.j, 2.+1.j]) Tensor
int_tensor_cx tensor([1.+1.j, 2.+2.j]) Tensor
double_cx torch.complex128 dtype
scalar_pow tensor([0.7692+0.6390j]) Tensor
isclose True bool
clamp tensor(0.+1.j) Tensor
tensor_from_cx_list_c128 tensor([[0.0000+1.j, 2.0000+0.j],
        [3.0000+0.j, 4.5000+0.j]], dtype=torch.complex128) Tensor
as_tensor tensor([1.+1.j, 2.+0.j]) Tensor
mean 2j complex
"##,
    );
    assert_eq!(out, expected);
}
