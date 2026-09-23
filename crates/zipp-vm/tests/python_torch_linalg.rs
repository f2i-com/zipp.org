//! The bundled `torch.linalg` (and the top-level aliases it installs:
//! `torch.det`, `torch.svd`, `Tensor.inverse`, ...) checked against CPU
//! PyTorch 2.11. `fixtures/torch_linalg/linalg_cases.py` runs unchanged under
//! both; `gen.py` writes its PyTorch results to `linalg_expected.json` and
//! `errors_expected.txt`. Every function is compared on its forward values
//! and, where PyTorch differentiates it, on the gradient of a scalar loss
//! (second derivatives for inv, det, solve, cholesky and eigvalsh). The
//! factorizations run natively (`vm::py_tensor::linalg`) when the engine
//! takes them and as torch_linalg.py's Python algorithms otherwise; both
//! paths are checked against PyTorch.
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
            state.set_limits(2_000_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Float64 cases agree to 1e-12 relative (Zipp factors in doubles, LAPACK
/// in doubles); float32 cases to 2e-6 (Zipp computes in doubles and rounds
/// once, PyTorch rounds every float32 step).
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

/// Forward values and gradients of every torch.linalg function (det,
/// slogdet, logdet, inv/inv_ex, solve/solve_ex incl. left=False, vector and
/// broadcast right-hand sides, solve_triangular, cholesky/cholesky_ex,
/// cholesky_solve/inverse, qr in every mode, eigh/eigvalsh with UPLO,
/// svd/svdvals incl. full_matrices, pinv (hermitian, atol/rtol),
/// matrix_rank, lstsq with each driver, vector/matrix norms and norm, cond,
/// matrix_power, matrix_exp, cross, multi_dot, vecdot, diagonal, vander,
/// householder_product, eig/eigvals (complex results, real and complex
/// spectra), lu/lu_factor/lu_solve with their gradients,
/// tensorinv/tensorsolve) and the torch.* aliases.
#[test]
fn linalg_matches_pytorch_values_and_gradients() {
    let source = format!(
        "{}\nimport json\nexpected = json.loads(r'''{}''')\n{}",
        include_str!("fixtures/torch_linalg/linalg_cases.py"),
        include_str!("fixtures/torch_linalg/linalg_expected.json"),
        COMPARE
    );
    assert_eq!(run(&source).unwrap(), ["compared 138 [] []"]);
}

/// The same parity with the native loops off: torch_linalg.py's Python
/// algorithms (the path a declined native kernel takes) match PyTorch too.
#[test]
fn linalg_python_fallback_matches_pytorch() {
    let source = format!(
        "import _zipp_tensor\n_zipp_tensor._native(False)\n{}\nimport json\nexpected = json.loads(r'''{}''')\n{}",
        include_str!("fixtures/torch_linalg/linalg_cases.py"),
        include_str!("fixtures/torch_linalg/linalg_expected.json"),
        COMPARE
    );
    assert_eq!(run(&source).unwrap(), ["compared 138 [] []"]);
}

/// LinAlgError messages (with PyTorch's batch-element prefix), shape and
/// dtype errors, unsupported orders and modes, QR's non-differentiable
/// modes, and the version-counter error for in-place writes to tensors a
/// decomposition saved.
#[test]
fn linalg_errors_match_pytorch() {
    let source = format!(
        "{}\nfor line in errors():\n    print(line)\n",
        include_str!("fixtures/torch_linalg/linalg_cases.py")
    );
    let out = lines(&run(&source).unwrap().join("\n"));
    assert_eq!(out, lines(include_str!("fixtures/torch_linalg/errors_expected.txt")));
}

/// What has no PyTorch counterpart to compare with: eig's order and
/// normalization conventions on a rotation, the aliases after `import
/// torch.linalg`, and moderately sized factorizations staying accurate.
#[test]
fn linalg_zipp_specific_behaviour() {
    let source = r#"
import torch
import torch.linalg as LA
rot = torch.tensor([[0.0, -1.0], [1.0, 0.0]])
lam, vec = LA.eig(rot)
print("eig:", lam.dtype, torch.view_as_real(lam).tolist(), float((rot.to(lam.dtype) @ vec - vec * lam).abs().max()) < 1e-6)
print("eigvals:", torch.view_as_real(LA.eigvals(rot.double())).tolist())
print(issubclass(LA.LinAlgError, RuntimeError))
print(all(hasattr(torch, n) for n in ("det", "logdet", "slogdet", "inverse", "cholesky", "qr", "svd", "pinverse", "matrix_power", "matrix_exp", "cholesky_solve", "cholesky_inverse")))
print(all(hasattr(torch.Tensor, n) for n in ("det", "logdet", "slogdet", "inverse", "cholesky", "qr", "svd", "pinverse", "matrix_power", "matrix_exp")))
n = 12
A = torch.tensor([[1.0 / (i + j + 1) + (2.0 if i == j else 0.0) for j in range(n)] for i in range(n)], dtype=torch.float64)
w, V = LA.eigh(A)
print("eigh", float((V @ torch.diag(w) @ V.T - A).abs().max()) < 1e-12, float((V.T @ V - torch.eye(n, dtype=torch.float64)).abs().max()) < 1e-12)
U, S, Vh = LA.svd(A[:, :7])
print("svd", float((U[:, :7] @ torch.diag(S) @ Vh - A[:, :7]).abs().max()) < 1e-12, float((U.T @ U - torch.eye(n, dtype=torch.float64)).abs().max()) < 1e-12)
Q, R = LA.qr(A)
print("qr", float((Q @ R - A).abs().max()) < 1e-12)
ones = torch.ones(5, 3, dtype=torch.float64)
U, S, Vh = LA.svd(ones)
I5 = torch.eye(5, dtype=torch.float64)
print("rank1 svd", float((U.T @ U - I5).abs().max()) < 1e-12, float((Vh @ Vh.T - torch.eye(3, dtype=torch.float64)).abs().max()) < 1e-12, float((U[:, :3] @ torch.diag(S) @ Vh - ones).abs().max()) < 1e-12)
print("inv", float((LA.inv(A) @ A - torch.eye(n, dtype=torch.float64)).abs().max()) < 1e-10)
T = torch.triu(A) + torch.diag(torch.arange(n, dtype=torch.float64))
lam, vec = LA.eig(Q @ T @ Q.T)
print("eig", float((torch.sort(lam.real).values - torch.sort(torch.diagonal(T)).values).abs().max()) < 1e-9, float(((Q @ T @ Q.T).to(vec.dtype) @ vec - vec * lam).abs().max()) < 1e-9)
r = LA.slogdet(A)
print(r)
"#;
    let out = run(source).unwrap().join("\n");
    let expected = [
        "eig: torch.complex64 [[0.0, 1.0], [0.0, -1.0]] True",
        "eigvals: [[0.0, 1.0], [0.0, -1.0]]",
        "True",
        "True",
        "True",
        "eigh True True",
        "svd True True",
        "qr True",
        "rank1 svd True True True",
        "inv True",
        "eig True True",
    ];
    let got = lines(&out);
    assert_eq!(&got[..expected.len()], &expected[..], "{out}");
    assert!(got[expected.len()].starts_with("torch.return_types.linalg_slogdet("), "{out}");
}

/// The native factorizations and torch_linalg.py's Python algorithms
/// agree on moderately sized matrices (the eigenproblems up to the
/// conventions they share: ascending/descending order, the sign and phase
/// rules), and eig's eigenvectors follow PyTorch's normalization (unit
/// norm, the largest component real and positive).
#[test]
fn linalg_native_and_python_paths_agree() {
    let source = r#"
import math
import torch
import torch.linalg as LA
import _zipp_tensor as _k

def mat(m, n, seed, batch=()):
    count = 1
    for b in batch:
        count *= b
    vals = [math.sin(1.3 * i + 0.7 * j + seed + 2.1 * b) + math.cos(0.37 * (i * n + j) + b) + math.sin(0.11 * i * i * j + 0.3 * b) + (0.9 + 0.31 * i if i == j else 0.0) for b in range(count) for i in range(m) for j in range(n)]
    return torch.tensor(vals, dtype=torch.float64).reshape(tuple(batch) + (m, n))

A = mat(14, 14, 0.3)
S = A + A.T
P = A @ A.T + 14 * torch.eye(14, dtype=torch.float64)
T = mat(11, 6, 1.1, (2,))
W = mat(5, 9, 0.7)
B = mat(14, 3, 2.2)
def results():
    lam, vec = LA.eig(A)
    return [LA.det(A), LA.slogdet(A).logabsdet, LA.inv(A), LA.solve(A, B), LA.solve_triangular(torch.triu(A), B, upper=True),
            LA.cholesky(P), LA.qr(A).Q, LA.qr(T, mode="complete").R, LA.eigh(S).eigenvalues, LA.eigh(S).eigenvectors,
            LA.svd(T).U, LA.svd(T).S, LA.svd(W).Vh, LA.svd(W, full_matrices=False).U, LA.pinv(T), LA.matrix_rank(W),
            torch.view_as_real(lam), torch.view_as_real(vec), LA.lu_factor(W).LU, LA.lu(T).L, LA.cholesky_ex(-P).info]
_k._native(True)
fast = results()
_k._native(False)
slow = results()
worst = 0.0
for x, y in zip(fast, slow):
    x = x.double()
    y = y.double()
    worst = max(worst, float((x - y).abs().max()) / max(1.0, float(y.abs().max())))
print("agree", worst < 1e-11)
_k._native(True)
lam, vec = LA.eig(A)
norms = torch.view_as_real(vec).pow(2).sum(-1).sum(0)
cols = torch.view_as_real(vec)
ok = True
for j in range(14):
    mags = [math.hypot(cols[i, j, 0].item(), cols[i, j, 1].item()) for i in range(14)]
    k = mags.index(max(mags))
    ok = ok and cols[k, j, 1].item() == 0.0 and cols[k, j, 0].item() > 0
print("normalized", float((norms - 1).abs().max()) < 1e-12, ok)
"#;
    let out = run(source).unwrap();
    assert_eq!(out, ["agree True", "normalized True True"], "{out:#?}");
}
