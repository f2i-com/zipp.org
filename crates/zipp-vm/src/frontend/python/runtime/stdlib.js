/* ZIPP Python runtime — string formatting and the built-in modules. Apache-2.0. */
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T, E = rt.E, STOP = rt.STOP, NOTIMPL = rt.NOTIMPL;
    const fail = rt.fail, typeOf = rt.typeOf, isType = rt.isType, isInstance = rt.isInstance;
    const list = rt.list, tuple = rt.tuple, call = rt.call, iter = rt.iter, fornext = rt.fornext, drain = rt.drain;
    const str = rt.str, repr = rt.repr, eq = rt.eq, cmp = rt.cmp, truth = rt.truth, len = rt.len, getitem = rt.getitem;
    const isInt = rt.isInt, asInt = rt.asInt, isNum = rt.isNum, toFloat = rt.toFloat, codepoints = rt.codepoints;
    const dict = rt.dict, dictGet = rt.dictGet, dictSet = rt.dictSet, dictEntries = rt.dictEntries;
    const builtin = rt.builtin, typeMethod = rt.typeMethod, descrGet = rt.descrGet, needInt = rt.needInt, needStr = rt.needStr;
    const kwOf = rt.kwOf, kwget = rt.kwget;

    // ---- format specs ---------------------------------------------------------------------------------------------------
    function parseSpec(spec) {
        const m = /^(?:(.)?([<>=^]))?([+\- ])?(z)?(#)?(0)?(\d+)?([,_])?(?:\.(\d+))?([bcdeEfFgGnosxX%])?$/.exec(spec);
        if (!m) fail(E.ValueError, "Invalid format specifier '" + spec + "'");
        return { fill: m[1], align: m[2], sign: m[3] || "-", alt: !!m[5], zero: !!m[6], width: m[7] ? parseInt(m[7], 10) : 0,
            group: m[8], precision: m[9] === undefined ? null : parseInt(m[9], 10), type: m[10] };
    }
    function group(digits, sep) { return digits.replace(/\B(?=(\d{3})+(?!\d))/g, sep); }
    function applyAlign(body, s, defaultAlign, signChar) {
        let fill = s.fill, align = s.align;
        if (align === undefined) { if (s.zero && s.type !== "s") { fill = "0"; align = "="; } else align = defaultAlign; }
        if (fill === undefined) fill = s.zero && align === "=" ? "0" : " ";
        const n = codepoints(body).length + (signChar || "").length;
        if (n >= s.width) return (signChar || "") + body;
        const padLen = s.width - n;
        switch (align) {
            case "<": return (signChar || "") + body + fill.repeat(padLen);
            case ">": return fill.repeat(padLen) + (signChar || "") + body;
            case "^": { const l = Math.floor(padLen / 2); return fill.repeat(l) + (signChar || "") + body + fill.repeat(padLen - l); }
            case "=": return (signChar || "") + fill.repeat(padLen) + body;
        }
        return body;
    }
    function signOf(neg, s) { return neg ? "-" : s.sign === "+" ? "+" : s.sign === " " ? " " : ""; }
    function formatFloat(x, s, type) {
        let p = s.precision === null ? 6 : s.precision;
        const neg = x < 0 || Object.is(x, -0);
        const ax = Math.abs(x);
        let body;
        if (!Number.isFinite(ax)) body = Number.isNaN(ax) ? "nan" : "inf";
        else switch (type) {
            case "f": case "F": body = rt.fixedHalfEven(ax, Math.min(p, 100)); break;
            case "e": case "E": body = expForm(ax, p); break;
            case "%": body = rt.fixedHalfEven(ax * 100, Math.min(p, 100)) + "%"; break;
            case "g": case "G": case undefined: {
                if (p === 0) p = 1;
                if (ax === 0) { body = type === undefined ? "0.0" : "0"; break; }
                const exp = Math.floor(Math.log10(ax));
                let e2 = exp;
                let r = expForm(ax, p - 1); const em = /e([+-]\d+)$/.exec(r); if (em) e2 = parseInt(em[1], 10);
                if (e2 < -4 || e2 >= (type === undefined && s.precision === null ? 16 : p)) {
                    body = expForm(ax, p - 1);
                    if (!s.alt) body = body.replace(/\.?0+e/, "e");
                } else {
                    body = ax.toFixed(Math.max(0, p - 1 - e2));
                    if (!s.alt && body.includes(".")) body = body.replace(/\.?0+$/, "");
                    if (type === undefined && s.precision === null) body = rt.floatRepr(ax);
                    else if (type === undefined && !body.includes(".") && !body.includes("e")) body += ".0";
                }
                break;
            }
        }
        if (type === "E" || type === "G" || type === "F") body = body.toUpperCase();
        if (s.group && Number.isFinite(ax)) { const i = body.indexOf("."); body = i < 0 ? group(body, s.group) : group(body.slice(0, i), s.group) + body.slice(i); }
        return applyAlign(body, s, ">", signOf(neg, s));
    }
    function expForm(ax, p) {
        let e = ax.toExponential(Math.min(p, 100));
        const m = /^(.*)e([+-])(\d+)$/.exec(e);
        return m[1] + "e" + m[2] + m[3].padStart(2, "0");
    }
    function formatInt(v, s) {
        const neg = v < 0n; const av = neg ? -v : v;
        let body;
        switch (s.type) {
            case undefined: case "d": case "n": body = av.toString(); break;
            case "b": body = (s.alt ? "0b" : "") + av.toString(2); break;
            case "o": body = (s.alt ? "0o" : "") + av.toString(8); break;
            case "x": body = (s.alt ? "0x" : "") + av.toString(16); break;
            case "X": body = (s.alt ? "0X" : "") + av.toString(16).toUpperCase(); break;
            case "c": return applyAlign(String.fromCodePoint(Number(v)), s, "<", "");
            case "e": case "E": case "f": case "F": case "g": case "G": case "%": return formatFloat(Number(v), s, s.type);
            default: fail(E.ValueError, "Unknown format code '" + s.type + "' for object of type 'int'");
        }
        const prefix = /^0[boxBOX]/.test(body) ? body.slice(0, 2) : "";
        let digits = body.slice(prefix.length);
        if (s.group) {
            if (s.type && "boxX".includes(s.type)) { if (s.group === ",") fail(E.ValueError, "Cannot specify ',' with '" + s.type + "'."); digits = digits.replace(/\B(?=(\w{4})+(?!\w))/g, "_"); }
            else digits = group(digits, s.group);
        }
        // Zero padding goes between the prefix/sign and the digits.
        if (s.zero && s.align === undefined && s.width !== undefined) {
            const sign = signOf(neg, s);
            const pad = Math.max(0, s.width - sign.length - prefix.length - digits.length);
            return sign + prefix + "0".repeat(pad) + digits;
        }
        return applyAlign(prefix + digits, s, ">", signOf(neg, s));
    }
    function formatValue(v, spec) {
        if (typeof v === "string") {
            const s = parseSpec(spec);
            if (s.type !== undefined && s.type !== "s") fail(E.ValueError, "Unknown format code '" + s.type + "' for object of type 'str'");
            if (s.sign !== "-" && spec.includes(s.sign)) fail(E.ValueError, "Sign not allowed in string format specifier");
            let body = v; if (s.precision !== null) body = codepoints(v).slice(0, s.precision).join("");
            return applyAlign(body, s, "<", "");
        }
        if (typeof v === "boolean" && spec === "") return v ? "True" : "False";
        if (isInt(v)) return formatInt(asInt(v), parseSpec(spec));
        if (typeof v === "number") { const s = parseSpec(spec); if (s.type && "bcdoxXn".includes(s.type)) fail(E.ValueError, "Unknown format code '" + s.type + "' for object of type 'float'"); return formatFloat(v, s, s.type); }
        if (v === null && spec === "") return "None";
        const m = typeMethod(v, "__format__");
        if (m !== undefined) { const r = call(descrGet(m, v, typeOf(v)), [spec], null); if (typeof r !== "string") fail(E.TypeError, "__format__ must return a str"); return r; }
        if (spec === "") return str(v);
        fail(E.TypeError, "unsupported format string passed to " + typeOf(v).name + ".__format__");
    }
    rt.formatValue = formatValue;
    R.fmt = function (v, spec, conv) {
        if (conv === 114) v = repr(v); else if (conv === 115) v = str(v); else if (conv === 97) v = call(rt.builtins.get("ascii"), [v], null);
        return formatValue(v, spec);
    };
    // str.format: {} {0} {name} {0.attr} {0[key]} {!r} {:spec} with nested {} in specs.
    rt.strFormat = function (s, args, kw) {
        let out = "", auto = 0, i = 0;
        const n = s.length;
        while (i < n) {
            const ch = s[i];
            if (ch === "{") {
                if (s[i + 1] === "{") { out += "{"; i += 2; continue; }
                let depth = 1, j = i + 1;
                while (j < n && depth > 0) { if (s[j] === "{") depth++; else if (s[j] === "}") depth--; if (depth > 0) j++; }
                if (depth !== 0) fail(E.ValueError, "Single '{' encountered in format string");
                const field = s.slice(i + 1, j);
                out += formatField(field, args, kw, () => auto++);
                i = j + 1;
            } else if (ch === "}") {
                if (s[i + 1] === "}") { out += "}"; i += 2; continue; }
                fail(E.ValueError, "Single '}' encountered in format string");
            } else { out += ch; i++; }
        }
        return out;
    };
    function formatField(field, args, kw, nextAuto) {
        let conv = null, spec = "";
        let name = field;
        const ci = field.indexOf("!"), si = field.indexOf(":");
        if (si >= 0 && (ci < 0 || si < ci)) { name = field.slice(0, si); spec = field.slice(si + 1); }
        else if (ci >= 0) { name = field.slice(0, ci); const rest = field.slice(ci + 1); conv = rest[0]; const s2 = rest.indexOf(":"); if (s2 >= 0) spec = rest.slice(s2 + 1); }
        if (spec.includes("{")) spec = rt.strFormat(spec, args, kw);
        const m = /^([^.\[]*)(.*)$/.exec(name);
        let base = m[1], rest = m[2];
        let v;
        if (base === "") { if (args.length === 0 && (kw === null || kw.size === 0)) fail(E.IndexError, "Replacement index 0 out of range for positional args tuple"); const k = nextAuto(); if (k >= args.length) fail(E.IndexError, "Replacement index " + k + " out of range for positional args tuple"); v = args[k]; }
        else if (/^\d+$/.test(base)) { const k = parseInt(base, 10); if (k >= args.length) fail(E.IndexError, "Replacement index " + k + " out of range for positional args tuple"); v = args[k]; }
        else { v = kw === null ? undefined : kw.get(base); if (v === undefined) throw rt.makeExc(E.KeyError, [base]); }
        while (rest.length) {
            if (rest[0] === ".") { const mm = /^\.([^.\[]+)(.*)$/.exec(rest); v = rt.getattr(v, mm[1]); rest = mm[2]; }
            else { const mm = /^\[([^\]]*)\](.*)$/.exec(rest); const key = /^\d+$/.test(mm[1]) ? BigInt(mm[1]) : mm[1]; v = getitem(v, key); rest = mm[2]; }
        }
        if (conv === "r") v = repr(v); else if (conv === "s") v = str(v); else if (conv === "a") v = call(rt.builtins.get("ascii"), [v], null);
        return formatValue(v, spec);
    }
    // printf-style `%` formatting.
    rt.percentFormat = function (s, values) {
        const args = values !== null && typeof values === "object" && values.cls === T.tuple ? values.items.slice() : [values];
        const mapping = values !== null && typeof values === "object" && values.cls === T.dict ? values : null;
        let out = "", i = 0, argi = 0;
        const re = /%(?:\(([^)]*)\))?([-+ #0]*)(\*|\d+)?(?:\.(\*|\d+))?([hlL])?([diouxXeEfFgGcrsa%])/g;
        let m;
        while ((m = re.exec(s)) !== null) {
            out += s.slice(i, m.index); i = re.lastIndex;
            if (m[6] === "%") { out += "%"; continue; }
            let width = m[3], prec = m[4];
            if (width === "*") width = String(needInt(args[argi++]));
            if (prec === "*") prec = String(needInt(args[argi++]));
            let v;
            if (m[1] !== undefined) { if (mapping === null) fail(E.TypeError, "format requires a mapping"); v = getitem(mapping, m[1]); }
            else { if (argi >= args.length) fail(E.TypeError, "not enough arguments for format string"); v = args[argi++]; }
            const flags = m[2] || "";
            const spec = { fill: undefined, align: flags.includes("-") ? "<" : undefined, sign: flags.includes("+") ? "+" : flags.includes(" ") ? " " : "-", alt: flags.includes("#"), zero: flags.includes("0") && !flags.includes("-"), width: width ? parseInt(width, 10) : 0, group: undefined, precision: prec === undefined ? null : parseInt(prec, 10), type: undefined };
            const t = m[6];
            // %-formatting right-aligns unless the `-` flag is given.
            if (t === "s") { spec.type = "s"; out += applyAlign(prec === undefined ? str(v) : codepoints(str(v)).slice(0, spec.precision).join(""), Object.assign(spec, { precision: null }), ">", ""); }
            else if (t === "r" || t === "a") { out += applyAlign(t === "a" ? rt.ascii(v) : repr(v), Object.assign(spec, { precision: null }), ">", ""); }
            else if (t === "c") { out += applyAlign(isInt(v) ? String.fromCodePoint(Number(asInt(v))) : str(v), spec, ">", ""); }
            else if ("diu".includes(t)) { if (typeof v === "number") v = BigInt(Math.trunc(v)); if (!isInt(v)) fail(E.TypeError, "%d format: a real number is required, not " + typeOf(v).name); spec.type = "d"; out += formatInt(asInt(v), spec); }
            else if ("oxX".includes(t)) { if (!isInt(v)) fail(E.TypeError, "%" + t + " format: an integer is required, not " + typeOf(v).name); spec.type = t; out += formatInt(asInt(v), spec); }
            else { if (!isNum(v)) fail(E.TypeError, "must be real number, not " + typeOf(v).name); out += formatFloat(toFloat(v), spec, t); }
        }
        out += s.slice(i);
        if (argi < args.length && mapping === null) fail(E.TypeError, "not all arguments converted during string formatting");
        return out;
    };

    // ---- module helpers -----------------------------------------------------------------------------------------------------
    function mod(name, define) {
        rt.builtinModules.set(name, () => {
            const m = rt.newModule(name, null);
            m.globals.set("__name__", name);
            define(m.globals, m);
            return m;
        });
    }
    rt.defineModule = mod;
    function fn(g, name, arity, code, minArity) { g.set(name, builtin(name, arity, code, minArity)); }
    function fnkw(g, name, code) { const f = builtin(name, -1, code); f.kwnames = true; g.set(name, f); }
    function pyClass(name, module, methods) {
        const d = new Map();
        for (const [k, v] of Object.entries(methods)) d.set(k, typeof v === "function" ? builtin(k, -1, v) : v);
        const t = rt.newType(name, [], d, module);
        return t;
    }

    // ---- ui: buffered drawing commands drained by the host --------------------------------------------------------------------
    mod("ui", (g) => {
        const MAX_UI = 100000;
        const num = (v) => { if (isNum(v)) return toFloat(v); fail(E.TypeError, "ui: a number is required, not " + typeOf(v).name); };
        const color = (v) => { if (typeof v !== "string" || v.length > 64) fail(E.TypeError, "ui: a color is a short string such as '#ff8800' or 'red'"); return v; };
        const emit = (c) => { if (__zipp_py_ui.length >= MAX_UI) fail(E.MemoryError, "ui command limit exceeded for one frame"); __zipp_py_ui.push(c); return null; };
        fn(g, "canvas", 2, (a) => emit(["canvas", num(a[0]), num(a[1])]));
        fn(g, "clear", 1, (a) => emit(["clear", color(a[0])]));
        fn(g, "rect", 5, (a) => emit(["rect", num(a[0]), num(a[1]), num(a[2]), num(a[3]), color(a[4])]));
        fn(g, "circle", 4, (a) => emit(["circle", num(a[0]), num(a[1]), num(a[2]), color(a[3])]));
        fn(g, "line", 5, (a) => emit(["line", num(a[0]), num(a[1]), num(a[2]), num(a[3]), color(a[4])]));
        fn(g, "text", 4, (a) => emit(["text", num(a[0]), num(a[1]), rt.checkedText(str(a[2])), color(a[3])]));
        fn(g, "font", 1, (a) => emit(["font", num(a[0])]));
        fn(g, "button", 5, (a) => { const x = num(a[0]), y = num(a[1]), w = num(a[2]), h = num(a[3]); emit(["button", x, y, w, h, rt.checkedText(str(a[4]))]); const i = __zipp_py_input; return !!i.clicked && i.mx >= x && i.mx < x + w && i.my >= y && i.my < y + h; });
        fn(g, "mouse", 0, () => { const i = __zipp_py_input; return tuple([BigInt(Math.trunc(i.mx)), BigInt(Math.trunc(i.my)), !!i.down]); });
        fn(g, "clicked", 0, () => !!__zipp_py_input.clicked);
        fn(g, "key", 1, (a) => __zipp_py_input.keys[needStr(a[0])] === true);
        fn(g, "width", 0, () => BigInt(Math.trunc(__zipp_py_input.w)));
        fn(g, "height", 0, () => BigInt(Math.trunc(__zipp_py_input.h)));
    });

    // ---- math ---------------------------------------------------------------------------------------------------------------------
    mod("math", (g) => {
        const f = (v) => { if (isNum(v)) return toFloat(v); const m = typeMethod(v, "__float__"); if (m) return call(descrGet(m, v, typeOf(v)), [], null); fail(E.TypeError, "must be real number, not " + typeOf(v).name); };
        const dom = (x) => { if (Number.isNaN(x)) fail(E.ValueError, "math domain error"); return x; };
        g.set("pi", Math.PI); g.set("e", Math.E); g.set("tau", 2 * Math.PI); g.set("inf", Infinity); g.set("nan", NaN);
        for (const [name, impl] of [["sin", Math.sin], ["cos", Math.cos], ["tan", Math.tan], ["asin", Math.asin], ["acos", Math.acos], ["atan", Math.atan], ["sinh", Math.sinh], ["cosh", Math.cosh], ["tanh", Math.tanh], ["asinh", Math.asinh], ["acosh", Math.acosh], ["atanh", Math.atanh], ["exp", Math.exp], ["expm1", Math.expm1], ["log1p", Math.log1p], ["log2", Math.log2], ["log10", Math.log10], ["fabs", Math.abs], ["cbrt", Math.cbrt], ["erf", (x) => erfExact(x)], ["erfc", (x) => 1 - erfExact(x)], ["gamma", (x) => gammaExact(x)], ["lgamma", (x) => lgammaExact(x)], ["exp2", (x) => Math.pow(2, x)]]) fn(g, name, 1, (a) => dom(impl(f(a[0]))));
        fn(g, "sqrt", 1, (a) => { const x = f(a[0]); if (x < 0) fail(E.ValueError, "math domain error"); return Math.sqrt(x); });
        fn(g, "log", 2, (a) => { const x = f(a[0]); if (x <= 0) fail(E.ValueError, "math domain error"); if (isInt(a[0]) && asInt(a[0]) > 0n && !Number.isFinite(x)) { const s = asInt(a[0]).toString(); return (Math.log(Number("0." + s)) + s.length * Math.LN10) / (a[1] === undefined ? 1 : Math.log(f(a[1]))); } return a[1] === undefined ? Math.log(x) : Math.log(x) / Math.log(f(a[1])); }, 1);
        fn(g, "pow", 2, (a) => Math.pow(f(a[0]), f(a[1])));
        fn(g, "atan2", 2, (a) => Math.atan2(f(a[0]), f(a[1])));
        fn(g, "hypot", -1, (a) => Math.hypot(...a.map(f)));
        fn(g, "floor", 1, (a) => { if (isInt(a[0])) return asInt(a[0]); const m = typeMethod(a[0], "__floor__"); if (m) return call(descrGet(m, a[0], typeOf(a[0])), [], null); const x = f(a[0]); if (!Number.isFinite(x)) fail(Number.isNaN(x) ? E.ValueError : E.OverflowError, "cannot convert float " + rt.floatRepr(x) + " to integer"); return BigInt(Math.floor(x)); });
        fn(g, "ceil", 1, (a) => { if (isInt(a[0])) return asInt(a[0]); const m = typeMethod(a[0], "__ceil__"); if (m) return call(descrGet(m, a[0], typeOf(a[0])), [], null); const x = f(a[0]); if (!Number.isFinite(x)) fail(Number.isNaN(x) ? E.ValueError : E.OverflowError, "cannot convert float " + rt.floatRepr(x) + " to integer"); return BigInt(Math.ceil(x)); });
        fn(g, "trunc", 1, (a) => { if (isInt(a[0])) return asInt(a[0]); const x = f(a[0]); if (!Number.isFinite(x)) fail(E.ValueError, "cannot convert float to integer"); return BigInt(Math.trunc(x)); });
        fn(g, "isnan", 1, (a) => Number.isNaN(f(a[0])));
        fn(g, "isinf", 1, (a) => { const x = f(a[0]); return x === Infinity || x === -Infinity; });
        fn(g, "isfinite", 1, (a) => Number.isFinite(f(a[0])));
        fnkw(g, "isclose", (a) => { const kw = kwOf(a, ["rel_tol", "abs_tol"]); const x = f(a[0]), y = f(a[1]); const rel = toFloat(kwget(kw, "rel_tol", 1e-9)), abs = toFloat(kwget(kw, "abs_tol", 0)); if (x === y) return true; if (!Number.isFinite(x) || !Number.isFinite(y)) return false; const d = Math.abs(x - y); return d <= Math.max(rel * Math.max(Math.abs(x), Math.abs(y)), abs); });
        fn(g, "degrees", 1, (a) => f(a[0]) * 180 / Math.PI);
        fn(g, "radians", 1, (a) => f(a[0]) * Math.PI / 180);
        fn(g, "copysign", 2, (a) => { const x = Math.abs(f(a[0])), y = f(a[1]); return y < 0 || Object.is(y, -0) ? -x : x; });
        fn(g, "fmod", 2, (a) => { const y = f(a[1]); if (y === 0) fail(E.ValueError, "math domain error"); return f(a[0]) % y; });
        fn(g, "modf", 1, (a) => { const x = f(a[0]); const i = Math.trunc(x); return tuple([x - i, i]); });
        fn(g, "frexp", 1, (a) => { let x = f(a[0]); if (x === 0 || !Number.isFinite(x)) return tuple([x, 0n]); let e = Math.ceil(Math.log2(Math.abs(x))); let m = x / Math.pow(2, e); if (Math.abs(m) >= 1) { m /= 2; e++; } if (Math.abs(m) < 0.5) { m *= 2; e--; } return tuple([m, BigInt(e)]); });
        fn(g, "ldexp", 2, (a) => f(a[0]) * Math.pow(2, Number(needInt(a[1]))));
        fn(g, "gcd", -1, (a) => { let r = 0n; for (const v of a) { let x = asInt(needInt(v)); if (x < 0n) x = -x; let y = r; while (y) { [x, y] = [y, x % y]; } r = x; } return r; });
        fn(g, "lcm", -1, (a) => { let r = 1n; for (const v of a) { let x = asInt(needInt(v)); if (x < 0n) x = -x; if (x === 0n) return 0n; let p = r, q = x; while (q) { [p, q] = [q, p % q]; } r = r / p * x; } return r; });
        fn(g, "factorial", 1, (a) => { const n = needInt(a[0]); if (n < 0n) fail(E.ValueError, "factorial() not defined for negative values"); let r = 1n; for (let i = 2n; i <= n; i++) r *= i; return r; });
        fn(g, "comb", 2, (a) => { const n = needInt(a[0]), k = needInt(a[1]); if (n < 0n || k < 0n) fail(E.ValueError, "n must be a non-negative integer"); if (k > n) return 0n; let r = 1n; const kk = k < n - k ? k : n - k; for (let i = 1n; i <= kk; i++) r = r * (n - kk + i) / i; return r; });
        fn(g, "perm", 2, (a) => { const n = needInt(a[0]); const k = a[1] === undefined || a[1] === null ? n : needInt(a[1]); if (k > n) return 0n; let r = 1n; for (let i = 0n; i < k; i++) r *= n - i; return r; }, 1);
        fn(g, "isqrt", 1, (a) => { const n = needInt(a[0]); if (n < 0n) fail(E.ValueError, "isqrt() argument must be nonnegative"); if (n < 2n) return n; let x = BigInt(Math.floor(Math.sqrt(Number(n)))); while (x * x > n) x--; while ((x + 1n) * (x + 1n) <= n) x++; return x; });
        fn(g, "fsum", 1, (a) => { let s = 0, c = 0; for (const v of drain(a[0])) { const x = f(v); const y = x - c; const t = s + y; c = (t - s) - y; s = t; } return s; });
        fn(g, "prod", -1, (a) => { let r = 1n; for (const v of drain(a[0])) r = R.binop("mul", r, v); return r; });
        fn(g, "dist", 2, (a) => { const p = drain(a[0]).map(f), q = drain(a[1]).map(f); return Math.hypot(...p.map((x, i) => x - q[i])); });
        // erf via its Maclaurin series near zero and a continued fraction of
        // erfc in the tails (both converge to double precision).
        function erfExact(x) {
            if (x === 0) return x;
            if (!Number.isFinite(x)) return Number.isNaN(x) ? x : (x > 0 ? 1 : -1);
            const ax = Math.abs(x);
            let r;
            if (ax < 1.5) {
                let term = ax, sum = ax;
                for (let n = 1; n < 100; n++) { term *= -ax * ax / n; const add = term / (2 * n + 1); sum += add; if (Math.abs(add) < 1e-17 * Math.abs(sum)) break; }
                r = sum * 2 / Math.sqrt(Math.PI);
            } else {
                // erfc(x) = exp(-x^2)/(x sqrt(pi)) * 1/(1+ 1/(2x^2)/(1+ 2/(2x^2)/(1+ ...))) (Lentz)
                let f = 0; for (let k = 60; k >= 1; k--) f = k / 2 / (ax + f);
                r = 1 - Math.exp(-ax * ax) / Math.sqrt(Math.PI) / (ax + f);
            }
            return x < 0 ? -r : r;
        }
        const LANCZOS = [0.99999999999980993, 676.5203681218851, -1259.1392167224028, 771.32342877765313, -176.61502916214059, 12.507343278686905, -0.13857109526572012, 9.9843695780195716e-6, 1.5056327351493116e-7];
        function gammaExact(x) {
            if (Number.isInteger(x)) {
                if (x <= 0) fail(E.ValueError, "math domain error");
                if (x <= 171) { let r = 1; for (let i = 2; i < x; i++) r *= i; return r; }
                return Infinity;
            }
            if (x < 0.5) return Math.PI / (Math.sin(Math.PI * x) * gammaExact(1 - x));
            x -= 1;
            let a = LANCZOS[0]; const t = x + 7.5;
            for (let i = 1; i < 9; i++) a += LANCZOS[i] / (x + i);
            return Math.sqrt(2 * Math.PI) * Math.pow(t, x + 0.5) * Math.exp(-t) * a;
        }
        function lgammaExact(x) {
            if (Number.isInteger(x) && x <= 0) fail(E.ValueError, "math domain error");
            if (x < 0.5) return Math.log(Math.PI / Math.abs(Math.sin(Math.PI * x))) - lgammaExact(1 - x);
            x -= 1;
            let a = LANCZOS[0]; const t = x + 7.5;
            for (let i = 1; i < 9; i++) a += LANCZOS[i] / (x + i);
            return 0.5 * Math.log(2 * Math.PI) + (x + 0.5) * Math.log(t) - t + Math.log(a);
        }
        function erf(x) { const t = 1 / (1 + 0.3275911 * Math.abs(x)); const y = 1 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * Math.exp(-x * x); return x >= 0 ? y : -y; }
        function gamma(x) { if (x < 0.5) return Math.PI / (Math.sin(Math.PI * x) * gamma(1 - x)); x -= 1; const p = [0.99999999999980993, 676.5203681218851, -1259.1392167224028, 771.32342877765313, -176.61502916214059, 12.507343278686905, -0.13857109526572012, 9.9843695780195716e-6, 1.5056327351493116e-7]; let a = p[0]; const t = x + 7.5; for (let i = 1; i < 9; i++) a += p[i] / (x + i); return Math.sqrt(2 * Math.PI) * Math.pow(t, x + 0.5) * Math.exp(-t) * a; }
    });

    // ---- random (deterministic xorshift, seedable) ----------------------------------------------------------------------------------
    mod("random", (g) => {
        let s0 = 0x9E3779B9, s1 = 0x243F6A88;
        function seed(v) { let h = 2166136261 ^ Number(BigInt.asUintN(32, BigInt(v))); h = Math.imul(h ^ (h >>> 16), 2246822507); h = Math.imul(h ^ (h >>> 13), 3266489909); s0 = (h ^ (h >>> 16)) >>> 0 || 1; s1 = (Math.imul(s0, 1597334677) ^ 0x5bd1e995) >>> 0 || 2; }
        function next32() { let x = s0, y = s1; s0 = y; x ^= x << 23; x ^= x >>> 17; x ^= y ^ (y >>> 26); s1 = x >>> 0; return (s0 + s1) >>> 0; }
        function random() { return (next32() * 2097152 + (next32() >>> 11)) / 9007199254740992; }
        seed(Date.now());
        fn(g, "seed", 1, (a) => { const v = a[0] === undefined || a[0] === null ? Date.now() : isInt(a[0]) ? asInt(a[0]) : typeof a[0] === "number" ? Math.trunc(a[0] * 1e6) : BigInt(rt.hashInt(a[0])); seed(v); return null; }, 0);
        fn(g, "random", 0, () => random());
        fn(g, "randint", 2, (a) => { const lo = needInt(a[0]), hi = needInt(a[1]); if (hi < lo) fail(E.ValueError, "empty range for randrange()"); return lo + BigInt(Math.floor(random() * Number(hi - lo + 1n))); });
        fn(g, "randrange", 3, (a) => { let start = needInt(a[0]), stop = a[1] === undefined || a[1] === null ? null : needInt(a[1]); const step = a[2] === undefined ? 1n : needInt(a[2]); if (stop === null) { stop = start; start = 0n; } const n = (stop - start + step - (step > 0n ? 1n : -1n)) / step; if (n <= 0n) fail(E.ValueError, "empty range for randrange()"); return start + step * BigInt(Math.floor(random() * Number(n))); }, 1);
        fn(g, "uniform", 2, (a) => { const x = toFloat(a[0]), y = toFloat(a[1]); return x + (y - x) * random(); });
        fn(g, "choice", 1, (a) => { const items = drain(a[0]); if (!items.length) fail(E.IndexError, "Cannot choose from an empty sequence"); return items[Math.floor(random() * items.length)]; });
        fnkw(g, "choices", (a) => { const kw = kwOf(a, ["weights", "k"]); const items = drain(a[0]); const k = Number(kwget(kw, "k", 1n)); const weights = kwget(kw, "weights", null); const out = []; if (weights === null) { for (let i = 0; i < k; i++) out.push(items[Math.floor(random() * items.length)]); return list(out); } const w = drain(weights).map(toFloat); const total = w.reduce((x, y) => x + y, 0); for (let i = 0; i < k; i++) { let r = random() * total, j = 0; while (j < w.length - 1 && r >= w[j]) { r -= w[j]; j++; } out.push(items[j]); } return list(out); });
        fn(g, "shuffle", 1, (a) => { const l = a[0].items; for (let i = l.length - 1; i > 0; i--) { const j = Math.floor(random() * (i + 1)); [l[i], l[j]] = [l[j], l[i]]; } return null; });
        fn(g, "sample", 2, (a) => { const items = drain(a[0]).slice(); const k = Number(needInt(a[1])); if (k > items.length) fail(E.ValueError, "Sample larger than population or is negative"); const out = []; for (let i = 0; i < k; i++) { const j = i + Math.floor(random() * (items.length - i)); [items[i], items[j]] = [items[j], items[i]]; out.push(items[i]); } return list(out); });
        fn(g, "gauss", 2, (a) => { const mu = toFloat(a[0]), sigma = toFloat(a[1]); const u = 1 - random(), v = random(); return mu + sigma * Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v); });
        g.set("normalvariate", g.get("gauss"));
        fn(g, "getrandbits", 1, (a) => { const k = Number(needInt(a[0])); let v = 0n; for (let i = 0; i < k; i += 32) v = (v << 32n) | BigInt(next32()); return v & ((1n << BigInt(k)) - 1n); });
    });

    // ---- time / sys / os / io ---------------------------------------------------------------------------------------------------------
    mod("time", (g) => {
        const start = Date.now();
        fn(g, "time", 0, () => Date.now() / 1000);
        fn(g, "time_ns", 0, () => BigInt(Date.now()) * 1000000n);
        fn(g, "perf_counter", 0, () => (Date.now() - start) / 1000);
        fn(g, "perf_counter_ns", 0, () => BigInt(Date.now() - start) * 1000000n);
        fn(g, "monotonic", 0, () => (Date.now() - start) / 1000);
        fn(g, "process_time", 0, () => (Date.now() - start) / 1000);
        fn(g, "sleep", 1, () => null);
        fn(g, "strftime", 2, (a) => { const d = new Date(); const pad = (n) => String(n).padStart(2, "0"); return needStr(a[0]).replace(/%([YmdHMSyjp%])/g, (_, c) => ({ Y: d.getFullYear(), m: pad(d.getMonth() + 1), d: pad(d.getDate()), H: pad(d.getHours()), M: pad(d.getMinutes()), S: pad(d.getSeconds()), y: pad(d.getFullYear() % 100), j: String(Math.floor((d - new Date(d.getFullYear(), 0, 0)) / 86400000)).padStart(3, "0"), p: d.getHours() < 12 ? "AM" : "PM", "%": "%" })[c]); }, 1);
        fn(g, "localtime", 1, () => { const d = new Date(); return tuple([BigInt(d.getFullYear()), BigInt(d.getMonth() + 1), BigInt(d.getDate()), BigInt(d.getHours()), BigInt(d.getMinutes()), BigInt(d.getSeconds()), BigInt((d.getDay() + 6) % 7), 1n, 0n]); }, 0);
        g.set("gmtime", g.get("localtime"));
    });
    mod("sys", (g) => {
        g.set("argv", list([rt.entryModule() ? rt.entryModule().file || "main.py" : "main.py"].concat(rt.argv)));
        g.set("version", "3.12.0 (zipp)"); g.set("version_info", tuple([3n, 12n, 0n, "final", 0n]));
        g.set("platform", "zipp"); g.set("maxsize", 9223372036854775807n); g.set("byteorder", "little");
        g.set("path", list([])); g.set("modules", dict()); g.set("executable", "zipp");
        const stream = (isErr) => { const t = pyClass(isErr ? "stderr" : "stdout", "sys", { write: (a) => { rt.writeOut(needStr(a[1]), isErr ? { isStderr: true } : null); return BigInt(a[1].length); }, flush: () => null }); const o = { cls: t, dict: new Map(), isStdout: !isErr, isStderr: isErr, write: true }; return o; };
        g.set("stdout", stream(false)); g.set("stderr", stream(true)); g.set("stdin", null);
        fn(g, "exit", 1, (a) => { throw rt.makeExc(E.SystemExit, a.length ? [a[0]] : []); }, 0);
        fn(g, "getrecursionlimit", 0, () => 1000n);
        fn(g, "setrecursionlimit", 1, () => null);
        fn(g, "getsizeof", 1, () => 64n);
        fn(g, "intern", 1, (a) => a[0]);
        fn(g, "exc_info", 0, () => { const e = rt.currentExc(); return e === null ? tuple([null, null, null]) : tuple([typeOf(e), e, null]); });
        fn(g, "getdefaultencoding", 0, () => "utf-8");
        g.set("float_info", pyClass("float_info", "sys", {}) && (() => { const o = { cls: pyClass("float_info", "sys", {}), dict: new Map([["max", Number.MAX_VALUE], ["min", 2.2250738585072014e-308], ["epsilon", Number.EPSILON], ["dig", 15n], ["mant_dig", 53n]]) }; return o; })());
    });
    mod("os", (g, m) => {
        const path = rt.newModule("os.path", null);
        const pg = path.globals;
        fn(pg, "join", -1, (a) => { let out = ""; for (const p of a) { const s = needStr(p); if (s.startsWith("/")) out = s; else if (out === "" || out.endsWith("/")) out += s; else out += "/" + s; } return out; });
        fn(pg, "basename", 1, (a) => { const s = needStr(a[0]); return s.slice(s.lastIndexOf("/") + 1); });
        fn(pg, "dirname", 1, (a) => { const s = needStr(a[0]); const i = s.lastIndexOf("/"); return i < 0 ? "" : i === 0 ? "/" : s.slice(0, i); });
        fn(pg, "splitext", 1, (a) => { const s = needStr(a[0]); const b = s.slice(s.lastIndexOf("/") + 1); const i = b.lastIndexOf("."); if (i <= 0) return tuple([s, ""]); return tuple([s.slice(0, s.length - b.length + i), b.slice(i)]); });
        fn(pg, "split", 1, (a) => { const s = needStr(a[0]); const i = s.lastIndexOf("/"); return tuple([i < 0 ? "" : i === 0 ? "/" : s.slice(0, i), s.slice(i + 1)]); });
        fn(pg, "exists", 1, (a) => rt.vfs.has(needStr(a[0])) || rt.vfs.isDir(needStr(a[0])));
        fn(pg, "isfile", 1, (a) => rt.vfs.has(needStr(a[0])));
        fn(pg, "isdir", 1, (a) => !rt.vfs.has(needStr(a[0])) && rt.vfs.isDir(needStr(a[0])));
        fn(pg, "getsize", 1, (a) => { const f = rt.vfs.get(needStr(a[0])); if (f === undefined) fail(E.FileNotFoundError, "[Errno 2] No such file or directory: '" + a[0] + "'"); return BigInt(f.length); });
        fn(pg, "realpath", 1, (a) => "/" + rt.vfs.norm(needStr(a[0])));
        fn(pg, "abspath", 1, (a) => { const s = needStr(a[0]); return s.startsWith("/") ? s : "/" + s; });
        fn(pg, "normpath", 1, (a) => needStr(a[0]).replace(/\/+/g, "/"));
        fn(pg, "isabs", 1, (a) => needStr(a[0]).startsWith("/"));
        fn(pg, "expanduser", 1, (a) => a[0]);
        pg.set("sep", "/"); pg.set("__name__", "os.path");
        m.submodules = new Map([["path", path]]);
        rt.modules.set("os.path", path);
        g.set("path", path); g.set("sep", "/"); g.set("linesep", "\n"); g.set("name", "posix");
        g.set("environ", dict());
        fn(g, "getcwd", 0, () => "/");
        fn(g, "listdir", 1, (a) => { const p = a[0] === undefined ? "" : needStr(a[0]); if (p !== "" && !rt.vfs.isDir(p)) fail(E.FileNotFoundError, "[Errno 2] No such file or directory: '" + p + "'"); return list(rt.vfs.listDir(p)); }, 0);
        fn(g, "getenv", 2, (a) => a[1] === undefined ? null : a[1], 1);
        fn(g, "getpid", 0, () => 1n);
        fnkw(g, "mkdir", (a) => { const p = needStr(a[0]); if (rt.vfs.has(p) || (rt.vfs.isDir(p) && rt.vfs.norm(p) !== "")) fail(E.FileExistsError, "[Errno 17] File exists: '" + p + "'"); rt.vfs.mkdir(p); return null; });
        fnkw(g, "makedirs", (a) => { const kw = kwOf(a, ["exist_ok"]); const p = needStr(a[0]); if (rt.vfs.has(p)) fail(E.FileExistsError, "[Errno 17] File exists: '" + p + "'"); if (rt.vfs.isDir(p) && rt.vfs.norm(p) !== "" && !truth(kwget(kw, "exist_ok", false)) && a[1] === undefined) fail(E.FileExistsError, "[Errno 17] File exists: '" + p + "'"); rt.vfs.mkdir(p); return null; });
        for (const n of ["remove", "unlink"]) fn(g, n, 1, (a) => { if (!rt.vfs.remove(needStr(a[0]))) fail(E.FileNotFoundError, "[Errno 2] No such file or directory: '" + a[0] + "'"); return null; });
        fn(g, "rename", 2, (a) => { const f = rt.vfs.get(needStr(a[0])); if (f === undefined) fail(E.FileNotFoundError, "[Errno 2] No such file or directory: '" + a[0] + "'"); rt.vfs.set(needStr(a[1]), f); rt.vfs.remove(needStr(a[0])); return null; });
        fn(g, "rmdir", 1, () => null);
        for (const n of ["chdir", "system"]) fn(g, n, -1, () => fail(E.OSError, "os." + n + " is not available in the Python sandbox"));
        fn(g, "urandom", 1, (a) => rt.bytes(Array.from({ length: Number(needInt(a[0])) }, () => Math.floor(Math.random() * 256))));
    });
    mod("io", (g) => {
        const StringIO = pyClass("StringIO", "io", {
            __init__: (a) => { a[0].dict.set("_buf", a.length > 1 ? needStr(a[1]) : ""); a[0].dict.set("_pos", 0); return null; },
            write: (a) => { const s = needStr(a[1]); a[0].dict.set("_buf", a[0].dict.get("_buf") + s); return BigInt(s.length); },
            getvalue: (a) => a[0].dict.get("_buf"),
            read: (a) => { const b = a[0].dict.get("_buf"), p = a[0].dict.get("_pos"); a[0].dict.set("_pos", b.length); return b.slice(p); },
            readline: (a) => { const b = a[0].dict.get("_buf"), p = a[0].dict.get("_pos"); const i = b.indexOf("\n", p); const end = i < 0 ? b.length : i + 1; a[0].dict.set("_pos", end); return b.slice(p, end); },
            readlines: (a) => list(a[0].dict.get("_buf").slice(a[0].dict.get("_pos")).split(/(?<=\n)/).filter((x) => x.length)),
            seek: (a) => { a[0].dict.set("_pos", Number(needInt(a[1]))); return a[1]; },
            tell: (a) => BigInt(a[0].dict.get("_pos")),
            close: () => null, flush: () => null,
            __enter__: (a) => a[0], __exit__: () => false,
            __iter__: (a) => iter(list(a[0].dict.get("_buf").split(/(?<=\n)/).filter((x) => x.length))),
        });
        g.set("StringIO", StringIO);
        const BytesIO = pyClass("BytesIO", "io", {
            __init__: (a) => { const init = a.length > 1 && a[1] !== null ? a[1] : null; if (init !== null && (typeof init !== "object" || init.cls !== T.bytes)) fail(E.TypeError, "a bytes-like object is required, not '" + typeOf(init).name + "'"); a[0].dict.set("_buf", init === null ? [] : init.items.slice()); a[0].dict.set("_pos", 0); return null; },
            write: (a) => { const b = a[1]; if (b === null || typeof b !== "object" || b.cls !== T.bytes) fail(E.TypeError, "a bytes-like object is required, not '" + typeOf(b).name + "'"); const buf = a[0].dict.get("_buf"); let pos = a[0].dict.get("_pos"); for (const x of b.items) buf[pos++] = x; a[0].dict.set("_pos", pos); return BigInt(b.items.length); },
            getvalue: (a) => rt.bytes(a[0].dict.get("_buf").slice()),
            read: (a) => { const buf = a[0].dict.get("_buf"), p = a[0].dict.get("_pos"); const n = a[1] === undefined || a[1] === null ? -1 : Number(needInt(a[1])); const end = n < 0 ? buf.length : Math.min(buf.length, p + n); a[0].dict.set("_pos", end); return rt.bytes(buf.slice(p, end)); },
            readline: (a) => { const buf = a[0].dict.get("_buf"), p = a[0].dict.get("_pos"); let i = buf.indexOf(10, p); i = i < 0 ? buf.length : i + 1; a[0].dict.set("_pos", i); return rt.bytes(buf.slice(p, i)); },
            seek: (a) => { const whence = a[2] === undefined ? 0 : Number(needInt(a[2])); const off = Number(needInt(a[1])); const buf = a[0].dict.get("_buf"); const pos = whence === 0 ? off : whence === 1 ? a[0].dict.get("_pos") + off : buf.length + off; a[0].dict.set("_pos", Math.max(0, pos)); return BigInt(Math.max(0, pos)); },
            tell: (a) => BigInt(a[0].dict.get("_pos")),
            truncate: (a) => { const buf = a[0].dict.get("_buf"); const n = a[1] === undefined ? a[0].dict.get("_pos") : Number(needInt(a[1])); buf.length = n; return BigInt(n); },
            close: () => null, flush: () => null, readable: () => true, writable: () => true, seekable: () => true,
            __enter__: (a) => a[0], __exit__: () => false,
        });
        g.set("BytesIO", BytesIO);
    });

    // ---- json ------------------------------------------------------------------------------------------------------------------------
    mod("json", (g) => {
        function toJs(v, seen) {
            if (v === null || typeof v === "boolean" || typeof v === "string") return v;
            if (isInt(v)) { const i = asInt(v); return i >= -9007199254740991n && i <= 9007199254740991n ? Number(i) : { __big: i.toString() }; }
            if (typeof v === "number") { if (!Number.isFinite(v)) return { __special: Number.isNaN(v) ? "NaN" : v > 0 ? "Infinity" : "-Infinity" }; return v; }
            if (v.cls === T.list || v.cls === T.tuple) { if (seen.has(v)) fail(E.ValueError, "Circular reference detected"); seen.add(v); const out = v.items.map((x) => toJs(x, seen)); seen.delete(v); return out; }
            if (v.cls === T.dict) { if (seen.has(v)) fail(E.ValueError, "Circular reference detected"); seen.add(v); const out = {}; for (const [k, x] of dictEntries(v)) { const key = typeof k === "string" ? k : k === null ? "null" : typeof k === "boolean" ? (k ? "true" : "false") : isNum(k) ? str(k) : fail(E.TypeError, "keys must be str, int, float, bool or None, not " + typeOf(k).name); out[key] = toJs(x, seen); } seen.delete(v); return out; }
            fail(E.TypeError, "Object of type " + typeOf(v).name + " is not JSON serializable");
        }
        function render(js, indent, level, sep, kv, sortKeys) {
            if (js === null) return "null";
            if (typeof js === "boolean") return js ? "true" : "false";
            if (typeof js === "number") return Number.isInteger(js) ? String(js) : rt.floatRepr(js);
            if (typeof js === "string") return JSON.stringify(js).replace(/[-￿]/g, (c) => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0"));
            if (Array.isArray(js)) { if (!js.length) return "[]"; if (indent === null) return "[" + js.map((x) => render(x, indent, level, sep, kv, sortKeys)).join(sep) + "]"; const pad = "\n" + " ".repeat(indent * (level + 1)); return "[" + pad + js.map((x) => render(x, indent, level + 1, sep, kv, sortKeys)).join("," + pad) + "\n" + " ".repeat(indent * level) + "]"; }
            if (js.__big !== undefined) return js.__big;
            if (js.__special !== undefined) return js.__special;
            const keys = Object.keys(js); if (sortKeys) keys.sort();
            if (!keys.length) return "{}";
            const items = keys.map((k) => render(k, indent, level, sep, kv, sortKeys) + kv + render(js[k], indent, level + 1, sep, kv, sortKeys));
            if (indent === null) return "{" + items.join(sep) + "}";
            const pad = "\n" + " ".repeat(indent * (level + 1));
            return "{" + pad + items.join("," + pad) + "\n" + " ".repeat(indent * level) + "}";
        }
        function dumps(v, kw) {
            const indentV = kwget(kw, "indent", null); const indent = indentV === null ? null : isInt(indentV) ? Number(asInt(indentV)) : indentV.length;
            const seps = kwget(kw, "separators", null);
            const sep = seps !== null ? str(seps.items[0]) : indent === null ? ", " : ",";
            const kv = seps !== null ? str(seps.items[1]) : ": ";
            const dflt = kwget(kw, "default", null);
            const conv = (x, seen) => { try { return toJs(x, seen); } catch (e) { if (dflt !== null && e && e.cls === E.TypeError) return toJs(call(dflt, [x], null), seen); throw e; } };
            const js = dflt === null ? toJs(v, new Set()) : (function deep(x, seen) { if (x === null || typeof x !== "object" || isInt(x)) return conv(x, seen); if (x.cls === T.list || x.cls === T.tuple) return x.items.map((y) => deep(y, seen)); if (x.cls === T.dict) { const out = {}; for (const [k, y] of dictEntries(x)) out[str(k)] = deep(y, seen); return out; } return deep(call(dflt, [x], null), seen); })(v, new Set());
            return render(js, indent, 0, sep, kv, truth(kwget(kw, "sort_keys", false)));
        }
        function fromJs(js) {
            if (js === null || typeof js === "boolean" || typeof js === "string") return js;
            if (typeof js === "number") return Number.isInteger(js) && !/[.eE]/.test(String(js)) ? BigInt(js) : js;
            if (Array.isArray(js)) return list(js.map(fromJs));
            const d = dict(); for (const k of Object.keys(js)) dictSet(d, k, fromJs(js[k])); return d;
        }
        // A small JSON reader producing Python values directly: ints stay
        // exact (BigInt) and `1.0` stays a float, which JSON.parse cannot do.
        function loads(s) {
            let i = 0;
            const n = s.length;
            const err = (msg) => { throw rt.makeExc(E.JSONDecodeError, [msg + ": line 1 column " + (i + 1) + " (char " + i + ")"]); };
            const ws = () => { while (i < n && (s[i] === " " || s[i] === "\t" || s[i] === "\n" || s[i] === "\r")) i++; };
            function value() {
                ws();
                if (i >= n) err("Expecting value");
                const c = s[i];
                if (c === "{") { i++; const d = dict(); ws(); if (s[i] === "}") { i++; return d; } for (;;) { ws(); if (s[i] !== '"') err("Expecting property name enclosed in double quotes"); const k = string(); ws(); if (s[i] !== ":") err("Expecting ':' delimiter"); i++; dictSet(d, k, value()); ws(); if (s[i] === ",") { i++; continue; } if (s[i] === "}") { i++; return d; } err("Expecting ',' delimiter"); } }
                if (c === "[") { i++; const items = []; ws(); if (s[i] === "]") { i++; return list(items); } for (;;) { items.push(value()); ws(); if (s[i] === ",") { i++; continue; } if (s[i] === "]") { i++; return list(items); } err("Expecting ',' delimiter"); } }
                if (c === '"') return string();
                if (s.startsWith("true", i)) { i += 4; return true; }
                if (s.startsWith("false", i)) { i += 5; return false; }
                if (s.startsWith("null", i)) { i += 4; return null; }
                if (s.startsWith("NaN", i)) { i += 3; return NaN; }
                if (s.startsWith("Infinity", i)) { i += 8; return Infinity; }
                if (s.startsWith("-Infinity", i)) { i += 9; return -Infinity; }
                const m = /^-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?/.exec(s.slice(i));
                if (!m) err("Expecting value");
                i += m[0].length;
                return m[2] || m[3] ? Number(m[0]) : BigInt(m[0]);
            }
            function string() {
                i++; let out = "";
                for (;;) {
                    if (i >= n) err("Unterminated string starting at");
                    const c = s[i++];
                    if (c === '"') return out;
                    if (c === "\\") {
                        const e = s[i++];
                        if (e === "u") { out += String.fromCharCode(parseInt(s.slice(i, i + 4), 16)); i += 4; }
                        else out += { n: "\n", t: "\t", r: "\r", b: "\b", f: "\f", "/": "/", "\\": "\\", '"': '"' }[e] || err("Invalid \\escape");
                    } else out += c;
                }
            }
            const v = value(); ws();
            if (i < n) err("Extra data");
            return v;
        }
        fnkw(g, "dumps", (a) => { const kw = kwOf(a, ["indent", "sort_keys", "separators", "default", "ensure_ascii"]); return dumps(a[0], kw); });
        fnkw(g, "loads", (a) => { kwOf(a, ["object_hook"]); return loads(needStr(a[0])); });
        fnkw(g, "dump", (a) => { const kw = kwOf(a, ["indent", "sort_keys", "separators", "default", "ensure_ascii"]); call(rt.getattr(a[1], "write"), [dumps(a[0], kw)], null); return null; });
        fnkw(g, "load", (a) => { kwOf(a, ["object_hook"]); return loads(needStr(call(rt.getattr(a[0], "read"), [], null))); });
        g.set("JSONDecodeError", E.JSONDecodeError);
    });

    // ---- string / textwrap / copy / operator / abc / typing / __future__ ------------------------------------------------------------------------
    mod("string", (g) => {
        g.set("ascii_lowercase", "abcdefghijklmnopqrstuvwxyz"); g.set("ascii_uppercase", "ABCDEFGHIJKLMNOPQRSTUVWXYZ");
        g.set("ascii_letters", "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"); g.set("digits", "0123456789");
        g.set("hexdigits", "0123456789abcdefABCDEF"); g.set("octdigits", "01234567"); g.set("punctuation", "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~");
        g.set("whitespace", " \t\n\r\x0b\x0c"); g.set("printable", "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~ \t\n\r\x0b\x0c");
        fn(g, "capwords", 1, (a) => needStr(a[0]).split(/\s+/).filter((x) => x).map((w) => w[0].toUpperCase() + w.slice(1).toLowerCase()).join(" "));
    });
    mod("textwrap", (g) => {
        fn(g, "dedent", 1, (a) => { const lines = needStr(a[0]).split("\n"); let margin = null; for (const l of lines) { if (!l.trim()) continue; const m = /^[ \t]*/.exec(l)[0]; margin = margin === null ? m : m.startsWith(margin) ? margin : margin.startsWith(m) ? m : ""; } return lines.map((l) => l.trim() ? l.slice(margin.length) : l.trim()).join("\n"); });
        fn(g, "indent", 2, (a) => needStr(a[0]).split("\n").map((l) => l.trim() ? a[1] + l : l).join("\n"));
        fnkw(g, "wrap", (a) => { const kw = kwOf(a, ["width"]); const width = Number(a[1] !== undefined ? needInt(a[1]) : kwget(kw, "width", 70n)); const words = needStr(a[0]).split(/\s+/).filter((x) => x); const out = []; let cur = ""; for (const w of words) { if (cur.length + w.length + (cur ? 1 : 0) > width && cur) { out.push(cur); cur = w; } else cur = cur ? cur + " " + w : w; } if (cur) out.push(cur); return list(out); });
        fnkw(g, "fill", (a) => { const kw = kwOf(a, ["width"]); const width = Number(a[1] !== undefined ? needInt(a[1]) : kwget(kw, "width", 70n)); return call(rt.modules.get("textwrap").globals.get("wrap"), [a[0], BigInt(width)], null).items.join("\n"); });
        fn(g, "shorten", 2, (a) => { const s = needStr(a[0]).split(/\s+/).join(" "); const w = Number(needInt(a[1])); return s.length <= w ? s : s.slice(0, Math.max(0, w - 6)).replace(/\s+\S*$/, "") + " [...]"; });
    });
    mod("copy", (g) => {
        function shallow(v) { if (v === null || typeof v !== "object") return v; const c = v.cls; if (c === T.list) return list(v.items.slice()); if (c === T.tuple) return v; if (c === T.dict) return rt.dictCopy(v); if (c === T.set) return rt.setFrom(v, T.set); const m = typeMethod(v, "__copy__"); if (m) return call(descrGet(m, v, c), [], null); if (v.dict !== undefined && !v.isType) { const o = rt.allocInstance(c); for (const [k, x] of v.dict) o.dict.set(k, x); if (v.items) o.items = v.items.slice(); return o; } return v; }
        function deep(v, memo) { if (v === null || typeof v !== "object") return v; if (memo.has(v)) return memo.get(v); const c = v.cls; let out; if (c === T.list) { out = list([]); memo.set(v, out); for (const x of v.items) out.items.push(deep(x, memo)); return out; } if (c === T.tuple) { out = tuple(v.items.map((x) => deep(x, memo))); return out; } if (c === T.dict) { out = dict(); memo.set(v, out); for (const [k, x] of dictEntries(v)) dictSet(out, deep(k, memo), deep(x, memo)); return out; } if (c === T.set) { out = rt.set(); memo.set(v, out); for (const x of rt.setValues(v)) rt.setAdd(out, deep(x, memo)); return out; } const m = typeMethod(v, "__deepcopy__"); if (m) return call(descrGet(m, v, c), [rt.dictFromMap(new Map())], null); if (v.dict !== undefined && !v.isType && c !== T.function && c !== T.module) { out = rt.allocInstance(c); memo.set(v, out); for (const [k, x] of v.dict) out.dict.set(k, deep(x, memo)); if (v.items) out.items = v.items.map((x) => deep(x, memo)); return out; } return v; }
        fn(g, "copy", 1, (a) => shallow(a[0]));
        fn(g, "deepcopy", 2, (a) => deep(a[0], new Map()), 1);
        rt.deepcopy = (v) => deep(v, new Map());
    });
    mod("operator", (g) => {
        for (const [name, op] of [["add", "add"], ["sub", "sub"], ["mul", "mul"], ["truediv", "truediv"], ["floordiv", "floordiv"], ["mod", "mod"], ["pow", "pow"], ["and_", "and"], ["or_", "or"], ["xor", "xor"], ["lshift", "lshift"], ["rshift", "rshift"]]) fn(g, name, 2, (a) => R.binop(op, a[0], a[1]));
        for (const [name, op] of [["eq", "eq"], ["ne", "ne"], ["lt", "lt"], ["le", "le"], ["gt", "gt"], ["ge", "ge"], ["contains", "in"]]) fn(g, name, 2, (a) => op === "in" ? rt.contains(a[0], a[1]) : cmp(op, a[0], a[1]));
        fn(g, "neg", 1, (a) => R.unop("neg", a[0])); fn(g, "not_", 1, (a) => !truth(a[0])); fn(g, "truth", 1, (a) => truth(a[0]));
        fn(g, "getitem", 2, (a) => getitem(a[0], a[1])); fn(g, "setitem", 3, (a) => rt.setitem(a[0], a[1], a[2])); fn(g, "delitem", 2, (a) => rt.delitem(a[0], a[1]));
        fn(g, "itemgetter", -1, (a) => builtin("itemgetter", 1, (b) => a.length === 1 ? getitem(b[0], a[0]) : tuple(a.map((k) => getitem(b[0], k)))));
        fn(g, "attrgetter", -1, (a) => builtin("attrgetter", 1, (b) => { const get = (o, n) => n.split(".").reduce((x, p) => rt.getattr(x, p), o); return a.length === 1 ? get(b[0], a[0]) : tuple(a.map((n) => get(b[0], n))); }));
        fn(g, "methodcaller", -1, (a) => builtin("methodcaller", 1, (b) => call(rt.getattr(b[0], a[0]), a.slice(1), null)));
        fn(g, "index", 1, (a) => rt.indexOf(a[0]));
        fn(g, "concat", 2, (a) => R.binop("add", a[0], a[1]));
        fn(g, "abs", 1, (a) => call(rt.builtins.get("abs"), [a[0]], null));
    });
    mod("abc", (g) => {
        const ABC = rt.newType("ABC", [], new Map(), "abc"); ABC.isABC = true; g.set("ABC", ABC);
        g.set("ABCMeta", rt.TypeType);
        fn(g, "abstractmethod", 1, (a) => { const f = a[0]; if (f !== null && typeof f === "object") f.isabstract = true; return f; });
        fn(g, "abstractproperty", 1, (a) => a[0]);
    });
    mod("typing", (g) => {
        // Generic aliases print like CPython's: typing.Dict[str, int].
        const typeName = (x) => isType(x) ? (x.module === "builtins" ? x.name : x.module + "." + x.qualname) : x === null ? "None" : repr(x);
        const any = pyClass("_Alias", "typing", {
            __getitem__: (a) => ({ cls: a[0].cls, dict: new Map([["name", a[0].dict.get("name")], ["args", a[1] !== null && typeof a[1] === "object" && a[1].cls === T.tuple ? a[1].items : [a[1]]]]) }),
            __repr__: (a) => { const args = a[0].dict.get("args"); return "typing." + a[0].dict.get("name") + (args === undefined ? "" : "[" + args.map(typeName).join(", ") + "]"); },
            __call__: (a) => a[1] === undefined ? null : a[1], __or__: (a) => a[0], __ror__: (a) => a[0] });
        for (const n of ["Any", "List", "Dict", "Set", "FrozenSet", "Tuple", "Optional", "Union", "Callable", "Iterable", "Iterator", "Generator", "Sequence", "Mapping", "MutableMapping", "Type", "TypeVar", "Generic", "Protocol", "ClassVar", "Final", "Literal", "NoReturn", "Hashable", "Sized", "Collection", "Awaitable", "Coroutine", "AsyncIterator", "Deque", "DefaultDict", "OrderedDict", "Counter", "ChainMap", "Text", "IO", "TextIO", "BinaryIO", "Pattern", "Match", "SupportsInt", "SupportsFloat", "Annotated", "Self", "TypeAlias", "ParamSpec", "Concatenate", "TypeGuard", "Never", "LiteralString", "Required", "NotRequired", "Unpack", "TypedDict", "NamedTuple"]) { const o = { cls: any, dict: new Map([["name", n]]) }; g.set(n, o); }
        g.set("TYPE_CHECKING", false);
        fn(g, "cast", 2, (a) => a[1]); fn(g, "overload", 1, (a) => a[0]); fn(g, "final", 1, (a) => a[0]); fn(g, "no_type_check", 1, (a) => a[0]);
        fn(g, "get_type_hints", -1, () => dict()); fn(g, "runtime_checkable", 1, (a) => a[0]);
        // typing.NamedTuple: the functional form is collections.namedtuple;
        // a subclass turns its annotations into fields (class attributes
        // with the same names are the defaults) and keeps its own methods.
        const namedtuple = (name, fields, defaults) => call(rt.modules.get("collections").globals.get("namedtuple"), [name, list(fields)], new Map([["defaults", list(defaults)]]));
        const NT = rt.newType("NamedTuple", [], new Map(), "typing");
        NT.dict.set("__init_subclass__", { cls: T.classmethod, func: builtin("__init_subclass__", -1, (a) => {
            const cls = a[0];
            if (cls.bases.indexOf(NT) < 0) return null;
            const ann = cls.dict.get("__annotations__");
            const fields = ann === undefined ? [] : rt.dictEntryList(ann).map((e) => needStr(e[0]));
            const defaults = [];
            for (const f of fields) { const v = cls.dict.get(f); if (v !== undefined) { defaults.push(v); cls.dict.delete(f); } else if (defaults.length) fail(E.TypeError, "Non-default namedtuple field " + f + " cannot follow default field" + (defaults.length === 1 ? "" : "s")); }
            const base = namedtuple(cls.name, fields, defaults);
            cls.bases = [base];
            cls.mro = rt.computeMro(cls);
            rt.bumpEpoch();
            return null;
        }) });
        NT.dict.set("__new__", { cls: T.staticmethod, func: builtin("__new__", -1, (a) => {
            // NamedTuple("Name", [("a", int), ...]) or NamedTuple("Name", a=int).
            const kwm = a[a.length - 1] instanceof Map ? a.pop() : null;
            const name = needStr(a[1]);
            const fields = a[2] !== undefined ? drain(a[2]).map((p) => needStr(rt.getitem(p, 0n))) : kwm ? Array.from(kwm.keys()) : [];
            return namedtuple(name, fields, []);
        }) });
        NT.dict.get("__new__").func.kwnames = true;
        g.set("NamedTuple", NT);
        // typing.TypedDict subclasses construct plain dicts.
        const TD = rt.newType("TypedDict", [], new Map(), "typing");
        TD.dict.set("__new__", { cls: T.staticmethod, func: builtin("__new__", -1, (a) => { const kwm = a[a.length - 1] instanceof Map ? a.pop() : null; const d = a[1] !== undefined ? rt.dictCopy(rt.asDict(a[1])) : dict(); if (kwm) for (const [k, v] of kwm) dictSet(d, k, v); return d; }) });
        TD.dict.get("__new__").func.kwnames = true;
        g.set("TypedDict", TD);
        for (const n of ["Protocol", "Literal", "ClassVar", "Final", "Annotated", "Self", "Never", "NoReturn", "TypeAlias", "Concatenate", "ParamSpec", "TypeVarTuple", "Unpack", "Required", "NotRequired", "LiteralString", "AnyStr", "Text", "Hashable", "Sized", "Awaitable", "Coroutine", "AsyncIterator", "AsyncIterable", "ContextManager", "Deque", "DefaultDict", "OrderedDict", "Counter", "ChainMap", "Collection", "Container", "MutableSequence", "MutableSet", "AbstractSet", "KeysView", "ValuesView", "ItemsView", "Reversible", "SupportsInt", "SupportsFloat", "SupportsIndex", "SupportsAbs", "SupportsRound", "ByteString", "Pattern", "Match", "IO", "TextIO", "BinaryIO"]) if (!g.has(n)) g.set(n, { cls: any, dict: new Map([["name", n]]) });
    });
    mod("__future__", (g) => { for (const n of ["annotations", "division", "print_function", "absolute_import", "unicode_literals", "generators", "nested_scopes", "with_statement", "generator_stop", "barry_as_FLUFL"]) g.set(n, null); });
    mod("builtins", (g) => { for (const [k, v] of rt.builtins) g.set(k, v); });

    // ---- itertools / functools / collections / heapq / bisect / statistics -------------------------------------------------------------------------
    mod("itertools", (g) => {
        fn(g, "compress", 2, (a) => { const data = rt.iter(a[0]), sel = rt.iter(a[1]); return { cls: T.iterator, next: () => { for (;;) { const d = rt.fornext(data); if (d === STOP) return STOP; const s = rt.fornext(sel); if (s === STOP) return STOP; if (rt.truth(s)) return d; } } }; });
        const gen = (next) => ({ cls: T.iterator, next: next });
        fn(g, "count", 2, (a) => { let v = a[0] === undefined ? 0n : a[0]; const step = a[1] === undefined ? 1n : a[1]; return gen(() => { const out = v; v = R.binop("add", v, step); return out; }); }, 0);
        fn(g, "cycle", 1, (a) => { const saved = []; const it = iter(a[0]); let i = 0, exhausted = false; return gen(() => { if (!exhausted) { const v = fornext(it); if (v !== STOP) { saved.push(v); return v; } exhausted = true; } if (!saved.length) return STOP; return saved[i++ % saved.length]; }); });
        fn(g, "repeat", 2, (a) => { let n = a[1] === undefined ? -1 : Number(needInt(a[1])); return gen(() => n === 0 ? STOP : (n > 0 && n--, a[0])); }, 1);
        fn(g, "chain", -1, (a) => { const its = a.map(iter); let i = 0; return gen(() => { while (i < its.length) { const v = fornext(its[i]); if (v !== STOP) return v; i++; } return STOP; }); });
        g.get("chain").dict = new Map([["from_iterable", builtin("from_iterable", 1, (a) => { const outer = iter(a[0]); let inner = null; return gen(() => { for (;;) { if (inner !== null) { const v = fornext(inner); if (v !== STOP) return v; inner = null; } const n = fornext(outer); if (n === STOP) return STOP; inner = iter(n); } }); })]]);
        fn(g, "islice", -1, (a) => { const it = iter(a[0]); let start = 0, stop = null, step = 1; if (a.length === 2) stop = a[1] === null ? null : Number(needInt(a[1])); else { start = a[1] === null ? 0 : Number(needInt(a[1])); stop = a[2] === null || a[2] === undefined ? null : Number(needInt(a[2])); if (a[3] !== undefined && a[3] !== null) step = Number(needInt(a[3])); } let i = 0, nextIdx = start; return gen(() => { for (;;) { if (stop !== null && nextIdx >= stop) return STOP; const v = fornext(it); if (v === STOP) return STOP; const idx = i++; if (idx === nextIdx) { nextIdx += step; return v; } } }); });
        fn(g, "zip_longest", -1, (a) => { const kw = kwOf(a, ["fillvalue"]); const fill = kwget(kw, "fillvalue", null); const its = a.map(iter); return gen(() => { let any = false; const out = its.map((it) => { const v = fornext(it); if (v === STOP) return fill; any = true; return v; }); return any ? tuple(out) : STOP; }); });
        g.get("zip_longest").kwnames = true;
        fn(g, "product", -1, (a) => { const kw = kwOf(a, ["repeat"]); let pools = a.map((x) => drain(x)); const rep = Number(kwget(kw, "repeat", 1n)); const all = []; for (let r = 0; r < rep; r++) for (const p of pools) all.push(p); let result = [[]]; for (const p of all) { const next = []; for (const r of result) for (const x of p) next.push(r.concat([x])); result = next; } let i = 0; return gen(() => i < result.length ? tuple(result[i++]) : STOP); });
        g.get("product").kwnames = true;
        fn(g, "permutations", 2, (a) => { const pool = drain(a[0]); const r = a[1] === undefined || a[1] === null ? pool.length : Number(needInt(a[1])); const out = []; const rec = (cur, used) => { if (cur.length === r) { out.push(tuple(cur.slice())); return; } for (let i = 0; i < pool.length; i++) { if (used[i]) continue; used[i] = true; cur.push(pool[i]); rec(cur, used); cur.pop(); used[i] = false; } }; rec([], new Array(pool.length).fill(false)); let i = 0; return gen(() => i < out.length ? out[i++] : STOP); }, 1);
        fn(g, "combinations", 2, (a) => { const pool = drain(a[0]); const r = Number(needInt(a[1])); const out = []; const rec = (start, cur) => { if (cur.length === r) { out.push(tuple(cur.slice())); return; } for (let i = start; i < pool.length; i++) { cur.push(pool[i]); rec(i + 1, cur); cur.pop(); } }; rec(0, []); let i = 0; return gen(() => i < out.length ? out[i++] : STOP); });
        fn(g, "combinations_with_replacement", 2, (a) => { const pool = drain(a[0]); const r = Number(needInt(a[1])); const out = []; const rec = (start, cur) => { if (cur.length === r) { out.push(tuple(cur.slice())); return; } for (let i = start; i < pool.length; i++) { cur.push(pool[i]); rec(i, cur); cur.pop(); } }; rec(0, []); let i = 0; return gen(() => i < out.length ? out[i++] : STOP); });
        fn(g, "accumulate", -1, (a) => { const kw = kwOf(a, ["initial", "func"]); const it = iter(a[0]); const f = a[1] !== undefined ? a[1] : kwget(kw, "func", null); let acc = kwget(kw, "initial", undefined); let first = acc === undefined; return gen(() => { if (first) { first = false; const v = fornext(it); if (v === STOP) return STOP; acc = v; return acc; } const v = fornext(it); if (v === STOP) return STOP; acc = f === null ? R.binop("add", acc, v) : call(f, [acc, v], null); return acc; }); });
        g.get("accumulate").kwnames = true;
        fn(g, "groupby", 2, (a) => { const it = iter(a[0]); const key = a[1] === undefined ? null : a[1]; let pending = fornext(it); return gen(() => { if (pending === STOP) return STOP; const k = key === null ? pending : call(key, [pending], null); const group = []; while (pending !== STOP && eq(key === null ? pending : call(key, [pending], null), k)) { group.push(pending); pending = fornext(it); } return tuple([k, iter(list(group))]); }); }, 1);
        fn(g, "takewhile", 2, (a) => { const it = iter(a[1]); let done = false; return gen(() => { if (done) return STOP; const v = fornext(it); if (v === STOP || !truth(call(a[0], [v], null))) { done = true; return STOP; } return v; }); });
        fn(g, "dropwhile", 2, (a) => { const it = iter(a[1]); let dropping = true; return gen(() => { for (;;) { const v = fornext(it); if (v === STOP) return STOP; if (dropping && truth(call(a[0], [v], null))) continue; dropping = false; return v; } }); });
        fn(g, "filterfalse", 2, (a) => { const it = iter(a[1]); return gen(() => { for (;;) { const v = fornext(it); if (v === STOP) return STOP; if (!(a[0] === null ? truth(v) : truth(call(a[0], [v], null)))) return v; } }); });
        fn(g, "starmap", 2, (a) => { const it = iter(a[1]); return gen(() => { const v = fornext(it); return v === STOP ? STOP : call(a[0], drain(v), null); }); });
        fn(g, "tee", 2, (a) => { const items = drain(a[0]); const n = a[1] === undefined ? 2 : Number(needInt(a[1])); return tuple(Array.from({ length: n }, () => iter(list(items)))); }, 1);
        fn(g, "pairwise", 1, (a) => { const items = drain(a[0]); let i = 0; return gen(() => i + 1 < items.length ? tuple([items[i], items[++i]]) : STOP); });
        fn(g, "batched", 2, (a) => { const items = drain(a[0]); const n = Number(needInt(a[1])); let i = 0; return gen(() => { if (i >= items.length) return STOP; const b = items.slice(i, i + n); i += n; return tuple(b); }); });
    });
    // ---- _zipp_gpu: the transport behind zipp_gpu.Graph.submit ------------------------------
    mod("_zipp_gpu", (g) => {
        fn(g, "hosted", 0, () => rt.hosted === true);
        fn(g, "post", 2, (a) => {
            const program = a[0];
            if (program === null || typeof program !== "object" || program.cls !== T.dict) fail(E.TypeError, "a program is a dict");
            const callback = a[1];
            if (!(callback !== null && typeof callback === "object" && (callback.cls === T.function || callback.cls === T.method || callback.cls === T.builtin_function_or_method))) fail(E.TypeError, "post() needs a callable");
            return BigInt(rt.postHost("gpu.execute", program, callback));
        });
        fn(g, "pending", 0, () => BigInt(rt.pendingHostRequests()));
    });
    // ---- struct: pack/unpack of the standard codes through a DataView ---------------------
    mod("struct", (g) => {
        const StructError = rt.newType("error", [E.Exception], new Map(), "struct");
        g.set("error", StructError);
        const SIZES = { x: 1, c: 1, b: 1, B: 1, "?": 1, h: 2, H: 2, i: 4, I: 4, l: 4, L: 4, q: 8, Q: 8, n: 8, N: 8, e: 2, f: 4, d: 8, s: 1, p: 1, P: 8 };
        const NATIVE_ALIGN = { h: 2, H: 2, i: 4, I: 4, l: 4, L: 4, q: 8, Q: 8, n: 8, N: 8, e: 2, f: 4, d: 8, P: 8 };
        function parse(fmt) {
            fmt = needStr(fmt);
            let little = true, native = true, i = 0;
            if (fmt.length && "@=<>!".includes(fmt[0])) { const c = fmt[0]; native = c === "@"; little = c === "<" || (c === "@" || c === "=" ? true : false); i = 1; }
            const items = []; let size = 0;
            while (i < fmt.length) {
                const ch = fmt[i];
                if (ch === " " || ch === "\t" || ch === "\n") { i++; continue; }
                let count = "";
                while (i < fmt.length && fmt[i] >= "0" && fmt[i] <= "9") count += fmt[i++];
                const code = fmt[i++];
                if (code === undefined || SIZES[code] === undefined) fail(StructError, "bad char in struct format");
                const n = count === "" ? 1 : parseInt(count, 10);
                if (native && NATIVE_ALIGN[code]) { const al = NATIVE_ALIGN[code]; size = Math.ceil(size / al) * al; }
                if (code === "s" || code === "p") { items.push({ code, count: n, offset: size }); size += n; }
                else for (let k = 0; k < n; k++) { items.push({ code, count: 1, offset: size }); size += SIZES[code]; }
            }
            return { little, items, size };
        }
        const RANGE = { b: [-128n, 127n], B: [0n, 255n], h: [-32768n, 32767n], H: [0n, 65535n], i: [-2147483648n, 2147483647n], I: [0n, 4294967295n], l: [-2147483648n, 2147483647n], L: [0n, 4294967295n], q: [-9223372036854775808n, 9223372036854775807n], Q: [0n, 18446744073709551615n], n: [-9223372036854775808n, 9223372036854775807n], N: [0n, 18446744073709551615n], P: [0n, 18446744073709551615n] };
        function pack(fmt, values) {
            const s = parse(fmt), buf = new ArrayBuffer(s.size), view = new DataView(buf), bytes = new Uint8Array(buf);
            const needed = s.items.filter((it) => it.code !== "x").length;
            if (values.length !== needed) fail(StructError, "pack expected " + needed + " items for packing (got " + values.length + ")");
            let vi = 0;
            for (const it of s.items) {
                const c = it.code;
                if (c === "x") continue;
                const v = values[vi++];
                if (c === "s" || c === "p") {
                    if (v === null || typeof v !== "object" || v.cls !== T.bytes) fail(StructError, "argument for '" + c + "' must be a bytes object");
                    for (let k = 0; k < it.count && k < v.items.length; k++) bytes[it.offset + k] = v.items[k];
                    continue;
                }
                if (c === "c") { if (v === null || typeof v !== "object" || v.cls !== T.bytes || v.items.length !== 1) fail(StructError, "char format requires a bytes object of length 1"); bytes[it.offset] = v.items[0]; continue; }
                if (c === "?") { bytes[it.offset] = rt.truth(v) ? 1 : 0; continue; }
                if (c === "f" || c === "d" || c === "e") {
                    if (!isNum(v)) fail(StructError, "required argument is not a float");
                    const x = toFloat(v);
                    if (c === "f") { if (Number.isFinite(x) && !Number.isFinite(Math.fround(x))) fail(E.OverflowError, "float too large to pack with f format"); view.setFloat32(it.offset, x, s.little); }
                    else if (c === "d") view.setFloat64(it.offset, x, s.little);
                    else view.setUint16(it.offset, halfBits(x), s.little);
                    continue;
                }
                if (!isInt(v)) fail(StructError, "required argument is not an integer");
                const n = asInt(v), [lo, hi] = RANGE[c];
                if (n < lo || n > hi) fail(StructError, "'" + c + "' format requires " + lo + " <= number <= " + hi);
                const size = SIZES[c];
                if (size === 8) { if (c === "q" || c === "n") view.setBigInt64(it.offset, n, s.little); else view.setBigUint64(it.offset, n, s.little); }
                else if (size === 4) { if (c === "i" || c === "l") view.setInt32(it.offset, Number(n), s.little); else view.setUint32(it.offset, Number(n), s.little); }
                else if (size === 2) { if (c === "h") view.setInt16(it.offset, Number(n), s.little); else view.setUint16(it.offset, Number(n), s.little); }
                else { if (c === "b") view.setInt8(it.offset, Number(n)); else view.setUint8(it.offset, Number(n)); }
            }
            return rt.bytes(Array.from(bytes));
        }
        function halfBits(x) {
            // IEEE 754 binary16 with round-to-nearest-even.
            if (Number.isNaN(x)) return 0x7e00;
            const sign = x < 0 || Object.is(x, -0) ? 0x8000 : 0; x = Math.abs(x);
            if (x === Infinity) return sign | 0x7c00;
            if (x === 0) return sign;
            if (x >= 65520) fail(E.OverflowError, "float too large to pack with e format");
            let e = Math.floor(Math.log2(x)); let m = x / Math.pow(2, e);
            if (m >= 2) { m /= 2; e++; } else if (m < 1) { m *= 2; e--; }
            if (e < -14) { const sub = Math.round(x / Math.pow(2, -24)); return sign | sub; }
            let frac = Math.round((m - 1) * 1024);
            if (frac === 1024) { frac = 0; e++; }
            return sign | ((e + 15) << 10) | frac;
        }
        function halfValue(bits) {
            const sign = bits & 0x8000 ? -1 : 1, e = (bits >> 10) & 0x1f, f = bits & 0x3ff;
            if (e === 0) return sign * f * Math.pow(2, -24);
            if (e === 31) return f ? NaN : sign * Infinity;
            return sign * (1 + f / 1024) * Math.pow(2, e - 15);
        }
        function unpack(fmt, data, offset) {
            const s = parse(fmt);
            if (data === null || typeof data !== "object" || data.cls !== T.bytes) fail(E.TypeError, "a bytes-like object is required, not '" + typeOf(data).name + "'");
            offset = offset === undefined ? 0 : Number(needInt(offset));
            if (data.items.length - offset !== s.size && offset === 0) fail(StructError, "unpack requires a buffer of " + s.size + " bytes");
            if (data.items.length - offset < s.size) fail(StructError, "unpack_from requires a buffer of at least " + (s.size + offset) + " bytes");
            const bytes = Uint8Array.from(data.items.slice(offset, offset + s.size)), view = new DataView(bytes.buffer);
            const out = [];
            for (const it of s.items) {
                const c = it.code;
                if (c === "x") continue;
                if (c === "s") { out.push(rt.bytes(Array.from(bytes.slice(it.offset, it.offset + it.count)))); continue; }
                if (c === "p") { const n = Math.min(bytes[it.offset], it.count - 1); out.push(rt.bytes(Array.from(bytes.slice(it.offset + 1, it.offset + 1 + n)))); continue; }
                if (c === "c") { out.push(rt.bytes([bytes[it.offset]])); continue; }
                if (c === "?") { out.push(bytes[it.offset] !== 0); continue; }
                if (c === "f") { out.push(view.getFloat32(it.offset, s.little)); continue; }
                if (c === "d") { out.push(view.getFloat64(it.offset, s.little)); continue; }
                if (c === "e") { out.push(halfValue(view.getUint16(it.offset, s.little))); continue; }
                const size = SIZES[c];
                if (size === 8) out.push(c === "q" || c === "n" ? view.getBigInt64(it.offset, s.little) : view.getBigUint64(it.offset, s.little));
                else if (size === 4) out.push(BigInt(c === "i" || c === "l" ? view.getInt32(it.offset, s.little) : view.getUint32(it.offset, s.little)));
                else if (size === 2) out.push(BigInt(c === "h" ? view.getInt16(it.offset, s.little) : view.getUint16(it.offset, s.little)));
                else out.push(BigInt(c === "b" ? view.getInt8(it.offset) : view.getUint8(it.offset)));
            }
            return tuple(out);
        }
        fn(g, "pack", -1, (a) => pack(a[0], a.slice(1)));
        fn(g, "unpack", 2, (a) => unpack(a[0], a[1]));
        fn(g, "unpack_from", 3, (a) => unpack(a[0], a[1], a[2]), 2);
        fn(g, "calcsize", 1, (a) => BigInt(parse(a[0]).size));
        fn(g, "iter_unpack", 2, (a) => { const s = parse(a[0]); const data = a[1]; const out = []; for (let off = 0; off + s.size <= data.items.length; off += s.size) out.push(unpack(a[0], data, off)); return iter(list(out)); });
        const Struct = rt.newType("Struct", [rt.ObjectType], new Map(), "struct");
        Struct.dict.set("__init__", builtin("__init__", 2, (a) => { a[0].dict.set("format", needStr(a[1])); a[0].dict.set("size", BigInt(parse(a[1]).size)); return null; }));
        Struct.dict.set("pack", builtin("pack", -1, (a) => pack(a[0].dict.get("format"), a.slice(1))));
        Struct.dict.set("unpack", builtin("unpack", 2, (a) => unpack(a[0].dict.get("format"), a[1])));
        Struct.dict.set("unpack_from", builtin("unpack_from", 3, (a) => unpack(a[0].dict.get("format"), a[1], a[2]), 2));
        g.set("Struct", Struct);
    });
    // ---- hashlib: SHA-256, SHA-1 and MD5 in JavaScript -------------------------------------------------------
    mod("hashlib", (g) => {
        function toBytes(v) {
            if (v === undefined) return [];
            if (v !== null && typeof v === "object" && v.cls === T.bytes) return v.items;
            if (typeof v === "string") fail(E.TypeError, "Strings must be encoded before hashing");
            fail(E.TypeError, "object supporting the buffer API required");
        }
        const rotr = (x, n) => (x >>> n) | (x << (32 - n));
        function sha256(bytes) {
            const K = [0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2];
            let H = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
            const n = bytes.length, padded = new Uint8Array(((n + 9 + 63) >> 6) << 6);
            padded.set(bytes); padded[n] = 0x80;
            const bits = n * 8; padded[padded.length - 4] = (bits >>> 24) & 255; padded[padded.length - 3] = (bits >>> 16) & 255; padded[padded.length - 2] = (bits >>> 8) & 255; padded[padded.length - 1] = bits & 255;
            padded[padded.length - 5] = Math.floor(bits / 4294967296) & 255;
            const W = new Int32Array(64);
            for (let off = 0; off < padded.length; off += 64) {
                for (let i = 0; i < 16; i++) W[i] = (padded[off + 4 * i] << 24) | (padded[off + 4 * i + 1] << 16) | (padded[off + 4 * i + 2] << 8) | padded[off + 4 * i + 3];
                for (let i = 16; i < 64; i++) { const s0 = rotr(W[i - 15], 7) ^ rotr(W[i - 15], 18) ^ (W[i - 15] >>> 3); const s1 = rotr(W[i - 2], 17) ^ rotr(W[i - 2], 19) ^ (W[i - 2] >>> 10); W[i] = (W[i - 16] + s0 + W[i - 7] + s1) | 0; }
                let [a, b, c, d, e, f, gg, h] = H;
                for (let i = 0; i < 64; i++) {
                    const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25), ch = (e & f) ^ (~e & gg), t1 = (h + S1 + ch + K[i] + W[i]) | 0;
                    const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22), maj = (a & b) ^ (a & c) ^ (b & c), t2 = (S0 + maj) | 0;
                    h = gg; gg = f; f = e; e = (d + t1) | 0; d = c; c = b; b = a; a = (t1 + t2) | 0;
                }
                H = [(H[0] + a) | 0, (H[1] + b) | 0, (H[2] + c) | 0, (H[3] + d) | 0, (H[4] + e) | 0, (H[5] + f) | 0, (H[6] + gg) | 0, (H[7] + h) | 0];
            }
            const out = [];
            for (const w of H) out.push((w >>> 24) & 255, (w >>> 16) & 255, (w >>> 8) & 255, w & 255);
            return out;
        }
        function sha1(bytes) {
            let H = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
            const n = bytes.length, padded = new Uint8Array(((n + 9 + 63) >> 6) << 6);
            padded.set(bytes); padded[n] = 0x80;
            const bits = n * 8; padded[padded.length - 4] = (bits >>> 24) & 255; padded[padded.length - 3] = (bits >>> 16) & 255; padded[padded.length - 2] = (bits >>> 8) & 255; padded[padded.length - 1] = bits & 255;
            padded[padded.length - 5] = Math.floor(bits / 4294967296) & 255;
            const W = new Int32Array(80);
            const rotl = (x, k) => (x << k) | (x >>> (32 - k));
            for (let off = 0; off < padded.length; off += 64) {
                for (let i = 0; i < 16; i++) W[i] = (padded[off + 4 * i] << 24) | (padded[off + 4 * i + 1] << 16) | (padded[off + 4 * i + 2] << 8) | padded[off + 4 * i + 3];
                for (let i = 16; i < 80; i++) W[i] = rotl(W[i - 3] ^ W[i - 8] ^ W[i - 14] ^ W[i - 16], 1);
                let [a, b, c, d, e] = H;
                for (let i = 0; i < 80; i++) {
                    let f, k;
                    if (i < 20) { f = (b & c) | (~b & d); k = 0x5A827999; } else if (i < 40) { f = b ^ c ^ d; k = 0x6ED9EBA1; } else if (i < 60) { f = (b & c) | (b & d) | (c & d); k = 0x8F1BBCDC; } else { f = b ^ c ^ d; k = 0xCA62C1D6; }
                    const t = (rotl(a, 5) + f + e + k + W[i]) | 0; e = d; d = c; c = rotl(b, 30); b = a; a = t;
                }
                H = [(H[0] + a) | 0, (H[1] + b) | 0, (H[2] + c) | 0, (H[3] + d) | 0, (H[4] + e) | 0];
            }
            const out = [];
            for (const w of H) out.push((w >>> 24) & 255, (w >>> 16) & 255, (w >>> 8) & 255, w & 255);
            return out;
        }
        function md5(bytes) {
            const S = [7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21];
            const K = []; for (let i = 0; i < 64; i++) K.push(Math.floor(Math.abs(Math.sin(i + 1)) * 4294967296) | 0);
            let a0 = 0x67452301, b0 = 0xefcdab89 | 0, c0 = 0x98badcfe | 0, d0 = 0x10325476;
            const n = bytes.length, padded = new Uint8Array(((n + 9 + 63) >> 6) << 6);
            padded.set(bytes); padded[n] = 0x80;
            const bits = n * 8; padded[padded.length - 8] = bits & 255; padded[padded.length - 7] = (bits >>> 8) & 255; padded[padded.length - 6] = (bits >>> 16) & 255; padded[padded.length - 5] = (bits >>> 24) & 255; padded[padded.length - 4] = Math.floor(bits / 4294967296) & 255;
            const rotl = (x, k) => (x << k) | (x >>> (32 - k));
            for (let off = 0; off < padded.length; off += 64) {
                const M = new Int32Array(16);
                for (let i = 0; i < 16; i++) M[i] = padded[off + 4 * i] | (padded[off + 4 * i + 1] << 8) | (padded[off + 4 * i + 2] << 16) | (padded[off + 4 * i + 3] << 24);
                let A = a0, B = b0, C = c0, D = d0;
                for (let i = 0; i < 64; i++) {
                    let F, g;
                    if (i < 16) { F = (B & C) | (~B & D); g = i; } else if (i < 32) { F = (D & B) | (~D & C); g = (5 * i + 1) % 16; } else if (i < 48) { F = B ^ C ^ D; g = (3 * i + 5) % 16; } else { F = C ^ (B | ~D); g = (7 * i) % 16; }
                    F = (F + A + K[i] + M[g]) | 0; A = D; D = C; C = B; B = (B + rotl(F, S[i])) | 0;
                }
                a0 = (a0 + A) | 0; b0 = (b0 + B) | 0; c0 = (c0 + C) | 0; d0 = (d0 + D) | 0;
            }
            const out = [];
            for (const w of [a0, b0, c0, d0]) out.push(w & 255, (w >>> 8) & 255, (w >>> 16) & 255, (w >>> 24) & 255);
            return out;
        }
        const ALGOS = { sha256: [sha256, 32], sha1: [sha1, 20], md5: [md5, 16] };
        const Hash = rt.newType("HASH", [rt.ObjectType], new Map(), "hashlib");
        const state = (self) => self.dict.get("_data");
        Hash.dict.set("update", builtin("update", 2, (a) => { const d = state(a[0]); for (const b of toBytes(a[1])) d.push(b); return null; }));
        Hash.dict.set("digest", builtin("digest", 1, (a) => rt.bytes(ALGOS[a[0].dict.get("name")][0](state(a[0])))));
        Hash.dict.set("hexdigest", builtin("hexdigest", 1, (a) => ALGOS[a[0].dict.get("name")][0](state(a[0])).map((b) => b.toString(16).padStart(2, "0")).join("")));
        Hash.dict.set("copy", builtin("copy", 1, (a) => ({ cls: Hash, dict: new Map([["name", a[0].dict.get("name")], ["_data", state(a[0]).slice()], ["digest_size", a[0].dict.get("digest_size")]]) })));
        const make = (name) => (a) => ({ cls: Hash, dict: new Map([["name", name], ["_data", toBytes(a[0]).slice()], ["digest_size", BigInt(ALGOS[name][1])]]) });
        for (const name of Object.keys(ALGOS)) fn(g, name, 1, make(name), 0);
        fn(g, "new", 2, (a) => { const name = needStr(a[0]).toLowerCase(); if (!ALGOS[name]) fail(E.ValueError, "unsupported hash type " + name); return make(name)(a.slice(1)); }, 1);
        g.set("algorithms_available", rt.set());
        for (const name of Object.keys(ALGOS)) rt.setAdd(g.get("algorithms_available"), name);
        g.set("algorithms_guaranteed", g.get("algorithms_available"));
    });
    mod("importlib", (g) => {
        fn(g, "import_module", 2, (a) => R.import(needStr(a[0]), null), 1);
        fn(g, "reload", 1, (a) => a[0]);
        fn(g, "invalidate_caches", 0, () => null);
    });
    mod("platform", (g) => {
        fn(g, "platform", 0, () => "Zipp-" + rt.version);
        fn(g, "system", 0, () => "Zipp"); fn(g, "machine", 0, () => "wasm32"); fn(g, "processor", 0, () => "");
        fn(g, "python_version", 0, () => "3.12.0"); fn(g, "python_implementation", 0, () => "Zipp");
        fn(g, "node", 0, () => "sandbox"); fn(g, "release", 0, () => rt.version); fn(g, "version", 0, () => rt.version);
        fn(g, "architecture", 0, () => tuple(["32bit", "wasm"]));
    });
    mod("contextlib", (g) => {
        // A generator-backed context manager: __enter__ runs to the first
        // yield, __exit__ resumes it (throwing the block's exception in).
        const GCM = rt.newType("_GeneratorContextManager", [rt.ObjectType], new Map(), "contextlib");
        const stopped = (e) => e !== null && typeof e === "object" && e.cls !== undefined && isInstance(e, E.StopIteration);
        GCM.dict.set("__enter__", builtin("__enter__", 1, (a) => {
            const gen = a[0].gen;
            try { const v = gen.next(); if (v === STOP) fail(E.RuntimeError, "generator didn't yield"); return v; }
            catch (e) { if (stopped(e)) fail(E.RuntimeError, "generator didn't yield"); throw e; }
        }));
        GCM.dict.set("__exit__", builtin("__exit__", 4, (a) => {
            const gen = a[0].gen, exc = a[2];
            if (exc === null) {
                try { const v = gen.next(); if (v !== STOP) fail(E.RuntimeError, "generator didn't stop"); }
                catch (e) { if (!stopped(e)) throw e; }
                return false;
            }
            try { const v = gen.throwIn(exc); if (v !== STOP) fail(E.RuntimeError, "generator didn't stop after throw()"); return true; }
            catch (e) {
                if (stopped(e)) return e !== exc;
                if (e === exc) return false;
                throw e;
            }
        }));
        fn(g, "contextmanager", 1, (a) => {
            const f = a[0];
            const w = builtin(f.name || "contextmanager", -1, (b) => {
                const kw = b.length && b[b.length - 1] instanceof Map ? b.pop() : null;
                const gen = call(f, b, kw);
                if (gen === null || typeof gen !== "object" || !isInstance(gen, T.generator)) fail(E.TypeError, "contextmanager function must be a generator");
                return { cls: GCM, dict: new Map(), gen: gen };
            });
            w.kwnames = true; w.dict = new Map([["__wrapped__", f]]);
            return w;
        });
        const Suppress = rt.newType("suppress", [rt.ObjectType], new Map(), "contextlib");
        Suppress.dict.set("__init__", builtin("__init__", -1, (a) => { a[0].kinds = tuple(a.slice(1)); return null; }));
        Suppress.dict.set("__enter__", builtin("__enter__", 1, () => null));
        Suppress.dict.set("__exit__", builtin("__exit__", 4, (a) => a[2] !== null && R.excmatch(a[2], a[0].kinds)));
        g.set("suppress", Suppress);
        const Closing = rt.newType("closing", [rt.ObjectType], new Map(), "contextlib");
        Closing.dict.set("__init__", builtin("__init__", 2, (a) => { a[0].thing = a[1]; return null; }));
        Closing.dict.set("__enter__", builtin("__enter__", 1, (a) => a[0].thing));
        Closing.dict.set("__exit__", builtin("__exit__", 4, (a) => { callMethod(a[0].thing, "close", []); return false; }));
        g.set("closing", Closing);
        const Null = rt.newType("nullcontext", [rt.ObjectType], new Map(), "contextlib");
        Null.dict.set("__init__", builtin("__init__", 2, (a) => { a[0].value = a[1] === undefined ? null : a[1]; return null; }, 1));
        Null.dict.set("__enter__", builtin("__enter__", 1, (a) => a[0].value));
        Null.dict.set("__exit__", builtin("__exit__", 4, () => false));
        g.set("nullcontext", Null);
        // ExitStack: exits run in reverse; a truthy __exit__ swallows the exception.
        const Stack = rt.newType("ExitStack", [rt.ObjectType], new Map(), "contextlib");
        Stack.dict.set("__init__", builtin("__init__", 1, (a) => { a[0].exits = []; return null; }));
        Stack.dict.set("__enter__", builtin("__enter__", 1, (a) => a[0]));
        Stack.dict.set("enter_context", builtin("enter_context", 2, (a) => { const [exit, value] = R.withenter(a[1]); a[0].exits.push(exit); return value; }));
        Stack.dict.set("push", builtin("push", 2, (a) => { const cm = a[1]; const t = rt.typeOf(cm); const ex = rt.lookupType(t, "__exit__"); a[0].exits.push(ex !== undefined ? rt.descrGet(ex, cm, t) : cm); return cm; }));
        Stack.dict.set("callback", builtin("callback", -1, (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; const f = a[1], rest = a.slice(2); a[0].exits.push(builtin("callback", 3, () => { call(f, rest, kw); return false; })); return f; }));
        Stack.dict.get("callback").kwnames = true;
        const unwind = (self, exc) => {
            const exits = self.exits; self.exits = [];
            let current = exc;
            while (exits.length) {
                const exit = exits.pop();
                try { if (rt.truth(call(exit, [current === null ? null : rt.typeOf(current), current, null], null))) current = null; }
                catch (e) { current = rt.normexc(e); }
            }
            return current;
        };
        Stack.dict.set("__exit__", builtin("__exit__", 4, (a) => { const left = unwind(a[0], a[2]); if (left === null) return true; if (left === a[2]) return false; throw left; }));
        Stack.dict.set("close", builtin("close", 1, (a) => { const left = unwind(a[0], null); if (left !== null) throw left; return null; }));
        Stack.dict.set("pop_all", builtin("pop_all", 1, (a) => { const s = { cls: Stack, dict: new Map(), exits: a[0].exits }; a[0].exits = []; return s; }));
        g.set("ExitStack", Stack);
    });
    mod("functools", (g) => {
        fn(g, "reduce", 3, (a) => { const it = iter(a[1]); let acc; if (a.length === 3) acc = a[2]; else { acc = fornext(it); if (acc === STOP) fail(E.TypeError, "reduce() of empty iterable with no initial value"); } for (;;) { const v = fornext(it); if (v === STOP) return acc; acc = call(a[0], [acc, v], null); } }, 2);
        const Partial = pyClass("partial", "functools", {
            __init__: (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; a[0].dict.set("func", a[1]); a[0].dict.set("args", tuple(a.slice(2))); a[0].dict.set("keywords", kw === null ? dict() : rt.dictFromMap(kw)); return null; },
            __call__: (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; const self = a[0]; const merged = new Map(); for (const [k, v] of dictEntries(self.dict.get("keywords"))) merged.set(k, v); if (kw) for (const [k, v] of kw) merged.set(k, v); return call(self.dict.get("func"), self.dict.get("args").items.concat(a.slice(1)), merged.size ? merged : null); },
            __repr__: (a) => "functools.partial(" + repr(a[0].dict.get("func")) + ")",
        });
        Partial.dict.get("__init__").kwnames = true; Partial.dict.get("__call__").kwnames = true;
        g.set("partial", Partial);
        fn(g, "wraps", -1, (a) => builtin("wraps", 1, (b) => { const f = b[0], src = a[0]; if (f !== null && typeof f === "object" && (f.cls === T.function || f.cls === T.builtin_function_or_method)) { const pick = (n, dflt) => { try { return rt.getattr(src, n); } catch (e) { return dflt; } }; f.name = pick("__name__", f.name); f.qualname = pick("__qualname__", f.qualname); f.doc = pick("__doc__", null); if (f.dict) f.dict.set("__wrapped__", src); } return f; }));
        fn(g, "update_wrapper", 2, (a) => a[0]);
        const CacheInfo = rt.newType("CacheInfo", [T.tuple], new Map(), "functools");
        CacheInfo.dict.set("__repr__", builtin("__repr__", 1, (a) => { const [h, m, mx, c] = a[0].items; return "CacheInfo(hits=" + h + ", misses=" + m + ", maxsize=" + repr(mx) + ", currsize=" + c + ")"; }));
        for (const [i, n] of ["hits", "misses", "maxsize", "currsize"].entries()) CacheInfo.dict.set(n, { cls: T.property, fget: builtin(n, 1, (a) => a[0].items[i]), fset: null, fdel: null, doc: null });
        const cache = (f, maxsize) => {
            const memo = new Map(); let hits = 0n, misses = 0n;
            const w = builtin(f.name || "cached", -1, (b) => {
                const kw = b[b.length - 1] instanceof Map ? b.pop() : null;
                const key = rt.keyStr(tuple(b)) + (kw ? "|" + [...kw].map(([k, v]) => k + "=" + rt.keyStr(v)).join(",") : "");
                if (memo.has(key)) { hits++; const v = memo.get(key); memo.delete(key); memo.set(key, v); return v; }
                misses++;
                const v = call(f, b, kw); memo.set(key, v);
                if (maxsize !== null && memo.size > Number(maxsize)) memo.delete(memo.keys().next().value);
                return v;
            });
            w.kwnames = true;
            w.dict = new Map([
                ["cache_clear", builtin("cache_clear", 0, () => { memo.clear(); hits = 0n; misses = 0n; return null; })],
                ["cache_info", builtin("cache_info", 0, () => ({ cls: CacheInfo, items: [hits, misses, maxsize, BigInt(memo.size)], dict: new Map() }))],
                ["__wrapped__", f]]);
            return w;
        };
        fn(g, "lru_cache", -1, (a) => { const kw = kwOf(a, ["maxsize", "typed"]); if (a.length === 1 && a[0] !== null && typeof a[0] === "object" && (a[0].cls === T.function || a[0].cls === T.builtin_function_or_method)) return cache(a[0], 128n); let maxsize = a.length ? a[0] : kwget(kw, "maxsize", 128n); if (maxsize !== null) maxsize = needInt(maxsize); return builtin("lru_cache", 1, (b) => cache(b[0], maxsize)); });
        g.get("lru_cache").kwnames = true;
        fn(g, "cache", 1, (a) => cache(a[0], null));
        fn(g, "cached_property", 1, (a) => { const f = a[0]; return { cls: T.property, fget: builtin("cached", 1, (b) => { const key = "__cached_" + f.name; const o = b[0]; if (o.dict.has(key)) return o.dict.get(key); const v = call(f, [o], null); o.dict.set(key, v); return v; }), fset: null, fdel: null, doc: f.doc }; });
        fn(g, "total_ordering", 1, (a) => { const c = a[0]; rt.bumpEpoch(); const has = (n) => c.dict.has(n); const lt = (x, y) => truth(call(rt.getattr(x, "__lt__"), [y], null)); if (has("__lt__")) { if (!has("__gt__")) c.dict.set("__gt__", builtin("__gt__", 2, (b) => !lt(b[0], b[1]) && !eq(b[0], b[1]))); if (!has("__le__")) c.dict.set("__le__", builtin("__le__", 2, (b) => lt(b[0], b[1]) || eq(b[0], b[1]))); if (!has("__ge__")) c.dict.set("__ge__", builtin("__ge__", 2, (b) => !lt(b[0], b[1]))); } return c; });
        fn(g, "cmp_to_key", 1, (a) => { const K = pyClass("K", "functools", { __init__: (b) => { b[0].dict.set("obj", b[1]); return null; }, __lt__: (b) => asInt(call(a[0], [b[0].dict.get("obj"), b[1].dict.get("obj")], null)) < 0n, __eq__: (b) => asInt(call(a[0], [b[0].dict.get("obj"), b[1].dict.get("obj")], null)) === 0n }); return K; });
        fn(g, "singledispatch", 1, (a) => a[0]);
    });
    mod("collections", (g) => {
        const DefaultDict = rt.newType("defaultdict", [T.dict], new Map(), "collections");
        DefaultDict.dict.set("__init__", (() => { const f = builtin("__init__", -1, (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; a[0].dict.set("default_factory", a[1] === undefined ? null : a[1]); if (a[2] !== undefined) for (const [k, v] of dictEntries(rt.asDict(a[2]))) dictSet(a[0], k, v); if (kw) for (const [k, v] of kw) dictSet(a[0], k, v); return null; }); f.kwnames = true; return f; })());
        DefaultDict.dict.set("__missing__", builtin("__missing__", 2, (a) => { const f = a[0].dict.get("default_factory"); if (f === null || f === undefined) throw rt.makeExc(E.KeyError, [a[1]]); const v = call(f, [], null); dictSet(a[0], a[1], v); return v; }));
        DefaultDict.dict.set("__repr__", builtin("__repr__", 1, (a) => "defaultdict(" + repr(a[0].dict.get("default_factory") === undefined ? null : a[0].dict.get("default_factory")) + ", " + rt.baseRepr(a[0]) + ")"));
        DefaultDict.dict.set("default_factory", { cls: T.property, fget: builtin("default_factory", 1, (a) => { const f = a[0].dict.get("default_factory"); return f === undefined ? null : f; }), fset: null, fdel: null, doc: null });
        g.set("defaultdict", DefaultDict);
        const OrderedDict = rt.newType("OrderedDict", [T.dict], new Map(), "collections");
        OrderedDict.dict.set("move_to_end", builtin("move_to_end", 3, (a) => { const v = dictGet(a[0], a[1]); if (v === undefined) throw rt.makeExc(E.KeyError, [a[1]]); rt.dictDel(a[0], a[1]); if (a[2] === undefined || truth(a[2])) dictSet(a[0], a[1], v); else { const entries = rt.dictEntryList(a[0]); a[0].map.clear(); a[0].size = 0; dictSet(a[0], a[1], v); for (const [k, x] of entries) dictSet(a[0], k, x); } return null; }, 2));
        OrderedDict.dict.set("__repr__", builtin("__repr__", 1, (a) => a[0].size ? "OrderedDict(" + rt.baseRepr(a[0]) + ")" : "OrderedDict()"));
        OrderedDict.dict.set("popitem", (() => { const f = builtin("popitem", -1, (a) => { const kw = kwOf(a, ["last"]); const last = a[1] !== undefined ? truth(a[1]) : truth(kwget(kw, "last", true)); const entries = rt.dictEntryList(a[0]); if (!entries.length) fail(E.KeyError, "dictionary is empty"); const [k, v] = entries[last ? entries.length - 1 : 0]; rt.dictDel(a[0], k); return tuple([k, v]); }); f.kwnames = true; return f; })());
        g.set("OrderedDict", OrderedDict);
        const Counter = rt.newType("Counter", [T.dict], new Map(), "collections");
        Counter.dict.set("__init__", (() => { const f = builtin("__init__", -1, (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; if (a[1] !== undefined && a[1] !== null) { if (a[1].cls === T.dict || isInstance(a[1], T.dict)) for (const [k, v] of dictEntries(a[1])) dictSet(a[0], k, v); else for (const x of drain(a[1])) { const c = dictGet(a[0], x); dictSet(a[0], x, c === undefined ? 1n : R.binop("add", c, 1n)); } } if (kw) for (const [k, v] of kw) dictSet(a[0], k, v); return null; }); f.kwnames = true; return f; })());
        Counter.dict.set("__missing__", builtin("__missing__", 2, () => 0n));
        Counter.dict.set("most_common", builtin("most_common", 2, (a) => { const items = rt.dictEntryList(a[0]).map((e) => tuple(e)); const sorted = rt.sortItems(items, builtin("k", 1, (b) => b[0].items[1]), true); return list(a[1] === undefined || a[1] === null ? sorted : sorted.slice(0, Number(needInt(a[1])))); }, 1));
        Counter.dict.set("elements", builtin("elements", 1, (a) => { const out = []; for (const [k, v] of dictEntries(a[0])) for (let i = 0n; i < asInt(v); i++) out.push(k); return iter(list(out)); }));
        Counter.dict.set("update", (() => { const f = builtin("update", -1, (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; if (a[1] !== undefined) { if (a[1].cls === T.dict || isInstance(a[1], T.dict)) for (const [k, v] of dictEntries(a[1])) dictSet(a[0], k, R.binop("add", dictGet(a[0], k) || 0n, v)); else for (const x of drain(a[1])) dictSet(a[0], x, R.binop("add", dictGet(a[0], x) || 0n, 1n)); } if (kw) for (const [k, v] of kw) dictSet(a[0], k, R.binop("add", dictGet(a[0], k) || 0n, v)); return null; }); f.kwnames = true; return f; })());
        Counter.dict.set("subtract", builtin("subtract", 2, (a) => { const src = a[1].cls === T.dict || isInstance(a[1], T.dict) ? rt.dictEntryList(a[1]) : drain(a[1]).map((x) => [x, 1n]); for (const [k, v] of src) dictSet(a[0], k, R.binop("sub", dictGet(a[0], k) || 0n, v)); return null; }));
        Counter.dict.set("total", builtin("total", 1, (a) => { let t = 0n; for (const [, v] of dictEntries(a[0])) t = R.binop("add", t, v); return t; }));
        Counter.dict.set("__repr__", builtin("__repr__", 1, (a) => { if (!a[0].size) return "Counter()"; const items = rt.sortItems(rt.dictEntryList(a[0]).map((e) => tuple(e)), builtin("k", 1, (b) => b[0].items[1]), true); return "Counter({" + items.map((t) => repr(t.items[0]) + ": " + repr(t.items[1])).join(", ") + "})"; }));
        Counter.dict.set("__pos__", builtin("__pos__", 1, (a) => { const out = rt.construct(Counter, [], null); for (const [k, v] of dictEntries(a[0])) if (cmp("gt", v, 0n)) dictSet(out, k, v); return out; }));
        Counter.dict.set("__neg__", builtin("__neg__", 1, (a) => { const out = rt.construct(Counter, [], null); for (const [k, v] of dictEntries(a[0])) if (cmp("lt", v, 0n)) dictSet(out, k, R.unop("neg", v)); return out; }));
        for (const [name, op] of [["__add__", "add"], ["__sub__", "sub"], ["__or__", "or"], ["__and__", "and"]]) Counter.dict.set(name, builtin(name, 2, (a) => { const out = rt.construct(Counter, [], null); const keys = new Set(); const all = []; for (const [k] of dictEntries(a[0])) { const kk = rt.keyOf(k); if (!keys.has(kk)) { keys.add(kk); all.push(k); } } for (const [k] of dictEntries(a[1])) { const kk = rt.keyOf(k); if (!keys.has(kk)) { keys.add(kk); all.push(k); } } for (const k of all) { const x = dictGet(a[0], k) || 0n, y = dictGet(a[1], k) || 0n; let v; if (op === "add") v = R.binop("add", x, y); else if (op === "sub") v = R.binop("sub", x, y); else if (op === "or") v = cmp("gt", x, y) ? x : y; else v = cmp("lt", x, y) ? x : y; if (cmp("gt", v, 0n)) dictSet(out, k, v); } return out; }));
        g.set("Counter", Counter);
        const Deque = pyClass("deque", "collections", {
            __init__: (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; a[0].items = a[1] === undefined || a[1] === null ? [] : drain(a[1]); const ml = a[2] !== undefined ? a[2] : kwget(kw, "maxlen", null); a[0].maxlen = ml === null ? null : Number(needInt(ml)); if (a[0].maxlen !== null) while (a[0].items.length > a[0].maxlen) a[0].items.shift(); return null; },
            append: (a) => { a[0].items.push(a[1]); if (a[0].maxlen !== null && a[0].items.length > a[0].maxlen) a[0].items.shift(); return null; },
            appendleft: (a) => { a[0].items.unshift(a[1]); if (a[0].maxlen !== null && a[0].items.length > a[0].maxlen) a[0].items.pop(); return null; },
            pop: (a) => { if (!a[0].items.length) fail(E.IndexError, "pop from an empty deque"); return a[0].items.pop(); },
            popleft: (a) => { if (!a[0].items.length) fail(E.IndexError, "pop from an empty deque"); return a[0].items.shift(); },
            extend: (a) => { for (const x of drain(a[1])) { a[0].items.push(x); if (a[0].maxlen !== null && a[0].items.length > a[0].maxlen) a[0].items.shift(); } return null; },
            extendleft: (a) => { for (const x of drain(a[1])) { a[0].items.unshift(x); if (a[0].maxlen !== null && a[0].items.length > a[0].maxlen) a[0].items.pop(); } return null; },
            clear: (a) => { a[0].items.length = 0; return null; },
            rotate: (a) => { const n = a[1] === undefined ? 1 : Number(needInt(a[1])); const it = a[0].items; if (!it.length) return null; const k = ((n % it.length) + it.length) % it.length; a[0].items = it.slice(it.length - k).concat(it.slice(0, it.length - k)); return null; },
            __len__: (a) => BigInt(a[0].items.length),
            __iter__: (a) => iter(list(a[0].items)),
            __getitem__: (a) => getitem(list(a[0].items), a[1]),
            __setitem__: (a) => { a[0].items[Number(needInt(a[1]))] = a[2]; return null; },
            __contains__: (a) => rt.contains(list(a[0].items), a[1]),
            __bool__: (a) => a[0].items.length > 0,
            __repr__: (a) => "deque(" + repr(list(a[0].items)) + (a[0].maxlen !== null ? ", maxlen=" + a[0].maxlen : "") + ")",
            __eq__: (a) => a[1] !== null && typeof a[1] === "object" && a[1].items !== undefined ? eq(list(a[0].items), list(a[1].items)) : NOTIMPL,
            count: (a) => BigInt(rt.acount(a[0].items, (x) => eq(x, a[1]))),
            index: (a) => { const i = rt.aindex(a[0].items, (x) => eq(x, a[1])); if (i < 0) fail(E.ValueError, "not in deque"); return BigInt(i); },
            remove: (a) => { const i = rt.aindex(a[0].items, (x) => eq(x, a[1])); if (i < 0) fail(E.ValueError, "deque.remove(x): x not in deque"); a[0].items.splice(i, 1); return null; },
            reverse: (a) => { a[0].items.reverse(); return null; },
            copy: (a) => { const o = rt.construct(a[0].cls, [list(a[0].items)], null); o.maxlen = a[0].maxlen; return o; },
        });
        Deque.dict.get("__init__").kwnames = true;
        rt.allocators.set(Deque, (cls) => ({ cls: cls, items: [], maxlen: null }));
        Deque.dict.set("maxlen", { cls: T.property, fget: builtin("maxlen", 1, (a) => a[0].maxlen === null ? null : BigInt(a[0].maxlen)), fset: null, fdel: null, doc: null });
        g.set("deque", Deque);
        fnkw(g, "namedtuple", (a) => {
            const kw = kwOf(a, ["defaults", "rename", "module"]);
            const name = needStr(a[0]);
            let fields = typeof a[1] === "string" ? a[1].split(/[,\s]+/).filter((x) => x) : drain(a[1]).map((x) => needStr(x));
            const defaults = drain(kwget(kw, "defaults", list([])));
            const cls = rt.newType(name, [T.tuple], new Map(), "main");
            cls.dict.set("_fields", tuple(fields.slice()));
            const init = builtin("__new__", -1, (b) => { const kwm = b[b.length - 1] instanceof Map ? b.pop() : null; const items = b.slice(1); if (kwm) for (const [k, v] of kwm) { const i = fields.indexOf(k); if (i < 0) fail(E.TypeError, "unexpected keyword " + k); items[i] = v; } for (let i = items.length; i < fields.length; i++) { const d = defaults[i - (fields.length - defaults.length)]; if (d === undefined) fail(E.TypeError, name + " missing required argument: '" + fields[i] + "'"); items[i] = d; } if (items.length > fields.length) fail(E.TypeError, name + " takes " + fields.length + " positional arguments but " + items.length + " were given"); const o = { cls: b[0], items: items, dict: new Map() }; return o; });
            init.kwnames = true;
            cls.dict.set("__new__", { cls: T.staticmethod, func: init });
            cls.dict.set("__init__", builtin("__init__", -1, () => null));
            cls.dict.get("__init__").kwnames = true;
            fields.forEach((f, i) => cls.dict.set(f, { cls: T.property, fget: builtin(f, 1, (b) => b[0].items[i]), fset: null, fdel: null, doc: null }));
            cls.dict.set("__repr__", builtin("__repr__", 1, (b) => name + "(" + rt.amap(fields, (f, i) => f + "=" + repr(b[0].items[i])).join(", ") + ")"));
            cls.dict.set("_asdict", builtin("_asdict", 1, (b) => { const d = dict(); fields.forEach((f, i) => dictSet(d, f, b[0].items[i])); return d; }));
            cls.dict.set("_replace", (() => { const f = builtin("_replace", -1, (b) => { const kwm = b[b.length - 1] instanceof Map ? b.pop() : null; const items = b[0].items.slice(); if (kwm) for (const [k, v] of kwm) items[fields.indexOf(k)] = v; return { cls: b[0].cls, items: items, dict: new Map() }; }); f.kwnames = true; return f; })());
            cls.dict.set("_make", { cls: T.classmethod, func: builtin("_make", 2, (b) => ({ cls: b[0], items: drain(b[1]), dict: new Map() })) });
            rt.bumpEpoch();
            return cls;
        });
        const chainKeys = (self) => { const seen = new Set(), out = []; const maps = self.dict.get("maps").items; for (let i = maps.length - 1; i >= 0; i--) for (const [k] of dictEntries(rt.asDict(maps[i]))) { const kk = rt.keyOf(k); if (!seen.has(kk)) { seen.add(kk); out.push(k); } } return out; };
        const ChainMap = pyClass("ChainMap", "collections", { __init__: (a) => { a[0].dict.set("maps", list(a.length > 1 ? a.slice(1) : [dict()])); return null; },
            __iter__: (a) => iter(list(chainKeys(a[0]))), __len__: (a) => BigInt(chainKeys(a[0]).length), keys: (a) => list(chainKeys(a[0])),
            values: (a) => list(chainKeys(a[0]).map((k) => getitem(a[0], k))), items: (a) => list(chainKeys(a[0]).map((k) => tuple([k, getitem(a[0], k)]))),
            __setitem__: (a) => { rt.setitem(a[0].dict.get("maps").items[0], a[1], a[2]); return null; },
            __delitem__: (a) => { rt.delitem(a[0].dict.get("maps").items[0], a[1]); return null; },
            new_child: (a) => rt.construct(a[0].cls, [a[1] === undefined ? dict() : a[1]].concat(a[0].dict.get("maps").items), null),
            __repr__: (a) => "ChainMap(" + rt.amap(a[0].dict.get("maps").items, repr).join(", ") + ")", __getitem__: (a) => { for (const m of a[0].dict.get("maps").items) { const v = dictGet(m, a[1]); if (v !== undefined) return v; } throw rt.makeExc(E.KeyError, [a[1]]); }, get: (a) => { for (const m of a[0].dict.get("maps").items) { const v = dictGet(m, a[1]); if (v !== undefined) return v; } return a[2] === undefined ? null : a[2]; }, __contains__: (a) => a[0].dict.get("maps").items.some((m) => rt.dictHas(m, a[1])) });
        g.set("ChainMap", ChainMap);
        g.set("abc", rt.newModule("collections.abc", null));
        for (const n of ["Iterable", "Iterator", "Mapping", "MutableMapping", "Sequence", "MutableSequence", "Set", "MutableSet", "Callable", "Hashable", "Sized", "Container", "Collection", "Generator"]) g.get("abc").globals.set(n, rt.newType(n, [], new Map(), "collections.abc"));
        rt.modules.set("collections.abc", g.get("abc"));
    });
    mod("heapq", (g) => {
        const lt = (a, b) => cmp("lt", a, b);
        const up = (h, i) => { while (i > 0) { const p = (i - 1) >> 1; if (lt(h[i], h[p])) { [h[i], h[p]] = [h[p], h[i]]; i = p; } else break; } };
        const down = (h, i) => { const n = h.length; for (;;) { let m = i; const l = 2 * i + 1, r = l + 1; if (l < n && lt(h[l], h[m])) m = l; if (r < n && lt(h[r], h[m])) m = r; if (m === i) break; [h[i], h[m]] = [h[m], h[i]]; i = m; } };
        fn(g, "heappush", 2, (a) => { a[0].items.push(a[1]); up(a[0].items, a[0].items.length - 1); return null; });
        fn(g, "heappop", 1, (a) => { const h = a[0].items; if (!h.length) fail(E.IndexError, "index out of range"); const top = h[0]; const last = h.pop(); if (h.length) { h[0] = last; down(h, 0); } return top; });
        fn(g, "heapify", 1, (a) => { const h = a[0].items; for (let i = (h.length >> 1) - 1; i >= 0; i--) down(h, i); return null; });
        fn(g, "heappushpop", 2, (a) => { const h = a[0].items; if (h.length && lt(h[0], a[1])) { const top = h[0]; h[0] = a[1]; down(h, 0); return top; } return a[1]; });
        fn(g, "heapreplace", 2, (a) => { const h = a[0].items; const top = h[0]; h[0] = a[1]; down(h, 0); return top; });
        fnkw(g, "nlargest", (a) => { const kw = kwOf(a, ["key"]); return list(rt.sortItems(drain(a[1]), kwget(kw, "key", null), true).slice(0, Number(needInt(a[0])))); });
        fnkw(g, "nsmallest", (a) => { const kw = kwOf(a, ["key"]); return list(rt.sortItems(drain(a[1]), kwget(kw, "key", null), false).slice(0, Number(needInt(a[0])))); });
    });
    mod("bisect", (g) => {
        const bis = (a, x, right, key) => { const items = a.items; let lo = 0, hi = items.length; while (lo < hi) { const mid = (lo + hi) >> 1; const v = key === null ? items[mid] : call(key, [items[mid]], null); if (right ? !cmp("lt", x, v) : cmp("lt", v, x)) lo = mid + 1; else hi = mid; } return lo; };
        fnkw(g, "bisect_left", (a) => { const kw = kwOf(a, ["lo", "hi", "key"]); return BigInt(bis(a[0], a[1], false, kwget(kw, "key", null))); });
        fnkw(g, "bisect_right", (a) => { const kw = kwOf(a, ["lo", "hi", "key"]); return BigInt(bis(a[0], a[1], true, kwget(kw, "key", null))); });
        g.set("bisect", g.get("bisect_right"));
        fnkw(g, "insort_left", (a) => { const kw = kwOf(a, ["lo", "hi", "key"]); a[0].items.splice(bis(a[0], a[1], false, kwget(kw, "key", null)), 0, a[1]); return null; });
        fnkw(g, "insort_right", (a) => { const kw = kwOf(a, ["lo", "hi", "key"]); a[0].items.splice(bis(a[0], a[1], true, kwget(kw, "key", null)), 0, a[1]); return null; });
        g.set("insort", g.get("insort_right"));
    });
    mod("statistics", (g) => {
        const nums = (v) => { const items = drain(v); if (!items.length) fail(E.ValueError, "statistics requires at least one data point"); return items; };
        fn(g, "mean", 1, (a) => { const items = nums(a[0]); let s = 0n; let isF = false; for (const x of items) { if (!isInt(x)) isF = true; s = R.binop("add", s, x); } return isF || typeof s === "number" ? toFloat(s) / items.length : (asInt(s) % BigInt(items.length) === 0n ? asInt(s) / BigInt(items.length) : Number(asInt(s)) / items.length); });
        fn(g, "fmean", 1, (a) => { const items = nums(a[0]).map(toFloat); return items.reduce((x, y) => x + y, 0) / items.length; });
        fn(g, "median", 1, (a) => { const items = rt.sortItems(nums(a[0]), null, false); const n = items.length; if (n % 2) return items[(n - 1) / 2]; return R.binop("truediv", R.binop("add", items[n / 2 - 1], items[n / 2]), 2n); });
        fn(g, "mode", 1, (a) => { const items = nums(a[0]); const counts = dict(); let best = items[0], bc = 0n; for (const x of items) { const c = (dictGet(counts, x) || 0n) + 1n; dictSet(counts, x, c); if (c > bc) { bc = c; best = x; } } return best; });
        const variance = (items, ddof) => { const xs = items.map(toFloat); const m = xs.reduce((x, y) => x + y, 0) / xs.length; return xs.reduce((s, x) => s + (x - m) * (x - m), 0) / (xs.length - ddof); };
        fn(g, "variance", 1, (a) => { const items = nums(a[0]); if (items.length < 2) fail(E.StatisticsError || E.ValueError, "variance requires at least two data points"); return variance(items, 1); });
        fn(g, "pvariance", 1, (a) => variance(nums(a[0]), 0));
        fn(g, "stdev", 1, (a) => { const items = nums(a[0]); if (items.length < 2) fail(E.ValueError, "stdev requires at least two data points"); return Math.sqrt(variance(items, 1)); });
        fn(g, "pstdev", 1, (a) => Math.sqrt(variance(nums(a[0]), 0)));
    });

    // ---- re: a JavaScript-backed subset ---------------------------------------------------------------------------------------------------------
    mod("re", (g) => {
        const FLAGS = { IGNORECASE: 2n, I: 2n, MULTILINE: 8n, M: 8n, DOTALL: 16n, S: 16n, VERBOSE: 64n, X: 64n, ASCII: 256n, A: 256n, UNICODE: 32n, U: 32n };
        for (const [k, v] of Object.entries(FLAGS)) g.set(k, v);
        function translate(pattern, flags) {
            let p = pattern;
            if (flags & 64n) p = p.replace(/\\#/g, " ").replace(/#.*$/gm, "").replace(/\s+/g, "").replace(/ /g, "\\#");
            p = p.replace(/\(\?P<([A-Za-z_]\w*)>/g, "(?<$1>").replace(/\(\?P=([A-Za-z_]\w*)\)/g, "\\k<$1>").replace(/\\Z/g, "$(?![\\s\\S])").replace(/\\A/g, "^");
            let js = "";
            if (flags & 2n) js += "i"; if (flags & 8n) js += "m"; if (flags & 16n) js += "s";
            try { return new RegExp(p, js + "gu"); } catch (e) { try { return new RegExp(p, js + "g"); } catch (e2) { fail(E.ValueError || E.RuntimeError, "bad pattern: " + e2.message); } }
        }
        const Match = pyClass("Match", "re", {
            group: (a) => { const m = a[0].m; if (a.length === 1) return m[0]; const pick = (k) => { const v = typeof k === "string" ? (m.groups || {})[k] : m[Number(needInt(k))]; return v === undefined ? null : v; }; return a.length === 2 ? pick(a[1]) : tuple(a.slice(1).map(pick)); },
            groups: (a) => tuple(a[0].m.slice(1).map((x) => x === undefined ? (a[1] === undefined ? null : a[1]) : x)),
            groupdict: (a) => { const d = dict(); for (const [k, v] of Object.entries(a[0].m.groups || {})) dictSet(d, k, v === undefined ? null : v); return d; },
            start: (a) => BigInt(a[0].m.index + (a[1] === undefined ? 0 : a[0].m[0].indexOf(a[0].m[Number(needInt(a[1]))]))),
            end: (a) => { const m = a[0].m; if (a[1] === undefined) return BigInt(m.index + m[0].length); const gi = Number(needInt(a[1])); return BigInt(m.index + m[0].indexOf(m[gi]) + m[gi].length); },
            span: (a) => { const m = a[0].m; if (a[1] === undefined) return tuple([BigInt(m.index), BigInt(m.index + m[0].length)]); const gi = Number(needInt(a[1])); const s = m.index + m[0].indexOf(m[gi]); return tuple([BigInt(s), BigInt(s + m[gi].length)]); },
            __getitem__: (a) => { const v = typeof a[1] === "string" ? (a[0].m.groups || {})[a[1]] : a[0].m[Number(needInt(a[1]))]; return v === undefined ? null : v; },
            __bool__: () => true,
            __repr__: (a) => "<re.Match object; span=(" + a[0].m.index + ", " + (a[0].m.index + a[0].m[0].length) + "), match=" + repr(a[0].m[0]) + ">",
        });
        Match.dict.set("string", { cls: T.property, fget: builtin("string", 1, (a) => a[0].s), fset: null, fdel: null, doc: null });
        Match.dict.set("lastindex", { cls: T.property, fget: builtin("lastindex", 1, (a) => { let n = 0; for (let i = 1; i < a[0].m.length; i++) if (a[0].m[i] !== undefined) n = i; return n ? BigInt(n) : null; }), fset: null, fdel: null, doc: null });
        const mk = (m, s) => ({ cls: Match, dict: new Map(), m: m, s: s });
        function exec(re, s, pos) { re.lastIndex = pos || 0; const m = re.exec(s); return m; }
        const Pattern = pyClass("Pattern", "re", {
            search: (a) => { const m = exec(a[0].re, needStr(a[1]), a[2] === undefined ? 0 : Number(needInt(a[2]))); return m === null ? null : mk(m, a[1]); },
            match: (a) => { const m = exec(a[0].re, needStr(a[1]), a[2] === undefined ? 0 : Number(needInt(a[2]))); return m === null || m.index !== (a[2] === undefined ? 0 : Number(needInt(a[2]))) ? null : mk(m, a[1]); },
            fullmatch: (a) => { const s = needStr(a[1]); const m = exec(a[0].re, s, 0); return m === null || m.index !== 0 || m[0].length !== s.length ? null : mk(m, s); },
            findall: (a) => { const s = needStr(a[1]); const out = []; a[0].re.lastIndex = 0; let m; while ((m = a[0].re.exec(s)) !== null) { if (m[0] === "") a[0].re.lastIndex++; out.push(m.length === 1 ? m[0] : m.length === 2 ? (m[1] === undefined ? "" : m[1]) : tuple(m.slice(1).map((x) => x === undefined ? "" : x))); } return list(out); },
            finditer: (a) => { const s = needStr(a[1]); const out = []; a[0].re.lastIndex = 0; let m; while ((m = a[0].re.exec(s)) !== null) { if (m[0] === "") a[0].re.lastIndex++; out.push(mk(m, s)); } return iter(list(out)); },
            sub: (a) => subImpl(a[0].re, a[1], needStr(a[2]), a[3] === undefined ? 0 : Number(needInt(a[3])), false),
            subn: (a) => subImpl(a[0].re, a[1], needStr(a[2]), a[3] === undefined ? 0 : Number(needInt(a[3])), true),
            split: (a) => { const s = needStr(a[1]); const max = a[2] === undefined ? 0 : Number(needInt(a[2])); const out = []; let last = 0, n = 0; a[0].re.lastIndex = 0; let m; while ((m = a[0].re.exec(s)) !== null) { if (m[0] === "") { a[0].re.lastIndex++; if (m.index >= s.length) break; if (m.index === last && n === 0 && m.index === 0) continue; } out.push(s.slice(last, m.index)); for (let i = 1; i < m.length; i++) out.push(m[i] === undefined ? null : m[i]); last = m.index + m[0].length; n++; if (max && n >= max) break; } out.push(s.slice(last)); return list(out); },
            __repr__: (a) => "re.compile(" + repr(a[0].dict.get("pattern")) + ")",
        });
        Pattern.dict.set("pattern", { cls: T.property, fget: builtin("pattern", 1, (a) => a[0].src), fset: null, fdel: null, doc: null });
        Pattern.dict.set("groups", { cls: T.property, fget: builtin("groups", 1, (a) => BigInt(new RegExp(a[0].re.source + "|").exec("").length - 1)), fset: null, fdel: null, doc: null });
        function subImpl(re, repl, s, count, withCount) {
            let n = 0; re.lastIndex = 0; let out = "", last = 0, m;
            while ((m = re.exec(s)) !== null) {
                if (count && n >= count) break;
                out += s.slice(last, m.index);
                if (typeof repl === "string") out += repl.replace(/\\(\d+)|\\g<([^>]+)>|\\n|\\t|\\\\/g, (x, d, name) => { if (d !== undefined) return m[Number(d)] === undefined ? "" : m[Number(d)]; if (name !== undefined) return /^\d+$/.test(name) ? (m[Number(name)] || "") : ((m.groups || {})[name] || ""); return x === "\\n" ? "\n" : x === "\\t" ? "\t" : "\\"; });
                else out += needStr(call(repl, [mk(m, s)], null));
                last = m.index + m[0].length; n++;
                if (m[0] === "") { if (m.index < s.length) out += s[m.index]; last = m.index + 1; re.lastIndex = m.index + 1; if (m.index >= s.length) break; }
            }
            out += s.slice(last);
            return withCount ? tuple([out, BigInt(n)]) : out;
        }
        const cache = new Map();
        function compile(pattern, flags) {
            if (pattern !== null && typeof pattern === "object" && pattern.cls === Pattern) return pattern;
            const key = pattern + " " + flags;
            if (cache.has(key)) return cache.get(key);
            const p = { cls: Pattern, dict: new Map([["pattern", pattern], ["flags", flags]]), re: translate(needStr(pattern), flags), src: pattern };
            if (cache.size > 512) cache.clear();
            cache.set(key, p); return p;
        }
        fn(g, "compile", 2, (a) => compile(a[0], a[1] === undefined ? 0n : asInt(a[1])), 1);
        for (const name of ["search", "match", "fullmatch", "findall", "finditer", "split"]) fn(g, name, 3, (a) => call(Pattern.dict.get(name), [compile(a[0], a[2] === undefined ? 0n : asInt(a[2])), a[1]], null), 2);
        fnkw(g, "sub", (a) => { const kw = kwOf(a, ["count", "flags"]); const flags = a[4] !== undefined ? asInt(a[4]) : asInt(kwget(kw, "flags", 0n)); return subImpl(compile(a[0], flags).re, a[1], needStr(a[2]), a[3] !== undefined ? Number(needInt(a[3])) : Number(kwget(kw, "count", 0n)), false); });
        fnkw(g, "subn", (a) => { const kw = kwOf(a, ["count", "flags"]); const flags = a[4] !== undefined ? asInt(a[4]) : asInt(kwget(kw, "flags", 0n)); return subImpl(compile(a[0], flags).re, a[1], needStr(a[2]), a[3] !== undefined ? Number(needInt(a[3])) : Number(kwget(kw, "count", 0n)), true); });
        fn(g, "escape", 1, (a) => needStr(a[0]).replace(/[.*+?^${}()|[\]\\\-#&~\s]/g, (c) => "\\" + c));
        fn(g, "purge", 0, () => { cache.clear(); return null; });
        g.set("error", E.ValueError); g.set("Pattern", Pattern); g.set("Match", Match);
    });

    // ---- dataclasses / enum (the common subsets) --------------------------------------------------------------------------------------------------
    mod("dataclasses", (g) => {
        const MISSING = { cls: pyClass("_MISSING_TYPE", "dataclasses", {}), dict: new Map() };
        g.set("MISSING", MISSING);
        const FrozenInstanceError = rt.newType("FrozenInstanceError", [E.AttributeError], new Map(), "dataclasses");
        rt.allocators.set(FrozenInstanceError, (cls) => rt.makeExc(cls, []));
        g.set("FrozenInstanceError", FrozenInstanceError);
        const Field = pyClass("Field", "dataclasses", { __repr__: (a) => "Field(" + repr(a[0].dict.get("name")) + ")" });
        fnkw(g, "field", (a) => { const kw = kwOf(a, ["default", "default_factory", "init", "repr", "compare", "hash", "metadata", "kw_only"]); const f = { cls: Field, dict: new Map([["default", kwget(kw, "default", MISSING)], ["default_factory", kwget(kw, "default_factory", MISSING)], ["init", kwget(kw, "init", true)], ["repr", kwget(kw, "repr", true)], ["compare", kwget(kw, "compare", true)], ["name", null]]) }; return f; });
        function process(cls, kw) {
            const ann = cls.dict.get("__annotations__");
            const names = [];
            // Fields come from annotations when present, else from class attributes that are not callables.
            const annotated = ann && ann.cls === T.dict ? rt.dictEntryList(ann).map((e) => e[0]) : [];
            const fields = [];
            for (const c of cls.mro.slice().reverse()) { const fs = c.dict.get("__dataclass_fields__"); if (fs && c !== cls) for (const [n, f] of rt.dictEntryList(fs)) if (!fields.find((x) => x.name === n)) fields.push({ name: n, dflt: f.dict.get("default"), factory: f.dict.get("default_factory"), init: truth(f.dict.get("init")), repr: truth(f.dict.get("repr")), compare: truth(f.dict.get("compare")) }); }
            for (const n of annotated) {
                if (n === "__slots__") continue;
                const v = cls.dict.get(n);
                let entry = { name: n, dflt: MISSING, factory: MISSING, init: true, repr: true, compare: true };
                if (v !== undefined && v !== null && typeof v === "object" && v.cls === Field) { entry.dflt = v.dict.get("default"); entry.factory = v.dict.get("default_factory"); entry.init = truth(v.dict.get("init")); entry.repr = truth(v.dict.get("repr")); entry.compare = truth(v.dict.get("compare")); v.dict.set("name", n); cls.dict.delete(n); if (entry.dflt !== MISSING) cls.dict.set(n, entry.dflt); }
                else if (v !== undefined) entry.dflt = v;
                const i = fields.findIndex((x) => x.name === n); if (i >= 0) fields[i] = entry; else fields.push(entry);
            }
            const fieldMap = dict(); for (const f of fields) dictSet(fieldMap, f.name, { cls: Field, dict: new Map([["name", f.name], ["default", f.dflt], ["default_factory", f.factory], ["init", f.init], ["repr", f.repr], ["compare", f.compare]]) });
            cls.dict.set("__dataclass_fields__", fieldMap);
            const frozen = truth(kwget(kw, "frozen", false));
            if (truth(kwget(kw, "init", true)) && !cls.dict.has("__init__")) {
                const init = builtin("__init__", -1, (a) => {
                    const kwm = a[a.length - 1] instanceof Map ? a.pop() : null;
                    const self = a[0]; let pi = 1;
                    for (const f of fields) {
                        let v;
                        if (!f.init) v = f.factory !== MISSING ? call(f.factory, [], null) : f.dflt;
                        else if (kwm && kwm.has(f.name)) v = kwm.get(f.name);
                        else if (pi < a.length) v = a[pi++];
                        else if (f.factory !== MISSING) v = call(f.factory, [], null);
                        else if (f.dflt !== MISSING) v = f.dflt;
                        else fail(E.TypeError, cls.name + ".__init__() missing required argument: '" + f.name + "'");
                        self.dict.set(f.name, v);
                    }
                    if (pi < a.length) fail(E.TypeError, cls.name + ".__init__() takes " + (fields.filter((f) => f.init).length + 1) + " positional arguments but " + a.length + " were given");
                    const post = rt.lookupType(cls, "__post_init__"); if (post !== undefined) call(descrGet(post, self, cls), [], null);
                    return null;
                });
                init.kwnames = true; cls.dict.set("__init__", init);
            }
            if (truth(kwget(kw, "repr", true)) && !cls.dict.has("__repr__")) cls.dict.set("__repr__", builtin("__repr__", 1, (a) => cls.qualname + "(" + rt.amap(rt.afilter(fields, (f) => f.repr), (f) => f.name + "=" + repr(a[0].dict.get(f.name))).join(", ") + ")"));
            if (truth(kwget(kw, "eq", true)) && !cls.dict.has("__eq__")) cls.dict.set("__eq__", builtin("__eq__", 2, (a) => { if (typeOf(a[1]) !== typeOf(a[0])) return NOTIMPL; return rt.aevery(rt.afilter(fields, (f) => f.compare), (f) => eq(a[0].dict.get(f.name), a[1].dict.get(f.name))); }));
            if (truth(kwget(kw, "order", false))) for (const [n, op] of [["__lt__", "lt"], ["__le__", "le"], ["__gt__", "gt"], ["__ge__", "ge"]]) cls.dict.set(n, builtin(n, 2, (a) => { if (typeOf(a[1]) !== typeOf(a[0])) return NOTIMPL; return cmp(op, tuple(fields.map((f) => a[0].dict.get(f.name))), tuple(fields.map((f) => a[1].dict.get(f.name)))); }));
            if (frozen) { cls.dict.set("__setattr__", builtin("__setattr__", 3, (a) => { if (a[0].dict.has(a[1]) || fields.some((f) => f.name === a[1])) fail(FrozenInstanceError, "cannot assign to field '" + a[1] + "'"); a[0].dict.set(a[1], a[2]); return null; })); cls.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(tuple(fields.filter((f) => f.compare).map((f) => a[0].dict.get(f.name)))))); }
            else if (truth(kwget(kw, "eq", true)) && !cls.dict.has("__hash__")) cls.dict.set("__hash__", null);
            cls.dict.set("__match_args__", tuple(fields.map((f) => f.name)));
            rt.bumpEpoch();
            return cls;
        }
        fnkw(g, "dataclass", (a) => { const kw = kwOf(a, ["init", "repr", "eq", "order", "frozen", "unsafe_hash", "slots", "kw_only"]); if (a.length === 1 && isType(a[0])) return process(a[0], kw); return builtin("dataclass", 1, (b) => process(b[0], kw)); });
        fn(g, "fields", 1, (a) => { const c = isType(a[0]) ? a[0] : typeOf(a[0]); return tuple(rt.dictEntryList(c.dict.get("__dataclass_fields__") || dict()).map((e) => e[1])); });
        fnkw(g, "asdict", (a) => { const conv = (v) => { if (v !== null && typeof v === "object" && v.dict && typeOf(v).dict.has("__dataclass_fields__")) { const d = dict(); for (const [n] of rt.dictEntryList(typeOf(v).dict.get("__dataclass_fields__"))) dictSet(d, n, conv(v.dict.get(n))); return d; } if (v !== null && typeof v === "object" && (v.cls === T.list || v.cls === T.tuple)) return rt.sequence(v.cls, v.items.map(conv)); if (v !== null && typeof v === "object" && v.cls === T.dict) { const d = dict(); for (const [k, x] of dictEntries(v)) dictSet(d, k, conv(x)); return d; } return v; }; return conv(a[0]); });
        fn(g, "astuple", 1, (a) => tuple(rt.dictEntryList(typeOf(a[0]).dict.get("__dataclass_fields__")).map((e) => a[0].dict.get(e[0]))));
        fn(g, "is_dataclass", 1, (a) => { const c = isType(a[0]) ? a[0] : typeOf(a[0]); return c.dict.has("__dataclass_fields__") || c.mro.some((x) => x.dict.has("__dataclass_fields__")); });
        fnkw(g, "replace", (a) => { const kw = kwOf(a, null); const o = a[0]; const c = typeOf(o); const args = new Map(); for (const [n] of rt.dictEntryList(c.dict.get("__dataclass_fields__"))) args.set(n, kw && kw.has(n) ? kw.get(n) : o.dict.get(n)); return call(c, [], args); });
    });
    mod("enum", (g) => {
        const EnumMeta = rt.TypeType;
        const Enum = rt.newType("Enum", [], new Map(), "enum");
        Enum.dict.set("__repr__", builtin("__repr__", 1, (a) => "<" + typeOf(a[0]).name + "." + a[0].dict.get("name") + ": " + repr(a[0].dict.get("value")) + ">"));
        Enum.dict.set("__str__", builtin("__str__", 1, (a) => typeOf(a[0]).name + "." + a[0].dict.get("name")));
        Enum.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(a[0].dict.get("name"))));
        Enum.dict.set("name", { cls: T.property, fget: builtin("name", 1, (a) => a[0].dict.get("name")), fset: null, fdel: null, doc: null });
        Enum.dict.set("value", { cls: T.property, fget: builtin("value", 1, (a) => a[0].dict.get("value")), fset: null, fdel: null, doc: null });
        Enum.dict.set("__init_subclass__", { cls: T.classmethod, func: builtin("__init_subclass__", -1, (a) => {
            const cls = a[0];
            const members = [];
            let auto = 1n;
            for (const [k, v] of Array.from(cls.dict)) {
                if (k.startsWith("_") || (v !== null && typeof v === "object" && (v.cls === T.function || v.cls === T.property || v.cls === T.staticmethod || v.cls === T.classmethod || v.cls === T.builtin_function_or_method))) continue;
                let value = v;
                if (v !== null && typeof v === "object" && v.isAuto) { value = auto++; } else if (isInt(v)) auto = asInt(v) + 1n;
                const existing = members.find((m) => eq(m.dict.get("value"), value));
                if (existing) { cls.dict.set(k, existing); continue; }
                const m = { cls: cls, dict: new Map([["name", k], ["value", value]]) };
                members.push(m); cls.dict.set(k, m);
            }
            cls.members = members;
            cls.dict.set("__members__", (() => { const d = dict(); for (const m of members) dictSet(d, m.dict.get("name"), m); return d; })());
            const mixin = cls.mro.find((c) => c !== rt.ObjectType && rt.constructors.has(c));
            if (mixin !== undefined) {
                const enumIndex = cls.mro.indexOf(Enum), mixinIndex = cls.mro.indexOf(mixin);
                for (const name of ["__str__", "__repr__", "__format__"]) {
                    const owner = cls.mro.find((c) => c.dict.has(name));
                    if (owner !== undefined && cls.mro.indexOf(owner) === mixinIndex && mixinIndex < enumIndex) {
                        const fromEnum = cls.mro.slice(enumIndex).find((c) => c.dict.has(name));
                        if (fromEnum !== undefined) cls.dict.set(name, fromEnum.dict.get(name));
                    }
                }
                if (!cls.mro.slice(0, mixinIndex).some((c) => c.dict.has("__eq__"))) {
                    cls.dict.set("__eq__", builtin("__eq__", 2, (a) => a[0] === a[1] || (isInstance(a[1], mixin) && !(a[1] !== null && typeof a[1] === "object" && a[1].dict !== undefined && a[1].dict.has("value") && isInstance(a[1], Enum)) && eq(a[0].dict.get("value"), a[1]))));
                    cls.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(a[0].dict.get("value"))));
                }
            }
            rt.bumpEpoch();
            return null;
        }) });
        rt.constructors.set(Enum, null); rt.constructors.delete(Enum);
        // Enum(value) looks a member up; iterating a class yields members.
        const lookup = (cls, args) => { if (args.length !== 1) fail(E.TypeError, "Enum takes one argument"); for (const m of cls.members || []) if (eq(m.dict.get("value"), args[0])) return m; fail(E.ValueError, repr(args[0]) + " is not a valid " + cls.name); };
        Enum.dict.set("__new__", { cls: T.staticmethod, func: builtin("__new__", -1, (a) => lookup(a[0], a.slice(1))) });
        rt.enumLookup = lookup;
        Enum.dict.set("__iter__", { cls: T.classmethod, func: builtin("__iter__", 1, (a) => iter(list((a[0].members || []).slice()))) });
        Enum.dict.set("__len__", { cls: T.classmethod, func: builtin("__len__", 1, (a) => BigInt((a[0].members || []).length)) });
        Enum.dict.set("__getitem__", { cls: T.classmethod, func: builtin("__getitem__", 2, (a) => { for (const m of a[0].members || []) if (m.dict.get("name") === a[1]) return m; throw rt.makeExc(E.KeyError, [a[1]]); }) });
        g.set("Enum", Enum);
        const IntEnum = rt.newType("IntEnum", [Enum], new Map(), "enum");
        IntEnum.dict.set("__int__", builtin("__int__", 1, (a) => asInt(a[0].dict.get("value"))));
        IntEnum.dict.set("__str__", builtin("__str__", 1, (a) => str(a[0].dict.get("value"))));
        IntEnum.dict.set("__format__", builtin("__format__", 2, (a) => rt.formatValue(a[0].dict.get("value"), a[1])));
        IntEnum.dict.set("__index__", builtin("__index__", 1, (a) => asInt(a[0].dict.get("value"))));
        IntEnum.dict.set("__eq__", builtin("__eq__", 2, (a) => isInt(a[1]) ? eq(a[0].dict.get("value"), a[1]) : a[0] === a[1]));
        IntEnum.dict.set("__lt__", builtin("__lt__", 2, (a) => cmp("lt", a[0].dict.get("value"), isInt(a[1]) ? a[1] : a[1].dict.get("value"))));
        IntEnum.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(a[0].dict.get("value"))));
        for (const [n, op] of [["__add__", "add"], ["__sub__", "sub"], ["__mul__", "mul"]]) IntEnum.dict.set(n, builtin(n, 2, (a) => R.binop(op, a[0].dict.get("value"), isInt(a[1]) ? a[1] : a[1].dict.get("value"))));
        g.set("IntEnum", IntEnum); g.set("StrEnum", rt.newType("StrEnum", [Enum], new Map(), "enum"));
        // Flag members combine bitwise; a combination without its own member
        // is a pseudo-member named after the flags it contains.
        const Flag = rt.newType("Flag", [Enum], new Map(), "enum");
        const flagValue = (cls, v) => {
            for (const m of cls.members || []) if (m.dict.get("value") === v) return m;
            if (cls.composites === undefined) cls.composites = new Map();
            let m = cls.composites.get(v);
            if (m === undefined) {
                const parts = (cls.members || []).filter((x) => { const b = x.dict.get("value"); return b !== 0n && (v & b) === b; }).map((x) => x.dict.get("name"));
                m = { cls: cls, dict: new Map([["name", parts.length ? parts.join("|") : null], ["value", v]]) };
                cls.composites.set(v, m);
            }
            return m;
        };
        const fv = (x) => isInt(x) ? asInt(x) : asInt(x.dict.get("value"));
        for (const [n, op] of [["__or__", (a, b) => a | b], ["__and__", (a, b) => a & b], ["__xor__", (a, b) => a ^ b]]) {
            Flag.dict.set(n, builtin(n, 2, (a) => flagValue(typeOf(a[0]), op(fv(a[0]), fv(a[1])))));
            Flag.dict.set("__r" + n.slice(2), builtin(n, 2, (a) => flagValue(typeOf(a[0]), op(fv(a[1]), fv(a[0])))));
        }
        Flag.dict.set("__invert__", builtin("__invert__", 1, (a) => { const cls = typeOf(a[0]); let all = 0n; for (const m of cls.members || []) all |= asInt(m.dict.get("value")); return flagValue(cls, all & ~fv(a[0])); }));
        Flag.dict.set("__contains__", builtin("__contains__", 2, (a) => { const v = fv(a[0]), o = fv(a[1]); return (v & o) === o; }));
        Flag.dict.set("__bool__", builtin("__bool__", 1, (a) => fv(a[0]) !== 0n));
        Flag.dict.set("__iter__", builtin("__iter__", 1, (a) => { const v = fv(a[0]); return iter(list((typeOf(a[0]).members || []).filter((m) => { const b = asInt(m.dict.get("value")); return b !== 0n && (v & b) === b && (b & (b - 1n)) === 0n; }))); }));
        Flag.dict.set("__len__", builtin("__len__", 1, (a) => { let v = fv(a[0]), c = 0n; while (v) { c += v & 1n; v >>= 1n; } return c; }));
        Flag.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(fv(a[0]))));
        Flag.dict.set("__eq__", builtin("__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && typeOf(a[1]) === typeOf(a[0]) && fv(a[0]) === fv(a[1])));
        g.set("Flag", Flag);
        const IntFlag = rt.newType("IntFlag", [Flag, IntEnum], new Map(), "enum");
        IntFlag.dict.set("__eq__", builtin("__eq__", 2, (a) => isInt(a[1]) ? fv(a[0]) === asInt(a[1]) : (a[1] !== null && typeof a[1] === "object" && a[1].dict !== undefined && a[1].dict.has("value") && fv(a[0]) === fv(a[1]))));
        IntFlag.dict.set("__str__", builtin("__str__", 1, (a) => str(fv(a[0]))));
        for (const n of ["__or__", "__and__", "__xor__", "__ror__", "__rand__", "__rxor__"]) IntFlag.dict.set(n, builtin(n, 2, (a) => { const other = a[1]; if (!isInt(other) && !(other !== null && typeof other === "object" && other.dict !== undefined && other.dict.has("value"))) return NOTIMPL; return call(Flag.dict.get(n), [a[0], other], null); }));
        g.set("IntFlag", IntFlag);
        fn(g, "auto", 0, () => ({ cls: rt.ObjectType, dict: new Map(), isAuto: true }));
        fn(g, "unique", 1, (a) => a[0]);
    });
    mod("fractions", (g) => { g.set("Fraction", null); });
})(__zipp_py);
