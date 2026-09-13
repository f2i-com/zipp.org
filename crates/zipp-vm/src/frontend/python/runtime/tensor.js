/* ZIPP Python runtime — `_zipp_tensor`: the numeric kernels behind the
 * bundled `torch` package. Apache-2.0.
 *
 * A storage is a JavaScript typed array with a dtype tag; every kernel is a
 * plain loop over contiguous row-major data (the Python side keeps shapes
 * and strides trivial: tensors are always contiguous, views copy). Nothing
 * here touches a GPU: this is the CPU path of the engine, on wasm or
 * native. Kernels take shapes as Python tuples of ints and return
 * `(storage, shape)` pairs where the shape changes.
 */
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T, E = rt.E, fail = rt.fail;
    const tuple = rt.tuple, list = rt.list;
    const Storage = rt.newType("_Storage", [rt.ObjectType], new Map(), "_zipp_tensor");
    const ARRAY = { float32: Float32Array, float64: Float64Array, int64: Float64Array, int32: Float64Array, bool: Uint8Array, uint8: Uint8Array };
    const RANK = { bool: 0, uint8: 1, int32: 2, int64: 3, float32: 4, float64: 5 };
    function make(dtype, data) { if (ARRAY[dtype] === undefined) fail(E.TypeError, "unknown dtype " + dtype); return { cls: Storage, dtype: dtype, data: data }; }
    function alloc(dtype, n) { return make(dtype, new ARRAY[dtype](n)); }
    function isStorage(v) { return v !== null && typeof v === "object" && v.cls === Storage; }
    function needS(v, what) { if (!isStorage(v)) fail(E.TypeError, (what || "argument") + " must be a tensor storage"); return v; }
    function shapeOf(v) { const out = []; if (v === null || typeof v !== "object" || v.items === undefined) fail(E.TypeError, "shape must be a tuple"); for (const x of v.items) out.push(Number(rt.asInt(x))); return out; }
    function ints(v) { return shapeOf(v); }
    function pyShape(shape) { const out = []; for (const d of shape) out.push(BigInt(d)); return tuple(out); }
    function numel(shape) { let n = 1; for (const d of shape) n *= d; return n; }
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
        floordiv: (x, y) => Math.floor(x / y), mod: (x, y) => x - Math.floor(x / y) * y,
    };
    const COMPARE = new Set(["eq", "ne", "lt", "le", "gt", "ge", "and", "or", "xor"]);
    function binary(op, a, ashape, b, bshape) {
        const f = BIN[op]; if (f === undefined) fail(E.ValueError, "unknown op " + op);
        const shape = broadcastShape(ashape, bshape);
        let dtype = COMPARE.has(op) ? "bool" : promote(a.dtype, b.dtype);
        if (op === "div" && RANK[dtype] < RANK.float32) dtype = "float32";
        if (op === "pow" && RANK[dtype] < RANK.float32 && b.dtype !== "int64") dtype = "float32";
        const out = alloc(dtype, numel(shape)), A = a.data, Bd = b.data, O = out.data;
        const f32 = dtype === "float32";
        if (ashape.length === shape.length && bshape.length === shape.length && numel(ashape) === numel(shape) && numel(bshape) === numel(shape)) {
            for (let i = 0; i < O.length; i++) O[i] = f(A[i], Bd[i]);
        } else if (numel(bshape) === 1 && ashape.length === shape.length && numel(ashape) === numel(shape)) {
            const y = Bd[0]; for (let i = 0; i < O.length; i++) O[i] = f(A[i], y);
        } else {
            forEachBroadcast(shape, bstrides(ashape, shape), bstrides(bshape, shape), (o, x, y) => { O[o] = f(A[x], Bd[y]); });
        }
        if (f32) { /* Float32Array rounds on store */ }
        return tuple([out, pyShape(shape)]);
    }
    const UN = {
        neg: (x) => -x, exp: Math.exp, log: Math.log, tanh: Math.tanh, sigmoid: (x) => 1 / (1 + Math.exp(-x)),
        silu: (x) => x / (1 + Math.exp(-x)), relu: (x) => (x > 0 ? x : 0), sqrt: Math.sqrt, square: (x) => x * x,
        abs: Math.abs, sign: (x) => (x > 0 ? 1 : x < 0 ? -1 : 0), floor: Math.floor, ceil: Math.ceil, round: (x) => { const r = Math.round(x); return (Math.abs(x % 1) === 0.5 && r % 2 !== 0) ? r - Math.sign(x) : r; },
        isfinite: (x) => (Number.isFinite(x) ? 1 : 0), isnan: (x) => (x !== x ? 1 : 0), not: (x) => (x ? 0 : 1), reciprocal: (x) => 1 / x, log1p: Math.log1p, expm1: Math.expm1,
        gelu: (x) => 0.5 * x * (1 + erf(x / Math.SQRT2)), softplus: (x) => (x > 20 ? x : Math.log1p(Math.exp(x))), sin: Math.sin, cos: Math.cos,
    };
    function erf(x) { const t = 1 / (1 + 0.3275911 * Math.abs(x)); const y = 1 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * Math.exp(-x * x); return x >= 0 ? y : -y; }
    function unary(op, a, p1, p2) {
        let f = UN[op];
        if (op === "clamp") { const lo = p1 === null ? -Infinity : jsNumber(p1), hi = p2 === null ? Infinity : jsNumber(p2); f = (x) => (x < lo ? lo : x > hi ? hi : x); }
        if (f === undefined) fail(E.ValueError, "unknown op " + op);
        const dtype = op === "isfinite" || op === "isnan" || op === "not" ? "bool" : (["exp", "log", "tanh", "sigmoid", "silu", "sqrt", "reciprocal", "log1p", "expm1", "gelu", "softplus", "sin", "cos"].includes(op) && RANK[a.dtype] < RANK.float32 ? "float32" : a.dtype);
        const out = alloc(dtype, a.data.length), A = a.data, O = out.data;
        for (let i = 0; i < O.length; i++) O[i] = f(A[i]);
        return out;
    }
    // ---- reductions ------------------------------------------------------------------
    function reduce(op, a, shape, dims, keepdim) {
        const rank = shape.length;
        let red = dims === null ? null : ints(dims).map((d) => (d < 0 ? d + rank : d));
        if (red !== null) for (const d of red) if (d < 0 || d >= rank) fail(E.IndexError, "Dimension out of range");
        const all = red === null;
        const isRed = new Array(rank).fill(all);
        if (!all) for (const d of red) isRed[d] = true;
        const outShape = [], keptShape = [];
        for (let d = 0; d < rank; d++) { if (isRed[d]) { keptShape.push(1); } else { outShape.push(shape[d]); keptShape.push(shape[d]); } }
        const finalShape = keepdim ? keptShape : outShape;
        const nOut = numel(keptShape), nIn = numel(shape);
        const argOp = op === "argmax" || op === "argmin";
        const dtype = argOp ? "int64" : (op === "all" || op === "any") ? "bool" : (op === "mean" && RANK[a.dtype] < RANK.float32) ? "float32" : a.dtype;
        const out = alloc(dtype, nOut), O = out.data, A = a.data;
        const init = op === "sum" || op === "mean" ? 0 : op === "prod" ? 1 : op === "max" || op === "argmax" ? -Infinity : op === "min" || op === "argmin" ? Infinity : op === "all" ? 1 : 0;
        const best = argOp ? new Float64Array(nOut).fill(init) : null;
        O.fill(init);
        // Map every input index to its output offset.
        const inS = strides(shape), outS = strides(keptShape);
        const idx = new Array(rank).fill(0);
        const count = nIn / (nOut || 1);
        let oo = 0, pos = new Array(rank).fill(0);
        for (let flat = 0; flat < nIn; flat++) {
            const x = A[flat];
            switch (op) {
                case "sum": case "mean": O[oo] += x; break;
                case "prod": O[oo] *= x; break;
                case "max": if (x > O[oo] || x !== x) O[oo] = x; break;
                case "min": if (x < O[oo] || x !== x) O[oo] = x; break;
                case "argmax": if (x > best[oo]) { best[oo] = x; O[oo] = redIndex(pos, isRed, shape); } break;
                case "argmin": if (x < best[oo]) { best[oo] = x; O[oo] = redIndex(pos, isRed, shape); } break;
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
        return tuple([out, pyShape(finalShape)]);
    }
    // The flat index within the reduced dims (row-major over them).
    function redIndex(pos, isRed, shape) {
        let out = 0;
        for (let d = 0; d < shape.length; d++) if (isRed[d]) out = out * shape[d] + pos[d];
        return out;
    }
    // ---- shape kernels -----------------------------------------------------------------
    function permute(a, shape, perm) {
        const p = ints(perm), rank = shape.length;
        const outShape = p.map((d) => shape[d]);
        const inS = strides(shape), sa = p.map((d) => inS[d]);
        const out = alloc(a.dtype, a.data.length), O = out.data, A = a.data;
        forEachBroadcast(outShape, sa, new Array(rank).fill(0), (o, x) => { O[o] = A[x]; });
        return tuple([out, pyShape(outShape)]);
    }
    function expand(a, shape, target) {
        const t = ints(target);
        const sa = bstrides(shape, t), out = alloc(a.dtype, numel(t)), O = out.data, A = a.data;
        forEachBroadcast(t, sa, new Array(t.length).fill(0), (o, x) => { O[o] = A[x]; });
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
        forEachBroadcast(outShape, sa, new Array(outShape.length).fill(0), (o, x) => { O[o] = A[base + x]; });
        return tuple([out, pyShape(outShape)]);
    }
    function setSlice(a, shape, spec, v, vshape) {
        const s = sliceSpec(spec, shape), inS = strides(shape);
        const outShape = [], sa = [];
        let base = 0;
        for (let d = 0; d < shape.length; d++) { base += s.starts[d] * inS[d]; if (s.keep[d]) { outShape.push(s.counts[d]); sa.push(inS[d] * s.steps[d]); } }
        const sv = bstrides(vshape, outShape), A = a.data, V = v.data, f32 = a.dtype === "float32";
        forEachBroadcast(outShape, sa, sv, (o, x, y) => { A[base + x] = castValue(a.dtype, V[y]); });
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
        const f32 = dtype === "float32";
        forEachBroadcast(batch, sa, sb, (bi, oa, ob) => {
            const baseA = oa * m * k, baseB = ob * k * n, baseO = bi * m * n;
            for (let i = 0; i < m; i++) {
                for (let j = 0; j < n; j++) {
                    let s = 0;
                    if (f32) { for (let p = 0; p < k; p++) s = Math.fround(s + Math.fround(Ad[baseA + i * k + p] * Bd[baseB + p * n + j])); }
                    else { for (let p = 0; p < k; p++) s += Ad[baseA + i * k + p] * Bd[baseB + p * n + j]; }
                    O[baseO + i * n + j] = s;
                }
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
        for (let b = 0; b < B; b++) for (let o = 0; o < Oc; o++) for (let t = 0; t < Lo; t++) {
            const gv = G[(b * Oc + o) * Lo + t]; if (gv === 0) continue;
            GB[o] += gv;
            for (let c = 0; c < C; c++) { const xb = (b * C + c) * L + t, wb = (o * C + c) * K; for (let k = 0; k < K; k++) { GX[xb + k] += gv * W[wb + k]; GW[wb + k] += gv * X[xb + k]; } }
        }
        return tuple([gx, gw, gb]);
    }
    function softmax(a, shape, dim, log) {
        const d = dim < 0 ? dim + shape.length : dim, n = shape[d];
        const outer = numel(shape.slice(0, d)), inner = numel(shape.slice(d + 1));
        const dtype = RANK[a.dtype] < RANK.float32 ? "float32" : a.dtype, out = alloc(dtype, a.data.length), O = out.data, A = a.data;
        for (let x = 0; x < outer; x++) for (let r = 0; r < inner; r++) {
            const base = x * n * inner + r;
            let mx = -Infinity; for (let i = 0; i < n; i++) { const v = A[base + i * inner]; if (v > mx) mx = v; }
            let sum = 0; for (let i = 0; i < n; i++) sum += Math.exp(A[base + i * inner] - mx);
            const ls = Math.log(sum);
            for (let i = 0; i < n; i++) { const z = A[base + i * inner] - mx; O[base + i * inner] = log ? z - ls : Math.exp(z) / sum; }
        }
        return out;
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
                    const takeLeft = desc ? !(b > a) : !(b < a);
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
    function where(c, cs, a, as_, b, bs) {
        const shape = broadcastShape(broadcastShape(cs, as_), bs), dtype = promote(a.dtype, b.dtype);
        const out = alloc(dtype, numel(shape)), O = out.data;
        const sc = bstrides(cs, shape), sa = bstrides(as_, shape), sb = bstrides(bs, shape);
        const Cd = c.data, Ad = a.data, Bd = b.data;
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
    function randint(g, low, high, n) {
        const s = needGen(g), out = alloc("int64", n), O = out.data;
        const range = high - low;
        if (range <= 0) fail(E.RuntimeError, "random_ expects 'from' to be less than 'to'");
        for (let i = 0; i < n; i++) {
            const r = range <= 4294967296 ? next32(s) % range : ((next32(s) * 4294967296 + next32(s)) % range);
            O[i] = low + r;
        }
        return out;
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
    function randperm(g, n) {
        const s = needGen(g), out = alloc("int64", n), O = out.data;
        for (let i = 0; i < n; i++) O[i] = i;
        for (let i = n - 1; i > 0; i--) { const j = next32(s) % (i + 1); const t = O[i]; O[i] = O[j]; O[j] = t; }
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
        return rt.bytes(Array.from(bytes));
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
    // ---- the module --------------------------------------------------------------------------------
    rt.defineModule("_zipp_tensor", (g) => {
        const fn = (name, arity, code, min) => g.set(name, rt.builtin(name, arity, code, min));
        const num = (v) => jsNumber(v);
        fn("zeros", 2, (a) => alloc(rt.needStr(a[0]), num(a[1])));
        fn("full", 3, (a) => { const s = alloc(rt.needStr(a[0]), num(a[1])); s.data.fill(castValue(s.dtype, num(a[2]))); return s; });
        fn("from_flat", 2, (a) => { const items = a[1].items, s = alloc(rt.needStr(a[0]), items.length); for (let i = 0; i < items.length; i++) s.data[i] = castValue(s.dtype, jsNumber(items[i])); return s; });
        fn("to_list", 1, (a) => { const s = needS(a[0]); const out = new Array(s.data.length); for (let i = 0; i < out.length; i++) out[i] = pyNumber(s.dtype, s.data[i]); return list(out); });
        fn("item", 2, (a) => { const s = needS(a[0]); return pyNumber(s.dtype, s.data[num(a[1])]); });
        fn("setitem", 3, (a) => { const s = needS(a[0]); s.data[num(a[1])] = castValue(s.dtype, num(a[2])); return null; });
        fn("copy", 1, (a) => { const s = needS(a[0]); return make(s.dtype, s.data.slice()); });
        fn("astype", 2, (a) => { const s = needS(a[0]), d = rt.needStr(a[1]); const out = alloc(d, s.data.length); for (let i = 0; i < out.data.length; i++) out.data[i] = castValue(d, s.data[i]); return out; });
        fn("dtype", 1, (a) => needS(a[0]).dtype);
        fn("size", 1, (a) => BigInt(needS(a[0]).data.length));
        fn("fill", 2, (a) => { const s = needS(a[0]); s.data.fill(castValue(s.dtype, num(a[1]))); return null; });
        fn("copy_into", 2, (a) => { const d = needS(a[0]), s = needS(a[1]); if (d.data.length !== s.data.length) fail(E.RuntimeError, "size mismatch"); for (let i = 0; i < d.data.length; i++) d.data[i] = castValue(d.dtype, s.data[i]); return null; });
        fn("binary", 5, (a) => binary(rt.needStr(a[0]), needS(a[1]), shapeOf(a[2]), needS(a[3]), shapeOf(a[4])));
        fn("unary", 4, (a) => unary(rt.needStr(a[0]), needS(a[1]), a[2] === undefined ? null : a[2], a[3] === undefined ? null : a[3]), 2);
        fn("reduce", 5, (a) => reduce(rt.needStr(a[0]), needS(a[1]), shapeOf(a[2]), a[3], rt.truth(a[4])));
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
        fn("softmax", 4, (a) => softmax(needS(a[0]), shapeOf(a[1]), num(a[2]), rt.truth(a[3])));
        fn("argsort", 4, (a) => argsort(needS(a[0]), shapeOf(a[1]), num(a[2]), rt.truth(a[3])));
        fn("one_hot", 2, (a) => oneHot(needS(a[0]), num(a[1])));
        fn("where", 6, (a) => where(needS(a[0]), shapeOf(a[1]), needS(a[2]), shapeOf(a[3]), needS(a[4]), shapeOf(a[5])));
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
        fn("tobytes", 1, (a) => toBytes(needS(a[0])));
        fn("frombytes", 3, (a) => fromBytes(rt.needStr(a[0]), a[1], a[2] === undefined || a[2] === null ? null : num(a[2])), 2);
        fn("dot_sum", 2, (a) => { const x = needS(a[0]).data, y = needS(a[1]).data; let s = 0; for (let i = 0; i < x.length; i++) s += x[i] * y[i]; return s; });
        fn("axpy", 3, (a) => { const alpha = num(a[0]), x = needS(a[1]).data, y = needS(a[2]).data; for (let i = 0; i < y.length; i++) y[i] = castValue(a[2].dtype, y[i] + alpha * x[i]); return null; });
        g.set("Storage", Storage); g.set("Generator", Gen);
    });
})(__zipp_py);
