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
# matmul with a transposed right operand (F.linear's weight): the bytes of
# the plain product with that operand transposed into a copy.
for dta in ["float32", "float64", "int64"]:
    for dtb in ["float32", "float64"]:
        for sa, sb in [((16, 32), (64, 32)), ((4, 10, 12), (48, 12)), ((12,), (7, 12)), ((3, 0), (5, 0)), ((5, 7), (3, 7)), ((2, 3, 70), (9, 70)), ((0, 4), (6, 4))]:
            seed += 1
            A = storage(dta, sa, seed)
            B = storage(dtb, sb, seed + 3)
            both("matmul_t %s %s %s %s" % (dta, dtb, sa, sb), lambda: _k.matmul(A, sa, B, sb, True))
            for on in (False, True):
                _k._native(on)
                Bt, st = _k.permute(B, sb, [1, 0])
                if image(_k.matmul(A, sa, B, sb, True)) != image(_k.matmul(A, sa, Bt, st)):
                    BAD.append("matmul_t vs transposed copy %s %s %s %s %s" % (dta, dtb, sa, sb, on))
A = storage("float32", (8, 8), 5)
both("matmul_t self", lambda: _k.matmul(A, (8, 8), A, (8, 8), True))
both("matmul_t mismatch", lambda: _k.matmul(A, (8, 8), A, (4, 16), True))
both("matmul_t 3-d", lambda: _k.matmul(A, (8, 8), A, (2, 4, 8), True))
# max_pool2d and its gradient: dims (Kh, Kw, Sh, Sw, Ph, Pw, Dh, Dw).
for dt in ["float32", "float64", "float16", "bfloat16"]:
    for kd in [(2, 2, 2, 2, 0, 0, 1, 1), (2, 2, 3, 3, 0, 0, 1, 1), (2, 2, 2, 3, 1, 1, 1, 1), (3, 3, 3, 3, 1, 1, 1, 1), (2, 2, 3, 3, 0, 0, 2, 2),
               (1, 2, 1, 2, 0, 0, 1, 1), (3, 1, 3, 2, 1, 0, 1, 1), (3, 3, 1, 1, 1, 1, 1, 1), (2, 3, 1, 2, 0, 1, 2, 1)]:
        kh, kw, sh, sw, ph, pw, dh, dw = kd
        xs = (2, 3, 9, 8)
        ho = (xs[2] + 2 * ph - dh * (kh - 1) - 1) // sh + 1
        wo = (xs[3] + 2 * pw - dw * (kw - 1) - 1) // sw + 1
        dims = kd + (ho, wo)
        seed += 1
        X = storage("float32", xs, seed) if dt in ("float16", "bfloat16") else storage(dt, xs, seed)
        if dt in ("float16", "bfloat16"):
            X = _k.astype(X, dt)
        both("max_pool2d %s %s" % (dt, kd), lambda: _k.max_pool2d(X, xs, dims))
        G = storage("float32", (2, 3, ho, wo), seed + 1)
        if dt != "float32":
            G = _k.astype(G, dt)
        I = _k.max_pool2d(X, xs, dims)[1]
        both("max_pool2d_backward %s %s" % (dt, kd), lambda: _k.max_pool2d_backward(G, I, xs, dims))
        J = _k.from_flat("int64", [(i * 5) % 11 for i in range(2 * 3 * ho * wo)])
        both("max_pool2d_backward index %s %s" % (dt, kd), lambda: _k.max_pool2d_backward(G, J, xs, dims))
# binary with a Python number operand: `binary` against that number's 1-element storage.
for op in ["add", "sub", "mul", "div", "pow", "max", "lt", "eq", "floordiv"]:
    for dt, v, sdt, want in [("float32", 2.5, "float32", "float32"), ("float32", -0.0, "float32", "float32"), ("int64", 3, "int64", "int64"), ("float16", 0.1, "float32", "float16"), ("float64", 1e-3, "float64", "float64")]:
        for flip in (False, True):
            seed += 1
            A = storage("float32", (70,), seed) if dt == "float16" else storage(dt, (70,), seed)
            if dt == "float16":
                A = _k.astype(A, dt)
            both("binary_scalar %s %s %s" % (op, dt, flip), lambda: _k.binary_scalar(op, A, (70,), v, sdt, want, flip))
            for on in (False, True):
                _k._native(on)
                S = _k.full(sdt, 1, v)
                ref = _k.binary(op, S, (), A, (70,), want) if flip else _k.binary(op, A, (70,), S, (), want)
                if image(_k.binary_scalar(op, A, (70,), v, sdt, want, flip)) != image(ref):
                    BAD.append("binary_scalar vs full %s %s %s %s" % (op, dt, flip, on))
# float16 (a Float16Array the native loops read and copy) and bfloat16 (a
# Uint16Array of bits, decoded before any arithmetic and only copied as it
# is): every kernel, alone and mixed with float32/float64.
for dt in ["float16", "bfloat16"]:
    for op in ["add", "sub", "mul", "div", "pow", "max", "min", "lt", "eq", "and", "floordiv", "mod", "atan2"]:
        for sa, sb in [((96,), (96,)), ((12, 8), (8,)), ((4, 1, 6), (1, 5, 6)), ((70,), ())]:
            seed += 1
            A = storage(dt, sa, seed)
            B = storage(dt, sb, seed + 1)
            F = storage("float32", sb, seed + 2)
            both("half binary %s %s %s %s" % (op, dt, sa, sb), lambda: _k.binary(op, A, sa, B, sb))
            both("half binary %s %s f32 %s" % (op, dt, sa), lambda: _k.binary(op, A, sa, F, sb))
            both("half binary %s %s want %s" % (op, dt, sa), lambda: _k.binary(op, F, sb, A, sa, "float64"))
    for op in ["neg", "relu", "exp", "log", "tanh", "sigmoid", "sqrt", "abs", "sign", "gelu", "reciprocal", "floor", "isnan", "isinf", "sin", "atanh"]:
        seed += 1
        A = storage(dt, (70,), seed)
        both("half unary %s %s" % (op, dt), lambda: _k.unary(op, A))
    seed += 1
    A = storage(dt, (100,), seed)
    both("half clamp %s" % dt, lambda: _k.unary("clamp", A, -0.5, 1.5))
    for op in ["sum", "mean", "prod", "max", "min", "argmax", "argmin", "all", "any"]:
        for shape, dims in [((256,), None), ((16, 12), (0,)), ((4, 6, 8), (1,)), ((4, 6, 8), (0, 2))]:
            for precise in (False, True):
                seed += 1
                A = storage(dt, shape, seed)
                both("half reduce %s %s %s %s %s" % (op, dt, shape, dims, precise), lambda: _k.reduce(op, A, shape, dims, True, precise))
    for shape, dim in (((8, 10), 1), ((2, 3, 70), -1)):
        for log in (False, True):
            seed += 1
            A = storage(dt, shape, seed)
            both("half softmax %s %s %s" % (dt, shape, log), lambda: _k.softmax(A, shape, dim, log))
    for dtb in [dt, "float32", "float64"]:
        for sa, sb in [((5, 7), (7, 3)), ((16, 32), (32, 20)), ((64,), (64, 9)), ((3, 4, 5), (5, 6))]:
            seed += 1
            A = storage(dt, sa, seed)
            B = storage(dtb, sb, seed + 3)
            both("half matmul %s %s %s" % (dt, dtb, sa), lambda: _k.matmul(A, sa, B, sb))
        seed += 1
        A = storage(dt, (4, 10, 12), seed)
        B = storage(dtb, (9, 12), seed + 1)
        both("half matmul_t %s %s" % (dt, dtb), lambda: _k.matmul(A, (4, 10, 12), B, (9, 12), True))
    for xs, ws, st, pd, dl, g in [((2, 3, 9, 9), (4, 3, 3, 3), (1, 1), (0, 0), (1, 1), 1), ((2, 4, 9, 8), (6, 2, 3, 2), (2, 1), (1, 2), (1, 2), 2)]:
        seed += 1
        X = storage(dt, xs, seed)
        W = storage(dt, ws, seed + 1)
        for Bi in (None, storage(dt, (ws[0],), seed + 2)):
            both("half conv2d %s %s" % (dt, xs), lambda: _k.conv2d(X, xs, W, ws, Bi, st, pd, dl, g))
        ho = (xs[2] + 2 * pd[0] - dl[0] * (ws[2] - 1) - 1) // st[0] + 1
        wo = (xs[3] + 2 * pd[1] - dl[1] * (ws[3] - 1) - 1) // st[1] + 1
        G = storage(dt, (xs[0], ws[0], ho, wo), seed + 5)
        both("half conv2d_backward %s %s" % (dt, xs), lambda: _k.conv2d_backward(X, xs, W, ws, G, st, pd, dl, g))
    seed += 1
    X = storage(dt, (2, 3, 20), seed)
    W = storage(dt, (4, 3, 5), seed + 1)
    both("half conv1d %s" % dt, lambda: _k.conv1d(X, (2, 3, 20), W, (4, 3, 5), storage(dt, (4,), seed + 2)))
    G = storage(dt, (2, 4, 16), seed + 3)
    both("half conv1d_backward %s" % dt, lambda: _k.conv1d_backward(X, (2, 3, 20), W, (4, 3, 5), G))
    seed += 1
    A = storage(dt, (6, 5, 7), seed)
    for perm in ((1, 0, 2), (2, 0, 1)):
        both("half permute %s %s" % (dt, perm), lambda: _k.permute(A, (6, 5, 7), perm))
    M = storage(dt, (20, 13), seed)
    both("half transpose %s" % dt, lambda: _k.permute(M, (20, 13), (1, 0)))
    V = storage(dt, (1, 13), seed)
    both("half expand %s" % dt, lambda: _k.expand(V, (1, 13), (4, 9, 13)))
    both("half slice %s" % dt, lambda: _k.slice(A, (6, 5, 7), [(1, None, 2), None, (0, 6, 1)]))
    both("half strided %s" % dt, lambda: _k.strided(A, (5, 6), (1, 7), 3))
    C = storage("bool", (6, 1, 7), seed + 1)
    Bv = storage(dt, (5, 1), seed + 2)
    Fv = storage("float32", (5, 1), seed + 3)
    both("half where %s" % dt, lambda: _k.where(C, (6, 1, 7), A, (6, 5, 7), Bv, (5, 1)))
    both("half where mixed %s" % dt, lambda: _k.where(C, (6, 1, 7), A, (6, 5, 7), Fv, (5, 1)))
    both("half all_finite %s" % dt, lambda: _k.all_finite(A))
    both("half all_finite finite %s" % dt, lambda: _k.all_finite(_k.from_flat(dt, [i / 8.0 for i in range(100)])))
# arange's native loop (N_ARANGE) against its inline JavaScript loop.
for dt in ["float32", "float64", "int64", "int32"]:
    for st, step, n in ((0, 1, 100), (3, -2, 70), (0.5, 0.1, 90), (-1e8, 3.7, 128), (1e7, 1e-3, 65), (0, 1, 63)):
        if dt in ("int64", "int32") and (st != int(st) or step != int(step)):
            continue
        both("arange %s %s %s %d" % (dt, st, step, n), lambda: _k.arange(dt, st, step, n))
# Operands of 16 to 63 elements, which the engine's own native loops take
# (NATIVE_SMALL) where the JavaScript loops ran before.
for op in ["add", "sub", "mul", "div", "pow", "max", "lt", "floordiv"]:
    for dt in ["float32", "float64", "int64"]:
        for sa, sb in [((16,), (16,)), ((4, 12), (4, 12)), ((4, 12), (12,)), ((12,), (4, 12)), ((63,), ()), ((), (17,)), ((3, 1, 6), (1, 5, 6)), ((5, 7), (5, 1))]:
            seed += 1
            A = storage(dt, sa, seed)
            B = storage(dt, sb, seed + 1)
            both("small binary %s %s %s %s" % (op, dt, sa, sb), lambda: _k.binary(op, A, sa, B, sb))
        for flip in (False, True):
            seed += 1
            A = storage(dt, (4, 12), seed)
            v = 3 if dt == "int64" else 2.5
            both("small binary_scalar %s %s %s" % (op, dt, flip), lambda: _k.binary_scalar(op, A, (4, 12), v, dt, dt, flip))
for op in ["neg", "relu", "exp", "log", "tanh", "sigmoid", "sqrt", "square", "abs", "gelu", "gelu_grad", "silu", "reciprocal"]:
    for dt in ["float32", "float64"]:
        for n in (16, 48, 63):
            seed += 1
            A = storage(dt, (n,), seed)
            both("small unary %s %s %d" % (op, dt, n), lambda: _k.unary(op, A))
for op in ["sum", "mean", "prod", "max", "argmax", "all"]:
    for dt in ["float32", "float64", "int64"]:
        for shape, dims in [((48,), None), ((4, 12), (0,)), ((4, 12), (1,)), ((2, 3, 8), (0, 1)), ((2, 3, 8), (1,))]:
            for precise in (False, True):
                seed += 1
                A = storage(dt, shape, seed)
                both("small reduce %s %s %s %s %s" % (op, dt, shape, dims, precise), lambda: _k.reduce(op, A, shape, dims, True, precise))
for dt in ["float32", "float64", "int64", "bool"]:
    seed += 1
    A = storage(dt, (3, 4, 5), seed)
    C = storage("bool", (3, 1, 5), seed + 1)
    Bv = storage(dt, (4, 1), seed + 2)
    I = _k.from_flat("int64", [2, 0, 1, 1, 0, 2])
    both("small where %s" % dt, lambda: _k.where(C, (3, 1, 5), A, (3, 4, 5), Bv, (4, 1)))
    both("small slice %s" % dt, lambda: _k.slice(A, (3, 4, 5), [None, (1, None, 1), (0, 5, 2)]))
    both("small permute %s" % dt, lambda: _k.permute(A, (3, 4, 5), (2, 0, 1)))
    both("small expand %s" % dt, lambda: _k.expand(Bv, (4, 1), (5, 4, 3)))
    both("small index_select %s" % dt, lambda: _k.index_select(A, (3, 4, 5), 0, I))
# The small add/sub/mul/div loop (fewer than 64 elements, no helper calls)
# against the same values in operands long enough for the native loop:
# element i of the long result is element i % n of the short one.
def tiled(dt, shape, seed, k):
    return _k.from_flat(dt, values(numel(shape), seed, dt) * k)
for op in ["add", "sub", "mul", "div"]:
    for dt in FLOATS:
        for sa, sb in [((5,), (5,)), ((3, 4), (3, 4)), ((3, 4), ()), ((3, 4), (1, 1)), ((), (2, 3)), ((1,), (2, 3)), ((7,), (1,)), ((), ())]:
            seed += 1
            A = storage(dt, sa, seed)
            B = storage(dt, sb, seed + 1)
            short = _k.binary(op, A, sa, B, sb)
            n = numel(short[1])
            k = 70
            if sa == sb:
                la, lsa, lb, lsb = tiled(dt, sa, seed, k), (numel(sa) * k,), tiled(dt, sb, seed + 1, k), (numel(sb) * k,)
            elif numel(sb) == 1 and len(sb) <= len(sa):
                la, lsa, lb, lsb = tiled(dt, sa, seed, k), (numel(sa) * k,), B, sb
            else:
                la, lsa, lb, lsb = A, sa, tiled(dt, sb, seed + 1, k), (numel(sb) * k,)
            long = _k.binary(op, la, lsa, lb, lsb)
            CASES[0] += 1
            if _k.dtype(short[0]) != _k.dtype(long[0]) or _k.tobytes(short[0]) != _k.tobytes(long[0])[:len(_k.tobytes(short[0]))]:
                BAD.append("small binary %s %s %s %s" % (op, dt, sa, sb))
            for v in (2.5, -3, 0.1, 1e30):
                for flip in (False, True):
                    short = _k.binary_scalar(op, A, sa, v, dt, dt, flip)
                    long = _k.binary_scalar(op, tiled(dt, sa, seed, k), (numel(sa) * k,), v, dt, dt, flip)
                    CASES[0] += 1
                    if _k.tobytes(short[0]) != _k.tobytes(long[0])[:len(_k.tobytes(short[0]))]:
                        BAD.append("small binary_scalar %s %s %s %s %s" % (op, dt, sa, v, flip))
# torch.fft's transforms (`vm::py_tensor::fft`): mode 0 complex -> complex,
# 1 real -> onesided, 2 onesided -> real, 3 real -> all bins; lengths of
# 2, 3 and 5, odd primes to 31 and Bluestein, inputs shorter and longer
# than the transform, both directions, three scales.
for dt in FLOATS:
    for n in (1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 13, 16, 30, 31, 37, 60, 64, 77, 97, 100, 128, 243, 1000, 1009):
        for mode in (0, 1, 2, 3):
            for inverse in (False, True):
                base = n // 2 + 1 if mode == 2 else n
                for n_in in (base, max(1, base - 3), base + 2):
                    seed += 1
                    rows = 3
                    width = 1 if mode in (1, 3) else 2
                    A = storage(dt, (rows * n_in * width,), seed)
                    if width == 2:
                        A = _k.as_complex(A)
                    scale = (1.0, 1.0 / n, 1.0 / (n ** 0.5))[seed % 3]
                    both("fft %s n%d mode%d inv%s in%d" % (dt, n, mode, inverse, n_in), lambda: _k.fft(A, rows, n_in, n, mode, inverse, scale))
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

/// float16 storages (Float16Array) take the native loops directly, and a
/// bfloat16 storage (Uint16Array bits) is copied natively: both runs cost
/// far fewer steps than the interpreted loops, with the same bytes.
#[test]
fn half_storages_take_the_native_loops() {
    let program = |native: bool, body: &str| {
        format!(
            "import _zipp_tensor as _k
a = _k.astype(_k.from_flat(\"float32\", [((i * 7) % 13) / 13.0 for i in range(4096)]), \"float16\")
b = _k.astype(_k.from_flat(\"float32\", [((i * 5) % 11) / 11.0 for i in range(4096)]), \"bfloat16\")
_k._native({})
{body}
",
            if native { "True" } else { "False" }
        )
    };
    let matmul = "out, shape = _k.matmul(a, (64, 64), a, (64, 64))\nprint(_k.dtype(out), _k.nbytes(out), _k.tobytes(out)[:64])";
    let permute = "out, shape = _k.permute(b, (64, 64), (1, 0))\nprint(_k.dtype(out), _k.nbytes(out), _k.tobytes(out))";
    for body in [matmul, permute] {
        let native = run(&program(true, body), 2_000_000_000, false);
        let interpreted = run(&program(false, body), 2_000_000_000, false);
        assert_eq!(native.output.clone().unwrap(), interpreted.output.clone().unwrap());
        assert!(native.output.unwrap()[0].contains(" 8192 "), "two bytes per element");
        assert!(native.steps < interpreted.steps, "{body}: {} vs {}", native.steps, interpreted.steps);
    }
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

/// A torch.fft transform or torch.linalg factorization run natively gives
/// the same result as with the native loops off (the FFT's JavaScript loop
/// byte for byte; the Python factorizations to their tolerance), is charged
/// to the budget, and costs far fewer steps than the interpreted work.
#[test]
fn fft_and_linalg_kernels_are_charged_and_agree() {
    let program = |native: bool| {
        format!(
            "import torch
import _zipp_tensor as _k
_k._native({})
x = torch.tensor([((i * 7) % 13) / 13.0 - 0.4 for i in range(4 * 1000)], dtype=torch.float64).reshape(4, 1000)
y = torch.fft.fft(x)
print(_k.tobytes(y._s) == _k.tobytes(torch.fft.fft(x)._s), round(float(torch.view_as_real(y).abs().sum()), 6))
a = x[:, :40].T @ x[:, :40] + torch.eye(40, dtype=torch.float64)
print(round(float(torch.linalg.inv(a).sum()), 9), round(float(torch.linalg.eigvalsh(a).sum()), 9))
",
            if native { "True" } else { "False" }
        )
    };
    let native = run(&program(true), 4_000_000_000, false);
    let interpreted = run(&program(false), 4_000_000_000, false);
    assert_eq!(native.output.clone().unwrap(), interpreted.output.clone().unwrap());
    assert!(native.steps > 4 * 4 * 1000, "native run charged only {} steps", native.steps);
    // The native kernels charge far less than the interpreted loops they
    // replace. (The margin was 4x until Python's own fused instructions made
    // the interpreted fallback itself about 4x cheaper to meter.)
    assert!(native.steps * 2 < interpreted.steps, "{} vs {}", native.steps, interpreted.steps);
}

/// SGD's and Adam's fused update (`optim_step`, one kernel call per
/// parameter) gives the bytes of the per-tensor composite it replaced, for
/// every option, both float dtypes, sizes on both sides of the native
/// threshold and special values, with its native loop on and off: the
/// parameters and every state tensor, step by step.
const OPTIM: &str = r#"
import torch
import _zipp_tensor as _k

INF = float("inf")
NAN = float("nan")
SPECIAL = [0.0, -0.0, 1.0, -1.0, 1e-45, 3e38, 1e-8, INF, -INF, NAN, 0.5, -2.5]


def vals(n, seed, special):
    out = []
    for i in range(n):
        j = (i * 7 + seed * 13) % 31
        if special and j < len(SPECIAL) and (i + seed) % 3 == 0:
            out.append(SPECIAL[j])
        else:
            out.append(((i * 37 + seed * 11) % 23 - 11) / 8.0 + (seed % 5) * 0.137)
    return out


def image(opt, params):
    out = []
    for t in params:
        out.append(_k.tobytes(t._s))
        st = opt.state[t]
        for k in sorted(st):
            v = st[k]
            out.append((k, (_k.dtype(v._s), tuple(v.shape), _k.tobytes(v._s)) if isinstance(v, torch.Tensor) else repr(v)))
    return out


def run(make, dt, shape, seed, special, fused, native):
    _k._native(native)
    saved = _k.optim_step
    if not fused:
        # (declined: every parameter takes the composite)
        _k.optim_step = lambda *a: None
    try:
        n = 1
        for d in shape:
            n *= d
        p = torch.nn.Parameter(torch.tensor(vals(n, seed, special), dtype=dt).reshape(shape))
        q = torch.nn.Parameter(torch.tensor(vals(7, seed + 1, False), dtype=dt))
        opt = make([p, q])
        out = []
        for step in range(3):
            p.grad = torch.tensor(vals(n, seed + 10 + step, special), dtype=dt).reshape(shape)
            q.grad = torch.tensor(vals(7, seed + 20 + step, False), dtype=dt)
            opt.step()
            out.append(image(opt, [p, q]))
        return out
    finally:
        _k.optim_step = saved


O = torch.optim
CONFIGS = [
    ("sgd", lambda ps: O.SGD(ps, lr=0.05)),
    ("sgd momentum", lambda ps: O.SGD(ps, lr=0.05, momentum=0.9)),
    ("sgd dampening wd", lambda ps: O.SGD(ps, lr=0.05, momentum=0.9, dampening=0.1, weight_decay=0.01)),
    ("sgd nesterov", lambda ps: O.SGD(ps, lr=0.03, momentum=0.9, nesterov=True, weight_decay=0.001)),
    ("sgd maximize ints", lambda ps: O.SGD(ps, lr=1, momentum=1, dampening=0, weight_decay=2, maximize=True)),
    ("adam", lambda ps: O.Adam(ps, lr=0.01)),
    ("adam amsgrad", lambda ps: O.Adam(ps, lr=0.01, amsgrad=True)),
    ("adam wd maximize", lambda ps: O.Adam(ps, lr=0.02, weight_decay=0.01, maximize=True, betas=(0.5, 0.9), eps=1e-3)),
    ("adam decoupled", lambda ps: O.Adam(ps, lr=0.01, weight_decay=0.1, decoupled_weight_decay=True)),
    ("adamw", lambda ps: O.AdamW(ps, lr=0.01)),
    ("adamw amsgrad maximize", lambda ps: O.AdamW(ps, lr=0.003, amsgrad=True, maximize=True, weight_decay=0.05)),
]
cases = 0
bad = []
seed = 0
for name, make in CONFIGS:
    for dt in (torch.float32, torch.float64):
        for shape in ((5,), (63,), (64,), (10, 7), (100,)):
            for special in (False, True):
                seed += 1
                ref = run(make, dt, shape, seed, special, False, True)
                for native in (False, True):
                    cases += 1
                    if run(make, dt, shape, seed, special, True, native) != ref:
                        bad.append("%s %s %s special=%s native=%s" % (name, dt, shape, special, native))
print("cases", cases, "mismatches", len(bad), bad[:5])
"#;

#[test]
fn fused_optimizer_steps_give_the_composites_bytes() {
    let run = run(OPTIM, 20_000_000_000, false);
    let out = run.output.expect("runs");
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(out[0].contains("mismatches 0 []"), "{}", out[0]);
    let cases: usize = out[0].split_whitespace().nth(1).unwrap().parse().unwrap();
    assert_eq!(cases, 11 * 2 * 5 * 2 * 2, "{}", out[0]);
}

/// The fused LSTM and GRU cells (`lstm_cell`, `gru_cell` and their
/// backward kernels) give the bytes of the composite they replace, with
/// their native loops on and off: outputs, final states and every
/// gradient of nn.LSTM / nn.GRU (layers, bidirectional, batch_first,
/// proj_size, given initial states, losses reading h_n / c_n, inf / nan /
/// -0 / saturating inputs) and of LSTMCell / GRUCell, float32 and float64.
/// Under create_graph the first derivatives are the composite's bytes and
/// the second derivatives agree to rounding (their sums run in another
/// order).
const CELLS: &str = r#"
import struct

import torch
import _zipp_tensor as _k
from torch import nn

SPECIAL = [0.0, -0.0, 30.0, -30.0, 1e-45, 1e-8, 0.5, -2.5, 89.0, -89.0, float("inf"), float("nan")]


def vals(n, seed, special):
    out = []
    for i in range(n):
        j = (i * 7 + seed * 13) % 37
        if special and j < len(SPECIAL) and (i + seed) % 3 == 0:
            out.append(SPECIAL[j])
        else:
            out.append(((i * 37 + seed * 11) % 23 - 11) / 16.0 + (seed % 5) * 0.037)
    return out


def tensor(shape, seed, dt, special=False, grad=False):
    n = 1
    for d in shape:
        n *= d
    t = torch.tensor(vals(n, seed, special), dtype=dt).reshape(shape)
    return t.requires_grad_(True) if grad else t


def fill(module, dt, seed):
    with torch.no_grad():
        for k, p in enumerate(module.parameters()):
            p.copy_(tensor(tuple(p.shape), seed + k, dt) * 0.5)


def image(ts):
    return [None if t is None else (str(t.dtype), tuple(t.shape), _k.tobytes(t.detach()._s)) for t in ts]


SAVED = {}


def mode(fused, native):
    # fused False: the kernels decline, so every step takes the composite
    for name in ("lstm_cell", "gru_cell"):
        if name not in SAVED:
            SAVED[name] = getattr(_k, name)
        setattr(_k, name, SAVED[name] if fused else (lambda *a: None))
    _k._native(native)


def run_rnn(kind, dt, cfg, seed, special):
    I, H, L, bf, bi, proj, B, T, h0g, use_hn = cfg
    kw = dict(num_layers=L, batch_first=bf, bidirectional=bi)
    if proj:
        kw["proj_size"] = proj
    m = getattr(nn, kind)(I, H, **kw).to(dt)
    fill(m, dt, seed)
    x = tensor((B, T, I) if bf else (T, B, I), seed + 50, dt, special, True)
    D = 2 if bi else 1
    args, extra = [x], []
    if h0g:
        h0 = tensor((L * D, B, proj or H), seed + 60, dt, False, True)
        extra.append(h0)
        if kind == "LSTM":
            c0 = tensor((L * D, B, H), seed + 61, dt, False, True)
            extra.append(c0)
            args.append((h0, c0))
        else:
            args.append(h0)
    out, st = m(*args)
    st = list(st) if kind == "LSTM" else [st]
    loss = (out * out).sum() + out.sum()
    if use_hn:
        loss = loss + (st[0] * 3.0).sum() + (st[-1] * st[-1]).sum()
    loss.backward()
    return image([out, loss, x.grad] + st + [p.grad for p in m.parameters()] + [t.grad for t in extra])


def run_cell(kind, dt, H, B, seed, special, double):
    m = getattr(nn, kind)(5, H).to(dt)
    fill(m, dt, seed)
    x = tensor((B, 5), seed + 40, dt, special, True)
    h = tensor((B, H), seed + 41, dt, False, True)
    ps = list(m.parameters())
    if kind == "LSTMCell":
        c = tensor((B, H), seed + 42, dt, special, True)
        h2, c2 = m(x, (h, c))
        h3, c3 = m(x, (h2, c2))
        outs, loss, leaves = [h2, c2, h3, c3], (h3 * c3).sum() + h2.sum(), ps + [x, h, c]
    else:
        h2 = m(x, h)
        h3 = m(x * 0.5, h2)
        outs, loss, leaves = [h2, h3], (h3 * h3).sum() + h2.sum(), ps + [x, h]
    if not double:
        loss.backward()
        return image(outs + [loss] + [t.grad for t in leaves]), None
    grads = torch.autograd.grad(loss, leaves, create_graph=True)
    second = torch.autograd.grad(sum((g * g).sum() for g in grads), [x] + ps)
    return image(outs + [loss] + list(grads)), [t.detach().reshape(-1).tolist() for t in second]


def close(a, b, rel):
    # each tensor's elements within rel of its largest magnitude
    for x, y in zip(a, b):
        scale = max(abs(p) for p in x + y)
        for p, q in zip(x, y):
            if p != q and abs(p - q) > rel * scale:
                return False
    return True


cases = 0
bad = []
seed = 0
CFGS = [
    # I, H, layers, batch_first, bidirectional, proj_size, B, T, h0/c0 given (requiring grad), loss reads h_n/c_n
    (3, 4, 1, True, False, 0, 2, 3, False, False),
    (6, 12, 1, True, False, 0, 4, 10, False, False),
    (5, 7, 2, False, True, 0, 3, 4, True, True),
    (4, 20, 1, False, False, 0, 5, 3, True, True),
    (3, 6, 2, True, False, 3, 2, 3, True, True),
]
for kind in ("LSTM", "GRU"):
    for dt in (torch.float32, torch.float64):
        for cfg in CFGS:
            if kind == "GRU" and cfg[5]:
                continue
            for special in (False, True):
                seed += 1
                mode(False, True)
                ref = run_rnn(kind, dt, cfg, seed, special)
                for native in (False, True):
                    cases += 1
                    mode(True, native)
                    if run_rnn(kind, dt, cfg, seed, special) != ref:
                        bad.append("%s %s %s special=%s native=%s" % (kind, dt, cfg, special, native))
for kind in ("LSTMCell", "GRUCell"):
    for dt in (torch.float32, torch.float64):
        for H, B in ((4, 3), (16, 8)):
            for special in (False, True):
                for double in ((False, True) if not special else (False,)):
                    seed += 1
                    mode(False, True)
                    ref, ref2 = run_cell(kind, dt, H, B, seed, special, double)
                    for native in (False, True):
                        cases += 1
                        mode(True, native)
                        got, got2 = run_cell(kind, dt, H, B, seed, special, double)
                        # second derivatives: the same up to the order of
                        # their sums (create_graph records the composite's
                        # backward over fresh activations)
                        if got != ref or (double and not close(got2, ref2, 1e-5 if dt is torch.float32 else 1e-13)):
                            bad.append("%s %s %d %d special=%s double=%s native=%s" % (kind, dt, H, B, special, double, native))
mode(True, True)
print("cases", cases, "mismatches", len(bad), bad[:6])
"#;

#[test]
fn fused_recurrent_cells_give_the_composites_bytes() {
    let run = run(CELLS, 20_000_000_000, false);
    let out = run.output.expect("runs");
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(out[0].contains("mismatches 0 []"), "{}", out[0]);
    let cases: usize = out[0].split_whitespace().nth(1).unwrap().parse().unwrap();
    assert_eq!(cases, 120, "{}", out[0]);
}

/// The native MT19937 draws (`pt_rng`) against tensor.js's loops: the same
/// generator from the same seed, native loops off then on, must draw the
/// same bytes for rand / randn / normal_ / uniform_ in float32 and float64
/// at every size (the blocks of 16, a size no multiple of 16, the twist
/// every 624 words), leave the same generator state, and continue a stream
/// restored with set_rng_state or drawn through another Generator.
const DRAWS: &str = r#"
import torch
import _zipp_tensor as _k

SIZES = list(range(1, 41)) + [47, 63, 64, 100, 255, 623, 624, 625, 1000, 1249]


def draws(native):
    _k._native(native)
    out = []
    for seed in (0, 12345):
        torch.manual_seed(seed)
        for n in SIZES:
            out.append(torch.rand(n))
            out.append(torch.randn(n))
            out.append(torch.rand(n, dtype=torch.float64))
            out.append(torch.randn(n, dtype=torch.float64))
            t = torch.empty(n)
            t.normal_(0.5, 2.0)
            out.append(t)
            t = torch.empty(n, dtype=torch.float64)
            t.normal_(-1.25, 0.3)
            out.append(t)
            t.uniform_(-0.5, 0.25)
            out.append(t)
            out.append(torch.randn(n, dtype=torch.float16))
        out.append(torch.get_rng_state())
        st = torch.get_rng_state()
        a = torch.randn(40)
        torch.set_rng_state(st)
        out.append(a)
        out.append(torch.randn(40))
        g = torch.Generator().manual_seed(seed + 1)
        out.append(torch.rand(700, generator=g))
        out.append(torch.randn(33, generator=g))
        out.append(g.get_state())
    return [_k.tobytes(t._s) for t in out]


ref = draws(False)
got = draws(True)
bad = [i for i in range(len(ref)) if ref[i] != got[i]]
print("draws", len(ref), "mismatches", len(bad), bad[:6])
"#;

#[test]
fn native_random_draws_give_the_javascript_loops_bytes() {
    let run = run(DRAWS, 20_000_000_000, false);
    let out = run.output.expect("runs");
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(out[0].contains("mismatches 0 []"), "{}", out[0]);
    let draws: usize = out[0].split_whitespace().nth(1).unwrap().parse().unwrap();
    assert_eq!(draws, 2 * (50 * 8 + 6), "{}", out[0]);
}

/// A native draw is charged, below what the interpreted loop spends.
#[test]
fn native_random_draws_are_charged() {
    let program = |native: bool| {
        format!(
            "import torch\nimport _zipp_tensor as _k\n_k._native({})\ntorch.manual_seed(1)\nprint(float(torch.randn(4096).sum()), float(torch.rand(4096).sum()))\n",
            if native { "True" } else { "False" }
        )
    };
    let native = run(&program(true), 2_000_000_000, false);
    let interpreted = run(&program(false), 2_000_000_000, false);
    assert_eq!(native.output.clone().unwrap(), interpreted.output.clone().unwrap());
    assert!(native.steps + 100_000 < interpreted.steps, "{} vs {}", native.steps, interpreted.steps);
}
