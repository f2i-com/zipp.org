/* ZIPP Python runtime — values: numbers, comparison, hashing, containers,
 * indexing, truth, str/repr. Apache-2.0. */
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T, E = rt.E, STOP = rt.STOP, NOTIMPL = rt.NOTIMPL;
    const fail = rt.fail, typeOf = rt.typeOf, isType = rt.isType, isInstance = rt.isInstance, isSubclass = rt.isSubclass;
    const list = rt.list, tuple = rt.tuple, sequence = rt.sequence, call = rt.call;
    const typeMethod = rt.typeMethod, descrGet = rt.descrGet, callMethod = rt.callMethod;

    // ---- numbers ----------------------------------------------------------------------------------
    function isInt(v) { return typeof v === "bigint" || typeof v === "boolean"; }
    function asInt(v) { return typeof v === "boolean" ? (v ? 1n : 0n) : v; }
    function isNum(v) { const t = typeof v; return t === "bigint" || t === "number" || t === "boolean"; }
    // int -> float raises OverflowError past the double range, as CPython does.
    function intToFloat(i) {
        const x = Number(i);
        if (x === Infinity || x === -Infinity) fail(E.OverflowError, "int too large to convert to float");
        return x;
    }
    function toFloat(v) { return typeof v === "number" ? v : intToFloat(asInt(v)); }
    // float -> int truncation (int(), round(), math.trunc/floor/ceil).
    rt.floatToInt = function (x) {
        if (x !== x) fail(E.ValueError, "cannot convert float NaN to integer");
        if (x === Infinity || x === -Infinity) fail(E.OverflowError, "cannot convert float infinity to integer");
        return BigInt(Math.trunc(x));
    };
    // An int's size is bounded only by the engine's BigInt limit (2^30 bits,
    // 2^20 in the hardened profile), on every path alike: the emitter's
    // inline arithmetic, these helpers, `**` and `<<` (which the engine
    // checks before computing). Its RangeError surfaces as OverflowError.
    rt.isInt = isInt; rt.asInt = asInt; rt.isNum = isNum; rt.toFloat = toFloat;
    function pyFloorDiv(a, b) {
        if (b === 0n) fail(E.ZeroDivisionError, "integer division or modulo by zero");
        let q = a / b; if ((a % b !== 0n) && ((a < 0n) !== (b < 0n))) q -= 1n;
        return q;
    }
    function pyMod(a, b) {
        if (b === 0n) fail(E.ZeroDivisionError, "integer modulo by zero");
        let r = a % b; if (r !== 0n && ((r < 0n) !== (b < 0n))) r += b;
        return r;
    }
    // float // and % follow CPython's _float_div_mod: fmod (JS %, exact),
    // the quotient of the exact multiple snapped to an integral value, and a
    // zero remainder carrying the divisor's sign. `mod !== 0` is C's `if (mod)`
    // (NaN is true there).
    function floatFloorDiv(x, y) {
        if (y === 0) fail(E.ZeroDivisionError, "float floor division by zero");
        const mod = x % y;
        let div = (x - mod) / y;
        if (mod !== 0 && (y < 0) !== (mod < 0)) div -= 1;
        if (div !== 0) { const f = Math.floor(div); return div - f > 0.5 ? f + 1 : f; }
        const q = x / y;
        return q < 0 || Object.is(q, -0) ? -0 : 0;
    }
    function floatMod(x, y) {
        if (y === 0) fail(E.ZeroDivisionError, "float modulo by zero");
        const mod = x % y;
        if (mod === 0) return y < 0 ? -0 : 0;
        return (y < 0) !== (mod < 0) ? mod + y : mod;
    }
    function isOddInteger(x) { return Number.isInteger(x) && Math.abs(x % 2) === 1; }
    // float ** float with CPython's special cases (float_pow): 1 ** nan and
    // (-1) ** inf are 1.0, 0.0 ** negative raises, and a finite overflow is
    // OverflowError(34, 'Result too large') (errno ERANGE).
    function floatPow(x, y) {
        if (y === 0) return 1;
        if (x !== x) return x;
        if (y !== y) return x === 1 ? 1 : y;
        if (y === Infinity || y === -Infinity) {
            const ax = Math.abs(x);
            if (ax === 1) return 1;
            return (y > 0) === (ax > 1) ? Infinity : 0;
        }
        if (x === Infinity || x === -Infinity) {
            const odd = isOddInteger(y);
            if (y > 0) return odd ? x : Infinity;
            return odd && x < 0 ? -0 : 0;
        }
        if (x === 0) {
            if (y < 0) fail(E.ZeroDivisionError, "0.0 cannot be raised to a negative power");
            return isOddInteger(y) ? x : 0;
        }
        let negate = false;
        if (x < 0) {
            if (!Number.isInteger(y)) return NaN; // CPython's result is complex
            x = -x; negate = isOddInteger(y);
        }
        if (x === 1) return negate ? -1 : 1;
        const r = Math.pow(x, y);
        if (r === Infinity) throw rt.makeExc(E.OverflowError, [34n, "Result too large"]);
        return negate ? -r : r;
    }
    // int / int, correctly rounded even beyond 2^53: scale the quotient to at
    // least 70 significant bits before the (correctly rounded) BigInt to
    // Number conversion.
    function intTrueDiv(a, b) {
        if (b === 0n) fail(E.ZeroDivisionError, "division by zero");
        const LIMIT = 9007199254740992n;
        if (a > -LIMIT && a < LIMIT && b > -LIMIT && b < LIMIT) return Number(a) / Number(b);
        const neg = (a < 0n) !== (b < 0n);
        let x = a < 0n ? -a : a, y = b < 0n ? -b : b;
        const bits = (v) => v.toString(2).length;
        let shift = 70 - (bits(x) - bits(y));
        if (shift < 0) shift = 0;
        const q = (x << BigInt(shift)) / y;
        const r = Number(q) * Math.pow(2, -shift);
        if (r === Infinity) fail(E.OverflowError, "integer division result too large for a float");
        return neg ? -r : r;
    }
    function intPow(a, b) {
        if (b < 0n) {
            if (a === 0n) fail(E.ZeroDivisionError, "0.0 cannot be raised to a negative power");
            return floatPow(intToFloat(a), intToFloat(b));
        }
        return a ** b;
    }
    function intShift(a, b, left) {
        if (b < 0n) fail(E.ValueError, "negative shift count");
        return left ? a << b : a >> b;
    }
    function intBinop(op, a, b) {
        switch (op) {
            case "add": return a + b;
            case "sub": return a - b;
            case "mul": return a * b;
            case "truediv": return intTrueDiv(a, b);
            case "floordiv": return pyFloorDiv(a, b);
            case "mod": return pyMod(a, b);
            case "pow": return intPow(a, b);
            case "lshift": return intShift(a, b, true);
            case "rshift": return intShift(a, b, false);
            case "and": return a & b;
            case "or": return a | b;
            case "xor": return a ^ b;
        }
        return NOTIMPL;
    }
    function floatBinop(op, x, y) {
        switch (op) {
            case "add": return x + y;
            case "sub": return x - y;
            case "mul": return x * y;
            case "truediv": if (y === 0) fail(E.ZeroDivisionError, "float division by zero"); return x / y;
            case "floordiv": return floatFloorDiv(x, y);
            case "mod": return floatMod(x, y);
            case "pow": return floatPow(x, y);
        }
        return NOTIMPL;
    }
    function numBinop(op, a, b) {
        const ta = typeof a, tb = typeof b;
        if (ta === "boolean" && tb === "boolean") {
            // bool & | ^ bool stays a bool.
            if (op === "and") return a && b;
            if (op === "or") return a || b;
            if (op === "xor") return a !== b;
        }
        if (ta !== "number" && tb !== "number") return intBinop(op, asInt(a), asInt(b));
        const r = floatBinop(op, toFloat(a), toFloat(b));
        // Not NOTIMPL: int's reflected methods would dispatch straight back here.
        if (r === NOTIMPL) fail(E.TypeError, "unsupported operand type(s) for " + opSymbol(op) + ": '" + typeOf(a).name + "' and '" + typeOf(b).name + "'");
        return r;
    }
    rt.floatDivmod = function (x, y) {
        if (y === 0) fail(E.ZeroDivisionError, "float divmod()");
        return [floatFloorDiv(x, y), floatMod(x, y)];
    };
    const SYMBOLS = { add: "+", sub: "-", mul: "*", truediv: "/", floordiv: "//", mod: "%", pow: "**",
        lshift: "<<", rshift: ">>", and: "&", or: "|", xor: "^", matmul: "@" };
    function opSymbol(op) { return SYMBOLS[op] || op; }
    const DUNDER = {};
    for (const op of Object.keys(SYMBOLS)) DUNDER[op] = ["__" + op + "__", "__r" + op + "__", "__i" + op + "__"];
    function repeat(seq, n) {
        n = Number(asInt(n)); if (n < 0) n = 0;
        if (typeof seq === "string") {
            if (seq.length * n > rt.MAX_TEXT) fail(E.MemoryError, "string limit exceeded");
            return seq.repeat(n);
        }
        if (seq.items.length * n > rt.MAX_ITEMS) fail(E.MemoryError, "sequence limit exceeded");
        const out = [];
        for (let i = 0; i < n; i++) for (const x of seq.items) out.push(x);
        return sequence(seq.cls, out);
    }
    function binop(op, a, b, inplace) {
        // Fast paths for the primitive types.
        const ta = typeof a, tb = typeof b;
        if ((ta === "bigint" || ta === "number" || ta === "boolean") && (tb === "bigint" || tb === "number" || tb === "boolean")) {
            const r = numBinop(op, a, b);
            if (r !== NOTIMPL) return r;
        }
        if (ta === "string") {
            if (op === "add" && tb === "string") return rt.checkedText(a + b);
            if (op === "mul" && isInt(b)) return repeat(a, b);
            if (op === "mod") return rt.percentFormat(a, b);
        }
        if (a !== null && ta === "object") {
            const c = a.cls;
            if (c === T.list || c === T.tuple || c === T.dict || c === T.set || c === T.frozenset || c === T.bytes) {
                const r = baseBinop(op, a, b, inplace); if (r !== NOTIMPL) return r;
            }
        }
        if (tb === "string" && op === "mul" && isInt(a)) return repeat(b, a);
        if (b !== null && tb === "object" && (b.cls === T.list || b.cls === T.tuple) && op === "mul" && isInt(a)) return repeat(b, a);
        // Dunder dispatch: a.__op__(b), then b.__rop__(a).
        const names = DUNDER[op];
        if (names === undefined) fail(E.TypeError, "unsupported operator " + op);
        if (inplace) {
            const im = typeMethod(a, names[2]);
            if (im !== undefined) { const r = im.isBase ? baseBinop(op, a, b, true) : call(descrGet(im, a, typeOf(a)), [b], null); if (r !== NOTIMPL) return r; }
        }
        const ca = typeOf(a), cb = typeOf(b);
        let rm = ca !== cb ? typeMethod(b, names[1]) : undefined;
        // A right operand whose type is a subclass overriding the reflected
        // method is tried first (CPython's binary_op1).
        if (rm !== undefined && isSubclass(cb, ca) && rm !== typeMethod(a, names[1])) {
            const r = call(descrGet(rm, b, cb), [a], null); if (r !== NOTIMPL) return r;
            rm = undefined;
        }
        const m = typeMethod(a, names[0]);
        if (m !== undefined) { const r = m.isBase ? baseBinop(op, a, b, false) : call(descrGet(m, a, ca), [b], null); if (r !== NOTIMPL) return r; }
        if (rm !== undefined) { const r = call(descrGet(rm, b, cb), [a], null); if (r !== NOTIMPL) return r; }
        // A boxed int/float/str subclass instance without its own operator.
        if ((ta === "object" && a !== null && a.pyval !== undefined) || (tb === "object" && b !== null && b.pyval !== undefined)) return binop(op, rt.unbox(a), rt.unbox(b), inplace);
        if (op === "add" && ta === "string") fail(E.TypeError, 'can only concatenate str (not "' + typeOf(b).name + '") to str');
        if (op === "add" && a !== null && ta === "object" && (a.cls === T.list || a.cls === T.tuple)) fail(E.TypeError, 'can only concatenate ' + a.cls.name + ' (not "' + typeOf(b).name + '") to ' + a.cls.name);
        if (op === "mul") {
            const isSeq = (v) => typeof v === "string" || (v !== null && typeof v === "object" && v.items !== undefined && (isInstance(v, T.list) || isInstance(v, T.tuple) || isInstance(v, T.bytes)));
            const other = isSeq(a) ? b : isSeq(b) ? a : undefined;
            if (other !== undefined && !isInt(other)) fail(E.TypeError, "can't multiply sequence by non-int of type '" + typeOf(other).name + "'");
        }
        fail(E.TypeError, "unsupported operand type(s) for " + opSymbol(op) + (inplace ? "=" : "") + ": '" + typeOf(a).name + "' and '" + typeOf(b).name + "'");
    }
    // The builtin containers' arithmetic, structural (works for subclasses).
    function baseBinop(op, a, b, inplace) {
        if (a === null || typeof a !== "object" || a.pyval !== undefined) return NOTIMPL;
        if (a.items !== undefined && (isInstance(a, T.list) || isInstance(a, T.tuple))) {
            const isList = isInstance(a, T.list);
            const base = isList ? T.list : T.tuple;
            if (op === "add" && b !== null && typeof b === "object" && b.items !== undefined && isInstance(b, base)) {
                if (inplace && isList) { for (const x of b.items) a.items.push(x); return a; }
                return sequence(base, a.items.concat(b.items));
            }
            if (op === "add" && inplace && isList) { R.extend(a.items, b); return a; }
            if (op === "mul" && isInt(b)) {
                if (inplace && isList) { const copy = a.items.slice(); a.items.length = 0; const n = Number(asInt(b)); for (let i = 0; i < n; i++) for (const x of copy) a.items.push(x); return a; }
                return repeat({ cls: base, items: a.items }, b);
            }
            return NOTIMPL;
        }
        if (a.map !== undefined && isInstance(a, T.dict)) {
            if (op === "or" && b !== null && typeof b === "object" && b.map !== undefined && isInstance(b, T.dict)) {
                const out = inplace ? a : rt.dictCopy(a); for (const [k, v] of rt.dictEntries(b)) rt.dictSet(out, k, v); return out;
            }
            return NOTIMPL;
        }
        if (a.map !== undefined && b !== null && typeof b === "object" && b.map !== undefined && !isInstance(b, T.dict)) return rt.setBinop(op, a, b, inplace);
        if (isInstance(a, T.bytes) && op === "add" && b !== null && typeof b === "object" && isInstance(b, T.bytes)) return { cls: T.bytes, items: a.items.concat(b.items) };
        if (isInstance(a, T.bytes) && op === "mul" && isInt(b)) return { cls: T.bytes, items: repeat({ cls: T.bytes, items: a.items }, b).items };
        if (isInstance(b, T.bytes) && op === "mul" && isInt(a)) return { cls: T.bytes, items: repeat({ cls: T.bytes, items: b.items }, a).items };
        return NOTIMPL;
    }
    rt.baseBinop = baseBinop;
    R.binop = function (op, a, b) { return binop(op, a, b, false); };
    R.iop = function (op, a, b) { return binop(op, a, b, true); };
    rt.binop = R.binop;
    // The emitter's per-operator entry points (int op int is already inline
    // VM code): primitive pairs are decided by typeof tests without an
    // op-name dispatch; everything else goes through `binop`.
    R.add = function (a, b) {
        const ta = typeof a, tb = typeof b;
        if (ta === "number") { if (tb === "number") return a + b; if (tb === "bigint") return a + intToFloat(b); }
        else if (ta === "bigint") { if (tb === "bigint") return a + b; if (tb === "number") return intToFloat(a) + b; }
        else if (ta === "string" && tb === "string") return rt.checkedText(a + b);
        return binop("add", a, b, false);
    };
    R.sub = function (a, b) {
        const ta = typeof a, tb = typeof b;
        if (ta === "number") { if (tb === "number") return a - b; if (tb === "bigint") return a - intToFloat(b); }
        else if (ta === "bigint") { if (tb === "bigint") return a - b; if (tb === "number") return intToFloat(a) - b; }
        return binop("sub", a, b, false);
    };
    R.mul = function (a, b) {
        const ta = typeof a, tb = typeof b;
        if (ta === "number") { if (tb === "number") return a * b; if (tb === "bigint") return a * intToFloat(b); }
        else if (ta === "bigint") { if (tb === "bigint") return a * b; if (tb === "number") return intToFloat(a) * b; }
        return binop("mul", a, b, false);
    };
    R.truediv = function (a, b) {
        if (typeof a === "number" && typeof b === "number" && b !== 0) return a / b;
        return binop("truediv", a, b, false);
    };
    R.floordiv = function (a, b) {
        const ta = typeof a;
        if (ta === typeof b) { if (ta === "number") return floatFloorDiv(a, b); if (ta === "bigint") return pyFloorDiv(a, b); }
        return binop("floordiv", a, b, false);
    };
    R.mod = function (a, b) {
        const ta = typeof a;
        if (ta === typeof b) { if (ta === "number") return floatMod(a, b); if (ta === "bigint") return pyMod(a, b); }
        return binop("mod", a, b, false);
    };
    R.pow = function (a, b) {
        const ta = typeof a;
        if (ta === typeof b) { if (ta === "number") return floatPow(a, b); if (ta === "bigint") return intPow(a, b); }
        return binop("pow", a, b, false);
    };
    R.lshift = function (a, b) { return typeof a === "bigint" && typeof b === "bigint" ? intShift(a, b, true) : binop("lshift", a, b, false); };
    R.rshift = function (a, b) { return typeof a === "bigint" && typeof b === "bigint" ? intShift(a, b, false) : binop("rshift", a, b, false); };
    R.and = function (a, b) { return typeof a === "bigint" && typeof b === "bigint" ? a & b : binop("and", a, b, false); };
    R.or = function (a, b) { return typeof a === "bigint" && typeof b === "bigint" ? a | b : binop("or", a, b, false); };
    R.xor = function (a, b) { return typeof a === "bigint" && typeof b === "bigint" ? a ^ b : binop("xor", a, b, false); };
    R.matmul = function (a, b) { return binop("matmul", a, b, false); };
    // In-place forms: a primitive left operand has no __iop__, so it is the
    // binary operator.
    // Every fallback stays on the in-place path: `binop`'s primitive fast
    // path is the same one the binary helpers take, and a TypeError from
    // `x += y` must name `+=`, not `+`.
    R.iadd = function (a, b) {
        const ta = typeof a, tb = typeof b;
        if (ta === "number") { if (tb === "number") return a + b; }
        else if (ta === "bigint") { if (tb === "bigint") return a + b; }
        else if (ta === "string") { if (tb === "string") return rt.checkedText(a + b); }
        return binop("add", a, b, true);
    };
    R.isub = function (a, b) { return typeof a === "number" && typeof b === "number" ? a - b : typeof a === "bigint" && typeof b === "bigint" ? a - b : binop("sub", a, b, true); };
    R.imul = function (a, b) { return typeof a === "number" && typeof b === "number" ? a * b : typeof a === "bigint" && typeof b === "bigint" ? a * b : binop("mul", a, b, true); };
    for (const op of ["truediv", "floordiv", "mod", "pow", "lshift", "rshift", "and", "or", "xor", "matmul"]) {
        R["i" + op] = function (a, b) { return binop(op, a, b, true); };
    }
    R.unop = function (op, v) {
        if (isInt(v)) { const i = asInt(v); return op === "neg" ? -i : op === "pos" ? i : ~i; }
        if (typeof v === "number") { if (op === "invert") fail(E.TypeError, "bad operand type for unary ~: 'float'"); return op === "neg" ? -v : v; }
        const name = op === "neg" ? "__neg__" : op === "pos" ? "__pos__" : "__invert__";
        const r = callMethod(v, name, []);
        if (r !== undefined) return r;
        fail(E.TypeError, "bad operand type for unary " + (op === "neg" ? "-" : op === "pos" ? "+" : "~") + ": '" + typeOf(v).name + "'");
    };

    // ---- truth, equality, ordering ----------------------------------------------------------------------
    function truth(v) {
        if (v === true) return true;
        if (v === null || v === false) return false;
        switch (typeof v) {
            case "bigint": return v !== 0n;
            case "number": return v !== 0;
            case "string": return v.length !== 0;
            case "undefined": return false;
        }
        const c = v.cls;
        if (c === T.list || c === T.tuple || c === T.bytes) return v.items.length !== 0;
        if (c === T.dict || c === T.set || c === T.frozenset) return v.size !== 0;
        if (c === T.range) return rangeLength(v) !== 0n;
        const b = typeMethod(v, "__bool__");
        if (b !== undefined) {
            const r = call(descrGet(b, v, c), [], null);
            if (typeof r !== "boolean") fail(E.TypeError, "__bool__ should return bool, returned " + typeOf(r).name);
            return r;
        }
        const l = typeMethod(v, "__len__");
        if (l !== undefined) {
            const n = call(descrGet(l, v, c), [], null);
            if (!isInt(n)) fail(E.TypeError, "'" + typeOf(n).name + "' object cannot be interpreted as an integer");
            if (asInt(n) < 0n) fail(E.ValueError, "__len__() should return >= 0");
            return asInt(n) !== 0n;
        }
        return true;
    }
    R.truth = truth; rt.truth = truth;
    // Equality as the containers use it (PyObject_RichCompareBool): identity
    // first, so the same NaN equals itself inside a list, a dict or `in`.
    // The `==` operator itself goes through `cmp`, where NaN != NaN.
    function eq(a, b) {
        if (a === b) return true;
        const ta = typeof a, tb = typeof b;
        if (ta !== "object" && tb !== "object") {
            // Mixed int/float compares exactly (a BigInt == a Number is exact
            // in JS); bools are the ints 0 and 1.
            // CPython's containers compare identity first, so a NaN finds
            // ITSELF in a list or dict. Floats are unboxed JS numbers here and
            // two NaNs have no identity to tell apart, so treating them as
            // equal would make every NaN one key: `{nan1: "a", nan2: "b"}`
            // would collapse to one entry and lose a value. A miss is the safe
            // answer (and what the engine did before); the limitation is in
            // docs/PYTHON_FRONTEND_EXPERIMENT.md.
            if (ta === "number") return tb === "number" ? false : tb === "bigint" ? a == b : tb === "boolean" && a === (b ? 1 : 0);
            if (ta === "bigint") return tb === "number" ? a == b : tb === "boolean" && a === (b ? 1n : 0n);
            if (ta === "boolean") return tb === "bigint" ? (a ? 1n : 0n) === b : tb === "number" && (a ? 1 : 0) === b;
            return false;
        }
        if (ta === "object" && tb === "object" && a !== null && b !== null && a.cls === b.cls) {
            const c = a.cls;
            if (c === T.list || c === T.tuple || c === T.bytes) {
                if (a.items.length !== b.items.length) return false;
                for (let i = 0; i < a.items.length; i++) if (!eq(a.items[i], b.items[i])) return false;
                return true;
            }
            if (c === T.dict) return rt.dictEq(a, b);
            if (c === T.set || c === T.frozenset) return rt.setEq(a, b);
            if (c === T.range) { const n = rangeLength(a); return n === rangeLength(b) && (n === 0n || (a.start === b.start && (n === 1n || a.step === b.step))); }
            if (c === T.slice) return eq(a.start, b.start) && eq(a.stop, b.stop) && eq(a.step, b.step);
            // Bound methods are fresh per access: equal when they bind the
            // same function to the same (identical) object.
            if (c === T.method) return a.func === b.func && a.self === b.self;
        }
        if (ta === "object" && a !== null) {
            const m = typeMethod(a, "__eq__");
            if (m !== undefined && m !== rt.ObjectType.dict.get("__eq__")) { const r = call(descrGet(m, a, a.cls), [b], null); if (r !== NOTIMPL) return truth(r); }
        }
        if (tb === "object" && b !== null) {
            const m = typeMethod(b, "__eq__");
            if (m !== undefined && m !== rt.ObjectType.dict.get("__eq__")) { const r = call(descrGet(m, b, b.cls), [a], null); if (r !== NOTIMPL) return truth(r); }
        }
        return false;
    }
    rt.eq = eq;
    const ORDER = { lt: ["__lt__", "__gt__"], le: ["__le__", "__ge__"], gt: ["__gt__", "__lt__"], ge: ["__ge__", "__le__"] };
    function compareSeq(a, b) {
        const n = Math.min(a.length, b.length);
        for (let i = 0; i < n; i++) if (!eq(a[i], b[i])) return order3(a[i], b[i]);
        return a.length < b.length ? -1 : a.length > b.length ? 1 : 0;
    }
    function order3(a, b) {
        if (cmp("lt", a, b)) return -1;
        if (cmp("gt", a, b)) return 1;
        return 0;
    }
    rt.order3 = order3;
    function cmp(op, a, b) {
        const ta = typeof a, tb = typeof b;
        if ((ta === "number" || ta === "bigint" || ta === "boolean") && (tb === "number" || tb === "bigint" || tb === "boolean")) {
            // JS relational and loose-equality operators compare a BigInt with
            // a Number exactly (and NaN compares false), as CPython does.
            const x = ta === "boolean" ? (a ? 1 : 0) : a, y = tb === "boolean" ? (b ? 1 : 0) : b;
            switch (op) {
                case "lt": return x < y; case "le": return x <= y; case "gt": return x > y; case "ge": return x >= y;
                case "eq": return x == y; case "ne": return x != y;
            }
        }
        switch (op) {
            case "eq": return eq(a, b);
            case "ne": {
                if (a !== null && ta === "object") {
                    const m = typeMethod(a, "__ne__");
                    if (m !== undefined && m !== rt.ObjectType.dict.get("__ne__")) { const r = call(descrGet(m, a, a.cls), [b], null); if (r !== NOTIMPL) return truth(r); }
                }
                return !eq(a, b);
            }
            case "is": return ta === "number" && tb === "number" ? Object.is(a, b) : a === b;
            case "isnot": return ta === "number" && tb === "number" ? !Object.is(a, b) : a !== b;
            case "in": return contains(b, a);
            case "notin": return !contains(b, a);
        }
        if (ta === "string" && tb === "string") {
            const c = compareStrings(a, b);
            switch (op) { case "lt": return c < 0; case "le": return c <= 0; case "gt": return c > 0; case "ge": return c >= 0; }
        }
        if (ta === "object" && tb === "object" && a !== null && b !== null) {
            const ca = a.cls, cb = b.cls;
            let c = null;
            if (ca === cb && (ca === T.list || ca === T.tuple)) c = compareSeq(a.items, b.items);
            else if ((ca === T.bytes || ca === T.bytearray) && (cb === T.bytes || cb === T.bytearray)) c = compareBytes(a.items, b.items);
            else if ((ca === T.set || ca === T.frozenset) && (cb === T.set || cb === T.frozenset)) return rt.setCompare(op, a, b);
            if (c !== null) switch (op) { case "lt": return c < 0; case "le": return c <= 0; case "gt": return c > 0; case "ge": return c >= 0; }
        }
        const names = ORDER[op];
        let rm = b !== null && tb === "object" ? typeMethod(b, names[1]) : undefined;
        // The reflected method of a right operand whose type subclasses the
        // left one's goes first (CPython's do_richcompare).
        if (rm !== undefined && a !== null && ta === "object" && a.cls !== b.cls && isSubclass(typeOf(b), typeOf(a))) {
            const r = call(descrGet(rm, b, b.cls), [a], null); if (r !== NOTIMPL) return truth(r);
            rm = undefined;
        }
        if (a !== null && ta === "object") {
            const m = typeMethod(a, names[0]);
            if (m !== undefined) { const r = call(descrGet(m, a, a.cls), [b], null); if (r !== NOTIMPL) return truth(r); }
        }
        if (rm !== undefined) { const r = call(descrGet(rm, b, b.cls), [a], null); if (r !== NOTIMPL) return truth(r); }
        if ((ta === "object" && a !== null && a.pyval !== undefined) || (tb === "object" && b !== null && b.pyval !== undefined)) return cmp(op, rt.unbox(a), rt.unbox(b));
        const sym = { lt: "<", le: "<=", gt: ">", ge: ">=" }[op];
        fail(E.TypeError, "'" + sym + "' not supported between instances of '" + typeOf(a).name + "' and '" + typeOf(b).name + "'");
    }
    function compareBytes(a, b) {
        const n = Math.min(a.length, b.length);
        for (let i = 0; i < n; i++) if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
        return a.length < b.length ? -1 : a.length > b.length ? 1 : 0;
    }
    R.cmp = cmp; rt.cmp = cmp;
    // The comparison OPERATORS: a user-defined dunder's result is returned as
    // is (a tensor, a symbolic expression, ...); everything else is a bool.
    const RICH = { eq: ["__eq__", "__eq__"], ne: ["__ne__", "__ne__"], lt: ["__lt__", "__gt__"], le: ["__le__", "__ge__"], gt: ["__gt__", "__lt__"], ge: ["__ge__", "__le__"] };
    function userDunder(v, name) {
        if (v === null || typeof v !== "object" || v.cls === undefined) return undefined;
        const m = typeMethod(v, name);
        if (m === undefined || m === null || m.isBase || m === rt.ObjectType.dict.get(name)) return undefined;
        return m;
    }
    R.richcmp = function (op, a, b) {
        const names = RICH[op];
        if (names !== undefined && ((a !== null && typeof a === "object") || (b !== null && typeof b === "object"))) {
            let rm = userDunder(b, names[1]);
            if (rm !== undefined && typeOf(a) !== b.cls && isSubclass(b.cls, typeOf(a))) {
                const r = call(descrGet(rm, b, b.cls), [a], null); if (r !== NOTIMPL) return r;
                rm = undefined;
            }
            const m = userDunder(a, names[0]);
            if (m !== undefined) { const r = call(descrGet(m, a, a.cls), [b], null); if (r !== NOTIMPL) return r; }
            if (rm !== undefined) { const r = call(descrGet(rm, b, b.cls), [a], null); if (r !== NOTIMPL) return r; }
        }
        return cmp(op, a, b);
    };
    // The emitter's per-operator comparisons: primitive pairs inline, the
    // rest through `richcmp`.
    R.lt = function (a, b) {
        const ta = typeof a, tb = typeof b;
        if ((ta === "number" || ta === "bigint") && (tb === "number" || tb === "bigint")) return a < b;
        if (ta === "string" && tb === "string") return compareStrings(a, b) < 0;
        return R.richcmp("lt", a, b);
    };
    R.le = function (a, b) {
        const ta = typeof a, tb = typeof b;
        if ((ta === "number" || ta === "bigint") && (tb === "number" || tb === "bigint")) return a <= b;
        if (ta === "string" && tb === "string") return compareStrings(a, b) <= 0;
        return R.richcmp("le", a, b);
    };
    R.gt = function (a, b) {
        const ta = typeof a, tb = typeof b;
        if ((ta === "number" || ta === "bigint") && (tb === "number" || tb === "bigint")) return a > b;
        if (ta === "string" && tb === "string") return compareStrings(a, b) > 0;
        return R.richcmp("gt", a, b);
    };
    R.ge = function (a, b) {
        const ta = typeof a, tb = typeof b;
        if ((ta === "number" || ta === "bigint") && (tb === "number" || tb === "bigint")) return a >= b;
        if (ta === "string" && tb === "string") return compareStrings(a, b) >= 0;
        return R.richcmp("ge", a, b);
    };
    R.eq = function (a, b) {
        const ta = typeof a, tb = typeof b;
        if ((ta === "number" || ta === "bigint") && (tb === "number" || tb === "bigint")) return a == b;
        if (ta === "string" && tb === "string") return a === b;
        return R.richcmp("eq", a, b);
    };
    R.ne = function (a, b) {
        const ta = typeof a, tb = typeof b;
        if ((ta === "number" || ta === "bigint") && (tb === "number" || tb === "bigint")) return a != b;
        if (ta === "string" && tb === "string") return a !== b;
        return R.richcmp("ne", a, b);
    };
    R.is = function (a, b) { return typeof a === "number" && typeof b === "number" ? Object.is(a, b) : a === b; };
    R.isnot = function (a, b) { return typeof a === "number" && typeof b === "number" ? !Object.is(a, b) : a !== b; };
    R.in = function (a, b) { return contains(b, a); };
    R.notin = function (a, b) { return !contains(b, a); };
    function compareStrings(a, b) {
        // Code-point order (JS compares UTF-16 units, which differs for astral characters).
        if (a === b) return 0;
        const n = Math.min(a.length, b.length);
        for (let i = 0; i < n; i++) {
            const x = a.codePointAt(i), y = b.codePointAt(i);
            if (x !== y) return x < y ? -1 : 1;
            if (x > 0xFFFF) i++;
        }
        return a.length < b.length ? -1 : a.length > b.length ? 1 : 0;
    }
    rt.compareStrings = compareStrings;
    // Structural membership for the builtin containers and their subclasses.
    function baseContains(container, needle) {
        if (typeof container === "string") {
            if (typeof needle !== "string") fail(E.TypeError, "'in <string>' requires string as left operand, not " + typeOf(needle).name);
            return container.includes(needle);
        }
        if (container.items !== undefined && (isInstance(container, T.list) || isInstance(container, T.tuple))) { for (const x of container.items) if (eq(x, needle)) return true; return false; }
        if (container.map !== undefined && isInstance(container, T.dict)) return rt.dictHas(container, needle);
        if (container.map !== undefined) return rt.setHas(container, needle);
        if (container.pyval !== undefined) return baseContains(container.pyval, needle);
        if (isInstance(container, T.range)) {
            if (typeof needle === "number" && Number.isInteger(needle)) needle = BigInt(needle);
            if (!isInt(needle)) return false;
            const x = asInt(needle), r = container;
            return (r.step > 0n ? x >= r.start && x < r.stop : x <= r.start && x > r.stop) && (x - r.start) % r.step === 0n;
        }
        if (isInstance(container, T.bytes)) {
            if (isInt(needle)) return container.items.indexOf(Number(asInt(needle))) >= 0;
            if (needle !== null && typeof needle === "object" && needle.cls === T.bytes) return rt.bytesToLatin1(container).indexOf(rt.bytesToLatin1(needle)) >= 0;
            fail(E.TypeError, "a bytes-like object is required, not '" + typeOf(needle).name + "'");
        }
        const it = rt.iter(container);
        for (;;) { const v = rt.fornext(it); if (v === STOP) return false; if (eq(v, needle)) return true; }
    }
    rt.baseContains = baseContains;
    function contains(container, needle) {
        if (typeof container === "string") return baseContains(container, needle);
        if (container !== null && typeof container === "object") {
            const c = container.cls;
            if (c === T.list || c === T.tuple || c === T.dict || c === T.set || c === T.frozenset || c === T.range || c === T.bytes) return baseContains(container, needle);
            if (c === T.dict_keys) return rt.dictHas(container.dict, needle);
            if (c === T.dict_values || c === T.dict_items) { const it = container.iter(); for (;;) { const v = it.next(); if (v === STOP) return false; if (eq(v, needle)) return true; } }
            if (container.isType) { const cc = rt.classDunder(container, "__contains__"); if (cc !== undefined) return truth(call(cc, [needle], null)); }
            const m = typeMethod(container, "__contains__");
            if (m !== undefined) { if (m.isBase) return baseContains(container, needle); return truth(call(descrGet(m, container, c), [needle], null)); }
        }
        if (!rt.isIterable(container)) fail(E.TypeError, "argument of type '" + typeOf(container).name + "' is not iterable");
        const it = rt.iter(container);
        for (;;) { const v = rt.fornext(it); if (v === STOP) return false; if (eq(v, needle)) return true; }
    }
    rt.contains = contains;

    // ---- hashing: the bucket key of a value in dicts and sets ----------------------------------------------
    // The bucket key is whatever is cheapest that keeps Python's equality:
    // strings are themselves (a NUL-led string is escaped, NUL leads every
    // composite key), integers within 2^53 (and integral floats and bools,
    // which equal them) are JS numbers, None is one sentinel object, and
    // identity-hashed instances are the object itself. Ints beyond 2^53 and
    // instances with a __hash__ use the bucket of their Python hash (so a
    // user key equal to an int finds it); NaN, tuples and the other builtin
    // hashables get NUL-prefixed strings. Values with the same bucket are
    // then compared with __eq__.
    const NONE_KEY = { none: true };
    const SAFE = 9007199254740991;
    function keyOf(v) {
        const tv = typeof v;
        if (tv === "string") return v.charCodeAt(0) === 0 ? "\0s" + v : v;
        if (tv === "bigint") { const n = Number(v); return Number.isSafeInteger(n) ? n : intKey(hashBigInt(v)); }
        if (tv !== "object") {
            if (tv === "boolean") return v ? 1 : 0;
            if (tv === "number") {
                if (v !== v) return "\0f";
                if (Number.isInteger(v) && (v > SAFE || v < -SAFE)) return intKey(hashBigInt(BigInt(v)));
                return v;
            }
            return NONE_KEY;
        }
        if (v === null) return NONE_KEY;
        const c = v.cls;
        if (c === T.tuple || c === T.frozenset || c === T.bytes || c === T.range) return "\0" + baseKey(v);
        if (c === T.list || c === T.dict || c === T.set) fail(E.TypeError, "unhashable type: '" + c.name + "'");
        if (c === T.slice) fail(E.TypeError, "unhashable type: 'slice'");
        if (c === T.method) { const s = v.self; return "\0m" + rt.ident(v.func) + ":" + (s !== null && typeof s === "object" ? "o" + rt.ident(s) : keyStr(s)); }
        const h = typeMethod(v, "__hash__");
        if (h === null) fail(E.TypeError, "unhashable type: '" + c.name + "'");
        if (h === undefined || h === rt.ObjectType.dict.get("__hash__")) {
            // Identity, unless the class redefines equality without a hash
            // (Python then sets __hash__ = None).
            const eqm = typeMethod(v, "__eq__");
            if (eqm !== undefined && eqm !== rt.ObjectType.dict.get("__eq__")) fail(E.TypeError, "unhashable type: '" + c.name + "'");
            return v;
        }
        if (h.isBase) return v.pyval !== undefined ? keyOf(v.pyval) : "\0" + baseKey(v);
        // A user hash shares the bucket of the int with that hash, so an
        // instance equal to an int key (through either side's __eq__) finds it.
        return intKey(hashBigInt(userHash(h, v, c)));
    }
    function userHash(h, v, c) {
        const r = call(descrGet(h, v, c), [], null);
        if (!isInt(r)) fail(E.TypeError, "__hash__ method should return an integer");
        return asInt(r);
    }
    function intKey(i) { const n = Number(i); return Number.isSafeInteger(n) ? n : "\0n" + i.toString(); }
    rt.keyOf = keyOf;
    // The bucket key as a string, for composing tuple keys and hashes.
    function keyStr(v) {
        const k = keyOf(v);
        switch (typeof k) {
            case "string": return k.charCodeAt(0) === 0 ? k.slice(1) : "s" + k;
            case "number": return "n" + k;
        }
        return k === NONE_KEY ? "N" : "o" + rt.ident(k);
    }
    rt.keyStr = keyStr;
    function baseKey(v) {
        if (isInstance(v, T.tuple)) { let s = "t("; for (const x of v.items) s += keyStr(x) + ","; return s + ")"; }
        if (isInstance(v, T.frozenset)) { const ks = []; for (const x of rt.setValues(v)) ks.push(keyStr(x)); ks.sort(); return "F{" + ks.join(",") + "}"; }
        if (isInstance(v, T.bytes)) return "b" + v.items.join(",");
        if (isInstance(v, T.range)) return "r" + v.start + ":" + v.stop + ":" + v.step;
        if (typeof v === "string" || isNum(v) || v === null) return keyStr(v);
        fail(E.TypeError, "unhashable type: '" + typeOf(v).name + "'");
    }
    rt.baseKey = baseKey;
    // A deterministic string hash from two 32-bit shift-add lanes (str
    // hashes are randomized in CPython, so only stability matters), mixed
    // and combined into a non-negative 53-bit value. The per-character step
    // is plain int arithmetic, which the interpreter runs without calls.
    function strHash(k) {
        let h1 = 5381, h2 = 0x6a09e667 | 0;
        for (let i = 0; i < k.length; i++) {
            const c = k.charCodeAt(i);
            h1 = ((h1 << 5) + h1 + c) | 0;
            h2 = ((h2 << 7) - h2) ^ c;
        }
        h1 = Math.imul(h1 ^ (h1 >>> 15), 0x2c1b3c6d); h2 = Math.imul(h2 ^ (h2 >>> 13), 0x297a2d39);
        return BigInt((h2 >>> 11) * 4294967296 + ((h1 ^ (h1 >>> 16)) >>> 0));
    }
    // CPython's numeric hash: the value modulo the Mersenne prime 2^61 - 1
    // with the sign kept, and -1 reserved (it becomes -2).
    const HASH_MODULUS = 2305843009213693951n;
    function hashBigInt(i) {
        let h;
        if (i > -HASH_MODULUS && i < HASH_MODULUS) h = i;
        else h = i < 0n ? -((-i) % HASH_MODULUS) : i % HASH_MODULUS;
        return h === -1n ? -2n : h;
    }
    const F64 = new Float64Array(1), F64_WORDS = new Uint32Array(F64.buffer);
    function hashFloat(x) {
        if (Number.isInteger(x)) return hashBigInt(BigInt(x));
        if (x !== x) return 0n;
        if (x === Infinity) return 314159n;
        if (x === -Infinity) return -314159n;
        // x = mantissa * 2^exp exactly; 2^61 = 1 mod the modulus, so the
        // exponent reduces modulo 61.
        F64[0] = x;
        const hi = F64_WORDS[1], lo = F64_WORDS[0], bits = (hi >>> 20) & 0x7ff;
        let mant = BigInt((hi & 0xfffff) * 4294967296 + lo), exp;
        if (bits === 0) exp = -1074; else { mant += 4503599627370496n; exp = bits - 1075; }
        const h = (mant << BigInt(((exp % 61) + 61) % 61)) % HASH_MODULUS;
        const r = x < 0 ? -h : h;
        return r === -1n ? -2n : r;
    }
    // CPython's tuple hash (xxHash-based tuplehash, 64-bit build).
    const XXPRIME_1 = 11400714785074694791n, XXPRIME_2 = 14029467366897019727n, XXPRIME_5 = 2870177450012600261n;
    function hashTuple(items) {
        let acc = XXPRIME_5;
        for (let i = 0; i < items.length; i++) {
            const lane = BigInt.asUintN(64, hashInt(items[i]));
            acc = BigInt.asUintN(64, acc + lane * XXPRIME_2);
            acc = ((acc << 31n) | (acc >> 33n)) & 0xFFFFFFFFFFFFFFFFn;
            acc = BigInt.asUintN(64, acc * XXPRIME_1);
        }
        acc = BigInt.asUintN(64, acc + (BigInt(items.length) ^ (XXPRIME_5 ^ 3527539n)));
        return acc === 0xFFFFFFFFFFFFFFFFn ? 1546275796n : BigInt.asIntN(64, acc);
    }
    rt.baseHash = function (v) { return isInstance(v, T.tuple) ? hashTuple(v.items) : strHash(baseKey(v)); };
    function hashInt(v) {
        // A stable, Python-shaped hash for the `hash()` builtin.
        const tv = typeof v;
        if (tv === "string") return strHash(v);
        if (tv === "bigint") return v === -1n ? -2n : v > -HASH_MODULUS && v < HASH_MODULUS ? v : hashBigInt(v);
        if (tv === "boolean") return v ? 1n : 0n;
        if (tv === "number") return hashFloat(v);
        if (v === null) return 4238894112n;
        const c = v.cls;
        if (c === T.tuple) return hashTuple(v.items);
        if (c !== undefined && c !== T.frozenset && c !== T.bytes && c !== T.range && c !== T.list && c !== T.dict && c !== T.set && c !== T.slice) {
            const h = typeMethod(v, "__hash__");
            if (h !== undefined && h !== null && !h.isBase && h !== rt.ObjectType.dict.get("__hash__")) return hashBigInt(userHash(h, v, c));
        }
        const k = keyOf(v);
        if (typeof k === "string") return isInstance(v, T.tuple) ? hashTuple(v.items) : strHash(keyStr(v));
        if (typeof k === "number") return hashBigInt(BigInt(k));
        if (k === NONE_KEY) return 4238894112n;
        return BigInt(rt.ident(k));
    }
    rt.hashInt = hashInt;

    // ---- dict ---------------------------------------------------------------------------------------------
    // {cls: T.dict, map, size, str, coll}. While every key is a str (`str`),
    // `map` is a plain Map<str, value>. The first other key converts it to
    // buckets, Map<bucket, Array<[key, value, seq]>>, which iterate in
    // creation order: insertion order while every bucket holds one key. Once
    // two unequal keys share a bucket (`coll`), iteration orders the entries
    // by their insertion stamp instead.
    let entrySeq = 0;
    function dict() { return { cls: T.dict, map: new Map(), size: 0, str: true }; }
    function toBuckets(d) {
        const old = d.map;
        d.map = new Map(); d.str = false;
        for (const [k, v] of old) d.map.set(k.charCodeAt(0) === 0 ? "\0s" + k : k, [[k, v, ++entrySeq]]);
    }
    function findEntry(bucket, key) {
        if (bucket.length === 1) { const k = bucket[0][0]; return k === key || eq(k, key) ? 0 : -1; }
        for (let i = 0; i < bucket.length; i++) { const k = bucket[i][0]; if (k === key || eq(k, key)) return i; }
        return -1;
    }
    function dictGet(d, key) {
        if (d.str === true) {
            if (typeof key === "string") return d.map.get(key);
            keyOf(key); // an unhashable key still raises
            return undefined;
        }
        const b = d.map.get(keyOf(key));
        if (b === undefined) return undefined;
        const i = findEntry(b, key);
        return i < 0 ? undefined : b[i][1];
    }
    function dictSet(d, key, value) {
        if (d.str === true && typeof key === "string") { d.map.set(key, value); d.size = d.map.size; return; }
        const k = keyOf(key);
        if (d.str === true) toBuckets(d);
        const b = d.map.get(k);
        if (b === undefined) { d.map.set(k, [[key, value, ++entrySeq]]); d.size++; return; }
        const i = findEntry(b, key);
        if (i < 0) { b.push([key, value, ++entrySeq]); d.size++; d.coll = true; } else b[i][1] = value;
    }
    function dictDel(d, key) {
        if (d.str === true) {
            if (typeof key !== "string") { keyOf(key); return false; }
            if (!d.map.delete(key)) return false;
            d.size--; return true;
        }
        const k = keyOf(key);
        const b = d.map.get(k);
        if (b === undefined) return false;
        const i = findEntry(b, key);
        if (i < 0) return false;
        b.splice(i, 1); d.size--;
        if (b.length === 0) d.map.delete(k);
        return true;
    }
    function dictClear(d) { d.map.clear(); d.size = 0; d.str = true; d.coll = false; }
    function dictHas(d, key) { return dictGet(d, key) !== undefined; }
    function* dictEntries(d) {
        if (d.str === true) { yield* d.map; return; }
        if (d.coll === true) { yield* dictEntryList(d); return; }
        for (const b of d.map.values()) for (const e of b) yield e;
    }
    function dictEntryList(d) {
        if (d.str === true) return Array.from(d.map);
        const out = []; for (const b of d.map.values()) for (const e of b) out.push(e);
        if (d.coll === true) out.sort((x, y) => x[2] - y[2]);
        return out;
    }
    function dictKeyIter(d) {
        const keys = d.str === true ? Array.from(d.map.keys()) : dictEntryList(d).map((e) => e[0]);
        let i = 0; const size = d.size;
        return { cls: T.iterator, next: () => {
            if (d.size !== size) fail(E.RuntimeError, "dictionary changed size during iteration");
            return i < keys.length ? keys[i++] : STOP;
        } };
    }
    function dictEq(a, b) {
        if (a.size !== b.size) return false;
        for (const [k, v] of dictEntries(a)) { const w = dictGet(b, k); if (w === undefined || !eq(v, w)) return false; }
        return true;
    }
    function dictCopy(d) { const out = dict(); for (const [k, v] of dictEntries(d)) dictSet(out, k, v); return out; }
    function dictFromMap(m) { const d = dict(); for (const [k, v] of m) dictSet(d, k, v); return d; }
    function mapFromDict(d) { const m = new Map(); for (const [k, v] of dictEntries(d)) m.set(typeof k === "string" ? k : rt.str(k), v); return m; }
    // A dict subclass that keeps dict's own __iter__ is read from its
    // storage (CPython's dict_merge fast path ignores __getitem__/keys()).
    function isPlainDictStorage(v) {
        if (v === null || typeof v !== "object" || v.map === undefined) return false;
        if (v.cls === T.dict) return true;
        if (!isInstance(v, T.dict)) return false;
        const it = typeMethod(v, "__iter__");
        return it !== undefined && it !== null && it.isBase === true;
    }
    rt.isPlainDictStorage = isPlainDictStorage;
    function asDict(v, message) {
        if (isPlainDictStorage(v)) return v;
        if (v !== null && typeof v === "object" && typeMethod(v, "keys") !== undefined) {
            const out = dict();
            const keys = callMethod(v, "keys", []);
            const it = rt.iter(keys);
            for (;;) { const k = rt.fornext(it); if (k === STOP) break; dictSet(out, k, rt.getitem(v, k)); }
            return out;
        }
        fail(E.TypeError, message || "'" + typeOf(v).name + "' object is not a mapping");
    }
    Object.assign(rt, { dict, dictGet, dictSet, dictDel, dictClear, dictHas, dictEntries, dictEntryList, dictKeyIter, dictEq, dictCopy, dictFromMap, mapFromDict, asDict });
    R.dict = function () { return dict(); };
    R.dictfill = function (d, keys, values) { for (let i = 0; i < keys.length; i++) dictSet(d, keys[i], values[i]); return null; };
    R.dictmerge = function (d, mapping) { for (const [k, v] of dictEntries(asDict(mapping))) dictSet(d, k, v); return d; };
    rt.mappingProxy = function (m) { return dictFromMap(m); };

    // ---- set ------------------------------------------------------------------------------------------------
    function set(type) { return { cls: type || T.set, map: new Map(), size: 0 }; }
    function setAdd(s, v) {
        const k = keyOf(v); const b = s.map.get(k);
        if (b === undefined) { s.map.set(k, [v]); s.size++; return; }
        for (const x of b) if (eq(x, v)) return;
        b.push(v); s.size++;
    }
    function setHas(s, v) {
        const b = s.map.get(keyOf(v)); if (b === undefined) return false;
        for (const x of b) if (eq(x, v)) return true; return false;
    }
    function setDel(s, v) {
        const k = keyOf(v); const b = s.map.get(k); if (b === undefined) return false;
        const i = rt.aindex(b, (x) => eq(x, v)); if (i < 0) return false;
        b.splice(i, 1); s.size--; if (b.length === 0) s.map.delete(k); return true;
    }
    function* setValues(s) { for (const x of setList(s)) yield x; }
    function setList(s) {
        const out = []; for (const b of s.map.values()) for (const x of b) out.push(x);
        // CPython iterates a set in hash-table order. Its table is sized by
        // the element count, and small non-negative ints hash to themselves,
        // so a set of such ints below the table size comes out ascending; the
        // common `{3, 1, 2}` prints `{1, 2, 3}` there and here.
        if (out.length > 1 && out.length <= 50000) {
            let size = 8, fill = 0;
            for (let i = 0; i < out.length; i++) { fill++; if (fill * 5 >= (size - 1) * 3) { let next = 8; while (next <= fill * 4) next *= 2; size = next; } }
            const bound = BigInt(size);
            let small = true;
            for (const x of out) { if (typeof x !== "bigint" || x < 0n || x >= bound) { small = false; break; } }
            if (small) out.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
        }
        return out;
    }
    function setIter(s) {
        const items = setList(s); let i = 0; const size = s.size;
        return { cls: T.iterator, next: () => { if (s.size !== size) fail(E.RuntimeError, "Set changed size during iteration"); return i < items.length ? items[i++] : STOP; } };
    }
    function setFrom(iterable, type) {
        const s = set(type); const it = rt.iter(iterable);
        for (;;) { const v = rt.fornext(it); if (v === STOP) break; setAdd(s, v); }
        return s;
    }
    function setEq(a, b) { if (a.size !== b.size) return false; for (const x of setValues(a)) if (!setHas(b, x)) return false; return true; }
    function setBinop(op, a, b, inplace) {
        const type = a.cls;
        let out;
        switch (op) {
            case "or": out = inplace ? a : setFrom(a, type); for (const x of setValues(b)) setAdd(out, x); return out;
            case "and": {
                // As CPython: walk the right operand unless it is the larger,
                // so equal elements of different types come from that side.
                const walk = b.size > a.size ? a : b, probe = walk === a ? b : a;
                out = set(type); for (const x of setValues(walk)) if (setHas(probe, x)) setAdd(out, x);
                if (inplace) { a.map = out.map; a.size = out.size; return a; } return out;
            }
            case "sub": out = set(type); for (const x of setValues(a)) if (!setHas(b, x)) setAdd(out, x); if (inplace) { a.map = out.map; a.size = out.size; return a; } return out;
            case "xor": out = set(type); for (const x of setValues(a)) if (!setHas(b, x)) setAdd(out, x); for (const x of setValues(b)) if (!setHas(a, x)) setAdd(out, x); if (inplace) { a.map = out.map; a.size = out.size; return a; } return out;
        }
        return NOTIMPL;
    }
    function setCompare(op, a, b) {
        const sub = (x, y) => { for (const v of setValues(x)) if (!setHas(y, v)) return false; return true; };
        switch (op) {
            case "le": return sub(a, b);
            case "lt": return a.size < b.size && sub(a, b);
            case "ge": return sub(b, a);
            case "gt": return a.size > b.size && sub(b, a);
        }
        return false;
    }
    Object.assign(rt, { set, setAdd, setHas, setDel, setValues, setList, setIter, setFrom, setEq, setBinop, setCompare });
    R.set = function (items) { const s = set(); if (items !== undefined) for (const x of items) setAdd(s, x); return s; };

    // ---- range, slice, bytes ---------------------------------------------------------------------------------
    function rangeLength(r) {
        if (r.step > 0n) return r.start >= r.stop ? 0n : (r.stop - r.start - 1n) / r.step + 1n;
        return r.start <= r.stop ? 0n : (r.start - r.stop - 1n) / (-r.step) + 1n;
    }
    rt.rangeLength = rangeLength;
    rt.range = function (start, stop, step) { return { cls: T.range, start: start, stop: stop, step: step }; };
    R.slice = function (lo, hi, step) { return { cls: T.slice, start: lo, stop: hi, step: step }; };
    R.bytes = function (items) { return { cls: T.bytes, items: items }; };
    rt.bytes = R.bytes;
    // Resolve a slice against a length: [start, stop, step, count].
    function sliceIndices(s, length) {
        const step = s.step === null ? 1 : Number(rt.indexOf(s.step, "slice"));
        if (step === 0) fail(E.ValueError, "slice step cannot be zero");
        const clamp = (v, dflt) => {
            if (v === null) return dflt;
            let i = Number(rt.indexOf(v, "slice"));
            if (i < 0) { i += length; if (i < 0) i = step < 0 ? -1 : 0; }
            else if (i >= length) i = step < 0 ? length - 1 : length;
            return i;
        };
        const start = clamp(s.start, step < 0 ? length - 1 : 0);
        const stop = clamp(s.stop, step < 0 ? -1 : length);
        let count = 0;
        if (step > 0) count = stop > start ? Math.ceil((stop - start) / step) : 0;
        else count = start > stop ? Math.ceil((start - stop) / -step) : 0;
        return [start, stop, step, count];
    }
    rt.sliceIndices = sliceIndices;
    function sliceArray(items, s) {
        const [start, , step, count] = sliceIndices(s, items.length);
        if (step === 1) return items.slice(start, start + count);
        const out = new Array(count);
        for (let i = 0, j = start; i < count; i++, j += step) out[i] = items[j];
        return out;
    }
    rt.sliceArray = sliceArray;
    function codepoints(s) { return hasSurrogate(s) ? Array.from(s) : s.split(""); }
    rt.codepoints = codepoints;
    function hasSurrogate(s) {
        const n = s.length;
        if (n < 48) {
            for (let i = 0; i < n; i++) { const c = s.charCodeAt(i); if (c >= 0xd800 && c <= 0xdfff) return true; }
            return false;
        }
        return /[\ud800-\udfff]/.test(s);
    }
    rt.hasSurrogate = hasSurrogate;
    // The length in code points.
    function strLen(s) {
        if (!hasSurrogate(s)) return s.length;
        let n = 0; for (let i = 0; i < s.length; i++) { const c = s.charCodeAt(i); if (c < 0xdc00 || c > 0xdfff) n++; }
        return n;
    }
    rt.strLen = strLen;
    function normIndex(i, length, what) {
        let n = Number(rt.indexOf(i, what));
        if (n < 0) n += length;
        if (n < 0 || n >= length) fail(E.IndexError, what + " index out of range");
        return n;
    }
    rt.indexOf = function (v, what) {
        if (isInt(v)) return asInt(v);
        const m = typeMethod(v, "__index__");
        if (m !== undefined) return asInt(call(descrGet(m, v, typeOf(v)), [], null));
        fail(E.TypeError, (what || "sequence") + " indices must be integers or slices, not " + typeOf(v).name);
    };
    function isSlice(k) { return k !== null && typeof k === "object" && k.cls === T.slice; }
    function baseGetitem(o, k) {
        if (typeof o === "string") {
            if (isSlice(k)) return sliceArray(codepoints(o), k).join("");
            if (!hasSurrogate(o)) return o[normIndex(k, o.length, "string")];
            const cps = Array.from(o); return cps[normIndex(k, cps.length, "string")];
        }
        if (o.items !== undefined && (isInstance(o, T.list) || isInstance(o, T.tuple))) {
            const base = isInstance(o, T.list) ? T.list : T.tuple;
            if (isSlice(k)) return sequence(base, sliceArray(o.items, k));
            return o.items[normIndex(k, o.items.length, base.name)];
        }
        if (o.map !== undefined && isInstance(o, T.dict)) {
            const v = dictGet(o, k);
            if (v !== undefined) return v;
            const miss = typeMethod(o, "__missing__");
            if (miss !== undefined) return call(descrGet(miss, o, o.cls), [k], null);
            throw rt.makeExc(E.KeyError, [k]);
        }
        if (o.pyval !== undefined) return baseGetitem(o.pyval, k);
        if (isInstance(o, T.range)) {
            if (isSlice(k)) {
                const [start, , step, count] = sliceIndices(k, Number(rangeLength(o)));
                const s = o.start + BigInt(start) * o.step; const st = o.step * BigInt(step);
                return rt.range(s, s + st * BigInt(count), st);
            }
            const n = rangeLength(o); let i = rt.indexOf(k, "range"); if (i < 0n) i += n;
            if (i < 0n || i >= n) fail(E.IndexError, "range object index out of range");
            return o.start + i * o.step;
        }
        if (isInstance(o, T.bytes)) {
            if (isSlice(k)) return { cls: T.bytes, items: sliceArray(o.items, k) };
            return BigInt(o.items[normIndex(k, o.items.length, "bytes")]);
        }
        fail(E.TypeError, "'" + typeOf(o).name + "' object is not subscriptable");
    }
    function getitem(o, k) {
        if (typeof o === "string") return baseGetitem(o, k);
        if (o !== null && typeof o === "object") {
            const c = o.cls;
            if (c === T.dict) {
                const v = dictGet(o, k);
                if (v !== undefined) return v;
                throw rt.makeExc(E.KeyError, [k]);
            }
            if ((c === T.list || c === T.tuple) && typeof k === "bigint") {
                const items = o.items; let i = Number(k); if (i < 0) i += items.length;
                if (i < 0 || i >= items.length) fail(E.IndexError, c.name + " index out of range");
                return items[i];
            }
            if (c === T.list || c === T.tuple || c === T.range || c === T.bytes) return baseGetitem(o, k);
            if (c === T.dict_keys || c === T.dict_values || c === T.dict_items) fail(E.TypeError, "'" + c.name + "' object is not subscriptable");
            if (o.isType) {
                const cg = rt.classDunder(o, "__getitem__") || rt.classDunder(o, "__class_getitem__");
                if (cg !== undefined) return call(cg, [k], null);
                return o; // generic aliases: list[int] -> list
            }
            const m = typeMethod(o, "__getitem__");
            if (m !== undefined) { if (m.isBase) return baseGetitem(o, k); return call(descrGet(m, o, c), [k], null); }
        }
        fail(E.TypeError, "'" + typeOf(o).name + "' object is not subscriptable");
    }
    function baseSetitem(o, k, v) {
        if (o.items !== undefined && isInstance(o, T.list)) {
            if (isSlice(k)) {
                const [start, stop, step, count] = sliceIndices(k, o.items.length);
                const values = R.extend([], v);
                if (step === 1) { o.items.splice(start, Math.max(0, stop - start), ...values); return null; }
                if (values.length !== count) fail(E.ValueError, "attempt to assign sequence of size " + values.length + " to extended slice of size " + count);
                for (let i = 0, j = start; i < count; i++, j += step) o.items[j] = values[i];
                return null;
            }
            o.items[normIndex(k, o.items.length, "list assignment")] = v; return null;
        }
        if (o.map !== undefined && isInstance(o, T.dict)) { dictSet(o, k, v); return null; }
        if (isInstance(o, T.tuple) || typeof o === "string") fail(E.TypeError, "'" + typeOf(o).name + "' object does not support item assignment");
        fail(E.TypeError, "'" + typeOf(o).name + "' object does not support item assignment");
    }
    function setitem(o, k, v) {
        if (o !== null && typeof o === "object") {
            const c = o.cls;
            if (c === T.dict) { dictSet(o, k, v); return null; }
            if (c === T.list) {
                if (typeof k === "bigint") {
                    const items = o.items; let i = Number(k); if (i < 0) i += items.length;
                    if (i < 0 || i >= items.length) fail(E.IndexError, "list assignment index out of range");
                    items[i] = v; return null;
                }
                return baseSetitem(o, k, v);
            }
            const m = typeMethod(o, "__setitem__");
            if (m !== undefined) { if (m.isBase) return baseSetitem(o, k, v); call(descrGet(m, o, c), [k, v], null); return null; }
        }
        fail(E.TypeError, "'" + typeOf(o).name + "' object does not support item assignment");
    }
    function baseDelitem(o, k) {
        if (o.items !== undefined && isInstance(o, T.list)) {
            if (isSlice(k)) {
                const [start, stop, step, count] = sliceIndices(k, o.items.length);
                if (step === 1) { o.items.splice(start, Math.max(0, stop - start)); return null; }
                const drop = new Set(); for (let i = 0, j = start; i < count; i++, j += step) drop.add(j);
                o.items = o.items.filter((_, i) => !drop.has(i)); return null;
            }
            o.items.splice(normIndex(k, o.items.length, "list assignment"), 1); return null;
        }
        if (o.map !== undefined && isInstance(o, T.dict)) { if (!dictDel(o, k)) throw rt.makeExc(E.KeyError, [k]); return null; }
        fail(E.TypeError, "'" + typeOf(o).name + "' object doesn't support item deletion");
    }
    function delitem(o, k) {
        if (o !== null && typeof o === "object") {
            const c = o.cls;
            if (c === T.list || c === T.dict) return baseDelitem(o, k);
            const m = typeMethod(o, "__delitem__");
            if (m !== undefined) { if (m.isBase) return baseDelitem(o, k); call(descrGet(m, o, c), [k], null); return null; }
        }
        fail(E.TypeError, "'" + typeOf(o).name + "' object doesn't support item deletion");
    }
    R.getitem = getitem; R.setitem = setitem; R.delitem = delitem;
    rt.getitem = getitem; rt.setitem = setitem; rt.delitem = delitem;
    rt.baseGetitem = baseGetitem; rt.baseSetitem = baseSetitem; rt.baseDelitem = baseDelitem;
    function baseLen(v) {
        if (typeof v === "string") return BigInt(strLen(v));
        if (v.items !== undefined) return BigInt(v.items.length);
        if (v.map !== undefined) return BigInt(v.size);
        // A boxed str subclass instance.
        if (v.pyval !== undefined) return baseLen(v.pyval);
        if (isInstance(v, T.range)) return rangeLength(v);
        fail(E.TypeError, "object of type '" + typeOf(v).name + "' has no len()");
    }
    rt.baseLen = baseLen;
    function len(v) {
        if (typeof v === "string") return BigInt(strLen(v));
        if (v !== null && typeof v === "object") {
            const c = v.cls;
            if (c === T.list || c === T.tuple) return BigInt(v.items.length);
            if (c === T.dict || c === T.set) return BigInt(v.size);
            if (c === T.bytes || c === T.frozenset || c === T.range) return baseLen(v);
            if (c === T.dict_keys || c === T.dict_values || c === T.dict_items) return BigInt(v.dict.size);
            if (v.isType) { const cl = rt.classDunder(v, "__len__"); if (cl !== undefined) return asInt(call(cl, [], null)); }
            const m = typeMethod(v, "__len__");
            if (m !== undefined) {
                if (m.isBase) return baseLen(v);
                const r = call(descrGet(m, v, c), [], null);
                if (!isInt(r)) fail(E.TypeError, "'" + typeOf(r).name + "' object cannot be interpreted as an integer");
                if (asInt(r) < 0n) fail(E.ValueError, "__len__() should return >= 0");
                return asInt(r);
            }
        }
        fail(E.TypeError, "object of type '" + typeOf(v).name + "' has no len()");
    }
    rt.len = len;

    // ---- str / repr -------------------------------------------------------------------------------------------
    function floatRepr(x) {
        if (Number.isNaN(x)) return "nan";
        if (x === Infinity) return "inf";
        if (x === -Infinity) return "-inf";
        if (x === 0) return Object.is(x, -0) ? "-0.0" : "0.0";
        const e = x.toExponential();                     // shortest round-trip digits
        const m = /^(-?)(\d)(?:\.(\d+))?e([+-]\d+)$/.exec(e);
        const sign = m[1], digits = m[2] + (m[3] || ""), exp = parseInt(m[4], 10);
        if (exp >= 16 || exp < -4) {
            const mant = digits.length > 1 ? digits[0] + "." + digits.slice(1) : digits;
            return sign + mant + "e" + (exp < 0 ? "-" : "+") + String(Math.abs(exp)).padStart(2, "0");
        }
        if (exp >= 0) {
            if (digits.length <= exp + 1) return sign + digits + "0".repeat(exp + 1 - digits.length) + ".0";
            return sign + digits.slice(0, exp + 1) + "." + digits.slice(exp + 1);
        }
        return sign + "0." + "0".repeat(-exp - 1) + digits;
    }
    rt.floatRepr = floatRepr;
    // Code points repr() escapes: not printable in CPython's sense (the
    // categories Cc Cf Cs Co Cn Zl Zp, and Zs other than the space).
    const NONPRINTABLE = /[\p{C}\p{Zl}\p{Zp}\u{a0}\u{1680}\u{2000}-\u{200a}\u{202f}\u{205f}\u{3000}]/u;
    const NONPRINTABLE_CHAR = /^[\p{C}\p{Zl}\p{Zp}\u{a0}\u{1680}\u{2000}-\u{200a}\u{202f}\u{205f}\u{3000}]$/u;
    rt.NONPRINTABLE = NONPRINTABLE;
    function hexEscape(cp) {
        return cp < 0x100 ? "\\x" + cp.toString(16).padStart(2, "0") : cp < 0x10000 ? "\\u" + cp.toString(16).padStart(4, "0") : "\\U" + cp.toString(16).padStart(8, "0");
    }
    rt.hexEscape = hexEscape;
    function quoteStr(s) {
        const hasSingle = s.includes("'");
        const useDouble = hasSingle && !s.includes('"');
        const q = useDouble ? '"' : "'";
        if (!(hasSingle && !useDouble) && !s.includes("\\") && !NONPRINTABLE.test(s)) return q + s + q;
        let out = q;
        for (const ch of s) {
            if (ch === "\\") out += "\\\\";
            else if (ch === q) out += "\\" + q;
            else if (ch === "\n") out += "\\n";
            else if (ch === "\r") out += "\\r";
            else if (ch === "\t") out += "\\t";
            else if (ch.charCodeAt(0) >= 0x20 && ch.charCodeAt(0) < 0x7f) out += ch;
            else if (NONPRINTABLE_CHAR.test(ch)) out += hexEscape(ch.codePointAt(0));
            else out += ch;
        }
        return out + q;
    }
    rt.quoteStr = quoteStr;
    const reprStack = [];
    function repr(v) {
        if (v === null) return "None";
        const tv = typeof v;
        if (tv !== "object") {
            if (tv === "bigint") return v.toString();
            if (tv === "string") return quoteStr(v);
            if (tv === "number") return floatRepr(v);
            if (tv === "boolean") return v ? "True" : "False";
            if (tv === "undefined") return "None";
            if (tv === "function") return "<built-in function>";
        }
        if (v === NOTIMPL) return "NotImplemented";
        if (v === rt.ELLIPSIS) return "Ellipsis";
        const c = v.cls;
        if (c === undefined) return "<js object>";
        if (c === T.list || c === T.tuple || c === T.dict || c === T.set || c === T.frozenset) return baseRepr(v);
        if (c === T.range) return "range(" + v.start + ", " + v.stop + (v.step === 1n ? "" : ", " + v.step) + ")";
        if (c === T.slice) return "slice(" + repr(v.start) + ", " + repr(v.stop) + ", " + repr(v.step) + ")";
        if (c === T.function) return "<function " + v.qualname + " at 0x" + rt.ident(v).toString(16).padStart(8, "0") + ">";
        if (c === T.builtin_function_or_method) return "<built-in function " + v.name + ">";
        if (c === T.method) return "<bound method " + (v.func.qualname || v.func.name) + " of " + repr(v.self) + ">";
        if (c === T.module) return "<module '" + v.name + "'" + (v.file ? " from '" + v.file + "'" : " (built-in)") + ">";
        if (c === T.generator) return "<generator object " + v.qualname + " at 0x" + rt.ident(v).toString(16).padStart(8, "0") + ">";
        if (c === T.bytes) return rt.bytesRepr(v);
        if (c === T.cell) return "<cell>";
        if (c === T.dict_keys || c === T.dict_values || c === T.dict_items) { const parts = []; const it = v.iter(); for (;;) { const x = it.next(); if (x === STOP) break; parts.push(repr(x)); } return c.name + "([" + parts.join(", ") + "])"; }
        if (v.isType) return "<class '" + (v.module === "builtins" ? "" : v.module + ".") + v.qualname + "'>";
        const m = typeMethod(v, "__repr__");
        if (m !== undefined && m !== rt.ObjectType.dict.get("__repr__")) {
            const r = call(descrGet(m, v, c), [], null);
            if (typeof r !== "string") fail(E.TypeError, "__repr__ returned non-string (type " + typeOf(r).name + ")");
            return r;
        }
        if (isInstance(v, E.BaseException)) {
            return c.name + "(" + v.args.items.map(repr).join(", ") + ")";
        }
        return "<" + (c.module === "builtins" ? "" : c.module + ".") + c.qualname + " object at 0x" + rt.ident(v).toString(16).padStart(8, "0") + ">";
    }
    // The builtin containers' own repr, by base type: used for them and for
    // subclasses that do not override __repr__ (never back through `repr`
    // on the container itself, which would recurse).
    function baseRepr(v) {
        if (isInstance(v, T.list) || isInstance(v, T.tuple)) {
            const isList = isInstance(v, T.list);
            if (reprStack.indexOf(v) >= 0) return isList ? "[...]" : "(...)";
            reprStack.push(v);
            try {
                const parts = v.items.map(repr);
                if (!isList) return "(" + parts.join(", ") + (parts.length === 1 ? ",)" : ")");
                return "[" + parts.join(", ") + "]";
            } finally { reprStack.pop(); }
        }
        if (isInstance(v, T.dict)) {
            if (reprStack.indexOf(v) >= 0) return "{...}";
            reprStack.push(v);
            try { const parts = []; for (const [k, x] of dictEntries(v)) parts.push(repr(k) + ": " + repr(x)); return "{" + parts.join(", ") + "}"; }
            finally { reprStack.pop(); }
        }
        if (isInstance(v, T.set) || isInstance(v, T.frozenset)) {
            const frozen = isInstance(v, T.frozenset);
            const name = v.cls === T.set || v.cls === T.frozenset ? (frozen ? "frozenset" : "set") : v.cls.name;
            if (v.size === 0) return name + "()";
            const parts = []; for (const x of setValues(v)) parts.push(repr(x));
            return frozen || v.cls !== T.set ? name + "({" + parts.join(", ") + "})" : "{" + parts.join(", ") + "}";
        }
        return "<" + typeOf(v).name + " object>";
    }
    rt.baseRepr = baseRepr;
    function str(v) {
        if (typeof v === "string") return v;
        if (v !== null && typeof v === "object" && v.cls !== undefined && !v.isType) {
            const m = typeMethod(v, "__str__");
            if (m !== undefined && m !== rt.ObjectType.dict.get("__str__")) {
                const r = call(descrGet(m, v, v.cls), [], null);
                if (typeof r !== "string") fail(E.TypeError, "__str__ returned non-string (type " + typeOf(r).name + ")");
                return r;
            }
            if (isInstance(v, E.BaseException)) {
                const a = v.args.items;
                return a.length === 0 ? "" : a.length === 1 ? str(a[0]) : repr(v.args);
            }
        }
        return repr(v);
    }
    rt.setStrRepr(str, repr);
    rt.str = str; rt.repr = repr;
    R.strjoin = function (parts) { let out = ""; for (const p of parts) out += p; return rt.checkedText(out); };

    // ---- attributes of primitives and other special objects --------------------------------------------------------
    rt.specialAttr = function (obj, t, name) {
        if (obj !== null && typeof obj === "object") {
            const c = obj.cls;
            if (c === T.function) {
                switch (name) {
                    case "__name__": return obj.name;
                    case "__qualname__": return obj.qualname;
                    case "__doc__": return obj.doc;
                    case "__module__": return obj.module;
                    case "__defaults__": return obj.defaults.length ? tuple(obj.defaults.slice()) : null;
                    case "__dict__": return rt.instanceDict(obj);
                    case "__globals__": return dictFromMap(obj.globals);
                    case "__code__": return { cls: T.code, name: obj.name, argcount: obj.positional, varnames: obj.argnames };
                    case "__wrapped__": return obj.dict.get("__wrapped__");
                    case "__annotations__": return dictFromMap(obj.annotations || new Map());
                    case "__kwdefaults__": return obj.kwdefaults === null ? null : dictFromMap(obj.kwdefaults);
                }
            } else if (c === T.builtin_function_or_method) {
                if (name === "__name__" || name === "__qualname__") return obj.name;
                if (name === "__doc__") return null;
                if (name === "__module__") return "builtins";
            } else if (c === T.method) {
                if (name === "__self__") return obj.self;
                if (name === "__func__") return obj.func;
                if (name === "__name__" || name === "__qualname__" || name === "__doc__" || name === "__module__") return rt.getattr(obj.func, name);
            } else if (c === T.generator) {
                if (name === "__name__") return obj.name;
                if (name === "__qualname__") return obj.qualname;
            } else if (c === T.code) {
                if (name === "co_name") return obj.name;
                if (name === "co_argcount") return BigInt(obj.argcount);
                if (name === "co_varnames") return tuple(obj.varnames.slice());
            } else if (c === T.property) {
                if (name === "fget") return obj.fget; if (name === "fset") return obj.fset; if (name === "fdel") return obj.fdel; if (name === "__doc__") return obj.doc;
            } else if (c === T.slice) {
                if (name === "start") return obj.start; if (name === "stop") return obj.stop; if (name === "step") return obj.step;
            } else if (c === T.range) {
                if (name === "start") return obj.start; if (name === "stop") return obj.stop; if (name === "step") return obj.step;
            } else if (c === T.staticmethod || c === T.classmethod) {
                if (name === "__func__") return obj.func;
            } else if (isInstance(obj, E.BaseException)) {
                if (name === "args") return obj.args;
                if (name === "value" && isInstance(obj, E.StopIteration)) return obj.args.items.length ? obj.args.items[0] : null;
                if (name === "code" && isInstance(obj, E.SystemExit)) return obj.args.items.length ? obj.args.items[0] : null;
                if (name === "__cause__") return obj.cause;
                if (name === "__context__") return obj.context;
                if (name === "__traceback__") return null;
                if (name === "__suppress_context__") return obj.suppress;
            }
        } else if (isNum(obj)) {
            if (name === "real") return obj;
            if (name === "imag") return isInt(obj) ? 0n : 0.0;
            if (name === "numerator" && isInt(obj)) return asInt(obj);
            if (name === "denominator" && isInt(obj)) return 1n;
        }
        if (name === "__doc__") { const d = rt.lookupType(t, "__doc__"); return d === undefined ? null : d; }
        if (name === "__module__" && obj !== null && typeof obj === "object" && obj.cls) return obj.cls.module;
        return undefined;
    };
})(__zipp_py);
