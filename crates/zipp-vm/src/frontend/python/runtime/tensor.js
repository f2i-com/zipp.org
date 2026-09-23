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
        N_REDUCE = 8, N_SOFTMAX = 9, N_GATHER = 10, N_ALL_FINITE = 11, N_WHERE = 12, N_MATMUL_NT = 13, N_MAX_POOL2D = 14, N_MAX_POOL2D_BACKWARD = 15, N_FFT = 16, N_LINALG = 17, N_SPMM = 18, N_SP_COALESCE = 19, N_SP_KEYS = 20, N_SP_SCATTER = 21, N_SP_MERGE = 22, N_INDEX_SELECT = 23;
    const BIN_CODE = { add: 1, sub: 2, mul: 3, div: 4, pow: 5, max: 6, min: 7, eq: 8, ne: 9, lt: 10, le: 11, gt: 12, ge: 13,
        and: 14, or: 15, xor: 16, floordiv: 17, mod: 18, atan2: 19 };
    const UN_CODE = { neg: 1, relu: 2, exp: 3, log: 4, tanh: 5, sigmoid: 6, sqrt: 7, square: 8, abs: 9, sign: 10, silu: 11,
        gelu: 12, gelu_grad: 13, reciprocal: 14, rsqrt: 15, log1p: 16, expm1: 17, softplus: 18, sin: 19, cos: 20, floor: 21,
        ceil: 22, trunc: 23, isfinite: 24, isnan: 25, not: 26, clamp: 27, frac: 28, tan: 29, atan: 30, log2: 31, log10: 32,
        isinf: 33, exp2: 34, sinh: 35, cosh: 36, asin: 37, acos: 38, asinh: 39, acosh: 40, atanh: 41 };
    const RED_CODE = { sum: 1, mean: 2, prod: 3, max: 4, min: 5, argmax: 6, argmin: 7, all: 8, any: 9 };
    // Two bytes per element for the reduced-precision floats: float16 in a
    // Float16Array (a read gives the value), bfloat16 in a Uint16Array of
    // the upper 16 bits of the float32 (`bfValue` reads one, `bfBits`
    // rounds a float32 to one; `wide` decodes a whole storage). A kernel
    // that computes a float16/bfloat16 result writes float32 values into a
    // Float32Array (`work`) and `finish` rounds them to the format once, as
    // it always has: the Float16Array's own double -> half store would round
    // once where PyTorch's float opmath rounds twice. Copies (permute,
    // slice, gather, ...) move the 2-byte elements as they are. int8/int16
    // wrap on store as uint8 does.
    //
    //
    // complex64/complex128 storages hold interleaved (real, imaginary)
    // pairs in a Float32Array/Float64Array of twice the element count
    // (PyTorch's ComplexFloat/ComplexDouble layout), as objects of their
    // own class (`CStorage`, made by `cmake`/`calloc`). `PAIR` names each
    // one's real dtype. Only the kernels that know the layout accept them
    // (`needSC`); every other kernel's `needS` refuses one, so a complex
    // storage can never be read as twice as many reals by mistake, and a
    // real kernel call pays nothing for them. The copying kernels
    // (permute, slice, cat, gather, ...) run on the same array seen as a
    // real storage with a trailing dimension of 2 (`realOf`), which is
    // exactly the complex layout.
    const ARRAY = { float32: Float32Array, float64: Float64Array, int64: Float64Array, int32: Float64Array, bool: Uint8Array, uint8: Uint8Array,
        float16: Float16Array, bfloat16: Uint16Array, int8: Int8Array, int16: Int16Array };
    const CARRAY = { complex64: Float32Array, complex128: Float64Array };
    const RANK = { bool: 0, uint8: 1, int8: 1, int16: 2, int32: 3, int64: 4, float16: 5, bfloat16: 5, float32: 6, float64: 7, complex64: 8, complex128: 9 };
    const FLOAT = { float16: 1, bfloat16: 1, float32: 1, float64: 1 };
    const PAIR = { complex64: "float32", complex128: "float64" };
    const COMPLEX_OF = { float32: "complex64", float64: "complex128" };
    function make(dtype, data) { if (ARRAY[dtype] === undefined) fail(E.TypeError, "unknown dtype " + dtype); return { cls: Storage, dtype: dtype, data: data, version: 0, untracked: 0 }; }
    function alloc(dtype, n) { return make(dtype, new ARRAY[dtype](n)); }
    // A result storage a kernel computes into: a float32 scratch for
    // float16/bfloat16 (rounded and packed by `finish`), else `alloc`.
    function work(dtype, n) { return make(dtype, dtype === "float16" || dtype === "bfloat16" ? new Float32Array(n) : new ARRAY[dtype](n)); }
    // Dtypes whose storages can alias one another's memory (`view_dtype`).
    const VIEW_GROUP = { float16: 2, bfloat16: 2, int16: 2, uint8: 1, int8: 1 };
    function isStorage(v) { return v !== null && typeof v === "object" && v.cls === Storage; }
    const CStorage = rt.newType("_ComplexStorage", [rt.ObjectType], new Map(), "_zipp_tensor");
    function isCStorage(v) { return v !== null && typeof v === "object" && v.cls === CStorage; }
    function cmake(dtype, data) { if (CARRAY[dtype] === undefined) fail(E.TypeError, "unknown complex dtype " + dtype); return { cls: CStorage, dtype: dtype, data: data, version: 0, untracked: 0 }; }
    function calloc(dtype, n) { return cmake(dtype, new CARRAY[dtype](2 * n)); }
    // A storage of any dtype, complex included (the creation kernels): the
    // complex dtype names are the only ones longer than 8 characters.
    function anyAlloc(dtype, n) { return dtype.length > 8 ? calloc(dtype, n) : alloc(dtype, n); }
    // A storage argument a kernel's complex path takes (a complex storage,
    // or a real one mixed with a complex operand).
    function needC(v) { if (!isCStorage(v)) needS(v); return v; }
    function needS(v, what) {
        if (!isStorage(v)) {
            if (isCStorage(v)) fail(E.RuntimeError, "this operation does not support complex tensors on Zipp");
            fail(E.TypeError, (what || "argument") + " must be a tensor storage");
        }
        return v;
    }
    // A storage argument of a kernel that handles complex storages too.
    function needSC(v, what) { if (!isStorage(v) && !isCStorage(v)) fail(E.TypeError, (what || "argument") + " must be a tensor storage"); return v; }
    // A view storage (`as_real`/`as_complex`) shares its base's memory and
    // version counter: writes through either are one history.
    function root(s) { return s.base === undefined ? s : s.base; }
    function written(s) { if (s.base === undefined) s.version++; else s.base.version++; }
    // The elements of a storage (complex pairs count once).
    function count(s) { return s.cls === CStorage ? s.data.length >> 1 : s.data.length; }
    // A complex storage's memory seen as its real dtype (trailing dim 2).
    function realOf(s) { return make(PAIR[s.dtype], s.data); }
    // A fresh real result of a kernel run on `realOf` storages, as `dtype`.
    function asPair(r, dtype) { return cmake(dtype, r.data); }
    // A (storage, Size) kernel result over shape + [2], as the complex
    // storage and the shape without the trailing 2.
    function lowered(res, dtype) {
        const items = res.items, sh = shapeOf(items[1]);
        sh.pop();
        return tuple([asPair(items[0], dtype), pyShape(sh)]);
    }
    // `s` converted to the complex dtype `d` (a real storage gets zero
    // imaginary parts); `s` itself when it already is one.
    function toPair(s, d) {
        if (s.dtype === d) return s;
        const n = count(s), out = calloc(d, n), O = out.data;
        if (s.cls === CStorage) { O.set(s.data); return out; }
        const S = vals(s);
        for (let i = 0; i < n; i++) O[2 * i] = S[i];
        return out;
    }
    function pyInts(arr) { const out = new Array(arr.length); for (let i = 0; i < arr.length; i++) out[i] = BigInt(arr[i]); return tuple(out); }
    // The host transport (entry.js): a float32 storage leaves as its
    // Float32Array, and a Float32Array the host sends arrives as a storage
    // that owns it (the VM made it from the host's copy).
    rt.float32Storage = (data) => make("float32", data);
    rt.isFloat32Storage = (v) => isStorage(v) && v.dtype === "float32";
    function isFloatDtype(dtype) { return FLOAT[dtype] === 1; }
    // Rounding a float32 value to float16 (Math.f16round of a float32 is the
    // second step of PyTorch's double -> float -> half conversion) or to
    // bfloat16 (round to nearest even on the upper 16 bits; NaN stays NaN,
    // overflow gives infinity).
    const BF_F = new Float32Array(1), BF_U = new Uint32Array(BF_F.buffer);
    function bf16(x) {
        if (x !== x) return x;
        BF_F[0] = x;
        const u = BF_U[0];
        BF_U[0] = (u + 0x7fff + ((u >>> 16) & 1)) & 0xffff0000;
        return BF_F[0];
    }
    const f16 = Math.f16round;
    const HALF = { float16: 1, bfloat16: 1 };
    // The bfloat16 bits of float32 value x: round to nearest even on the
    // upper half (bf16's rounding); NaN as 0x7fc0 (a JavaScript NaN is the
    // one canonical NaN).
    function bfBits(x) {
        if (x !== x) return 0x7fc0;
        BF_F[0] = x;
        const u = BF_U[0];
        return ((u + 0x7fff + ((u >>> 16) & 1)) >>> 16) & 0xffff;
    }
    function bfValue(bits) { BF_U[0] = bits << 16; return BF_F[0]; }
    // A bfloat16 storage's values, as a Float32Array (exact).
    function wide(s) {
        const U = s.data, n = U.length, w = new Uint32Array(n);
        for (let i = 0; i < n; i++) w[i] = U[i] << 16;
        return new Float32Array(w.buffer);
    }
    // A storage's elements as numbers a kernel can read: the typed array
    // itself, except that bfloat16 is decoded.
    function vals(s) { return s.dtype === "bfloat16" ? wide(s) : s.data; }
    // Finish a float16/bfloat16 result computed into a `work` scratch:
    // round each float32 to the format (a Float16Array store of a float32
    // value is Math.f16round of it) into the 2-byte storage. A NaN is the
    // canonical one whichever loop (native or JavaScript) produced it. A
    // storage that is already packed, or of another dtype, is left as it is.
    function finish(s) {
        const d = s.dtype, O = s.data;
        if (d === "float16") {
            if (O instanceof Float32Array) { const n = O.length, h = new Float16Array(n); for (let i = 0; i < n; i++) h[i] = O[i]; s.data = h; }
        } else if (d === "bfloat16" && O instanceof Float32Array) {
            const n = O.length, u = new Uint32Array(O.buffer, O.byteOffset, n), h = new Uint16Array(n);
            for (let i = 0; i < n; i++) {
                const w = u[i];
                h[i] = (w & 0x7fffffff) > 0x7f800000 ? 0x7fc0 : ((w + 0x7fff + ((w >>> 16) & 1)) >>> 16) & 0xffff;
            }
            s.data = h;
        }
        return s;
    }
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
    // PyTorch's promote_types: the higher rank wins, except uint8 with
    // int8 (int16) and float16 with bfloat16 (float32).
    function promote(a, b) {
        if (a === b) return a;
        const ra = RANK[a], rb = RANK[b];
        // A complex dtype wins, at double precision when either side is
        // (complex64 with float64 is complex128).
        if (ra >= 8 || rb >= 8) return a === "complex128" || b === "complex128" || a === "float64" || b === "float64" ? "complex128" : "complex64";
        if (ra === rb) return ra === 1 ? "int16" : "float32";
        return ra > rb ? a : b;
    }
    function pyNumber(dtype, v) {
        if (dtype === "bool") return v !== 0;
        if (!FLOAT[dtype]) return BigInt(Math.trunc(v));
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
        if (dtype === "float16") return f16(Math.fround(v));
        if (dtype === "bfloat16") return bf16(Math.fround(v));
        if (dtype === "int8" || dtype === "int16") return Math.trunc(v);
        return v;
    }
    // What a storage of `dtype` holds for the number v: castValue's value,
    // or for bfloat16 its bits.
    function enc(dtype, v) { return dtype === "bfloat16" ? bfBits(Math.fround(v)) : castValue(dtype, v); }
    // enc as a function of (dtype, v) for a per-element loop: castValue
    // itself unless the storage is bfloat16, so other dtypes pay no extra call.
    function encBf(dtype, v) { return bfBits(Math.fround(v)); }
    function encoder(dtype) { return dtype === "bfloat16" ? encBf : castValue; }
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
        xlogy: (x, y) => xlogy(x, y), xlog1py: (x, y) => xlog1py(x, y), zeta: (x, y) => zeta(x, y), igamma: (x, y) => igamma(x, y), igammac: (x, y) => igammac(x, y),
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
        if ((op === "div" || op === "atan2") && !isFloatDtype(dtype)) dtype = "float32";
        // An integer result: powers stay integral (the other arithmetic is
        // exact on integers already).
        if (op === "pow" && !isFloatDtype(dtype)) op = "ipow";
        const f = BIN[op]; if (f === undefined) fail(E.ValueError, "unknown op " + op);
        if (dtype === "bool" && !COMPARE.has(op)) return binaryBool(f, vals(a), ashape, vals(b), bshape, shape);
        const n = numel(shape), out = work(dtype, n), A = a.dtype === "bfloat16" ? wide(a) : a.data, Bd = b.dtype === "bfloat16" ? wide(b) : b.data, O = out.data;
        // Every layout below visits elements in the same order and applies
        // the same double-precision operation as the closure form, and the
        // typed array rounds on store, so results are identical.
        const na = numel(ashape), nb = numel(bshape);
        // Same rank and (nonzero) size as an operand means the same shape.
        const outShape = n !== 0 && na === n && ashape.length === shape.length && tupleShape(apy) ? apy
            : n !== 0 && nb === n && bshape.length === shape.length && tupleShape(bpy) ? bpy : null;
        if (n >= NATIVE_MIN && NATIVE !== null && BIN_CODE[op] !== undefined
            && NATIVE(N_BINARY, BIN_CODE[op], A, Bd, O, shape, bstrides(ashape, shape), bstrides(bshape, shape)))
            { if (HALF[out.dtype] === 1) finish(out); return tuple([out, outShape === null ? pyShape(shape) : outShape]); }
        if (na === n && ashape.length === shape.length) {
            if (nb === 1 ? binaryScalar(op, O, A, Bd[0], false) : (nb === n && bshape.length === shape.length ? binaryTile(op, O, A, Bd, n, false) : (suffixTile(bshape, shape) === nb && binaryTile(op, O, A, Bd, nb, false))))
                { if (HALF[out.dtype] === 1) finish(out); return tuple([out, outShape === null ? pyShape(shape) : outShape]); }
        } else if (nb === n && bshape.length === shape.length) {
            if (na === 1 ? binaryScalar(op, O, Bd, A[0], true) : (suffixTile(ashape, shape) === na && binaryTile(op, O, Bd, A, na, true)))
                { if (HALF[out.dtype] === 1) finish(out); return tuple([out, outShape === null ? pyShape(shape) : outShape]); }
        }
        if (ashape.length === shape.length && bshape.length === shape.length && na === n && nb === n) {
            for (let i = 0; i < n; i++) O[i] = f(A[i], Bd[i]);
        } else if (nb === 1 && ashape.length === shape.length && na === n) {
            const y = Bd[0]; for (let i = 0; i < n; i++) O[i] = f(A[i], y);
        } else {
            forEachBroadcast(shape, bstrides(ashape, shape), bstrides(bshape, shape), (o, x, y) => { O[o] = f(A[x], Bd[y]); });
        }
        { if (HALF[out.dtype] === 1) finish(out); return tuple([out, outShape === null ? pyShape(shape) : outShape]); }
    }
    // Arithmetic with a bool result: computed like the others, then any
    // nonzero stores as 1 (True + True is True).
    function binaryBool(f, A, ashape, Bd, bshape, shape) {
        const out = alloc("bool", numel(shape)), O = out.data;
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
        lgamma: (x) => lgamma(x), digamma: (x) => digamma(x), erfcx: (x) => erfcx(x), i0: (x) => i0(x), i0e: (x) => i0e(x),
        i1: (x) => i1(x), i1e: (x) => i1e(x), ndtr: (x) => ndtr(x), ndtri: (x) => ndtri(x), log_ndtr: (x) => logNdtr(x), entr: (x) => entr(x),
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
    // ---- special functions (torch.special), in double precision --------------------------
    // The algorithms PyTorch's CPU kernels use (Cephes, as in ATen's
    // Math.h), so values agree to double rounding.
    const LOG_SQRT_2PI = 0.9189385332046727, SQRT_2PI = 2.5066282746310002, INV_SQRT_PI = 0.5641895835477563;
    function polevl(x, c) { let r = c[0]; for (let i = 1; i < c.length; i++) r = r * x + c[i]; return r; }
    function p1evl(x, c) { let r = x + c[0]; for (let i = 1; i < c.length; i++) r = r * x + c[i]; return r; }
    // Lanczos (g = 7, n = 9) below 10, Stirling's series above; the
    // reflection formula below 0.5.
    const LANCZOS = [0.99999999999980993, 676.5203681218851, -1259.1392167224028, 771.32342877765313, -176.61502916214059,
        12.507343278686905, -0.13857109526572012, 9.9843695780195716e-6, 1.5056327351493116e-7];
    function lgamma(x) {
        if (x !== x) return NaN;
        if (x === Infinity || x === -Infinity) return Infinity;
        if (x <= 0 && x === Math.floor(x)) return Infinity;
        if (x < 0.5) return Math.log(Math.PI / Math.abs(Math.sin(Math.PI * x))) - lgamma(1 - x);
        if (x === 1 || x === 2) return 0;
        if (x >= 10) {
            const z = 1 / (x * x);
            return (x - 0.5) * Math.log(x) - x + LOG_SQRT_2PI + (1 / 12 - z * (1 / 360 - z * (1 / 1260 - z * (1 / 1680 - z / 1188)))) / x;
        }
        const y = x - 1;
        let a = LANCZOS[0];
        const t = y + 7.5;
        for (let i = 1; i < 9; i++) a += LANCZOS[i] / (y + i);
        return LOG_SQRT_2PI + (y + 0.5) * Math.log(t) - t + Math.log(a);
    }
    // ATen's calc_digamma: reflection for negative x (NaN at the negative
    // integers, -inf/+inf at -0/+0), the recurrence up to 10, then the
    // asymptotic series.
    function digamma(x) {
        if (x === 0) return Object.is(x, -0) ? Infinity : -Infinity;
        if (x !== x) return NaN;
        if (x < 0) {
            if (x === Math.trunc(x)) return NaN;
            if (x === -Infinity) return NaN;
            const r = x - Math.trunc(x);
            return digamma(1 - x) - Math.PI / Math.tan(Math.PI * r);
        }
        if (x === Infinity) return Infinity;
        let result = 0;
        while (x < 10) { result -= 1 / x; x += 1; }
        if (x === 10) return result + 2.25175258906672110764;
        let y = 0;
        if (x < 1e17) {
            const z = 1 / (x * x);
            y = z * polevl(z, [8.33333333333333333333E-2, -2.10927960927960927961E-2, 7.57575757575757575758E-3,
                -4.16666666666666666667E-3, 3.96825396825396825397E-3, -8.33333333333333333333E-3, 8.33333333333333333333E-2]);
        }
        return result + Math.log(x) - 0.5 / x - y;
    }
    // ATen's calc_trigamma.
    function trigamma(x) {
        let sign = 1, result = 0;
        if (x < 0.5) {
            sign = -1;
            const s = Math.sin(Math.PI * x);
            result -= (Math.PI * Math.PI) / (s * s);
            x = 1 - x;
        }
        for (let i = 0; i < 6; i++) { result += 1 / (x * x); x += 1; }
        const ixx = 1 / (x * x);
        result += (1 + 1 / (2 * x) + ixx * (1 / 6 - ixx * (1 / 30 - ixx * (1 / 42)))) / x;
        return sign * result;
    }
    // The Hurwitz zeta function zeta(x, q) (Cephes, as ATen's zeta).
    const ZETA_A = [12.0, -720.0, 30240.0, -1209600.0, 47900160.0, -1.8924375803183791606e9, 7.47242496e10,
        -2.950130727918164224e12, 1.1646782814350067249e14, -4.5979787224074726105e15, 1.8152105401943546773e17, -7.1661652561756670113e18];
    function zeta(x, q) {
        const MACHEP = 1.11022302462515654042E-16;
        if (x === 1) return Infinity;
        if (x < 1) return NaN;
        if (q <= 0) {
            if (q === Math.floor(q)) return Infinity;
            if (x !== Math.floor(x)) return NaN;
        }
        let s = Math.pow(q, -x), a = q, i = 0, b = 0;
        while (i < 9 || a <= 9) {
            i += 1; a += 1; b = Math.pow(a, -x); s += b;
            if (Math.abs(b / s) < MACHEP) return s;
        }
        const w = a;
        s += b * w / (x - 1);
        s -= 0.5 * b;
        let aa = 1, k = 0;
        for (let j = 0; j < 12; j++) {
            aa *= x + k; b /= w;
            let t = aa * b / ZETA_A[j];
            s += t;
            t = Math.abs(t / s);
            if (t < MACHEP) return s;
            k += 1; aa *= x + k; b /= w; k += 1;
        }
        return s;
    }
    function polygamma(n, x) {
        if (n === 0) return digamma(x);
        if (n === 1) return trigamma(x);
        return ((n % 2) ? 1 : -1) * Math.exp(lgamma(n + 1)) * zeta(n + 1, x);
    }
    // erfcx(x) = exp(x^2) erfc(x): erfc's continued fraction without its
    // exp(-x^2) factor from 3 up, exp(x^2) erfc(x) below, and
    // 2 exp(x^2) - erfcx(-x) for negative x.
    function erfcx(x) {
        if (x !== x) return NaN;
        if (x < 0) return x < -26.7 ? Infinity : 2 * Math.exp(x * x) - erfcx(-x);
        if (x < 3) return Math.exp(x * x) * erfc(x);
        if (x > 1e150) return INV_SQRT_PI / x;
        const tiny = 1e-300;
        let f = x, C = x, D = 0;
        for (let n = 1; n < 300; n++) {
            const an = n / 2;
            D = x + an * D; if (D === 0) D = tiny; D = 1 / D;
            C = x + an / C; if (C === 0) C = tiny;
            const delta = C * D; f *= delta;
            if (Math.abs(delta - 1) < 1e-16) break;
        }
        return INV_SQRT_PI / f;
    }
    function ndtr(x) {
        if (x !== x) return NaN;
        const t = x * 0.7071067811865476, z = Math.abs(t);
        if (z < 0.7071067811865476) return 0.5 + 0.5 * erf(t);
        const y = 0.5 * erfc(z);
        return t > 0 ? 1 - y : y;
    }
    function logNdtr(x) {
        const t = x * 0.7071067811865476;
        if (x < -1) return Math.log(erfcx(-t) / 2) - t * t;
        return Math.log1p(-erfc(t) / 2);
    }
    // Cephes ndtri.
    const NDTRI_P0 = [-5.99633501014107895267E1, 9.80010754185999661536E1, -5.66762857469070293439E1, 1.39312609387279679503E1, -1.23916583867381258016E0];
    const NDTRI_Q0 = [1.95448858338141759834E0, 4.67627912898881538453E0, 8.63602421390890590575E1, -2.25462687854119370527E2, 2.00260212380060660359E2,
        -8.20372256168333339912E1, 1.59056225126211695515E1, -1.18331621121330003142E0];
    const NDTRI_P1 = [4.05544892305962419923E0, 3.15251094599893866154E1, 5.71628192246421288162E1, 4.40805073893200834700E1, 1.46849561928858024014E1,
        2.18663306850790267539E0, -1.40256079171354495875E-1, -3.50424626827848203418E-2, -8.57456785154685413611E-4];
    const NDTRI_Q1 = [1.57799883256466749731E1, 4.53907635128879210584E1, 4.13172038254672030440E1, 1.50425385692907503408E1, 2.50464946208309415979E0,
        -1.42182922854787788574E-1, -3.80806407691578277194E-2, -9.33259480895457427372E-4];
    const NDTRI_P2 = [3.23774891776946035970E0, 6.91522889068984211695E0, 3.93881025292474443415E0, 1.33303460815807542389E0, 2.01485389549179081538E-1,
        1.23716634817820021358E-2, 3.01581553508235416007E-4, 2.65806974686737550832E-6, 6.23974539184983293730E-9];
    const NDTRI_Q2 = [6.02427039364742014255E0, 3.67983563856160859403E0, 1.37702099489081330271E0, 2.16236993594496635890E-1, 1.34204006088543189037E-2,
        3.28014464682127739104E-4, 2.89247864745380683936E-6, 6.79019408009981274425E-9];
    function ndtri(y0) {
        if (y0 !== y0 || y0 < 0 || y0 > 1) return NaN;
        if (y0 === 0) return -Infinity;
        if (y0 === 1) return Infinity;
        const EXP_M2 = 0.13533528323661269189;
        let code = true, y = y0;
        if (y > 1 - EXP_M2) { y = 1 - y; code = false; }
        let x;
        if (y > EXP_M2) {
            y -= 0.5;
            const y2 = y * y;
            x = (y + y * (y2 * polevl(y2, NDTRI_P0) / p1evl(y2, NDTRI_Q0))) * SQRT_2PI;
            return x;
        }
        x = Math.sqrt(-2 * Math.log(y));
        const x0 = x - Math.log(x) / x, z = 1 / x;
        const x1 = x < 8 ? z * polevl(z, NDTRI_P1) / p1evl(z, NDTRI_Q1) : z * polevl(z, NDTRI_P2) / p1evl(z, NDTRI_Q2);
        x = x0 - x1;
        return code ? -x : x;
    }
    // Modified Bessel functions of the first kind (Cephes' Chebyshev
    // expansions, as ATen's calc_i0/i0e/i1/i1e).
    function chbevl(x, c) {
        let b0 = c[0], b1 = 0, b2 = 0;
        for (let i = 1; i < c.length; i++) { b2 = b1; b1 = b0; b0 = x * b1 - b2 + c[i]; }
        return 0.5 * (b0 - b2);
    }
    const I0_A = [-4.41534164647933937950E-18, 3.33079451882223809783E-17, -2.43127984654795469359E-16, 1.71539128555513303061E-15,
        -1.16853328779934516808E-14, 7.67618549860493561688E-14, -4.85644678311192946090E-13, 2.95505266312963983461E-12,
        -1.72682629144155570723E-11, 9.67580903537323691224E-11, -5.18979560163526290666E-10, 2.65982372468238665035E-9,
        -1.30002500998624804212E-8, 6.04699502254191894932E-8, -2.67079385394061173391E-7, 1.11738753912010371815E-6,
        -4.41673835845875056359E-6, 1.64484480707288970893E-5, -5.75419501008210370398E-5, 1.88502885095841655729E-4,
        -5.76375574538582365885E-4, 1.63947561694133579842E-3, -4.32430999505057594430E-3, 1.05464603945949983183E-2,
        -2.37374148058994688156E-2, 4.93052842396707084878E-2, -9.49010970480476444210E-2, 1.71620901522208775349E-1,
        -3.04682672343198398683E-1, 6.76795274409476084995E-1];
    const I0_B = [-7.23318048787475395456E-18, -4.83050448594418207126E-18, 4.46562142029675999901E-17, 3.46122286769746109310E-17,
        -2.82762398051658348494E-16, -3.42548561967721913462E-16, 1.77256013305652638360E-15, 3.81168066935262242075E-15,
        -9.55484669882830764870E-15, -4.15056934728722208663E-14, 1.54008621752140982691E-14, 3.85277838274214270114E-13,
        7.18012445138366623367E-13, -1.79417853150680611778E-12, -1.32158118404477131188E-11, -3.14991652796324136454E-11,
        1.18891471078464383424E-11, 4.94060238822496958910E-10, 3.39623202570838634515E-9, 2.26666899049817806459E-8,
        2.04891858946906374183E-7, 2.89137052083475648297E-6, 6.88975834691682398426E-5, 3.36911647825569408990E-3,
        8.04490411014108831608E-1];
    const I1_A = [2.77791411276104639959E-18, -2.11142121435816608115E-17, 1.55363195773620046921E-16, -1.10559694773538630805E-15,
        7.60068429473540693410E-15, -5.04218550472791168711E-14, 3.22379336594557470981E-13, -1.98397439776494371520E-12,
        1.17361862988909016308E-11, -6.66348972350202774223E-11, 3.62559028155211703701E-10, -1.88724975172282928790E-9,
        9.38153738649577178388E-9, -4.44505912879632808065E-8, 2.00329475355213526229E-7, -8.56872026469545474066E-7,
        3.47025130813767847674E-6, -1.32731636560394358279E-5, 4.78156510755005422638E-5, -1.61760815825896745588E-4,
        5.12285956168575772895E-4, -1.51357245063125314899E-3, 4.15642294431288815669E-3, -1.05640848946261981558E-2,
        2.47264490306265168283E-2, -5.29459812080949914269E-2, 1.02643658689847095384E-1, -1.76416518357834055153E-1,
        2.52587186443633654823E-1];
    const I1_B = [7.51729631084210481353E-18, 4.41434832307170791151E-18, -4.65030536848935832153E-17, -3.20952592199342395980E-17,
        2.96262899764595013876E-16, 3.30820231092092828324E-16, -1.88035477551078244854E-15, -3.81440307243700780478E-15,
        1.04202769841288027642E-14, 4.27244001671195135429E-14, -2.10154184277266431302E-14, -4.08355111109219731823E-13,
        -7.19855177624590851209E-13, 2.03562854414708950722E-12, 1.41258074366137813316E-11, 3.25260358301548823856E-11,
        -1.89749581235054123450E-11, -5.58974346219658380687E-10, -3.83538038596423702205E-9, -2.63146884688951950684E-8,
        -2.51223623787020892529E-7, -3.88256480887769039346E-6, -1.10588938762623716291E-4, -9.76109749136146840777E-3,
        7.78576235018280120474E-1];
    // i0e(x) = exp(-|x|) i0(x); i1e likewise, odd.
    function i0e(x) {
        const a = Math.abs(x);
        if (a <= 8) return chbevl(a / 2 - 2, I0_A);
        return chbevl(32 / a - 2, I0_B) / Math.sqrt(a);
    }
    function i0(x) { return Math.exp(Math.abs(x)) * i0e(x); }
    function i1e(x) {
        const a = Math.abs(x);
        const r = a <= 8 ? chbevl(a / 2 - 2, I1_A) * a : chbevl(32 / a - 2, I1_B) / Math.sqrt(a);
        return x < 0 ? -r : r;
    }
    function i1(x) { return Math.exp(Math.abs(x)) * i1e(x); }
    function entr(x) {
        if (x !== x) return NaN;
        if (x > 0) return -x * Math.log(x);
        if (x === 0) return 0;
        return -Infinity;
    }
    // The regularized incomplete gamma functions P(a, x) (igamma) and
    // Q(a, x) (igammac): the power series below max(a, 1), the continued
    // fraction above (Cephes igam/igamc), with ATen's edge cases.
    function igamFactor(a, x) { return Math.exp(a * Math.log(x) - x - lgamma(a)); }
    function igamSeries(a, x) {
        const ax = igamFactor(a, x);
        if (ax === 0) return 0;
        let r = a, c = 1, sum = 1;
        for (let n = 0; n < 100000; n++) { r += 1; c *= x / r; sum += c; if (c <= 1.11022302462515654042E-16 * sum) break; }
        return sum * ax / a;
    }
    function igamcFraction(a, x) {
        const ax = igamFactor(a, x);
        if (ax === 0) return 0;
        const big = 4.503599627370496e15, biginv = 2.22044604925031308085e-16;
        let y = 1 - a, z = x + y + 1, c = 0, pkm2 = 1, qkm2 = x, pkm1 = x + 1, qkm1 = z * x, ans = pkm1 / qkm1, t;
        for (let n = 0; n < 100000; n++) {
            c += 1; y += 1; z += 2;
            const yc = y * c, pk = pkm1 * z - pkm2 * yc, qk = qkm1 * z - qkm2 * yc;
            if (qk !== 0) { const r = pk / qk; t = Math.abs((ans - r) / r); ans = r; } else t = 1;
            pkm2 = pkm1; pkm1 = pk; qkm2 = qkm1; qkm1 = qk;
            if (Math.abs(pk) > big) { pkm2 *= biginv; pkm1 *= biginv; qkm2 *= biginv; qkm1 *= biginv; }
            if (t <= 1.11022302462515654042E-16) break;
        }
        return ans * ax;
    }
    function igamma(a, x) {
        if (x < 0 || a < 0 || a !== a || x !== x) return NaN;
        if (a === 0) return x > 0 ? 1 : NaN;
        if (x === 0) return 0;
        if (a === Infinity) return x === Infinity ? NaN : 0;
        if (x === Infinity) return 1;
        if (x > 1 && x > a) return 1 - igamcFraction(a, x);
        return igamSeries(a, x);
    }
    function igammac(a, x) {
        if (x < 0 || a < 0 || a !== a || x !== x) return NaN;
        if (a === 0) return x > 0 ? 0 : NaN;
        if (x === 0) return 1;
        if (a === Infinity) return x === Infinity ? NaN : 1;
        if (x === Infinity) return 0;
        if (x < 1 || x < a) return 1 - igamSeries(a, x);
        return igamcFraction(a, x);
    }
    // xlogy(x, y) = x log y, 0 where x is 0 (NaN y stays NaN).
    function xlogy(x, y) { if (y !== y) return NaN; return x === 0 ? 0 : x * Math.log(y); }
    function xlog1py(x, y) { if (y !== y) return NaN; return x === 0 ? 0 : x * Math.log1p(y); }
    // sinc(x) = sin(pi x) / (pi x), 1 at 0. PyTorch forms pi * x in the
    // op's precision (float for float32/float16/bfloat16), which matters
    // for large x: `f` rounds as that product rounds.
    function sinc(x, f) { if (x === 0) return 1; if (x !== x) return NaN; const p = f(f(Math.PI) * x); return Math.sin(p) / p; }
    const same = (x) => x;
    const BOOL_UNARY = new Set(["isfinite", "isnan", "not", "isinf", "isposinf", "isneginf", "signbit"]);
    const FLOAT_UNARY = new Set(["exp", "log", "tanh", "sigmoid", "silu", "sqrt", "reciprocal", "log1p", "expm1", "gelu", "gelu_grad", "softplus", "sin", "cos", "rsqrt",
        "tan", "asin", "acos", "atan", "sinh", "cosh", "asinh", "acosh", "atanh", "log2", "log10", "exp2", "erf", "erfc", "erfinv",
        "lgamma", "digamma", "polygamma", "erfcx", "i0", "i0e", "i1", "i1e", "ndtr", "ndtri", "log_ndtr", "entr", "sinc", "logit"]);
    function unary(op, a, p1, p2) {
        let f = UN[op], lo = 0, hi = 0;
        if (op === "clamp") { lo = p1 === null ? -Infinity : jsNumber(p1); hi = p2 === null ? Infinity : jsNumber(p2); f = (x) => (x < lo ? lo : x > hi ? hi : x); }
        const dtype = BOOL_UNARY.has(op) ? "bool" : (FLOAT_UNARY.has(op) && !isFloatDtype(a.dtype) ? "float32" : a.dtype);
        if (op === "polygamma") { const k = jsNumber(p1); f = (x) => polygamma(k, x); }
        else if (op === "sinc") { const r = dtype === "float64" ? same : Math.fround; f = (x) => sinc(x, r); }
        else if (op === "logit") {
            // log(x / (1 - x)), x first clamped to [eps, 1 - eps] when eps is
            // given; the bounds and 1 - x in the op's precision, as PyTorch.
            const r = dtype === "float64" ? same : Math.fround;
            if (p1 === null) f = (x) => Math.log(x / r(1 - x));
            else { const e = r(jsNumber(p1)), top = r(1 - e); f = (x) => { const c = x < e ? e : x > top ? top : x; return Math.log(c / r(1 - c)); }; }
        }
        if (f === undefined) fail(E.ValueError, "unknown op " + op);
        const out = work(dtype, a.data.length), A = a.dtype === "bfloat16" ? wide(a) : a.data, O = out.data, n = O.length;
        if (n >= NATIVE_MIN && NATIVE !== null && UN_CODE[op] !== undefined && NATIVE(N_UNARY, UN_CODE[op], A, O, lo, hi)) { if (HALF[out.dtype] === 1) finish(out); return out; }
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
        { if (HALF[out.dtype] === 1) finish(out); return out; }
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
        const dtype = argOp ? "int64" : (op === "all" || op === "any") ? "bool" : (op === "mean" && !isFloatDtype(a.dtype)) ? "float32" : a.dtype;
        // A float16/bfloat16 product rounds every partial product to the
        // format, as PyTorch's reduced-precision prod accumulates.
        if (op === "prod" && HALF[dtype] === 1) return halfProd(a, shape, rank, red, keepdim, dtype);
        const out = work(dtype, nOut), A = a.dtype === "bfloat16" ? wide(a) : a.data;
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
                { if (HALF[out.dtype] === 1) finish(out); return tuple([out, pyShape(finalShape)]); }
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
            { if (HALF[out.dtype] === 1) finish(out); return tuple([out, pyShape(finalShape)]); }
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
        { if (HALF[out.dtype] === 1) finish(out); return tuple([out, pyShape(finalShape)]); }
    }
    function halfProd(a, shape, rank, red, keepdim, dtype) {
        const round = dtype === "float16" ? f16 : bf16;
        const isRed = new Array(rank).fill(red === null);
        if (red !== null) for (let i = 0; i < red.length; i++) isRed[red[i]] = true;
        const outShape = [], keptShape = [];
        for (let d = 0; d < rank; d++) { if (isRed[d]) keptShape.push(1); else { outShape.push(shape[d]); keptShape.push(shape[d]); } }
        const out = work(dtype, numel(keptShape)), O = out.data, A = vals(a), outS = strides(keptShape), nIn = numel(shape);
        O.fill(1);
        const pos = new Array(rank).fill(0);
        let oo = 0;
        for (let flat = 0; flat < nIn; flat++) {
            O[oo] = round(O[oo] * A[flat]);
            for (let d = rank - 1; d >= 0; d--) {
                pos[d]++; if (!isRed[d]) oo += outS[d];
                if (pos[d] < shape[d]) break;
                if (!isRed[d]) oo -= outS[d] * shape[d];
                pos[d] = 0;
            }
        }
        return tuple([finish(out), pyShape(keepdim ? keptShape : outShape)]);
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
        const sv = bstrides(vshape, outShape), A = a.data, V = vals(v), dt = a.dtype;
        const cv = encoder(dt);
        forEachBroadcast(outShape, sa, sv, (o, x, y) => { A[base + x] = cv(dt, V[y]); });
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
        const sv = bstrides(vshape, target), A = a.data, V = vals(v), dt = a.dtype, cv = encoder(dt);
        const I = idx.items.map((s) => s.data);
        // Value offset for [i, r]: walk with forEachBroadcast over the target shape.
        const restS = strides(rest);
        let flat = 0;
        const idxRank = ish.length, tRank = target.length;
        forEachBroadcast(target, new Array(tRank).fill(0), sv, (o, x, y) => {
            const i = Math.floor(o / restN), r = o - i * restN;
            let base = 0;
            for (let d = 0; d < k; d++) { let j = I[d][i]; if (j < 0) j += shape[d]; if (j < 0 || j >= shape[d]) fail(E.IndexError, "index out of bounds"); base += j * inS[d]; }
            A[base + r] = cv(dt, V[y]);
        });
        written(a);
        return null;
    }
    // Like scatter, but accumulating: the gradient of a gather.
    function scatterAdd(a, shape, idx, ishape, v, vshape) {
        const k = idx.items.length, inS = strides(shape), ish = ints(ishape), rest = shape.slice(k);
        const restN = numel(rest), target = ish.concat(rest);
        const sv = bstrides(vshape, target), A = a.data, V = vals(v), dt = a.dtype, bf = dt === "bfloat16";
        const I = idx.items.map((s) => s.data);
        forEachBroadcast(target, new Array(target.length).fill(0), sv, (o, x, y) => {
            const i = Math.floor(o / restN), r = o - i * restN;
            let base = 0;
            for (let d = 0; d < k; d++) { let j = I[d][i]; if (j < 0) j += shape[d]; if (j < 0 || j >= shape[d]) fail(E.IndexError, "index out of bounds"); base += j * inS[d]; }
            A[base + r] = bf ? bfBits(Math.fround(bfValue(A[base + r]) + V[y])) : castValue(dt, A[base + r] + V[y]);
        });
        written(a);
        return null;
    }
    function indexSelect(a, shape, dim, indices) {
        const d = dim < 0 ? dim + shape.length : dim, I = indices.data;
        const outShape = shape.slice(); outShape[d] = I.length;
        const inner = numel(shape.slice(d + 1)), outer = numel(shape.slice(0, d));
        const out = alloc(a.dtype, numel(outShape)), O = out.data, A = a.data;
        if (NATIVE !== null && O.length >= NATIVE_MIN && NATIVE(N_INDEX_SELECT, A, I, O, outer, shape[d], inner)) return tuple([out, pyShape(outShape)]);
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
        // Parts of the result's dtype copy their elements as they are; a
        // promoted part converts (its values into a scratch `finish` rounds).
        let mixed = false;
        for (let i = 0; i < items.length; i++) if (items[i].items[0].dtype !== dtype) mixed = true;
        const out = mixed ? work(dtype, numel(outShape)) : alloc(dtype, numel(outShape)), O = out.data;
        const outer = numel(first.slice(0, d)), inner = numel(first.slice(d + 1));
        const src = items.map((p) => mixed ? vals(p.items[0]) : p.items[0].data);
        let o = 0;
        for (let x = 0; x < outer; x++) for (let i = 0; i < items.length; i++) {
            const A = src[i], n = shapes[i][d] * inner, base = x * n;
            for (let r = 0; r < n; r++) O[o++] = A[base + r];
        }
        { if (HALF[out.dtype] === 1) finish(out); return tuple([out, pyShape(outShape)]); }
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
        const out = work(a.dtype, outer * newLast), O = out.data, A = vals(a);
        if (value !== 0) O.fill(value);
        for (let x = 0; x < outer; x++) for (let i = 0; i < last; i++) O[x * newLast + left + i] = A[x * last + i];
        { if (HALF[out.dtype] === 1) finish(out); return tuple([out, pyShape(outShape)]); }
    }
    // ---- linear algebra ----------------------------------------------------------------
    // `transB`: b holds the transpose of the right operand, a 2-D [n, k]
    // (F.linear's weight), read in place instead of transposed into a copy.
    // Each output element sums the same k products in the same order, so
    // the result is the plain product's, byte for byte.
    function matmul(a, ashape, b, bshape, transB) {
        let A = ashape.slice(), Bs = bshape.slice();
        if (A.length === 0 || Bs.length === 0) fail(E.RuntimeError, "both arguments to matmul need to be at least 1D");
        if (transB) {
            if (Bs.length !== 2) fail(E.RuntimeError, "matmul: a transposed right operand must be 2-D");
            Bs = [Bs[1], Bs[0]];
        }
        const squeezeA = A.length === 1, squeezeB = Bs.length === 1;
        if (squeezeA) A = [1, A[0]]; if (squeezeB) Bs = [Bs[0], 1];
        const m = A[A.length - 2], k = A[A.length - 1], k2 = Bs[Bs.length - 2], n = Bs[Bs.length - 1];
        if (k !== k2) fail(E.RuntimeError, "mat1 and mat2 shapes cannot be multiplied (" + m + "x" + k + " and " + k2 + "x" + n + ")");
        const batchA = A.slice(0, -2), batchB = Bs.slice(0, -2), batch = broadcastShape(batchA, batchB);
        const nb = numel(batch), sa = bstrides(batchA, batch), sb = bstrides(batchB, batch);
        const dtype = promote(a.dtype, b.dtype);
        const out = work(dtype, nb * m * n), O = out.data, Ad = a.dtype === "bfloat16" ? wide(a) : a.data, Bd = b.dtype === "bfloat16" ? wide(b) : b.data;
        // i-k-j order over one double-precision row: each output element
        // sums its k products in the same order as i-j-k, and rounds to the
        // dtype once, on store. float64 results are unchanged; float32 ones
        // no longer round every partial sum, which moves them by at most a
        // few ulps (the GPU reference keeps that rounding: `graph_matmul`).
        const row = new Float64Array(n);
        forEachBroadcast(batch, sa, sb, (bi, oa, ob) => {
            const baseA = oa * m * k, baseB = ob * k * n, baseO = bi * m * n;
            if (transB) {
                if (NATIVE !== null && NATIVE(N_MATMUL_NT, Ad, Bd, O, baseA, baseB, baseO, m, k, n)) return;
                // Element (i, j) sums A[i, p] * B[j, p] over p in order in a
                // double, as the row loop below sums it, and rounds on store.
                for (let i = 0, ia = baseA, io = baseO; i < m; i++, ia += k) {
                    for (let j = 0, jb = baseB; j < n; j++, jb += k, io++) {
                        let s = 0;
                        for (let p = 0; p < k; p++) s += Ad[ia + p] * Bd[jb + p];
                        O[io] = s;
                    }
                }
                return;
            }
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
        { if (HALF[out.dtype] === 1) finish(out); return tuple([out, pyShape(outShape)]); }
    }
    // max_pool2d over x [N, C, H, W] (dims `d`: [Kh, Kw, Sh, Sw, Ph, Pw, Dh,
    // Dw, Ho, Wo]): per output, the window's values in row-major order, a
    // padded position reading -Infinity (the -inf padding torch pads with).
    // The value is what the `max` reduction of the stacked window keeps
    // (NaN wins, the last one), the int64 index what `argmax` keeps (the
    // first maximum, or the first NaN), so F.max_pool2d's values and window
    // indices are the stacked-views path's exactly.
    function maxPool2d(x, xs, d) {
        const NC = xs[0] * xs[1], H = xs[2], W = xs[3];
        const [kh, kw, sh, sw, ph, pw, dh, dw, Ho, Wo] = d;
        const n = NC * Ho * Wo, out = work(x.dtype, n), idx = alloc("int64", n), X = vals(x), O = out.data, I = idx.data;
        if (NATIVE !== null && NATIVE(N_MAX_POOL2D, X, O, I, NC, H, W, kh, kw, sh, sw, ph, pw, dh, dw, Ho, Wo)) { if (HALF[out.dtype] === 1) finish(out); return tuple([out, idx]); }
        for (let p = 0, o = 0; p < NC; p++) {
            const base = p * H * W;
            for (let i = 0; i < Ho; i++) for (let j = 0; j < Wo; j++, o++) {
                let m = -Infinity, best = -Infinity, at = 0, r = 0;
                for (let a = 0; a < kh; a++) {
                    const y = i * sh - ph + a * dh;
                    for (let b = 0; b < kw; b++, r++) {
                        const xx = j * sw - pw + b * dw;
                        const v = y >= 0 && y < H && xx >= 0 && xx < W ? X[base + y * W + xx] : -Infinity;
                        if (v > m || v !== v) m = v;
                        if (v > best || (v !== v && best === best)) { best = v; at = r; }
                    }
                }
                O[o] = m; I[o] = at;
            }
        }
        if (HALF[out.dtype] === 1) finish(out);
        return tuple([out, idx]);
    }
    // The input-shaped gradient of `maxPool2d`: zeros with g + 0 at each
    // output's window position (for windows that do not overlap, what the
    // stacked views' slice gradients sum to; a position in the padding is
    // dropped, as the padding's own gradient drops it).
    function maxPool2dBackward(g, idx, xs, d) {
        const NC = xs[0] * xs[1], H = xs[2], W = xs[3];
        const [kh, kw, sh, sw, ph, pw, dh, dw, Ho, Wo] = d;
        const out = work(g.dtype, NC * H * W), GX = out.data, G = vals(g), I = idx.data;
        if (G.length !== NC * Ho * Wo || I.length !== G.length) fail(E.RuntimeError, "max_pool2d_backward: size mismatch");
        if (NATIVE !== null && NATIVE(N_MAX_POOL2D_BACKWARD, G, I, GX, NC, H, W, kh, kw, sh, sw, ph, pw, dh, dw, Ho, Wo)) { if (HALF[out.dtype] === 1) finish(out); return out; }
        for (let p = 0, o = 0; p < NC; p++) {
            const base = p * H * W;
            for (let i = 0; i < Ho; i++) for (let j = 0; j < Wo; j++, o++) {
                const r = I[o], a = Math.floor(r / kw), b = r - a * kw;
                const y = i * sh - ph + a * dh, xx = j * sw - pw + b * dw;
                if (y >= 0 && y < H && xx >= 0 && xx < W) GX[base + y * W + xx] = G[o] + 0;
            }
        }
        if (HALF[out.dtype] === 1) finish(out);
        return out;
    }
    // conv1d, stride 1, no padding, no dilation: x [B,C,L], w [O,C,K], bias [O]?
    function conv1d(x, xs, w, ws, bias) {
        const [B, C, L] = xs, [Oc, C2, K] = ws;
        if (C !== C2) fail(E.RuntimeError, "conv1d: expected input with " + C2 + " channels, got " + C);
        const Lo = L - K + 1; if (Lo < 1) fail(E.RuntimeError, "conv1d: kernel size can't be greater than actual input size");
        const dtype = promote(x.dtype, w.dtype), out = work(dtype, B * Oc * Lo), O = out.data, X = vals(x), W = vals(w);
        const Bi = bias === null ? null : vals(bias);
        if (NATIVE !== null && NATIVE(N_CONV1D, X, W, Bi, O, B, C, L, Oc, K, Lo)) { if (HALF[out.dtype] === 1) finish(out); return tuple([out, pyShape([B, Oc, Lo])]); }
        for (let b = 0; b < B; b++) for (let o = 0; o < Oc; o++) {
            const bv = Bi === null ? 0 : Bi[o];
            for (let t = 0; t < Lo; t++) {
                let s = bv;
                for (let c = 0; c < C; c++) { const xb = (b * C + c) * L + t, wb = (o * C + c) * K; for (let k = 0; k < K; k++) s += X[xb + k] * W[wb + k]; }
                O[(b * Oc + o) * Lo + t] = s;
            }
        }
        { if (HALF[out.dtype] === 1) finish(out); return tuple([out, pyShape([B, Oc, Lo])]); }
    }
    function conv1dBackward(x, xs, w, ws, g) {
        const [B, C, L] = xs, [Oc, , K] = ws, Lo = L - K + 1;
        const gx = work(x.dtype, B * C * L), gw = work(w.dtype, Oc * C * K), gb = work(w.dtype, Oc);
        const GX = gx.data, GW = gw.data, GB = gb.data, X = vals(x), W = vals(w), G = vals(g);
        if (NATIVE !== null && NATIVE(N_CONV1D_BACKWARD, X, W, G, GX, GW, GB, B, C, L, Oc, K, Lo)) return tuple([finish(gx), finish(gw), finish(gb)]);
        for (let b = 0; b < B; b++) for (let o = 0; o < Oc; o++) for (let t = 0; t < Lo; t++) {
            const gv = G[(b * Oc + o) * Lo + t]; if (gv === 0) continue;
            GB[o] += gv;
            for (let c = 0; c < C; c++) { const xb = (b * C + c) * L + t, wb = (o * C + c) * K; for (let k = 0; k < K; k++) { GX[xb + k] += gv * W[wb + k]; GW[wb + k] += gv * X[xb + k]; } }
        }
        return tuple([finish(gx), finish(gw), finish(gb)]);
    }
    // Contiguous NCHW cross-correlation; the same index walk computes gradients.
    function conv2d(x, xs, w, ws, bias, stride, padding, dilation, groups, grad = null) {
        if (xs.length !== 4 || ws.length !== 4 || stride.length !== 2 || padding.length !== 2 || dilation.length !== 2)
            fail(E.RuntimeError, "conv2d: invalid dimensions");
        const [B, C, H, W] = xs, [O, Cg, Kh, Kw] = ws;
        if (![B,C,H,W,O,Cg,Kh,Kw,groups,...stride,...padding,...dilation].every(Number.isSafeInteger) ||
            B < 0 || Math.min(C,H,W,O,Cg,Kh,Kw,groups,...stride,...dilation) < 1 || Math.min(...padding) < 0 ||
            C !== Cg * groups || O % groups !== 0 || x.data.length !== B*C*H*W || w.data.length !== O*Cg*Kh*Kw ||
            !isFloatDtype(x.dtype) || x.dtype !== w.dtype || (bias && (bias.data.length !== O || bias.dtype !== x.dtype)))
            fail(E.RuntimeError, "conv2d: incompatible shape, dtype or convolution parameters");
        const [Sh, Sw] = stride, [Ph, Pw] = padding, [Dh, Dw] = dilation;
        const Ho = Math.floor((H + 2*Ph - Dh*(Kh-1) - 1)/Sh) + 1;
        const Wo = Math.floor((W + 2*Pw - Dw*(Kw-1) - 1)/Sw) + 1;
        if (Ho < 1 || Wo < 1) fail(E.RuntimeError, "conv2d: kernel exceeds padded input");
        if (grad && (grad.data.length !== B*O*Ho*Wo || grad.dtype !== x.dtype)) fail(E.RuntimeError, "conv2d: invalid gradient");
        const out = grad ? null : work(x.dtype, B*O*Ho*Wo);
        const gx = grad ? work(x.dtype, x.data.length) : null;
        const gw = grad ? work(w.dtype, w.data.length) : null;
        const gb = grad ? work(w.dtype, O) : null;
        const perGroup = O / groups;
        const Xd = vals(x), Wd = vals(w), Bd = bias ? vals(bias) : null, Gd = grad ? vals(grad) : null;
        if (NATIVE !== null && (grad
            ? NATIVE(N_CONV2D_BACKWARD, Xd, Wd, Gd, gx.data, gw.data, gb.data, B, C, H, W, O, Cg, Kh, Kw, Sh, Sw, Ph, Pw, Dh, Dw, groups, Ho, Wo)
            : NATIVE(N_CONV2D, Xd, Wd, Bd, out.data, B, C, H, W, O, Cg, Kh, Kw, Sh, Sw, Ph, Pw, Dh, Dw, groups, Ho, Wo)))
            return grad ? tuple([finish(gx),finish(gw),finish(gb)]) : tuple([finish(out),pyShape([B,O,Ho,Wo])]);
        for (let b=0; b<B; b++) for (let o=0; o<O; o++) {
            const firstChannel = Math.floor(o/perGroup)*Cg;
            for (let h=0; h<Ho; h++) for (let v=0; v<Wo; v++) {
                const oi = ((b*O+o)*Ho+h)*Wo+v;
                const gv = grad ? Gd[oi] : 0;
                let sum = bias ? Bd[o] : 0;
                if (grad) gb.data[o] += gv;
                for (let c=0; c<Cg; c++) for (let kh=0; kh<Kh; kh++) for (let kw=0; kw<Kw; kw++) {
                    const ih = h*Sh-Ph+kh*Dh, iw = v*Sw-Pw+kw*Dw;
                    if (ih<0 || ih>=H || iw<0 || iw>=W) continue;
                    const xi = ((b*C+firstChannel+c)*H+ih)*W+iw;
                    const wi = ((o*Cg+c)*Kh+kh)*Kw+kw;
                    if (grad) { gx.data[xi] += gv*Wd[wi]; gw.data[wi] += gv*Xd[xi]; }
                    else sum += Xd[xi]*Wd[wi];
                }
                if (!grad) out.data[oi] = sum;
            }
        }
        return grad ? tuple([finish(gx),finish(gw),finish(gb)]) : tuple([finish(out),pyShape([B,O,Ho,Wo])]);
    }
    function softmax(a, shape, dim, log) {
        const d = dim < 0 ? dim + shape.length : dim, n = shape[d];
        const outer = numel(shape.slice(0, d)), inner = numel(shape.slice(d + 1));
        const dtype = !isFloatDtype(a.dtype) ? "float32" : a.dtype, out = work(dtype, a.data.length), O = out.data, A = vals(a);
        if (NATIVE !== null && NATIVE(N_SOFTMAX, A, O, outer, n, inner, !!log)) { if (HALF[out.dtype] === 1) finish(out); return out; }
        for (let x = 0; x < outer; x++) for (let r = 0; r < inner; r++) {
            const base = x * n * inner + r;
            let mx = -Infinity; for (let i = 0; i < n; i++) { const v = A[base + i * inner]; if (v > mx) mx = v; }
            let sum = 0; for (let i = 0; i < n; i++) sum += Math.exp(A[base + i * inner] - mx);
            const ls = Math.log(sum);
            for (let i = 0; i < n; i++) { const z = A[base + i * inner] - mx; O[base + i * inner] = log ? z - ls : Math.exp(z) / sum; }
        }
        { if (HALF[out.dtype] === 1) finish(out); return out; }
    }
    // A running reduction along `dim`, accumulated in double precision and
    // rounded on store. cummax/cummin also return the index of each running
    // extreme (the latest one on ties, and NaN propagating, as PyTorch).
    function scan(op, a, shape, dim) {
        const d = dim < 0 ? dim + shape.length : dim, n = shape.length === 0 ? 1 : shape[d];
        const outer = shape.length === 0 ? 1 : numel(shape.slice(0, d)), inner = shape.length === 0 ? 1 : numel(shape.slice(d + 1));
        const out = work(a.dtype, a.data.length), O = out.data, A = vals(a);
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
        { if (HALF[out.dtype] === 1) finish(out); return arg ? tuple([out, idx]) : out; }
    }
    // Stable merge sort of indices along `dim`.
    function argsort(a, shape, dim, descending) {
        const d = dim < 0 ? dim + shape.length : dim, n = shape[d];
        const outer = numel(shape.slice(0, d)), inner = numel(shape.slice(d + 1));
        const out = alloc("int64", a.data.length), O = out.data, A = vals(a);
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
        const out = work(dtype, numel(shape)), O = out.data;
        const sc = bstrides(cs, shape), sa = bstrides(as_, shape), sb = bstrides(bs, shape);
        const Cd = c.data, Ad = vals(a), Bd = vals(b);
        if (O.length >= NATIVE_MIN && NATIVE !== null && NATIVE(N_WHERE, Cd, Ad, Bd, O, shape, sc, sa, sb)) { if (HALF[out.dtype] === 1) finish(out); return tuple([out, pyShape(shape)]); }
        const rank = shape.length, idx = new Array(rank).fill(0);
        let oc = 0, oa = 0, ob = 0;
        for (let flat = 0; flat < O.length; flat++) {
            O[flat] = Cd[oc] ? Ad[oa] : Bd[ob];
            for (let d = rank - 1; d >= 0; d--) { idx[d]++; oc += sc[d]; oa += sa[d]; ob += sb[d]; if (idx[d] < shape[d]) break; oc -= sc[d] * shape[d]; oa -= sa[d] * shape[d]; ob -= sb[d] * shape[d]; idx[d] = 0; }
        }
        { if (HALF[out.dtype] === 1) finish(out); return tuple([out, pyShape(shape)]); }
    }
    // astype between real dtypes.
    function astype(s, d) {
        const out = alloc(d, s.data.length);
        // A float target converts exactly as castValue does (the typed
        // array rounds float32 on store; float16 and bfloat16 round that
        // float32 to the format); integer targets truncate.
        if (d === s.dtype && HALF[d] === 1) { out.data.set(s.data); return out; }
        const S = vals(s), O = out.data, n = O.length;
        if (d === "float32" || d === "float64") O.set(S);
        else if (d === "float16") { if (S instanceof Float32Array) O.set(S); else for (let i = 0; i < n; i++) O[i] = Math.fround(S[i]); }
        else if (d === "bfloat16") for (let i = 0; i < n; i++) O[i] = bfBits(Math.fround(S[i]));
        else for (let i = 0; i < n; i++) O[i] = castValue(d, S[i]);
        return out;
    }
    // Every element of complex storage s set to re + im j (rounded to its dtype).
    function fillPair(s, re, im) { const D = s.data; for (let i = 0; i < D.length; i += 2) { D[i] = re; D[i + 1] = im; } }
    // ---- complex kernels ----------------------------------------------------------------------
    // Elementwise complex arithmetic in double precision, rounded once on
    // store (complex64 into its Float32Array). Operands arrive as complex
    // storages or real storages (read with a zero imaginary part). The
    // formulas follow C++'s std::complex / NumPy: division scales by the
    // larger of |c| and |d| (Smith), sqrt is the stable half-angle form,
    // tanh Kahan's form.
    const CZ = new Float64Array(2);
    function cdiv(a, b, c, d) {
        const ac = Math.abs(c), ad = Math.abs(d);
        if (ac >= ad) {
            if (ac === 0 && ad === 0) { CZ[0] = a / ac; CZ[1] = b / ad; return; }
            const rat = d / c, scl = 1 / (c + d * rat);
            CZ[0] = (a + b * rat) * scl; CZ[1] = (b - a * rat) * scl;
        } else {
            const rat = c / d, scl = 1 / (d + c * rat);
            CZ[0] = (a * rat + b) * scl; CZ[1] = (b * rat - a) * scl;
        }
    }
    function cexp(x, y) {
        if (y === 0) { CZ[0] = Math.exp(x); CZ[1] = y; return; }
        const e = Math.exp(x); CZ[0] = e * Math.cos(y); CZ[1] = e * Math.sin(y);
    }
    function clog(x, y) { CZ[0] = Math.log(Math.hypot(x, y)); CZ[1] = Math.atan2(y, x); }
    function csqrt(x, y) {
        if (x === 0 && y === 0) { CZ[0] = 0; CZ[1] = y; return; }
        if (y === Infinity || y === -Infinity) { CZ[0] = Infinity; CZ[1] = y; return; }
        const t = Math.sqrt((Math.abs(x) + Math.hypot(x, y)) / 2);
        if (x >= 0) { CZ[0] = t; CZ[1] = y / (2 * t); }
        else { CZ[0] = Math.abs(y) / (2 * t); CZ[1] = y < 0 || Object.is(y, -0) ? -t : t; }
    }
    function ctanh(x, y) {
        if (Math.abs(x) > 22) { CZ[0] = x > 0 ? 1 : -1; CZ[1] = 4 * Math.sin(y) * Math.cos(y) * Math.exp(-2 * Math.abs(x)); return; }
        const t = Math.tan(y), b = 1 + t * t, sh = Math.sinh(x), r = Math.sqrt(1 + sh * sh), den = 1 + b * sh * sh;
        CZ[0] = b * r * sh / den; CZ[1] = t / den;
    }
    function cpow(a, b, c, d) {
        // z ** w = exp(w log z); z ** 0 is 1, 0 ** w is 0 for a positive real w.
        if (c === 0 && d === 0) { CZ[0] = 1; CZ[1] = 0; return; }
        if (a === 0 && b === 0 && c > 0 && d === 0) { CZ[0] = 0; CZ[1] = 0; return; }
        clog(a, b);
        const lr = CZ[0], li = CZ[1];
        cexp(c * lr - d * li, c * li + d * lr);
    }
    // One complex unary op into CZ; false for an op with no complex form.
    function cunaryOne(op, x, y) {
        switch (op) {
            case "neg": CZ[0] = -x; CZ[1] = -y; return true;
            case "conj": CZ[0] = x; CZ[1] = -y; return true;
            case "exp": cexp(x, y); return true;
            case "log": clog(x, y); return true;
            case "log2": clog(x, y); CZ[0] /= Math.LN2; CZ[1] /= Math.LN2; return true;
            case "log10": clog(x, y); CZ[0] /= Math.LN10; CZ[1] /= Math.LN10; return true;
            case "sqrt": csqrt(x, y); return true;
            case "rsqrt": { csqrt(x, y); const p = CZ[0], q = CZ[1]; cdiv(1, 0, p, q); return true; }
            case "reciprocal": cdiv(1, 0, x, y); return true;
            case "square": CZ[0] = x * x - y * y; CZ[1] = 2 * x * y; return true;
            case "sin": CZ[0] = Math.sin(x) * Math.cosh(y); CZ[1] = Math.cos(x) * Math.sinh(y); return true;
            case "cos": CZ[0] = Math.cos(x) * Math.cosh(y); CZ[1] = -Math.sin(x) * Math.sinh(y); return true;
            case "sinh": CZ[0] = Math.sinh(x) * Math.cos(y); CZ[1] = Math.cosh(x) * Math.sin(y); return true;
            case "cosh": CZ[0] = Math.cosh(x) * Math.cos(y); CZ[1] = Math.sinh(x) * Math.sin(y); return true;
            case "tanh": ctanh(x, y); return true;
            // tan z = -i tanh(i z)
            case "tan": { ctanh(-y, x); const u = CZ[0]; CZ[0] = CZ[1]; CZ[1] = -u; return true; }
            case "sigmoid": { cexp(-x, -y); const p = 1 + CZ[0], q = CZ[1]; cdiv(1, 0, p, q); return true; }
            case "expm1": { const e = Math.exp(x), h = Math.sin(y / 2); CZ[0] = Math.expm1(x) * Math.cos(y) - 2 * h * h; CZ[1] = e * Math.sin(y); return true; }
            case "log1p": CZ[0] = 0.5 * Math.log1p(x * (2 + x) + y * y); CZ[1] = Math.atan2(y, 1 + x); return true;
            case "exp2": cexp(x * Math.LN2, y * Math.LN2); return true;
            // z / |z| as a complex division (so -2.5-0j gives -1+0j, as PyTorch).
            case "sgn": { const r = Math.hypot(x, y); if (r === 0) { CZ[0] = 0; CZ[1] = 0; } else cdiv(x, y, r, 0); return true; }
        }
        return false;
    }
    // Complex unary ops with a real (1) or bool (2) result.
    const CREAL = { abs: 1, angle: 1, real: 1, imag: 1, isnan: 2, isinf: 2, isfinite: 2, not: 2 };
    function cunary(op, a) {
        const n = count(a), A = a.data, kind = CREAL[op];
        if (kind !== undefined) {
            const out = alloc(kind === 2 ? "bool" : PAIR[a.dtype], n), O = out.data;
            for (let i = 0, j = 0; i < n; i++, j += 2) {
                const x = A[j], y = A[j + 1];
                switch (op) {
                    case "abs": O[i] = Math.hypot(x, y); break;
                    case "angle": O[i] = Math.atan2(y, x); break;
                    case "real": O[i] = x; break;
                    case "imag": O[i] = y; break;
                    case "isnan": O[i] = x !== x || y !== y ? 1 : 0; break;
                    case "isinf": O[i] = x === Infinity || x === -Infinity || y === Infinity || y === -Infinity ? 1 : 0; break;
                    case "isfinite": O[i] = Number.isFinite(x) && Number.isFinite(y) ? 1 : 0; break;
                    default: O[i] = x === 0 && y === 0 ? 1 : 0;
                }
            }
            return out;
        }
        if (!cunaryOne(op, 0, 0)) fail(E.RuntimeError, "\"" + op + "\" is not implemented for complex tensors on Zipp");
        const out = calloc(a.dtype, n), O = out.data;
        for (let j = 0; j < 2 * n; j += 2) { cunaryOne(op, A[j], A[j + 1]); O[j] = CZ[0]; O[j + 1] = CZ[1]; }
        return out;
    }
    // The scalar exponents PyTorch's complex pow takes a shortcut for.
    function cpowScalar(e, x, y) {
        if (e === 2) { CZ[0] = x * x - y * y; CZ[1] = 2 * x * y; return true; }
        if (e === 3) { const p = x * x - y * y, q = 2 * x * y; CZ[0] = p * x - q * y; CZ[1] = p * y + q * x; return true; }
        if (e === 0.5) { csqrt(x, y); return true; }
        if (e === -0.5) { csqrt(x, y); const p = CZ[0], q = CZ[1]; cdiv(1, 0, p, q); return true; }
        if (e === -1) { cdiv(1, 0, x, y); return true; }
        if (e === -2) { const p = x * x - y * y, q = 2 * x * y; cdiv(1, 0, p, q); return true; }
        return false;
    }
    const CBIN = { add: 1, sub: 1, mul: 1, div: 1, pow: 1, eq: 2, ne: 2 };
    function cbinary(op, a, ashape, b, bshape, want) {
        const kind = CBIN[op];
        if (kind === undefined) fail(E.RuntimeError, "\"" + op + "\" is not implemented for complex tensors on Zipp");
        const shape = broadcastShape(ashape, bshape), n = numel(shape);
        const cdt = want !== null && PAIR[want] !== undefined ? want : promote(a.dtype, b.dtype);
        const A = toPair(a, cdt).data, B = toPair(b, cdt).data;
        const out = kind === 2 ? alloc("bool", n) : calloc(cdt, n), O = out.data;
        const sa = bstrides(ashape, shape), sb = bstrides(bshape, shape);
        // A scalar real exponent (a one-element operand with no imaginary part).
        const scalarExp = op === "pow" && numel(bshape) === 1 && B[1] === 0 && cpowScalar(B[0], 1, 0) ? B[0] : null;
        forEachBroadcast(shape, sa, sb, (o, xi, yi) => {
            const x = A[2 * xi], y = A[2 * xi + 1], c = B[2 * yi], d = B[2 * yi + 1];
            switch (op) {
                case "add": O[2 * o] = x + c; O[2 * o + 1] = y + d; return;
                case "sub": O[2 * o] = x - c; O[2 * o + 1] = y - d; return;
                case "mul": O[2 * o] = x * c - y * d; O[2 * o + 1] = x * d + y * c; return;
                case "div": cdiv(x, y, c, d); break;
                case "pow": if (scalarExp !== null) cpowScalar(scalarExp, x, y); else cpow(x, y, c, d); break;
                case "eq": O[o] = x === c && y === d ? 1 : 0; return;
                default: O[o] = x !== c || y !== d ? 1 : 0; return;
            }
            O[2 * o] = CZ[0]; O[2 * o + 1] = CZ[1];
        });
        return tuple([out, pyShape(shape)]);
    }
    // The reduced dims of a `reduce` call, normalized, as a JS array (all
    // of them for None).
    function redDims(dims, rank) {
        if (dims === null) { const all = []; for (let d = 0; d < rank; d++) all.push(d); return all; }
        const red = ints(dims);
        for (let i = 0; i < red.length; i++) { const d = red[i] < 0 ? red[i] + rank : red[i]; if (d < 0 || d >= rank) fail(E.IndexError, "Dimension out of range"); red[i] = d; }
        return red;
    }
    // sum/mean run as the real reduction over shape + [2] (each part
    // separately, which is complex addition); prod multiplies pairs in
    // double precision, rounding once.
    function creduce(op, a, shape, dims, keepdim, precise) {
        const rank = shape.length, red = redDims(dims, rank);
        if (op === "sum" || op === "mean") {
            if (rank === 0) return tuple([cmake(a.dtype, a.data.slice()), pyShape([])]);
            return lowered(reduce(op, realOf(a), shape.concat([2]), pyInts(red), keepdim, precise), a.dtype);
        }
        if (op !== "prod") fail(E.RuntimeError, "\"" + op + "\" is not implemented for complex tensors on Zipp");
        const isRed = new Array(rank).fill(false);
        for (let i = 0; i < red.length; i++) isRed[red[i]] = true;
        const outShape = [], keptShape = [];
        for (let d = 0; d < rank; d++) { if (isRed[d]) keptShape.push(1); else { outShape.push(shape[d]); keptShape.push(shape[d]); } }
        const nOut = numel(keptShape), nIn = numel(shape), P = new Float64Array(2 * nOut), A = a.data, outS = strides(keptShape);
        for (let i = 0; i < nOut; i++) P[2 * i] = 1;
        const pos = new Array(rank).fill(0);
        let oo = 0;
        for (let flat = 0; flat < nIn; flat++) {
            const x = P[2 * oo], y = P[2 * oo + 1], c = A[2 * flat], d = A[2 * flat + 1];
            P[2 * oo] = x * c - y * d; P[2 * oo + 1] = x * d + y * c;
            for (let q = rank - 1; q >= 0; q--) {
                pos[q]++; if (!isRed[q]) oo += outS[q];
                if (pos[q] < shape[q]) break;
                if (!isRed[q]) oo -= outS[q] * shape[q];
                pos[q] = 0;
            }
        }
        const out = calloc(a.dtype, nOut);
        out.data.set(P);
        return tuple([out, pyShape(keepdim ? keptShape : outShape)]);
    }
    function cscan(op, a, shape, dim) {
        const rank = shape.length, d = rank === 0 ? 0 : (dim < 0 ? dim + rank : dim);
        if (op === "cumsum") {
            if (rank === 0) return cmake(a.dtype, a.data.slice());
            return cmake(a.dtype, scan("cumsum", realOf(a), shape.concat([2]), d).data);
        }
        if (op !== "cumprod") fail(E.RuntimeError, "\"" + op + "\" is not implemented for complex tensors on Zipp");
        const n = rank === 0 ? 1 : shape[d], outer = rank === 0 ? 1 : numel(shape.slice(0, d)), inner = rank === 0 ? 1 : numel(shape.slice(d + 1));
        const out = calloc(a.dtype, count(a)), O = out.data, A = a.data;
        for (let x = 0; x < outer; x++) for (let r = 0; r < inner; r++) {
            let pr = 1, pi = 0;
            for (let i = 0; i < n; i++) {
                const p = 2 * (x * n * inner + i * inner + r), c = A[p], q = A[p + 1];
                const nr = pr * c - pi * q; pi = pr * q + pi * c; pr = nr;
                O[p] = pr; O[p + 1] = pi;
            }
        }
        return out;
    }
    // A complex matmul as four real double-precision products of the
    // parts (each a `matmul` of float64 planes, which sums exactly as the
    // real kernel does), combined and rounded once to the result dtype.
    function cmatmul(a, ashape, b, bshape) {
        const cdt = promote(a.dtype, b.dtype), A = toPair(a, cdt).data, B = toPair(b, cdt).data;
        const plane = (D, off) => { const n = D.length >> 1, P = new Float64Array(n); for (let i = 0; i < n; i++) P[i] = D[2 * i + off]; return make("float64", P); };
        const ar = plane(A, 0), ai = plane(A, 1), br = plane(B, 0), bi = plane(B, 1);
        const rr = matmul(ar, ashape, br, bshape, false), ii = matmul(ai, ashape, bi, bshape, false);
        const ri = matmul(ar, ashape, bi, bshape, false), ir = matmul(ai, ashape, br, bshape, false);
        const RR = rr.items[0].data, II = ii.items[0].data, RI = ri.items[0].data, IR = ir.items[0].data, n = RR.length;
        const out = calloc(cdt, n), O = out.data;
        for (let i = 0; i < n; i++) { O[2 * i] = RR[i] - II[i]; O[2 * i + 1] = RI[i] + IR[i]; }
        return tuple([out, rr.items[1]]);
    }
    // astype involving a complex dtype: into complex (a real source gets
    // zero imaginary parts), or from complex into a real dtype (the real
    // part; bool asks for a nonzero part).
    function castPair(s, d) {
        if (PAIR[d] !== undefined) return toPair(s, d);
        const n = count(s), S = s.data;
        if (d === "bool") { const out = alloc("bool", n); for (let i = 0; i < n; i++) out.data[i] = S[2 * i] !== 0 || S[2 * i + 1] !== 0 ? 1 : 0; return out; }
        const re = alloc(PAIR[s.dtype], n), R = re.data;
        for (let i = 0; i < n; i++) R[i] = S[2 * i];
        return d === re.dtype ? re : astype(re, d);
    }
    // ---- FFT (torch.fft) ----------------------------------------------------------------------
    // One-dimensional discrete Fourier transforms of contiguous rows, in
    // double precision, rounded once to the output dtype. A length whose
    // prime factors are all at most 31 runs a mixed-radix
    // decimation-in-time Cooley-Tukey (radix 4 first, then 2, 3, 5 and the
    // odd primes to 31); any other length runs Bluestein's chirp-z
    // algorithm over a power-of-two convolution. Twiddles are cos/sin of
    // 2*pi*k/N, computed directly (no recurrences).
    // `vm::py_tensor::fft` is this code line for line (the same operations
    // in the same order), so the native loop gives these bytes exactly.
    // modes: 0 complex -> complex, 1 real -> onesided complex (n/2+1
    // bins), 2 onesided complex -> real (Hermitian extension; the
    // imaginary parts of bins 0 and n/2 are ignored), 3 real -> all n
    // bins (the onesided ones, then their conjugates). A real input's
    // bins 0 and n/2 are real exactly.
    function fftFactors(n) {
        const f = [];
        let m = n;
        while (m % 4 === 0) { f.push(4); m /= 4; }
        while (m % 2 === 0) { f.push(2); m /= 2; }
        while (m % 3 === 0) { f.push(3); m /= 3; }
        while (m % 5 === 0) { f.push(5); m /= 5; }
        // Other primes up to 31 run the generic odd-radix pass.
        for (let q = 7; q <= 31; q += 2) while (m % q === 0) { f.push(q); m /= q; }
        return m === 1 ? f : null;
    }
    // cos and sin of 2*pi*k/n into TW: the angle folded into the first
    // octant by the circle's symmetries (in exact integers, eighths of k),
    // so the table is exactly symmetric and exact at multiples of pi/2.
    const TW = new Float64Array(2);
    function twiddle(k, n) {
        const T = 8 * n;
        let a = 8 * k, sc = 1, ss = 1, swap = false;
        if (2 * a > T) { a = T - a; ss = -1; }
        if (4 * a > T) { a = T / 2 - a; sc = -1; }
        if (8 * a > T) { a = T / 4 - a; swap = true; }
        const t = 2 * Math.PI * a / T, c = Math.cos(t), s = Math.sin(t);
        TW[0] = sc * (swap ? s : c); TW[1] = ss * (swap ? c : s);
    }
    // A plan for length n and direction sign (-1 forward, +1 inverse).
    function fftPlan(n, sign) {
        const factors = fftFactors(n);
        if (factors !== null) {
            const cr = new Float64Array(n), ci = new Float64Array(n);
            for (let k = 0; k < n; k++) { twiddle(k, n); cr[k] = TW[0]; ci[k] = sign * TW[1]; }
            return { n: n, sign: sign, factors: factors, cr: cr, ci: ci, blue: null };
        }
        let m = 1;
        while (m < 2 * n - 1) m *= 2;
        const sub = fftPlan(m, -1), inv = fftPlan(m, 1);
        // The chirp c_j = exp(sign * i * pi * j^2 / n), j^2 taken mod 2n.
        const wr = new Float64Array(n), wi = new Float64Array(n);
        for (let j = 0; j < n; j++) { twiddle((j * j) % (2 * n), 2 * n); wr[j] = TW[0]; wi[j] = sign * TW[1]; }
        // The transformed conjugate chirp, wrapped: b_j = b_(m-j) = conj(c_j).
        const br = new Float64Array(m), bi = new Float64Array(m);
        for (let j = 0; j < n; j++) { br[j] = wr[j]; bi[j] = -wi[j]; if (j > 0) { br[m - j] = wr[j]; bi[m - j] = -wi[j]; } }
        const Br = new Float64Array(m), Bi = new Float64Array(m);
        fftRun(sub, br, bi, Br, Bi);
        return { n: n, sign: sign, factors: null, cr: null, ci: null, blue: { m: m, sub: sub, inv: inv, wr: wr, wi: wi, Br: Br, Bi: Bi,
            ar: new Float64Array(m), ai: new Float64Array(m), tr: new Float64Array(m), ti: new Float64Array(m) } };
    }
    // out[oo + k] (k < len) = DFT of in[io + j * stride] over the plan's
    // factors from index fi on; the twiddle of W_len^e is table entry
    // e * (N / len).
    function fftRec(p, len, xr, xi, io, stride, yr, yi, oo, fi) {
        if (len === 1) { yr[oo] = xr[io]; yi[oo] = xi[io]; return; }
        const r = p.factors[fi], m = len / r, N = p.n, step = N / len, cr = p.cr, ci = p.ci;
        for (let q = 0; q < r; q++) fftRec(p, m, xr, xi, io + q * stride, stride * r, yr, yi, oo + q * m, fi + 1);
        for (let k = 0; k < m; k++) {
            if (r === 2) {
                const a = oo + k, b = a + m;
                let br = yr[b], bi = yi[b];
                if (k !== 0) { const e = k * step, wr = cr[e], wi = ci[e], t = br * wr - bi * wi; bi = br * wi + bi * wr; br = t; }
                const ar = yr[a], ai = yi[a];
                yr[a] = ar + br; yi[a] = ai + bi; yr[b] = ar - br; yi[b] = ai - bi;
            } else if (r === 4) {
                const i0 = oo + k, i1 = i0 + m, i2 = i1 + m, i3 = i2 + m;
                let x1r = yr[i1], x1i = yi[i1], x2r = yr[i2], x2i = yi[i2], x3r = yr[i3], x3i = yi[i3];
                if (k !== 0) {
                    let e = k * step, wr = cr[e], wi = ci[e], t = x1r * wr - x1i * wi; x1i = x1r * wi + x1i * wr; x1r = t;
                    e = 2 * k * step; wr = cr[e]; wi = ci[e]; t = x2r * wr - x2i * wi; x2i = x2r * wi + x2i * wr; x2r = t;
                    e = 3 * k * step; wr = cr[e]; wi = ci[e]; t = x3r * wr - x3i * wi; x3i = x3r * wi + x3i * wr; x3r = t;
                }
                const x0r = yr[i0], x0i = yi[i0];
                const s0r = x0r + x2r, s0i = x0i + x2i, d0r = x0r - x2r, d0i = x0i - x2i;
                const s1r = x1r + x3r, s1i = x1i + x3i, d1r = x1r - x3r, d1i = x1i - x3i;
                // W_4 = sign * i: X1 = d0 + sign*i*d1, X3 = d0 - sign*i*d1.
                const sg = p.sign;
                const jr = sg > 0 ? -d1i : d1i, ji = sg > 0 ? d1r : -d1r;
                yr[i0] = s0r + s1r; yi[i0] = s0i + s1i;
                yr[i1] = d0r + jr; yi[i1] = d0i + ji;
                yr[i2] = s0r - s1r; yi[i2] = s0i - s1i;
                yr[i3] = d0r - jr; yi[i3] = d0i - ji;
            } else {
                // Radix 3 or 5: twiddled inputs, then the r-point DFT with
                // its exact constants (cos 2pi/3 = -1/2, ...).
                const tr = p.tmpr, ti = p.tmpi;
                for (let q = 0; q < r; q++) {
                    const at = oo + q * m + k;
                    let xr0 = yr[at], xi0 = yi[at];
                    if (q !== 0 && k !== 0) { const e = q * k * step, wr = cr[e], wi = ci[e], t = xr0 * wr - xi0 * wi; xi0 = xr0 * wi + xi0 * wr; xr0 = t; }
                    tr[q] = xr0; ti[q] = xi0;
                }
                const sg = p.sign;
                if (r > 5) {
                    // Odd prime r: pairs q, r-q (sums and differences), then
                    // X_s = A + i B and X_(r-s) = A - i B.
                    const h = (r - 1) >> 1, pr = p.pr, pi = p.pi, mr = p.mr, mi = p.mi, big = N / r;
                    let x0r = tr[0], x0i = ti[0];
                    for (let q = 1; q <= h; q++) { pr[q] = tr[q] + tr[r - q]; pi[q] = ti[q] + ti[r - q]; mr[q] = tr[q] - tr[r - q]; mi[q] = ti[q] - ti[r - q]; x0r += pr[q]; x0i += pi[q]; }
                    yr[oo + k] = x0r; yi[oo + k] = x0i;
                    for (let s = 1; s <= h; s++) {
                        let ar = tr[0], ai = ti[0], br = 0, bi = 0;
                        for (let q = 1; q <= h; q++) {
                            const e = ((q * s) % r) * big, wr = cr[e], wi = ci[e];
                            ar += wr * pr[q]; ai += wr * pi[q]; br += wi * mr[q]; bi += wi * mi[q];
                        }
                        yr[oo + s * m + k] = ar - bi; yi[oo + s * m + k] = ai + br;
                        yr[oo + (r - s) * m + k] = ar + bi; yi[oo + (r - s) * m + k] = ai - br;
                    }
                } else if (r === 3) {
                    const t1r = tr[1] + tr[2], t1i = ti[1] + ti[2];
                    const t2r = tr[0] - 0.5 * t1r, t2i = ti[0] - 0.5 * t1i;
                    const t3r = 0.8660254037844386 * (tr[1] - tr[2]), t3i = 0.8660254037844386 * (ti[1] - ti[2]);
                    // s * i * t3
                    const ur = sg > 0 ? -t3i : t3i, ui = sg > 0 ? t3r : -t3r;
                    const i0 = oo + k, i1 = i0 + m, i2 = i1 + m;
                    yr[i0] = tr[0] + t1r; yi[i0] = ti[0] + t1i;
                    yr[i1] = t2r + ur; yi[i1] = t2i + ui;
                    yr[i2] = t2r - ur; yi[i2] = t2i - ui;
                } else {
                    const c1 = 0.30901699437494745, c2 = -0.8090169943749475, s1 = 0.9510565162951535, s2 = 0.5877852522924731;
                    const t1r = tr[1] + tr[4], t1i = ti[1] + ti[4], t2r = tr[2] + tr[3], t2i = ti[2] + ti[3];
                    const d1r = tr[1] - tr[4], d1i = ti[1] - ti[4], d2r = tr[2] - tr[3], d2i = ti[2] - ti[3];
                    const a1r = tr[0] + c1 * t1r + c2 * t2r, a1i = ti[0] + c1 * t1i + c2 * t2i;
                    const a2r = tr[0] + c2 * t1r + c1 * t2r, a2i = ti[0] + c2 * t1i + c1 * t2i;
                    const b1r = s1 * d1r + s2 * d2r, b1i = s1 * d1i + s2 * d2i;
                    const b2r = s2 * d1r - s1 * d2r, b2i = s2 * d1i - s1 * d2i;
                    // s * i * b
                    const u1r = sg > 0 ? -b1i : b1i, u1i = sg > 0 ? b1r : -b1r, u2r = sg > 0 ? -b2i : b2i, u2i = sg > 0 ? b2r : -b2r;
                    const i0 = oo + k, i1 = i0 + m, i2 = i1 + m, i3 = i2 + m, i4 = i3 + m;
                    yr[i0] = tr[0] + t1r + t2r; yi[i0] = ti[0] + t1i + t2i;
                    yr[i1] = a1r + u1r; yi[i1] = a1i + u1i;
                    yr[i4] = a1r - u1r; yi[i4] = a1i - u1i;
                    yr[i2] = a2r + u2r; yi[i2] = a2i + u2i;
                    yr[i3] = a2r - u2r; yi[i3] = a2i - u2i;
                }
            }
        }
    }
    // The unnormalized transform of (xr, xi) into (yr, yi), all of the
    // plan's length (x is not modified).
    function fftRun(p, xr, xi, yr, yi) {
        const n = p.n;
        if (p.blue === null) {
            if (p.tmpr === undefined) {
                p.tmpr = new Float64Array(32); p.tmpi = new Float64Array(32);
                p.pr = new Float64Array(16); p.pi = new Float64Array(16); p.mr = new Float64Array(16); p.mi = new Float64Array(16);
            }
            fftRec(p, n, xr, xi, 0, 1, yr, yi, 0, 0);
            return;
        }
        const b = p.blue, m = b.m, ar = b.ar, ai = b.ai, tr = b.tr, ti = b.ti, wr = b.wr, wi = b.wi;
        for (let j = 0; j < m; j++) { ar[j] = 0; ai[j] = 0; }
        for (let j = 0; j < n; j++) { const a = xr[j], c = xi[j]; ar[j] = a * wr[j] - c * wi[j]; ai[j] = a * wi[j] + c * wr[j]; }
        fftRun(b.sub, ar, ai, tr, ti);
        for (let j = 0; j < m; j++) { const a = tr[j], c = ti[j], d = b.Br[j], e = b.Bi[j]; ar[j] = a * d - c * e; ai[j] = a * e + c * d; }
        fftRun(b.inv, ar, ai, tr, ti);
        for (let k = 0; k < n; k++) { const a = tr[k] / m, c = ti[k] / m; yr[k] = a * wr[k] - c * wi[k]; yi[k] = a * wi[k] + c * wr[k]; }
    }
    // fft(src, rows, nIn, n, mode, inverse, scale): `rows` rows of nIn
    // input elements each (zero-padded or truncated to the transform
    // length n; for mode 2, to n/2+1 bins), every output scaled by `scale`.
    function fft(a, rows, nIn, n, mode, inverse, scale) {
        const complexIn = a.cls === CStorage, rdt = complexIn ? PAIR[a.dtype] : a.dtype;
        const realIn = mode === 1 || mode === 3;
        if (realIn === complexIn || (rdt !== "float32" && rdt !== "float64")) fail(E.TypeError, "fft: bad input dtype " + a.dtype);
        const half = (n >> 1) + 1, nOut = mode === 1 ? half : n;
        const out = mode === 2 ? alloc(rdt, rows * nOut) : calloc(COMPLEX_OF[rdt], rows * nOut), O = out.data, A = a.data;
        if (rows === 0 || n === 0) return out;
        if (NATIVE !== null && NATIVE(N_FFT, A, O, rows, nIn, n, mode, inverse ? 1 : 0, scale)) return out;
        const p = fftPlan(n, inverse ? 1 : -1);
        const xr = new Float64Array(n), xi = new Float64Array(n), yr = new Float64Array(n), yi = new Float64Array(n);
        for (let row = 0; row < rows; row++) {
            for (let j = 0; j < n; j++) { xr[j] = 0; xi[j] = 0; }
            if (mode === 0) {
                const c = nIn < n ? nIn : n, base = 2 * row * nIn;
                for (let j = 0; j < c; j++) { xr[j] = A[base + 2 * j]; xi[j] = A[base + 2 * j + 1]; }
            } else if (realIn) {
                const c = nIn < n ? nIn : n, base = row * nIn;
                for (let j = 0; j < c; j++) xr[j] = A[base + j];
            } else {
                // The Hermitian extension of the first n/2+1 bins.
                const c = nIn < half ? nIn : half, base = 2 * row * nIn;
                for (let k = 0; k < c; k++) {
                    const re = A[base + 2 * k], im = A[base + 2 * k + 1];
                    if (k === 0 || 2 * k === n) { xr[k] = re; continue; }
                    xr[k] = re; xi[k] = im; xr[n - k] = re; xi[n - k] = -im;
                }
            }
            fftRun(p, xr, xi, yr, yi);
            if (realIn) {
                yi[0] = 0;
                if ((n & 1) === 0) yi[n >> 1] = 0;
                if (mode === 3) for (let k = half; k < n; k++) { yr[k] = yr[n - k]; yi[k] = -yi[n - k]; }
            }
            if (mode === 2) { const base = row * n; for (let j = 0; j < n; j++) O[base + j] = yr[j] * scale; }
            else { const base = 2 * row * nOut; for (let k = 0; k < nOut; k++) { O[base + 2 * k] = yr[k] * scale; O[base + 2 * k + 1] = yi[k] * scale; } }
        }
        return out;
    }
    // ---- torch.linalg's factorizations ----------------------------------------------------------
    // linalg(op, inputs, dims): a batch of dense factorizations run
    // natively (`vm::py_tensor::linalg`) into float64 storages sized here,
    // or null when the native kernel declines (or the native loops are
    // off); torch_linalg.py then runs its own Python algorithms, which the
    // native ones match to its documented tolerances. ops: 1 lu, 2 solve,
    // 3 triangular solve, 4 cholesky, 5 qr, 6 eigh, 7 svd, 8 eig.
    const LINALG_OUTS = {
        1: (d) => [d[0] * d[1] * d[2], d[0] * d[1], d[0] * Math.min(d[1], d[2]), d[0], d[0]],
        2: (d) => [d[0] * d[1] * d[2], d[0]],
        3: (d) => [d[0] * d[1] * d[2]],
        4: (d) => [d[0] * d[1] * d[1], d[0]],
        5: (d) => [d[0] * d[1] * d[3], d[0] * d[4] * d[2]],
        6: (d) => [d[0] * d[1], d[0] * d[1] * d[1]],
        7: (d) => { const k = Math.min(d[1], d[2]); return [d[0] * d[1] * (d[3] ? d[1] : k), d[0] * k, d[0] * (d[3] ? d[2] : k) * d[2]]; },
        8: (d) => [d[0] * d[1], d[0] * d[1], d[0] * d[1] * d[1], d[0] * d[1] * d[1], d[0]],
    };
    function linalg(op, ins, dims) {
        const f = LINALG_OUTS[op];
        if (f === undefined) fail(E.ValueError, "unknown linalg op " + op);
        if (NATIVE === null) return null;
        const outs = f(dims).map((n) => alloc("float64", n));
        const args = [N_LINALG, op, ins.length, outs.length];
        for (let i = 0; i < ins.length; i++) args.push(ins[i].data);
        for (let i = 0; i < outs.length; i++) args.push(outs[i].data);
        for (let i = 0; i < dims.length; i++) args.push(dims[i]);
        return NATIVE(...args) ? list(outs) : null;
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
    function rand(g, n, dtype) {
        // float32 uniform from 24 random bits, as torch's uniform_ for float;
        // float16 from 11 and bfloat16 from 8 (their mantissa digits).
        const s = needGen(g), out = work(dtype, n), O = out.data;
        if (dtype === "float16") for (let i = 0; i < n; i++) O[i] = (next32(s) & 0x7ff) * 0.00048828125;
        else if (dtype === "bfloat16") for (let i = 0; i < n; i++) O[i] = (next32(s) & 0xff) * 0.00390625;
        else for (let i = 0; i < n; i++) O[i] = (next32(s) & 0xffffff) * 5.9604644775390625e-8;
        return finish(out);
    }
    function randDouble(g, n) {
        const s = needGen(g), out = alloc("float64", n), O = out.data;
        for (let i = 0; i < n; i++) O[i] = nextDouble(s);
        return out;
    }
    function randn(g, n, dtype) {
        // Box-Muller on doubles; pairs, as torch's normal_ (scalar path).
        const s = needGen(g), out = work(dtype, n), O = out.data;
        for (let i = 0; i < n; i += 2) {
            const u1 = 1 - nextDouble(s), u2 = nextDouble(s);
            const r = Math.sqrt(-2 * Math.log(u1)), t = 2 * Math.PI * u2;
            O[i] = r * Math.cos(t);
            if (i + 1 < n) O[i + 1] = r * Math.sin(t);
        }
        { if (HALF[out.dtype] === 1) finish(out); return out; }
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
        const out = alloc("int64", rows * samples), O = out.data, P = vals(probs);
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
        // A complex storage's interleaved pairs are PyTorch's layout.
        if (a.dtype === "float32" || a.dtype === "complex64") bytes = new Uint8Array(Float32Array.from(a.data).buffer);
        else if (a.dtype === "float64" || a.dtype === "complex128") bytes = new Uint8Array(Float64Array.from(a.data).buffer);
        else if (a.dtype === "int64") { const b = new ArrayBuffer(n * 8), v = new DataView(b); for (let i = 0; i < n; i++) v.setBigInt64(i * 8, BigInt(Math.trunc(a.data[i])), true); bytes = new Uint8Array(b); }
        else if (a.dtype === "int32") { const b = new ArrayBuffer(n * 4), v = new DataView(b); for (let i = 0; i < n; i++) v.setInt32(i * 4, a.data[i], true); bytes = new Uint8Array(b); }
        else if (a.dtype === "int16") bytes = new Uint8Array(Int16Array.from(a.data).buffer);
        // float16 and bfloat16 storages hold PyTorch's 2-byte elements already.
        else if (a.dtype === "float16" || a.dtype === "bfloat16") bytes = new Uint8Array(a.data.buffer, a.data.byteOffset, n * 2).slice();
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
        if (dtype === "complex64") { const m = n === undefined ? items.length / 8 : n, out = calloc("complex64", m); for (let i = 0; i < 2 * m; i++) out.data[i] = view.getFloat32(i * 4, true); return out; }
        if (dtype === "complex128") { const m = n === undefined ? items.length / 16 : n, out = calloc("complex128", m); for (let i = 0; i < 2 * m; i++) out.data[i] = view.getFloat64(i * 8, true); return out; }
        if (dtype === "int64") { const m = n === undefined ? items.length / 8 : n, out = alloc("int64", m); for (let i = 0; i < m; i++) out.data[i] = Number(view.getBigInt64(i * 8, true)); return out; }
        if (dtype === "int32") { const m = n === undefined ? items.length / 4 : n, out = alloc("int32", m); for (let i = 0; i < m; i++) out.data[i] = view.getInt32(i * 4, true); return out; }
        if (dtype === "int16") { const m = n === undefined ? items.length / 2 : n, out = alloc("int16", m); for (let i = 0; i < m; i++) out.data[i] = view.getInt16(i * 2, true); return out; }
        if (dtype === "int8") { const m = n === undefined ? items.length : n, out = alloc("int8", m); for (let i = 0; i < m; i++) out.data[i] = view.getInt8(i); return out; }
        if (dtype === "float16" || dtype === "bfloat16") {
            // The 2-byte elements as they are (bits, NaN payloads included).
            const m = n === undefined ? items.length / 2 : n, out = alloc(dtype, m), h = new Uint16Array(out.data.buffer);
            for (let i = 0; i < m; i++) h[i] = view.getUint16(i * 2, true);
            return out;
        }
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
    // The graph protocol's `uniform` draw, from the two keys zipp_gpu derives
    // from (seed, step): element i is (mix(mix(i ^ k2) + k1) >>> 8) * 2^-24,
    // mix being lowbias32, all modulo 2^32, so the bits equal every backend's.
    function graphUniformMix(x) {
        x = (x ^ (x >>> 16)) >>> 0; x = Math.imul(x, 0x7feb352d) >>> 0;
        x = (x ^ (x >>> 15)) >>> 0; x = Math.imul(x, 0x846ca68b) >>> 0;
        return (x ^ (x >>> 16)) >>> 0;
    }
    function graphUniform(n, k1, k2) {
        const out = alloc("float32", n), O = out.data;
        for (let i = 0; i < n; i++) O[i] = (graphUniformMix((graphUniformMix((i ^ k2) >>> 0) + k1) >>> 0) >>> 8) * 5.9604644775390625e-8;
        return out;
    }
    // Graph protocol version 4: selection and its gradients, in the order
    // zipp_gpu's reference and every host backend use. `dims`/`strides` are
    // four padded dimensions; a box's strides may be negative. The two
    // accumulations add each contribution onto the base in ascending index
    // position, one float32 rounding per addition (the Float32Array store).
    function graphSlice(a, dims, st, offset) {
        const out = alloc("float32", dims[0] * dims[1] * dims[2] * dims[3]), O = out.data, A = a.data;
        let i = 0;
        for (let x0 = 0; x0 < dims[0]; x0++) for (let x1 = 0; x1 < dims[1]; x1++) for (let x2 = 0; x2 < dims[2]; x2++) {
            const base = offset + x0 * st[0] + x1 * st[1] + x2 * st[2];
            for (let x3 = 0; x3 < dims[3]; x3++) O[i++] = A[base + x3 * st[3]];
        }
        return out;
    }
    function graphSliceScatter(b, src, dims, st, offset) {
        const out = alloc("float32", b.data.length), O = out.data, S = src.data;
        O.set(b.data);
        let i = 0;
        for (let x0 = 0; x0 < dims[0]; x0++) for (let x1 = 0; x1 < dims[1]; x1++) for (let x2 = 0; x2 < dims[2]; x2++) {
            const base = offset + x0 * st[0] + x1 * st[1] + x2 * st[2];
            for (let x3 = 0; x3 < dims[3]; x3++) O[base + x3 * st[3]] = S[i++];
        }
        return out;
    }
    function graphIndexSelect(a, index, outer, len, count, inner) {
        const out = alloc("float32", outer * count * inner), O = out.data, A = a.data, I = index.data;
        for (let o = 0, at = 0; o < outer; o++) for (let k = 0; k < count; k++) {
            const from = (o * len + I[k]) * inner;
            for (let r = 0; r < inner; r++) O[at++] = A[from + r];
        }
        return out;
    }
    function graphIndexAdd(b, src, index, outer, len, count, inner) {
        const out = alloc("float32", b.data.length), O = out.data, S = src.data, I = index.data;
        O.set(b.data);
        for (let o = 0; o < outer; o++) for (let k = 0; k < count; k++) {
            const to = (o * len + I[k]) * inner, from = (o * count + k) * inner;
            for (let r = 0; r < inner; r++) O[to + r] += S[from + r];
        }
        return out;
    }
    function graphGather(a, index, dims, st, axisStride) {
        const out = alloc("float32", index.data.length), O = out.data, A = a.data, I = index.data;
        let i = 0;
        for (let x0 = 0; x0 < dims[0]; x0++) for (let x1 = 0; x1 < dims[1]; x1++) for (let x2 = 0; x2 < dims[2]; x2++) {
            const base = x0 * st[0] + x1 * st[1] + x2 * st[2];
            for (let x3 = 0; x3 < dims[3]; x3++, i++) O[i] = A[base + x3 * st[3] + I[i] * axisStride];
        }
        return out;
    }
    // Row-major over the index: two elements landing on one output differ
    // only along the axis, so they arrive in ascending position.
    function graphScatterAdd(b, src, index, dims, st, axisStride) {
        const out = alloc("float32", b.data.length), O = out.data, S = src.data, I = index.data;
        O.set(b.data);
        let i = 0;
        for (let x0 = 0; x0 < dims[0]; x0++) for (let x1 = 0; x1 < dims[1]; x1++) for (let x2 = 0; x2 < dims[2]; x2++) {
            const base = x0 * st[0] + x1 * st[1] + x2 * st[2];
            for (let x3 = 0; x3 < dims[3]; x3++, i++) O[base + x3 * st[3] + I[i] * axisStride] += S[i];
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
        const d = vals(s), n = d.length;
        if (n >= NATIVE_MIN && NATIVE !== null) { const r = NATIVE(N_ALL_FINITE, d); if (r !== null) return r; }
        for (let i = 0; i < n; i++) { const x = d[i]; if (x - x !== 0) return false; }
        return true;
    }
    // ---- the module --------------------------------------------------------------------------------
    // ---- sparse COO/CSR (torch.sparse) -------------------------------------------------
    // A sparse tensor is an int64 index storage and a values storage whose
    // nonzero k is the contiguous block [k * block, (k + 1) * block) (the
    // dense dims of a hybrid tensor, 1 otherwise). Entries are compared by
    // their row-major linear key over the sparse dims (`spKeys`), which
    // orders in-range indices as PyTorch's lexicographic compares do.
    //
    // y += x on one element of a storage of `dt`, as PyTorch's cpublas axpy
    // for that type stores it: the typed array rounds (float32/float16) or
    // wraps (uint8/int8/int16) on store; bool is a logical or; bfloat16
    // (bits) adds in float and rounds.
    function spAcc(dt) {
        if (dt === "bool") return (O, d, x) => { O[d] = (O[d] !== 0 || x !== 0) ? 1 : 0; };
        if (dt === "bfloat16") return (O, d, x) => { O[d] = bfBits(Math.fround(bfValue(O[d]) + bfValue(x))); };
        return null;
    }
    // The stable ascending order of the keys K (positions; equal keys keep
    // their order): a native numeric sort of key * n + position when that is
    // exact in a double, the merge sort otherwise.
    function stableOrder(K) {
        const n = K.length, perm = new Float64Array(n);
        let lo = 0, hi = 0;
        for (let i = 0; i < n; i++) { const k = K[i]; if (k < lo) lo = k; if (k > hi) hi = k; }
        if (lo >= 0 && (hi + 1) * n <= 9007199254740991) {
            const enc = new Float64Array(n);
            for (let i = 0; i < n; i++) enc[i] = K[i] * n + i;
            enc.sort();
            for (let i = 0; i < n; i++) perm[i] = enc[i] % n;
            return perm;
        }
        for (let i = 0; i < n; i++) perm[i] = i;
        mergeSort(perm, new Float64Array(n), K, n, false);
        return perm;
    }
    // sp_keys(indices, nnz, sizes): each nonzero's linear key over the
    // sparse dims (indices is [len(sizes), nnz], row-major).
    function spKeys(ind, nnz, sizes) {
        const I = ind.data, sd = sizes.length, out = alloc("int64", nnz), O = out.data;
        if (I.length !== sd * nnz) fail(E.RuntimeError, "sparse indices: size mismatch");
        if (NATIVE !== null && nnz >= NATIVE_MIN && NATIVE(N_SP_KEYS, I, O, nnz, sizes)) return out;
        let stride = 1;
        for (let d = sd - 1; d >= 0; d--) {
            const base = d * nnz;
            for (let k = 0; k < nnz; k++) O[k] += I[base + k] * stride;
            stride *= sizes[d];
        }
        return out;
    }
    // sp_coalesce(keys, values, block): PyTorch's _coalesce_sparse_cpu. The
    // nonzeros in stable key order; a run of equal keys becomes one entry,
    // its values copied from the first and the rest added in that order.
    // Returns (first, values): the original position of each entry's first
    // nonzero (to gather its indices) and the summed values.
    function spCoalesce(keys, v, block) {
        const K = keys.data, n = K.length, V = v.data, dt = v.dtype, acc = spAcc(dt);
        if (V.length !== n * block) fail(E.RuntimeError, "sparse values: size mismatch");
        if (NATIVE !== null && acc === null && n * block >= NATIVE_MIN) {
            // The native loop sorts and sums into outputs of the input's
            // size and reports how many entries it kept.
            const F = new Float64Array(n), O = new V.constructor(n * block), G = new Float64Array(1);
            if (NATIVE(N_SP_COALESCE, K, V, F, O, G, n, block)) {
                const groups = G[0], first = alloc("int64", groups);
                first.data.set(F.subarray(0, groups));
                return tuple([first, make(dt, O.slice(0, groups * block))]);
            }
        }
        const perm = stableOrder(K);
        let groups = 0;
        for (let j = 0; j < n; j++) if (j === 0 || K[perm[j]] !== K[perm[j - 1]]) groups++;
        const first = alloc("int64", groups), F = first.data;
        const out = make(dt, new V.constructor(groups * block)), O = out.data;
        let g = -1, prev = 0;
        for (let j = 0; j < n; j++) {
            const p = perm[j], k = K[p], src = p * block;
            if (j === 0 || k !== prev) {
                g++;
                F[g] = p;
                const dst = g * block;
                for (let b = 0; b < block; b++) O[dst + b] = V[src + b];
            } else {
                const dst = g * block;
                if (acc === null) for (let b = 0; b < block; b++) O[dst + b] = O[dst + b] + V[src + b];
                else for (let b = 0; b < block; b++) acc(O, dst + b, V[src + b]);
            }
            prev = k;
        }
        return tuple([first, out]);
    }
    // sp_merge(tkeys, tvalues, skeys, svalues, block, alpha): PyTorch's
    // add_out_sparse_contiguous, t + alpha * s. One pass over both lists as
    // if each were sorted: the smaller key goes out first, equal keys are
    // summed. Returns (take, values): for each result entry the position of
    // its indices in cat([t_indices, s_indices], 1), and its values (t's,
    // then alpha * s's added, into zeros).
    function spMerge(tk, tv, sk, sv, block, alpha) {
        const TK = tk.data, SK = sk.data, tn = TK.length, sn = SK.length;
        const TV = tv.data, SV = sv.data, dt = tv.dtype, acc = spAcc(dt);
        if (sv.dtype !== dt) fail(E.RuntimeError, "sparse add: values must share a dtype");
        const take = new Float64Array(tn + sn), O = new TV.constructor((tn + sn) * block);
        const a = castValue(dt === "bfloat16" ? "float32" : dt, alpha), scaled = alpha !== 1;
        if (NATIVE !== null && (dt === "float32" || dt === "float64") && (tn + sn) * block >= NATIVE_MIN) {
            const G = new Float64Array(1);
            if (NATIVE(N_SP_MERGE, TK, TV, SK, SV, take, O, G, tn, sn, block, a)) {
                const r = G[0], t = alloc("int64", r);
                t.data.set(take.subarray(0, r));
                return tuple([t, make(dt, O.slice(0, r * block))]);
            }
        }
        let r = 0, i = 0, j = 0;
        while (i < tn || j < sn) {
            const cmp = i >= tn ? -1 : j >= sn ? 1 : (TK[i] < SK[j] ? 1 : TK[i] > SK[j] ? -1 : 0);
            const dst = r * block;
            if (cmp >= 0) {
                take[r] = i;
                const src = i * block;
                if (acc === null) for (let b = 0; b < block; b++) O[dst + b] = O[dst + b] + TV[src + b];
                else for (let b = 0; b < block; b++) acc(O, dst + b, TV[src + b]);
                i++;
            }
            if (cmp <= 0) {
                take[r] = tn + j;
                const src = j * block;
                for (let b = 0; b < block; b++) {
                    let x = SV[src + b];
                    if (scaled) x = dt === "bfloat16" ? bfBits(Math.fround(a * bfValue(x))) : castValue(dt, a * x);
                    if (acc === null) O[dst + b] = O[dst + b] + x; else acc(O, dst + b, x);
                }
                j++;
            }
            r++;
        }
        const t = alloc("int64", r); t.data.set(take.subarray(0, r));
        return tuple([t, make(dt, O.slice(0, r * block))]);
    }
    // sp_spmm(rows, cols, values, m, dense, n): the [m, n] product of the
    // sparse [m, k] matrix (nonzero e at (rows[e], cols[e])) and the dense
    // [k, n] one. Each result row sums its products in nonzero order in a
    // double and rounds once to the values' dtype.
    function spSpmm(rows, cols, v, m, dense, n) {
        const Rw = rows.data, C = cols.data, V = vals(v), D = vals(dense), nnz = Rw.length;
        const out = work(v.dtype, m * n), O = out.data;
        const k = n === 0 ? 0 : D.length / n;
        if (NATIVE !== null && nnz * n >= NATIVE_MIN && NATIVE(N_SPMM, Rw, C, V, D, O, nnz, m, k, n)) { if (HALF[out.dtype] === 1) finish(out); return out; }
        const acc = new Float64Array(m * n);
        for (let e = 0; e < nnz; e++) {
            const r = Rw[e], c = C[e], x = V[e];
            if (r < 0 || r >= m || c < 0 || c >= k) fail(E.RuntimeError, "sparse mm: index out of bounds");
            const ro = r * n, co = c * n;
            for (let j = 0; j < n; j++) acc[ro + j] += x * D[co + j];
        }
        if (out.dtype === "bool") for (let i = 0; i < acc.length; i++) O[i] = acc[i] !== 0 ? 1 : 0;
        else O.set(acc);
        if (HALF[out.dtype] === 1) finish(out);
        return out;
    }
    // sp_search(sorted, queries): the position of each query key in the
    // ascending keys `sorted`, or -1.
    function spSearch(sorted, queries) {
        const S = sorted.data, Q = queries.data, n = S.length, out = alloc("int64", Q.length), O = out.data;
        let ascending = true;
        for (let i = 1; i < Q.length; i++) if (Q[i] < Q[i - 1]) { ascending = false; break; }
        if (ascending) {
            for (let i = 0, j = 0; i < Q.length; i++) {
                const q = Q[i];
                while (j < n && S[j] < q) j++;
                O[i] = j < n && S[j] === q ? j : -1;
            }
            return out;
        }
        for (let i = 0; i < Q.length; i++) {
            const q = Q[i];
            let lo = 0, hi = n;
            while (lo < hi) { const mid = (lo + hi) >>> 1; if (S[mid] < q) lo = mid + 1; else hi = mid; }
            O[i] = lo < n && S[lo] === q ? lo : -1;
        }
        return out;
    }
    // sp_pool(op, pools, values, block): per pool of nonzeros sharing a key
    // (softmax's rows), each of the `block` dense columns: 0 softmax, 1
    // log_softmax, 2 the pool's sum at every member (softmax backward).
    // Computed in doubles, rounded once to the values' dtype.
    function spPool(op, pools, v, block) {
        const P = pools.data, n = P.length, V = vals(v), out = work(v.dtype, n * block), O = out.data;
        const gid = new Float64Array(n);
        let groups = 0, runs = true;
        for (let e = 1; e < n; e++) if (P[e] < P[e - 1]) { runs = false; break; }
        if (runs) {
            // Ascending keys (a coalesced tensor's rows): each run is a pool.
            for (let e = 0; e < n; e++) { if (e === 0 || P[e] !== P[e - 1]) groups++; gid[e] = groups - 1; }
        } else {
            const ids = new Map();
            for (let e = 0; e < n; e++) { let g = ids.get(P[e]); if (g === undefined) { g = groups++; ids.set(P[e], g); } gid[e] = g; }
        }
        const mx = new Float64Array(groups * block), sum = new Float64Array(groups * block);
        if (op === 2) {
            for (let e = 0; e < n; e++) { const g = gid[e] * block; for (let b = 0; b < block; b++) sum[g + b] += V[e * block + b]; }
            for (let e = 0; e < n; e++) { const g = gid[e] * block; for (let b = 0; b < block; b++) O[e * block + b] = sum[g + b]; }
        } else {
            mx.fill(-Infinity);
            for (let e = 0; e < n; e++) { const g = gid[e] * block; for (let b = 0; b < block; b++) { const x = V[e * block + b]; if (x > mx[g + b] || x !== x) mx[g + b] = x; } }
            for (let e = 0; e < n; e++) { const g = gid[e] * block; for (let b = 0; b < block; b++) sum[g + b] += Math.exp(V[e * block + b] - mx[g + b]); }
            for (let e = 0; e < n; e++) {
                const g = gid[e] * block;
                for (let b = 0; b < block; b++) {
                    const z = V[e * block + b] - mx[g + b];
                    O[e * block + b] = op === 1 ? z - Math.log(sum[g + b]) : Math.exp(z) / sum[g + b];
                }
            }
        }
        if (HALF[out.dtype] === 1) finish(out);
        return out;
    }
    // sp_index_select(dimvals, index, size): for each entry of `index` in
    // turn, the nonzeros whose index along the selected dim is that value,
    // in their order. Returns (positions, new index along the dim).
    function spIndexSelect(dv, index, size) {
        const D = dv.data, X = index.data, where = new Map();
        for (let e = 0; e < D.length; e++) { const k = D[e]; let l = where.get(k); if (l === undefined) { l = []; where.set(k, l); } l.push(e); }
        const pos = [], nv = [];
        for (let i = 0; i < X.length; i++) {
            let j = X[i];
            if (j < -size || j >= size) fail(E.IndexError, "index out of range in self");
            if (j < 0) j += size;
            const l = where.get(j);
            if (l !== undefined) for (let q = 0; q < l.length; q++) { pos.push(l[q]); nv.push(i); }
        }
        const p = alloc("int64", pos.length), q = alloc("int64", nv.length);
        for (let i = 0; i < pos.length; i++) { p.data[i] = pos[i]; q.data[i] = nv[i]; }
        return tuple([p, q]);
    }
    // sp_compress(rows, n): the compressed (CSR crow) indices [n + 1] of
    // row indices sorted ascending in [0, n).
    function spCompress(rows, n) {
        const R = rows.data, out = alloc("int64", n + 1), O = out.data;
        for (let e = 0; e < R.length; e++) {
            const r = R[e];
            if (r < 0 || r >= n) fail(E.RuntimeError, "sparse compress: row index out of range");
            O[r + 1]++;
        }
        for (let i = 0; i < n; i++) O[i + 1] += O[i];
        return out;
    }
    // sp_expand(crow): each entry's row index, from compressed indices.
    function spExpand(crow) {
        const C = crow.data, n = C.length - 1, nnz = n >= 0 ? C[n] : 0;
        if (!(nnz >= 0)) fail(E.RuntimeError, "sparse expand: invalid compressed indices");
        const out = alloc("int64", nnz), O = out.data;
        for (let i = 0; i < n; i++) {
            const lo = C[i], hi = C[i + 1];
            if (lo > hi || hi > nnz) fail(E.RuntimeError, "sparse expand: compressed indices must be non-decreasing and end at nnz");
            for (let e = lo; e < hi; e++) O[e] = i;
        }
        return out;
    }
    // sp_compact(s): the positions of the nonzero elements of s, ascending.
    function spCompact(s) {
        const S = vals(s), keep = [];
        for (let i = 0; i < S.length; i++) if (S[i] !== 0) keep.push(i);
        const out = alloc("int64", keep.length), O = out.data;
        for (let i = 0; i < keep.length; i++) O[i] = keep[i];
        return out;
    }
    // sp_nonzero(s, shape, sd): the [sd, nnz] indices, in row-major order, of
    // the positions over the first sd dims whose block of trailing elements
    // has a nonzero (a NaN counts), as to_sparse(sd) finds them.
    function spNonzero(s, shape, sd) {
        const S = vals(s), outer = numel(shape.slice(0, sd)), block = numel(shape.slice(sd)), hits = [];
        for (let p = 0; p < outer; p++) {
            const base = p * block;
            for (let b = 0; b < block; b++) if (S[base + b] !== 0) { hits.push(p); break; }
        }
        const n = hits.length, out = alloc("int64", sd * n), O = out.data;
        for (let k = 0; k < n; k++) {
            let rest = hits[k];
            for (let d = sd - 1; d >= 0; d--) { const q = Math.floor(rest / shape[d]); O[d * n + k] = rest - q * shape[d]; rest = q; }
        }
        return out;
    }
    // sp_spgemm(arows, acols, avals, bcrow, bcols, bvals): the products of a
    // sparse a (entries in row-major order) and b (compressed by row), as
    // Gustavson's algorithm reaches them: for each entry (i, j) of a, the
    // entries of b's row j. Returns (rows, cols, values), not summed.
    function spSpgemm(ar, ac, av, bc, bcol, bv) {
        const AR = ar.data, AC = ac.data, AV = vals(av), BC = bc.data, BK = bcol.data, BV = vals(bv), k = BC.length - 1;
        let total = 0;
        for (let e = 0; e < AR.length; e++) { const j = AC[e]; if (j < 0 || j >= k) fail(E.RuntimeError, "sparse mm: index out of bounds"); total += BC[j + 1] - BC[j]; }
        const rows = alloc("int64", total), cols = alloc("int64", total), out = work(av.dtype, total), R = rows.data, C = cols.data, O = out.data;
        let t = 0;
        for (let e = 0; e < AR.length; e++) {
            const i = AR[e], j = AC[e], x = AV[e];
            for (let q = BC[j]; q < BC[j + 1]; q++, t++) { R[t] = i; C[t] = BK[q]; O[t] = x * BV[q]; }
        }
        if (HALF[out.dtype] === 1) finish(out);
        return tuple([rows, cols, out]);
    }
    // sp_scatter_add(dst, keys, values, block): each nonzero's block of
    // values added into dst at key * block, in nonzero order, each sum
    // stored as `scatter_add` stores it (to_dense, dense + sparse).
    function spScatterAdd(dst, keys, v, block) {
        const O = dst.data, K = keys.data, V = vals(v), dt = dst.dtype, n = K.length, size = O.length;
        if (V.length !== n * block) fail(E.RuntimeError, "sparse scatter: size mismatch");
        const f32 = dt === "float32" || dt === "float64", bf = dt === "bfloat16";
        if (NATIVE !== null && f32 && n * block >= NATIVE_MIN && NATIVE(N_SP_SCATTER, O, K, V, n, block)) { written(dst); return null; }
        for (let e = 0; e < n; e++) {
            const base = K[e] * block, src = e * block;
            if (!(base >= 0 && base + block <= size)) fail(E.IndexError, "index out of bounds");
            if (f32) for (let b = 0; b < block; b++) O[base + b] = O[base + b] + V[src + b];
            else if (bf) for (let b = 0; b < block; b++) O[base + b] = bfBits(Math.fround(bfValue(O[base + b]) + V[src + b]));
            else for (let b = 0; b < block; b++) O[base + b] = castValue(dt, O[base + b] + V[src + b]);
        }
        written(dst);
        return null;
    }
    rt.defineModule("_zipp_tensor", (g) => {
        const fn = (name, arity, code, min) => g.set(name, rt.builtin(name, arity, code, min));
        const num = (v) => jsNumber(v);
        fn("sp_scatter_add", 4, (a) => {
            const d = a[0], v = a[2], block = num(a[3]);
            if (isStorage(d) && isStorage(v)) return spScatterAdd(d, needS(a[1]), v, block);
            needC(d); needC(v);
            if (d.dtype !== v.dtype) fail(E.RuntimeError, "sparse scatter: complex storages must share a dtype");
            spScatterAdd(realOf(d), needS(a[1]), realOf(v), 2 * block);
            written(d);
            return null;
        });
        fn("sp_compress", 2, (a) => spCompress(needS(a[0]), num(a[1])));
        fn("sp_expand", 1, (a) => spExpand(needS(a[0])));
        fn("sp_compact", 1, (a) => spCompact(needS(a[0])));
        fn("sp_nonzero", 3, (a) => spNonzero(needS(a[0]), shapeOf(a[1]), num(a[2])));
        fn("sp_spgemm", 6, (a) => spSpgemm(needS(a[0]), needS(a[1]), needS(a[2]), needS(a[3]), needS(a[4]), needS(a[5])));
        // Sparse kernels (see `spKeys` ...). A complex values storage runs as
        // its real pairs (block doubled): coalescing and merging only add.
        fn("sp_keys", 3, (a) => spKeys(needS(a[0]), num(a[1]), shapeOf(a[2])));
        fn("sp_coalesce", 3, (a) => {
            const v = a[1], block = num(a[2]);
            if (isStorage(v)) return spCoalesce(needS(a[0]), v, block);
            needC(v);
            const r = spCoalesce(needS(a[0]), realOf(v), 2 * block);
            return tuple([r.items[0], asPair(r.items[1], v.dtype)]);
        });
        fn("sp_merge", 6, (a) => {
            const tv = a[1], sv = a[3], block = num(a[4]), alpha = num(a[5]);
            if (isStorage(tv) && isStorage(sv)) return spMerge(needS(a[0]), tv, needS(a[2]), sv, block, alpha);
            needC(tv); needC(sv);
            if (tv.dtype !== sv.dtype || alpha !== 1) fail(E.RuntimeError, "sparse add: complex values need one dtype and alpha 1");
            const r = spMerge(needS(a[0]), realOf(tv), needS(a[2]), realOf(sv), 2 * block, 1);
            return tuple([r.items[0], asPair(r.items[1], tv.dtype)]);
        });
        fn("sp_spmm", 6, (a) => spSpmm(needS(a[0]), needS(a[1]), needS(a[2]), num(a[3]), needS(a[4]), num(a[5])));
        fn("sp_search", 2, (a) => spSearch(needS(a[0]), needS(a[1])));
        fn("sp_pool", 4, (a) => spPool(num(a[0]), needS(a[1]), needS(a[2]), num(a[3])));
        fn("sp_index_select", 3, (a) => spIndexSelect(needS(a[0]), needS(a[1]), num(a[2])));
        fn("zeros", 2, (a) => { const d = rt.needStr(a[0]); return d.length > 8 ? calloc(d, num(a[1])) : alloc(d, num(a[1])); });
        fn("full", 3, (a) => { const d = rt.needStr(a[0]); if (d.length > 8) { const c = calloc(d, num(a[1])); fillPair(c, num(a[2]), 0); return c; } const s = alloc(d, num(a[1])); s.data.fill(enc(s.dtype, num(a[2]))); return s; });
        // arange(dtype, start, step, n): element i is castValue(start + i * step),
        // what from_flat gives for the list torch builds (torch.arange checks
        // that the double arithmetic is exact or is Python's float arithmetic).
        fn("arange", 4, (a) => { const s = alloc(rt.needStr(a[0]), num(a[3])), O = s.data, d = s.dtype, st = num(a[1]), step = num(a[2]), n = O.length, cv = encoder(d); for (let i = 0; i < n; i++) O[i] = cv(d, st + i * step); return s; });
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
        fn("from_flat", 2, (a) => {
            const items = a[1].items, s = anyAlloc(rt.needStr(a[0]), items.length), cv = encoder(s.dtype);
            // A complex dtype takes real values (zero imaginary parts).
            if (s.cls === CStorage) { for (let i = 0; i < items.length; i++) s.data[2 * i] = jsNumber(items[i]); return s; }
            for (let i = 0; i < items.length; i++) s.data[i] = cv(s.dtype, jsNumber(items[i])); return s;
        });
        // from_pairs(dtype, flat): a complex storage from interleaved
        // (real, imaginary) numbers.
        fn("from_pairs", 2, (a) => {
            const items = a[1].items, d = rt.needStr(a[0]);
            if (PAIR[d] === undefined || (items.length & 1)) fail(E.TypeError, "from_pairs needs a complex dtype and an even count");
            const s = calloc(d, items.length >> 1);
            for (let i = 0; i < items.length; i++) s.data[i] = jsNumber(items[i]);
            return s;
        });
        // as_real(s) / as_complex(s): the same memory as the real dtype
        // with a trailing dimension of 2, or back (view_as_real /
        // view_as_complex); both share `s`'s version counter.
        fn("as_real", 1, (a) => { const s = needSC(a[0]); if (s.cls !== CStorage) fail(E.TypeError, "as_real needs a complex storage"); const v = make(PAIR[s.dtype], s.data); v.base = root(s); return v; });
        fn("as_complex", 1, (a) => { const s = needSC(a[0]), d = COMPLEX_OF[s.dtype]; if (d === undefined || (s.data.length & 1)) fail(E.TypeError, "as_complex needs a float32/float64 storage of even size"); const v = cmake(d, s.data); v.base = root(s); return v; });
        // fill_complex(s, re, im): every element set to re + im j.
        fn("fill_complex", 3, (a) => { const s = needSC(a[0]); if (s.cls !== CStorage) fail(E.TypeError, "fill_complex needs a complex storage"); fillPair(s, num(a[1]), num(a[2])); written(s); return null; });
        fn("to_list", 1, (a) => {
            const s = needS(a[0]), d = vals(s), out = new Array(d.length);
            if (isFloatDtype(s.dtype)) { for (let i = 0; i < out.length; i++) out[i] = d[i]; }
            else { for (let i = 0; i < out.length; i++) out[i] = pyNumber(s.dtype, d[i]); }
            return list(out);
        });
        fn("item", 2, (a) => { const s = needS(a[0]), i = num(a[1]); return pyNumber(s.dtype, s.dtype === "bfloat16" && s.data[i] !== undefined ? bfValue(s.data[i]) : s.data[i]); });
        fn("setitem", 3, (a) => { const s = needS(a[0]); s.data[num(a[1])] = enc(s.dtype, num(a[2])); written(s); return null; });
        fn("copy", 1, (a) => { const s = a[0]; if (isStorage(s)) return make(s.dtype, s.data.slice()); needC(s); return cmake(s.dtype, s.data.slice()); });
        fn("astype", 2, (a) => { const s = a[0], d = rt.needStr(a[1]); return isStorage(s) && d.length <= 8 ? astype(s, d) : castPair(needC(s), d); });
        fn("dtype", 1, (a) => needSC(a[0]).dtype);
        fn("size", 1, (a) => { const s = a[0]; return BigInt(isStorage(s) ? s.data.length : count(needC(s))); });
        fn("version", 1, (a) => BigInt(root(needSC(a[0])).version));
        // Autograd's view of the version: writes made through `.data` (which
        // PyTorch gives a version counter of its own) are not counted, the
        // total `version` above still is.
        fn("aversion", 1, (a) => { const s = root(needSC(a[0])); return BigInt(s.version - s.untracked); });
        fn("untrack", 1, (a) => { root(needSC(a[0])).untracked++; return null; });
        fn("all_finite", 1, (a) => { const s = a[0]; return isStorage(s) ? allFinite(s) : allFinite(realOf(needC(s))); });
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
        // graph_uniform(n, k1, k2): the protocol's counter-based uniform draw.
        fn("graph_uniform", 3, (a) => graphUniform(num(a[0]), num(a[1]) >>> 0, num(a[2]) >>> 0));
        // Version 4 (see graphSlice): four-integer lists are padded dims and strides.
        const graphInts = (v) => { const it = v.items; return [num(it[0]), num(it[1]), num(it[2]), num(it[3])]; };
        fn("graph_slice", 4, (a) => graphSlice(needS(a[0]), graphInts(a[1]), graphInts(a[2]), num(a[3])));
        fn("graph_slice_scatter", 5, (a) => graphSliceScatter(needS(a[0]), needS(a[1]), graphInts(a[2]), graphInts(a[3]), num(a[4])));
        fn("graph_index_select", 6, (a) => graphIndexSelect(needS(a[0]), needS(a[1]), num(a[2]), num(a[3]), num(a[4]), num(a[5])));
        fn("graph_index_add", 7, (a) => graphIndexAdd(needS(a[0]), needS(a[1]), needS(a[2]), num(a[3]), num(a[4]), num(a[5]), num(a[6])));
        fn("graph_gather", 5, (a) => graphGather(needS(a[0]), needS(a[1]), graphInts(a[2]), graphInts(a[3]), num(a[4])));
        fn("graph_scatter_add", 6, (a) => graphScatterAdd(needS(a[0]), needS(a[1]), needS(a[2]), graphInts(a[3]), graphInts(a[4]), num(a[5])));
        fn("life", 3, (a) => life(needS(a[0]), num(a[1]), num(a[2])));
        fn("fill", 2, (a) => { const s = a[0]; if (isStorage(s)) s.data.fill(enc(s.dtype, num(a[1]))); else fillPair(needC(s), num(a[1]), 0); written(s); return null; });
        fn("copy_into", 2, (a) => {
            const d = a[0], s = a[1];
            if (!isStorage(d) || !isStorage(s)) {
                needC(d); needC(s);
                // Into a complex storage: a real source's values with zero
                // imaginary parts; a complex source is never written into
                // a real storage (the caller refuses that cast).
                if (d.cls !== CStorage) fail(E.RuntimeError, "a complex value cannot be written into a real storage");
                if (count(d) !== count(s)) fail(E.RuntimeError, "size mismatch");
                d.data.set(toPair(s, d.dtype).data);
                written(d);
                return null;
            }
            if (d.data.length !== s.data.length) fail(E.RuntimeError, "size mismatch");
            // A float storage of its own dtype holds values castValue leaves
            // unchanged, so a block copy is the same store.
            if (d.dtype === s.dtype && isFloatDtype(d.dtype)) d.data.set(s.data);
            else { const S = vals(s), D = d.data, dt = d.dtype, cv = encoder(dt); for (let i = 0; i < D.length; i++) D[i] = cv(dt, S[i]); }
            written(d);
            return null;
        });
        // binary(op, a, ashape, b, bshape, dtype=None): `dtype` is the result type promotion chose.
        // binary_scalar(op, a, ashape, value, sdtype, want, flip): `binary` with
        // a Python number for one operand, held in a 1-element `sdtype`
        // storage exactly as `full` stores it (`flip`: the number is the
        // left operand), so no 0-d tensor has to be built for it.
        fn("binary_scalar", 7, (a) => {
            const sd = rt.needStr(a[4]), t = a[1], want = a[5] === undefined || a[5] === null ? null : rt.needStr(a[5]);
            if (sd.length <= 8 && isStorage(t)) {
                const s = alloc(sd, 1);
                s.data[0] = enc(s.dtype, num(a[3]));
                return rt.truth(a[6]) ? binary(rt.needStr(a[0]), s, [], t, shapeOf(a[2]), undefined, a[2], want)
                    : binary(rt.needStr(a[0]), t, shapeOf(a[2]), s, [], a[2], undefined, want);
            }
            const s = anyAlloc(sd, 1);
            s.data[0] = enc(s.dtype, num(a[3]));
            needC(t);
            return rt.truth(a[6]) ? cbinary(rt.needStr(a[0]), s, [], t, shapeOf(a[2]), want) : cbinary(rt.needStr(a[0]), t, shapeOf(a[2]), s, [], want);
        });
        fn("binary", 6, (a) => {
            const x = a[1], y = a[3], want = a[5] === undefined || a[5] === null ? null : rt.needStr(a[5]);
            if (isStorage(x) && isStorage(y) && (want === null || want.length <= 8)) return binary(rt.needStr(a[0]), x, shapeOf(a[2]), y, shapeOf(a[4]), a[2], a[4], want);
            return cbinary(rt.needStr(a[0]), needC(x), shapeOf(a[2]), needC(y), shapeOf(a[4]), want);
        }, 5);
        fn("unary", 4, (a) => { const x = a[1]; return isStorage(x) ? unary(rt.needStr(a[0]), x, a[2] === undefined ? null : a[2], a[3] === undefined ? null : a[3]) : cunary(rt.needStr(a[0]), needC(x)); }, 2);
        fn("reduce", 6, (a) => { const x = a[1]; return isStorage(x) ? reduce(rt.needStr(a[0]), x, shapeOf(a[2]), a[3], rt.truth(a[4]), a[5] !== undefined && rt.truth(a[5])) : creduce(rt.needStr(a[0]), needC(x), shapeOf(a[2]), a[3], rt.truth(a[4]), a[5] !== undefined && rt.truth(a[5])); }, 5);
        // The copying kernels on a complex storage: the same kernel over
        // its real memory with a trailing dimension of 2.
        fn("permute", 3, (a) => {
            const x = a[0];
            if (isStorage(x)) { const sh = shapeOf(a[1]); return permute(x, sh, a[2]); }
            needC(x);
            const sh = shapeOf(a[1]);
            const p = ints(a[2]); p.push(sh.length);
            return lowered(permute(realOf(x), sh.concat([2]), pyInts(p)), x.dtype);
        });
        fn("expand", 3, (a) => {
            const x = a[0];
            if (isStorage(x)) { const sh = shapeOf(a[1]); return expand(x, sh, a[2]); }
            needC(x);
            const sh = shapeOf(a[1]);
            return asPair(expand(realOf(x), sh.concat([2]), pyInts(ints(a[2]).concat([2]))), x.dtype);
        });
        fn("slice", 3, (a) => {
            const x = a[0];
            if (isStorage(x)) { const sh = shapeOf(a[1]); return slice(x, sh, a[2]); }
            needC(x);
            const sh = shapeOf(a[1]);
            return lowered(slice(realOf(x), sh.concat([2]), a[2]), x.dtype);
        });
        fn("setslice", 5, (a) => {
            const x = a[0];
            if (isStorage(x)) { const sh = shapeOf(a[1]); return setSlice(x, sh, a[2], needS(a[3]), shapeOf(a[4])); }
            needC(x);
            const sh = shapeOf(a[1]);
            setSlice(realOf(x), sh.concat([2]), a[2], realOf(toPair(needSC(a[3]), x.dtype)), shapeOf(a[4]).concat([2]));
            written(x);
            return null;
        });
        fn("gather", 4, (a) => {
            const x = a[0];
            if (isStorage(x)) { const sh = shapeOf(a[1]); return gather(x, sh, a[2], a[3]); }
            needC(x);
            const sh = shapeOf(a[1]);
            return lowered(gather(realOf(x), sh.concat([2]), a[2], a[3]), x.dtype);
        });
        const scatterC = (f) => (a) => {
            const x = a[0];
            if (isStorage(x)) { const sh = shapeOf(a[1]); return f(x, sh, a[2], a[3], needS(a[4]), shapeOf(a[5])); }
            needC(x);
            const sh = shapeOf(a[1]);
            f(realOf(x), sh.concat([2]), a[2], a[3], realOf(toPair(needSC(a[4]), x.dtype)), shapeOf(a[5]).concat([2]));
            written(x);
            return null;
        };
        fn("scatter", 6, scatterC(scatter));
        fn("scatter_add", 6, scatterC(scatterAdd));
        fn("index_select", 4, (a) => {
            const x = a[0], d = num(a[2]);
            if (isStorage(x)) { const sh = shapeOf(a[1]); return indexSelect(x, sh, d, needS(a[3])); }
            needC(x);
            const sh = shapeOf(a[1]);
            return lowered(indexSelect(realOf(x), sh.concat([2]), d < 0 ? d + sh.length : d, needS(a[3])), x.dtype);
        });
        fn("cat", 2, (a) => {
            const items = a[0].items;
            let cdt = null;
            for (let i = 0; i < items.length; i++) { const st = items[i].items[0]; if (!isStorage(st) && isCStorage(st)) cdt = cdt === null ? st.dtype : promote(cdt, st.dtype); }
            if (cdt === null) return cat(a[0], num(a[1]));
            for (let i = 0; i < items.length; i++) cdt = promote(cdt, items[i].items[0].dtype);
            const parts = new Array(items.length);
            for (let i = 0; i < items.length; i++) parts[i] = tuple([realOf(toPair(items[i].items[0], cdt)), pyShape(shapeOf(items[i].items[1]).concat([2]))]);
            const rank = shapeOf(items[0].items[1]).length, d = num(a[1]);
            return lowered(cat(list(parts), d < 0 ? d + rank : d), cdt);
        });
        fn("roll", 4, (a) => {
            const x = a[0], d = num(a[3]);
            if (isStorage(x)) { const sh = shapeOf(a[1]); return roll(x, sh, num(a[2]), d); }
            needC(x);
            const sh = shapeOf(a[1]);
            return asPair(roll(realOf(x), sh.concat([2]), num(a[2]), d < 0 ? d + sh.length : d), x.dtype);
        });
        fn("pad_last", 5, (a) => padLast(needS(a[0]), shapeOf(a[1]), num(a[2]), num(a[3]), num(a[4])));
        fn("matmul", 5, (a) => {
            const x = a[0], y = a[2], transB = a[4] !== undefined && a[4] !== null && rt.truth(a[4]);
            if (!isStorage(x) || !isStorage(y)) {
                needC(x); needC(y);
                if (transB) fail(E.RuntimeError, "matmul: a transposed complex operand is not supported");
                return cmatmul(x, shapeOf(a[1]), y, shapeOf(a[3]));
            }
            return matmul(x, shapeOf(a[1]), y, shapeOf(a[3]), transB);
        }, 4);
        fn("max_pool2d", 3, (a) => maxPool2d(needS(a[0]), shapeOf(a[1]), ints(a[2])));
        fn("max_pool2d_backward", 4, (a) => maxPool2dBackward(needS(a[0]), needS(a[1]), shapeOf(a[2]), ints(a[3])));
        fn("conv1d", 5, (a) => conv1d(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3]), a[4] === null ? null : needS(a[4])));
        fn("conv1d_backward", 5, (a) => conv1dBackward(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3]), needS(a[4])));
        fn("conv2d", 9, (a) => conv2d(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3]), a[4] === null ? null : needS(a[4]), shapeOf(a[5]), shapeOf(a[6]), shapeOf(a[7]), num(a[8])));
        fn("conv2d_backward", 9, (a) => conv2d(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3]), null, shapeOf(a[5]), shapeOf(a[6]), shapeOf(a[7]), num(a[8]), needS(a[4])));
        fn("softmax", 4, (a) => softmax(needS(a[0]), shapeOf(a[1]), num(a[2]), rt.truth(a[3])));
        fn("argsort", 4, (a) => argsort(needS(a[0]), shapeOf(a[1]), num(a[2]), rt.truth(a[3])));
        fn("one_hot", 2, (a) => oneHot(needS(a[0]), num(a[1])));
        fn("where", 7, (a) => {
            const x = a[2], y = a[4], want = a[6] === undefined || a[6] === null ? null : rt.needStr(a[6]);
            if (isStorage(x) && isStorage(y) && (want === null || want.length <= 8))
                return where(needS(a[0]), shapeOf(a[1]), x, shapeOf(a[3]), y, shapeOf(a[5]), want);
            needC(x); needC(y);
            const cdt = want !== null && PAIR[want] !== undefined ? want : promote(x.dtype, y.dtype);
            return lowered(where(needS(a[0]), shapeOf(a[1]).concat([1]), realOf(toPair(x, cdt)), shapeOf(a[3]).concat([2]), realOf(toPair(y, cdt)), shapeOf(a[5]).concat([2]), PAIR[cdt]), cdt);
        }, 6);
        fn("scan", 4, (a) => { const x = a[1]; return isStorage(x) ? scan(rt.needStr(a[0]), x, shapeOf(a[2]), num(a[3])) : cscan(rt.needStr(a[0]), needC(x), shapeOf(a[2]), num(a[3])); });
        fn("strided", 4, (a) => {
            const x = a[0], sh = shapeOf(a[1]), st = shapeOf(a[2]), off = num(a[3]);
            if (isStorage(x)) return strided(x, sh, st, off);
            needC(x);
            return asPair(strided(realOf(x), sh.concat([2]), st.map((v) => 2 * v).concat([1]), 2 * off), x.dtype);
        });
        fn("gen_get_state", 1, (a) => genGetState(a[0]));
        fn("gen_set_state", 2, (a) => genSetState(a[0], a[1]));
        fn("allclose", 4, (a) => { const x = needS(a[0]), y = needS(a[1]); const rtol = num(a[2]), atol = num(a[3]); if (x.data.length !== y.data.length) return false; const X = vals(x), Y = vals(y); for (let i = 0; i < X.length; i++) { const p = X[i], q = Y[i]; if (p === q) continue; if (!Number.isFinite(p) || !Number.isFinite(q) || Math.abs(p - q) > atol + rtol * Math.abs(q)) return false; } return true; });
        fn("equal", 2, (a) => { const x = needSC(a[0]), y = needSC(a[1]); if (x.data.length !== y.data.length) return false; const X = vals(x), Y = vals(y); for (let i = 0; i < X.length; i++) if (X[i] !== Y[i]) return false; return true; });
        fn("gen", 1, (a) => genNew(rt.asInt(rt.needInt(a[0]))));
        fn("gen_seed", 2, (a) => { const g = a[0]; g.seed = rt.asInt(rt.needInt(a[1])); g.state = mt(Number(BigInt.asUintN(32, g.seed))); return null; });
        fn("gen_initial_seed", 1, (a) => a[0].seed);
        fn("rand", 3, (a) => rand(a[0], num(a[1]), a[2] === undefined ? "float32" : rt.needStr(a[2])), 2);
        fn("rand_double", 2, (a) => randDouble(a[0], num(a[1])));
        fn("randn", 3, (a) => randn(a[0], num(a[1]), rt.needStr(a[2])));
        fn("randint", 4, (a) => randint(a[0], num(a[1]), num(a[2]), num(a[3])));
        fn("multinomial", 5, (a) => multinomial(a[0], needS(a[1]), shapeOf(a[2]), num(a[3]), rt.truth(a[4])));
        fn("randperm", 2, (a) => randperm(a[0], num(a[1])));
        fn("random64", 2, (a) => random64(a[0], num(a[1])));
        fn("tobytes", 1, (a) => toBytes(needSC(a[0])));
        fn("frombytes", 3, (a) => fromBytes(rt.needStr(a[0]), a[1], a[2] === undefined || a[2] === null ? null : num(a[2])), 2);
        // zlib's CRC-32 of a bytes object, for zipfile (torch.save checkpoints).
        fn("crc32", 2, (a) => BigInt(crc32(a[0].items, a[1] === undefined ? 0 : num(a[1]))), 1);
        fn("dot_sum", 2, (a) => { const x = vals(needS(a[0])), y = vals(needS(a[1])); let s = 0; for (let i = 0; i < x.length; i++) s += x[i] * y[i]; return s; });
        fn("axpy", 3, (a) => {
            const alpha = num(a[0]), xs = a[1], ys = a[2];
            if (!isStorage(xs) || !isStorage(ys)) {
                needC(xs); needC(ys);
                // y += alpha * x on complex storages of one dtype, part by part.
                if (xs.dtype !== ys.dtype) fail(E.RuntimeError, "axpy: complex storages must share a dtype");
                const X = xs.data, Y = ys.data;
                for (let i = 0; i < Y.length; i++) Y[i] = Y[i] + alpha * X[i];
                written(ys);
                return null;
            }
            const x = vals(xs), y = ys.data, dt = ys.dtype;
            if (dt === "bfloat16") for (let i = 0; i < y.length; i++) y[i] = bfBits(Math.fround(bfValue(y[i]) + alpha * x[i]));
            else for (let i = 0; i < y.length; i++) y[i] = castValue(dt, y[i] + alpha * x[i]);
            written(ys);
            return null;
        });
        // nbytes(s): the bytes a storage's elements occupy.
        fn("nbytes", 1, (a) => BigInt(needSC(a[0]).data.byteLength));
        // fft(s, rows, n_in, n, mode, inverse, scale): see `fft`.
        fn("fft", 7, (a) => fft(needSC(a[0]), num(a[1]), num(a[2]), num(a[3]), num(a[4]), rt.truth(a[5]), num(a[6])));
        // linalg(op, [storages], dims): see `linalg`.
        fn("linalg", 3, (a) => { const items = a[1].items, ins = new Array(items.length); for (let i = 0; i < items.length; i++) ins[i] = needS(items[i]); return linalg(num(a[0]), ins, ints(a[2])); });
        // view_dtype(s, dtype): a storage of `dtype` over the same memory,
        // for two dtypes whose elements have the same typed-array layout
        // (float16/bfloat16/int16, uint8/int8), else None.
        fn("view_dtype", 2, (a) => {
            const s = needS(a[0]), d = rt.needStr(a[1]), C = ARRAY[d];
            if (C === undefined) fail(E.TypeError, "unknown dtype " + d);
            const same = (VIEW_GROUP[d] !== undefined && VIEW_GROUP[d] === VIEW_GROUP[s.dtype]);
            if (!same) return null;
            return make(d, new C(s.data.buffer, s.data.byteOffset, s.data.length));
        });
        g.set("Storage", Storage); g.set("ComplexStorage", CStorage); g.set("Generator", Gen);
    });
})(__zipp_py);
