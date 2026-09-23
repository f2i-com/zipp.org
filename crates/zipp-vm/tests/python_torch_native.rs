//! The native tensor loops behind `_zipp_tensor` (`vm::py_tensor`): every
//! kernel they replace must give the JavaScript loop's exact bytes, they must
//! be charged to the instruction budget (or decline, leaving the budget to
//! run out in the interpreted loop exactly as before), and ordinary
//! JavaScript must not be able to see them.
#![cfg(feature = "python")]
use zipp_vm::embed::{compile_script, JsValue};
use zipp_vm::frontend::compile_python_program;

struct Run {
    output: Result<Vec<String>, String>,
    steps: u64,
}

fn run(source: &str, budget: u64, trace: bool) -> Run {
    run_traced(source, budget, if trace { Some(1 << 12) } else { None })
}

fn run_traced(source: &str, budget: u64, trace: Option<usize>) -> Run {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &[], &[], false).expect("compiles");
            let state = compiled.state_mut();
            state.set_limits(budget, None);
            if let Some(rows) = trace {
                state.start_trace(rows);
            }
            let result = state.run_init();
            let steps = state.steps_used();
            if trace.is_some() {
                assert!(!state.trace_truncated(), "the trace must cover the whole run");
            }
            Run {
                output: result.map(|_| state.take_output()),
                steps,
            }
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

/// Runs each case with the native loops off and on and compares the result
/// storages byte for byte (`tobytes`), errors included.
const DIFF: &str = r#"
import _zipp_tensor as _k

INF = float("inf")
NAN = float("nan")
SPECIAL = [0.0, -0.0, 1.0, -1.0, 0.5, -0.5, 2.5, -2.5, 1e-45, -3e-39, 1e30, -1e30, 3.4e38, 1e300, INF, -INF, NAN, 20.5, -20.5, 7.0, 3.0, -7.0, 0.1, 0.7071, 1e-8]
FLOATS = ["float32", "float64"]


def numel(shape):
    n = 1
    for d in shape:
        n *= d
    return n


def values(n, seed, dtype):
    out = []
    for i in range(n):
        j = (i * 7 + seed * 13) % 97
        if j < len(SPECIAL) and (i + seed) % 3 == 0:
            v = SPECIAL[j]
        else:
            v = ((i * 37 + seed * 11) % 23 - 11) / 4.0 + (seed % 5) * 0.137
        if dtype in ("int64", "int32"):
            if v != v or v in (INF, -INF) or abs(v) > 1e15:
                v = float(i % 9 - 4)
            v = int(v)
        elif dtype == "uint8":
            if v != v or v in (INF, -INF) or abs(v) > 1e15:
                v = float(i % 9)
            v = int(abs(v)) % 256
        elif dtype == "bool":
            v = 1 if (i + seed) % 3 else 0
        out.append(v)
    return out


def storage(dtype, shape, seed):
    return _k.from_flat(dtype, values(numel(shape), seed, dtype))


CASES = [0]
BAD = []


def image(r):
    if r is None:
        return None
    if isinstance(r, tuple):
        return tuple(image(x) for x in r)
    if isinstance(r, (bool, int, float)):
        return repr(r)
    try:
        return (_k.dtype(r), _k.tobytes(r))
    except Exception:
        # An int64 storage holding inf/nan has no int64 bytes.
        return (_k.dtype(r), _k.tobytes(_k.astype(r, "float64")))


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


seed = 0
for op in ["add", "sub", "mul", "div", "pow", "max", "min", "eq", "ne", "lt", "le", "gt", "ge", "and", "or", "xor", "floordiv", "mod", "atan2", "fmod"]:
    for dta in ["float32", "float64", "int64", "bool"]:
        for dtb in ["float32", "int64"]:
            for sa, sb in [((96,), (96,)), ((12, 8), (8,)), ((12, 8), (12, 1)), ((70,), ()), ((), (66,)), ((4, 1, 6), (1, 5, 6))]:
                seed += 1
                A = storage(dta, sa, seed)
                B = storage(dtb, sb, seed + 1)
                both("binary %s %s %s %s %s" % (op, dta, dtb, sa, sb), lambda: _k.binary(op, A, sa, B, sb))
                if op in ("add", "div", "pow", "max"):
                    both("binary %s %s %s want" % (op, dta, dtb), lambda: _k.binary(op, A, sa, B, sb, "float64"))
for op in ["neg", "relu", "exp", "log", "tanh", "sigmoid", "sqrt", "square", "abs", "sign", "silu", "gelu", "gelu_grad", "reciprocal", "rsqrt", "log1p", "expm1", "softplus", "sin", "cos", "floor", "ceil", "trunc", "isfinite", "isnan", "not", "frac", "tan", "atan", "log2", "log10", "isinf", "exp2", "sinh", "cosh", "asin", "acos", "asinh", "acosh", "atanh", "round", "erf"]:
    for dt in ["float32", "float64", "int64", "bool", "uint8"]:
        seed += 1
        A = storage(dt, (70,), seed)
        both("unary %s %s" % (op, dt), lambda: _k.unary(op, A))
for dt in ["float32", "float64", "int64"]:
    for lo, hi in ((None, 1.5), (-0.5, None), (-2, 3), (NAN, 1.0)):
        seed += 1
        A = storage(dt, (100,), seed)
        both("clamp %s %s %s" % (dt, lo, hi), lambda: _k.unary("clamp", A, lo, hi))
for op in ["sum", "mean", "prod", "max", "min", "argmax", "argmin", "all", "any"]:
    for dt in ["float32", "float64", "int64", "bool", "uint8"]:
        for shape, dims in [((256,), None), ((16, 12), (0,)), ((16, 12), (1,)), ((4, 6, 8), (1,)), ((4, 6, 8), (0, 2)), ((3, 70), (-1,))]:
            for precise in (False, True):
                seed += 1
                A = storage(dt, shape, seed)
                both("reduce %s %s %s %s %s" % (op, dt, shape, dims, precise), lambda: _k.reduce(op, A, shape, dims, True, precise))
for dt in ["float32", "float64", "int64"]:
    for shape, dim in (((8, 10), 1), ((8, 10), 0), ((2, 3, 70), -1)):
        for log in (False, True):
            seed += 1
            A = storage(dt, shape, seed)
            both("softmax %s %s %d %s" % (dt, shape, dim, log), lambda: _k.softmax(A, shape, dim, log))
for dta in ["float32", "float64", "int64"]:
    for dtb in ["float32", "float64"]:
        for sa, sb in [((5, 7), (7, 3)), ((16, 32), (32, 20)), ((64,), (64, 9)), ((9, 64), (64,)), ((64,), (64,)), ((3, 4, 5), (5, 6)), ((2, 1, 4, 5), (3, 5, 2)), ((0, 5), (5, 3)), ((4, 0), (0, 3))]:
            seed += 1
            A = storage(dta, sa, seed)
            B = storage(dtb, sb, seed + 3)
            both("matmul %s %s %s %s" % (dta, dtb, sa, sb), lambda: _k.matmul(A, sa, B, sb))
A = storage("float32", (8, 8), 5)
both("matmul self", lambda: _k.matmul(A, (8, 8), A, (8, 8)))
both("matmul mismatch", lambda: _k.matmul(A, (8, 8), A, (4, 8)))
for dt in FLOATS:
    for xs, ws, st, pd, dl, g in [
        ((2, 3, 9, 9), (4, 3, 3, 3), (1, 1), (0, 0), (1, 1), 1),
        ((2, 4, 9, 8), (6, 2, 3, 2), (2, 1), (1, 2), (1, 2), 2),
        ((1, 2, 7, 7), (2, 1, 3, 3), (1, 2), (2, 1), (2, 1), 2),
    ]:
        seed += 1
        X = storage(dt, xs, seed)
        W = storage(dt, ws, seed + 1)
        for Bi in (None, storage(dt, (ws[0],), seed + 2)):
            both("conv2d %s %s %s" % (dt, xs, ws), lambda: _k.conv2d(X, xs, W, ws, Bi, st, pd, dl, g))
        ho = (xs[2] + 2 * pd[0] - dl[0] * (ws[2] - 1) - 1) // st[0] + 1
        wo = (xs[3] + 2 * pd[1] - dl[1] * (ws[3] - 1) - 1) // st[1] + 1
        G = storage(dt, (xs[0], ws[0], ho, wo), seed + 5)
        both("conv2d_backward %s %s %s" % (dt, xs, ws), lambda: _k.conv2d_backward(X, xs, W, ws, G, st, pd, dl, g))
    for xs, ws in (((2, 3, 20), (4, 3, 5)), ((3, 2, 9), (1, 2, 1))):
        seed += 1
        X = storage(dt, xs, seed)
        W = storage(dt, ws, seed + 1)
        for Bi in (None, storage(dt, (ws[0],), seed + 2)):
            both("conv1d %s %s" % (dt, xs), lambda: _k.conv1d(X, xs, W, ws, Bi))
        lo = xs[2] - ws[2] + 1
        G = _k.from_flat(dt, [0.0 if i % 4 == 0 else v for i, v in enumerate(values(xs[0] * ws[0] * lo, seed + 9, dt))])
        both("conv1d_backward %s %s" % (dt, xs), lambda: _k.conv1d_backward(X, xs, W, ws, G))
for dt in ["float32", "float64", "int64", "bool"]:
    seed += 1
    A = storage(dt, (6, 5, 7), seed)
    for perm in ((1, 0, 2), (2, 0, 1)):
        both("permute %s %s" % (dt, perm), lambda: _k.permute(A, (6, 5, 7), perm))
    M = storage(dt, (20, 13), seed)
    both("transpose %s" % dt, lambda: _k.permute(M, (20, 13), (1, 0)))
    V = storage(dt, (1, 13), seed)
    both("expand %s" % dt, lambda: _k.expand(V, (1, 13), (4, 9, 13)))
    both("slice %s" % dt, lambda: _k.slice(A, (6, 5, 7), [(1, None, 2), None, (0, 6, 1)]))
    C = storage("bool", (6, 1, 7), seed + 1)
    Bv = storage(dt, (5, 1), seed + 2)
    both("where %s" % dt, lambda: _k.where(C, (6, 1, 7), A, (6, 5, 7), Bv, (5, 1)))
    both("all_finite %s" % dt, lambda: _k.all_finite(A))
print("cases", CASES[0], "mismatches", len(BAD), BAD[:5])
"#;

#[test]
fn native_loops_give_the_javascript_loops_bytes() {
    let run = run(DIFF, 20_000_000_000, false);
    let out = run.output.expect("runs");
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(out[0].contains("mismatches 0 []"), "{}", out[0]);
    let cases: usize = out[0].split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(cases > 1500, "{}", out[0]);
}

const SETUP: &str = r#"
import _zipp_tensor as _k
a = _k.from_flat("float32", [((i * 7) % 13) / 13.0 for i in range(4096)])
b = _k.from_flat("float32", [((i * 5) % 11) / 11.0 for i in range(4096)])
"#;

/// A 64x64x64 float32 matmul checksum, with the native loops on or off.
fn matmul_program(native: bool) -> String {
    format!(
        "{SETUP}_k._native({})
out, shape = _k.matmul(a, (64, 64), b, (64, 64))
print(sum(_k.to_list(out)), list(shape))
",
        if native { "True" } else { "False" }
    )
}

#[test]
fn a_native_kernel_is_charged_for_its_work() {
    let native = run(&matmul_program(true), 1_000_000_000, false);
    let interpreted = run(&matmul_program(false), 1_000_000_000, false);
    assert_eq!(native.output.clone().unwrap(), interpreted.output.clone().unwrap());
    // 262,144 multiply-adds at the kernel's price: charged, and in full.
    let macs = 64 * 64 * 64;
    assert!(native.steps > 4 * macs, "native run charged only {} steps", native.steps);
    // ... and below what the interpreted loop spends on the same work.
    assert!(native.steps < interpreted.steps, "{} vs {}", native.steps, interpreted.steps);
}

#[test]
fn a_kernel_the_budget_cannot_cover_runs_out_as_before() {
    // The setup fits; the matmul's price (over a million steps) does not.
    // The native loop declines and the interpreted one exhausts the budget.
    let setup = run(SETUP, 1_000_000_000, false).steps;
    let budget = setup + 500_000;
    for native in [true, false] {
        let r = run(&matmul_program(native), budget, false);
        let err = r.output.expect_err("the budget must run out");
        assert!(err.contains("instruction budget"), "{err}");
        assert_eq!(r.steps, budget, "a stopped script reports exactly the cap");
    }
}

/// A 12x12x12 matmul: small enough to trace every instruction.
fn small_matmul_program(native: bool) -> String {
    format!(
        "import _zipp_tensor as _k
_k._native({})
a = _k.from_flat(\"float32\", [i / 7.0 for i in range(144)])
out, shape = _k.matmul(a, (12, 12), a, (12, 12))
print(sum(_k.to_list(out)))
",
        if native { "True" } else { "False" }
    )
}

#[test]
fn a_traced_run_takes_the_interpreted_loops() {
    let plain = run(&small_matmul_program(true), 1_000_000_000, false);
    let interpreted = run(&small_matmul_program(false), 1_000_000_000, false);
    let rows = usize::try_from(interpreted.steps).unwrap() + 100_000;
    let traced = run_traced(&small_matmul_program(true), 1_000_000_000, Some(rows));
    assert!(plain.steps < interpreted.steps, "the untraced run takes the native loop");
    assert_eq!(traced.output.clone().unwrap(), plain.output.unwrap());
    // Tracing records every instruction, so the kernel ran interpreted: the
    // interpreted loop's steps plus the declined native call's few.
    assert!(
        traced.steps >= interpreted.steps && traced.steps < interpreted.steps + 1_000,
        "{} vs {}",
        traced.steps,
        interpreted.steps
    );
}

#[test]
fn javascript_cannot_see_the_native_loops() {
    let mut st = compile_script(
        "function probe() { return typeof __zipp_py_native + ',' + Object.getOwnPropertyNames(globalThis).includes('__zipp_py_native') + ',' + ('__zipp_py_native' in globalThis); }",
    )
    .expect("compiles");
    st.run_init().expect("runs");
    let got = st.call_global("probe", &[]).expect("calls");
    assert!(matches!(&got, JsValue::String(s) if s == "undefined,false,false"), "{got:?}");
}
