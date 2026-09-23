/* ZIPP Python runtime — `_zipp_tensor`: the numeric kernels behind the
 * bundled `torch` package. Apache-2.0.
 *
 * A storage is a JavaScript typed array with a dtype tag; every kernel is a
 * plain loop over contiguous row-major data (the Python side keeps shapes
 * and strides trivial: tensors are always contiguous, views copy). Nothing
 * here touches a GPU: this is the CPU path of the engine, on wasm or
 * native. Kernels take shapes as Python tuples of ints and return
 * `(storage, shape)` pairs where the shape changes.
 *
 * Python runs on the interpreter (the VM JIT is off for Python states), so
 * the hot kernels are written as one inline loop per operation: a closure
 * call per element costs a frame, which was most of an elementwise kernel.
 *
 * A storage carries a version, bumped by every kernel that writes into an
 * existing storage (`fill`, `setitem`, `copy_into`, `setslice`, `scatter`,
 * `scatter_add`, `axpy`), so a compiled GPU result can tell whether the
 * tensors it was recorded from changed without keeping a copy to compare.
 */
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T, E = rt.E, fail = rt.fail;
    const tuple = rt.tuple, list = rt.list;
    const Storage = rt.newType("_Storage", [rt.ObjectType], new Map(), "_zipp_tensor");
    // The engine's native loops (`vm::py_tensor`), bound only in Python
    // states. `NATIVE(op, ...)` runs one kernel's loop in Rust over the same
    // typed arrays and returns true, or declines with false (an odd view, a
    // budget that cannot cover it, a trace being recorded) and the
    // JavaScript loop after it runs instead. The values are the same either
    // way; `_native(False)` turns the native loops off to compare.
    let NATIVE_FN = null;
    try { NATIVE_FN = typeof __zipp_py_native === "function" ? __zipp_py_native : null; } catch (e) { NATIVE_FN = null; }
    let NATIVE = NATIVE_FN;
    // Below this many elements an elementwise kernel's own inline loop is
    // as cheap as the native call.
    const NATIVE_MIN = 64;
    const N_MATMUL = 1, N_CONV2D = 2, N_CONV2D_BACKWARD = 3, N_CONV1D = 4, N_CONV1D_BACKWARD = 5, N_BINARY = 6, N_UNARY = 7,
        N_REDUCE = 8, N_SOFTMAX = 9, N_GATHER = 10, N_ALL_FINITE = 11, N_WHERE = 12;
    const BIN_CODE = { add: 1, sub: 2, mul: 3, div: 4, pow: 5, max: 6, min: 7, eq: 8, ne: 9, lt: 10, le: 11, gt: 12, ge: 13,
        and: 14, or: 15, xor: 16, floordiv: 17, mod: 18, atan2: 19 };
    const UN_CODE = { neg: 1, relu: 2, exp: 3, log: 4, tanh: 5, sigmoid: 6, sqrt: 7, square: 8, abs: 9, sign: 10, silu: 11,
        gelu: 12, gelu_grad: 13, reciprocal: 14, rsqrt: 15, log1p: 16, expm1: 17, softplus: 18, sin: 19, cos: 20, floor: 21,
        ceil: 22, trunc: 23, isfinite: 24, isnan: 25, not: 26, clamp: 27, frac: 28, tan: 29, atan: 30, log2: 31, log10: 32,
        isinf: 33, exp2: 34, sinh: 35, cosh: 36, asin: 37, acos: 38, asinh: 39, acosh: 40, atanh: 41 };
    const RED_CODE = { sum: 1, mean: 2, prod: 3, max: 4, min: 5, argmax: 6, argmin: 7, all: 8, any: 9 };
    const ARRAY = { float32: Float32Array, float64: Float64Array, int64: Float64Array, int32: Float64Array, bool: Uint8Array, uint8: Uint8Array };
    const RANK = { bool: 0, uint8: 1, int32: 2, int64: 3, float32: 4, float64: 5 };
    function make(dtype, data) { if (ARRAY[dtype] === undefined) fail(E.TypeError, "unknown dtype " + dtype); return { cls: Storage, dtype: dtype, data: data, version: 0, untracked: 0 }; }
    function alloc(dtype, n) { return make(dtype, new ARRAY[dtype](n)); }
    function isStorage(v) { return v !== null && typeof v === "object" && v.cls === Storage; }
    function needS(v, what) { if (!isStorage(v)) fail(E.TypeError, (what || "argument") + " must be a tensor storage"); return v; }
    function written(s) { s.version++; }
    // The host transport (entry.js): a float32 storage leaves as its
    // Float32Array, and a Float32Array the host sends arrives as a storage
    // that owns it (the VM made it from the host's copy).
    rt.float32Storage = (data) => make("float32", data);
    rt.isFloat32Storage = (v) => isStorage(v) && v.dtype === "float32";
    function isFloatDtype(dtype) { return dtype === "float32" || dtype === "float64"; }
    // Indexed loops, not for...of: these run on every kernel call, and the
    // iterator protocol is several interpreter calls per element.
    function shapeOf(v) { if (v === null || typeof v !== "object" || v.items === undefined) fail(E.TypeError, "shape must be a tuple"); const items = v.items, out = new Array(items.length); for (let i = 0; i < items.length; i++) out[i] = Number(rt.asInt(items[i])); return out; }
    function ints(v) { return shapeOf(v); }
    // Result shapes are torch.Size values once torch registers the type
    // (`_set_size_type`): a Tensor keeps a Size as it is, where a tuple
    // would be copied into a new Size. A Size is a tuple subclass instance,
    // built as `rt.allocInstance` builds one.
    let SIZE = null;
    function pyShape(shape) {
        const out = new Array(shape.length); for (let i = 0; i < shape.length; i++) out[i] = BigInt(shape[i]);
        return SIZE === null ? tuple(out) : { cls: SIZE, items: out, dict: new Map() };
    }
    function numel(shape) { let n = 1; for (let i = 0; i < shape.length; i++) n *= shape[i]; return n; }
    function strides(shape) { const s = new Array(shape.length); let acc = 1; for (let i = shape.length - 1; i >= 0; i--) { s[i] = acc; acc *= shape[i]; } return s; }
    function promote(a, b) { return RANK[a] >= RANK[b] ? a : b; }
    function pyNumber(dtype, v) {
        if (dtype === "bool") return v !== 0;
        if (dtype === "int64" || dtype === "int32" || dtype === "uint8") return BigInt(Math.trunc(v));
        return v;
    }
    function jsNumber(v) {
        if (typeof v === "bigint") return Number(v);
        if (typeof v === "boolean") return v ? 1 : 0;
        if (typeof v === "number") return v;
        fail(E.TypeError, "a number is required, not " + rt.typeOf(v).name);
    }
    function castValue(dtype, v) {
        if (dtype === "float32") return Math.fround(v);
        if (dtype === "bool") return v !== 0 ? 1 : 0;
        if (dtype === "int64" || dtype === "int32") return Math.trunc(v);
        if (dtype === "uint8") return Math.trunc(v) & 255;
        return v;
    }
    // Broadcast `shape` against `target`: the stride per target dim (0 where broadcast).
    function bstrides(shape, target) {
        const s = strides(shape), out = new Array(target.length).fill(0), off = target.length - shape.length;
        if (off < 0) fail(E.RuntimeError, "cannot broadcast shape [" + shape + "] to [" + target + "]");
        for (let i = 0; i < shape.length; i++) {
            const t = target[off + i];
            if (shape[i] === t) out[off + i] = s[i];
            else if (shape[i] === 1) out[off + i] = 0;
            else fail(E.RuntimeError, "The size of tensor a (" + shape[i] + ") must match the size of tensor b (" + t + ") at non-singleton dimension " + i);
        }
        return out;
    }
    function broadcastShape(a, b) {
        const n = Math.max(a.length, b.length), out = new Array(n);
        for (let i = 0; i < n; i++) {
            const x = i < n - a.length ? 1 : a[i - (n - a.length)], y = i < n - b.length ? 1 : b[i - (n - b.length)];
            if (x === y || y === 1) out[i] = x; else if (x === 1) out[i] = y;
            else fail(E.RuntimeError, "The size of tensor a (" + x + ") must match the size of tensor b (" + y + ") at non-singleton dimension " + i);
        }
        return out;
    }
    // Iterate every index of `shape`, calling fn(flatOut, offA, offB) with
    // offsets computed from stride tables (0 strides broadcast).
    function forEachBroadcast(shape, sa, sb, fn) {
        const n = numel(shape), rank = shape.length;
        if (rank === 0) { fn(0, 0, 0); return; }
        const idx = new Array(rank).fill(0);
        let oa = 0, ob = 0;
        for (let flat = 0; flat < n; flat++) {
            fn(flat, oa, ob);
            for (let d = rank - 1; d >= 0; d--) {
                idx[d]++; oa += sa[d]; ob += sb[d];
                if (idx[d] < shape[d]) break;
                oa -= sa[d] * shape[d]; ob -= sb[d] * shape[d]; idx[d] = 0;
            }
        }
    }
    const BIN = {
        add: (x, y) => x + y, sub: (x, y) => x - y, mul: (x, y) => x * y, div: (x, y) => x / y,
        pow: (x, y) => Math.pow(x, y), max: (x, y) => (x !== x || y !== y) ? NaN : Math.max(x, y), min: (x, y) => (x !== x || y !== y) ? NaN : Math.min(x, y),
        eq: (x, y) => (x === y ? 1 : 0), ne: (x, y) => (x !== y ? 1 : 0), lt: (x, y) => (x < y ? 1 : 0), le: (x, y) => (x <= y ? 1 : 0), gt: (x, y) => (x > y ? 1 : 0), ge: (x, y) => (x >= y ? 1 : 0),
        and: (x, y) => (x && y ? 1 : 0), or: (x, y) => (x || y ? 1 : 0), xor: (x, y) => ((x ? 1 : 0) ^ (y ? 1 : 0)),
        floordiv: (x, y) => Math.floor(x / y), mod: (x, y) => x - Math.floor(x / y) * y, fmod: (x, y) => x % y,
        bitand: (x, y) => bitwise(x, y, 0), bitor: (x, y) => bitwise(x, y, 1), bitxor: (x, y) => bitwise(x, y, 2),
        lshift: (x, y) => x * Math.pow(2, y), rshift: (x, y) => Math.floor(x / Math.pow(2, y)),
        atan2: Math.atan2, ipow: intPow,
    };
    // Integer bitwise ops on the float64 storage of int64 values: 32-bit JS
    // operators when both fit, BigInt otherwise (exact up to 2**53 either way).
    function bitwise(x, y, kind) {
        if ((x | 0) === x && (y | 0) === y) return kind === 0 ? x & y : kind === 1 ? x | y : x ^ y;
        const a = BigInt(x), b = BigInt(y);
        return Number(kind === 0 ? a & b : kind === 1 ? a | b : a ^ b);
    }
    // An integer power stays an integer: a negative exponent gives 1 for a
    // base of 1, +-1 for -1 and 0 otherwise (truncated 1/x**n), as PyTorch's powi.
    function intPow(x, y) {
        if (y < 0) return x === 1 ? 1 : x === -1 ? (y % 2 === 0 ? 1 : -1) : 0;
        return Math.pow(x, y);
    }
    const COMPARE = new Set(["eq", "ne", "lt", "le", "gt", "ge", "and", "or", "xor"]);
    // How many trailing elements `part` tiles over `shape` with: its
    // numel when `part` (leading 1s dropped) is exactly a suffix of `shape`,
    // so element i of the result reads element i % tile of `part`; 0 when
    // the broadcast is not of that form.
    function suffixTile(part, shape) {
        let lead = 0; while (lead < part.length && part[lead] === 1) lead++;
        const off = shape.length - (part.length - lead);
        if (off < 0) return 0;
        let n = 1;
        for (let i = lead; i < part.length; i++) { if (part[i] !== shape[off + i - lead]) return 0; n *= part[i]; }
        return n;
    }
    // O[i] = A[i] op B[i % nb], or B[i % nb] op A[i] when `flip`; nb divides
    // O.length. false when `op` has no inline loop.
    function binaryTile(op, O, A, B, nb, flip) {
        const n = O.length;
        switch (op) {
            case "add": for (let i = 0; i < n;) for (let j = 0; j < nb; j++, i++) O[i] = A[i] + B[j]; return true;
            case "mul": for (let i = 0; i < n;) for (let j = 0; j < nb; j++, i++) O[i] = A[i] * B[j]; return true;
            case "sub":
                if (flip) for (let i = 0; i < n;) for (let j = 0; j < nb; j++, i++) O[i] = B[j] - A[i];
                else for (let i = 0; i < n;) for (let j = 0; j < nb; j++, i++) O[i] = A[i] - B[j];
                return true;
            case "div":
                if (flip) for (let i = 0; i < n;) for (let j = 0; j < nb; j++, i++) O[i] = B[j] / A[i];
                else for (let i = 0; i < n;) for (let j = 0; j < nb; j++, i++) O[i] = A[i] / B[j];
                return true;
        }
        return false;
    }
    // O[i] = A[i] op y, or y op A[i] when `flip`.
    function binaryScalar(op, O, A, y, flip) {
        const n = O.length;
        switch (op) {
            case "add": for (let i = 0; i < n; i++) O[i] = A[i] + y; return true;
            case "mul": for (let i = 0; i < n; i++) O[i] = A[i] * y; return true;
            case "sub":
                if (flip) for (let i = 0; i < n; i++) O[i] = y - A[i];
                else for (let i = 0; i < n; i++) O[i] = A[i] - y;
                return true;
            case "div":
                if (flip) for (let i = 0; i < n; i++) O[i] = y / A[i];
                else for (let i = 0; i < n; i++) O[i] = A[i] / y;
                return true;
            case "pow":
                if (flip) for (let i = 0; i < n; i++) O[i] = Math.pow(y, A[i]);
                else for (let i = 0; i < n; i++) O[i] = Math.pow(A[i], y);
                return true;
            case "gt": if (flip) for (let i = 0; i < n; i++) O[i] = y > A[i] ? 1 : 0; else for (let i = 0; i < n; i++) O[i] = A[i] > y ? 1 : 0; return true;
            case "lt": if (flip) for (let i = 0; i < n; i++) O[i] = y < A[i] ? 1 : 0; else for (let i = 0; i < n; i++) O[i] = A[i] < y ? 1 : 0; return true;
            case "ge": if (flip) for (let i = 0; i < n; i++) O[i] = y >= A[i] ? 1 : 0; else for (let i = 0; i < n; i++) O[i] = A[i] >= y ? 1 : 0; return true;
            case "le": if (flip) for (let i = 0; i < n; i++) O[i] = y <= A[i] ? 1 : 0; else for (let i = 0; i < n; i++) O[i] = A[i] <= y ? 1 : 0; return true;
        }
        return false;
    }
    // A shape tuple the caller passed in, when the result has exactly that
    // shape: the kernel hands it back instead of building an equal one.
    function tupleShape(v) { return v !== undefined && (v.cls === T.tuple || rt.isSubclass(v.cls, T.tuple)); }
    // `want`: the result dtype Python's type promotion chose (null: promote
    // the operands' dtypes). The kernel computes in double precision and
    // rounds on store.
    function binary(op, a, ashape, b, bshape, apy, bpy, want) {
        const shape = broadcastShape(ashape, bshape);
        let dtype = COMPARE.has(op) ? "bool" : (want ? want : promote(a.dtype, b.dtype));
        if ((op === "div" || op === "atan2") && RANK[dtype] < RANK.float32) dtype = "float32";
        // An integer result: powers stay integral (the other arithmetic is
        // exact on integers already).
        if (op === "pow" && RANK[dtype] < RANK.float32) op = "ipow";
        const f = BIN[op]; if (f === undefined) fail(E.ValueError, "unknown op " + op);
        if (dtype === "bool" && !COMPARE.has(op)) return binaryBool(f, a, ashape, b, bshape, shape);
        const n = numel(shape), out = alloc(dtype, n), A = a.data, Bd = b.data, O = out.data;
        // Every layout below visits elements in the same order and applies
        // the same double-precision operation as the closure form, and the
        // typed array rounds on store, so results are identical.
        const na = numel(ashape), nb = numel(bshape);
        // Same rank and (nonzero) size as an operand means the same shape.
        const outShape = n !== 0 && na === n && ashape.length === shape.length && tupleShape(apy) ? apy
            : n !== 0 && nb === n && bshape.length === shape.length && tupleShape(bpy) ? bpy : null;
        if (n >= NATIVE_MIN && NATIVE !== null && BIN_CODE[op] !== undefined
            && NATIVE(N_BINARY, BIN_CODE[op], A, Bd, O, shape, bstrides(ashape, shape), bstrides(bshape, shape)))
            return tuple([out, outShape === null ? pyShape(shape) : outShape]);
        if (na === n && ashape.length === shape.length) {
            if (nb === 1 ? binaryScalar(op, O, A, Bd[0], false) : (nb === n && bshape.length === shape.length ? binaryTile(op, O, A, Bd, n, false) : (suffixTile(bshape, shape) === nb && binaryTile(op, O, A, Bd, nb, false))))
                return tuple([out, outShape === null ? pyShape(shape) : outShape]);
        } else if (nb === n && bshape.length === shape.length) {
            if (na === 1 ? binaryScalar(op, O, Bd, A[0], true) : (suffixTile(ashape, shape) === na && binaryTile(op, O, Bd, A, na, true)))
                return tuple([out, outShape === null ? pyShape(shape) : outShape]);
        }
        if (ashape.length === shape.length && bshape.length === shape.length && na === n && nb === n) {
            for (let i = 0; i < n; i++) O[i] = f(A[i], Bd[i]);
        } else if (nb === 1 && ashape.length === shape.length && na === n) {
            const y = Bd[0]; for (let i = 0; i < n; i++) O[i] = f(A[i], y);
        } else {
            forEachBroadcast(shape, bstrides(ashape, shape), bstrides(bshape, shape), (o, x, y) => { O[o] = f(A[x], Bd[y]); });
        }
        return tuple([out, outShape === null ? pyShape(shape) : outShape]);
    }
    // Arithmetic with a bool result: computed like the others, then any
    // nonzero stores as 1 (True + True is True).
    function binaryBool(f, a, ashape, b, bshape, shape) {
        const out = alloc("bool", numel(shape)), O = out.data, A = a.data, Bd = b.data;
        forEachBroadcast(shape, bstrides(ashape, shape), bstrides(bshape, shape), (o, x, y) => { O[o] = f(A[x], Bd[y]) ? 1 : 0; });
        return tuple([out, pyShape(shape)]);
    }
    const UN = {
        neg: (x) => -x, exp: Math.exp, log: Math.log, tanh: Math.tanh, sigmoid: (x) => 1 / (1 + Math.exp(-x)),
        silu: (x) => x / (1 + Math.exp(-x)), relu: (x) => (x > 0 || x !== x ? x : 0), sqrt: Math.sqrt, square: (x) => x * x,
        abs: Math.abs, sign: (x) => (x > 0 ? 1 : x < 0 ? -1 : 0), floor: Math.floor, ceil: Math.ceil, round: (x) => { const r = Math.round(x); return (Math.abs(x % 1) === 0.5 && r % 2 !== 0) ? r - 1 : r; }, // Math.round breaks ties upward; odd means one too high
        isfinite: (x) => (Number.isFinite(x) ? 1 : 0), isnan: (x) => (x !== x ? 1 : 0), not: (x) => (x ? 0 : 1), reciprocal: (x) => 1 / x, rsqrt: (x) => 1 / Math.sqrt(x), log1p: Math.log1p, expm1: Math.expm1,
        gelu: (x) => x * cdf(x), gelu_grad: geluGrad, softplus: (x) => (x > 20 ? x : Math.log1p(Math.exp(x))), sin: Math.sin, cos: Math.cos,
        tan: Math.tan, asin: Math.asin, acos: Math.acos, atan: Math.atan, sinh: Math.sinh, cosh: Math.cosh, asinh: Math.asinh, acosh: Math.acosh, atanh: Math.atanh,
        log2: Math.log2, log10: Math.log10, exp2: (x) => Math.pow(2, x), erf: erf, erfc: erfc, erfinv: erfinv, trunc: Math.trunc, frac: (x) => x - Math.trunc(x),
        isinf: (x) => (x === Infinity || x === -Infinity ? 1 : 0), isposinf: (x) => (x === Infinity ? 1 : 0), isneginf: (x) => (x === -Infinity ? 1 : 0),
        bitnot: (x) => -x - 1, signbit: (x) => (x < 0 || Object.is(x, -0) ? 1 : 0),
    };
    // erf to double precision: below 3 the all-positive series
    // 2/sqrt(pi) e^(-x^2) sum 2^n x^(2n+1) / (2n+1)!! (no cancellation),
    // above it erfc's continued fraction.
    function erfcFrac(x) {
        // Lentz's method on erfc(x) = e^(-x^2)/sqrt(pi) / (x + 1/2 / (x + 1 / (x + 3/2 / (x + ...)))).
        const tiny = 1e-300;
        let f = x, C = x, D = 0;
        for (let n = 1; n < 300; n++) {
            const an = n / 2;
            D = x + an * D; if (D === 0) D = tiny; D = 1 / D;
            C = x + an / C; if (C === 0) C = tiny;
            const delta = C * D; f *= delta;
            if (Math.abs(delta - 1) < 1e-16) break;
        }
        return Math.exp(-x * x) / Math.sqrt(Math.PI) / f;
    }
    function erf(x) {
        if (x !== x) return NaN;
        const a = Math.abs(x);
        if (a >= 6) return x > 0 ? 1 : -1;
        if (a < 3) {
            const t = x * x;
            let term = x, sum = x;
            for (let n = 1; n < 200; n++) { term *= 2 * t / (2 * n + 1); sum += term; if (Math.abs(term) < 1e-17 * Math.abs(sum)) break; }
            return 2 / Math.sqrt(Math.PI) * Math.exp(-t) * sum;
        }
        const c = erfcFrac(a);
        return x > 0 ? 1 - c : c - 1;
    }
    function erfc(x) {
        if (x !== x) return NaN;
        if (x >= 3) return x > 27 ? 0 : erfcFrac(x);
        return 1 - erf(x);
    }
    // Giles' single-precision erfinv, then Newton steps on the double erf.
    function erfinv(y) {
        if (y !== y || y < -1 || y > 1) return NaN;
        if (y === 1) return Infinity; if (y === -1) return -Infinity;
        let w = -Math.log((1 - y) * (1 + y)), x;
        if (w < 5) {
            w -= 2.5;
            let p = 2.81022636e-08; p = 3.43273939e-07 + p * w; p = -3.5233877e-06 + p * w; p = -4.39150654e-06 + p * w; p = 0.00021858087 + p * w;
            p = -0.00125372503 + p * w; p = -0.00417768164 + p * w; p = 0.246640727 + p * w; p = 1.50140941 + p * w; x = p * y;
        } else {
            w = Math.sqrt(w) - 3;
            let p = -0.000200214257; p = 0.000100950558 + p * w; p = 0.00134934322 + p * w; p = -0.00367342844 + p * w; p = 0.00573950773 + p * w;
            p = -0.0076224613 + p * w; p = 0.00943887047 + p * w; p = 1.00167406 + p * w; p = 2.83297682 + p * w; x = p * y;
        }
        for (let i = 0; i < 3; i++) { const e = erf(x) - y, d = 2 / Math.sqrt(Math.PI) * Math.exp(-x * x); if (d === 0 || e === 0) break; x -= e / d; }
        return x;
    }
    // The standard normal CDF behind GELU (the exact-erf form, F.gelu's
    // default), with the one erf approximation every zipp_gpu backend
    // shares (gpu-lab's kernel-math.mjs, the Python reference's `_cdf`): an
    // odd series for |z| < 0.5, Numerical Recipes' erfc fit above it, the
    // lower tail from erfc directly. Same expressions, same evaluation order.
    function erfSeries(z) {
        const t = z * z;
        return z * (1.1283791670955126 + t * (-0.37612638903183754 + t * (0.11283791670955126 + t * (-0.026866170645131252 +
            t * (0.005223977625442188 + t * (-0.0008548327023450852 + t * 0.00012055332981789664))))));
    }
    function erfcFit(a) {
        const t = 1 / (1 + 0.5 * a);
        return t * Math.exp(-a * a - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 +
            t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))));
    }
    function cdf(x) {
        const z = x * 0.7071067811865476;
        if (Math.abs(z) < 0.5) return 0.5 + 0.5 * erfSeries(z);
        if (z >= 10) return 1;
        if (z <= -10) return 0;
        const c = 0.5 * erfcFit(Math.abs(z));
        return z > 0 ? 1 - c : c;
    }
    // d/dx gelu(x) = cdf(x) + x * pdf(x).
    function geluGrad(x) { return cdf(x) + x * 0.3989422804014327 * Math.exp(-0.5 * x * x); }
    const BOOL_UNARY = new Set(["isfinite", "isnan", "not", "isinf", "isposinf", "isneginf", "signbit"]);
    const FLOAT_UNARY = new Set(["exp", "log", "tanh", "sigmoid", "silu", "sqrt", "reciprocal", "log1p", "expm1", "gelu", "gelu_grad", "softplus", "sin", "cos", "rsqrt",
        "tan", "asin", "acos", "atan", "sinh", "cosh", "asinh", "acosh", "atanh", "log2", "log10", "exp2", "erf", "erfc", "erfinv"]);
    function unary(op, a, p1, p2) {
        let f = UN[op], lo = 0, hi = 0;
        if (op === "clamp") { lo = p1 === null ? -Infinity : jsNumber(p1); hi = p2 === null ? Infinity : jsNumber(p2); f = (x) => (x < lo ? lo : x > hi ? hi : x); }
        if (f === undefined) fail(E.ValueError, "unknown op " + op);
        const dtype = BOOL_UNARY.has(op) ? "bool" : (FLOAT_UNARY.has(op) && RANK[a.dtype] < RANK.float32 ? "float32" : a.dtype);
        const out = alloc(dtype, a.data.length), A = a.data, O = out.data, n = O.length;
        if (n >= NATIVE_MIN && NATIVE !== null && UN_CODE[op] !== undefined && NATIVE(N_UNARY, UN_CODE[op], A, O, lo, hi)) return out;
        // The hot activations and their gradients inline; the expressions
        // are the table's own.
        switch (op) {
            case "neg": for (let i = 0; i < n; i++) O[i] = -A[i]; break;
            case "relu": for (let i = 0; i < n; i++) { const x = A[i]; O[i] = x > 0 || x !== x ? x : 0; } break;
            case "exp": for (let i = 0; i < n; i++) O[i] = Math.exp(A[i]); break;
            case "log": for (let i = 0; i < n; i++) O[i] = Math.log(A[i]); break;
            case "tanh": for (let i = 0; i < n; i++) O[i] = Math.tanh(A[i]); break;
            case "sigmoid": for (let i = 0; i < n; i++) O[i] = 1 / (1 + Math.exp(-A[i])); break;
            case "sqrt": for (let i = 0; i < n; i++) O[i] = Math.sqrt(A[i]); break;
            case "square": for (let i = 0; i < n; i++) { const x = A[i]; O[i] = x * x; } break;
            case "abs": for (let i = 0; i < n; i++) O[i] = Math.abs(A[i]); break;
            case "sign": for (let i = 0; i < n; i++) { const x = A[i]; O[i] = x > 0 ? 1 : x < 0 ? -1 : 0; } break;
            default: for (let i = 0; i < n; i++) O[i] = f(A[i]);
        }
        return out;
    }
    // ---- reductions ------------------------------------------------------------------
    const CONTIGUOUS_REDUCE = new Set(["sum", "mean", "prod", "max", "min", "argmax", "argmin", "all", "any"]);
    // `precise`: accumulate in double precision and round once on store
    // (eager torch reductions). Without it a float32 reduction rounds every
    // partial sum, the index-order float32 accumulation the zipp_gpu graph
    // reference defines for an axis sum.
    function reduce(op, a, shape, dims, keepdim, precise) {
        const rank = shape.length;
        const red = dims === null ? null : ints(dims);
        if (red !== null) for (let i = 0; i < red.length; i++) { const d = red[i] < 0 ? red[i] + rank : red[i]; if (d < 0 || d >= rank) fail(E.IndexError, "Dimension out of range"); red[i] = d; }
        const all = red === null;
        const isRed = new Array(rank).fill(all);
        if (!all) for (let i = 0; i < red.length; i++) isRed[red[i]] = true;
        const outShape = [], keptShape = [];
        for (let d = 0; d < rank; d++) { if (isRed[d]) { keptShape.push(1); } else { outShape.push(shape[d]); keptShape.push(shape[d]); } }
        const finalShape = keepdim ? keptShape : outShape;
        const nOut = numel(keptShape), nIn = numel(shape);
        const argOp = op === "argmax" || op === "argmin";
        const dtype = argOp ? "int64" : (op === "all" || op === "any") ? "bool" : (op === "mean" && RANK[a.dtype] < RANK.float32) ? "float32" : a.dtype;
        const out = alloc(dtype, nOut), A = a.data;
        // Reductions accumulate in double precision (O) and round once when
        // stored into the result's dtype: a precise float32 sum of a million
        // elements keeps float32 accuracy, and a uint8/bool max/min can
        // start from +-Infinity.
        const O = dtype === "float64" || dtype === "int64" || (dtype === "float32" && !precise) ? out.data : new Float64Array(nOut);
        const init = op === "sum" || op === "mean" ? 0 : op === "prod" ? 1 : op === "max" || op === "argmax" ? -Infinity : op === "min" || op === "argmin" ? Infinity : op === "all" ? 1 : 0;
        const best = argOp ? new Float64Array(nOut).fill(init) : null;
        O.fill(argOp ? 0 : init);
        const count = nIn / (nOut || 1);
        // Reduced dims that form one contiguous block (every dim, a leading
        // batch dim, a trailing feature dim): outer x block x inner loops,
        // one per op. Each output element takes its inputs in the same
        // increasing flat order, through the same store, as the walk below.
        let first = -1, last = -1, contiguous = true;
        for (let d = 0; d < rank; d++) if (isRed[d]) { if (first < 0) first = d; else if (last !== d - 1) contiguous = false; last = d; }
        // An arg reduction's index is the input's position within the
        // reduced block, which is `r`. NaN is the maximum and the minimum:
        // the first NaN wins both, as in PyTorch.
        if (contiguous && CONTIGUOUS_REDUCE.has(op)) {
            const outer = first < 0 ? nIn : numel(shape.slice(0, first)), block = first < 0 ? 1 : numel(shape.slice(first, last + 1)), inner = first < 0 ? 1 : numel(shape.slice(last + 1));
            // The native loop accumulates as O does: rounding every update
            // when O is the float32 result itself, once on store otherwise.
            if (nIn >= NATIVE_MIN && NATIVE !== null && NATIVE(N_REDUCE, RED_CODE[op], A, out.data, outer, block, inner, count, O === out.data))
                return tuple([out, pyShape(finalShape)]);
            let i = 0;
            switch (op) {
                case "sum": case "mean":
                    for (let x = 0; x < outer; x++) for (let r = 0; r < block; r++) for (let k = 0, o = x * inner; k < inner; k++, o++) O[o] += A[i++];
                    if (op === "mean") for (let k = 0; k < O.length; k++) O[k] /= count;
                    break;
                case "prod":
                    for (let x = 0; x < outer; x++) for (let r = 0; r < block; r++) for (let k = 0, o = x * inner; k < inner; k++, o++) O[o] *= A[i++];
                    break;
                case "max":
                    for (let x = 0; x < outer; x++) for (let r = 0; r < block; r++) for (let k = 0, o = x * inner; k < inner; k++, o++) { const v = A[i++]; if (v > O[o] || v !== v) O[o] = v; }
                    break;
                case "min":
                    for (let x = 0; x < outer; x++) for (let r = 0; r < block; r++) for (let k = 0, o = x * inner; k < inner; k++, o++) { const v = A[i++]; if (v < O[o] || v !== v) O[o] = v; }
                    break;
                case "argmax":
                    for (let x = 0; x < outer; x++) for (let r = 0; r < block; r++) for (let k = 0, o = x * inner; k < inner; k++, o++) { const v = A[i++], b = best[o]; if (v > b || (v !== v && b === b)) { best[o] = v; O[o] = r; } }
                    break;
                case "argmin":
                    for (let x = 0; x < outer; x++) for (let r = 0; r < block; r++) for (let k = 0, o = x * inner; k < inner; k++, o++) { const v = A[i++], b = best[o]; if (v < b || (v !== v && b === b)) { best[o] = v; O[o] = r; } }
                    break;
                case "all":
                    for (let x = 0; x < outer; x++) for (let r = 0; r < block; r++) for (let k = 0, o = x * inner; k < inner; k++, o++) if (!A[i++]) O[o] = 0;
                    break;
                case "any":
                    for (let x = 0; x < outer; x++) for (let r = 0; r < block; r++) for (let k = 0, o = x * inner; k < inner; k++, o++) if (A[i++]) O[o] = 1;
                    break;
            }
            if (O !== out.data) out.data.set(O);
            return tuple([out, pyShape(finalShape)]);
        }
        // Map every input index to its output offset.
        const inS = strides(shape), outS = strides(keptShape);
        let oo = 0, pos = new Array(rank).fill(0);
        for (let flat = 0; flat < nIn; flat++) {
            const x = A[flat];
            switch (op) {
                case "sum": case "mean": O[oo] += x; break;
                case "prod": O[oo] *= x; break;
                case "max": if (x > O[oo] || x !== x) O[oo] = x; break;
                case "min": if (x < O[oo] || x !== x) O[oo] = x; break;
                case "argmax": { const b = best[oo]; if (x > b || (x !== x && b === b)) { best[oo] = x; O[oo] = redIndex(pos, isRed, shape); } break; }
                case "argmin": { const b = best[oo]; if (x < b || (x !== x && b === b)) { best[oo] = x; O[oo] = redIndex(pos, isRed, shape); } break; }
                case "all": if (!x) O[oo] = 0; break;
                case "any": if (x) O[oo] = 1; break;
                default: fail(E.ValueError, "unknown reduction " + op);
            }
            for (let d = rank - 1; d >= 0; d--) {
                pos[d]++; if (!isRed[d]) oo += outS[d];
                if (pos[d] < shape[d]) break;
                if (!isRed[d]) oo -= outS[d] * shape[d];
                pos[d] = 0;
            }
        }
        if (op === "mean") for (let i = 0; i < O.length; i++) O[i] /= count;
        if (O !== out.data) out.data.set(O);
        return tuple([out, pyShape(finalShape)]);
    }
    // The flat index within the reduced dims (row-major over them).
    function redIndex(pos, isRed, shape) {
        let out = 0;
        for (let d = 0; d < shape.length; d++) if (isRed[d]) out = out * shape[d] + pos[d];
        return out;
    }
    // ---- shape kernels -----------------------------------------------------------------
    // O[o] = A[base + offset of o], the offset stepping by the strides `sa`
    // over `shape` in row-major order: forEachBroadcast's walk with the last
    // dim as an inline loop and no callback.
    function gatherStrided(O, A, shape, sa, base) {
        const rank = shape.length, n = O.length;
        if (n >= NATIVE_MIN && NATIVE !== null && NATIVE(N_GATHER, A, O, shape, sa, base)) return;
        if (rank === 0) { if (n) O[0] = A[base]; return; }
        const lastN = shape[rank - 1], lastS = sa[rank - 1], idx = new Array(rank).fill(0);
        let x = base, o = 0;
        while (o < n) {
            for (let k = 0, p = x; k < lastN; k++, p += lastS) O[o++] = A[p];
            let d = rank - 2;
            for (; d >= 0; d--) { idx[d]++; x += sa[d]; if (idx[d] < shape[d]) break; x -= sa[d] * shape[d]; idx[d] = 0; }
            if (d < 0) break;
        }
    }
    // A contiguous copy of the elements a (storage offset, strides) view
    // reads: PyTorch checkpoints store non-contiguous tensors that way.
    function strided(a, shape, st, offset) {
        const n = numel(shape);
        if (n > 0) {
            let lo = offset, hi = offset;
            for (let d = 0; d < shape.length; d++) { if (st[d] < 0) fail(E.RuntimeError, "negative strides are not supported"); hi += (shape[d] - 1) * st[d]; }
            if (lo < 0 || hi >= a.data.length) fail(E.RuntimeError, "setStorage: sizes " + JSON.stringify(shape) + ", strides " + JSON.stringify(st) + " and storage offset " + offset + " are out of bounds for storage of size " + a.data.length);
        }
        const out = alloc(a.dtype, n);
        if (n > 0) gatherStrided(out.data, a.data, shape, st, offset);
        return out;
    }
    function permute(a, shape, perm) {
        const p = ints(perm), rank = shape.length;
        const outShape = p.map((d) => shape[d]);
        const inS = strides(shape), sa = p.map((d) => inS[d]);
        const out = alloc(a.dtype, a.data.length), O = out.data, A = a.data;
        if (O.length >= NATIVE_MIN && NATIVE !== null && NATIVE(N_GATHER, A, O, outShape, sa, 0)) {
            // The strided walk, run natively.
        } else if (rank === 2 && p[0] === 1 && p[1] === 0) {
            // A matrix transpose (every Linear's weight.T): column by column.
            const h = shape[0], w = shape[1];
            for (let j = 0, o = 0; j < w; j++) for (let i = 0, q = j; i < h; i++, q += w) O[o++] = A[q];
        } else {
            gatherStrided(O, A, outShape, sa, 0);
        }
        return tuple([out, pyShape(outShape)]);
    }
    function expand(a, shape, target) {
        const t = ints(target);
        const sa = bstrides(shape, t), out = alloc(a.dtype, numel(t)), O = out.data, A = a.data;
        gatherStrided(O, A, t, sa, 0);
        return out;
    }
    // spec: a Python list, one entry per dim: an int (drops the dim) or a
    // (start, stop, step) tuple.
    function sliceSpec(spec, shape) {
        const items = spec.items, starts = [], counts = [], steps = [], keep = [];
        for (let d = 0; d < shape.length; d++) {
            const it = d < items.length ? items[d] : null;
            if (it === null) { starts.push(0); counts.push(shape[d]); steps.push(1); keep.push(true); continue; }
            if (rt.isInt(it)) {
                let i = Number(rt.asInt(it)); if (i < 0) i += shape[d];
                if (i < 0 || i >= shape[d]) fail(E.IndexError, "index " + rt.asInt(it) + " is out of bounds for dimension " + d + " with size " + shape[d]);
                starts.push(i); counts.push(1); steps.push(1); keep.push(false); continue;
            }
            const parts = it.items;
            const step = parts[2] === null ? 1 : Number(rt.asInt(parts[2]));
            if (step <= 0) fail(E.ValueError, "step must be greater than zero");
            let start = parts[0] === null ? 0 : Number(rt.asInt(parts[0])), stop = parts[1] === null ? shape[d] : Number(rt.asInt(parts[1]));
            if (start < 0) start += shape[d]; if (stop < 0) stop += shape[d];
            start = Math.min(Math.max(start, 0), shape[d]); stop = Math.min(Math.max(stop, 0), shape[d]);
            const count = Math.max(0, Math.ceil((stop - start) / step));
            starts.push(start); counts.push(count); steps.push(step); keep.push(true);
        }
        return { starts, counts, steps, keep };
    }
    function slice(a, shape, spec) {
        const s = sliceSpec(spec, shape), inS = strides(shape);
        const outShape = [], sa = [];
        let base = 0;
        for (let d = 0; d < shape.length; d++) { base += s.starts[d] * inS[d]; if (s.keep[d]) { outShape.push(s.counts[d]); sa.push(inS[d] * s.steps[d]); } }
        const out = alloc(a.dtype, numel(outShape)), O = out.data, A = a.data;
        gatherStrided(O, A, outShape, sa, base);
        return tuple([out, pyShape(outShape)]);
    }
    function setSlice(a, shape, spec, v, vshape) {
        const s = sliceSpec(spec, shape), inS = strides(shape);
        const outShape = [], sa = [];
        let base = 0;
        for (let d = 0; d < shape.length; d++) { base += s.starts[d] * inS[d]; if (s.keep[d]) { outShape.push(s.counts[d]); sa.push(inS[d] * s.steps[d]); } }
        const sv = bstrides(vshape, outShape), A = a.data, V = v.data, f32 = a.dtype === "float32";
        forEachBroadcast(outShape, sa, sv, (o, x, y) => { A[base + x] = castValue(a.dtype, V[y]); });
        written(a);
        return null;
    }
    // Advanced indexing: `idx` is a Python list of int64 storages (already
    // broadcast to one shape `ishape`), one per leading dim.
    function gather(a, shape, idx, ishape) {
        const k = idx.items.length, inS = strides(shape), ish = ints(ishape), rest = shape.slice(k);
        const restN = numel(rest), nIdx = numel(ish);
        const outShape = ish.concat(rest), out = alloc(a.dtype, nIdx * restN), O = out.data, A = a.data;
        const I = idx.items.map((s) => s.data);
        for (let i = 0; i < nIdx; i++) {
            let base = 0;
            for (let d = 0; d < k; d++) { let j = I[d][i]; if (j < 0) j += shape[d]; if (j < 0 || j >= shape[d]) fail(E.IndexError, "index " + I[d][i] + " is out of bounds for dimension " + d + " with size " + shape[d]); base += j * inS[d]; }
            for (let r = 0; r < restN; r++) O[i * restN + r] = A[base + r];
        }
        return tuple([out, pyShape(outShape)]);
    }
    function scatter(a, shape, idx, ishape, v, vshape) {
        const k = idx.items.length, inS = strides(shape), ish = ints(ishape), rest = shape.slice(k);
        const restN = numel(rest), nIdx = numel(ish), target = ish.concat(rest);
        const sv = bstrides(vshape, target), A = a.data, V = v.data;
        const I = idx.items.map((s) => s.data);
        // Value offset for [i, r]: walk with forEachBroadcast over the target shape.
        const restS = strides(rest);
        let flat = 0;
        const idxRank = ish.length, tRank = target.length;
        forEachBroadcast(target, new Array(tRank).fill(0), sv, (o, x, y) => {
            const i = Math.floor(o / restN), r = o - i * restN;
            let base = 0;
            for (let d = 0; d < k; d++) { let j = I[d][i]; if (j < 0) j += shape[d]; if (j < 0 || j >= shape[d]) fail(E.IndexError, "index out of bounds"); base += j * inS[d]; }
            A[base + r] = castValue(a.dtype, V[y]);
        });
        written(a);
        return null;
    }
    // Like scatter, but accumulating: the gradient of a gather.
    function scatterAdd(a, shape, idx, ishape, v, vshape) {
        const k = idx.items.length, inS = strides(shape), ish = ints(ishape), rest = shape.slice(k);
        const restN = numel(rest), target = ish.concat(rest);
        const sv = bstrides(vshape, target), A = a.data, V = v.data;
        const I = idx.items.map((s) => s.data);
        forEachBroadcast(target, new Array(target.length).fill(0), sv, (o, x, y) => {
            const i = Math.floor(o / restN), r = o - i * restN;
            let base = 0;
            for (let d = 0; d < k; d++) { let j = I[d][i]; if (j < 0) j += shape[d]; if (j < 0 || j >= shape[d]) fail(E.IndexError, "index out of bounds"); base += j * inS[d]; }
            A[base + r] = castValue(a.dtype, A[base + r] + V[y]);
        });
        written(a);
        return null;
    }
    function indexSelect(a, shape, dim, indices) {
        const d = dim < 0 ? dim + shape.length : dim, I = indices.data;
        const outShape = shape.slice(); outShape[d] = I.length;
        const inner = numel(shape.slice(d + 1)), outer = numel(shape.slice(0, d));
        const out = alloc(a.dtype, numel(outShape)), O = out.data, A = a.data;
        let o = 0;
        for (let x = 0; x < outer; x++) for (let i = 0; i < I.length; i++) {
            let j = I[i]; if (j < 0) j += shape[d]; if (j < 0 || j >= shape[d]) fail(E.IndexError, "index out of range");
            const base = (x * shape[d] + j) * inner;
            for (let r = 0; r < inner; r++) O[o++] = A[base + r];
        }
        return tuple([out, pyShape(outShape)]);
    }
    function cat(parts, dim) {
        const items = parts.items; if (items.length === 0) fail(E.RuntimeError, "cat expects a non-empty list");
        const shapes = items.map((p) => shapeOf(p.items[1])), first = shapes[0];
        const d = dim < 0 ? dim + first.length : dim;
        let dtype = items[0].items[0].dtype, total = 0;
        for (let i = 0; i < items.length; i++) {
            const s = shapes[i]; if (s.length !== first.length) fail(E.RuntimeError, "Tensors must have same number of dimensions");
            for (let k = 0; k < s.length; k++) if (k !== d && s[k] !== first[k]) fail(E.RuntimeError, "Sizes of tensors must match except in dimension " + d);
            total += s[d]; dtype = promote(dtype, items[i].items[0].dtype);
        }
        const outShape = first.slice(); outShape[d] = total;
        const out = alloc(dtype, numel(outShape)), O = out.data;
        const outer = numel(first.slice(0, d)), inner = numel(first.slice(d + 1));
        let o = 0;
        for (let x = 0; x < outer; x++) for (let i = 0; i < items.length; i++) {
            const A = items[i].items[0].data, n = shapes[i][d] * inner, base = x * n;
            for (let r = 0; r < n; r++) O[o++] = A[base + r];
        }
        return tuple([out, pyShape(outShape)]);
    }
    function roll(a, shape, shift, dim) {
        const d = dim < 0 ? dim + shape.length : dim, n = shape[d];
        const out = alloc(a.dtype, a.data.length), O = out.data, A = a.data;
        const outer = numel(shape.slice(0, d)), inner = numel(shape.slice(d + 1));
        const s = ((shift % n) + n) % n;
        for (let x = 0; x < outer; x++) for (let i = 0; i < n; i++) {
            const src = x * n * inner + i * inner, dst = x * n * inner + ((i + s) % n) * inner;
            for (let r = 0; r < inner; r++) O[dst + r] = A[src + r];
        }
        return out;
    }
    function padLast(a, shape, left, right, value) {
        const last = shape[shape.length - 1], outer = a.data.length / (last || 1);
        const newLast = last + left + right, outShape = shape.slice(); outShape[shape.length - 1] = newLast;
        const out = alloc(a.dtype, outer * newLast), O = out.data, A = a.data;
        if (value !== 0) O.fill(value);
        for (let x = 0; x < outer; x++) for (let i = 0; i < last; i++) O[x * newLast + left + i] = A[x * last + i];
        return tuple([out, pyShape(outShape)]);
    }
    // ---- linear algebra ----------------------------------------------------------------
    function matmul(a, ashape, b, bshape) {
        let A = ashape.slice(), Bs = bshape.slice();
        if (A.length === 0 || Bs.length === 0) fail(E.RuntimeError, "both arguments to matmul need to be at least 1D");
        const squeezeA = A.length === 1, squeezeB = Bs.length === 1;
        if (squeezeA) A = [1, A[0]]; if (squeezeB) Bs = [Bs[0], 1];
        const m = A[A.length - 2], k = A[A.length - 1], k2 = Bs[Bs.length - 2], n = Bs[Bs.length - 1];
        if (k !== k2) fail(E.RuntimeError, "mat1 and mat2 shapes cannot be multiplied (" + m + "x" + k + " and " + k2 + "x" + n + ")");
        const batchA = A.slice(0, -2), batchB = Bs.slice(0, -2), batch = broadcastShape(batchA, batchB);
        const nb = numel(batch), sa = bstrides(batchA, batch), sb = bstrides(batchB, batch);
        const dtype = promote(a.dtype, b.dtype);
        const out = alloc(dtype, nb * m * n), O = out.data, Ad = a.data, Bd = b.data;
        // i-k-j order over one double-precision row: each output element
        // sums its k products in the same order as i-j-k, and rounds to the
        // dtype once, on store. float64 results are unchanged; float32 ones
        // no longer round every partial sum, which moves them by at most a
        // few ulps (the GPU reference keeps that rounding: `graph_matmul`).
        const row = new Float64Array(n);
        forEachBroadcast(batch, sa, sb, (bi, oa, ob) => {
            const baseA = oa * m * k, baseB = ob * k * n, baseO = bi * m * n;
            if (NATIVE !== null && NATIVE(N_MATMUL, Ad, Bd, O, baseA, baseB, baseO, m, k, n)) return;
            for (let i = 0; i < m; i++) {
                row.fill(0);
                for (let p = 0, ia = baseA + i * k, ib = baseB; p < k; p++, ib += n) {
                    const x = Ad[ia + p];
                    for (let j = 0; j < n; j++) row[j] += x * Bd[ib + j];
                }
                O.set(row, baseO + i * n);
            }
        });
        let outShape = batch.concat([m, n]);
        if (squeezeA) outShape.splice(outShape.length - 2, 1);
        if (squeezeB) outShape.splice(outShape.length - 1, 1);
        return tuple([out, pyShape(outShape)]);
    }
    // conv1d, stride 1, no padding, no dilation: x [B,C,L], w [O,C,K], bias [O]?
    function conv1d(x, xs, w, ws, bias) {
        const [B, C, L] = xs, [Oc, C2, K] = ws;
        if (C !== C2) fail(E.RuntimeError, "conv1d: expected input with " + C2 + " channels, got " + C);
        const Lo = L - K + 1; if (Lo < 1) fail(E.RuntimeError, "conv1d: kernel size can't be greater than actual input size");
        const dtype = promote(x.dtype, w.dtype), out = alloc(dtype, B * Oc * Lo), O = out.data, X = x.data, W = w.data;
        const Bi = bias === null ? null : bias.data;
        if (NATIVE !== null && NATIVE(N_CONV1D, X, W, Bi, O, B, C, L, Oc, K, Lo)) return tuple([out, pyShape([B, Oc, Lo])]);
        for (let b = 0; b < B; b++) for (let o = 0; o < Oc; o++) {
            const bv = Bi === null ? 0 : Bi[o];
            for (let t = 0; t < Lo; t++) {
                let s = bv;
                for (let c = 0; c < C; c++) { const xb = (b * C + c) * L + t, wb = (o * C + c) * K; for (let k = 0; k < K; k++) s += X[xb + k] * W[wb + k]; }
                O[(b * Oc + o) * Lo + t] = s;
            }
        }
        return tuple([out, pyShape([B, Oc, Lo])]);
    }
    function conv1dBackward(x, xs, w, ws, g) {
        const [B, C, L] = xs, [Oc, , K] = ws, Lo = L - K + 1;
        const gx = alloc(x.dtype, B * C * L), gw = alloc(w.dtype, Oc * C * K), gb = alloc(w.dtype, Oc);
        const GX = gx.data, GW = gw.data, GB = gb.data, X = x.data, W = w.data, G = g.data;
        if (NATIVE !== null && NATIVE(N_CONV1D_BACKWARD, X, W, G, GX, GW, GB, B, C, L, Oc, K, Lo)) return tuple([gx, gw, gb]);
        for (let b = 0; b < B; b++) for (let o = 0; o < Oc; o++) for (let t = 0; t < Lo; t++) {
            const gv = G[(b * Oc + o) * Lo + t]; if (gv === 0) continue;
            GB[o] += gv;
            for (let c = 0; c < C; c++) { const xb = (b * C + c) * L + t, wb = (o * C + c) * K; for (let k = 0; k < K; k++) { GX[xb + k] += gv * W[wb + k]; GW[wb + k] += gv * X[xb + k]; } }
        }
        return tuple([gx, gw, gb]);
    }
    // Contiguous NCHW cross-correlation; the same index walk computes gradients.
    function conv2d(x, xs, w, ws, bias, stride, padding, dilation, groups, grad = null) {
        if (xs.length !== 4 || ws.length !== 4 || stride.length !== 2 || padding.length !== 2 || dilation.length !== 2)
            fail(E.RuntimeError, "conv2d: invalid dimensions");
        const [B, C, H, W] = xs, [O, Cg, Kh, Kw] = ws;
        if (![B,C,H,W,O,Cg,Kh,Kw,groups,...stride,...padding,...dilation].every(Number.isSafeInteger) ||
            B < 0 || Math.min(C,H,W,O,Cg,Kh,Kw,groups,...stride,...dilation) < 1 || Math.min(...padding) < 0 ||
            C !== Cg * groups || O % groups !== 0 || x.data.length !== B*C*H*W || w.data.length !== O*Cg*Kh*Kw ||
            !["float32","float64"].includes(x.dtype) || x.dtype !== w.dtype || (bias && (bias.data.length !== O || bias.dtype !== x.dtype)))
            fail(E.RuntimeError, "conv2d: incompatible shape, dtype or convolution parameters");
        const [Sh, Sw] = stride, [Ph, Pw] = padding, [Dh, Dw] = dilation;
        const Ho = Math.floor((H + 2*Ph - Dh*(Kh-1) - 1)/Sh) + 1;
        const Wo = Math.floor((W + 2*Pw - Dw*(Kw-1) - 1)/Sw) + 1;
        if (Ho < 1 || Wo < 1) fail(E.RuntimeError, "conv2d: kernel exceeds padded input");
        if (grad && (grad.data.length !== B*O*Ho*Wo || grad.dtype !== x.dtype)) fail(E.RuntimeError, "conv2d: invalid gradient");
        const out = grad ? null : alloc(x.dtype, B*O*Ho*Wo);
        const gx = grad ? alloc(x.dtype, x.data.length) : null;
        const gw = grad ? alloc(w.dtype, w.data.length) : null;
        const gb = grad ? alloc(w.dtype, O) : null;
        const perGroup = O / groups;
        if (NATIVE !== null && (grad
            ? NATIVE(N_CONV2D_BACKWARD, x.data, w.data, grad.data, gx.data, gw.data, gb.data, B, C, H, W, O, Cg, Kh, Kw, Sh, Sw, Ph, Pw, Dh, Dw, groups, Ho, Wo)
            : NATIVE(N_CONV2D, x.data, w.data, bias ? bias.data : null, out.data, B, C, H, W, O, Cg, Kh, Kw, Sh, Sw, Ph, Pw, Dh, Dw, groups, Ho, Wo)))
            return grad ? tuple([gx,gw,gb]) : tuple([out,pyShape([B,O,Ho,Wo])]);
        for (let b=0; b<B; b++) for (let o=0; o<O; o++) {
            const firstChannel = Math.floor(o/perGroup)*Cg;
            for (let h=0; h<Ho; h++) for (let v=0; v<Wo; v++) {
                const oi = ((b*O+o)*Ho+h)*Wo+v;
                const gv = grad ? grad.data[oi] : 0;
                let sum = bias ? bias.data[o] : 0;
                if (grad) gb.data[o] += gv;
                for (let c=0; c<Cg; c++) for (let kh=0; kh<Kh; kh++) for (let kw=0; kw<Kw; kw++) {
                    const ih = h*Sh-Ph+kh*Dh, iw = v*Sw-Pw+kw*Dw;
                    if (ih<0 || ih>=H || iw<0 || iw>=W) continue;
                    const xi = ((b*C+firstChannel+c)*H+ih)*W+iw;
                    const wi = ((o*Cg+c)*Kh+kh)*Kw+kw;
                    if (grad) { gx.data[xi] += gv*w.data[wi]; gw.data[wi] += gv*x.data[xi]; }
                    else sum += x.data[xi]*w.data[wi];
                }
                if (!grad) out.data[oi] = sum;
            }
        }
        return grad ? tuple([gx,gw,gb]) : tuple([out,pyShape([B,O,Ho,Wo])]);
    }
    function softmax(a, shape, dim, log) {
        const d = dim < 0 ? dim + shape.length : dim, n = shape[d];
        const outer = numel(shape.slice(0, d)), inner = numel(shape.slice(d + 1));
        const dtype = RANK[a.dtype] < RANK.float32 ? "float32" : a.dtype, out = alloc(dtype, a.data.length), O = out.data, A = a.data;
        if (NATIVE !== null && NATIVE(N_SOFTMAX, A, O, outer, n, inner, !!log)) return out;
        for (let x = 0; x < outer; x++) for (let r = 0; r < inner; r++) {
            const base = x * n * inner + r;
            let mx = -Infinity; for (let i = 0; i < n; i++) { const v = A[base + i * inner]; if (v > mx) mx = v; }
            let sum = 0; for (let i = 0; i < n; i++) sum += Math.exp(A[base + i * inner] - mx);
            const ls = Math.log(sum);
            for (let i = 0; i < n; i++) { const z = A[base + i * inner] - mx; O[base + i * inner] = log ? z - ls : Math.exp(z) / sum; }
        }
        return out;
    }
    // A running reduction along `dim`, accumulated in double precision and
    // rounded on store. cummax/cummin also return the index of each running
    // extreme (the latest one on ties, and NaN propagating, as PyTorch).
    function scan(op, a, shape, dim) {
        const d = dim < 0 ? dim + shape.length : dim, n = shape.length === 0 ? 1 : shape[d];
        const outer = shape.length === 0 ? 1 : numel(shape.slice(0, d)), inner = shape.length === 0 ? 1 : numel(shape.slice(d + 1));
        const out = alloc(a.dtype, a.data.length), O = out.data, A = a.data;
        const arg = op === "cummax" || op === "cummin", idx = arg ? alloc("int64", a.data.length) : null, I = arg ? idx.data : null;
        for (let x = 0; x < outer; x++) for (let r = 0; r < inner; r++) {
            const base = x * n * inner + r;
            let acc = op === "cumprod" ? 1 : op === "logcumsumexp" ? -Infinity : 0, at = 0;
            for (let i = 0; i < n; i++) {
                const p = base + i * inner, v = A[p];
                switch (op) {
                    case "cumsum": acc += v; break;
                    case "cumprod": acc *= v; break;
                    case "cummax": if (i === 0 || acc !== acc) { if (i === 0) { acc = v; at = 0; } } else if (v >= acc || v !== v) { acc = v; at = i; } break;
                    case "cummin": if (i === 0 || acc !== acc) { if (i === 0) { acc = v; at = 0; } } else if (v <= acc || v !== v) { acc = v; at = i; } break;
                    case "logcumsumexp": { const m = Math.max(acc, v); acc = m === -Infinity ? -Infinity : m === Infinity ? Infinity : m + Math.log(Math.exp(acc - m) + Math.exp(v - m)); break; }
                    default: fail(E.ValueError, "unknown scan " + op);
                }
                O[p] = acc;
                if (arg) I[p] = at;
            }
        }
        return arg ? tuple([out, idx]) : out;
    }
    // Stable merge sort of indices along `dim`.
    function argsort(a, shape, dim, descending) {
        const d = dim < 0 ? dim + shape.length : dim, n = shape[d];
        const outer = numel(shape.slice(0, d)), inner = numel(shape.slice(d + 1));
        const out = alloc("int64", a.data.length), O = out.data, A = a.data;
        const idx = new Float64Array(n), tmp = new Float64Array(n), keys = new Float64Array(n);
        for (let x = 0; x < outer; x++) for (let r = 0; r < inner; r++) {
            const base = x * n * inner + r;
            for (let i = 0; i < n; i++) { idx[i] = i; keys[i] = A[base + i * inner]; }
            mergeSort(idx, tmp, keys, n, descending);
            for (let i = 0; i < n; i++) O[base + i * inner] = idx[i];
        }
        return out;
    }
    function mergeSort(idx, tmp, keys, n, desc) {
        for (let width = 1; width < n; width *= 2) {
            for (let lo = 0; lo < n; lo += 2 * width) {
                const mid = Math.min(lo + width, n), hi = Math.min(lo + 2 * width, n);
                let i = lo, j = mid, k = lo;
                while (i < mid && j < hi) {
                    const a = keys[idx[i]], b = keys[idx[j]];
                    // NaN is the largest value (last ascending, first
                    // descending), as PyTorch sorts; ties keep their order.
                    const takeLeft = a !== a ? (desc || b !== b) : b !== b ? !desc : desc ? !(b > a) : !(b < a);
                    tmp[k++] = takeLeft ? idx[i++] : idx[j++];
                }
                while (i < mid) tmp[k++] = idx[i++];
                while (j < hi) tmp[k++] = idx[j++];
                for (let q = lo; q < hi; q++) idx[q] = tmp[q];
            }
        }
    }
    function oneHot(a, n) {
        const out = alloc("int64", a.data.length * n), O = out.data, A = a.data;
        for (let i = 0; i < A.length; i++) { const j = A[i]; if (j < 0 || j >= n) fail(E.RuntimeError, "Class values must be smaller than num_classes."); O[i * n + j] = 1; }
        return out;
    }
    function where(c, cs, a, as_, b, bs, want) {
        const shape = broadcastShape(broadcastShape(cs, as_), bs), dtype = want ? want : promote(a.dtype, b.dtype);
        const out = alloc(dtype, numel(shape)), O = out.data;
        const sc = bstrides(cs, shape), sa = bstrides(as_, shape), sb = bstrides(bs, shape);
        const Cd = c.data, Ad = a.data, Bd = b.data;
        if (O.length >= NATIVE_MIN && NATIVE !== null && NATIVE(N_WHERE, Cd, Ad, Bd, O, shape, sc, sa, sb)) return tuple([out, pyShape(shape)]);
        const rank = shape.length, idx = new Array(rank).fill(0);
        let oc = 0, oa = 0, ob = 0;
        for (let flat = 0; flat < O.length; flat++) {
            O[flat] = Cd[oc] ? Ad[oa] : Bd[ob];
            for (let d = rank - 1; d >= 0; d--) { idx[d]++; oc += sc[d]; oa += sa[d]; ob += sb[d]; if (idx[d] < shape[d]) break; oc -= sc[d] * shape[d]; oa -= sa[d] * shape[d]; ob -= sb[d] * shape[d]; idx[d] = 0; }
        }
        return tuple([out, pyShape(shape)]);
    }
    // ---- random: MT19937, with PyTorch's CPU transforms ------------------------------------------
    function mt(seed) {
        const s = { mt: new Uint32Array(624), i: 625 };
        seedMt(s, seed >>> 0);
        return s;
    }
    function seedMt(s, seed) {
        const m = s.mt; m[0] = seed >>> 0;
        for (let i = 1; i < 624; i++) { const prev = m[i - 1] ^ (m[i - 1] >>> 30); m[i] = (Math.imul(1812433253, prev) + i) >>> 0; }
        s.i = 624;
    }
    function next32(s) {
        const m = s.mt;
        if (s.i >= 624) {
            for (let k = 0; k < 624; k++) {
                const y = (m[k] & 0x80000000) | (m[(k + 1) % 624] & 0x7fffffff);
                m[k] = (m[(k + 397) % 624] ^ (y >>> 1) ^ ((y & 1) ? 0x9908b0df : 0)) >>> 0;
            }
            s.i = 0;
        }
        let y = m[s.i++];
        y ^= y >>> 11; y ^= (y << 7) & 0x9d2c5680; y ^= (y << 15) & 0xefc60000; y ^= y >>> 18;
        return y >>> 0;
    }
    // A uniform double in [0, 1) from 53 bits, as torch's random64 path.
    function nextDouble(s) { const hi = next32(s), lo = next32(s); const v = (hi * 4294967296 + lo) % 9007199254740992; return v / 9007199254740992; }
    const Gen = rt.newType("Generator", [rt.ObjectType], new Map(), "_zipp_tensor");
    function genNew(seed) { return { cls: Gen, dict: new Map(), state: mt(Number(BigInt.asUintN(32, BigInt(seed)))), seed: BigInt(seed) }; }
    // A generator's whole state as a uint8 storage: the 624 MT words, the
    // position (little-endian uint32 each) and the 64-bit seed.
    function genGetState(g) {
        const s = needGen(g), out = alloc("uint8", 624 * 4 + 4 + 8), v = new DataView(out.data.buffer);
        for (let i = 0; i < 624; i++) v.setUint32(i * 4, s.mt[i], true);
        v.setUint32(624 * 4, s.i, true);
        v.setBigUint64(624 * 4 + 4, BigInt.asUintN(64, g.seed), true);
        return out;
    }
    function genSetState(g, st) {
        needGen(g); needS(st);
        if (st.data.length !== 624 * 4 + 4 + 8) fail(E.RuntimeError, "Expected a generator state of " + (624 * 4 + 4 + 8) + " bytes, got " + st.data.length);
        const bytes = Uint8Array.from(st.data), v = new DataView(bytes.buffer), mtState = { mt: new Uint32Array(624), i: 0 };
        for (let i = 0; i < 624; i++) mtState.mt[i] = v.getUint32(i * 4, true);
        mtState.i = v.getUint32(624 * 4, true);
        if (mtState.i > 624) fail(E.RuntimeError, "invalid generator state");
        g.state = mtState; g.seed = v.getBigUint64(624 * 4 + 4, true);
        return null;
    }
    function needGen(g) { if (g === null || typeof g !== "object" || g.cls !== Gen) fail(E.TypeError, "a Generator is required"); return g.state; }
    function rand(g, n) {
        // float32 uniform from 24 random bits, as torch's uniform_ for float.
        const s = needGen(g), out = alloc("float32", n), O = out.data;
        for (let i = 0; i < n; i++) O[i] = (next32(s) & 0xffffff) * 5.9604644775390625e-8;
        return out;
    }
    function randDouble(g, n) {
        const s = needGen(g), out = alloc("float64", n), O = out.data;
        for (let i = 0; i < n; i++) O[i] = nextDouble(s);
        return out;
    }
    function randn(g, n, dtype) {
        // Box-Muller on doubles; pairs, as torch's normal_ (scalar path).
        const s = needGen(g), out = alloc(dtype, n), O = out.data;
        for (let i = 0; i < n; i += 2) {
            const u1 = 1 - nextDouble(s), u2 = nextDouble(s);
            const r = Math.sqrt(-2 * Math.log(u1)), t = 2 * Math.PI * u2;
            O[i] = r * Math.cos(t);
            if (i + 1 < n) O[i + 1] = r * Math.sin(t);
        }
        return out;
    }
    // PyTorch's CPU randint: a range below 2**28 takes one 32-bit word per
    // element; a larger one takes random64() (two words, the first high)
    // modulo the range. One exception: a range of exactly 2**32 stays one
    // raw word, which torch.utils.data composes into its random64 seed.
    function randint(g, low, high, n) {
        const s = needGen(g), out = alloc("int64", n), O = out.data;
        const range = high - low;
        if (range <= 0) fail(E.RuntimeError, "random_ expects 'from' to be less than 'to'");
        for (let i = 0; i < n; i++) {
            const r = range < 268435456 || range === 4294967296 ? next32(s) % range : Number(((BigInt(next32(s)) << 32n) | BigInt(next32(s))) % BigInt(range));
            O[i] = low + r;
        }
        return out;
    }
    // PyTorch's random64(): two words, the first high, as Python ints.
    function random64(g, n) {
        const s = needGen(g), out = new Array(n);
        for (let i = 0; i < n; i++) out[i] = (BigInt(next32(s)) << 32n) | BigInt(next32(s));
        return list(out);
    }
    function multinomial(g, probs, shape, samples, replacement) {
        const s = needGen(g), n = shape[shape.length - 1], rows = probs.data.length / n;
        const out = alloc("int64", rows * samples), O = out.data, P = probs.data;
        const w = new Float64Array(n);
        for (let r = 0; r < rows; r++) {
            for (let i = 0; i < n; i++) { w[i] = P[r * n + i]; if (w[i] < 0 || w[i] !== w[i]) fail(E.RuntimeError, "probability tensor contains either `inf`, `nan` or element < 0"); }
            for (let k = 0; k < samples; k++) {
                let total = 0; for (let i = 0; i < n; i++) total += w[i];
                if (total <= 0) fail(E.RuntimeError, "invalid multinomial distribution (sum of probabilities <= 0)");
                const u = nextDouble(s) * total;
                let acc = 0, pick = n - 1;
                for (let i = 0; i < n; i++) { acc += w[i]; if (u < acc && w[i] > 0) { pick = i; break; } }
                O[r * samples + k] = pick;
                if (!replacement) w[pick] = 0;
            }
        }
        return out;
    }
    // PyTorch's CPU randperm: a forward Fisher-Yates shuffle, position i
    // swapped with i + random() % (n - i).
    function randperm(g, n) {
        const s = needGen(g), out = alloc("int64", n), O = out.data;
        for (let i = 0; i < n; i++) O[i] = i;
        for (let i = 0; i < n - 1; i++) { const j = i + next32(s) % (n - i); const t = O[i]; O[i] = O[j]; O[j] = t; }
        return out;
    }
    // ---- bytes ---------------------------------------------------------------------------------
    function toBytes(a) {
        const n = a.data.length;
        let bytes;
        if (a.dtype === "float32") bytes = new Uint8Array(Float32Array.from(a.data).buffer);
        else if (a.dtype === "float64") bytes = new Uint8Array(Float64Array.from(a.data).buffer);
        else if (a.dtype === "int64") { const b = new ArrayBuffer(n * 8), v = new DataView(b); for (let i = 0; i < n; i++) v.setBigInt64(i * 8, BigInt(Math.trunc(a.data[i])), true); bytes = new Uint8Array(b); }
        else if (a.dtype === "int32") { const b = new ArrayBuffer(n * 4), v = new DataView(b); for (let i = 0; i < n; i++) v.setInt32(i * 4, a.data[i], true); bytes = new Uint8Array(b); }
        else bytes = Uint8Array.from(a.data);
        return rt.bytes(rt.bytesFromU8(bytes));
    }
    let CRC_TABLE = null;
    function crc32(items, start) {
        if (CRC_TABLE === null) {
            CRC_TABLE = new Int32Array(256);
            for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = (c & 1) ? 0xEDB88320 ^ (c >>> 1) : c >>> 1; CRC_TABLE[n] = c; }
        }
        let c = start ^ 0xFFFFFFFF;
        for (let i = 0; i < items.length; i++) c = CRC_TABLE[(c ^ items[i]) & 0xFF] ^ (c >>> 8);
        return (c ^ 0xFFFFFFFF) >>> 0;
    }
    function fromBytes(dtype, b, count) {
        const items = b.items, buf = new ArrayBuffer(items.length), u8 = new Uint8Array(buf);
        for (let i = 0; i < items.length; i++) u8[i] = items[i];
        const view = new DataView(buf);
        const n = count === null ? undefined : count;
        if (dtype === "float32") { const m = n === undefined ? items.length / 4 : n, out = alloc("float32", m); for (let i = 0; i < m; i++) out.data[i] = view.getFloat32(i * 4, true); return out; }
        if (dtype === "float64") { const m = n === undefined ? items.length / 8 : n, out = alloc("float64", m); for (let i = 0; i < m; i++) out.data[i] = view.getFloat64(i * 8, true); return out; }
        if (dtype === "int64") { const m = n === undefined ? items.length / 8 : n, out = alloc("int64", m); for (let i = 0; i < m; i++) out.data[i] = Number(view.getBigInt64(i * 8, true)); return out; }
        if (dtype === "int32") { const m = n === undefined ? items.length / 4 : n, out = alloc("int32", m); for (let i = 0; i < m; i++) out.data[i] = view.getInt32(i * 4, true); return out; }
        if (dtype === "bool" || dtype === "uint8") { const m = n === undefined ? items.length : n, out = alloc(dtype, m); for (let i = 0; i < m; i++) out.data[i] = items[i]; return out; }
        fail(E.TypeError, "unknown dtype " + dtype);
    }
    // ---- the zipp_gpu float32 reference ------------------------------------------------------------
    // Without a host, `zipp_gpu` evaluates a graph with these: bit for bit
    // what its pure-Python reference (`execute_locally`) computes, which is
    // the contract every backend meets. Every intermediate the reference
    // rounds to float32 is rounded here, in the same order: a Float32Array
    // store rounds once, `f` rounds a value the reference rounds before
    // using it again. matmul rounds every partial sum, a whole-tensor sum
    // reduces pairwise (an odd tail adds 0), an axis sum is `reduce`'s
    // index-order accumulation, softmax subtracts the row maximum,
    // exponentiates and divides by the rounded row sum, and the optimizer
    // steps compose as `_optimizer_step` does. Transcendentals are the same
    // `Math` functions the Python `math` module calls.
    const f32 = Math.fround;
    // [batch, m, k] @ [batch, k, n]; a batch stride of 0 broadcasts that
    // operand. i-k-j order: each output element still sums its k products
    // in k order, rounding each product and each partial sum. The product
    // rounds through a one-element Float32Array rather than a call: on the
    // interpreter that is the cheapest rounding, and the same value.
    function graphMatmul(a, b, m, k, n, batch, aStride, bStride) {
        const out = alloc("float32", batch * m * n), O = out.data, A = a.data, B = b.data, P = new Float32Array(1);
        for (let t = 0; t < batch; t++) {
            const ao = t * aStride, bo = t * bStride, oo = t * m * n;
            for (let r = 0; r < m; r++) {
                const ro = oo + r * n, ia = ao + r * k;
                for (let j = 0; j < k; j++) {
                    const x = A[ia + j], base = bo + j * n;
                    for (let c = 0; c < n; c++) { P[0] = x * B[base + c]; O[ro + c] += P[0]; }
                }
            }
        }
        return out;
    }
    // Pairwise float32 sum of `work` (a Float32Array this may overwrite):
    // step i reads 2i and 2i+1, never below i.
    function pairwise(work) {
        let len = work.length;
        while (len > 1) {
            const half = Math.ceil(len / 2);
            for (let i = 0; i < half; i++) work[i] = work[2 * i] + (2 * i + 1 < len ? work[2 * i + 1] : 0);
            len = half;
        }
        return work[0];
    }
    function pairSum(a, mean) {
        const n = a.data.length;
        if (n === 0) fail(E.ValueError, "sum of an empty graph tensor");
        const out = alloc("float32", 1), total = pairwise(new Float32Array(a.data));
        out.data[0] = mean ? total / n : total;
        return out;
    }
    // The elementwise functions of the graph protocol. relu keeps NaN (so a
    // diverged value reaches the finite check at readback), sigmoid takes
    // the overflow-free branch by sign, log and sqrt of a negative are NaN
    // and log 0 is -infinity, as the reference's guarded `math` calls give.
    function graphUnary(op, a) {
        const out = alloc("float32", a.data.length), A = a.data, O = out.data, n = O.length;
        switch (op) {
            case "relu": for (let i = 0; i < n; i++) { const x = A[i]; O[i] = (x > 0 || x !== x) ? x : 0; } break;
            case "positive": for (let i = 0; i < n; i++) O[i] = A[i] > 0 ? 1 : 0; break;
            case "neg": for (let i = 0; i < n; i++) O[i] = -A[i]; break;
            case "exp": for (let i = 0; i < n; i++) O[i] = Math.exp(A[i]); break;
            case "log": for (let i = 0; i < n; i++) O[i] = Math.log(A[i]); break;
            case "sqrt": for (let i = 0; i < n; i++) O[i] = Math.sqrt(A[i]); break;
            case "tanh": for (let i = 0; i < n; i++) O[i] = Math.tanh(A[i]); break;
            case "sigmoid": for (let i = 0; i < n; i++) { const x = A[i]; if (x >= 0) O[i] = 1 / (1 + Math.exp(-x)); else { const e = Math.exp(x); O[i] = e / (1 + e); } } break;
            case "gelu": for (let i = 0; i < n; i++) { const x = A[i]; O[i] = x * cdf(x); } break;
            case "gelu_grad": for (let i = 0; i < n; i++) O[i] = geluGrad(A[i]); break;
            default: fail(E.ValueError, "unknown graph op " + op);
        }
        return out;
    }
    // Row maximum, NaN winning, and the float32 sum of exp(x - max) in index order.
    function rowMax(A, base, cols) {
        let m = A[base];
        for (let j = 1; j < cols; j++) { const v = A[base + j]; if (v > m || v !== v) m = v; }
        return m;
    }
    function rowExpSum(A, base, cols, m) {
        let s = 0;
        for (let j = 0; j < cols; j++) s = f32(s + f32(Math.exp(f32(A[base + j] - m))));
        return s;
    }
    function graphSoftmax(a, rows, cols, log) {
        const out = alloc("float32", a.data.length), A = a.data, O = out.data;
        for (let r = 0, base = 0; r < rows; r++, base += cols) {
            const m = rowMax(A, base, cols), s = rowExpSum(A, base, cols, m);
            if (log) { const ls = f32(Math.log(s)); for (let j = base, end = base + cols; j < end; j++) O[j] = f32(A[j] - m) - ls; }
            else for (let j = base, end = base + cols; j < end; j++) O[j] = f32(Math.exp(f32(A[j] - m))) / s;
        }
        return out;
    }
    // Mean cross-entropy of logits [rows, cols] against class targets (a
    // float32 storage of integers), or its gradient (softmax - onehot) / rows.
    function graphCrossEntropy(a, t, rows, cols, grad) {
        const A = a.data, T = t.data;
        if (!grad) {
            const losses = new Float32Array(rows);
            for (let r = 0, base = 0; r < rows; r++, base += cols) {
                const m = rowMax(A, base, cols), s = rowExpSum(A, base, cols, m);
                losses[r] = f32(Math.log(s)) - f32(A[base + T[r]] - m);
            }
            const out = alloc("float32", 1);
            out.data[0] = pairwise(losses) / rows;
            return out;
        }
        const out = alloc("float32", A.length), O = out.data;
        for (let r = 0, base = 0; r < rows; r++, base += cols) {
            const m = rowMax(A, base, cols), s = rowExpSum(A, base, cols, m), target = base + T[r];
            for (let j = base, end = base + cols; j < end; j++) O[j] = f32(f32(f32(Math.exp(f32(A[j] - m))) / s) - (j === target ? 1 : 0)) / rows;
        }
        return out;
    }
    // One optimizer step over equal-sized storages; `s` holds the step's
    // float32 scalars, rounded by the caller as `_optimizer_step` rounds them.
    function graphStep(op, a, b, c, s) {
        const out = alloc("float32", a.data.length), A = a.data, B = b.data, O = out.data, n = O.length;
        switch (op) {
            case "sgd_update": { const lr = s[0]; for (let i = 0; i < n; i++) O[i] = A[i] - f32(lr * B[i]); break; }
            case "momentum_update": { const mu = s[0], w = s[1]; for (let i = 0; i < n; i++) O[i] = f32(mu * A[i]) + f32(w * B[i]); break; }
            case "adam_m": {
                // torch.lerp(m, grad, 1 - beta1), in the branch PyTorch takes for that weight.
                const w = s[0], v1 = s[1];
                if (w < 0.5) for (let i = 0; i < n; i++) O[i] = A[i] + f32(w * f32(B[i] - A[i]));
                else for (let i = 0; i < n; i++) O[i] = B[i] - f32(f32(B[i] - A[i]) * v1);
                break;
            }
            case "adam_v": { const beta = s[0], w = s[1]; for (let i = 0; i < n; i++) O[i] = f32(A[i] * beta) + f32(f32(w * B[i]) * B[i]); break; }
            case "adam_update": {
                // p - size * m / (sqrt(v) / bc + eps)
                const C = c.data, size = s[0], bc = s[1], eps = s[2];
                for (let i = 0; i < n; i++) O[i] = A[i] - f32(size * f32(B[i] / f32(f32(f32(Math.sqrt(C[i])) / bc) + eps)));
                break;
            }
            default: fail(E.ValueError, "unknown optimizer step " + op);
        }
        return out;
    }
    function life(a, h, w) {
        const out = alloc("float32", h * w), O = out.data, A = a.data;
        for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
            let count = 0;
            for (let dy = -1; dy <= 1; dy++) for (let dx = -1; dx <= 1; dx++)
                if ((dx !== 0 || dy !== 0) && A[((y + dy + h) % h) * w + (x + dx + w) % w] > 0.5) count++;
            O[y * w + x] = count === 3 || (A[y * w + x] > 0.5 && count === 2) ? 1 : 0;
        }
        return out;
    }
    // No NaN or infinity: `x - x` is 0 exactly for finite x.
    function allFinite(s) {
        if (!isFloatDtype(s.dtype)) return true;
        const d = s.data, n = d.length;
        if (n >= NATIVE_MIN && NATIVE !== null) { const r = NATIVE(N_ALL_FINITE, d); if (r !== null) return r; }
        for (let i = 0; i < n; i++) { const x = d[i]; if (x - x !== 0) return false; }
        return true;
    }
    // ---- the module --------------------------------------------------------------------------------
    rt.defineModule("_zipp_tensor", (g) => {
        const fn = (name, arity, code, min) => g.set(name, rt.builtin(name, arity, code, min));
        const num = (v) => jsNumber(v);
        fn("zeros", 2, (a) => alloc(rt.needStr(a[0]), num(a[1])));
        fn("full", 3, (a) => { const s = alloc(rt.needStr(a[0]), num(a[1])); s.data.fill(castValue(s.dtype, num(a[2]))); return s; });
        // arange(dtype, start, step, n): element i is castValue(start + i * step),
        // what from_flat gives for the list torch builds (torch.arange checks
        // that the double arithmetic is exact or is Python's float arithmetic).
        fn("arange", 4, (a) => { const s = alloc(rt.needStr(a[0]), num(a[3])), O = s.data, d = s.dtype, st = num(a[1]), step = num(a[2]), n = O.length; for (let i = 0; i < n; i++) O[i] = castValue(d, st + i * step); return s; });
        fn("_set_size_type", 1, (a) => { SIZE = a[0]; return null; });
        // _shape_eq(a, b): tuple equality of two shapes (tuples or Sizes of
        // ints), without the rich-comparison dispatch of a tuple subclass.
        fn("_shape_eq", 2, (a) => {
            const x = a[0], y = a[1];
            if (x === y) return true;
            if (x === null || y === null || typeof x !== "object" || typeof y !== "object"
                || (x.cls !== T.tuple && (SIZE === null || x.cls !== SIZE)) || (y.cls !== T.tuple && (SIZE === null || y.cls !== SIZE))) return rt.eq(x, y);
            const p = x.items, q = y.items;
            if (p.length !== q.length) return false;
            for (let i = 0; i < p.length; i++) {
                const u = p[i], v = q[i];
                if (u === v) continue;
                if (typeof u === "bigint" && typeof v === "bigint") return false;
                if (!rt.eq(u, v)) return false;
            }
            return true;
        });
        // _size(t): torch.Size(t) for a tuple t, as the tuple constructor
        // builds a subclass instance (`rt.allocInstance`, then its items).
        fn("_size", 1, (a) => { const v = a[0]; if (SIZE === null || v === null || typeof v !== "object" || v.cls !== T.tuple) fail(E.TypeError, "_size expects a tuple"); return { cls: SIZE, items: v.items.slice(), dict: new Map() }; });
        fn("from_flat", 2, (a) => { const items = a[1].items, s = alloc(rt.needStr(a[0]), items.length); for (let i = 0; i < items.length; i++) s.data[i] = castValue(s.dtype, jsNumber(items[i])); return s; });
        fn("to_list", 1, (a) => {
            const s = needS(a[0]), d = s.data, out = new Array(d.length);
            if (isFloatDtype(s.dtype)) { for (let i = 0; i < out.length; i++) out[i] = d[i]; }
            else { for (let i = 0; i < out.length; i++) out[i] = pyNumber(s.dtype, d[i]); }
            return list(out);
        });
        fn("item", 2, (a) => { const s = needS(a[0]); return pyNumber(s.dtype, s.data[num(a[1])]); });
        fn("setitem", 3, (a) => { const s = needS(a[0]); s.data[num(a[1])] = castValue(s.dtype, num(a[2])); written(s); return null; });
        fn("copy", 1, (a) => { const s = needS(a[0]); return make(s.dtype, s.data.slice()); });
        fn("astype", 2, (a) => {
            const s = needS(a[0]), d = rt.needStr(a[1]); const out = alloc(d, s.data.length);
            // A float target converts exactly as castValue does (the typed
            // array rounds float32 on store); integer targets truncate.
            if (isFloatDtype(d)) out.data.set(s.data);
            else for (let i = 0; i < out.data.length; i++) out.data[i] = castValue(d, s.data[i]);
            return out;
        });
        fn("dtype", 1, (a) => needS(a[0]).dtype);
        fn("size", 1, (a) => BigInt(needS(a[0]).data.length));
        fn("version", 1, (a) => BigInt(needS(a[0]).version));
        // Autograd's view of the version: writes made through `.data` (which
        // PyTorch gives a version counter of its own) are not counted, the
        // total `version` above still is.
        fn("aversion", 1, (a) => { const s = needS(a[0]); return BigInt(s.version - s.untracked); });
        fn("untrack", 1, (a) => { needS(a[0]).untracked++; return null; });
        fn("all_finite", 1, (a) => allFinite(needS(a[0])));
        // _native(on): switch the native loops (`vm::py_tensor`) on or off,
        // returning whether they were on; for comparing the two paths.
        fn("_native", 1, (a) => { const was = NATIVE !== null; NATIVE = rt.truth(a[0]) ? NATIVE_FN : null; return was; });
        // graph_matmul(a, b, m, k, n, batch=1, a_batch_stride=0, b_batch_stride=0)
        fn("graph_matmul", 8, (a) => graphMatmul(needS(a[0]), needS(a[1]), num(a[2]), num(a[3]), num(a[4]),
            a[5] === undefined ? 1 : num(a[5]), a[6] === undefined ? 0 : num(a[6]), a[7] === undefined ? 0 : num(a[7])), 5);
        // pair_sum(a, mean=False): the pairwise total, divided by the count when `mean`.
        fn("pair_sum", 2, (a) => pairSum(needS(a[0]), a[1] !== undefined && rt.truth(a[1])), 1);
        fn("graph_unary", 2, (a) => graphUnary(rt.needStr(a[0]), needS(a[1])));
        fn("graph_softmax", 4, (a) => graphSoftmax(needS(a[0]), num(a[1]), num(a[2]), rt.truth(a[3])));
        fn("graph_cross_entropy", 5, (a) => graphCrossEntropy(needS(a[0]), needS(a[1]), num(a[2]), num(a[3]), rt.truth(a[4])));
        // graph_step(op, a, b, c_or_None, scalars): one optimizer update.
        fn("graph_step", 5, (a) => { const s = a[4].items, sc = new Array(s.length); for (let i = 0; i < s.length; i++) sc[i] = jsNumber(s[i]); return graphStep(rt.needStr(a[0]), needS(a[1]), needS(a[2]), a[3] === null ? null : needS(a[3]), sc); });
        fn("life", 3, (a) => life(needS(a[0]), num(a[1]), num(a[2])));
        fn("fill", 2, (a) => { const s = needS(a[0]); s.data.fill(castValue(s.dtype, num(a[1]))); written(s); return null; });
        fn("copy_into", 2, (a) => {
            const d = needS(a[0]), s = needS(a[1]); if (d.data.length !== s.data.length) fail(E.RuntimeError, "size mismatch");
            // A float storage of its own dtype holds values castValue leaves
            // unchanged, so a block copy is the same store.
            if (d.dtype === s.dtype && isFloatDtype(d.dtype)) d.data.set(s.data);
            else for (let i = 0; i < d.data.length; i++) d.data[i] = castValue(d.dtype, s.data[i]);
            written(d);
            return null;
        });
        // binary(op, a, ashape, b, bshape, dtype=None): `dtype` is the result type promotion chose.
        fn("binary", 6, (a) => binary(rt.needStr(a[0]), needS(a[1]), shapeOf(a[2]), needS(a[3]), shapeOf(a[4]), a[2], a[4], a[5] === undefined || a[5] === null ? null : rt.needStr(a[5])), 5);
        fn("unary", 4, (a) => unary(rt.needStr(a[0]), needS(a[1]), a[2] === undefined ? null : a[2], a[3] === undefined ? null : a[3]), 2);
        fn("reduce", 6, (a) => reduce(rt.needStr(a[0]), needS(a[1]), shapeOf(a[2]), a[3], rt.truth(a[4]), a[5] !== undefined && rt.truth(a[5])), 5);
        fn("permute", 3, (a) => permute(needS(a[0]), shapeOf(a[1]), a[2]));
        fn("expand", 3, (a) => expand(needS(a[0]), shapeOf(a[1]), a[2]));
        fn("slice", 3, (a) => slice(needS(a[0]), shapeOf(a[1]), a[2]));
        fn("setslice", 5, (a) => setSlice(needS(a[0]), shapeOf(a[1]), a[2], needS(a[3]), shapeOf(a[4])));
        fn("gather", 4, (a) => gather(needS(a[0]), shapeOf(a[1]), a[2], a[3]));
        fn("scatter", 6, (a) => scatter(needS(a[0]), shapeOf(a[1]), a[2], a[3], needS(a[4]), shapeOf(a[5])));
        fn("scatter_add", 6, (a) => scatterAdd(needS(a[0]), shapeOf(a[1]), a[2], a[3], needS(a[4]), shapeOf(a[5])));
        fn("index_select", 4, (a) => indexSelect(needS(a[0]), shapeOf(a[1]), num(a[2]), needS(a[3])));
        fn("cat", 2, (a) => cat(a[0], num(a[1])));
        fn("roll", 4, (a) => roll(needS(a[0]), shapeOf(a[1]), num(a[2]), num(a[3])));
        fn("pad_last", 5, (a) => padLast(needS(a[0]), shapeOf(a[1]), num(a[2]), num(a[3]), num(a[4])));
        fn("matmul", 4, (a) => matmul(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3])));
        fn("conv1d", 5, (a) => conv1d(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3]), a[4] === null ? null : needS(a[4])));
        fn("conv1d_backward", 5, (a) => conv1dBackward(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3]), needS(a[4])));
        fn("conv2d", 9, (a) => conv2d(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3]), a[4] === null ? null : needS(a[4]), shapeOf(a[5]), shapeOf(a[6]), shapeOf(a[7]), num(a[8])));
        fn("conv2d_backward", 9, (a) => conv2d(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3]), null, shapeOf(a[5]), shapeOf(a[6]), shapeOf(a[7]), num(a[8]), needS(a[4])));
        fn("softmax", 4, (a) => softmax(needS(a[0]), shapeOf(a[1]), num(a[2]), rt.truth(a[3])));
        fn("argsort", 4, (a) => argsort(needS(a[0]), shapeOf(a[1]), num(a[2]), rt.truth(a[3])));
        fn("one_hot", 2, (a) => oneHot(needS(a[0]), num(a[1])));
        fn("where", 7, (a) => where(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3]), needS(a[4]), shapeOf(a[5]), a[6] === undefined || a[6] === null ? null : rt.needStr(a[6])), 6);
        fn("scan", 4, (a) => scan(rt.needStr(a[0]), needS(a[1]), shapeOf(a[2]), num(a[3])));
        fn("strided", 4, (a) => strided(needS(a[0]), shapeOf(a[1]), shapeOf(a[2]), num(a[3])));
        fn("gen_get_state", 1, (a) => genGetState(a[0]));
        fn("gen_set_state", 2, (a) => genSetState(a[0], a[1]));
        fn("allclose", 4, (a) => { const x = needS(a[0]), y = needS(a[1]); const rtol = num(a[2]), atol = num(a[3]); if (x.data.length !== y.data.length) return false; for (let i = 0; i < x.data.length; i++) { const p = x.data[i], q = y.data[i]; if (p === q) continue; if (!Number.isFinite(p) || !Number.isFinite(q) || Math.abs(p - q) > atol + rtol * Math.abs(q)) return false; } return true; });
        fn("equal", 2, (a) => { const x = needS(a[0]), y = needS(a[1]); if (x.data.length !== y.data.length) return false; for (let i = 0; i < x.data.length; i++) if (x.data[i] !== y.data[i]) return false; return true; });
        fn("gen", 1, (a) => genNew(rt.asInt(rt.needInt(a[0]))));
        fn("gen_seed", 2, (a) => { const g = a[0]; g.seed = rt.asInt(rt.needInt(a[1])); g.state = mt(Number(BigInt.asUintN(32, g.seed))); return null; });
        fn("gen_initial_seed", 1, (a) => a[0].seed);
        fn("rand", 2, (a) => rand(a[0], num(a[1])));
        fn("rand_double", 2, (a) => randDouble(a[0], num(a[1])));
        fn("randn", 3, (a) => randn(a[0], num(a[1]), rt.needStr(a[2])));
        fn("randint", 4, (a) => randint(a[0], num(a[1]), num(a[2]), num(a[3])));
        fn("multinomial", 5, (a) => multinomial(a[0], needS(a[1]), shapeOf(a[2]), num(a[3]), rt.truth(a[4])));
        fn("randperm", 2, (a) => randperm(a[0], num(a[1])));
        fn("random64", 2, (a) => random64(a[0], num(a[1])));
        fn("tobytes", 1, (a) => toBytes(needS(a[0])));
        fn("frombytes", 3, (a) => fromBytes(rt.needStr(a[0]), a[1], a[2] === undefined || a[2] === null ? null : num(a[2])), 2);
        // zlib's CRC-32 of a bytes object, for zipfile (torch.save checkpoints).
        fn("crc32", 2, (a) => BigInt(crc32(a[0].items, a[1] === undefined ? 0 : num(a[1]))), 1);
        fn("dot_sum", 2, (a) => { const x = needS(a[0]).data, y = needS(a[1]).data; let s = 0; for (let i = 0; i < x.length; i++) s += x[i] * y[i]; return s; });
        fn("axpy", 3, (a) => { const alpha = num(a[0]), x = needS(a[1]).data, y = needS(a[2]).data; for (let i = 0; i < y.length; i++) y[i] = castValue(a[2].dtype, y[i] + alpha * x[i]); written(a[2]); return null; });
        g.set("Storage", Storage); g.set("Generator", Gen);
    });
})(__zipp_py);
