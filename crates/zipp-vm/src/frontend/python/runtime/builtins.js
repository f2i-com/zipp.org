/* ZIPP Python runtime — builtin functions, type constructors, methods of the
 * builtin types, and string formatting. Apache-2.0. */
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T, E = rt.E, STOP = rt.STOP, NOTIMPL = rt.NOTIMPL, ObjectType = rt.ObjectType, TypeType = rt.TypeType;
    const fail = rt.fail, typeOf = rt.typeOf, isType = rt.isType, isInstance = rt.isInstance, isSubclass = rt.isSubclass;
    const list = rt.list, tuple = rt.tuple, sequence = rt.sequence, call = rt.call, iter = rt.iter, fornext = rt.fornext;
    const str = rt.str, repr = rt.repr, eq = rt.eq, cmp = rt.cmp, truth = rt.truth, len = rt.len, getitem = rt.getitem;
    const isInt = rt.isInt, asInt = rt.asInt, isNum = rt.isNum, toFloat = rt.toFloat, codepoints = rt.codepoints;
    const dict = rt.dict, dictGet = rt.dictGet, dictSet = rt.dictSet, dictDel = rt.dictDel, dictEntries = rt.dictEntries, dictEntryList = rt.dictEntryList;
    const set = rt.set, setAdd = rt.setAdd, setHas = rt.setHas, setDel = rt.setDel, setValues = rt.setValues, setList = rt.setList, setFrom = rt.setFrom;
    const builtin = rt.builtin, typeMethod = rt.typeMethod, descrGet = rt.descrGet, callMethod = rt.callMethod, keyOf = rt.keyOf;
    const B = rt.builtins;
    function def(name, arity, code, minArity) { B.set(name, builtin(name, arity, code, minArity)); }
    function defkw(name, code) { const f = builtin(name, -1, code); f.kwnames = true; B.set(name, f); }
    function method(type, name, arity, code, minArity) { type.dict.set(name, builtin(name, arity, code, minArity)); }
    function methodkw(type, name, code) { const f = builtin(name, -1, code); f.kwnames = true; type.dict.set(name, f); }
    function drain(iterable) { const out = []; const it = iter(iterable); for (;;) { const v = fornext(it); if (v === STOP) break; out.push(v); } return out; }
    rt.drain = drain;
    function kwOf(args, allowed) {
        // Builtins accepting keywords receive them as a trailing Map.
        const last = args[args.length - 1];
        if (last instanceof Map) {
            args.pop();
            if (allowed !== null) for (const k of last.keys()) if (allowed.indexOf(k) < 0) fail(E.TypeError, "'" + k + "' is an invalid keyword argument");
            return last;
        }
        return null;
    }
    function kwget(kw, name, dflt) { if (kw === null) return dflt; const v = kw.get(name); return v === undefined ? dflt : v; }
    function needInt(v, what) { if (isInt(v)) return asInt(v); fail(E.TypeError, "'" + typeOf(v).name + "' object cannot be interpreted as an integer" + (what ? " (" + what + ")" : "")); }
    function needStr(v, what) { if (typeof v === "string") return v; fail(E.TypeError, (what || "argument") + " must be str, not " + typeOf(v).name); }
    function toIndex(v) { return Number(rt.indexOf(v)); }
    rt.needInt = needInt; rt.needStr = needStr; rt.kwOf = kwOf; rt.kwget = kwget;

    // ---- object ------------------------------------------------------------------------------------------------
    method(ObjectType, "__init__", -1, function (args) { return null; });
    ObjectType.dict.set("__new__", { cls: T.staticmethod, func: builtin("__new__", -1, function (args) {
        const cls = args[0]; if (!isType(cls)) fail(E.TypeError, "object.__new__(X): X is not a type object");
        return rt.allocInstance(cls);
    }) });
    method(ObjectType, "__eq__", 2, function (args) { return args[0] === args[1] ? true : NOTIMPL; });
    method(ObjectType, "__ne__", 2, function (args) { const r = rt.callMethod(args[0], "__eq__", [args[1]]); return r === NOTIMPL ? NOTIMPL : !truth(r); });
    method(ObjectType, "__hash__", 1, function (args) { return BigInt(rt.ident(args[0])); });
    method(ObjectType, "__repr__", 1, function (args) { const v = args[0]; const c = typeOf(v); return "<" + (c.module === "builtins" ? "" : c.module + ".") + c.qualname + " object at 0x" + rt.ident(v).toString(16).padStart(8, "0") + ">"; });
    method(ObjectType, "__str__", 1, function (args) { return repr(args[0]); });
    method(ObjectType, "__setattr__", 3, function (args) { const o = args[0]; if (o === null || typeof o !== "object" || o.dict === undefined) fail(E.AttributeError, "can't set attribute"); o.dict.set(args[1], args[2]); return null; });
    method(ObjectType, "__getattribute__", 2, function (args) { return rt.getattr(args[0], args[1]); });
    method(ObjectType, "__delattr__", 2, function (args) { const o = args[0]; if (!o.dict.delete(args[1])) fail(E.AttributeError, args[1]); return null; });
    method(ObjectType, "__format__", 2, function (args) { if (args[1] !== "") fail(E.TypeError, "unsupported format string passed to " + typeOf(args[0]).name + ".__format__"); return str(args[0]); });
    method(ObjectType, "__init_subclass__", -1, function () { return null; });
    method(ObjectType, "__dir__", 1, function (args) { return list(dirOf(args[0])); });
    method(ObjectType, "__reduce__", 1, function () { fail(E.TypeError, "cannot pickle"); });
    ObjectType.dict.set("__class__", { cls: T.property, fget: builtin("__class__", 1, (a) => typeOf(a[0])), fset: null, fdel: null, doc: null });
    method(ObjectType, "__sizeof__", 1, function () { return 64n; });
    function dirOf(v) {
        const names = new Set();
        const t = typeOf(v);
        for (const c of t.mro) for (const k of c.dict.keys()) names.add(k);
        if (v !== null && typeof v === "object" && v.dict) for (const k of v.dict.keys()) names.add(k);
        if (v !== null && typeof v === "object" && v.isType) for (const c of v.mro) for (const k of c.dict.keys()) names.add(k);
        if (v !== null && typeof v === "object" && v.cls === T.module) for (const k of v.globals.keys()) names.add(k);
        return Array.from(names).sort();
    }
    // Exceptions: BaseException.__init__ stores args.
    method(E.BaseException, "__init__", -1, function (args) {
        const self = args[0]; self.args = tuple(args.slice(1)); rt.refreshExc(self); return null;
    });
    method(E.BaseException, "__str__", 1, function (args) { const a = args[0].args.items; return a.length === 0 ? "" : a.length === 1 ? str(a[0]) : repr(args[0].args); });
    method(E.KeyError, "__str__", 1, function (args) { const a = args[0].args.items; return a.length === 1 ? repr(a[0]) : a.length === 0 ? "" : repr(args[0].args); });
    method(E.BaseException, "__repr__", 1, function (args) { return typeOf(args[0]).name + "(" + args[0].args.items.map(repr).join(", ") + ")"; });
    method(E.BaseException, "with_traceback", 2, function (args) { return args[0]; });
    method(E.BaseException, "add_note", 2, function (args) { return null; });
    for (const [name, exc] of Object.entries(E)) {
        rt.allocators.set(exc, (cls) => rt.makeExc(cls, []));
        B.set(name, exc);
    }
    B.set("EnvironmentError", E.OSError); B.set("IOError", E.OSError);
    // Constructing an exception directly: Exception("msg") -> instance with args.
    rt.constructors.set(E.BaseException, null);
    rt.constructors.delete(E.BaseException);

    // ---- type / super / property / staticmethod / classmethod -------------------------------------------------------
    B.set("object", ObjectType); B.set("type", TypeType);
    for (const name of ["int", "float", "str", "bool", "list", "tuple", "dict", "set", "frozenset", "range", "bytes", "slice", "property", "staticmethod", "classmethod", "super", "enumerate", "zip", "map", "filter", "reversed"]) B.set(name, T[name]);
    B.set("NotImplemented", NOTIMPL); B.set("Ellipsis", rt.ELLIPSIS); B.set("None", null); B.set("True", true); B.set("False", false);
    method(TypeType, "__call__", -1, function (args) { return rt.construct(args[0], args.slice(1), null); });
    method(TypeType, "__repr__", 1, function (args) { return repr(args[0]); });
    method(TypeType, "mro", 1, function (args) { return list(args[0].mro.slice()); });
    method(TypeType, "__subclasses__", 1, function () { return list([]); });
    method(TypeType, "__instancecheck__", 2, function (args) { return isInstance(args[1], args[0]); });
    rt.constructors.set(T.property, (args, kw) => {
        const k = kw; const get = (n, i) => { if (k && k.has(n)) return k.get(n); return args[i] === undefined ? null : args[i]; };
        return { cls: T.property, fget: get("fget", 0), fset: get("fset", 1), fdel: get("fdel", 2), doc: get("doc", 3) };
    });
    method(T.property, "getter", 2, (a) => ({ cls: T.property, fget: a[1], fset: a[0].fset, fdel: a[0].fdel, doc: a[0].doc }));
    method(T.property, "setter", 2, (a) => ({ cls: T.property, fget: a[0].fget, fset: a[1], fdel: a[0].fdel, doc: a[0].doc }));
    method(T.property, "deleter", 2, (a) => ({ cls: T.property, fget: a[0].fget, fset: a[0].fset, fdel: a[1], doc: a[0].doc }));
    rt.constructors.set(T.staticmethod, (args) => ({ cls: T.staticmethod, func: args[0] }));
    rt.constructors.set(T.classmethod, (args) => ({ cls: T.classmethod, func: args[0] }));
    rt.constructors.set(T.super, (args) => {
        if (args.length !== 2) fail(E.RuntimeError, "super(): use super() or super(type, obj)");
        return R.superof(args[0], args[1]);
    });
    method(T.super, "__repr__", 1, (a) => "<super: <class '" + a[0].type.name + "'>, <" + typeOf(a[0].obj).name + " object>>");

    // ---- numbers ---------------------------------------------------------------------------------------------------
    function parseIntLiteral(s, base) {
        let t = s.trim().replace(/_/g, "");
        let neg = false;
        if (t[0] === "+" || t[0] === "-") { neg = t[0] === "-"; t = t.slice(1); }
        if (base === 0) {
            const p = t.slice(0, 2).toLowerCase();
            base = p === "0x" ? 16 : p === "0o" ? 8 : p === "0b" ? 2 : 10;
            if (base !== 10) t = t.slice(2);
        } else if (base === 16 && /^0x/i.test(t)) t = t.slice(2);
        else if (base === 8 && /^0o/i.test(t)) t = t.slice(2);
        else if (base === 2 && /^0b/i.test(t)) t = t.slice(2);
        if (t.length === 0 || !/^[0-9a-z]+$/i.test(t)) fail(E.ValueError, "invalid literal for int() with base " + base + ": " + repr(s));
        let v = 0n; const big = BigInt(base);
        for (const ch of t.toLowerCase()) {
            const d = parseInt(ch, 36);
            if (Number.isNaN(d) || d >= base) fail(E.ValueError, "invalid literal for int() with base " + base + ": " + repr(s));
            v = v * big + BigInt(d);
        }
        return neg ? -v : v;
    }
    rt.constructors.set(T.int, (args, kw) => {
        const k = kw; let base = 10;
        if (k && k.has("base")) base = Number(needInt(k.get("base")));
        if (args.length === 0) return 0n;
        if (args.length === 2) base = Number(needInt(args[1]));
        const v = args[0];
        if (typeof v === "string") return parseIntLiteral(v, base);
        if (args.length === 2 || (k && k.has("base"))) fail(E.TypeError, "int() can't convert non-string with explicit base");
        if (isInt(v)) return asInt(v);
        if (typeof v === "number") { if (!Number.isFinite(v)) fail(Number.isNaN(v) ? E.ValueError : E.OverflowError, Number.isNaN(v) ? "cannot convert float NaN to integer" : "cannot convert float infinity to integer"); return BigInt(Math.trunc(v)); }
        if (v !== null && typeof v === "object" && v.cls === T.bytes) return parseIntLiteral(String.fromCharCode(...v.items), base);
        const m = typeMethod(v, "__int__") || typeMethod(v, "__index__") || typeMethod(v, "__trunc__");
        if (m !== undefined) return call(descrGet(m, v, typeOf(v)), [], null);
        fail(E.TypeError, "int() argument must be a string, a bytes-like object or a real number, not '" + typeOf(v).name + "'");
    });
    function parseFloatLiteral(s) {
        const t = s.trim().replace(/_/g, "").toLowerCase();
        if (t === "inf" || t === "+inf" || t === "infinity" || t === "+infinity") return Infinity;
        if (t === "-inf" || t === "-infinity") return -Infinity;
        if (t === "nan" || t === "+nan" || t === "-nan") return NaN;
        if (!/^[+-]?(\d+\.?\d*(e[+-]?\d+)?|\.\d+(e[+-]?\d+)?)$/.test(t)) fail(E.ValueError, "could not convert string to float: " + repr(s));
        return Number(t);
    }
    rt.constructors.set(T.float, (args) => {
        if (args.length === 0) return 0.0;
        const v = args[0];
        if (typeof v === "number") return v;
        if (isInt(v)) return Number(asInt(v));
        if (typeof v === "string") return parseFloatLiteral(v);
        const m = typeMethod(v, "__float__");
        if (m !== undefined) return call(descrGet(m, v, typeOf(v)), [], null);
        fail(E.TypeError, "float() argument must be a string or a real number, not '" + typeOf(v).name + "'");
    });
    rt.constructors.set(T.bool, (args) => args.length === 0 ? false : truth(args[0]));
    rt.constructors.set(T.str, (args, kw) => {
        if (args.length === 0) return "";
        if (args[0] !== null && typeof args[0] === "object" && args[0].cls === T.bytes && (args.length > 1 || (kw && kw.size))) return rt.decodeBytes(args[0], args[1] === undefined ? "utf-8" : args[1]);
        return str(args[0]);
    });
    for (const t of [T.int, T.float, T.str, T.bool]) rt.allocators.set(t, null), rt.allocators.delete(t);
    method(T.int, "__index__", 1, (a) => asInt(a[0]));
    method(T.int, "__int__", 1, (a) => asInt(a[0]));
    method(T.int, "__float__", 1, (a) => Number(asInt(a[0])));
    method(T.int, "bit_length", 1, (a) => { const v = asInt(a[0]); return BigInt((v < 0n ? -v : v).toString(2).replace("0", v === 0n ? "" : "0").length); });
    T.int.dict.set("bit_length", builtin("bit_length", 1, (a) => { const v = asInt(a[0]); return v === 0n ? 0n : BigInt((v < 0n ? -v : v).toString(2).length); }));
    method(T.int, "conjugate", 1, (a) => asInt(a[0]));
    method(T.int, "__repr__", 1, (a) => asInt(a[0]).toString());
    method(T.int, "__hash__", 1, (a) => rt.hashInt(a[0]));
    method(T.int, "to_bytes", -1, (a) => {
        const kw = kwOf(a, ["length", "byteorder", "signed"]);
        let v = asInt(a[0]); const length = Number(a[1] !== undefined ? needInt(a[1]) : kwget(kw, "length", 1n));
        const order = a[2] !== undefined ? a[2] : kwget(kw, "byteorder", "big");
        if (v < 0n) v += 1n << BigInt(8 * length);
        const out = []; for (let i = 0; i < length; i++) { out.push(Number(v & 0xFFn)); v >>= 8n; }
        if (order === "big") out.reverse();
        return rt.bytes(out);
    });
    T.int.dict.set("from_bytes", { cls: T.classmethod, func: builtin("from_bytes", -1, (a) => {
        const kw = kwOf(a, ["byteorder", "signed"]);
        const b = a[1].items.slice(); const order = a[2] !== undefined ? a[2] : kwget(kw, "byteorder", "big");
        if (order === "little") b.reverse();
        let v = 0n; for (const x of b) v = (v << 8n) | BigInt(x);
        if (truth(kwget(kw, "signed", false)) && b.length && (b[0] & 0x80)) v -= 1n << BigInt(8 * b.length);
        return v;
    }) });
    method(T.float, "is_integer", 1, (a) => Number.isInteger(a[0]));
    method(T.float, "__int__", 1, (a) => BigInt(Math.trunc(a[0])));
    method(T.float, "__float__", 1, (a) => a[0]);
    method(T.float, "__repr__", 1, (a) => rt.floatRepr(a[0]));
    method(T.float, "__round__", -1, (a) => R.bind ? roundValue(a[0], a[1]) : null);
    method(T.float, "hex", 1, (a) => a[0].toString(16));
    method(T.float, "conjugate", 1, (a) => a[0]);
    method(T.float, "as_integer_ratio", 1, (a) => {
        let x = a[0]; if (!Number.isFinite(x)) fail(E.ValueError, "cannot convert to integer ratio");
        let den = 1n; while (!Number.isInteger(x)) { x *= 2; den *= 2n; }
        return tuple([BigInt(x), den]);
    });
    method(T.bool, "__repr__", 1, (a) => a[0] ? "True" : "False");
    function roundValue(x, n) {
        if (isInt(x)) {
            const v = asInt(x);
            if (n === undefined || n === null) return v;
            const d = needInt(n); if (d >= 0n) return v;
            const p = 10n ** (-d); const q = v / p, r = v % p; const half = p / 2n;
            let out = q * p;
            const ar = r < 0n ? -r : r;
            if (ar > half || (ar === half && ((q % 2n) !== 0n))) out += r < 0n ? -p : p;
            return out;
        }
        if (typeof x !== "number") { const m = typeMethod(x, "__round__"); if (m !== undefined) return call(descrGet(m, x, typeOf(x)), n === undefined ? [] : [n], null); fail(E.TypeError, "type " + typeOf(x).name + " doesn't define __round__ method"); }
        if (n === undefined || n === null) {
            if (!Number.isFinite(x)) fail(Number.isNaN(x) ? E.ValueError : E.OverflowError, "cannot convert float " + rt.floatRepr(x) + " to integer");
            const f = Math.floor(x), diff = x - f;
            let r = diff < 0.5 ? f : diff > 0.5 ? f + 1 : (f % 2 === 0 ? f : f + 1);
            return BigInt(r);
        }
        const d = Number(needInt(n));
        if (!Number.isFinite(x)) return x;
        if (d > 0) return Number(fixedHalfEven(x, Math.min(d, 100)));
        const factor = Math.pow(10, -d);
        const scaled = x / factor;
        let r = Math.floor(scaled);
        const diff = scaled - r;
        if (diff > 0.5 || (diff === 0.5 && r % 2 !== 0)) r += 1;
        return r * factor;
    }
    rt.roundValue = roundValue;
    // `x` to `n` decimals, correctly rounded with ties to even on the EXACT
    // binary value (JS's toFixed rounds exact ties away from zero).
    function fixedHalfEven(x, n) {
        if (!Number.isFinite(x)) return String(x);
        const exact = Math.abs(x).toFixed(Math.min(n + 30, 100));
        const tail = exact.slice(exact.indexOf(".") + 1 + n);
        let s = Math.abs(x).toFixed(n);
        if (/^50*$/.test(tail)) {
            // An exact tie: keep the truncated digits and round to even.
            const kept = exact.slice(0, exact.indexOf(".") + 1 + n).replace(/\.$/, "");
            const digits = kept.replace(".", "");
            const lastDigit = Number(digits[digits.length - 1]);
            if (lastDigit % 2 === 0) s = n > 0 ? kept : kept;
            else {
                // Bump the kept digits by one unit in the last place.
                let arr = digits.split("").map(Number); let i = arr.length - 1;
                while (i >= 0) { if (arr[i] === 9) { arr[i] = 0; i--; } else { arr[i]++; break; } }
                let str2 = (i < 0 ? "1" : "") + arr.join("");
                if (n > 0) str2 = str2.slice(0, str2.length - n) + "." + str2.slice(str2.length - n);
                s = str2;
            }
        }
        return (x < 0 || Object.is(x, -0)) && Number(s) !== 0 ? "-" + s : (x < 0 && Number(s) === 0 ? "-" + s : s);
    }
    rt.fixedHalfEven = fixedHalfEven;

    // ---- str ------------------------------------------------------------------------------------------------------
    const S = T.str;
    function strSelf(a) { return needStr(a[0], "descriptor requires a 'str' object"); }
    method(S, "__len__", 1, (a) => rt.baseLen(a[0]));
    method(S, "__hash__", 1, (a) => rt.hashInt(a[0]));
    method(S, "__repr__", 1, (a) => rt.quoteStr(a[0]));
    method(S, "__str__", 1, (a) => a[0]);
    method(S, "__getitem__", 2, (a) => rt.baseGetitem(a[0], a[1]));
    method(S, "__contains__", 2, (a) => rt.baseContains(a[0], a[1]));
    method(S, "__add__", 2, (a) => typeof a[1] === "string" ? a[0] + a[1] : NOTIMPL);
    method(S, "__mul__", 2, (a) => R.binop("mul", a[0], a[1]));
    method(S, "__mod__", 2, (a) => rt.percentFormat(a[0], a[1]));
    method(S, "__eq__", 2, (a) => typeof a[1] === "string" ? a[0] === a[1] : NOTIMPL);
    method(S, "__lt__", 2, (a) => typeof a[1] === "string" ? rt.compareStrings(a[0], a[1]) < 0 : NOTIMPL);
    method(S, "__iter__", 1, (a) => rt.baseIter(a[0]));
    method(S, "upper", 1, (a) => strSelf(a).toUpperCase());
    method(S, "lower", 1, (a) => strSelf(a).toLowerCase());
    method(S, "casefold", 1, (a) => strSelf(a).toLowerCase().replace(/ß/g, "ss").replace(/ſ/g, "s").replace(/ﬁ/g, "fi").replace(/ﬂ/g, "fl"));
    method(S, "swapcase", 1, (a) => Array.from(strSelf(a), (c) => c === c.toUpperCase() ? c.toLowerCase() : c.toUpperCase()).join(""));
    method(S, "capitalize", 1, (a) => { const s = strSelf(a); return s.length ? Array.from(s)[0].toUpperCase() + Array.from(s).slice(1).join("").toLowerCase() : s; });
    method(S, "title", 1, (a) => strSelf(a).replace(/[A-Za-zÀ-ɏ]+/g, (w) => w[0].toUpperCase() + w.slice(1).toLowerCase()));
    function stripChars(s, chars, mode) {
        if (chars === undefined || chars === null) { if (mode === 0) return s.trim(); return mode < 0 ? s.replace(/^\s+/, "") : s.replace(/\s+$/, ""); }
        const set = new Set(Array.from(needStr(chars)));
        let cps = Array.from(s), i = 0, j = cps.length;
        if (mode <= 0) while (i < j && set.has(cps[i])) i++;
        if (mode >= 0) while (j > i && set.has(cps[j - 1])) j--;
        return cps.slice(i, j).join("");
    }
    method(S, "strip", 2, (a) => stripChars(strSelf(a), a[1], 0), 1);
    method(S, "lstrip", 2, (a) => stripChars(strSelf(a), a[1], -1), 1);
    method(S, "rstrip", 2, (a) => stripChars(strSelf(a), a[1], 1), 1);
    method(S, "split", -1, (a) => {
        const kw = kwOf(a, ["sep", "maxsplit"]);
        const s = strSelf(a); const sep = a[1] !== undefined ? a[1] : kwget(kw, "sep", null);
        let maxsplit = Number(a[2] !== undefined ? needInt(a[2]) : kwget(kw, "maxsplit", -1n));
        if (sep === null) {
            const parts = s.split(/\s+/).filter((x) => x.length);
            if (maxsplit < 0 || parts.length <= maxsplit + 1) return list(parts);
            // Re-split preserving the remainder verbatim.
            const out = []; let rest = s.replace(/^\s+/, "");
            for (let i = 0; i < maxsplit; i++) { const m = /^(\S+)\s+/.exec(rest); if (!m) break; out.push(m[1]); rest = rest.slice(m[0].length); }
            if (rest.length) out.push(rest.replace(/\s+$/, ""));
            return list(out);
        }
        if (needStr(sep) === "") fail(E.ValueError, "empty separator");
        const parts = s.split(sep);
        if (maxsplit >= 0 && parts.length > maxsplit + 1) { const head = parts.slice(0, maxsplit); head.push(parts.slice(maxsplit).join(sep)); return list(head); }
        return list(parts);
    });
    method(S, "rsplit", -1, (a) => {
        const kw = kwOf(a, ["sep", "maxsplit"]);
        const s = strSelf(a); const sep = a[1] !== undefined ? a[1] : kwget(kw, "sep", null);
        const maxsplit = Number(a[2] !== undefined ? needInt(a[2]) : kwget(kw, "maxsplit", -1n));
        if (sep === null) {
            const parts = s.split(/\s+/).filter((x) => x.length);
            if (maxsplit < 0 || parts.length <= maxsplit + 1) return list(parts);
            const out = []; let rest = s.replace(/\s+$/, "");
            for (let i = 0; i < maxsplit; i++) { const m = /\s+(\S+)$/.exec(rest); if (!m) break; out.unshift(m[1]); rest = rest.slice(0, rest.length - m[0].length); }
            if (rest.length) out.unshift(rest.replace(/^\s+/, ""));
            return list(out);
        }
        const parts = s.split(needStr(sep));
        if (maxsplit >= 0 && parts.length > maxsplit + 1) { const tail = parts.slice(parts.length - maxsplit); tail.unshift(parts.slice(0, parts.length - maxsplit).join(sep)); return list(tail); }
        return list(parts);
    });
    method(S, "splitlines", 2, (a) => { const keep = a[1] !== undefined && truth(a[1]); const s = strSelf(a); const out = []; const re = /([^\r\n]*)(\r\n|\r|\n|$)/g; let m; while ((m = re.exec(s)) !== null && (m[1].length || m[2].length)) { out.push(keep ? m[1] + m[2] : m[1]); if (m[2] === "") break; } return list(out); }, 1);
    method(S, "join", 2, (a) => { const parts = drain(a[1]); for (const p of parts) if (typeof p !== "string") fail(E.TypeError, "sequence item: expected str instance, " + typeOf(p).name + " found"); return rt.checkedText(parts.join(strSelf(a))); });
    method(S, "replace", 4, (a) => { const s = strSelf(a), from = needStr(a[1]), to = needStr(a[2]); const count = a[3] === undefined ? -1 : Number(needInt(a[3])); if (count < 0) return s.split(from).join(to); let out = "", rest = s, n = 0; while (n < count) { const i = from === "" ? (rest.length ? 0 : -1) : rest.indexOf(from); if (i < 0) break; if (from === "") { out += to + rest[0]; rest = rest.slice(1); } else { out += rest.slice(0, i) + to; rest = rest.slice(i + from.length); } n++; } return out + rest; }, 3);
    function findImpl(a, rev, raise) {
        const s = strSelf(a), sub = needStr(a[1]);
        const cps = codepoints(s);
        let start = a[2] === undefined || a[2] === null ? 0 : Number(needInt(a[2])), end = a[3] === undefined || a[3] === null ? cps.length : Number(needInt(a[3]));
        if (start < 0) start = Math.max(0, start + cps.length); if (end < 0) end = Math.max(0, end + cps.length); end = Math.min(end, cps.length);
        const hay = cps.slice(start, end).join("");
        const i = rev ? hay.lastIndexOf(sub) : hay.indexOf(sub);
        if (i < 0) { if (raise) fail(E.ValueError, "substring not found"); return -1n; }
        return BigInt(start + codepoints(hay.slice(0, i)).length);
    }
    method(S, "find", 4, (a) => findImpl(a, false, false), 2);
    method(S, "rfind", 4, (a) => findImpl(a, true, false), 2);
    method(S, "index", 4, (a) => findImpl(a, false, true), 2);
    method(S, "rindex", 4, (a) => findImpl(a, true, true), 2);
    method(S, "count", 4, (a) => { const s = strSelf(a), sub = needStr(a[1]); if (sub === "") return BigInt(codepoints(s).length + 1); return BigInt(s.split(sub).length - 1); }, 2);
    function affix(a, end) {
        const s = strSelf(a); const p = a[1];
        const test = (x) => end ? s.endsWith(needStr(x)) : s.startsWith(needStr(x));
        if (p !== null && typeof p === "object" && p.cls === T.tuple) return p.items.some(test);
        return test(p);
    }
    method(S, "startswith", 4, (a) => affix(a, false), 2);
    method(S, "endswith", 4, (a) => affix(a, true), 2);
    method(S, "isdigit", 1, (a) => /^[0-9٠-٩۰-۹]+$/.test(strSelf(a)));
    method(S, "isdecimal", 1, (a) => /^[0-9]+$/.test(strSelf(a)));
    method(S, "isnumeric", 1, (a) => /^[0-9]+$/.test(strSelf(a)));
    method(S, "isalpha", 1, (a) => /^[\p{L}]+$/u.test(strSelf(a)));
    method(S, "isalnum", 1, (a) => /^[\p{L}\p{N}]+$/u.test(strSelf(a)));
    method(S, "isspace", 1, (a) => /^\s+$/.test(strSelf(a)));
    method(S, "isupper", 1, (a) => { const s = strSelf(a); return /[A-Za-z]/.test(s) && s === s.toUpperCase(); });
    method(S, "islower", 1, (a) => { const s = strSelf(a); return /[A-Za-z]/.test(s) && s === s.toLowerCase(); });
    method(S, "istitle", 1, (a) => { const s = strSelf(a); return /[A-Za-z]/.test(s) && s === s.replace(/[A-Za-z]+/g, (w) => w[0].toUpperCase() + w.slice(1).toLowerCase()); });
    method(S, "isidentifier", 1, (a) => /^[A-Za-z_][A-Za-z0-9_]*$/.test(strSelf(a)));
    method(S, "isascii", 1, (a) => /^[\x00-\x7f]*$/.test(strSelf(a)));
    method(S, "isprintable", 1, (a) => !/[\x00-\x1f\x7f]/.test(strSelf(a)));
    function pad(a, mode) {
        const s = strSelf(a); const width = Number(needInt(a[1])); const fill = a[2] === undefined ? " " : needStr(a[2]);
        if (codepoints(fill).length !== 1) fail(E.TypeError, "The fill character must be exactly one character long");
        const n = codepoints(s).length; if (width <= n) return s;
        const total = width - n;
        if (mode < 0) return s + fill.repeat(total);
        if (mode > 0) return fill.repeat(total) + s;
        const left = Math.floor(total / 2) + ((total & 1) && (width & 1) ? 1 : 0);
        return fill.repeat(left) + s + fill.repeat(total - left);
    }
    method(S, "ljust", 3, (a) => pad(a, -1), 2);
    method(S, "rjust", 3, (a) => pad(a, 1), 2);
    method(S, "center", 3, (a) => pad(a, 0), 2);
    method(S, "zfill", 2, (a) => { const s = strSelf(a); const width = Number(needInt(a[1])); if (s.length >= width) return s; const sign = s[0] === "-" || s[0] === "+" ? s[0] : ""; return sign + "0".repeat(width - s.length) + s.slice(sign.length); });
    method(S, "partition", 2, (a) => { const s = strSelf(a), sep = needStr(a[1]); const i = s.indexOf(sep); return i < 0 ? tuple([s, "", ""]) : tuple([s.slice(0, i), sep, s.slice(i + sep.length)]); });
    method(S, "rpartition", 2, (a) => { const s = strSelf(a), sep = needStr(a[1]); const i = s.lastIndexOf(sep); return i < 0 ? tuple(["", "", s]) : tuple([s.slice(0, i), sep, s.slice(i + sep.length)]); });
    method(S, "encode", -1, (a) => { kwOf(a, ["encoding", "errors"]); return rt.encodeStr(strSelf(a), a[1] === undefined ? "utf-8" : a[1]); });
    method(S, "format", -1, (a) => { const kw = kwOf(a, null) === null ? null : null; return rt.strFormat(strSelf(a), a.slice(1), a.kwmap || null); });
    S.dict.get("format").kwnames = true;
    S.dict.set("format", builtin("format", -1, (a) => { const last = a[a.length - 1]; let kw = null; if (last instanceof Map) { kw = last; a.pop(); } return rt.strFormat(strSelf(a), a.slice(1), kw); }));
    S.dict.get("format").kwnames = true;
    method(S, "format_map", 2, (a) => rt.strFormat(strSelf(a), [], rt.mapFromDict(rt.asDict(a[1]))));
    method(S, "expandtabs", 2, (a) => { const size = a[1] === undefined ? 8 : Number(needInt(a[1])); let out = "", col = 0; for (const ch of strSelf(a)) { if (ch === "\t") { const n = size > 0 ? size - (col % size) : 0; out += " ".repeat(n); col += n; } else { out += ch; col = ch === "\n" || ch === "\r" ? 0 : col + 1; } } return out; }, 1);
    method(S, "removeprefix", 2, (a) => { const s = strSelf(a), p = needStr(a[1]); return s.startsWith(p) ? s.slice(p.length) : s; });
    method(S, "removesuffix", 2, (a) => { const s = strSelf(a), p = needStr(a[1]); return p.length && s.endsWith(p) ? s.slice(0, s.length - p.length) : s; });
    S.dict.set("maketrans", { cls: T.staticmethod, func: builtin("maketrans", 3, (a) => {
        const d = dict();
        if (a.length === 1) { for (const [k, v] of dictEntries(rt.asDict(a[0]))) dictSet(d, typeof k === "string" ? BigInt(k.codePointAt(0)) : k, v); return d; }
        const x = codepoints(needStr(a[0])), y = codepoints(needStr(a[1]));
        if (x.length !== y.length) fail(E.ValueError, "the first two maketrans arguments must have equal length");
        for (let i = 0; i < x.length; i++) dictSet(d, BigInt(x[i].codePointAt(0)), BigInt(y[i].codePointAt(0)));
        if (a[2] !== undefined) for (const ch of codepoints(needStr(a[2]))) dictSet(d, BigInt(ch.codePointAt(0)), null);
        return d;
    }, 1) });
    method(S, "translate", 2, (a) => { const table = a[1]; let out = ""; for (const ch of codepoints(strSelf(a))) { const cp = BigInt(ch.codePointAt(0)); let v; try { v = getitem(table, cp); } catch (e) { if (e && (e.cls === E.LookupError || isInstance(e, E.LookupError))) { out += ch; continue; } throw e; } if (v === null) continue; out += isInt(v) ? String.fromCodePoint(Number(asInt(v))) : str(v); } return out; });
    rt.encodeStr = function (s, encoding) {
        const enc = String(encoding).toLowerCase().replace("-", "");
        const out = [];
        if (enc === "ascii" || enc === "latin1" || enc === "latin_1" || enc === "iso88591") { for (const ch of s) { const cp = ch.codePointAt(0); if (cp > (enc === "ascii" ? 127 : 255)) fail(E.UnicodeEncodeError, "'" + encoding + "' codec can't encode character " + rt.quoteStr(ch)); out.push(cp); } return rt.bytes(out); }
        for (const ch of s) { let cp = ch.codePointAt(0); if (cp < 0x80) out.push(cp); else if (cp < 0x800) out.push(0xC0 | (cp >> 6), 0x80 | (cp & 63)); else if (cp < 0x10000) out.push(0xE0 | (cp >> 12), 0x80 | ((cp >> 6) & 63), 0x80 | (cp & 63)); else out.push(0xF0 | (cp >> 18), 0x80 | ((cp >> 12) & 63), 0x80 | ((cp >> 6) & 63), 0x80 | (cp & 63)); }
        return rt.bytes(out);
    };
    rt.decodeBytes = function (b, encoding) {
        const enc = String(encoding).toLowerCase().replace("-", "");
        const items = b.items;
        if (enc === "ascii" || enc === "latin1" || enc === "latin_1" || enc === "iso88591") return String.fromCharCode(...items);
        let out = "", i = 0;
        while (i < items.length) {
            const c = items[i];
            if (c < 0x80) { out += String.fromCharCode(c); i++; }
            else if (c >= 0xC0 && c < 0xE0 && i + 1 < items.length) { out += String.fromCodePoint(((c & 31) << 6) | (items[i + 1] & 63)); i += 2; }
            else if (c >= 0xE0 && c < 0xF0 && i + 2 < items.length) { out += String.fromCodePoint(((c & 15) << 12) | ((items[i + 1] & 63) << 6) | (items[i + 2] & 63)); i += 3; }
            else if (c >= 0xF0 && i + 3 < items.length) { out += String.fromCodePoint(((c & 7) << 18) | ((items[i + 1] & 63) << 12) | ((items[i + 2] & 63) << 6) | (items[i + 3] & 63)); i += 4; }
            else fail(E.UnicodeDecodeError, "'utf-8' codec can't decode byte 0x" + c.toString(16) + " in position " + i + ": invalid start byte");
        }
        return out;
    };
    rt.bytesRepr = function (b) {
        let out = "b'";
        for (const c of b.items) {
            if (c === 39) out += "\\'"; else if (c === 92) out += "\\\\"; else if (c === 10) out += "\\n"; else if (c === 13) out += "\\r"; else if (c === 9) out += "\\t";
            else if (c < 32 || c >= 127) out += "\\x" + c.toString(16).padStart(2, "0"); else out += String.fromCharCode(c);
        }
        return out + "'";
    };
    rt.constructors.set(T.bytes, (args) => {
        if (args.length === 0) return rt.bytes([]);
        const v = args[0];
        if (typeof v === "string") return rt.encodeStr(v, args[1] === undefined ? "utf-8" : args[1]);
        if (isInt(v)) return rt.bytes(new Array(Number(asInt(v))).fill(0));
        if (v !== null && typeof v === "object" && v.cls === T.bytes) return rt.bytes(v.items.slice());
        return rt.bytes(drain(v).map((x) => { const n = Number(needInt(x)); if (n < 0 || n > 255) fail(E.ValueError, "bytes must be in range(0, 256)"); return n; }));
    });
    method(T.bytes, "decode", -1, (a) => { kwOf(a, ["encoding", "errors"]); return rt.decodeBytes(a[0], a[1] === undefined ? "utf-8" : a[1]); });
    method(T.bytes, "__len__", 1, (a) => BigInt(a[0].items.length));
    method(T.bytes, "hex", 1, (a) => a[0].items.map((c) => c.toString(16).padStart(2, "0")).join(""));
    method(T.bytes, "__repr__", 1, (a) => rt.bytesRepr(a[0]));

    // ---- list ---------------------------------------------------------------------------------------------------
    const L = T.list;
    function listSelf(a) { const v = a[0]; if (v !== null && typeof v === "object" && v.items !== undefined && isInstance(v, T.list)) return v; fail(E.TypeError, "descriptor requires a 'list' object but received '" + typeOf(v).name + "'"); }
    rt.constructors.set(T.list, (args, kw, cls) => { const out = cls === T.list ? list([]) : rt.allocInstance(cls); if (args.length) out.items = drain(args[0]); return out; });
    rt.allocators.set(T.list, (cls) => ({ cls: cls, items: [] }));
    rt.allocators.set(T.tuple, (cls) => ({ cls: cls, items: [] }));
    rt.allocators.set(T.dict, (cls) => ({ cls: cls, map: new Map(), size: 0 }));
    rt.allocators.set(T.set, (cls) => ({ cls: cls, map: new Map(), size: 0 }));
    method(L, "__init__", -1, (a) => { const self = a[0]; self.items = a.length > 1 ? drain(a[1]) : []; return null; });
    method(L, "append", 2, (a) => { const l = listSelf(a); if (l.items.length >= rt.MAX_ITEMS) fail(E.MemoryError, "list limit exceeded"); l.items.push(a[1]); return null; });
    method(L, "extend", 2, (a) => { const l = listSelf(a); const items = drain(a[1]); for (const x of items) l.items.push(x); return null; });
    method(L, "insert", 3, (a) => { const l = listSelf(a); let i = Number(needInt(a[1])); if (i < 0) i = Math.max(0, i + l.items.length); l.items.splice(Math.min(i, l.items.length), 0, a[2]); return null; });
    method(L, "pop", 2, (a) => { const l = listSelf(a); if (l.items.length === 0) fail(E.IndexError, "pop from empty list"); if (a[1] === undefined) return l.items.pop(); let i = Number(needInt(a[1])); if (i < 0) i += l.items.length; if (i < 0 || i >= l.items.length) fail(E.IndexError, "pop index out of range"); return l.items.splice(i, 1)[0]; }, 1);
    method(L, "remove", 2, (a) => { const l = listSelf(a); const i = l.items.findIndex((x) => eq(x, a[1])); if (i < 0) fail(E.ValueError, "list.remove(x): x not in list"); l.items.splice(i, 1); return null; });
    method(L, "index", 4, (a) => { const l = listSelf(a); const start = a[2] === undefined ? 0 : Number(needInt(a[2])); const end = a[3] === undefined ? l.items.length : Number(needInt(a[3])); for (let i = Math.max(0, start < 0 ? start + l.items.length : start); i < Math.min(end < 0 ? end + l.items.length : end, l.items.length); i++) if (eq(l.items[i], a[1])) return BigInt(i); fail(E.ValueError, repr(a[1]) + " is not in list"); }, 2);
    method(L, "count", 2, (a) => BigInt(listSelf(a).items.filter((x) => eq(x, a[1])).length));
    method(L, "clear", 1, (a) => { listSelf(a).items.length = 0; return null; });
    method(L, "copy", 1, (a) => list(listSelf(a).items.slice()));
    method(L, "reverse", 1, (a) => { listSelf(a).items.reverse(); return null; });
    method(L, "__len__", 1, (a) => rt.baseLen(a[0]));
    method(L, "__getitem__", 2, (a) => rt.baseGetitem(a[0], a[1]));
    method(L, "__setitem__", 3, (a) => rt.baseSetitem(a[0], a[1], a[2]));
    method(L, "__delitem__", 2, (a) => rt.baseDelitem(a[0], a[1]));
    method(L, "__contains__", 2, (a) => rt.baseContains(a[0], a[1]));
    method(L, "__iter__", 1, (a) => rt.baseIter(a[0]));
    method(L, "__reversed__", 1, (a) => { const items = a[0].items; let i = items.length; return { cls: T.iterator, next: () => i > 0 ? items[--i] : STOP }; });
    method(L, "__add__", 2, (a) => rt.baseBinop("add", a[0], a[1], false));
    method(L, "__iadd__", 2, (a) => rt.baseBinop("add", a[0], a[1], true));
    method(L, "__mul__", 2, (a) => rt.baseBinop("mul", a[0], a[1], false));
    // Item-wise equality for the sequence types and their subclasses (never
    // back through `eq` on the containers, which would recurse).
    function seqEq(a, b) {
        if (a.items.length !== b.items.length) return false;
        for (let i = 0; i < a.items.length; i++) if (!eq(a.items[i], b.items[i])) return false;
        return true;
    }
    method(L, "__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.list) ? seqEq(a[0], a[1]) : NOTIMPL);
    method(L, "__repr__", 1, (a) => rt.baseRepr(a[0]));
    L.dict.set("__hash__", null);
    function sortItems(items, key, reverse) {
        const keyed = key === null ? items.map((x) => [x, x]) : items.map((x) => [call(key, [x], null), x]);
        // A stable merge sort using Python ordering (`<` only, as CPython).
        const lt = (a, b) => cmp("lt", a[0], b[0]);
        const sorted = mergeSort(keyed, reverse ? (a, b) => lt(b, a) : lt);
        return sorted.map((p) => p[1]);
    }
    function mergeSort(arr, lt) {
        if (arr.length <= 1) return arr;
        const mid = arr.length >> 1;
        const left = mergeSort(arr.slice(0, mid), lt), right = mergeSort(arr.slice(mid), lt);
        const out = []; let i = 0, j = 0;
        while (i < left.length && j < right.length) { if (lt(right[j], left[i])) out.push(right[j++]); else out.push(left[i++]); }
        while (i < left.length) out.push(left[i++]); while (j < right.length) out.push(right[j++]);
        return out;
    }
    rt.sortItems = sortItems;
    methodkw(L, "sort", (a) => { const kw = kwOf(a, ["key", "reverse"]); const l = listSelf(a); if (a.length > 1) fail(E.TypeError, "sort() takes no positional arguments"); l.items = sortItems(l.items, kwget(kw, "key", null), truth(kwget(kw, "reverse", false))); return null; });

    // ---- tuple ------------------------------------------------------------------------------------------------------
    rt.constructors.set(T.tuple, (args, kw, cls) => { const out = cls === T.tuple ? tuple([]) : rt.allocInstance(cls); if (args.length) out.items = drain(args[0]); return out; });
    method(T.tuple, "__len__", 1, (a) => rt.baseLen(a[0]));
    method(T.tuple, "__getitem__", 2, (a) => rt.baseGetitem(a[0], a[1]));
    method(T.tuple, "__contains__", 2, (a) => rt.baseContains(a[0], a[1]));
    method(T.tuple, "__iter__", 1, (a) => rt.baseIter(a[0]));
    method(T.tuple, "__hash__", 1, (a) => rt.baseHash(a[0]));
    method(T.tuple, "__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.tuple) ? seqEq(a[0], a[1]) : NOTIMPL);
    method(T.tuple, "__lt__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.tuple) ? cmp("lt", tuple(a[0].items), tuple(a[1].items)) : NOTIMPL);
    method(T.tuple, "__gt__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.tuple) ? cmp("gt", tuple(a[0].items), tuple(a[1].items)) : NOTIMPL);
    method(T.tuple, "__add__", 2, (a) => rt.baseBinop("add", a[0], a[1], false));
    method(T.tuple, "__mul__", 2, (a) => rt.baseBinop("mul", a[0], a[1], false));
    method(T.tuple, "__repr__", 1, (a) => rt.baseRepr(a[0]));
    method(T.tuple, "index", 2, (a) => { const i = a[0].items.findIndex((x) => eq(x, a[1])); if (i < 0) fail(E.ValueError, "tuple.index(x): x not in tuple"); return BigInt(i); });
    method(T.tuple, "count", 2, (a) => BigInt(a[0].items.filter((x) => eq(x, a[1])).length));

    // ---- dict ---------------------------------------------------------------------------------------------------------
    const D = T.dict;
    function dictSelf(a) { const v = a[0]; if (v !== null && typeof v === "object" && v.map !== undefined && isInstance(v, T.dict)) return v; fail(E.TypeError, "descriptor requires a 'dict' object but received '" + typeOf(v).name + "'"); }
    function fillDict(d, src, kw) {
        if (src !== undefined && src !== null) {
            if (src !== null && typeof src === "object" && src.cls === T.dict) { for (const [k, v] of dictEntries(src)) dictSet(d, k, v); }
            else if (src !== null && typeof src === "object" && typeMethod(src, "keys") !== undefined) { for (const [k, v] of dictEntries(rt.asDict(src))) dictSet(d, k, v); }
            else { for (const pair of drain(src)) { const kv = drain(pair); if (kv.length !== 2) fail(E.ValueError, "dictionary update sequence element has length " + kv.length + "; 2 is required"); dictSet(d, kv[0], kv[1]); } }
        }
        if (kw) for (const [k, v] of kw) dictSet(d, k, v);
    }
    rt.constructors.set(T.dict, (args, kw, cls) => { const d = cls === T.dict ? dict() : rt.allocInstance(cls); fillDict(d, args[0], kw); return d; });
    methodkw(D, "__init__", (a) => { const kw = kwOf(a, null); fillDict(a[0], a[1], kw); return null; });
    method(D, "__len__", 1, (a) => rt.baseLen(a[0]));
    method(D, "__getitem__", 2, (a) => rt.baseGetitem(a[0], a[1]));
    method(D, "__setitem__", 3, (a) => rt.baseSetitem(a[0], a[1], a[2]));
    method(D, "__delitem__", 2, (a) => rt.baseDelitem(a[0], a[1]));
    method(D, "__contains__", 2, (a) => rt.dictHas(a[0], a[1]));
    method(D, "__iter__", 1, (a) => rt.baseIter(a[0]));
    method(D, "__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.dict) ? rt.dictEq(a[0], a[1]) : NOTIMPL);
    method(D, "__repr__", 1, (a) => rt.baseRepr(a[0]));
    method(D, "__or__", 2, (a) => rt.baseBinop("or", a[0], a[1], false));
    D.dict.set("__hash__", null);
    method(D, "get", 3, (a) => { const v = dictGet(dictSelf(a), a[1]); return v === undefined ? (a[2] === undefined ? null : a[2]) : v; }, 2);
    method(D, "setdefault", 3, (a) => { const d = dictSelf(a); const v = dictGet(d, a[1]); if (v !== undefined) return v; const dflt = a[2] === undefined ? null : a[2]; dictSet(d, a[1], dflt); return dflt; }, 2);
    method(D, "pop", 3, (a) => { const d = dictSelf(a); const v = dictGet(d, a[1]); if (v === undefined) { if (a[2] === undefined) throw rt.makeExc(E.KeyError, [a[1]]); return a[2]; } dictDel(d, a[1]); return v; }, 2);
    method(D, "popitem", 1, (a) => { const d = dictSelf(a); const entries = dictEntryList(d); if (!entries.length) fail(E.KeyError, "popitem(): dictionary is empty"); const [k, v] = entries[entries.length - 1]; dictDel(d, k); return tuple([k, v]); });
    methodkw(D, "update", (a) => { const kw = kwOf(a, null); fillDict(dictSelf(a), a[1], kw); return null; });
    method(D, "clear", 1, (a) => { const d = dictSelf(a); d.map.clear(); d.size = 0; return null; });
    method(D, "copy", 1, (a) => rt.dictCopy(dictSelf(a)));
    function view(type, d, pick) {
        return { cls: type, dict: d, iter: () => { const entries = dictEntryList(d); let i = 0; return { cls: T.iterator, next: () => i < entries.length ? pick(entries[i++]) : STOP }; } };
    }
    method(D, "keys", 1, (a) => view(T.dict_keys, dictSelf(a), (e) => e[0]));
    method(D, "values", 1, (a) => view(T.dict_values, dictSelf(a), (e) => e[1]));
    method(D, "items", 1, (a) => view(T.dict_items, dictSelf(a), (e) => tuple([e[0], e[1]])));
    for (const v of [T.dict_keys, T.dict_values, T.dict_items]) {
        method(v, "__len__", 1, (a) => BigInt(a[0].dict.size));
        method(v, "__iter__", 1, (a) => a[0].iter());
        method(v, "__repr__", 1, (a) => repr(a[0]));
    }
    method(T.dict_keys, "__contains__", 2, (a) => rt.dictHas(a[0].dict, a[1]));
    method(T.dict_keys, "__sub__", 2, (a) => setFrom(a[0], T.set) && rt.setBinop("sub", setFrom(a[0], T.set), setFrom(a[1], T.set), false));
    method(T.dict_keys, "__and__", 2, (a) => rt.setBinop("and", setFrom(a[0], T.set), setFrom(a[1], T.set), false));
    method(T.dict_keys, "__or__", 2, (a) => rt.setBinop("or", setFrom(a[0], T.set), setFrom(a[1], T.set), false));
    D.dict.set("fromkeys", { cls: T.classmethod, func: builtin("fromkeys", 3, (a) => { const d = rt.construct(a[0], [], null); const v = a[2] === undefined ? null : a[2]; for (const k of drain(a[1])) dictSet(d, k, v); return d; }, 2) });

    // ---- set / frozenset -------------------------------------------------------------------------------------------------
    for (const [type, frozen] of [[T.set, false], [T.frozenset, true]]) {
        rt.constructors.set(type, (args, kw, cls) => { const s = cls === type ? set(type) : rt.allocInstance(cls); if (args.length) for (const x of drain(args[0])) setAdd(s, x); return s; });
        method(type, "__len__", 1, (a) => rt.baseLen(a[0]));
        method(type, "__contains__", 2, (a) => setHas(a[0], a[1]));
        method(type, "__iter__", 1, (a) => rt.baseIter(a[0]));
        method(type, "__repr__", 1, (a) => rt.baseRepr(a[0]));
        method(type, "__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && (a[1].cls === T.set || a[1].cls === T.frozenset) ? rt.setEq(a[0], a[1]) : NOTIMPL);
        for (const [name, op] of [["union", "or"], ["intersection", "and"], ["difference", "sub"], ["symmetric_difference", "xor"]]) {
            method(type, name, -1, (a) => { let out = setFrom(a[0], type); for (let i = 1; i < a.length; i++) out = rt.setBinop(op, out, setFrom(a[i], type), false); return out; });
            method(type, "__" + op + "__", 2, (a) => rt.baseBinop(op, a[0], a[1], false));
        }
        method(type, "issubset", 2, (a) => rt.setCompare("le", a[0], setFrom(a[1], type)));
        method(type, "issuperset", 2, (a) => rt.setCompare("ge", a[0], setFrom(a[1], type)));
        method(type, "isdisjoint", 2, (a) => { for (const x of drain(a[1])) if (setHas(a[0], x)) return false; return true; });
        method(type, "copy", 1, (a) => setFrom(a[0], type));
        if (frozen) { method(type, "__hash__", 1, (a) => rt.baseHash(a[0])); continue; }
        type.dict.set("__hash__", null);
        method(type, "__init__", -1, (a) => { const s = a[0]; s.map.clear(); s.size = 0; if (a.length > 1) for (const x of drain(a[1])) setAdd(s, x); return null; });
        method(type, "add", 2, (a) => { setAdd(a[0], a[1]); return null; });
        method(type, "remove", 2, (a) => { if (!setDel(a[0], a[1])) throw rt.makeExc(E.KeyError, [a[1]]); return null; });
        method(type, "discard", 2, (a) => { setDel(a[0], a[1]); return null; });
        method(type, "pop", 1, (a) => { const items = setList(a[0]); if (!items.length) fail(E.KeyError, "pop from an empty set"); setDel(a[0], items[0]); return items[0]; });
        method(type, "clear", 1, (a) => { a[0].map.clear(); a[0].size = 0; return null; });
        method(type, "update", -1, (a) => { for (let i = 1; i < a.length; i++) for (const x of drain(a[i])) setAdd(a[0], x); return null; });
        method(type, "intersection_update", -1, (a) => { for (let i = 1; i < a.length; i++) rt.setBinop("and", a[0], setFrom(a[i], type), true); return null; });
        method(type, "difference_update", -1, (a) => { for (let i = 1; i < a.length; i++) rt.setBinop("sub", a[0], setFrom(a[i], type), true); return null; });
        method(type, "symmetric_difference_update", 2, (a) => { rt.setBinop("xor", a[0], setFrom(a[1], type), true); return null; });
    }

    // ---- range / slice / iterator types --------------------------------------------------------------------------------------------
    rt.constructors.set(T.range, (args) => {
        if (args.length < 1 || args.length > 3) fail(E.TypeError, "range expected at most 3 arguments, got " + args.length);
        const start = args.length === 1 ? 0n : rt.indexOf(args[0], "range");
        const stop = rt.indexOf(args[args.length === 1 ? 0 : 1], "range");
        const step = args.length === 3 ? rt.indexOf(args[2], "range") : 1n;
        if (step === 0n) fail(E.ValueError, "range() arg 3 must not be zero");
        return rt.range(start, stop, step);
    });
    method(T.range, "__len__", 1, (a) => rt.rangeLength(a[0]));
    method(T.range, "__iter__", 1, (a) => rt.baseIter(a[0]));
    method(T.range, "__contains__", 2, (a) => rt.baseContains(a[0], a[1]));
    method(T.range, "__getitem__", 2, (a) => rt.baseGetitem(a[0], a[1]));
    method(T.range, "__repr__", 1, (a) => repr(a[0]));
    method(T.range, "__reversed__", 1, (a) => { const r = a[0]; const n = rt.rangeLength(r); let i = n; return { cls: T.iterator, next: () => i > 0n ? r.start + (--i) * r.step : STOP }; });
    method(T.range, "index", 2, (a) => { const r = a[0]; if (!rt.contains(r, a[1])) fail(E.ValueError, repr(a[1]) + " is not in range"); return (asInt(a[1]) - r.start) / r.step; });
    method(T.range, "count", 2, (a) => rt.contains(a[0], a[1]) ? 1n : 0n);
    rt.constructors.set(T.slice, (args) => { if (args.length === 1) return R.slice(null, args[0], null); return R.slice(args[0], args[1], args[2] === undefined ? null : args[2]); });
    method(T.slice, "indices", 2, (a) => { const [s, e, st] = rt.sliceIndices(a[0], Number(needInt(a[1]))); return tuple([BigInt(s), BigInt(e), BigInt(st)]); });
    for (const t of [T.iterator, T.list_iterator, T.generator, T.enumerate, T.zip, T.map, T.filter, T.reversed]) {
        method(t, "__iter__", 1, (a) => a[0]);
        method(t, "__next__", 1, (a) => { const v = a[0].next(); if (v === STOP) throw rt.makeExc(E.StopIteration, []); return v; });
    }
    method(T.generator, "send", 2, (a) => a[0].send(a[1]));
    method(T.generator, "close", 1, (a) => a[0].close());
    method(T.generator, "throw", -1, (a) => { let e = a[1]; if (isType(e)) e = rt.construct(e, a[2] === undefined ? [] : [a[2]], null); return a[0].throwIn(e); });
    rt.constructors.set(T.enumerate, (args, kw) => {
        let start = kw && kw.has("start") ? needInt(kw.get("start")) : args[1] === undefined ? 0n : needInt(args[1]);
        const it = iter(args[0]); let i = start;
        return { cls: T.enumerate, next: () => { const v = fornext(it); if (v === STOP) return STOP; return tuple([i++, v]); } };
    });
    rt.constructors.set(T.zip, (args, kw) => {
        const strict = kw && kw.has("strict") ? truth(kw.get("strict")) : false;
        const its = args.map(iter);
        return { cls: T.zip, next: () => {
            if (its.length === 0) return STOP;
            const out = [];
            for (let i = 0; i < its.length; i++) { const v = fornext(its[i]); if (v === STOP) { if (strict && i > 0) fail(E.ValueError, "zip() argument " + (i + 1) + " is shorter than argument 1"); if (strict && i === 0) for (let j = 1; j < its.length; j++) if (fornext(its[j]) !== STOP) fail(E.ValueError, "zip() argument " + (j + 1) + " is longer than argument 1"); return STOP; } out.push(v); }
            return tuple(out);
        } };
    });
    rt.constructors.set(T.map, (args) => {
        const f = args[0]; const its = args.slice(1).map(iter);
        return { cls: T.map, next: () => { const vals = []; for (const it of its) { const v = fornext(it); if (v === STOP) return STOP; vals.push(v); } return call(f, vals, null); } };
    });
    rt.constructors.set(T.filter, (args) => {
        const f = args[0]; const it = iter(args[1]);
        return { cls: T.filter, next: () => { for (;;) { const v = fornext(it); if (v === STOP) return STOP; if (f === null ? truth(v) : truth(call(f, [v], null))) return v; } } };
    });
    rt.constructors.set(T.reversed, (args) => {
        const v = args[0];
        if (v !== null && typeof v === "object" && (v.cls === T.list || v.cls === T.tuple)) { let i = v.items.length; return { cls: T.reversed, next: () => i > 0 ? v.items[--i] : STOP }; }
        if (typeof v === "string") { const cps = codepoints(v); let i = cps.length; return { cls: T.reversed, next: () => i > 0 ? cps[--i] : STOP }; }
        const m = typeMethod(v, "__reversed__");
        if (m !== undefined) return call(descrGet(m, v, typeOf(v)), [], null);
        if (typeMethod(v, "__len__") !== undefined && typeMethod(v, "__getitem__") !== undefined) { let i = len(v); return { cls: T.reversed, next: () => i > 0n ? getitem(v, --i) : STOP }; }
        fail(E.TypeError, "'" + typeOf(v).name + "' object is not reversible");
    });

    for (const t of [T.list, T.tuple, T.dict, T.set, T.frozenset, T.str, T.range, T.bytes, T.int, T.float, T.bool, ObjectType]) {
        for (const v of t.dict.values()) if (v !== null && typeof v === "object" && v.cls === T.builtin_function_or_method) v.isBase = true;
    }

    // ---- builtin functions ---------------------------------------------------------------------------------------------------
    defkw("print", (a) => {
        const kw = kwOf(a, ["sep", "end", "file", "flush"]);
        const sep = kwget(kw, "sep", null), end = kwget(kw, "end", null), file = kwget(kw, "file", null);
        const text = a.map(str).join(sep === null ? " " : needStr(sep, "sep")) + (end === null ? "\n" : needStr(end, "end"));
        rt.writeOut(text, file);
        return null;
    });
    // Console output is line-buffered by the engine; keep partial lines until a newline.
    let outBuf = "", errBuf = "";
    rt.writeOut = function (text, file) {
        const toErr = file !== null && file !== undefined && file.isStderr === true;
        if (toErr) { errBuf += text; const i = errBuf.lastIndexOf("\n"); if (i >= 0) { console.error(errBuf.slice(0, i)); errBuf = errBuf.slice(i + 1); } return; }
        if (file !== null && file !== undefined && file.write !== undefined && !file.isStdout) { call(rt.getattr(file, "write"), [text], null); return; }
        outBuf += text;
        const i = outBuf.lastIndexOf("\n");
        if (i >= 0) { console.log(outBuf.slice(0, i)); outBuf = outBuf.slice(i + 1); }
    };
    rt.flushOut = function () { if (outBuf.length) { console.log(outBuf); outBuf = ""; } if (errBuf.length) { console.error(errBuf); errBuf = ""; } };
    def("len", 1, (a) => len(a[0]));
    def("repr", 1, (a) => repr(a[0]));
    def("ascii", 1, (a) => repr(a[0]).replace(/[^\x00-\x7f]/g, (c) => { const cp = c.codePointAt(0); return cp < 0x100 ? "\\x" + cp.toString(16).padStart(2, "0") : cp < 0x10000 ? "\\u" + cp.toString(16).padStart(4, "0") : "\\U" + cp.toString(16).padStart(8, "0"); }));
    def("abs", 1, (a) => { const v = a[0]; if (isInt(v)) { const i = asInt(v); return i < 0n ? -i : i; } if (typeof v === "number") return Math.abs(v); const r = callMethod(v, "__abs__", []); if (r !== undefined) return r; fail(E.TypeError, "bad operand type for abs(): '" + typeOf(v).name + "'"); });
    defkw("min", (a) => minmax(a, "lt", "min"));
    defkw("max", (a) => minmax(a, "gt", "max"));
    function minmax(a, op, name) {
        const kw = kwOf(a, ["key", "default"]);
        const key = kwget(kw, "key", null);
        let items;
        if (a.length === 1) { items = drain(a[0]); if (!items.length) { if (kw && kw.has("default")) return kw.get("default"); fail(E.ValueError, name + "() arg is an empty sequence"); } }
        else { if (a.length === 0) fail(E.TypeError, name + " expected at least 1 argument, got 0"); items = a; }
        let best = items[0], bestKey = key === null ? best : call(key, [best], null);
        for (let i = 1; i < items.length; i++) { const k = key === null ? items[i] : call(key, [items[i]], null); if (cmp(op, k, bestKey)) { best = items[i]; bestKey = k; } }
        return best;
    }
    defkw("sum", (a) => { const kw = kwOf(a, ["start"]); let acc = a[1] !== undefined ? a[1] : kwget(kw, "start", 0n); if (typeof acc === "string") fail(E.TypeError, "sum() can't sum strings [use ''.join(seq) instead]"); const it = iter(a[0]); for (;;) { const v = fornext(it); if (v === STOP) break; acc = R.binop("add", acc, v); } return acc; });
    defkw("sorted", (a) => { const kw = kwOf(a, ["key", "reverse"]); return list(rt.sortItems(drain(a[0]), kwget(kw, "key", null), truth(kwget(kw, "reverse", false)))); });
    def("any", 1, (a) => { const it = iter(a[0]); for (;;) { const v = fornext(it); if (v === STOP) return false; if (truth(v)) return true; } });
    def("all", 1, (a) => { const it = iter(a[0]); for (;;) { const v = fornext(it); if (v === STOP) return true; if (!truth(v)) return false; } });
    def("iter", 2, (a) => { if (a.length === 2) { const f = a[0], sentinel = a[1]; return { cls: T.iterator, next: () => { const v = call(f, [], null); return eq(v, sentinel) ? STOP : v; } }; } return iter(a[0]); }, 1);
    def("next", 2, (a) => { const it = a[0]; if (it === null || typeof it !== "object" || (it.next === undefined && typeMethod(it, "__next__") === undefined)) fail(E.TypeError, "'" + typeOf(it).name + "' object is not an iterator"); const v = it.next !== undefined ? fornext(it) : (() => { try { return callMethod(it, "__next__", []); } catch (e) { if (e && e.cls === E.StopIteration) return STOP; throw e; } })(); if (v === STOP) { if (a.length === 2) return a[1]; throw rt.makeExc(E.StopIteration, it.cls === T.generator && it.returned !== null ? [it.returned] : []); } return v; }, 1);
    def("isinstance", 2, (a) => { const t = a[1]; if (t !== null && typeof t === "object" && t.cls === T.tuple) return t.items.some((x) => rt.isinstanceCheck(a[0], x)); return rt.isinstanceCheck(a[0], t); });
    rt.isinstanceCheck = function (v, t) {
        if (!isType(t)) { const m = typeMethod(t, "__instancecheck__"); if (m !== undefined) return truth(call(descrGet(m, t, typeOf(t)), [v], null)); fail(E.TypeError, "isinstance() arg 2 must be a type, a tuple of types, or a union"); }
        if (t === T.int && typeof v === "boolean") return true;
        return isInstance(v, t);
    };
    def("issubclass", 2, (a) => { if (!isType(a[0])) fail(E.TypeError, "issubclass() arg 1 must be a class"); const t = a[1]; if (t !== null && typeof t === "object" && t.cls === T.tuple) return t.items.some((x) => isSubclass(a[0], x)); if (!isType(t)) fail(E.TypeError, "issubclass() arg 2 must be a class, a tuple of classes, or a union"); return isSubclass(a[0], t); });
    def("hasattr", 2, (a) => { try { rt.getattr(a[0], needStr(a[1])); return true; } catch (e) { if (e && e.cls && isSubclass(e.cls, E.AttributeError)) return false; throw e; } });
    def("getattr", 3, (a) => { try { return rt.getattr(a[0], needStr(a[1])); } catch (e) { if (a.length === 3 && e && e.cls && isSubclass(e.cls, E.AttributeError)) return a[2]; throw e; } }, 2);
    def("setattr", 3, (a) => rt.setattr(a[0], needStr(a[1]), a[2]));
    def("delattr", 2, (a) => R.delattr(a[0], needStr(a[1])));
    def("id", 1, (a) => BigInt(rt.ident(a[0]) || (typeof a[0] === "string" ? rt.hashInt(a[0]) : 0n)));
    def("hash", 1, (a) => rt.hashInt(a[0]));
    def("callable", 1, (a) => { const v = a[0]; return v !== null && typeof v === "object" && (v.cls === T.function || v.cls === T.builtin_function_or_method || v.cls === T.method || v.isType === true || typeMethod(v, "__call__") !== undefined); });
    def("chr", 1, (a) => { const n = Number(needInt(a[0])); if (n < 0 || n > 0x10FFFF) fail(E.ValueError, "chr() arg not in range(0x110000)"); return String.fromCodePoint(n); });
    def("ord", 1, (a) => { const s = needStr(a[0], "ord() expected string of length 1, but"); const cps = codepoints(s); if (cps.length !== 1) fail(E.TypeError, "ord() expected a character, but string of length " + cps.length + " found"); return BigInt(cps[0].codePointAt(0)); });
    def("round", 2, (a) => roundValue(a[0], a[1]), 1);
    def("divmod", 2, (a) => tuple([R.binop("floordiv", a[0], a[1]), R.binop("mod", a[0], a[1])]));
    def("pow", 3, (a) => { if (a.length === 3 && a[2] !== null) { let b = needInt(a[0]), e = needInt(a[1]), m = needInt(a[2]); if (m === 0n) fail(E.ValueError, "pow() 3rd argument cannot be 0"); if (e < 0n) fail(E.ValueError, "negative exponent with modulus is not supported"); let r = 1n; b = ((b % m) + m) % m; while (e > 0n) { if (e & 1n) r = (r * b) % m; e >>= 1n; b = (b * b) % m; } return r; } return R.binop("pow", a[0], a[1]); }, 2);
    def("bin", 1, (a) => { const v = rt.indexOf(a[0]); return (v < 0n ? "-0b" : "0b") + (v < 0n ? -v : v).toString(2); });
    def("oct", 1, (a) => { const v = rt.indexOf(a[0]); return (v < 0n ? "-0o" : "0o") + (v < 0n ? -v : v).toString(8); });
    def("hex", 1, (a) => { const v = rt.indexOf(a[0]); return (v < 0n ? "-0x" : "0x") + (v < 0n ? -v : v).toString(16); });
    def("format", 2, (a) => rt.formatValue(a[0], a[1] === undefined ? "" : needStr(a[1])), 1);
    def("input", 1, (a) => { if (a.length) rt.writeOut(str(a[0]), null); fail(E.EOFError, "input() is not available: no interactive console in this environment"); }, 0);
    def("open", -1, (a) => fail(E.OSError, "open() is not available: the Python sandbox has no filesystem"));
    def("exit", 1, (a) => { throw rt.makeExc(E.SystemExit, a.length ? [a[0]] : []); }, 0);
    B.set("quit", B.get("exit"));
    def("vars", 1, (a) => { if (a.length === 0) fail(E.TypeError, "vars() without arguments is not supported"); const v = a[0]; if (v !== null && typeof v === "object" && v.dict) return rt.dictFromMap(v.dict); if (v !== null && typeof v === "object" && v.cls === T.module) return rt.dictFromMap(v.globals); fail(E.TypeError, "vars() argument must have __dict__ attribute"); }, 0);
    def("dir", 1, (a) => list(a.length ? dirOf(a[0]) : []), 0);
    def("globals", 0, () => { const m = rt.entryModule(); return m ? rt.dictFromMap(m.globals) : dict(); });
    def("locals", 0, () => dict());
    def("type", 3, (a) => a.length === 1 ? typeOf(a[0]) : rt.buildclass(null, rt.mapFromDict(rt.asDict(a[2])), needStr(a[0]), drain(a[1]), null), 1);
    B.set("type", TypeType);
    def("__import__", -1, (a) => R.import(needStr(a[0]), null));
    def("__build_class__", -1, () => fail(E.RuntimeError, "__build_class__ is internal"));
    def("breakpoint", -1, () => null);
    def("help", -1, () => { rt.writeOut("help() is not available in this environment\n", null); return null; });
    def("aiter", 1, () => fail(E.TypeError, "async iteration is not supported"));
    def("memoryview", 1, (a) => a[0]);
    def("bytearray", -1, (a) => rt.construct(T.bytes, a, null));
    def("complex", -1, () => fail(E.TypeError, "complex numbers are not supported"));
    B.set("__name__", "builtins");
    B.set("__debug__", true);
    // `object` and friends double as constructors through `construct`.
    rt.constructors.set(ObjectType, (args, kw, cls) => { if (cls === ObjectType) { if (args.length || (kw && kw.size)) fail(E.TypeError, "object() takes no arguments"); return { cls: ObjectType, dict: new Map() }; } return null; });
    rt.constructors.delete(ObjectType);
})(__zipp_py);
