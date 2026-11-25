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
    const SPEC = /^(?:(.)?([<>=^]))?([+\- ])?(z)?(#)?(0)?(\d+)?([,_])?(?:\.(\d+))?([bcdeEfFgGnosxX%])?$/;
    // The spec's longest valid prefix without a type, and what follows it.
    const SPEC_TAIL = /^(?:(?:.)?[<>=^])?[+\- ]?z?#?0?\d*[,_]?(?:\.\d+)?([\s\S]*)$/;
    function parseSpec(spec) {
        const m = SPEC.exec(spec);
        if (!m) fail(E.ValueError, "Invalid format specifier '" + spec + "'");
        return { fill: m[1], align: m[2], sign: m[3] || "-", noNeg0: !!m[4], alt: !!m[5], zero: !!m[6], width: m[7] ? parseInt(m[7], 10) : 0,
            group: m[8], precision: m[9] === undefined ? null : parseInt(m[9], 10), type: m[10] };
    }
    function group(digits, sep) { return digits.replace(/\B(?=(\d{3})+(?!\d))/g, sep); }
    // `digits` grouped by `size` with `sep`, left-padded with zeros that are
    // grouped too until at least `minWidth` characters, never starting with
    // a separator (CPython's zero padding with a grouping option).
    function groupPadded(digits, sep, size, minWidth) {
        let res = "", i = digits.length, count = 0;
        while (i > 0 || res.length < minWidth) {
            if (count === size) { res = sep + res; count = 0; }
            res = (i > 0 ? digits[--i] : "0") + res; count++;
        }
        return res;
    }
    // The '0' flag, or an explicit '0' fill with '=' alignment.
    const zeroPadded = (s) => (s.zero && s.align === undefined) || (s.fill === "0" && s.align === "=");
    function applyAlign(body, s, defaultAlign, signChar) {
        if (s.width === 0) return (signChar || "") + body;
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
        // No presentation type and no precision: repr's shortest digits,
        // including its exponent form.
        else if (type === undefined && s.precision === null) body = rt.floatRepr(ax);
        else switch (type) {
            case "f": case "F": body = rt.fixedHalfEven(ax, Math.min(p, 100)); break;
            case "e": case "E": body = expForm(ax, p); break;
            case "%": body = rt.fixedHalfEven(ax * 100, Math.min(p, 100)) + "%"; break;
            case "g": case "G": case undefined: {
                if (p === 0) p = 1;
                const exp = Math.floor(Math.log10(ax));
                let e2 = exp;
                let r = expForm(ax, p - 1); const em = /e([+-]\d+)$/.exec(r); if (em) e2 = parseInt(em[1], 10);
                // With no type (a precision but no presentation letter) the
                // switch to exponent form comes one digit earlier than 'g'.
                if (e2 < -4 || e2 >= (type === undefined ? p - 1 : p)) {
                    body = expForm(ax, p - 1);
                    // Without a type, "#" only keeps the point (repr digits, not %g's).
                    if ((!s.alt || (type === undefined && s.precision === null)) && body.includes(".")) body = body.replace(/\.?0+e/, "e");
                } else {
                    body = rt.fixedHalfEven(ax, Math.max(0, p - 1 - e2));
                    if (!s.alt && body.includes(".")) body = body.replace(/\.?0+$/, "");
                    if (type === undefined && s.precision === null) body = rt.floatRepr(ax);
                    else if (type === undefined && !body.includes(".") && !body.includes("e")) body += ".0";
                }
                break;
            }
        }
        if (type === "E" || type === "G" || type === "F") body = body.toUpperCase();
        if (!Number.isFinite(ax)) { if (type === "%") body += "%"; return applyAlign(body, s, ">", signOf(neg, s)); }
        // '#' keeps the decimal point even with no digits after it.
        if (s.alt && !body.includes(".")) { const k = body.search(/[eE%]/); body = k < 0 ? body + "." : body.slice(0, k) + "." + body.slice(k); }
        if (s.group) {
            const intLen = /^\d*/.exec(body)[0].length, rest = body.slice(intLen);
            if (zeroPadded(s) && s.width) { const sign = signOf(neg, s); return sign + groupPadded(body.slice(0, intLen), s.group, 3, s.width - sign.length - rest.length) + rest; }
            body = group(body.slice(0, intLen), s.group) + rest;
        }
        return applyAlign(body, s, ">", signOf(neg, s));
    }
    // `ax` with p digits after the point in exponent form, rounded with ties
    // to even on the EXACT binary value (toExponential breaks exact ties
    // away from zero), as rt.fixedHalfEven does for fixed point.
    function expForm(ax, p) {
        p = Math.min(p, 100);
        let e = ax.toExponential(p);
        const exact = /^(\d)\.?(\d*)e([+-]\d+)$/.exec(ax.toExponential(Math.min(p + 30, 100)));
        const digits = exact[1] + exact[2];
        if (/^50*$/.test(digits.slice(p + 1))) {
            // An exact tie: keep the truncated digits, bumped only when odd.
            let kept = digits.slice(0, p + 1), x = Number(exact[3]);
            if (Number(kept[p]) % 2 !== 0) {
                const arr = kept.split("").map(Number); let i = arr.length - 1;
                while (i >= 0) { if (arr[i] === 9) { arr[i] = 0; i--; } else { arr[i]++; break; } }
                // 9.5 -> 10: one more digit, so the exponent moves up.
                if (i < 0) { arr.unshift(1); arr.pop(); x++; }
                kept = arr.join("");
            }
            e = kept[0] + (p > 0 ? "." + kept.slice(1) : "") + "e" + (x < 0 ? "-" : "+") + Math.abs(x);
        }
        const m = /^(.*)e([+-])(\d+)$/.exec(e);
        return m[1] + "e" + m[2] + m[3].padStart(2, "0");
    }
    // _Py_dg_dtoa's digits of a finite x >= 0, trailing zeros removed ("0"
    // for zero, "" when mode 3 rounds to zero), and the decimal point's
    // position: mode 0 is the shortest round trip, mode 2 `nd` significant
    // digits, mode 3 `nd` digits after the point (both ties to even).
    const DIGITS = /^(\d)(?:\.(\d+))?e([+-]\d+)$/;
    function dtoaDigits(x, mode, nd) {
        if (x === 0) return ["0", 1];
        let digits, decpt;
        if (mode === 3) {
            const f = rt.fixedHalfEven(x, Math.min(nd, 100));
            const k = f.indexOf(".");
            digits = k < 0 ? f : f.slice(0, k) + f.slice(k + 1);
            decpt = k < 0 ? f.length : k;
            let lead = 0;
            while (lead < digits.length && digits[lead] === "0") lead++;
            digits = digits.slice(lead); decpt -= lead;
            if (digits === "" || /^0+$/.test(digits)) return ["", -nd];
        } else {
            const m = DIGITS.exec(mode === 0 ? x.toExponential() : expForm(x, Math.max(nd, 1) - 1));
            digits = m[1] + (m[2] || ""); decpt = parseInt(m[3], 10) + 1;
        }
        return [digits.replace(/0+$/, ""), decpt];
    }
    // PyOS_double_to_string (format_float_short): `code` is e/f/g/r or
    // E/F/G; `sign` always writes a sign, `alt` is '#', `noNeg0` is 'z'.
    function doubleToString(d, code, precision, sign, alt, noNeg0) {
        const upper = code === "E" || code === "F" || code === "G";
        if (upper) code = code.toLowerCase();
        let mode = 0;
        if (code === "e") { mode = 2; precision++; }
        else if (code === "f") mode = 3;
        else if (code === "g") { mode = 2; if (precision === 0) precision = 1; }
        let neg = d < 0 || Object.is(d, -0), out;
        if (!Number.isFinite(d)) {
            if (d !== d) neg = false;
            out = (neg ? "-" : sign ? "+" : "") + (d !== d ? "nan" : "inf");
            return upper ? out.toUpperCase() : out;
        }
        const dd = dtoaDigits(Math.abs(d), mode, precision), digits = dd[0], dl = digits.length;
        let decpt = dd[1];
        if (noNeg0 && neg && (dl === 0 || digits === "0")) neg = false;
        let vend = dl, useExp = false, exp = 0;
        if (code === "e") { useExp = true; vend = precision; }
        else if (code === "f") vend = decpt + precision;
        else if (code === "g") { if (decpt <= -4 || decpt > precision) useExp = true; if (alt) vend = precision; }
        else if (decpt <= -4 || decpt > 16) useExp = true;
        if (useExp) { exp = decpt - 1; decpt = 1; }
        const vstart = decpt <= 0 ? decpt - 1 : 0;
        if (vend < decpt) vend = decpt;
        out = neg ? "-" : sign ? "+" : "";
        if (decpt <= 0) out += "0".repeat(decpt - vstart) + "." + "0".repeat(-decpt);
        if (0 < decpt && decpt <= dl) out += digits.slice(0, decpt) + "." + digits.slice(decpt);
        else out += digits;
        if (dl < decpt) out += "0".repeat(decpt - dl) + "." + "0".repeat(vend - decpt);
        else out += "0".repeat(vend - dl);
        if (out.endsWith(".") && !alt) out = out.slice(0, -1);
        if (useExp) out += "e" + (exp < 0 ? "-" : "+") + String(Math.abs(exp)).padStart(2, "0");
        return upper ? out.toUpperCase() : out;
    }
    // complex.__format__ (format_complex_internal): each part formatted as
    // a float of the type, the imaginary one signed unless the real part
    // is left out, then padded as a whole. No type is repr's digits, with
    // the real part omitted when it is +0 and parentheses otherwise.
    rt.formatComplex = function (v, spec) {
        if (spec === "") return str(v);
        let s;
        const m = SPEC.exec(spec);
        if (m === null) {
            // CPython's parser errors: a '.' without digits, one unknown
            // trailing character (the type), or anything longer left over.
            const t = SPEC_TAIL.exec(spec);
            if (t !== null && t[1][0] === ".") fail(E.ValueError, "Format specifier missing precision");
            if (t !== null && t[1].length === 1) fail(E.ValueError, "Unknown format code '" + t[1] + "' for object of type 'complex'");
            fail(E.ValueError, "Invalid format specifier '" + spec + "' for object of type 'complex'");
        }
        s = parseSpec(spec);
        let type = s.type;
        if (type !== undefined && !"eEfFgGn".includes(type)) fail(E.ValueError, "Unknown format code '" + type + "' for object of type 'complex'");
        if (s.fill === "0" || (s.zero && s.fill === undefined)) fail(E.ValueError, "Zero padding is not allowed in complex format specifier");
        if (s.align === "=") fail(E.ValueError, "'=' alignment flag is not allowed in complex format specifier");
        const re = v.re, im = v.im;
        let precision = s.precision === null ? -1 : s.precision, dflt = 6, skipRe = false, parens = false;
        if (type === undefined) {
            type = "r"; dflt = 0;
            if (re === 0 && !Object.is(re, -0)) skipRe = true; else parens = true;
        }
        if (type === "n") type = "g";
        if (precision < 0) precision = dflt; else if (type === "r") type = "g";
        const part = (x, signSpec) => {
            let body = doubleToString(x, type, precision, false, s.alt, s.noNeg0), signChar = "";
            if (body[0] === "-") { signChar = "-"; body = body.slice(1); }
            else if (signSpec === "+" || signSpec === " ") signChar = signSpec;
            if (s.group) { const k = /^\d*/.exec(body)[0].length; body = group(body.slice(0, k), s.group) + body.slice(k); }
            return signChar + body;
        };
        const body = (parens ? "(" : "") + (skipRe ? "" : part(re, s.sign)) + part(im, skipRe ? s.sign : "+") + "j" + (parens ? ")" : "");
        const n = body.length;
        if (n >= s.width) return body;
        const fill = s.fill === undefined ? " " : s.fill, pad = s.width - n;
        switch (s.align) {
            case "<": return body + fill.repeat(pad);
            case "^": { const l = Math.floor(pad / 2); return fill.repeat(l) + body + fill.repeat(pad - l); }
        }
        return fill.repeat(pad) + body;
    };
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
            case "e": case "E": case "f": case "F": case "g": case "G": case "%": return formatFloat(toFloat(v), s, s.type);
            default: fail(E.ValueError, "Unknown format code '" + s.type + "' for object of type 'int'");
        }
        const prefix = /^0[boxBOX]/.test(body) ? body.slice(0, 2) : "";
        const raw = body.slice(prefix.length);
        const size = s.type && "boxX".includes(s.type) ? 4 : 3;
        if (s.group && size === 4 && s.group === ",") fail(E.ValueError, "Cannot specify ',' with '" + s.type + "'.");
        // Zero padding goes between the prefix/sign and the digits, and is
        // grouped along with them.
        if (zeroPadded(s) && s.width) {
            const sign = signOf(neg, s), minWidth = s.width - sign.length - prefix.length;
            return sign + prefix + (s.group ? groupPadded(raw, s.group, size, minWidth) : raw.padStart(minWidth, "0"));
        }
        return applyAlign(prefix + (s.group ? groupPadded(raw, s.group, size, 0) : raw), s, ">", signOf(neg, s));
    }
    function formatValue(v, spec) {
        if (typeof v === "string") {
            if (spec === "") return v;
            const s = parseSpec(spec);
            if (s.type !== undefined && s.type !== "s") fail(E.ValueError, "Unknown format code '" + s.type + "' for object of type 'str'");
            if (s.sign !== "-" && spec.includes(s.sign)) fail(E.ValueError, "Sign not allowed in string format specifier");
            let body = v; if (s.precision !== null) body = codepoints(v).slice(0, s.precision).join("");
            return applyAlign(body, s, "<", "");
        }
        if (typeof v === "bigint" && spec === "") return v.toString();
        if (typeof v === "boolean" && spec === "") return v ? "True" : "False";
        if (isInt(v)) return formatInt(asInt(v), parseSpec(spec));
        if (typeof v === "number") {
            if (spec === "") {
                if (Number.isNaN(v)) return "nan";
                if (!Number.isFinite(v)) return v < 0 ? "-inf" : "inf";
                return (v < 0 || Object.is(v, -0) ? "-" : "") + rt.floatRepr(Math.abs(v));
            }
            const s = parseSpec(spec);
            if (s.type && "bcdoxXn".includes(s.type)) fail(E.ValueError, "Unknown format code '" + s.type + "' for object of type 'float'");
            return formatFloat(v, s, s.type);
        }
        if (v === null && spec === "") return "None";
        const m = typeMethod(v, "__format__");
        // An int/float/str subclass instance formats its value unless it defines __format__.
        if (spec !== "" && v !== null && typeof v === "object" && v.pyval !== undefined && m === rt.ObjectType.dict.get("__format__")) return formatValue(v.pyval, spec);
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
    // `%d`/`%x`/`%f` accept any object with `__int__`/`__index__`/`__float__`
    // (a 0-d tensor, a numpy-like scalar), as CPython's PyNumber_* calls do.
    const numberDunder = (v, name) => {
        if (v === null || typeof v !== "object" || v.cls === undefined) return undefined;
        const m = typeMethod(v, name);
        return m ? call(descrGet(m, v, typeOf(v)), [], null) : undefined;
    };
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
            else if (t === "c") {
                if (!isInt(v) && !(typeof v === "string" && codepoints(v).length === 1)) fail(E.TypeError, "%c requires int or char");
                out += applyAlign(isInt(v) ? String.fromCodePoint(Number(asInt(v))) : v, spec, ">", "");
            }
            else if ("diu".includes(t)) { if (typeof v === "number") v = BigInt(Math.trunc(v)); else if (!isInt(v)) { let r = numberDunder(v, "__int__"); if (r === undefined) r = numberDunder(v, "__index__"); if (typeof r === "number") r = BigInt(Math.trunc(r)); if (r !== undefined && isInt(r)) v = r; } if (!isInt(v)) fail(E.TypeError, "%" + t + " format: a real number is required, not " + typeOf(v).name); spec.type = "d"; out += formatInt(asInt(v), spec); }
            else if ("oxX".includes(t)) { if (!isInt(v)) { const r = numberDunder(v, "__index__"); if (r !== undefined && isInt(r)) v = r; } if (!isInt(v)) fail(E.TypeError, "%" + t + " format: an integer is required, not " + typeOf(v).name); spec.type = t; out += formatInt(asInt(v), spec); }
            else { if (!isNum(v)) { let r = numberDunder(v, "__float__"); if (r === undefined) r = numberDunder(v, "__index__"); if (r !== undefined && isNum(r)) v = r; } if (!isNum(v)) fail(E.TypeError, "must be real number, not " + typeOf(v).name); out += formatFloat(toFloat(v), spec, t); }
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
        // CPython's math_1: a NaN from a non-NaN argument is a domain error;
        // an infinity from a finite argument is a range error for functions
        // that can overflow and a domain error for the rest (log1p(-1)).
        const unary = (x, r, canOverflow) => {
            if (r !== r) { if (x === x) fail(E.ValueError, "math domain error"); }
            else if ((r === Infinity || r === -Infinity) && Number.isFinite(x)) fail(canOverflow ? E.OverflowError : E.ValueError, canOverflow ? "math range error" : "math domain error");
            return r;
        };
        for (const [name, impl, canOverflow] of [["sin", Math.sin], ["cos", Math.cos], ["tan", Math.tan], ["asin", Math.asin], ["acos", Math.acos], ["atan", Math.atan], ["sinh", Math.sinh, true], ["cosh", Math.cosh, true], ["tanh", Math.tanh], ["asinh", Math.asinh], ["acosh", Math.acosh], ["atanh", Math.atanh], ["exp", Math.exp, true], ["expm1", Math.expm1, true], ["log1p", Math.log1p], ["fabs", Math.abs], ["cbrt", Math.cbrt], ["erf", (x) => erfExact(x)], ["erfc", (x) => 1 - erfExact(x)], ["gamma", (x) => gammaExact(x), true], ["lgamma", (x) => lgammaExact(x), true], ["exp2", (x) => Math.pow(2, x), true]]) fn(g, name, 1, (a) => { const x = f(a[0]); return unary(x, impl(x), canOverflow === true); });
        fn(g, "sqrt", 1, (a) => { const x = f(a[0]); if (x < 0) fail(E.ValueError, "math domain error"); return Math.sqrt(x); });
        // A float's square root natively (`vm::py_str`, op 11).
        if (rt.nativeEntry !== null) rt.nativeEntry(g.get("sqrt"), 1, 11);
        // CPython's loghelper: an int too large for a float is m * 2**e with
        // m the correctly rounded frexp mantissa, so log(m) + log(2) * e.
        const logOf = (v, impl) => {
            if (isInt(v)) {
                const i = asInt(v);
                if (i <= 0n) fail(E.ValueError, "math domain error");
                const x = Number(i);
                if (x !== Infinity) return impl(x);
                const bits = i.toString(16).length * 4 - 4 + (32 - Math.clz32(parseInt(i.toString(16)[0], 16)));
                const shift = BigInt(bits - 1000);
                // A sticky low bit keeps the conversion's rounding exact.
                let top = i >> shift; if ((top << shift) !== i) top |= 1n;
                let m = Number(top) / Math.pow(2, 1000), e = bits;
                if (m === 1) { m = 0.5; e += 1; }
                return impl(m) + impl(2) * e;
            }
            const x = f(v);
            if (x !== x) return x;
            if (x <= 0) fail(E.ValueError, "math domain error");
            return impl(x);
        };
        fn(g, "log", 2, (a) => {
            const num = logOf(a[0], Math.log);
            if (a[1] === undefined) return num;
            const den = logOf(a[1], Math.log);
            if (den === 0) fail(E.ZeroDivisionError, "float division by zero");
            return num / den;
        }, 1);
        fn(g, "log2", 1, (a) => logOf(a[0], Math.log2));
        fn(g, "log10", 1, (a) => logOf(a[0], Math.log10));
        // math.pow: C99 pow on the IEEE specials, then a domain error for a
        // NaN result or 0 ** negative and a range error for an overflow.
        fn(g, "pow", 2, (a) => {
            const x = f(a[0]), y = f(a[1]);
            if (!Number.isFinite(x) || !Number.isFinite(y)) {
                if (x !== x) return y === 0 ? 1 : x;
                if (y !== y) return x === 1 ? 1 : y;
                if (x === Infinity || x === -Infinity) {
                    const odd = Number.isFinite(y) && Math.abs(y) % 2 === 1;
                    if (y > 0) return odd ? x : Infinity;
                    if (y === 0) return 1;
                    return odd ? (x < 0 ? -0 : 0) : 0;
                }
                const ax = Math.abs(x);
                if (ax === 1) return 1;
                if (y > 0 && ax > 1) return y;
                if (y < 0 && ax < 1) return -y;
                return 0;
            }
            const r = Math.pow(x, y);
            if (r !== r) fail(E.ValueError, "math domain error");
            if (r === Infinity || r === -Infinity) fail(x === 0 ? E.ValueError : E.OverflowError, x === 0 ? "math domain error" : "math range error");
            return r;
        });
        fn(g, "atan2", 2, (a) => Math.atan2(f(a[0]), f(a[1])));
        fn(g, "hypot", -1, (a) => {
            const xs = a.map(f); const r = Math.hypot(...xs);
            if (r !== Infinity || xs.some((x) => !Number.isFinite(x))) return r;
            // The squares overflowed: scale by the largest magnitude.
            const m = Math.max(...xs.map(Math.abs));
            return m * Math.sqrt(xs.reduce((s, x) => s + (x / m) * (x / m), 0));
        });
        fn(g, "floor", 1, (a) => { if (isInt(a[0])) return asInt(a[0]); const m = typeMethod(a[0], "__floor__"); if (m) return call(descrGet(m, a[0], typeOf(a[0])), [], null); return rt.floatToInt(Math.floor(f(a[0]))); });
        fn(g, "ceil", 1, (a) => { if (isInt(a[0])) return asInt(a[0]); const m = typeMethod(a[0], "__ceil__"); if (m) return call(descrGet(m, a[0], typeOf(a[0])), [], null); return rt.floatToInt(Math.ceil(f(a[0]))); });
        fn(g, "trunc", 1, (a) => { if (isInt(a[0])) return asInt(a[0]); const m = typeMethod(a[0], "__trunc__"); if (m) return call(descrGet(m, a[0], typeOf(a[0])), [], null); return rt.floatToInt(f(a[0])); });
        fn(g, "isnan", 1, (a) => Number.isNaN(f(a[0])));
        fn(g, "isinf", 1, (a) => { const x = f(a[0]); return x === Infinity || x === -Infinity; });
        fn(g, "isfinite", 1, (a) => Number.isFinite(f(a[0])));
        fnkw(g, "isclose", (a) => { const kw = kwOf(a, ["rel_tol", "abs_tol"]); const x = f(a[0]), y = f(a[1]); const rel = toFloat(kwget(kw, "rel_tol", 1e-9)), abs = toFloat(kwget(kw, "abs_tol", 0)); if (x === y) return true; if (!Number.isFinite(x) || !Number.isFinite(y)) return false; const d = Math.abs(x - y); return d <= Math.max(rel * Math.max(Math.abs(x), Math.abs(y)), abs); });
        fn(g, "degrees", 1, (a) => f(a[0]) * 180 / Math.PI);
        fn(g, "radians", 1, (a) => f(a[0]) * Math.PI / 180);
        fn(g, "copysign", 2, (a) => { const x = Math.abs(f(a[0])), y = f(a[1]); return y < 0 || Object.is(y, -0) ? -x : x; });
        fn(g, "fmod", 2, (a) => { const x = f(a[0]), y = f(a[1]); const r = x % y; if (r !== r && x === x && y === y) fail(E.ValueError, "math domain error"); return r; });
        fn(g, "modf", 1, (a) => { const x = f(a[0]); const i = Math.trunc(x); return tuple([x - i, i]); });
        fn(g, "frexp", 1, (a) => { let x = f(a[0]); if (x === 0 || !Number.isFinite(x)) return tuple([x, 0n]); let e = Math.ceil(Math.log2(Math.abs(x))); let m = x / Math.pow(2, e); if (Math.abs(m) >= 1) { m /= 2; e++; } if (Math.abs(m) < 0.5) { m *= 2; e--; } return tuple([m, BigInt(e)]); });
        // x * 2**e with one rounding: split x into its exact mantissa and
        // exponent, then scale through powers of two that are representable.
        fn(g, "ldexp", 2, (a) => {
            const x = f(a[0]);
            if (!isInt(a[1])) fail(E.TypeError, "Expected an int as second argument to ldexp.");
            if (x === 0 || !Number.isFinite(x)) return x;
            const e = asInt(a[1]);
            const [m, ex] = frexpExact(x);
            if (e > 2200n) fail(E.OverflowError, "math range error");
            if (e < -2200n) return x < 0 ? -0 : 0;
            const n = ex + Number(e);
            if (n > 1024) fail(E.OverflowError, "math range error");
            if (n >= -1021) return (m * 2) * Math.pow(2, n - 1);
            if (n + 1000 < -1074) return x < 0 ? -0 : 0;
            return (m * Math.pow(2, -1000)) * Math.pow(2, n + 1000);
        });
        // x == m * 2**e exactly, 0.5 <= |m| < 1, from the IEEE fields.
        const F64 = new Float64Array(1), W = new Uint32Array(F64.buffer);
        function frexpExact(x) {
            F64[0] = x;
            let bits = (W[1] >>> 20) & 0x7ff, extra = 0;
            if (bits === 0) { F64[0] = x * 18014398509481984; bits = (W[1] >>> 20) & 0x7ff; extra = 54; }
            const e = bits - 1022;
            W[1] = (W[1] & 0x800fffff) | (1022 << 20);
            return [F64[0], e - extra];
        }
        fn(g, "gcd", -1, (a) => { let r = 0n; for (const v of a) { let x = asInt(needInt(v)); if (x < 0n) x = -x; let y = r; while (y) { [x, y] = [y, x % y]; } r = x; } return r; });
        fn(g, "lcm", -1, (a) => { let r = 1n; for (const v of a) { let x = asInt(needInt(v)); if (x < 0n) x = -x; if (x === 0n) return 0n; let p = r, q = x; while (q) { [p, q] = [q, p % q]; } r = r / p * x; } return r; });
        fn(g, "factorial", 1, (a) => { const n = needInt(a[0]); if (n < 0n) fail(E.ValueError, "factorial() not defined for negative values"); let r = 1n; for (let i = 2n; i <= n; i++) r *= i; return r; });
        fn(g, "comb", 2, (a) => { const n = needInt(a[0]), k = needInt(a[1]); if (n < 0n || k < 0n) fail(E.ValueError, "n must be a non-negative integer"); if (k > n) return 0n; let r = 1n; const kk = k < n - k ? k : n - k; for (let i = 1n; i <= kk; i++) r = r * (n - kk + i) / i; return r; });
        fn(g, "perm", 2, (a) => { const n = needInt(a[0]); const k = a[1] === undefined || a[1] === null ? n : needInt(a[1]); if (k > n) return 0n; let r = 1n; for (let i = 0n; i < k; i++) r *= n - i; return r; }, 1);
        fn(g, "isqrt", 1, (a) => {
            const n = needInt(a[0]); if (n < 0n) fail(E.ValueError, "isqrt() argument must be nonnegative"); if (n < 2n) return n;
            const approx = Math.sqrt(Number(n));
            let x;
            if (approx < 9007199254740992) x = BigInt(Math.floor(approx));
            else {
                // Newton's iteration from a power of two above the root.
                x = 1n << BigInt(Math.ceil(n.toString(16).length * 2) + 1);
                for (;;) { const y = (x + n / x) >> 1n; if (y >= x) break; x = y; }
            }
            while (x * x > n) x--; while ((x + 1n) * (x + 1n) <= n) x++; return x;
        });
        fn(g, "fsum", 1, (a) => { let s = 0, c = 0, finite = true; for (const v of drain(a[0])) { const x = f(v); if (!Number.isFinite(x)) finite = false; const y = x - c; const t = s + y; c = (t - s) - y; s = t; } if (finite && !Number.isFinite(s)) fail(E.OverflowError, "intermediate overflow in fsum"); return s; });
        fnkw(g, "prod", (a) => { const kw = kwOf(a, ["start"]); let r = kwget(kw, "start", 1n); for (const v of drain(a[0])) r = R.binop("mul", r, v); return r; });
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
            if (x === Infinity) return x;
            if (Number.isInteger(x)) {
                if (x <= 0) fail(E.ValueError, "math domain error");
                if (x <= 171) { let r = 1; for (let i = 2; i < x; i++) r *= i; return r; }
                return Infinity;
            }
            if (x < 0.5) return Math.PI / (Math.sin(Math.PI * x) * gammaExact(1 - x));
            x -= 1;
            let a = LANCZOS[0]; const t = x + 7.5;
            for (let i = 1; i < 9; i++) a += LANCZOS[i] / (x + i);
            const p = Math.pow(t, x + 0.5);
            if (p !== Infinity) return Math.sqrt(2 * Math.PI) * p * Math.exp(-t) * a;
            // t ** (x + 0.5) overflowed before exp(-t) could scale it down: in two halves.
            const half = Math.pow(t, (x + 0.5) / 2);
            return Math.sqrt(2 * Math.PI) * half * (half * Math.exp(-t)) * a;
        }
        function lgammaExact(x) {
            if (!Number.isFinite(x)) return x === x ? Infinity : x;
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
    // SHA-512 over a byte array (random.seed of a str/bytes, hashlib.sha512).
    rt.sha512 = function (bytes) {
        const K = [
        0x428a2f98d728ae22n, 0x7137449123ef65cdn, 0xb5c0fbcfec4d3b2fn, 0xe9b5dba58189dbbcn,
        0x3956c25bf348b538n, 0x59f111f1b605d019n, 0x923f82a4af194f9bn, 0xab1c5ed5da6d8118n,
        0xd807aa98a3030242n, 0x12835b0145706fben, 0x243185be4ee4b28cn, 0x550c7dc3d5ffb4e2n,
        0x72be5d74f27b896fn, 0x80deb1fe3b1696b1n, 0x9bdc06a725c71235n, 0xc19bf174cf692694n,
        0xe49b69c19ef14ad2n, 0xefbe4786384f25e3n, 0x0fc19dc68b8cd5b5n, 0x240ca1cc77ac9c65n,
        0x2de92c6f592b0275n, 0x4a7484aa6ea6e483n, 0x5cb0a9dcbd41fbd4n, 0x76f988da831153b5n,
        0x983e5152ee66dfabn, 0xa831c66d2db43210n, 0xb00327c898fb213fn, 0xbf597fc7beef0ee4n,
        0xc6e00bf33da88fc2n, 0xd5a79147930aa725n, 0x06ca6351e003826fn, 0x142929670a0e6e70n,
        0x27b70a8546d22ffcn, 0x2e1b21385c26c926n, 0x4d2c6dfc5ac42aedn, 0x53380d139d95b3dfn,
        0x650a73548baf63den, 0x766a0abb3c77b2a8n, 0x81c2c92e47edaee6n, 0x92722c851482353bn,
        0xa2bfe8a14cf10364n, 0xa81a664bbc423001n, 0xc24b8b70d0f89791n, 0xc76c51a30654be30n,
        0xd192e819d6ef5218n, 0xd69906245565a910n, 0xf40e35855771202an, 0x106aa07032bbd1b8n,
        0x19a4c116b8d2d0c8n, 0x1e376c085141ab53n, 0x2748774cdf8eeb99n, 0x34b0bcb5e19b48a8n,
        0x391c0cb3c5c95a63n, 0x4ed8aa4ae3418acbn, 0x5b9cca4f7763e373n, 0x682e6ff3d6b2b8a3n,
        0x748f82ee5defb2fcn, 0x78a5636f43172f60n, 0x84c87814a1f0ab72n, 0x8cc702081a6439ecn,
        0x90befffa23631e28n, 0xa4506cebde82bde9n, 0xbef9a3f7b2c67915n, 0xc67178f2e372532bn,
        0xca273eceea26619cn, 0xd186b8c721c0c207n, 0xeada7dd6cde0eb1en, 0xf57d4f7fee6ed178n,
        0x06f067aa72176fban, 0x0a637dc5a2c898a6n, 0x113f9804bef90daen, 0x1b710b35131c471bn,
        0x28db77f523047d84n, 0x32caab7b40c72493n, 0x3c9ebe0a15c9bebcn, 0x431d67c49c100d4cn,
        0x4cc5d4becb3e42b6n, 0x597f299cfc657e2an, 0x5fcb6fab3ad6faecn, 0x6c44198c4a475817n
        ];
        const M64 = (1n << 64n) - 1n;
        const rotr = (x, n) => ((x >> n) | (x << (64n - n))) & M64;
        let H = [0x6a09e667f3bcc908n, 0xbb67ae8584caa73bn, 0x3c6ef372fe94f82bn, 0xa54ff53a5f1d36f1n, 0x510e527fade682d1n, 0x9b05688c2b3e6c1fn, 0x1f83d9abfb41bd6bn, 0x5be0cd19137e2179n];
        const n = bytes.length, total = ((n + 17 + 127) >> 7) << 7, padded = new Uint8Array(total);
        padded.set(bytes); padded[n] = 0x80;
        let bits = BigInt(n) * 8n;
        for (let i = total - 1; i >= total - 16 && bits > 0n; i--) { padded[i] = Number(bits & 255n); bits >>= 8n; }
        const W = new Array(80);
        for (let off = 0; off < total; off += 128) {
            for (let i = 0; i < 16; i++) { let w = 0n; for (let b = 0; b < 8; b++) w = (w << 8n) | BigInt(padded[off + 8 * i + b]); W[i] = w; }
            for (let i = 16; i < 80; i++) {
                const s0 = rotr(W[i - 15], 1n) ^ rotr(W[i - 15], 8n) ^ (W[i - 15] >> 7n);
                const s1 = rotr(W[i - 2], 19n) ^ rotr(W[i - 2], 61n) ^ (W[i - 2] >> 6n);
                W[i] = (W[i - 16] + s0 + W[i - 7] + s1) & M64;
            }
            let [a, b, c, d, e, f, gg, h] = H;
            for (let i = 0; i < 80; i++) {
                const S1 = rotr(e, 14n) ^ rotr(e, 18n) ^ rotr(e, 41n), ch = (e & f) ^ (~e & M64 & gg), t1 = (h + S1 + ch + K[i] + W[i]) & M64;
                const S0 = rotr(a, 28n) ^ rotr(a, 34n) ^ rotr(a, 39n), maj = (a & b) ^ (a & c) ^ (b & c), t2 = (S0 + maj) & M64;
                h = gg; gg = f; f = e; e = (d + t1) & M64; d = c; c = b; b = a; a = (t1 + t2) & M64;
            }
            H = [(H[0] + a) & M64, (H[1] + b) & M64, (H[2] + c) & M64, (H[3] + d) & M64, (H[4] + e) & M64, (H[5] + f) & M64, (H[6] + gg) & M64, (H[7] + h) & M64];
        }
        const out = [];
        for (const w of H) for (let s = 56n; s >= 0n; s -= 8n) out.push(Number((w >> s) & 255n));
        return out;
    };

    // ---- random: CPython's Mersenne Twister and the derived distributions -------------------------------------------------
    // The same MT19937 state, seeding (init_by_array over the seed's 32-bit
    // words) and algorithms as CPython's `random`, so a seeded program draws
    // the same values: random(), getrandbits, _randbelow, randrange,
    // randint, choice, shuffle, sample, choices, uniform, gauss,
    // normalvariate and the other variates.
    // ---- cmath: CPython 3.13's cmathmodule.c. Each function returns the C99
    // Annex G value and sets `errno` as the C code does (EDOM becomes
    // "math domain error", ERANGE "math range error"); a non-finite argument
    // reads the special-value table of its (real, imag) classes.
    mod("cmath", (g) => {
        const cx = rt.cx, isCx = rt.isCx, hypot = rt.hypot;
        const EDOM = 33, ERANGE = 34;
        let errno = 0;
        const P = Math.PI, P14 = 0.25 * Math.PI, P12 = 0.5 * Math.PI, P34 = 0.75 * Math.PI, INF = Infinity, N = NaN;
        const U = -9.5426319407711027e33;   // never read: both parts finite
        const DBL_MIN = 2.2250738585072014e-308, LARGE = 1.7976931348623157e308 / 4;
        const SQRT_LARGE = Math.sqrt(LARGE), LOG_LARGE = Math.log(LARGE), SQRT_DBL_MIN = Math.sqrt(DBL_MIN);
        const LN2 = 0.6931471805599453094, LN10 = 2.302585092994045684;
        const isInf = (x) => x === Infinity || x === -Infinity;
        const copysign = (x, y) => (y < 0 || Object.is(y, -0)) ? -Math.abs(x) : Math.abs(x);
        const ldexp = (x, n) => x * Math.pow(2, n);
        const log1p = (x) => x === 0 ? x : Math.log1p(x);
        // special_type: -inf, negative, -0, +0, positive, +inf, nan.
        const st = (d) => Number.isFinite(d) ? (d !== 0 ? (d > 0 ? 4 : 1) : (Object.is(d, -0) ? 2 : 3)) : d !== d ? 6 : d > 0 ? 5 : 0;
        const special = (t, re, im) => { const k = (st(re) * 7 + st(im)) * 2; return [t[k], t[k + 1]]; };
        const ACOS = [
            P34, INF, P, INF, P, INF, P, -INF, P, -INF, P34, -INF, N, INF,
            P12, INF, U, U, U, U, U, U, U, U, P12, -INF, N, N,
            P12, INF, U, U, P12, 0, P12, -0, U, U, P12, -INF, P12, N,
            P12, INF, U, U, P12, 0, P12, -0, U, U, P12, -INF, P12, N,
            P12, INF, U, U, U, U, U, U, U, U, P12, -INF, N, N,
            P14, INF, 0, INF, 0, INF, 0, -INF, 0, -INF, P14, -INF, N, INF,
            N, INF, N, N, N, N, N, N, N, N, N, -INF, N, N];
        const ACOSH = [
            INF, -P34, INF, -P, INF, -P, INF, P, INF, P, INF, P34, INF, N,
            INF, -P12, U, U, U, U, U, U, U, U, INF, P12, N, N,
            INF, -P12, U, U, 0, -P12, 0, P12, U, U, INF, P12, N, N,
            INF, -P12, U, U, 0, -P12, 0, P12, U, U, INF, P12, N, N,
            INF, -P12, U, U, U, U, U, U, U, U, INF, P12, N, N,
            INF, -P14, INF, -0, INF, -0, INF, 0, INF, 0, INF, P14, INF, N,
            INF, N, N, N, N, N, N, N, N, N, INF, N, N, N];
        const ASINH = [
            -INF, -P14, -INF, -0, -INF, -0, -INF, 0, -INF, 0, -INF, P14, -INF, N,
            -INF, -P12, U, U, U, U, U, U, U, U, -INF, P12, N, N,
            -INF, -P12, U, U, -0, -0, -0, 0, U, U, -INF, P12, N, N,
            INF, -P12, U, U, 0, -0, 0, 0, U, U, INF, P12, N, N,
            INF, -P12, U, U, U, U, U, U, U, U, INF, P12, N, N,
            INF, -P14, INF, -0, INF, -0, INF, 0, INF, 0, INF, P14, INF, N,
            INF, N, N, N, N, -0, N, 0, N, N, INF, N, N, N];
        const ATANH = [
            -0, -P12, -0, -P12, -0, -P12, -0, P12, -0, P12, -0, P12, -0, N,
            -0, -P12, U, U, U, U, U, U, U, U, -0, P12, N, N,
            -0, -P12, U, U, -0, -0, -0, 0, U, U, -0, P12, -0, N,
            0, -P12, U, U, 0, -0, 0, 0, U, U, 0, P12, 0, N,
            0, -P12, U, U, U, U, U, U, U, U, 0, P12, N, N,
            0, -P12, 0, -P12, 0, -P12, 0, P12, 0, P12, 0, P12, 0, N,
            0, -P12, N, N, N, N, N, N, N, N, 0, P12, N, N];
        const COSH = [
            INF, N, U, U, INF, 0, INF, -0, U, U, INF, N, INF, N,
            N, N, U, U, U, U, U, U, U, U, N, N, N, N,
            N, 0, U, U, 1, 0, 1, -0, U, U, N, 0, N, 0,
            N, 0, U, U, 1, -0, 1, 0, U, U, N, 0, N, 0,
            N, N, U, U, U, U, U, U, U, U, N, N, N, N,
            INF, N, U, U, INF, -0, INF, 0, U, U, INF, N, INF, N,
            N, N, N, N, N, 0, N, 0, N, N, N, N, N, N];
        const EXP = [
            0, 0, U, U, 0, -0, 0, 0, U, U, 0, 0, 0, 0,
            N, N, U, U, U, U, U, U, U, U, N, N, N, N,
            N, N, U, U, 1, -0, 1, 0, U, U, N, N, N, N,
            N, N, U, U, 1, -0, 1, 0, U, U, N, N, N, N,
            N, N, U, U, U, U, U, U, U, U, N, N, N, N,
            INF, N, U, U, INF, -0, INF, 0, U, U, INF, N, INF, N,
            N, N, N, N, N, -0, N, 0, N, N, N, N, N, N];
        const LOG = [
            INF, -P34, INF, -P, INF, -P, INF, P, INF, P, INF, P34, INF, N,
            INF, -P12, U, U, U, U, U, U, U, U, INF, P12, N, N,
            INF, -P12, U, U, -INF, -P, -INF, P, U, U, INF, P12, N, N,
            INF, -P12, U, U, -INF, -0, -INF, 0, U, U, INF, P12, N, N,
            INF, -P12, U, U, U, U, U, U, U, U, INF, P12, N, N,
            INF, -P14, INF, -0, INF, -0, INF, 0, INF, 0, INF, P14, INF, N,
            INF, N, N, N, N, N, N, N, N, N, INF, N, N, N];
        const SINH = [
            INF, N, U, U, -INF, -0, -INF, 0, U, U, INF, N, INF, N,
            N, N, U, U, U, U, U, U, U, U, N, N, N, N,
            0, N, U, U, -0, -0, -0, 0, U, U, 0, N, 0, N,
            0, N, U, U, 0, -0, 0, 0, U, U, 0, N, 0, N,
            N, N, U, U, U, U, U, U, U, U, N, N, N, N,
            INF, N, U, U, INF, -0, INF, 0, U, U, INF, N, INF, N,
            N, N, N, N, N, -0, N, 0, N, N, N, N, N, N];
        const SQRT = [
            INF, -INF, 0, -INF, 0, -INF, 0, INF, 0, INF, INF, INF, N, INF,
            INF, -INF, U, U, U, U, U, U, U, U, INF, INF, N, N,
            INF, -INF, U, U, 0, -0, 0, 0, U, U, INF, INF, N, N,
            INF, -INF, U, U, 0, -0, 0, 0, U, U, INF, INF, N, N,
            INF, -INF, U, U, U, U, U, U, U, U, INF, INF, N, N,
            INF, -INF, INF, -0, INF, -0, INF, 0, INF, 0, INF, INF, INF, N,
            INF, -INF, N, N, N, N, N, N, N, N, INF, INF, N, N];
        const TANH = [
            -1, 0, U, U, -1, -0, -1, 0, U, U, -1, 0, -1, 0,
            N, N, U, U, U, U, U, U, U, U, N, N, N, N,
            N, N, U, U, -0, -0, -0, 0, U, U, N, N, N, N,
            N, N, U, U, 0, -0, 0, 0, U, U, N, N, N, N,
            N, N, U, U, U, U, U, U, U, U, N, N, N, N,
            1, 0, U, U, 1, -0, 1, 0, U, U, 1, 0, 1, 0,
            N, N, N, N, N, -0, N, 0, N, N, N, N, N, N];
        const RECT = [
            INF, N, U, U, -INF, 0, -INF, -0, U, U, INF, N, INF, N,
            N, N, U, U, U, U, U, U, U, U, N, N, N, N,
            0, 0, U, U, -0, 0, -0, -0, U, U, 0, 0, 0, 0,
            0, 0, U, U, 0, -0, 0, 0, U, U, 0, 0, 0, 0,
            N, N, U, U, U, U, U, U, U, U, N, N, N, N,
            INF, N, U, U, INF, -0, INF, 0, U, U, INF, N, INF, N,
            N, N, N, N, N, 0, N, 0, N, N, N, N, N, N];
        function cSqrt(zr, zi) {
            if (!Number.isFinite(zr) || !Number.isFinite(zi)) { errno = 0; return special(SQRT, zr, zi); }
            if (zr === 0 && zi === 0) return [0, zi];
            let ax = Math.abs(zr), s;
            const ay = Math.abs(zi);
            if (ax < DBL_MIN && ay < DBL_MIN) {
                // hypot(ax, ay) could be subnormal: scale up first.
                ax = ldexp(ax, 53);
                s = ldexp(Math.sqrt(ax + hypot(ax, ldexp(ay, 53))), -27);
            } else {
                ax /= 8;
                s = 2 * Math.sqrt(ax + hypot(ax, ay / 8));
            }
            const d = ay / (2 * s);
            errno = 0;
            return zr >= 0 ? [s, copysign(d, zi)] : [d, copysign(s, zi)];
        }
        function cAcos(zr, zi) {
            if (!Number.isFinite(zr) || !Number.isFinite(zi)) { errno = 0; return special(ACOS, zr, zi); }
            let rr, ri;
            if (Math.abs(zr) > LARGE || Math.abs(zi) > LARGE) {
                rr = Math.atan2(Math.abs(zi), zr);
                if (zr < 0) ri = -copysign(Math.log(hypot(zr / 2, zi / 2)) + LN2 * 2, zi);
                else ri = copysign(Math.log(hypot(zr / 2, zi / 2)) + LN2 * 2, -zi);
            } else {
                const s1 = cSqrt(1 - zr, -zi), s2 = cSqrt(1 + zr, zi);
                rr = 2 * Math.atan2(s1[0], s2[0]);
                ri = Math.asinh(s2[0] * s1[1] - s2[1] * s1[0]);
            }
            errno = 0;
            return [rr, ri];
        }
        function cAcosh(zr, zi) {
            if (!Number.isFinite(zr) || !Number.isFinite(zi)) { errno = 0; return special(ACOSH, zr, zi); }
            let rr, ri;
            if (Math.abs(zr) > LARGE || Math.abs(zi) > LARGE) {
                rr = Math.log(hypot(zr / 2, zi / 2)) + LN2 * 2;
                ri = Math.atan2(zi, zr);
            } else {
                const s1 = cSqrt(zr - 1, zi), s2 = cSqrt(zr + 1, zi);
                rr = Math.asinh(s1[0] * s2[0] + s1[1] * s2[1]);
                ri = 2 * Math.atan2(s1[1], s2[0]);
            }
            errno = 0;
            return [rr, ri];
        }
        function cAsinh(zr, zi) {
            if (!Number.isFinite(zr) || !Number.isFinite(zi)) { errno = 0; return special(ASINH, zr, zi); }
            let rr, ri;
            if (Math.abs(zr) > LARGE || Math.abs(zi) > LARGE) {
                if (zi >= 0) rr = copysign(Math.log(hypot(zr / 2, zi / 2)) + LN2 * 2, zr);
                else rr = -copysign(Math.log(hypot(zr / 2, zi / 2)) + LN2 * 2, -zr);
                ri = Math.atan2(zi, Math.abs(zr));
            } else {
                const s1 = cSqrt(1 + zi, -zr), s2 = cSqrt(1 - zi, zr);
                rr = Math.asinh(s1[0] * s2[1] - s2[0] * s1[1]);
                ri = Math.atan2(zi, s1[0] * s2[0] - s1[1] * s2[1]);
            }
            errno = 0;
            return [rr, ri];
        }
        function cAsin(zr, zi) { const s = cAsinh(-zi, zr); return [s[1], -s[0]]; }
        function cAtanh(zr, zi) {
            if (!Number.isFinite(zr) || !Number.isFinite(zi)) { errno = 0; return special(ATANH, zr, zi); }
            // atanh(z) = -atanh(-z), to have z.real >= 0.
            if (zr < 0) { const r = cAtanh(-zr, -zi); return [-r[0], -r[1]]; }
            const ay = Math.abs(zi);
            let rr, ri;
            if (zr > SQRT_LARGE || ay > SQRT_LARGE) {
                // atanh(z) ~ 1/z +/- i*pi/2 for a large z.
                const h = hypot(zr / 2, zi / 2);
                rr = zr / 4 / h / h;
                ri = -copysign(Math.PI / 2, -zi);
                errno = 0;
            } else if (zr === 1 && ay < SQRT_DBL_MIN) {
                if (ay === 0) { rr = INF; ri = zi; errno = EDOM; }
                else {
                    rr = -Math.log(Math.sqrt(ay) / Math.sqrt(hypot(ay, 2)));
                    ri = copysign(Math.atan2(2, -ay) / 2, zi);
                    errno = 0;
                }
            } else {
                rr = log1p(4 * zr / ((1 - zr) * (1 - zr) + ay * ay)) / 4;
                ri = -Math.atan2(-2 * zi, (1 - zr) * (1 + zr) - ay * ay) / 2;
                errno = 0;
            }
            return [rr, ri];
        }
        function cAtan(zr, zi) { const s = cAtanh(-zi, zr); return [s[1], -s[0]]; }
        function cCosh(zr, zi) {
            let rr, ri;
            if (!Number.isFinite(zr) || !Number.isFinite(zi)) {
                if (isInf(zr) && Number.isFinite(zi) && zi !== 0) {
                    rr = copysign(INF, Math.cos(zi));
                    ri = zr > 0 ? copysign(INF, Math.sin(zi)) : -copysign(INF, Math.sin(zi));
                } else [rr, ri] = special(COSH, zr, zi);
                errno = isInf(zi) && zr === zr ? EDOM : 0;
                return [rr, ri];
            }
            if (Math.abs(zr) > LOG_LARGE) {
                // cosh(z.real) overflows before cosh(z) does.
                const xm1 = zr - copysign(1, zr);
                rr = Math.cos(zi) * Math.cosh(xm1) * Math.E;
                ri = Math.sin(zi) * Math.sinh(xm1) * Math.E;
            } else {
                rr = Math.cos(zi) * Math.cosh(zr);
                ri = Math.sin(zi) * Math.sinh(zr);
            }
            errno = isInf(rr) || isInf(ri) ? ERANGE : 0;
            return [rr, ri];
        }
        function cCos(zr, zi) { return cCosh(-zi, zr); }
        function cExp(zr, zi) {
            let rr, ri;
            if (!Number.isFinite(zr) || !Number.isFinite(zi)) {
                if (isInf(zr) && Number.isFinite(zi) && zi !== 0) {
                    if (zr > 0) { rr = copysign(INF, Math.cos(zi)); ri = copysign(INF, Math.sin(zi)); }
                    else { rr = copysign(0, Math.cos(zi)); ri = copysign(0, Math.sin(zi)); }
                } else [rr, ri] = special(EXP, zr, zi);
                errno = isInf(zi) && (Number.isFinite(zr) || zr === Infinity) ? EDOM : 0;
                return [rr, ri];
            }
            if (zr > LOG_LARGE) {
                const l = Math.exp(zr - 1);
                rr = l * Math.cos(zi) * Math.E;
                ri = l * Math.sin(zi) * Math.E;
            } else {
                const l = Math.exp(zr);
                rr = l * Math.cos(zi);
                ri = l * Math.sin(zi);
            }
            errno = isInf(rr) || isInf(ri) ? ERANGE : 0;
            return [rr, ri];
        }
        function cLog(zr, zi) {
            if (!Number.isFinite(zr) || !Number.isFinite(zi)) { errno = 0; return special(LOG, zr, zi); }
            const ax = Math.abs(zr), ay = Math.abs(zi);
            let rr;
            if (ax > LARGE || ay > LARGE) rr = Math.log(hypot(ax / 2, ay / 2)) + LN2;
            else if (ax < DBL_MIN && ay < DBL_MIN) {
                if (ax > 0 || ay > 0) rr = Math.log(hypot(ldexp(ax, 53), ldexp(ay, 53))) - 53 * LN2;
                else { errno = EDOM; return [-INF, Math.atan2(zi, zr)]; }
            } else {
                const h = hypot(ax, ay);
                if (0.71 <= h && h <= 1.73) {
                    const am = ax > ay ? ax : ay, an = ax > ay ? ay : ax;
                    rr = log1p((am - 1) * (am + 1) + an * an) / 2;
                } else rr = Math.log(h);
            }
            errno = 0;
            return [rr, Math.atan2(zi, zr)];
        }
        function cLog10(zr, zi) { const r = cLog(zr, zi); return [r[0] / LN10, r[1] / LN10]; }
        function cSinh(zr, zi) {
            let rr, ri;
            if (!Number.isFinite(zr) || !Number.isFinite(zi)) {
                if (isInf(zr) && Number.isFinite(zi) && zi !== 0) {
                    rr = zr > 0 ? copysign(INF, Math.cos(zi)) : -copysign(INF, Math.cos(zi));
                    ri = copysign(INF, Math.sin(zi));
                } else [rr, ri] = special(SINH, zr, zi);
                errno = isInf(zi) && zr === zr ? EDOM : 0;
                return [rr, ri];
            }
            if (Math.abs(zr) > LOG_LARGE) {
                const xm1 = zr - copysign(1, zr);
                rr = Math.cos(zi) * Math.sinh(xm1) * Math.E;
                ri = Math.sin(zi) * Math.cosh(xm1) * Math.E;
            } else {
                rr = Math.cos(zi) * Math.sinh(zr);
                ri = Math.sin(zi) * Math.cosh(zr);
            }
            errno = isInf(rr) || isInf(ri) ? ERANGE : 0;
            return [rr, ri];
        }
        function cSin(zr, zi) { const s = cSinh(-zi, zr); return [s[1], -s[0]]; }
        function cTanh(zr, zi) {
            let rr, ri;
            if (!Number.isFinite(zr) || !Number.isFinite(zi)) {
                if (isInf(zr) && Number.isFinite(zi) && zi !== 0) {
                    rr = zr > 0 ? 1 : -1;
                    ri = copysign(0, 2 * Math.sin(zi) * Math.cos(zi));
                } else [rr, ri] = special(TANH, zr, zi);
                errno = isInf(zi) && Number.isFinite(zr) ? EDOM : 0;
                return [rr, ri];
            }
            if (Math.abs(zr) > LOG_LARGE) {
                rr = copysign(1, zr);
                ri = 4 * Math.sin(zi) * Math.cos(zi) * Math.exp(-2 * Math.abs(zr));
            } else {
                const tx = Math.tanh(zr), ty = Math.tan(zi), c = 1 / Math.cosh(zr), txty = tx * ty, denom = 1 + txty * txty;
                rr = tx * (1 + ty * ty) / denom;
                ri = ((ty / denom) * c) * c;
            }
            errno = 0;
            return [rr, ri];
        }
        function cTan(zr, zi) { const s = cTanh(-zi, zr); return [s[1], -s[0]]; }
        // c_atan2: C99's atan2 at the infinities and zeros.
        function cAtan2(zr, zi) {
            if (zr !== zr || zi !== zi) return NaN;
            if (isInf(zi)) {
                if (isInf(zr)) return copysign(zr > 0 ? 0.25 * Math.PI : 0.75 * Math.PI, zi);
                return copysign(0.5 * Math.PI, zi);
            }
            if (isInf(zr) || zi === 0) return copysign(zr > 0 || Object.is(zr, 0) ? 0 : Math.PI, zi);
            return Math.atan2(zi, zr);
        }
        // PyFloat_AsDouble.
        function asDouble(v) {
            if (typeof v === "number") return v;
            if (isInt(v)) return toFloat(v);
            if (v !== null && typeof v === "object" && typeof v.pyval === "number") return v.pyval;
            const m = typeMethod(v, "__float__");
            if (m !== undefined) {
                const r = call(descrGet(m, v, typeOf(v)), [], null);
                if (typeof r === "number") return r;
                if (r !== null && typeof r === "object" && typeof r.pyval === "number") return r.pyval;
                fail(E.TypeError, typeOf(v).name + ".__float__ returned non-float (type " + typeOf(r).name + ")");
            }
            if (typeMethod(v, "__index__") !== undefined) return toFloat(rt.indexOf(v));
            fail(E.TypeError, "must be real number, not " + typeOf(v).name);
        }
        // PyComplex_AsCComplex: a complex, __complex__, else a real number.
        function asComplex(v) {
            if (isCx(v)) return [v.re, v.im];
            const m = typeMethod(v, "__complex__");
            if (m !== undefined) {
                const r = call(descrGet(m, v, typeOf(v)), [], null);
                if (!isCx(r)) fail(E.TypeError, "__complex__ returned non-complex (type " + typeOf(r).name + ")");
                return [r.re, r.im];
            }
            return [asDouble(v), 0];
        }
        function mathError() {
            if (errno === EDOM) fail(E.ValueError, "math domain error");
            fail(E.OverflowError, "math range error");
        }
        for (const [name, impl] of [["acos", cAcos], ["acosh", cAcosh], ["asin", cAsin], ["asinh", cAsinh], ["atan", cAtan], ["atanh", cAtanh],
            ["cos", cCos], ["cosh", cCosh], ["exp", cExp], ["log10", cLog10], ["sin", cSin], ["sinh", cSinh], ["sqrt", cSqrt], ["tan", cTan], ["tanh", cTanh]]) {
            fn(g, name, 1, (a) => {
                const z = asComplex(a[0]);
                errno = 0;
                const r = impl(z[0], z[1]);
                if (errno !== 0) mathError();
                return cx(r[0], r[1]);
            });
        }
        // log(z[, base]): the quotient of the two logs; only the last log's
        // errno (or a zero divisor) counts, as in CPython.
        fn(g, "log", 2, (a) => {
            const z = asComplex(a[0]);
            errno = 0;
            let r = cLog(z[0], z[1]);
            if (a.length > 1) {
                const b = asComplex(a[1]);
                const lb = cLog(b[0], b[1]);
                const q = rt.cxQuot(r[0], r[1], lb[0], lb[1]);
                if (q[2]) errno = EDOM;
                r = q;
            }
            if (errno !== 0) mathError();
            return cx(r[0], r[1]);
        }, 1);
        fn(g, "phase", 1, (a) => { const z = asComplex(a[0]); return cAtan2(z[0], z[1]); });
        fn(g, "polar", 1, (a) => {
            const z = asComplex(a[0]);
            const phi = cAtan2(z[0], z[1]), r = rt.cxAbs(z[0], z[1]);
            if (r === Infinity && Number.isFinite(z[0]) && Number.isFinite(z[1])) { errno = ERANGE; mathError(); }
            return tuple([r, phi]);
        });
        fn(g, "rect", 2, (a) => {
            const r = asDouble(a[0]), phi = asDouble(a[1]);
            let zr, zi;
            errno = 0;
            if (!Number.isFinite(r) || !Number.isFinite(phi)) {
                if (isInf(r) && Number.isFinite(phi) && phi !== 0) {
                    zr = r > 0 ? copysign(INF, Math.cos(phi)) : -copysign(INF, Math.cos(phi));
                    zi = r > 0 ? copysign(INF, Math.sin(phi)) : -copysign(INF, Math.sin(phi));
                } else [zr, zi] = special(RECT, r, phi);
                if (r !== 0 && r === r && isInf(phi)) errno = EDOM;
            } else if (phi === 0) { zr = r; zi = r * phi; }
            else { zr = r * Math.cos(phi); zi = r * Math.sin(phi); }
            if (errno !== 0) mathError();
            return cx(zr, zi);
        });
        fn(g, "isfinite", 1, (a) => { const z = asComplex(a[0]); return Number.isFinite(z[0]) && Number.isFinite(z[1]); });
        fn(g, "isnan", 1, (a) => { const z = asComplex(a[0]); return z[0] !== z[0] || z[1] !== z[1]; });
        fn(g, "isinf", 1, (a) => { const z = asComplex(a[0]); return isInf(z[0]) || isInf(z[1]); });
        fnkw(g, "isclose", (a) => {
            const kw = kwOf(a, ["rel_tol", "abs_tol"]);
            if (a.length !== 2) fail(E.TypeError, "isclose() takes exactly 2 positional arguments (" + a.length + " given)");
            const x = asComplex(a[0]), y = asComplex(a[1]);
            const rel = asDouble(kwget(kw, "rel_tol", 1e-9)), abs = asDouble(kwget(kw, "abs_tol", 0));
            if (rel < 0 || abs < 0) fail(E.ValueError, "tolerances must be non-negative");
            if (x[0] === y[0] && x[1] === y[1]) return true;
            if (isInf(x[0]) || isInf(x[1]) || isInf(y[0]) || isInf(y[1])) return false;
            const diff = rt.cxAbs(x[0] - y[0], x[1] - y[1]);
            return diff <= rel * rt.cxAbs(y[0], y[1]) || diff <= rel * rt.cxAbs(x[0], x[1]) || diff <= abs;
        });
        g.set("pi", Math.PI); g.set("e", Math.E); g.set("tau", 2 * Math.PI); g.set("inf", Infinity);
        g.set("infj", cx(0, Infinity)); g.set("nan", NaN); g.set("nanj", cx(0, NaN));
    });
    mod("random", (g) => {
        const N = 624;
        function initGenrand(st, s) {
            const m = st.mt; m[0] = s >>> 0;
            for (let i = 1; i < N; i++) { const p = m[i - 1] ^ (m[i - 1] >>> 30); m[i] = (Math.imul(1812433253, p) + i) >>> 0; }
            st.i = N;
        }
        function initByArray(st, key) {
            initGenrand(st, 19650218);
            const m = st.mt;
            let i = 1, j = 0;
            for (let k = Math.max(N, key.length); k > 0; k--) {
                const p = m[i - 1] ^ (m[i - 1] >>> 30);
                m[i] = ((m[i] ^ Math.imul(p, 1664525)) + key[j] + j) >>> 0;
                i++; j++;
                if (i >= N) { m[0] = m[N - 1]; i = 1; }
                if (j >= key.length) j = 0;
            }
            for (let k = N - 1; k > 0; k--) {
                const p = m[i - 1] ^ (m[i - 1] >>> 30);
                m[i] = ((m[i] ^ Math.imul(p, 1566083941)) - i) >>> 0;
                i++;
                if (i >= N) { m[0] = m[N - 1]; i = 1; }
            }
            m[0] = 0x80000000;
            st.i = N;
        }
        function genrand(st) {
            const m = st.mt;
            if (st.i >= N) {
                let k = 0, y;
                for (; k < N - 397; k++) { y = (m[k] & 0x80000000) | (m[k + 1] & 0x7fffffff); m[k] = m[k + 397] ^ (y >>> 1) ^ (y & 1 ? 0x9908b0df : 0); }
                for (; k < N - 1; k++) { y = (m[k] & 0x80000000) | (m[k + 1] & 0x7fffffff); m[k] = m[k + 397 - N] ^ (y >>> 1) ^ (y & 1 ? 0x9908b0df : 0); }
                y = (m[N - 1] & 0x80000000) | (m[0] & 0x7fffffff); m[N - 1] = m[396] ^ (y >>> 1) ^ (y & 1 ? 0x9908b0df : 0);
                st.i = 0;
            }
            let y = m[st.i++];
            y ^= y >>> 11; y ^= (y << 7) & 0x9d2c5680; y ^= (y << 15) & 0xefc60000; y ^= y >>> 18;
            return y >>> 0;
        }
        const random = (st) => { const a = genrand(st) >>> 5, b = genrand(st) >>> 6; return (a * 67108864 + b) / 9007199254740992; };
        function getrandbits(st, k) {
            if (k < 0) fail(E.ValueError, "number of bits must be non-negative");
            if (k === 0) return 0n;
            if (k <= 32) return BigInt(genrand(st) >>> (32 - k));
            let out = 0n, shift = 0n;
            for (let left = k; left > 0; left -= 32, shift += 32n) {
                let r = genrand(st);
                if (left < 32) r >>>= 32 - left;
                out |= BigInt(r) << shift;
            }
            return out;
        }
        const bitLength = (n) => n === 0n ? 0 : n.toString(2).length;
        // Random._randbelow_with_getrandbits.
        function randbelow(st, n) {
            const k = bitLength(n);
            if (k <= 32) {
                const lim = Number(n);
                if (k === 0) return 0n;
                let r = genrand(st) >>> (32 - k);
                while (r >= lim) r = genrand(st) >>> (32 - k);
                return BigInt(r);
            }
            let r = getrandbits(st, k);
            while (r >= n) r = getrandbits(st, k);
            return r;
        }
        function seedFrom(st, a) {
            let n;
            if (a === null || a === undefined) {
                const t = BigInt(Date.now()) * 1000000n + BigInt(Math.floor((performance.now() % 1000) * 1000));
                n = t;
            } else if (isInt(a)) {
                n = asInt(a); if (n < 0n) n = -n;
            } else if (typeof a === "string" || (a !== null && typeof a === "object" && (a.cls === T.bytes || isInstance(a, T.bytes)))) {
                const bytes = typeof a === "string" ? rt.encodeStr(a, "utf-8").items : a.items;
                const all = bytes.concat(rt.sha512(bytes));
                n = 0n; for (const b of all) n = (n << 8n) | BigInt(b);
            } else if (typeof a === "number" || isInstance(a, T.float)) {
                n = BigInt.asUintN(64, BigInt(call(rt.builtins.get("hash"), [a], null)));
            } else fail(E.TypeError, "The only supported seed types are: None,\nint, float, str, bytes, and bytearray.");
            const key = [];
            while (n > 0n) { key.push(Number(n & 0xffffffffn)); n >>= 32n; }
            if (key.length === 0) key.push(0);
            initByArray(st, key);
            st.gauss = null;
        }
        const newState = () => ({ mt: new Uint32Array(N), i: N + 1, gauss: null });
        const Random = rt.newType("Random", [rt.ObjectType], new Map(), "random");
        rt.allocators.set(Random, (cls) => ({ cls: cls, rng: newState() }));
        const S = (self) => { if (self === null || typeof self !== "object" || self.rng === undefined) fail(E.TypeError, "descriptor requires a 'random.Random' object"); return self.rng; };
        const m = (name, arity, code, min) => Random.dict.set(name, builtin(name, arity, code, min));
        const mkw = (name, code) => { const f = builtin(name, -1, code); f.kwnames = true; Random.dict.set(name, f); };
        const num = (v, what) => { if (isNum(v)) return toFloat(v); fail(E.TypeError, "must be real number, not " + typeOf(v).name); };
        const index = (v) => rt.indexOf(v);
        mkw("__init__", (a) => { const kw = kwOf(a, ["x"]); const x = a[1] !== undefined ? a[1] : kwget(kw, "x", null); rt.callMethod(a[0], "seed", [x]); return null; });
        mkw("seed", (a) => {
            const kw = kwOf(a, ["a", "version"]);
            const st = S(a[0]), x = a[1] !== undefined ? a[1] : kwget(kw, "a", null);
            const version = a[2] !== undefined ? a[2] : kwget(kw, "version", 2n);
            if (Number(needInt(version)) === 1 && (typeof x === "string" || (x !== null && typeof x === "object" && x.cls === T.bytes))) {
                const s = typeof x === "string" ? codepointsOf(x) : x.items;
                let v = s.length ? BigInt(s[0]) << 7n : 0n;
                for (const c of s) v = ((1000003n * v) ^ BigInt(c)) & 0xFFFFFFFFFFFFFFFFn;
                v ^= BigInt(s.length);
                seedFrom(st, v === -1n ? -2n : v);
                return null;
            }
            seedFrom(st, x);
            return null;
        });
        const codepointsOf = (s) => codepoints(s).map((c) => c.codePointAt(0));
        m("random", 1, (a) => random(S(a[0])));
        m("getrandbits", 2, (a) => getrandbits(S(a[0]), Number(needInt(a[1]))));
        m("randbytes", 2, (a) => { const n = Number(needInt(a[1])); if (n < 0) fail(E.ValueError, "negative argument not allowed"); let v = getrandbits(S(a[0]), n * 8); const out = []; for (let i = 0; i < n; i++) { out.push(Number(v & 255n)); v >>= 8n; } return rt.bytes(out); });
        m("_randbelow", 2, (a) => randbelow(S(a[0]), needInt(a[1])));
        const fdiv = (x, y) => { const q = x / y; return x % y !== 0n && (x < 0n) !== (y < 0n) ? q - 1n : q; };
        function randrange(st, start, stop, step) {
            const istart = index(start);
            if (stop === undefined || stop === null) {
                if (step !== undefined) fail(E.TypeError, "Missing a non-None stop argument");
                if (istart > 0n) return randbelow(st, istart);
                fail(E.ValueError, "empty range for randrange()");
            }
            const istop = index(stop), width = istop - istart;
            const istep = step === undefined ? 1n : index(step);
            if (istep === 1n) {
                if (width > 0n) return istart + randbelow(st, width);
                fail(E.ValueError, "empty range for randrange() (" + istart + ", " + istop + ", " + width + ")");
            }
            let n;
            if (istep > 0n) n = fdiv(width + istep - 1n, istep);
            else if (istep < 0n) n = fdiv(width + istep + 1n, istep);
            else fail(E.ValueError, "zero step for randrange()");
            if (n <= 0n) fail(E.ValueError, "empty range for randrange()");
            return istart + istep * randbelow(st, n);
        }
        mkw("randrange", (a) => { const kw = kwOf(a, ["start", "stop", "step"]); return randrange(S(a[0]), a[1] !== undefined ? a[1] : kwget(kw, "start", undefined), a[2] !== undefined ? a[2] : kwget(kw, "stop", undefined), a[3] !== undefined ? a[3] : kwget(kw, "step", undefined)); });
        m("randint", 3, (a) => randrange(S(a[0]), a[1], R.binop("add", a[2], 1n)));
        m("choice", 2, (a) => { const st = S(a[0]), n = asInt(len(a[1])); if (n === 0n) fail(E.IndexError, "Cannot choose from an empty sequence"); return getitem(a[1], randbelow(st, n)); });
        m("shuffle", 2, (a) => {
            const st = S(a[0]), x = a[1];
            if (x !== null && typeof x === "object" && x.cls === T.list) { const l = x.items; for (let i = l.length - 1; i > 0; i--) { const j = Number(randbelow(st, BigInt(i + 1))); const t = l[i]; l[i] = l[j]; l[j] = t; } return null; }
            for (let i = asInt(len(x)) - 1n; i > 0n; i--) { const j = randbelow(st, i + 1n); const xi = getitem(x, i), xj = getitem(x, j); rt.setitem(x, i, xj); rt.setitem(x, j, xi); }
            return null;
        });
        function sample(st, population, k) {
            if (typeof population !== "string" && (population === null || typeof population !== "object" || isInstance(population, T.set) || isInstance(population, T.frozenset) || isInstance(population, T.dict))) fail(E.TypeError, "Population must be a sequence.  For dicts or sets, use sorted(d).");
            const n = Number(asInt(len(population)));
            if (!(0 <= k && k <= n)) fail(E.ValueError, "Sample larger than population or is negative");
            const result = new Array(k);
            let setsize = 21;
            if (k > 5) setsize += Math.pow(4, Math.ceil(Math.log(k * 3) / Math.log(4)));
            if (n <= setsize) {
                const pool = population.cls === T.list || population.cls === T.tuple ? population.items.slice() : drain(population);
                for (let i = 0; i < k; i++) { const j = Number(randbelow(st, BigInt(n - i))); result[i] = pool[j]; pool[j] = pool[n - i - 1]; }
            } else {
                const selected = new Set();
                for (let i = 0; i < k; i++) {
                    let j = Number(randbelow(st, BigInt(n)));
                    while (selected.has(j)) j = Number(randbelow(st, BigInt(n)));
                    selected.add(j);
                    result[i] = getitem(population, BigInt(j));
                }
            }
            return result;
        }
        mkw("sample", (a) => {
            const kw = kwOf(a, ["population", "k", "counts"]);
            const st = S(a[0]), population = a[1] !== undefined ? a[1] : kwget(kw, "population", undefined);
            const kv = a[2] !== undefined ? a[2] : kwget(kw, "k", undefined);
            if (population === undefined || kv === undefined) fail(E.TypeError, "Random.sample() missing required argument");
            const k = Number(needInt(kv)), counts = kwget(kw, "counts", null);
            if (counts !== null) {
                const cum = []; let total = 0n;
                for (const c of drain(counts)) { total = R.binop("add", total, c); cum.push(total); }
                if (cum.length !== Number(asInt(len(population)))) fail(E.ValueError, "The number of counts does not match the population");
                total = cum.pop();
                if (!isInt(total)) fail(E.TypeError, "Counts must be integers");
                if (asInt(total) <= 0n) fail(E.ValueError, "Total of counts must be greater than zero");
                const picks = sample(st, rt.range(0n, asInt(total), 1n), k);
                return list(picks.map((s) => { let lo = 0, hi = cum.length; while (lo < hi) { const mid = (lo + hi) >> 1; if (s < asInt(cum[mid])) hi = mid; else lo = mid + 1; } return getitem(population, BigInt(lo)); }));
            }
            return list(sample(st, population, k));
        });
        mkw("choices", (a) => {
            const kw = kwOf(a, ["population", "weights", "cum_weights", "k"]);
            const st = S(a[0]), population = a[1] !== undefined ? a[1] : kwget(kw, "population", undefined);
            let weights = a[2] !== undefined ? a[2] : kwget(kw, "weights", null);
            let cumw = kwget(kw, "cum_weights", null);
            const k = Number(needInt(a[3] !== undefined ? a[3] : kwget(kw, "k", 1n)));
            const n = Number(asInt(len(population)));
            const out = [];
            if (cumw === null) {
                if (weights === null) { for (let i = 0; i < k; i++) out.push(getitem(population, BigInt(Math.floor(random(st) * n)))); return list(out); }
                const acc = []; let t = null;
                for (const w of drain(weights)) { t = t === null ? w : R.binop("add", t, w); acc.push(t); }
                cumw = acc;
            } else if (weights !== null) fail(E.TypeError, "Cannot specify both weights and cumulative weights");
            else cumw = drain(cumw);
            if (cumw.length !== n) fail(E.ValueError, "The number of weights does not match the population");
            const cum = cumw.map((x) => num(x));
            const total = cum[n - 1] + 0.0;
            if (total <= 0.0) fail(E.ValueError, "Total of weights must be greater than zero");
            if (!Number.isFinite(total)) fail(E.ValueError, "Total of weights must be finite");
            const hi = n - 1;
            for (let i = 0; i < k; i++) {
                const x = random(st) * total;
                let lo = 0, h = hi;
                while (lo < h) { const mid = (lo + h) >> 1; if (x < cum[mid]) h = mid; else lo = mid + 1; }
                out.push(getitem(population, BigInt(lo)));
            }
            return list(out);
        });
        m("uniform", 3, (a) => { const x = num(a[1]), y = num(a[2]); return x + (y - x) * random(S(a[0])); });
        mkw("triangular", (a) => {
            const kw = kwOf(a, ["low", "high", "mode"]);
            const st = S(a[0]);
            let low = num(a[1] !== undefined ? a[1] : kwget(kw, "low", 0.0)), high = num(a[2] !== undefined ? a[2] : kwget(kw, "high", 1.0));
            const mode = a[3] !== undefined ? a[3] : kwget(kw, "mode", null);
            let u = random(st);
            if (high === low) return low;
            let c = mode === null ? 0.5 : (num(mode) - low) / (high - low);
            if (u > c) { u = 1.0 - u; c = 1.0 - c; const t = low; low = high; high = t; }
            return low + (high - low) * Math.sqrt(u * c);
        });
        const TWOPI = 2.0 * Math.PI, NV_MAGICCONST = 4 * Math.exp(-0.5) / Math.sqrt(2.0), LOG4 = Math.log(4.0), SG_MAGICCONST = 1.0 + Math.log(4.5);
        function gauss(st, mu, sigma) {
            let z = st.gauss; st.gauss = null;
            if (z === null) {
                const x2pi = random(st) * TWOPI, g2rad = Math.sqrt(-2.0 * Math.log(1.0 - random(st)));
                z = Math.cos(x2pi) * g2rad; st.gauss = Math.sin(x2pi) * g2rad;
            }
            return mu + z * sigma;
        }
        function normalvariate(st, mu, sigma) {
            let z;
            for (;;) { const u1 = random(st), u2 = 1.0 - random(st); z = NV_MAGICCONST * (u1 - 0.5) / u2; const zz = z * z / 4.0; if (zz <= -Math.log(u2)) break; }
            return mu + z * sigma;
        }
        function gammavariate(st, alpha, beta) {
            if (alpha <= 0.0 || beta <= 0.0) fail(E.ValueError, "gammavariate: alpha and beta must be > 0.0");
            if (alpha > 1.0) {
                const ainv = Math.sqrt(2.0 * alpha - 1.0), bbb = alpha - LOG4, ccc = alpha + ainv;
                for (;;) {
                    const u1 = random(st);
                    if (!(1e-7 < u1 && u1 < 0.9999999)) continue;
                    const u2 = 1.0 - random(st), v = Math.log(u1 / (1.0 - u1)) / ainv, x = alpha * Math.exp(v), z = u1 * u1 * u2, r = bbb + ccc * v - x;
                    if (r + SG_MAGICCONST - 4.5 * z >= 0.0 || r >= Math.log(z)) return x * beta;
                }
            }
            if (alpha === 1.0) return -Math.log(1.0 - random(st)) * beta;
            let x;
            for (;;) {
                const u = random(st), b = (Math.E + alpha) / Math.E, p = b * u;
                x = p <= 1.0 ? Math.pow(p, 1.0 / alpha) : -Math.log((b - p) / alpha);
                const u1 = random(st);
                if (p > 1.0) { if (u1 <= Math.pow(x, alpha - 1.0)) break; } else if (u1 <= Math.exp(-x)) break;
            }
            return x * beta;
        }
        const twoDefaults = (name, fnc) => mkw(name, (a) => { const kw = kwOf(a, ["mu", "sigma"]); return fnc(S(a[0]), num(a[1] !== undefined ? a[1] : kwget(kw, "mu", 0.0)), num(a[2] !== undefined ? a[2] : kwget(kw, "sigma", 1.0))); });
        twoDefaults("gauss", gauss);
        twoDefaults("normalvariate", normalvariate);
        m("lognormvariate", 3, (a) => Math.exp(normalvariate(S(a[0]), num(a[1]), num(a[2]))));
        m("expovariate", 2, (a) => { const l = num(a[1]); if (l === 0) fail(E.ZeroDivisionError, "float division by zero"); return -Math.log(1.0 - random(S(a[0]))) / l; });
        m("gammavariate", 3, (a) => gammavariate(S(a[0]), num(a[1]), num(a[2])));
        m("betavariate", 3, (a) => { const st = S(a[0]); const y = gammavariate(st, num(a[1]), 1.0); return y ? y / (y + gammavariate(st, num(a[2]), 1.0)) : 0.0; });
        m("paretovariate", 2, (a) => Math.pow(1.0 - random(S(a[0])), -1.0 / num(a[1])));
        m("weibullvariate", 3, (a) => num(a[1]) * Math.pow(-Math.log(1.0 - random(S(a[0]))), 1.0 / num(a[2])));
        m("getstate", 1, (a) => { const st = S(a[0]); const words = []; for (let i = 0; i < N; i++) words.push(BigInt(st.mt[i])); words.push(BigInt(st.i)); return tuple([3n, tuple(words), st.gauss]); });
        m("setstate", 2, (a) => {
            const st = S(a[0]), s = a[1];
            if (s === null || typeof s !== "object" || s.cls !== T.tuple || s.items.length !== 3) fail(E.ValueError, "state with version 3 passed to Random.setstate() of version 3");
            if (s.items[0] !== 3n) fail(E.ValueError, "state with version " + str(s.items[0]) + " passed to Random.setstate() of version 3");
            const words = drain(s.items[1]);
            if (words.length !== N + 1) fail(E.ValueError, "state vector is the wrong size");
            for (let i = 0; i < N; i++) st.mt[i] = Number(BigInt.asUintN(32, needInt(words[i])));
            const idx = Number(needInt(words[N]));
            if (idx < 0 || idx > N) fail(E.ValueError, "invalid state");
            st.i = idx;
            st.gauss = s.items[2] === null ? null : num(s.items[2]);
            return null;
        });
        // Copies and pickles rebuild from the state, as CPython's __reduce__.
        m("__reduce__", 1, (a) => tuple([typeOf(a[0]), tuple([]), rt.callMethod(a[0], "getstate", [])]));
        m("__getstate__", 1, (a) => rt.callMethod(a[0], "getstate", []));
        m("__setstate__", 2, (a) => rt.callMethod(a[0], "setstate", [a[1]]));
        g.set("Random", Random);
        const SystemRandom = rt.newType("SystemRandom", [Random], new Map(), "random");
        g.set("SystemRandom", SystemRandom);
        const inst = rt.allocInstance(Random);
        seedFrom(inst.rng, null);
        g.set("_inst", inst);
        for (const name of ["seed", "random", "uniform", "triangular", "randint", "choice", "randrange", "sample", "shuffle", "choices", "normalvariate", "lognormvariate", "expovariate", "gammavariate", "gauss", "betavariate", "paretovariate", "weibullvariate", "getstate", "setstate", "getrandbits", "randbytes"]) g.set(name, rt.getattr(inst, name));
    });
    // ---- time / sys / os / io ---------------------------------------------------------------------------------------------------------
    mod("time", (g) => {
        fn(g, "time", 0, () => Date.now() / 1000);
        fn(g, "time_ns", 0, () => BigInt(Date.now()) * 1000000n);
        // The interval clocks read the engine's monotonic high-resolution
        // clock (performance.now(): fractional milliseconds since start).
        for (const name of ["perf_counter", "monotonic", "process_time"]) {
            fn(g, name, 0, () => performance.now() / 1000);
            fn(g, name + "_ns", 0, () => BigInt(Math.round(performance.now() * 1e6)));
        }
        // A standalone run really blocks (a wait on a private cell nothing
        // notifies); under an embedding host sleep stays a no-op so the
        // host's worker is never held.
        let sleepCell = null;
        fn(g, "sleep", 1, (a) => {
            if (!isNum(a[0])) fail(E.TypeError, "'" + typeOf(a[0]).name + "' object cannot be interpreted as an integer");
            const secs = toFloat(a[0]);
            if (secs !== secs) fail(E.ValueError, "Invalid value NaN (not a number)");
            if (secs < 0) fail(E.ValueError, "sleep length must be non-negative");
            if (secs > 0 && !rt.hosted) {
                try {
                    if (sleepCell === null) sleepCell = new Int32Array(new SharedArrayBuffer(4));
                    Atomics.wait(sleepCell, 0, 0, secs * 1000);
                } catch (e) { /* no blocking wait on this target */ }
            }
            return null;
        });
        fn(g, "strftime", 2, (a) => { const d = new Date(); const pad = (n) => String(n).padStart(2, "0"); return needStr(a[0]).replace(/%([YmdHMSyjp%])/g, (_, c) => ({ Y: d.getFullYear(), m: pad(d.getMonth() + 1), d: pad(d.getDate()), H: pad(d.getHours()), M: pad(d.getMinutes()), S: pad(d.getSeconds()), y: pad(d.getFullYear() % 100), j: String(Math.floor((d - new Date(d.getFullYear(), 0, 0)) / 86400000)).padStart(3, "0"), p: d.getHours() < 12 ? "AM" : "PM", "%": "%" })[c]); }, 1);
        fn(g, "localtime", 1, () => { const d = new Date(); return tuple([BigInt(d.getFullYear()), BigInt(d.getMonth() + 1), BigInt(d.getDate()), BigInt(d.getHours()), BigInt(d.getMinutes()), BigInt(d.getSeconds()), BigInt((d.getDay() + 6) % 7), 1n, 0n]); }, 0);
        g.set("gmtime", g.get("localtime"));
    });
    mod("sys", (g) => {
        g.set("argv", list([rt.entryModule() ? rt.entryModule().file || "main.py" : "main.py"].concat(rt.argv)));
        const VersionInfo = rt.newType("version_info", [T.tuple], new Map(), "sys");
        const viFields = ["major", "minor", "micro", "releaselevel", "serial"];
        for (let i = 0; i < viFields.length; i++) VersionInfo.dict.set(viFields[i], { cls: T.property, fget: builtin(viFields[i], 1, (a) => a[0].items[i]), fset: null, fdel: null, doc: null });
        VersionInfo.dict.set("__repr__", builtin("__repr__", 1, (a) => "sys.version_info(major=" + a[0].items[0] + ", minor=" + a[0].items[1] + ", micro=" + a[0].items[2] + ", releaselevel=" + repr(a[0].items[3]) + ", serial=" + a[0].items[4] + ")"));
        const versionInfo = { cls: VersionInfo, items: [3n, 12n, 0n, "final", 0n], dict: new Map() };
        g.set("version", "3.12.0 (zipp)"); g.set("version_info", versionInfo);
        g.set("hexversion", 0x030c00f0n);
        g.set("platform", "zipp"); g.set("maxsize", 9223372036854775807n); g.set("byteorder", "little");
        g.set("path", list([])); g.set("modules", rt.namespaceView(rt.modules)); g.set("executable", "zipp");
        g.set("warnoptions", list([]));
        const stream = (isErr) => {
            const bt = pyClass("BufferedWriter", "io", {
                write: (a) => {
                    const b = a[1];
                    if (b === null || typeof b !== "object" || (b.cls !== T.bytes && b.cls !== T.bytearray && b.cls !== T.memoryview)) {
                        fail(E.TypeError, "a bytes-like object is required, not '" + typeOf(b).name + "'");
                    }
                    rt.writeBytesOut(b, isErr);
                    return BigInt(b.items.length);
                },
                flush: () => { rt.flushStream(isErr); return null; },
            });
            const bufObj = { cls: bt, dict: new Map() };
            const t = pyClass(isErr ? "stderr" : "stdout", "sys", {
                write: (a) => { const s = needStr(a[1]); rt.writeOut(s, isErr ? { isStderr: true } : null); return len(s); },
                writelines: (a) => { for (const line of drain(a[1])) rt.callMethod(a[0], "write", [line]); return null; },
                flush: () => { rt.flushStream(isErr); return null; },
                isatty: () => false,
            });
            const o = { cls: t, dict: new Map([["encoding", "utf-8"], ["errors", "strict"], ["buffer", bufObj]]), isStdout: !isErr, isStderr: isErr, write: true };
            return o;
        };
        g.set("stdout", stream(false)); g.set("stderr", stream(true)); g.set("stdin", null);
        fn(g, "exit", 1, (a) => { throw rt.makeExc(E.SystemExit, a.length ? [a[0]] : []); }, 0);
        fn(g, "getrecursionlimit", 0, () => 1000n);
        fn(g, "setrecursionlimit", 1, () => null);
        fn(g, "getsizeof", 1, () => 64n);
        fn(g, "intern", 1, (a) => a[0]);
        fn(g, "exc_info", 0, () => { const e = rt.currentExc(); return e === null ? tuple([null, null, null]) : tuple([typeOf(e), e, rt.tracebackOf(e)]); });
        fn(g, "exception", 0, () => rt.currentExc());
        g.set("stdlib_module_names", rt.call(rt.builtins.get("frozenset"), [list(rt.stdlibModuleNames())], null));
        fn(g, "getdefaultencoding", 0, () => "utf-8");
        fn(g, "getfilesystemencoding", 0, () => "utf-8");
        g.set("implementation", { cls: pyClass("SimpleNamespace", "types", {}), dict: new Map([["name", "zipp"], ["version", versionInfo], ["hexversion", 0x030c00f0n], ["cache_tag", null]]) });
        g.set("float_info", pyClass("float_info", "sys", {}) && (() => { const o = { cls: pyClass("float_info", "sys", {}), dict: new Map([["max", Number.MAX_VALUE], ["min", 2.2250738585072014e-308], ["epsilon", Number.EPSILON], ["dig", 15n], ["mant_dig", 53n]]) }; return o; })());
    });
    mod("keyword", (g) => {
        const kw = ["False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield"];
        const soft = ["_", "case", "match", "type"];
        const kwSet = new Set(kw), softSet = new Set(soft);
        g.set("kwlist", list(kw.slice()));
        g.set("softkwlist", list(soft.slice()));
        fn(g, "iskeyword", 1, (a) => typeof a[0] === "string" && kwSet.has(a[0]));
        fn(g, "issoftkeyword", 1, (a) => typeof a[0] === "string" && softSet.has(a[0]));
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
        fn(g, "rmdir", 1, (a) => {
            const p = needStr(a[0]), r = rt.vfs.rmdir(p);
            if (r === "missing") fail(E.FileNotFoundError, "[Errno 2] No such file or directory: '" + p + "'");
            if (r === "file") fail(E.NotADirectoryError, "[Errno 20] Not a directory: '" + p + "'");
            if (r === "full") fail(E.OSError, "[Errno 39] Directory not empty: '" + p + "'");
            return null;
        });
        for (const n of ["chdir", "system"]) fn(g, n, -1, () => fail(E.OSError, "os." + n + " is not available in the Python sandbox"));
        fn(g, "urandom", 1, (a) => { const n = Number(needInt(a[0])); if (n < 0) fail(E.ValueError, "negative argument not allowed"); const out = rt.zeroBytes(n); for (let i = 0; i < n; i++) out[i] = Math.floor(Math.random() * 256); return rt.bytes(out); });
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
        // The encoder renders straight from the Python values: ints and
        // floats stay distinct (1.0 is "1.0"), a dict is its entries in
        // insertion order (so any key text is just a key), and `default`
        // is the fallback for anything else.
        // A test first: a replace with a callback costs several times the
        // test even when nothing matches, and almost nothing does.
        const NON_PRINTABLE = /[^\x20-\x7e]/;
        function quote(s, ensureAscii) {
            const q = JSON.stringify(s);
            return ensureAscii && NON_PRINTABLE.test(q) ? q.replace(/[^\x20-\x7e]/g, (c) => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0")) : q;
        }
        function floatText(x, o) {
            if (Number.isFinite(x)) return rt.floatRepr(x);
            const t = x !== x ? "NaN" : x > 0 ? "Infinity" : "-Infinity";
            if (!o.allowNan) fail(E.ValueError, "Out of range float values are not JSON compliant: " + (x !== x ? "nan" : x > 0 ? "inf" : "-inf"));
            return t;
        }
        function keyText(k, o) {
            if (typeof k === "string") return k;
            if (k === null) return "null";
            if (typeof k === "boolean") return k ? "true" : "false";
            if (typeof k === "bigint") return k.toString();
            if (typeof k === "number") return floatText(k, o);
            if (o.skipKeys) return undefined;
            fail(E.TypeError, "keys must be str, int, float, bool or None, not " + typeOf(k).name);
        }
        // No closure captures a parameter here (a captured one lives in a
        // cell), and every loop indexes: this is the encoder's hot path.
        function encode(v, o, level) {
            if (v === null) return "null";
            if (typeof v === "string") return quote(v, o.ensureAscii);
            if (typeof v === "bigint") return v.toString();
            if (typeof v === "boolean") return v ? "true" : "false";
            if (typeof v === "number") return floatText(v, o);
            const c = v.cls;
            if (c === T.list || c === T.tuple || (v.items !== undefined && (isInstance(v, T.list) || isInstance(v, T.tuple)))) return encodeSeq(v, o, level);
            if (c === T.dict || (v.map !== undefined && isInstance(v, T.dict))) return encodeDict(v, o, level);
            if (o.dflt === null) fail(E.TypeError, "Object of type " + typeOf(v).name + " is not JSON serializable");
            enter(v, o);
            const r = encode(call(o.dflt, [v], null), o, level);
            o.seen.delete(v);
            return r;
        }
        function encodeSeq(v, o, level) {
            const items = v.items;
            if (!items.length) return "[]";
            enter(v, o);
            const parts = new Array(items.length);
            for (let i = 0; i < items.length; i++) parts[i] = encode(items[i], o, level + 1);
            o.seen.delete(v);
            return wrap("[", parts, "]", o, level);
        }
        const entryKey = (e) => e[0];
        function sortedEntries(v, entries) {
            // Python ordering of the keys. Str keys sort by code point,
            // which is the UTF-16 order of JS `<` unless a key has an
            // astral character.
            let strKeys = true;
            for (let i = 0; i < entries.length; i++) if (typeof entries[i][0] !== "string") { strKeys = false; break; }
            // Distinct str keys: the engine's ordering (code-point order)
            // sorts them without a callback per comparison.
            const perm = strKeys && rt.PYORD !== null ? rt.PYORD(1, rt.amap(entries, entryKey)) : undefined;
            if (perm !== undefined) {
                const out = new Array(perm.length);
                for (let i = 0; i < perm.length; i++) out[i] = entries[perm[i]];
                return out;
            }
            if (strKeys) return entries.sort(entries.some((e) => rt.hasSurrogate(e[0])) ? (x, y) => rt.compareStrings(x[0], y[0]) : (x, y) => (x[0] < y[0] ? -1 : x[0] > y[0] ? 1 : 0));
            return rt.sortItems(entries.map(entryKey), null, false).map((k) => [k, dictGet(v, k)]);
        }
        function encodeDict(v, o, level) {
            let entries = rt.dictEntryList(v);
            if (!entries.length) return "{}";
            enter(v, o);
            if (o.sortKeys) entries = sortedEntries(v, entries);
            const parts = [];
            const ascii = o.ensureAscii, kv = o.kv;
            for (let i = 0; i < entries.length; i++) {
                const e = entries[i];
                const kt = keyText(e[0], o);
                if (kt !== undefined) parts.push(quote(kt, ascii) + kv + encode(e[1], o, level + 1));
            }
            o.seen.delete(v);
            return parts.length ? wrap("{", parts, "}", o, level) : "{}";
        }
        function enter(v, o) { if (o.seen.has(v)) fail(E.ValueError, "Circular reference detected"); o.seen.add(v); }
        function wrap(open, parts, close, o, level) {
            if (o.indent === null) return open + parts.join(o.sep) + close;
            const pad = "\n" + o.indent.repeat(level + 1);
            return open + pad + parts.join(o.sep + pad) + "\n" + o.indent.repeat(level) + close;
        }
        // The engine's native encoder and decoder (`vm::py_json`, bound only
        // in a Python program) take the documents made of plain values and
        // answer undefined for every other one, which the code here takes.
        let JSON_NATIVE = null;
        try { JSON_NATIVE = typeof __zipp_py_json === "function" ? __zipp_py_json : null; } catch (e) { JSON_NATIVE = null; }
        // The decoder's templates: every list and dict it makes is a copy.
        const JSON_LIST = list([]), JSON_DICT = dict();
        function dumps(v, kw) {
            const indentV = kwget(kw, "indent", null);
            // An int indent is that many spaces per level, a str is itself.
            const indent = indentV === null ? null : isInt(indentV) ? " ".repeat(Math.max(0, Number(asInt(indentV)))) : needStr(indentV);
            const seps = kwget(kw, "separators", null);
            const o = {
                indent: indent,
                sep: seps !== null ? str(getitem(seps, 0n)) : indent === null ? ", " : ",",
                kv: seps !== null ? str(getitem(seps, 1n)) : ": ",
                dflt: kwget(kw, "default", null),
                sortKeys: truth(kwget(kw, "sort_keys", false)),
                ensureAscii: truth(kwget(kw, "ensure_ascii", true)),
                allowNan: truth(kwget(kw, "allow_nan", true)),
                skipKeys: truth(kwget(kw, "skipkeys", false)),
                seen: new Set(),
            };
            if (JSON_NATIVE !== null) {
                const r = JSON_NATIVE(0, v, o.indent, o.sep, o.kv, (o.sortKeys ? 1 : 0) | (o.ensureAscii ? 2 : 0) | (o.allowNan ? 4 : 0), T.list, T.tuple, T.dict);
                if (r !== undefined) return r;
            }
            return encode(v, o, 0);
        }
        // A JSON number at lastIndex (sticky: no copy of the rest of the text).
        const NUMBER = /-?(?:0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?/y;
        // A small JSON reader producing Python values directly: ints stay
        // exact (BigInt) and `1.0` stays a float, which JSON.parse cannot do.
        // object_hook / object_pairs_hook / parse_float / parse_int /
        // parse_constant apply as in CPython.
        function loads(s, kw) {
            const objectHook = kwget(kw, "object_hook", null), pairsHook = kwget(kw, "object_pairs_hook", null);
            const pFloat = kwget(kw, "parse_float", null), pInt = kwget(kw, "parse_int", null), pConst = kwget(kw, "parse_constant", null);
            if (JSON_NATIVE !== null && objectHook === null && pairsHook === null && pFloat === null && pInt === null && pConst === null) {
                const r = JSON_NATIVE(1, s, JSON_LIST, JSON_DICT);
                if (r !== undefined) return r;
            }
            let i = 0;
            const n = s.length;
            const err = (msg, at) => {
                const pos = at === undefined ? i : at;
                const line = s.slice(0, pos).split("\n").length, col = pos === 0 ? 1 : pos - s.lastIndexOf("\n", pos - 1);
                throw rt.makeExc(E.JSONDecodeError, [msg + ": line " + line + " column " + col + " (char " + pos + ")"]);
            };
            const ws = () => { let c; while (i < n && ((c = s.charCodeAt(i)) === 32 || c === 9 || c === 10 || c === 13)) i++; };
            const constant = (text, v) => { i += text.length; return pConst === null ? v : call(pConst, [text], null); };
            function object() {
                // object_pairs_hook sees the (key, value) list; otherwise a dict.
                const d = pairsHook === null ? dict() : null, pairs = d === null ? [] : null;
                i++; if (s.charCodeAt(i) <= 32) ws();
                if (s[i] !== "}") {
                    for (;;) {
                        if (s.charCodeAt(i) <= 32) ws(); if (s[i] !== '"') err("Expecting property name enclosed in double quotes");
                        const k = string(); if (s.charCodeAt(i) <= 32) ws(); if (s[i] !== ":") err("Expecting ':' delimiter"); i++;
                        const v = value();
                        if (d !== null) dictSet(d, k, v); else pairs.push(tuple([k, v]));
                        if (s.charCodeAt(i) <= 32) ws();
                        if (s[i] === ",") { i++; continue; }
                        if (s[i] === "}") break;
                        err("Expecting ',' delimiter");
                    }
                }
                i++;
                if (d === null) return call(pairsHook, [list(pairs)], null);
                return objectHook === null ? d : call(objectHook, [d], null);
            }
            function value() {
                if (s.charCodeAt(i) <= 32) ws();
                if (i >= n) err("Expecting value");
                const c = s[i];
                const u = s.charCodeAt(i);
                // A digit, or a minus not starting `-Infinity`: a number.
                if ((u >= 48 && u <= 57) || (u === 45 && s.charCodeAt(i + 1) !== 73)) {
                    NUMBER.lastIndex = i;
                    const m = NUMBER.exec(s);
                    if (!m) err("Expecting value");
                    i += m[0].length;
                    if (m[1] || m[2]) return pFloat === null ? Number(m[0]) : call(pFloat, [m[0]], null);
                    return pInt === null ? BigInt(m[0]) : call(pInt, [m[0]], null);
                }
                if (c === "{") return object();
                if (c === "[") { i++; const items = []; if (s.charCodeAt(i) <= 32) ws(); if (s[i] === "]") { i++; return list(items); } for (;;) { items.push(value()); if (s.charCodeAt(i) <= 32) ws(); if (s[i] === ",") { i++; continue; } if (s[i] === "]") { i++; return list(items); } err("Expecting ',' delimiter"); } }
                if (c === '"') return string();
                if (s.startsWith("true", i)) { i += 4; return true; }
                if (s.startsWith("false", i)) { i += 5; return false; }
                if (s.startsWith("null", i)) { i += 4; return null; }
                if (s.startsWith("NaN", i)) return constant("NaN", NaN);
                if (s.startsWith("Infinity", i)) return constant("Infinity", Infinity);
                if (s.startsWith("-Infinity", i)) return constant("-Infinity", -Infinity);
                NUMBER.lastIndex = i;
                const m = NUMBER.exec(s);
                if (!m) err("Expecting value");
                i += m[0].length;
                if (m[1] || m[2]) return pFloat === null ? Number(m[0]) : call(pFloat, [m[0]], null);
                return pInt === null ? BigInt(m[0]) : call(pInt, [m[0]], null);
            }
            // The next backslash at or after `i`, found natively and kept
            // (-2: not looked for yet; n: there is none).
            let nextBackslash = -2;
            function string() {
                const start = i;
                i++; let out = "";
                for (;;) {
                    // Copy the run up to the next quote or backslash at once.
                    if (nextBackslash !== n && nextBackslash < i) { nextBackslash = s.indexOf("\\", i); if (nextBackslash < 0) nextBackslash = n; }
                    let j = s.indexOf('"', i);
                    if (j < 0) j = n;
                    if (nextBackslash < j) j = nextBackslash;
                    if (j > i) { out += s.slice(i, j); i = j; }
                    if (i >= n) err("Unterminated string starting at", start);
                    const c = s[i++];
                    if (c === '"') return out;
                    if (c === "\\") {
                        const e = s[i++];
                        if (e === "u") {
                            const hex = s.slice(i, i + 4);
                            if (!/^[0-9a-fA-F]{4}$/.test(hex)) err("Invalid \\uXXXX escape", i - 1);
                            out += String.fromCharCode(parseInt(hex, 16)); i += 4;
                        }
                        else out += { n: "\n", t: "\t", r: "\r", b: "\b", f: "\f", "/": "/", "\\": "\\", '"': '"' }[e] || err("Invalid \\escape", i - 2);
                    } else out += c;
                }
            }
            const v = value(); ws();
            if (i < n) err("Extra data");
            return v;
        }
        const DUMP_KW = ["indent", "sort_keys", "separators", "default", "ensure_ascii", "allow_nan", "skipkeys", "check_circular"];
        const LOAD_KW = ["object_hook", "object_pairs_hook", "parse_float", "parse_int", "parse_constant", "strict"];
        fnkw(g, "dumps", (a) => { const kw = kwOf(a, DUMP_KW); return dumps(a[0], kw); });
        fnkw(g, "loads", (a) => { const kw = kwOf(a, LOAD_KW); return loads(needStr(a[0]), kw); });
        fnkw(g, "dump", (a) => { const kw = kwOf(a, DUMP_KW); call(rt.getattr(a[1], "write"), [dumps(a[0], kw)], null); return null; });
        fnkw(g, "load", (a) => { const kw = kwOf(a, LOAD_KW); return loads(needStr(call(rt.getattr(a[0], "read"), [], null)), kw); });
        g.set("JSONDecodeError", E.JSONDecodeError);
    });

    // ---- string / textwrap / copy / operator / abc / typing / __future__ ------------------------------------------------------------------------
    mod("string", (g) => {
        g.set("ascii_lowercase", "abcdefghijklmnopqrstuvwxyz"); g.set("ascii_uppercase", "ABCDEFGHIJKLMNOPQRSTUVWXYZ");
        g.set("ascii_letters", "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"); g.set("digits", "0123456789");
        g.set("hexdigits", "0123456789abcdefABCDEF"); g.set("octdigits", "01234567"); g.set("punctuation", "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~");
        g.set("whitespace", " \t\n\r\x0b\x0c"); g.set("printable", "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~ \t\n\r\x0b\x0c");
        // The words str.split() finds: between runs of Python's whitespace.
        fn(g, "capwords", 1, (a) => needStr(a[0]).split(rt.WS_RUN).filter((x) => x).map((w) => w[0].toUpperCase() + w.slice(1).toLowerCase()).join(" "));
    });
    mod("textwrap", (g) => {
        const TW_KW = ["width", "initial_indent", "subsequent_indent", "expand_tabs", "replace_whitespace", "fix_sentence_endings", "break_long_words", "drop_whitespace", "break_on_hyphens", "tabsize", "max_lines", "placeholder"];
        function expandTabsStr(s, tabsize) {
            if (s.indexOf("\t") < 0) return s;
            let out = "", col = 0;
            for (let i = 0; i < s.length; i++) {
                const ch = s[i];
                if (ch === "\t") { const pad = tabsize > 0 ? tabsize - (col % tabsize) : 0; out += " ".repeat(pad); col += pad; }
                else { out += ch; col = (ch === "\n" || ch === "\r") ? 0 : col + 1; }
            }
            return out;
        }
        let TEXTWRAP_NATIVE = null;
        try { TEXTWRAP_NATIVE = typeof __zipp_py_textwrap === "function" ? __zipp_py_textwrap : null; } catch (e) { TEXTWRAP_NATIVE = null; }
        function wrapText(text, opts) {
            const width = opts.width;
            if (width <= 0) fail(E.ValueError, "invalid width " + width + " (must be > 0)");
            if (TEXTWRAP_NATIVE !== null) {
                const flags = (opts.expand_tabs ? 1 : 0) | (opts.replace_whitespace ? 2 : 0) | (opts.break_long_words ? 4 : 0) | (opts.drop_whitespace ? 8 : 0) | (opts.break_on_hyphens ? 16 : 0);
                const r = TEXTWRAP_NATIVE(text, width, opts.initial_indent, opts.subsequent_indent, flags, opts.tabsize, opts.max_lines, opts.placeholder);
                if (r !== undefined) return r;
            }
            let s = text;
            if (opts.expand_tabs) s = expandTabsStr(s, opts.tabsize);
            if (opts.replace_whitespace) {
                let start = 0, out = "";
                const n = s.length;
                for (let i = 0; i < n; i++) {
                    const c = s.charCodeAt(i);
                    if (c <= 13 && c >= 9) {
                        if (i > start) out += s.slice(start, i);
                        out += " ";
                        start = i + 1;
                    }
                }
                if (start !== 0) s = start < n ? out + s.slice(start) : out;
            }
            const slen = s.length;
            if (slen === 0) return [];
            const initIndent = opts.initial_indent;
            const avail0 = width - initIndent.length;
            if (slen <= avail0) {
                if (!opts.drop_whitespace) return [initIndent + s];
                let end = slen;
                while (end > 0) {
                    const c = s.charCodeAt(end - 1);
                    if (c === 32 || (c <= 13 && c >= 9) || (c >= 160 && (c === 160 || c === 0xFEFF || c === 0x1680 || (c >= 0x2000 && c <= 0x200A) || c === 0x2028 || c === 0x2029 || c === 0x202F || c === 0x205F || c === 0x3000))) end--;
                    else break;
                }
                return end === 0 ? [] : [initIndent + (end === slen ? s : s.slice(0, end))];
            }
            const chunks = [];
            const wsFlags = [];
            const breakHyphens = opts.break_on_hyphens;
            let i = 0;
            while (i < slen) {
                const start = i;
                const c0 = s.charCodeAt(i);
                const ws0 = c0 === 32 || (c0 <= 13 && c0 >= 9) || (c0 >= 160 && (c0 === 160 || c0 === 0xFEFF || c0 === 0x1680 || (c0 >= 0x2000 && c0 <= 0x200A) || c0 === 0x2028 || c0 === 0x2029 || c0 === 0x202F || c0 === 0x205F || c0 === 0x3000));
                if (ws0) {
                    i++;
                    while (i < slen) {
                        const c = s.charCodeAt(i);
                        if (c === 32 || (c <= 13 && c >= 9) || (c >= 160 && (c === 160 || c === 0xFEFF || c === 0x1680 || (c >= 0x2000 && c <= 0x200A) || c === 0x2028 || c === 0x2029 || c === 0x202F || c === 0x205F || c === 0x3000))) i++;
                        else break;
                    }
                    chunks.push(s.slice(start, i));
                    wsFlags.push(true);
                } else {
                    i++;
                    while (i < slen) {
                        const c = s.charCodeAt(i);
                        if (c === 32 || (c <= 13 && c >= 9) || (c >= 160 && (c === 160 || c === 0xFEFF || c === 0x1680 || (c >= 0x2000 && c <= 0x200A) || c === 0x2028 || c === 0x2029 || c === 0x202F || c === 0x205F || c === 0x3000))) break;
                        i++;
                    }
                    if (breakHyphens) {
                        let seg = start;
                        for (let j = start; j < i; j++) {
                            if (s.charCodeAt(j) === 45 && j + 1 < i && s.charCodeAt(j + 1) !== 45) {
                                chunks.push(s.slice(seg, j + 1));
                                wsFlags.push(false);
                                seg = j + 1;
                            }
                        }
                        if (seg < i) { chunks.push(s.slice(seg, i)); wsFlags.push(false); }
                    } else {
                        chunks.push(s.slice(start, i));
                        wsFlags.push(false);
                    }
                }
            }
            chunks.reverse();
            wsFlags.reverse();
            const lines = [];
            const maxLines = opts.max_lines;
            const placeholder = opts.placeholder;
            const dropWs = opts.drop_whitespace;
            const breakLong = opts.break_long_words;
            while (chunks.length) {
                const curLine = [];
                const curWs = [];
                let curLen = 0;
                const indent = lines.length ? opts.subsequent_indent : initIndent;
                const avail = width - indent.length;
                if (dropWs && lines.length && wsFlags[wsFlags.length - 1]) { chunks.pop(); wsFlags.pop(); }
                while (chunks.length) {
                    const top = chunks[chunks.length - 1];
                    const l = top.length;
                    if (curLen + l <= avail) { curLine.push(chunks.pop()); curWs.push(wsFlags.pop()); curLen += l; }
                    else break;
                }
                if (chunks.length && chunks[chunks.length - 1].length > avail) {
                    const top = chunks[chunks.length - 1];
                    const topWs = wsFlags[wsFlags.length - 1];
                    const spaceLeft = avail - curLen;
                    if (breakLong && spaceLeft > 0) {
                        curLine.push(top.slice(0, spaceLeft));
                        curWs.push(topWs);
                        chunks[chunks.length - 1] = top.slice(spaceLeft);
                    } else if (!curLine.length) {
                        if (breakLong && avail > 0) {
                            curLine.push(top.slice(0, avail));
                            curWs.push(topWs);
                            chunks[chunks.length - 1] = top.slice(avail);
                        } else {
                            curLine.push(chunks.pop());
                            curWs.push(wsFlags.pop());
                        }
                    }
                }
                if (dropWs && curLine.length && curWs[curWs.length - 1]) { curLine.pop(); curWs.pop(); }
                if (curLine.length) {
                    if (maxLines === null || lines.length + 1 < maxLines || (!chunks.length || (dropWs && chunks.length === 1 && wsFlags[0])) && curLen <= avail) {
                        lines.push(indent + curLine.join(""));
                    } else {
                        while (curLine.length) {
                            if (!curWs[curWs.length - 1] && curLen + placeholder.length <= avail) {
                                curLine.push(placeholder);
                                lines.push(indent + curLine.join(""));
                                break;
                            }
                            curLen -= curLine.pop().length;
                            curWs.pop();
                        }
                        if (!curLine.length) lines.push(indent + placeholder.trimStart());
                        break;
                    }
                }
            }
            return lines;
        }
        function optsFrom(a, kw, startIdx) {
            const w = a[startIdx] !== undefined ? Number(needInt(a[startIdx])) : Number(needInt(kwget(kw, "width", 70n)));
            const ml = kwget(kw, "max_lines", null);
            return {
                width: w,
                initial_indent: needStr(kwget(kw, "initial_indent", "")),
                subsequent_indent: needStr(kwget(kw, "subsequent_indent", "")),
                expand_tabs: truth(kwget(kw, "expand_tabs", true)),
                replace_whitespace: truth(kwget(kw, "replace_whitespace", true)),
                fix_sentence_endings: truth(kwget(kw, "fix_sentence_endings", false)),
                break_long_words: truth(kwget(kw, "break_long_words", true)),
                drop_whitespace: truth(kwget(kw, "drop_whitespace", true)),
                break_on_hyphens: truth(kwget(kw, "break_on_hyphens", true)),
                tabsize: Number(needInt(kwget(kw, "tabsize", 8n))),
                max_lines: ml === null ? null : Number(needInt(ml)),
                placeholder: needStr(kwget(kw, "placeholder", " [...]")),
            };
        }
        const TextWrapper = pyClass("TextWrapper", "textwrap", {
            __init__: (a) => {
                const kw = kwOf(a, TW_KW);
                const opts = optsFrom(a, kw, 1);
                for (const k of TW_KW) a[0].dict.set(k, k === "width" || k === "tabsize" ? BigInt(opts[k]) : k === "max_lines" ? (opts[k] === null ? null : BigInt(opts[k])) : opts[k]);
                return null;
            },
            wrap: (a) => {
                const d = a[0].dict;
                const ml = d.get("max_lines");
                return list(wrapText(needStr(a[1]), {
                    width: Number(needInt(d.get("width"))),
                    initial_indent: needStr(d.get("initial_indent")),
                    subsequent_indent: needStr(d.get("subsequent_indent")),
                    expand_tabs: truth(d.get("expand_tabs")),
                    replace_whitespace: truth(d.get("replace_whitespace")),
                    fix_sentence_endings: truth(d.get("fix_sentence_endings")),
                    break_long_words: truth(d.get("break_long_words")),
                    drop_whitespace: truth(d.get("drop_whitespace")),
                    break_on_hyphens: truth(d.get("break_on_hyphens")),
                    tabsize: Number(needInt(d.get("tabsize"))),
                    max_lines: ml === null ? null : Number(needInt(ml)),
                    placeholder: needStr(d.get("placeholder")),
                }));
            },
            fill: (a) => call(TextWrapper.dict.get("wrap"), [a[0], a[1]], null).items.join("\n"),
        });
        TextWrapper.dict.get("__init__").kwnames = true;
        g.set("TextWrapper", TextWrapper);
        fn(g, "dedent", 1, (a) => { const lines = needStr(a[0]).split("\n"); let margin = null; for (const l of lines) { if (!l.trim()) continue; const m = /^[ \t]*/.exec(l)[0]; margin = margin === null ? m : m.startsWith(margin) ? margin : margin.startsWith(m) ? m : ""; } return lines.map((l) => l.trim() ? l.slice(margin.length) : l.trim()).join("\n"); });
        fn(g, "indent", 2, (a) => needStr(a[0]).split("\n").map((l) => l.trim() ? a[1] + l : l).join("\n"));
        function wrapSimple(text, w) {
            const s = needStr(text), width = Number(needInt(w));
            if (TEXTWRAP_NATIVE !== null && width > 0) {
                const r = TEXTWRAP_NATIVE(s, width, "", "", 31, 8, null, " [...]");
                if (r !== undefined) return r;
            }
            return wrapText(s, {
                width: width,
                initial_indent: "", subsequent_indent: "",
                expand_tabs: true, replace_whitespace: true,
                fix_sentence_endings: false, break_long_words: true,
                drop_whitespace: true, break_on_hyphens: true,
                tabsize: 8, max_lines: null, placeholder: " [...]",
            });
        }
        function wrapBlwSub(text, w, blw, sub) {
            const s = needStr(text), width = Number(needInt(w)), subS = needStr(sub), blwB = truth(blw);
            if (TEXTWRAP_NATIVE !== null && width > 0) {
                const r = TEXTWRAP_NATIVE(s, width, "", subS, blwB ? 31 : 27, 8, null, " [...]");
                if (r !== undefined) return r;
            }
            return wrapText(s, {
                width: width,
                initial_indent: "", subsequent_indent: subS,
                expand_tabs: true, replace_whitespace: true,
                fix_sentence_endings: false, break_long_words: blwB,
                drop_whitespace: true, break_on_hyphens: true,
                tabsize: 8, max_lines: null, placeholder: " [...]",
            });
        }
        fnkw(g, "wrap", (a) => { const kw = kwOf(a, TW_KW); return list(wrapText(needStr(a[0]), optsFrom(a, kw, 1))); });
        fnkw(g, "fill", (a) => { const kw = kwOf(a, TW_KW); return wrapText(needStr(a[0]), optsFrom(a, kw, 1)).join("\n"); });
        {
            const twWrap = g.get("wrap");
            twWrap.c1 = (t) => list(wrapSimple(t, 70n));
            twWrap.c2 = (t, w) => list(wrapSimple(t, w));
            twWrap["k1:width"] = twWrap.c2;
            twWrap["k2:break_long_words,subsequent_indent"] = (t, w, blw, sub) => list(wrapBlwSub(t, w, blw, sub));
            twWrap["k2:subsequent_indent,break_long_words"] = (t, w, sub, blw) => list(wrapBlwSub(t, w, blw, sub));
            twWrap["k1:width,break_long_words,subsequent_indent"] = (t, w, blw, sub) => list(wrapBlwSub(t, w, blw, sub));
            const twFill = g.get("fill");
            twFill.c1 = (t) => wrapSimple(t, 70n).join("\n");
            twFill.c2 = (t, w) => wrapSimple(t, w).join("\n");
            twFill["k1:width"] = twFill.c2;
            twFill["k2:break_long_words,subsequent_indent"] = (t, w, blw, sub) => wrapBlwSub(t, w, blw, sub).join("\n");
            twFill["k2:subsequent_indent,break_long_words"] = (t, w, sub, blw) => wrapBlwSub(t, w, blw, sub).join("\n");
        }
        // CPython collapses the text first with `" ".join(text.strip().split())`.
        fn(g, "shorten", 2, (a) => { const s = needStr(a[0]).split(rt.WS_RUN).filter((x) => x).join(" "); const w = Number(needInt(a[1])); return s.length <= w ? s : s.slice(0, Math.max(0, w - 6)).replace(/\s+\S*$/, "") + " [...]"; });
    });
    mod("copy", (g) => {
        // CPython's copy protocol: the builtin containers directly, then
        // `__copy__`/`__deepcopy__`, then a user `__reduce_ex__`/`__reduce__`
        // (rebuilt as `copy._reconstruct` does), and otherwise the default
        // object reduction: a new instance of the same class (no `__init__`)
        // with the attribute state (`__getstate__`/`__setstate__` when
        // defined) and the builtin storage it inherits (dict, list, set,
        // deque, tuple subclasses such as OrderedDict, defaultdict, Counter).
        // Native fields (an exception's args, a deque's maxlen, a boxed
        // value) carry over; typed-array buffers are duplicated by deepcopy.
        const SKIP = new Set(["id", "dictView", "cls", "dict", "items", "buf", "map", "size"]);
        const ATOMIC = new Set([T.function, T.module, T.builtin_function_or_method, T.range, T.bytes, T.property, T.classmethod, T.staticmethod, T.complex]);
        const isAtomic = (v) => v === null || typeof v !== "object" || v.cls === undefined || isType(v) || ATOMIC.has(v.cls) || (v.cls === T.frozenset && v.dict === undefined);
        const objectReduce = rt.ObjectType.dict.get("__reduce__");
        const userMethod = (v, name) => { const m = typeMethod(v, name); return m === undefined || m === null || m.isBase === true || m === objectReduce ? null : m; };
        const callOn = (v, m, args) => call(descrGet(m, v, typeOf(v)), args, null);
        const isBuffer = (x) => x !== null && typeof x === "object" && ArrayBuffer.isView(x);
        // The Python memo dict a user `__deepcopy__(memo)` receives carries the
        // JavaScript Map keyed by object, so copy.deepcopy(x, memo) inside it
        // shares the one memo.
        const pyMemo = (memo) => { if (memo.py === undefined) { memo.py = dict(); memo.py.jsMemo = memo; } return memo.py; };
        function cloneInstance(v, f, memo) {
            const c = v.cls, o = rt.allocInstance(c);
            for (const k of Object.keys(v)) {
                if (SKIP.has(k)) continue;
                const x = v[k];
                o[k] = memo !== null && isBuffer(x) ? x.slice() : x;
            }
            if (v.dict === undefined) delete o.dict;
            if (memo !== null) memo.set(v, o);
            if (v.dict !== undefined && v.dict !== null) {
                const getstate = userMethod(v, "__getstate__");
                const setstate = userMethod(o, "__setstate__");
                if (getstate !== null || setstate !== null) {
                    let state = getstate !== null ? callOn(v, getstate, []) : rt.dictFromMap(v.dict);
                    if (memo !== null) state = f(state);
                    if (setstate !== null) callOn(o, setstate, [state]);
                    else if (state !== null) for (const [k, x] of dictEntries(rt.asDict(state))) o.dict.set(needStr(k), x);
                } else for (const [k, x] of v.dict) o.dict.set(k, f(x));
            }
            if (Array.isArray(v.items)) { const items = []; for (const x of v.items) items.push(f(x)); o.items = items; }
            if (Array.isArray(v.buf) && typeof v.head === "number" && typeof v.len === "number") {
                const nb = new Array(v.buf.length), m = v.buf.length - 1, h = v.head, n = v.len;
                for (let i = 0; i < n; i++) { const idx = (h + i) & m; nb[idx] = f(v.buf[idx]); }
                o.buf = nb;
            }
            if (v.map !== undefined && isInstance(v, T.dict)) {
                const setitem = typeMethod(v, "__setitem__");
                const plain = setitem === undefined || setitem === null || setitem.isBase === true;
                for (const [k, x] of rt.dictEntryList(v)) { const key = f(k), val = f(x); if (plain) dictSet(o, key, val); else rt.setitem(o, key, val); }
            } else if (v.map !== undefined && (isInstance(v, T.set) || isInstance(v, T.frozenset))) {
                for (const x of rt.setValues(v)) rt.setAdd(o, f(x));
            }
            return o;
        }
        // copy._reconstruct over a `__reduce_ex__`/`__reduce__` result.
        function reconstruct(v, rv, f, memo) {
            if (typeof rv === "string") return v;
            if (rv === null || typeof rv !== "object" || rv.cls !== T.tuple || rv.items.length < 2) fail(E.TypeError, "__reduce__ must return a string or tuple");
            const it = rv.items, deepMode = memo !== null;
            const func = it[0], args = it[1], state = it[2], listiter = it[3], dictiter = it[4];
            const y = call(func, drain(deepMode ? f(args) : args), null);
            if (deepMode) memo.set(v, y);
            if (state !== undefined && state !== null) {
                const st = deepMode ? f(state) : state;
                const setstate = y !== null && typeof y === "object" ? typeMethod(y, "__setstate__") : undefined;
                if (setstate !== undefined && setstate !== null) callOn(y, setstate, [st]);
                else {
                    let d = st, slots = null;
                    if (st !== null && typeof st === "object" && st.cls === T.tuple && st.items.length === 2) { d = st.items[0]; slots = st.items[1]; }
                    if (d !== null) for (const [k, x] of dictEntries(rt.asDict(d))) y.dict.set(needStr(k), x);
                    if (slots !== null) for (const [k, x] of dictEntries(rt.asDict(slots))) rt.setattr(y, k, x);
                }
            }
            if (listiter !== undefined && listiter !== null) for (const x of drain(listiter)) rt.callMethod(y, "append", [deepMode ? f(x) : x]);
            if (dictiter !== undefined && dictiter !== null) for (const kv of drain(dictiter)) { const k = getitem(kv, 0n), x = getitem(kv, 1n); rt.setitem(y, deepMode ? f(k) : k, deepMode ? f(x) : x); }
            return y;
        }
        function reduced(v) {
            const rex = userMethod(v, "__reduce_ex__");
            if (rex !== null) return callOn(v, rex, [4n]);
            const r = userMethod(v, "__reduce__");
            return r !== null ? callOn(v, r, []) : undefined;
        }
        const same = (x) => x;
        function shallow(v) {
            if (isAtomic(v)) return v;
            const c = v.cls;
            if (c === T.list) return list(v.items.slice());
            if (c === T.tuple) return v;
            if (c === T.dict) return rt.dictCopy(v);
            if (c === T.set) return rt.setFrom(v, T.set);
            const m = typeMethod(v, "__copy__");
            if (m) return callOn(v, m, []);
            const rv = reduced(v);
            if (rv !== undefined) return reconstruct(v, rv, same, null);
            if (v.dict === undefined && v.map === undefined && v.items === undefined) return v;
            return cloneInstance(v, same, null);
        }
        function deep(v, memo) {
            if (isAtomic(v)) return v;
            const known = memo.get(v);
            if (known !== undefined) return known;
            const c = v.cls, f = (x) => deep(x, memo);
            let out;
            if (c === T.list) { out = list([]); memo.set(v, out); for (const x of v.items) out.items.push(deep(x, memo)); return out; }
            if (c === T.tuple) {
                const items = []; let changed = false;
                for (const x of v.items) { const y = deep(x, memo); if (y !== x) changed = true; items.push(y); }
                // A tuple holding itself through a list was copied meanwhile.
                const again = memo.get(v);
                if (again !== undefined) return again;
                out = changed ? tuple(items) : v; memo.set(v, out); return out;
            }
            if (c === T.dict) { out = dict(); memo.set(v, out); for (const [k, x] of rt.dictEntryList(v)) dictSet(out, deep(k, memo), deep(x, memo)); return out; }
            if (c === T.set) { out = rt.set(); memo.set(v, out); for (const x of rt.setValues(v)) rt.setAdd(out, deep(x, memo)); return out; }
            const m = typeMethod(v, "__deepcopy__");
            if (m) { out = callOn(v, m, [pyMemo(memo)]); memo.set(v, out); return out; }
            const rv = reduced(v);
            if (rv !== undefined) return reconstruct(v, rv, f, memo);
            // A native value holding a typed-array buffer (a tensor storage)
            // is duplicated rather than shared; other native values are.
            if (v.dict === undefined && v.map === undefined && v.items === undefined && !isBuffer(v.data)) return v;
            return cloneInstance(v, f, memo);
        }
        fn(g, "copy", 1, (a) => shallow(a[0]));
        fnkw(g, "deepcopy", (a) => {
            const kw = kwOf(a, ["memo"]);
            if (a.length < 1 || a.length > 2) fail(E.TypeError, "deepcopy() takes from 1 to 2 positional arguments but " + a.length + " were given");
            const pm = a[1] !== undefined ? a[1] : kwget(kw, "memo", null);
            let memo;
            if (pm === null) memo = new Map();
            else if (typeof pm === "object" && pm.jsMemo !== undefined) memo = pm.jsMemo;
            else { memo = new Map(); if (typeof pm === "object") { memo.py = pm; pm.jsMemo = memo; } }
            return deep(a[0], memo);
        });
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
        const ABCMeta = rt.newType("ABCMeta", [rt.TypeType], new Map(), "abc");
        const abcNew = builtin("__new__", -1, (a) => {
            const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
            const cls = rt.makeClass(a[0], a[1], a[2], a[3], kw);
            cls.isABC = true;
            cls._abc_registry = new Set();
            return cls;
        });
        abcNew.kwnames = true;
        ABCMeta.dict.set("__new__", { cls: T.staticmethod, func: abcNew });
        ABCMeta.dict.set("register", builtin("register", 2, (a) => {
            const cls = a[0], sub = a[1];
            if (!isType(sub)) fail(E.TypeError, "Can only register classes");
            if (rt.isSubclass(sub, cls)) return sub;
            if (rt.isSubclass(cls, sub)) fail(E.RuntimeError, "Refusing to create an inheritance cycle");
            if (cls._abc_registry === undefined) cls._abc_registry = new Set();
            cls._abc_registry.add(sub);
            return sub;
        }));
        const abcIsSub = (sub, cls, seen) => {
            if (!isType(sub)) fail(E.TypeError, "issubclass() arg 1 must be a class");
            if (rt.isSubclass(sub, cls)) return true;
            if (seen.has(cls)) return false;
            seen.add(cls);
            const hook = cls.dict.get("__subclasshook__");
            if (hook !== undefined) {
                const ok = call(descrGet(hook, null, cls), [sub], null);
                if (ok !== NOTIMPL) return truth(ok);
            }
            if (cls._abc_registry !== undefined) {
                for (const r of cls._abc_registry) if (abcIsSub(sub, r, new Set())) return true;
            }
            if (cls.subclasses !== undefined) {
                for (let i = 0; i < cls.subclasses.length; i++) {
                    const s = cls.subclasses[i].deref();
                    if (s !== undefined && s !== cls && abcIsSub(sub, s, seen)) return true;
                }
            }
            return false;
        };
        ABCMeta.dict.set("__subclasscheck__", builtin("__subclasscheck__", 2, (a) => abcIsSub(a[1], a[0], new Set())));
        ABCMeta.dict.set("__instancecheck__", builtin("__instancecheck__", 2, (a) => {
            const cls = a[0], t = typeOf(a[1]);
            return rt.isSubclass(t, cls) || abcIsSub(t, cls, new Set());
        }));
        const ABC = rt.newType("ABC", [], new Map(), "abc");
        ABC.cls = ABCMeta;
        ABC.isABC = true;
        ABC._abc_registry = new Set();
        g.set("ABC", ABC);
        g.set("ABCMeta", ABCMeta);
        fn(g, "abstractmethod", 1, (a) => { const f = a[0]; if (f !== null && typeof f === "object") f.isabstract = true; return f; });
        fn(g, "abstractproperty", 1, (a) => a[0]);
    });
    mod("typing", (g) => {
        // Generic aliases print like CPython's: typing.Dict[str, int].
        const ABCMeta = R.import("abc", null).globals.get("ABCMeta");
        const typeName = (x) => isType(x) ? (x.module === "builtins" ? x.name : x.module + "." + x.qualname) : x === null ? "None" : repr(x);
        const rootInitSub = { cls: T.classmethod, func: builtin("__init_subclass__", -1, () => null) };
        rootInitSub.func.kwnames = true;
        const _Final = rt.newType("_Final", [], new Map([["__init_subclass__", rootInitSub]]), "typing");
        const _SpecialForm = rt.newType("_SpecialForm", [_Final], new Map([
            ["__init_subclass__", rootInitSub],
            ["__init__", builtin("__init__", -1, (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                const f = a[1];
                a[0].dict.set("_getitem", f);
                a[0].dict.set("_name", f && f.name ? f.name : (kw && kw.has("name") ? kw.get("name") : "SpecialForm"));
                a[0].dict.set("__doc__", f && f.doc !== undefined ? f.doc : null);
                return null;
            })],
            ["__getitem__", builtin("__getitem__", 2, (a) => {
                const fn = a[0].dict.get("_getitem");
                if (fn !== undefined && fn !== null) return call(fn, [a[0], a[1]], null);
                return { cls: _GenericAlias, dict: new Map([["__origin__", a[0]], ["__args__", a[1] !== null && typeof a[1] === "object" && a[1].cls === T.tuple ? a[1] : tuple([a[1]])], ["_name", a[0].dict.get("_name") || a[0].dict.get("name") || ""]]) };
            })],
            ["__repr__", builtin("__repr__", 1, (a) => "typing." + (a[0].dict.get("_name") || a[0].dict.get("name") || "SpecialForm"))],
            ["__or__", builtin("__or__", 2, (a) => rt.makeUnion(a[0], a[1]))],
            ["__ror__", builtin("__ror__", 2, (a) => rt.makeUnion(a[1], a[0]))],
            ["__call__", builtin("__call__", -1, (a) => a[1] === undefined ? null : a[1])],
        ]), "typing");
        _SpecialForm.dict.get("__init__").kwnames = true;
        const _BaseGenericAlias = rt.newType("_BaseGenericAlias", [_Final], new Map([
            ["__init_subclass__", rootInitSub],
            ["__init__", builtin("__init__", -1, (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                const origin = a[1];
                a[0].dict.set("__origin__", origin);
                a[0].dict.set("_inst", kw && kw.has("inst") ? kw.get("inst") : true);
                a[0].dict.set("_name", kw && kw.has("name") ? kw.get("name") : (origin && origin.name ? origin.name : null));
                return null;
            })],
            ["__mro_entries__", builtin("__mro_entries__", 2, (a) => { const o = a[0].dict.get("__origin__"); return isType(o) ? tuple([o]) : tuple([rt.Generic]); })],
            ["__instancecheck__", builtin("__instancecheck__", 2, (a) => { const o = a[0].dict.get("__origin__"); if (isType(o)) return rt.isinstanceCheck(a[1], o); fail(E.TypeError, "Subscripted generics cannot be used with class and instance checks"); })],
            ["__subclasscheck__", builtin("__subclasscheck__", 2, (a) => { const o = a[0].dict.get("__origin__"); if (isType(o)) return rt.call(rt.builtins.get("issubclass"), [a[1], o], null); fail(E.TypeError, "Subscripted generics cannot be used with class and instance checks"); })],
            ["__or__", builtin("__or__", 2, (a) => rt.makeUnion(a[0], a[1]))],
            ["__ror__", builtin("__ror__", 2, (a) => rt.makeUnion(a[1], a[0]))],
            ["__call__", builtin("__call__", -1, (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; const o = a[0].dict.get("__origin__"); return o && isType(o) ? call(o, a.slice(1), kw) : (a[1] === undefined ? null : a[1]); })],
        ]), "typing");
        _BaseGenericAlias.dict.get("__init__").kwnames = true;
        _BaseGenericAlias.dict.get("__call__").kwnames = true;
        const _GenericAlias = rt.newType("_GenericAlias", [_BaseGenericAlias], new Map([
            ["__init_subclass__", rootInitSub],
            ["__init__", builtin("__init__", -1, (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                const origin = a[1], args = a[2];
                a[0].dict.set("__origin__", origin);
                a[0].dict.set("__args__", args !== null && typeof args === "object" && args.cls === T.tuple ? args : tuple(args === undefined ? [] : [args]));
                a[0].dict.set("__parameters__", tuple([]));
                a[0].dict.set("_inst", kw && kw.has("inst") ? kw.get("inst") : true);
                a[0].dict.set("_name", kw && kw.has("name") ? kw.get("name") : (origin && origin.name ? origin.name : null));
                return null;
            })],
            ["copy_with", builtin("copy_with", 2, (a) => ({ cls: a[0].cls, dict: new Map([["__origin__", a[0].dict.get("__origin__")], ["__args__", a[1] !== null && typeof a[1] === "object" && a[1].cls === T.tuple ? a[1] : tuple([a[1]])], ["__parameters__", tuple([])], ["_name", a[0].dict.get("_name")]]) }))],
            ["__getitem__", builtin("__getitem__", 2, (a) => {
                const self = a[0], arg = a[1];
                const isSingle = !(arg !== null && typeof arg === "object" && arg.cls === T.tuple);
                if (isSingle) {
                    const cache = self._subCache;
                    if (cache !== undefined) {
                        const hit = cache.get(arg);
                        if (hit !== undefined) return hit;
                    }
                }
                const res = { cls: self.cls, dict: new Map([["__origin__", self.dict.get("__origin__")], ["__args__", isSingle ? tuple([arg]) : arg], ["__parameters__", tuple([])], ["_name", self.dict.get("_name")]]) };
                if (isSingle) {
                    let cache = self._subCache;
                    if (cache === undefined) cache = self._subCache = new Map();
                    if (cache.size < 256) cache.set(arg, res);
                }
                return res;
            })],
            ["__repr__", builtin("__repr__", 1, (a) => { const n = a[0].dict.get("_name") || typeName(a[0].dict.get("__origin__")); const args = a[0].dict.get("__args__"); return "typing." + n + (args === undefined || args.items.length === 0 ? "" : "[" + args.items.map(typeName).join(", ") + "]"); })],
        ]), "typing");
        _GenericAlias.dict.get("__init__").kwnames = true;
        const _SpecialGenericAlias = rt.newType("_SpecialGenericAlias", [_BaseGenericAlias], new Map([
            ["__init_subclass__", rootInitSub],
            ["__init__", builtin("__init__", -1, (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                a[0].dict.set("__origin__", a[1]);
                a[0].dict.set("_nparams", a[2] === undefined ? 0n : a[2]);
                a[0].dict.set("_name", kw && kw.has("name") ? kw.get("name") : (a[1] && a[1].name ? a[1].name : ""));
                a[0].dict.set("__args__", tuple([]));
                return null;
            })],
            ["__getitem__", builtin("__getitem__", 2, (a) => {
                const self = a[0], arg = a[1];
                const isSingle = !(arg !== null && typeof arg === "object" && arg.cls === T.tuple);
                if (isSingle) {
                    const cache = self._subCache;
                    if (cache !== undefined) {
                        const hit = cache.get(arg);
                        if (hit !== undefined) return hit;
                    }
                }
                const res = { cls: _GenericAlias, dict: new Map([["__origin__", self.dict.get("__origin__")], ["__args__", isSingle ? tuple([arg]) : arg], ["__parameters__", tuple([])], ["_name", self.dict.get("_name")]]) };
                if (isSingle) {
                    let cache = self._subCache;
                    if (cache === undefined) cache = self._subCache = new Map();
                    if (cache.size < 256) cache.set(arg, res);
                }
                return res;
            })],
            ["copy_with", builtin("copy_with", 2, (a) => ({ cls: _GenericAlias, dict: new Map([["__origin__", a[0].dict.get("__origin__")], ["__args__", a[1] !== null && typeof a[1] === "object" && a[1].cls === T.tuple ? a[1] : tuple([a[1]])], ["__parameters__", tuple([])], ["_name", a[0].dict.get("_name")]]) }))],
            ["__repr__", builtin("__repr__", 1, (a) => "typing." + (a[0].dict.get("_name") || ""))],
        ]), "typing");
        _SpecialGenericAlias.dict.get("__init__").kwnames = true;
        const _ConcatenateGenericAlias = rt.newType("_ConcatenateGenericAlias", [_GenericAlias], new Map([["__init_subclass__", rootInitSub]]), "typing");
        const _AnnotatedAlias = rt.newType("_AnnotatedAlias", [_GenericAlias], new Map([["__init_subclass__", rootInitSub]]), "typing");
        const _UnionGenericAlias = rt.newType("_UnionGenericAlias", [_GenericAlias], new Map([["__init_subclass__", rootInitSub]]), "typing");
        const ForwardRef = rt.newType("ForwardRef", [_Final], new Map([
            ["__slots__", tuple(["__forward_arg__", "__forward_code__", "__forward_evaluated__", "__forward_value__", "__forward_is_argument__", "__forward_is_class__", "__forward_module__"])],
            ["__init_subclass__", rootInitSub],
            ["__init__", builtin("__init__", -1, (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                a[0].dict.set("__forward_arg__", needStr(a[1]));
                a[0].dict.set("__forward_is_argument__", kw && kw.has("is_argument") ? truth(kw.get("is_argument")) : true);
                a[0].dict.set("__forward_is_class__", kw && kw.has("is_class") ? truth(kw.get("is_class")) : false);
                a[0].dict.set("__forward_module__", kw && kw.has("module") ? kw.get("module") : null);
                a[0].dict.set("__forward_evaluated__", false);
                a[0].dict.set("__forward_value__", null);
                return null;
            })],
            ["__repr__", builtin("__repr__", 1, (a) => "ForwardRef(" + repr(a[0].dict.get("__forward_arg__")) + ")")],
        ]), "typing");
        ForwardRef.dict.get("__init__").kwnames = true;
        const NewType = rt.newType("NewType", [], new Map([
            ["__init__", builtin("__init__", 3, (a) => { a[0].dict.set("__name__", needStr(a[1])); a[0].dict.set("__supertype__", a[2]); return null; })],
            ["__call__", builtin("__call__", 2, (a) => a[1])],
            ["__or__", builtin("__or__", 2, (a) => rt.makeUnion(a[0], a[1]))],
            ["__ror__", builtin("__ror__", 2, (a) => rt.makeUnion(a[1], a[0]))],
            ["__repr__", builtin("__repr__", 1, (a) => "typing." + a[0].dict.get("__name__"))],
        ]), "typing");
        const ParamSpecArgs = rt.newType("ParamSpecArgs", [], new Map([["__init__", builtin("__init__", 2, (a) => { a[0].dict.set("__origin__", a[1]); return null; })]]), "typing");
        const ParamSpecKwargs = rt.newType("ParamSpecKwargs", [], new Map([["__init__", builtin("__init__", 2, (a) => { a[0].dict.set("__origin__", a[1]); return null; })]]), "typing");
        const EXCLUDED_ATTRIBUTES = ["__abstractmethods__", "__annotations__", "__weakref__", "_is_protocol", "_is_runtime_protocol", "__dict__", "__args__", "__slots__", "_get_protocol_attrs", "__next_in_mro__", "__parameters__", "__origin__", "__orig_bases__", "__extra__", "__tree_hash__", "__doc__", "__subclasshook__", "__init__", "__new__", "__module__", "_MutableMapping__marker", "_gorg"];
        const excludedSet = new Set(EXCLUDED_ATTRIBUTES);
        const getProtoMembers = (cls) => {
            const out = new Set();
            for (const c of cls.mro) {
                if (c === rt.ObjectType || c === rt.Generic || c === Protocol) continue;
                for (const k of c.dict.keys()) { if (!excludedSet.has(k) && !k.startsWith("__")) out.add(k); }
                const ann = c.dict.get("__annotations__");
                if (ann !== undefined && ann !== null && isInstance(ann, T.dict)) {
                    for (const [k] of rt.dictEntryList(ann)) { if (typeof k === "string" && !excludedSet.has(k) && !k.startsWith("__")) out.add(k); }
                }
            }
            return out;
        };
        const _ProtocolMeta = rt.newType("_ProtocolMeta", [ABCMeta], new Map(), "typing");
        _ProtocolMeta.dict.set("__new__", { cls: T.staticmethod, func: builtin("__new__", -1, (a) => {
            const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
            const cls = rt.makeClass(a[0], a[1], a[2], a[3], kw);
            cls.isABC = true;
            cls._abc_registry = new Set();
            const directProto = cls.bases.some((b) => b === Protocol);
            cls.dict.set("_is_protocol", directProto);
            cls.dict.set("_is_runtime_protocol", false);
            return cls;
        }) });
        _ProtocolMeta.dict.get("__new__").func.kwnames = true;
        _ProtocolMeta.dict.set("__instancecheck__", builtin("__instancecheck__", 2, (a) => {
            const cls = a[0], inst = a[1];
            if (rt.isSubclass(typeOf(inst), cls)) return true;
            if (cls.dict.get("_is_protocol") && cls.dict.get("_is_runtime_protocol")) {
                for (const m of getProtoMembers(cls)) {
                    if (rt.getattr(inst, m, rt.MISSING_ATTR) === rt.MISSING_ATTR) return false;
                }
                return true;
            }
            return false;
        }));
        _ProtocolMeta.dict.set("__subclasscheck__", builtin("__subclasscheck__", 2, (a) => {
            const cls = a[0], sub = a[1];
            if (!isType(sub)) fail(E.TypeError, "issubclass() arg 1 must be a class");
            if (rt.isSubclass(sub, cls)) return true;
            if (cls.dict.get("_is_protocol") && cls.dict.get("_is_runtime_protocol")) {
                for (const m of getProtoMembers(cls)) {
                    if (rt.lookupType(sub, m) === undefined) return false;
                }
                return true;
            }
            return false;
        }));
        const Protocol = rt.newType("Protocol", [rt.Generic], new Map([
            ["__class_getitem__", { cls: T.classmethod, func: builtin("__class_getitem__", 2, (a) => rt.makeGenericAlias(a[0], a[1])) }],
            ["_is_protocol", true],
            ["_is_runtime_protocol", false],
        ]), "typing");
        Protocol.cls = _ProtocolMeta;
        Protocol.isABC = true;
        Protocol._abc_registry = new Set();
        const any = pyClass("_Alias", "typing", {
            __getitem__: (a) => {
                const self = a[0], arg = a[1];
                const isSingle = !(arg !== null && typeof arg === "object" && arg.cls === T.tuple);
                if (isSingle) {
                    const cache = self._subCache;
                    if (cache !== undefined) {
                        const hit = cache.get(arg);
                        if (hit !== undefined) return hit;
                    }
                }
                const res = { cls: self.cls, dict: new Map([["name", self.dict.get("name")], ["__origin__", self.dict.get("__origin__")], ["args", isSingle ? [arg] : arg.items]]) };
                if (isSingle) {
                    let cache = self._subCache;
                    if (cache === undefined) cache = self._subCache = new Map();
                    if (cache.size < 256) cache.set(arg, res);
                }
                return res;
            },
            __mro_entries__: (a) => { const o = a[0].dict.get("__origin__"); return isType(o) ? tuple([o]) : tuple([rt.Generic]); },
            __instancecheck__: (a) => { const o = a[0].dict.get("__origin__"); if (isType(o) && a[0].dict.get("args") === undefined) return rt.isinstanceCheck(a[1], o); fail(E.TypeError, "Subscripted generics cannot be used with class and instance checks"); },
            __subclasscheck__: (a) => { const o = a[0].dict.get("__origin__"); if (isType(o) && a[0].dict.get("args") === undefined) return rt.call(rt.builtins.get("issubclass"), [a[1], o], null); fail(E.TypeError, "Subscripted generics cannot be used with class and instance checks"); },
            __repr__: (a) => { const args = a[0].dict.get("args"); return "typing." + a[0].dict.get("name") + (args === undefined ? "" : "[" + args.map(typeName).join(", ") + "]"); },
            __call__: (a) => a[1] === undefined ? null : a[1], __or__: (a) => a[0], __ror__: (a) => a[0] });
        const colMod = R.import("collections", null);
        const colAbc = colMod.globals.get("abc").globals;
        const ORIGINS = new Map([
            ["List", T.list], ["Dict", T.dict], ["Set", T.set], ["FrozenSet", T.frozenset], ["Tuple", T.tuple],
            ["Deque", colMod.globals.get("deque")], ["DefaultDict", colMod.globals.get("defaultdict")],
            ["OrderedDict", colMod.globals.get("OrderedDict")], ["Counter", colMod.globals.get("Counter")],
            ["ChainMap", colMod.globals.get("ChainMap")], ["AbstractSet", colAbc.get("Set")],
        ]);
        for (const n of ["Iterable", "Iterator", "Generator", "Sequence", "MutableSequence", "Mapping", "MutableMapping", "MutableSet", "Callable", "Hashable", "Sized", "Collection", "Container", "Awaitable", "Coroutine", "AsyncIterator", "AsyncIterable", "KeysView", "ValuesView", "ItemsView", "Reversible", "ByteString"]) ORIGINS.set(n, colAbc.get(n));
        for (const n of ["Any", "List", "Dict", "Set", "FrozenSet", "Tuple", "Optional", "Union", "Callable", "Iterable", "Iterator", "Generator", "Sequence", "Mapping", "MutableMapping", "Type", "ClassVar", "Final", "Literal", "NoReturn", "Hashable", "Sized", "Collection", "Awaitable", "Coroutine", "AsyncIterator", "AsyncIterable", "Deque", "DefaultDict", "OrderedDict", "Counter", "ChainMap", "Text", "IO", "TextIO", "BinaryIO", "Pattern", "Match", "SupportsInt", "SupportsFloat", "SupportsComplex", "SupportsBytes", "SupportsIndex", "SupportsAbs", "SupportsRound", "Annotated", "Self", "TypeAlias", "Concatenate", "TypeGuard", "TypeIs", "TypeForm", "Never", "LiteralString", "Required", "NotRequired", "ReadOnly", "NoExtraItems", "Doc", "AnyStr", "ContextManager", "Container", "MutableSequence", "MutableSet", "AbstractSet", "KeysView", "ValuesView", "ItemsView", "Reversible", "ByteString"]) {
            const origin = ORIGINS.get(n);
            const m = new Map([["name", n]]);
            if (origin !== undefined) m.set("__origin__", origin);
            const o = { cls: any, dict: m };
            g.set(n, o);
        }
        g.set("TYPE_CHECKING", false);
        g.set("TypeVar", rt.TypeVar);
        g.set("ParamSpec", rt.ParamSpec);
        g.set("TypeVarTuple", rt.TypeVarTuple);
        g.set("TypeAliasType", rt.TypeAliasType);
        g.set("Generic", rt.Generic);
        g.set("NoDefault", rt.NoDefault);
        g.set("Unpack", rt.Unpack);
        g.set("Protocol", Protocol);
        g.set("_ProtocolMeta", _ProtocolMeta);
        g.set("_Final", _Final);
        g.set("_SpecialForm", _SpecialForm);
        g.set("_BaseGenericAlias", _BaseGenericAlias);
        g.set("_GenericAlias", _GenericAlias);
        g.set("_SpecialGenericAlias", _SpecialGenericAlias);
        g.set("_ConcatenateGenericAlias", _ConcatenateGenericAlias);
        g.set("_AnnotatedAlias", _AnnotatedAlias);
        g.set("_UnionGenericAlias", _UnionGenericAlias);
        g.set("ForwardRef", ForwardRef);
        g.set("NewType", NewType);
        g.set("ParamSpecArgs", ParamSpecArgs);
        g.set("ParamSpecKwargs", ParamSpecKwargs);
        g.set("EXCLUDED_ATTRIBUTES", tuple(EXCLUDED_ATTRIBUTES));
        g.set("_prohibited", tuple(["__new__", "__init__", "__slots__", "__getnewargs__", "_fields", "_field_defaults", "_make", "_replace", "_asdict", "_source"]));
        g.set("_ASSERT_NEVER_REPR_MAX_LENGTH", 100n);
        fn(g, "cast", 2, (a) => a[1]);
        {
            const fCast = g.get("cast");
            let CAST_STRM = null;
            try { CAST_STRM = typeof __zipp_py_strm === "function" ? __zipp_py_strm : null; } catch (e) { CAST_STRM = null; }
            if (CAST_STRM !== null) {
                fCast.strop = 47;
                fCast.j2 = (t, x) => x;
                fCast.c2 = CAST_STRM;
            }
        }
        fn(g, "overload", 1, (a) => a[0]);
        fn(g, "_overload_dummy", -1, () => fail(E.NotImplementedError, "You should not call an overloaded function."));
        fn(g, "get_overloads", 1, () => list([]));
        fn(g, "clear_overloads", 0, () => null);
        fn(g, "final", 1, (a) => a[0]);
        fn(g, "override", 1, (a) => a[0]);
        fn(g, "no_type_check", 1, (a) => a[0]);
        fn(g, "disjoint_base", 1, (a) => a[0]);
        fn(g, "dataclass_transform", -1, () => builtin("dataclass_transform", 1, (b) => b[0]));
        g.get("dataclass_transform").kwnames = true;
        fn(g, "assert_type", 2, (a) => a[0]);
        fn(g, "assert_never", 1, (a) => fail(E.AssertionError, "Expected code to be unreachable, but got: " + repr(a[0])));
        fn(g, "reveal_type", 1, (a) => a[0]);
        fn(g, "get_type_hints", -1, () => dict());
        fn(g, "runtime_checkable", 1, (a) => { const c = a[0]; if (isType(c)) c.dict.set("_is_runtime_protocol", true); return c; });
        fn(g, "is_protocol", 1, (a) => isType(a[0]) && Boolean(a[0].dict.get("_is_protocol")));
        fn(g, "get_protocol_members", 1, (a) => { const s = rt.set(T.frozenset); if (isType(a[0])) for (const m of getProtoMembers(a[0])) rt.setAdd(s, m); return s; });
        fn(g, "get_origin", 1, (a) => { const t = a[0]; if (t === null || typeof t !== "object") return null; if (t.cls === rt.UnionType) return rt.UnionType; if (t.dict && t.dict.has("__origin__")) return t.dict.get("__origin__"); return null; });
        fn(g, "get_args", 1, (a) => { const t = a[0]; if (t !== null && typeof t === "object" && t.dict && t.dict.has("__args__")) { const v = t.dict.get("__args__"); return v && v.cls === T.tuple ? v : tuple(Array.isArray(v) ? v : [v]); } return tuple([]); });
        fn(g, "_type_check", -1, (a) => a[0]);
        g.get("_type_check").kwnames = true;
        fn(g, "_type_repr", 1, (a) => typeName(a[0]));
        fn(g, "_type_convert", -1, (a) => a[0]);
        g.get("_type_convert").kwnames = true;
        fn(g, "_tp_cache", -1, (a) => (a.length === 1 && typeof a[0] === "object" && a[0] !== null && (a[0].cls === T.function || a[0].cls === T.builtin_function_or_method)) ? a[0] : builtin("_tp_cache", 1, (b) => b[0]));
        g.get("_tp_cache").kwnames = true;
        fn(g, "_eval_type", -1, (a) => a[0]);
        g.get("_eval_type").kwnames = true;
        fn(g, "evaluate_forward_ref", -1, (a) => a[0]);
        g.get("evaluate_forward_ref").kwnames = true;
        fn(g, "_collect_parameters", 1, () => tuple([]));
        fn(g, "_collect_type_vars", 1, () => tuple([]));
        fn(g, "_deduplicate", 1, (a) => list(drain(a[0])));
        fn(g, "_is_param_expr", 1, () => false);
        fn(g, "_check_generic", -1, () => null);
        fn(g, "_get_defaults", 1, () => dict());
        fn(g, "_generic_class_getitem", 2, (a) => rt.makeGenericAlias(a[0], a[1]));
        // typing.NamedTuple: the functional form is collections.namedtuple;
        // a subclass turns its annotations into fields (class attributes
        // with the same names are the defaults) and keeps its own methods.
        // collections loads on first use: the program need not import it.
        const namedtuple = (name, fields, defaults) => call(R.import("collections", null).globals.get("namedtuple"), [name, list(fields)], new Map([["defaults", list(defaults)]]));
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
        const _TypedDictMeta = rt.newType("_TypedDictMeta", [rt.TypeType], new Map(), "typing");
        const TD = rt.newType("TypedDict", [], new Map(), "typing");
        TD.cls = _TypedDictMeta;
        TD.dict.set("__new__", { cls: T.staticmethod, func: builtin("__new__", -1, (a) => { const kwm = a[a.length - 1] instanceof Map ? a.pop() : null; const d = a[1] !== undefined ? rt.dictCopy(rt.asDict(a[1])) : dict(); if (kwm) for (const [k, v] of kwm) dictSet(d, k, v); return d; }) });
        TD.dict.get("__new__").func.kwnames = true;
        g.set("_TypedDictMeta", _TypedDictMeta);
        g.set("TypedDict", TD);
        fn(g, "is_typeddict", 1, (a) => isType(a[0]) && a[0].cls === _TypedDictMeta);
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
        // state 0: the first item starts the total; 1: yield `initial` first; 2: accumulate.
        fn(g, "accumulate", -1, (a) => { const kw = kwOf(a, ["initial", "func"]); const it = iter(a[0]); const f = a[1] !== undefined ? a[1] : kwget(kw, "func", null); let acc = kwget(kw, "initial", null); let state = acc === null ? 0 : 1; return gen(() => { if (state === 1) { state = 2; return acc; } const v = fornext(it); if (v === STOP) return STOP; if (state === 0) { state = 2; acc = v; return acc; } acc = f === null ? R.binop("add", acc, v) : call(f, [acc, v], null); return acc; }); });
        g.get("accumulate").kwnames = true;
        fn(g, "groupby", 2, (a) => { const it = iter(a[0]); const key = a[1] === undefined ? null : a[1]; let pending = fornext(it); return gen(() => { if (pending === STOP) return STOP; const k = key === null ? pending : call(key, [pending], null); const group = []; while (pending !== STOP && eq(key === null ? pending : call(key, [pending], null), k)) { group.push(pending); pending = fornext(it); } return tuple([k, iter(list(group))]); }); }, 1);
        fn(g, "takewhile", 2, (a) => { const it = iter(a[1]); let done = false; return gen(() => { if (done) return STOP; const v = fornext(it); if (v === STOP || !truth(call(a[0], [v], null))) { done = true; return STOP; } return v; }); });
        fn(g, "dropwhile", 2, (a) => { const it = iter(a[1]); let dropping = true; return gen(() => { for (;;) { const v = fornext(it); if (v === STOP) return STOP; if (dropping && truth(call(a[0], [v], null))) continue; dropping = false; return v; } }); });
        fn(g, "filterfalse", 2, (a) => { const it = iter(a[1]); return gen(() => { for (;;) { const v = fornext(it); if (v === STOP) return STOP; if (!(a[0] === null ? truth(v) : truth(call(a[0], [v], null)))) return v; } }); });
        fn(g, "starmap", 2, (a) => { const it = iter(a[1]); return gen(() => { const v = fornext(it); return v === STOP ? STOP : call(a[0], drain(v), null); }); });
        fn(g, "tee", 2, (a) => { const items = drain(a[0]); const n = a[1] === undefined ? 2 : Number(needInt(a[1])); return tuple(Array.from({ length: n }, () => iter(list(items)))); }, 1);
        fn(g, "pairwise", 1, (a) => { const items = drain(a[0]); let i = 0; return gen(() => i + 1 < items.length ? tuple([items[i], items[++i]]) : STOP); });
        fnkw(g, "batched", (a) => {
            const kw = kwOf(a, ["strict"]);
            if (a.length !== 2) fail(E.TypeError, "batched expected 2 positional arguments, got " + a.length);
            const items = drain(a[0]);
            const n = Number(needInt(a[1]));
            if (n < 1) fail(E.ValueError, "n must be at least one");
            const strict = truth(kwget(kw, "strict", false));
            let i = 0;
            return gen(() => {
                if (i >= items.length) return STOP;
                const b = items.slice(i, i + n);
                i += n;
                if (strict && b.length < n) fail(E.ValueError, "batched(): incomplete batch");
                return tuple(b);
            });
        });
    });
    // ---- _zipp_gpu: the transport behind zipp_gpu.Graph.submit and Graph.prepare -------------
    // The host-request kinds a program may raise. Anything else never leaves
    // the engine: the embedder's adapters admit exactly this list.
    const GPU_REQUEST_KINDS = ["gpu.execute", "gpu.session.create", "gpu.session.run", "gpu.session.download", "gpu.session.dispose"];
    mod("_zipp_gpu", (g) => {
        const callable = (f) => f !== null && typeof f === "object" && (f.cls === T.function || f.cls === T.method || f.cls === T.builtin_function_or_method);
        fn(g, "hosted", 0, () => rt.hosted === true);
        fn(g, "post", 2, (a) => {
            const program = a[0];
            if (program === null || typeof program !== "object" || program.cls !== T.dict) fail(E.TypeError, "a program is a dict");
            if (!callable(a[1])) fail(E.TypeError, "post() needs a callable");
            return BigInt(rt.postHost("gpu.execute", program, a[1]));
        });
        // request(kind, payload, callback): a session request (`gpu.session.*`).
        fn(g, "request", 3, (a) => {
            const kind = a[0];
            if (typeof kind !== "string" || GPU_REQUEST_KINDS.indexOf(kind) < 0) fail(E.ValueError, "unknown host request kind");
            const payload = a[1];
            if (payload === null || typeof payload !== "object" || payload.cls !== T.dict) fail(E.TypeError, "a payload is a dict");
            if (!callable(a[2])) fail(E.TypeError, "request() needs a callable");
            return BigInt(rt.postHost(kind, payload, a[2]));
        });
        fn(g, "pending", 0, () => BigInt(rt.pendingHostRequests()));
        g.set("KINDS", tuple(GPU_REQUEST_KINDS.slice()));
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
                    if (c === "p") {
                        // A Pascal string: a length byte, then at most count - 1 bytes.
                        const n = Math.min(v.items.length, it.count - 1);
                        for (let k = 0; k < n; k++) bytes[it.offset + 1 + k] = v.items[k];
                        if (it.count > 0) bytes[it.offset] = Math.min(n, 255);
                        continue;
                    }
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
            return rt.bytes(rt.bytesFromU8(bytes));
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
                if (c === "s") { out.push(rt.bytes(rt.bytesFromU8(bytes.slice(it.offset, it.offset + it.count)))); continue; }
                if (c === "p") { const n = Math.min(bytes[it.offset], it.count - 1); out.push(rt.bytes(rt.bytesFromU8(bytes.slice(it.offset + 1, it.offset + 1 + n)))); continue; }
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
        const ALGOS = { sha256: [sha256, 32], sha1: [sha1, 20], md5: [md5, 16], sha512: [rt.sha512, 64] };
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
        fn(g, "import_module", 2, (a) => {
            let name = needStr(a[0]);
            if (name.startsWith(".")) {
                const pkg = a[1] === undefined || a[1] === null ? null : needStr(a[1]);
                if (!pkg) fail(E.TypeError, "the 'package' argument is required to perform a relative import for '" + name + "'");
                let level = 0;
                while (level < name.length && name[level] === ".") level++;
                const rest = name.slice(level);
                const bits = pkg.split(".");
                if (level - 1 > bits.length) fail(E.ImportError, "attempted relative import beyond top-level package");
                const base = bits.slice(0, bits.length - (level - 1));
                name = rest ? (base.length ? base.join(".") + "." + rest : rest) : base.join(".");
            }
            return R.import(name, null);
        }, 1);
        fn(g, "reload", 1, (a) => a[0]);
        fn(g, "invalidate_caches", 0, () => null);
        fn(g, "_exec_source", 4, (a) => { rt.execModuleSource(a[0], needStr(a[1]), needStr(a[2]), needStr(a[3])); return null; });
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
                // A StopIteration from the block comes back as PEP 479's RuntimeError.
                if (stopped(exc) && e !== null && typeof e === "object" && e.cls === E.RuntimeError && e.cause === exc) return false;
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
        const ABCMeta = R.import("abc", null).globals.get("ABCMeta");
        const cgCm = { cls: T.classmethod, func: builtin("__class_getitem__", 2, (a) => rt.makeGenericAlias(a[0], a[1])) };
        const ACM = rt.newType("AbstractContextManager", [], new Map([
            ["__enter__", builtin("__enter__", 1, (a) => a[0])],
            ["__exit__", builtin("__exit__", 4, () => null)],
            ["__class_getitem__", cgCm],
            ["__subclasshook__", { cls: T.classmethod, func: builtin("__subclasshook__", 2, (a) => a[0] === ACM && rt.lookupType(a[1], "__enter__") !== undefined && rt.lookupType(a[1], "__exit__") !== undefined ? true : NOTIMPL) }],
        ]), "contextlib");
        ACM.cls = ABCMeta; ACM.isABC = true; ACM._abc_registry = new Set();
        const AACM = rt.newType("AbstractAsyncContextManager", [], new Map([
            ["__class_getitem__", cgCm],
            ["__subclasshook__", { cls: T.classmethod, func: builtin("__subclasshook__", 2, (a) => a[0] === AACM && rt.lookupType(a[1], "__aenter__") !== undefined && rt.lookupType(a[1], "__aexit__") !== undefined ? true : NOTIMPL) }],
        ]), "contextlib");
        AACM.cls = ABCMeta; AACM.isABC = true; AACM._abc_registry = new Set();
        g.set("AbstractContextManager", ACM);
        g.set("AbstractAsyncContextManager", AACM);
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
        fn(g, "wraps", -1, (a) => builtin("wraps", 1, (b) => { const f = b[0], src = a[0]; if (f !== null && typeof f === "object" && (f.cls === T.function || f.cls === T.builtin_function_or_method)) { const pick = (n, dflt) => { try { return rt.getattr(src, n); } catch (e) { return dflt; } }; f.name = pick("__name__", f.name); f.qualname = pick("__qualname__", f.qualname); f.doc = pick("__doc__", null); if (f.cls === T.function && f.dict === null) f.dict = new Map(); if (f.dict) f.dict.set("__wrapped__", src); } return f; }));
        fn(g, "update_wrapper", 2, (a) => a[0]);
        const CacheInfo = rt.newType("CacheInfo", [T.tuple], new Map(), "functools");
        CacheInfo.dict.set("__repr__", builtin("__repr__", 1, (a) => { const [h, m, mx, c] = a[0].items; return "CacheInfo(hits=" + h + ", misses=" + m + ", maxsize=" + repr(mx) + ", currsize=" + c + ")"; }));
        for (const [i, n] of ["hits", "misses", "maxsize", "currsize"].entries()) CacheInfo.dict.set(n, { cls: T.property, fget: builtin(n, 1, (a) => a[0].items[i]), fset: null, fdel: null, doc: null });
        const cache = (f, maxsize) => {
            const memo = new Map(); let hits = 0, misses = 0;
            const maxN = maxsize === null ? -1 : Number(maxsize);
            const isPyFn = f !== null && typeof f === "object" && f.cls === T.function;
            const store = (key, v) => {
                if (maxN !== 0) {
                    memo.set(key, v);
                    if (maxN > 0 && memo.size > maxN) memo.delete(memo.keys().next().value);
                }
                return v;
            };
            const hit = (key) => {
                const v = memo.get(key);
                if (v === undefined) return undefined;
                hits++;
                if (maxN > 0 && memo.size > 1) { memo.delete(key); memo.set(key, v); }
                return v;
            };
            const posKey = (b) => {
                const n = b.length;
                if (n === 0) return "\x00";
                if (n === 1 && typeof b[0] === "string") return "\x01" + b[0];
                if (n === 2 && typeof b[0] === "string" && typeof b[1] === "string") return "\x02" + b[0].length + ":" + b[0] + b[1];
                if (n === 3 && typeof b[0] === "string" && typeof b[1] === "string" && typeof b[2] === "string") return "\x03" + b[0].length + ":" + b[1].length + ":" + b[0] + b[1] + b[2];
                return rt.keyStr(tuple(b));
            };
            const w = builtin(f.name || "cached", -1, (b) => {
                const kw = b.length > 0 && b[b.length - 1] instanceof Map ? b.pop() : null;
                if (maxN === 0) { misses++; return call(f, b, kw); }
                const key = kw === null ? posKey(b) : rt.keyStr(tuple(b)) + "|" + [...kw].map(([k, v]) => k + "=" + rt.keyStr(v)).join(",");
                const cv = hit(key);
                if (cv !== undefined) return cv;
                misses++;
                return store(key, call(f, b, kw));
            });
            w.c0 = function () {
                if (maxN === 0) { misses++; return isPyFn && f.c0 !== undefined ? f.c0() : call(f, [], null); }
                const cv = hit("\x00");
                if (cv !== undefined) return cv;
                misses++;
                return store("\x00", isPyFn && f.c0 !== undefined ? f.c0() : call(f, [], null));
            };
            w.c1 = function (a0) {
                if (maxN === 0) { misses++; return isPyFn && f.c1 !== undefined ? f.c1(a0) : call(f, [a0], null); }
                const key = typeof a0 === "string" ? "\x01" + a0 : rt.keyStr(tuple([a0]));
                const cv = hit(key);
                if (cv !== undefined) return cv;
                misses++;
                return store(key, isPyFn && f.c1 !== undefined ? f.c1(a0) : call(f, [a0], null));
            };
            w.c2 = function (a0, a1) {
                if (maxN === 0) { misses++; return isPyFn && f.c2 !== undefined ? f.c2(a0, a1) : call(f, [a0, a1], null); }
                const key = typeof a0 === "string" && typeof a1 === "string" ? "\x02" + a0.length + ":" + a0 + a1 : rt.keyStr(tuple([a0, a1]));
                const cv = hit(key);
                if (cv !== undefined) return cv;
                misses++;
                return store(key, isPyFn && f.c2 !== undefined ? f.c2(a0, a1) : call(f, [a0, a1], null));
            };
            w.c3 = function (a0, a1, a2) {
                if (maxN === 0) { misses++; return isPyFn && f.c3 !== undefined ? f.c3(a0, a1, a2) : call(f, [a0, a1, a2], null); }
                const key = typeof a0 === "string" && typeof a1 === "string" && typeof a2 === "string" ? "\x03" + a0.length + ":" + a1.length + ":" + a0 + a1 + a2 : rt.keyStr(tuple([a0, a1, a2]));
                const cv = hit(key);
                if (cv !== undefined) return cv;
                misses++;
                return store(key, isPyFn && f.c3 !== undefined ? f.c3(a0, a1, a2) : call(f, [a0, a1, a2], null));
            };
            w.kwnames = true;
            w.dict = new Map([
                ["cache_clear", builtin("cache_clear", 0, () => { memo.clear(); hits = 0; misses = 0; return null; })],
                ["cache_info", builtin("cache_info", 0, () => ({ cls: CacheInfo, items: [BigInt(hits), BigInt(misses), maxsize, BigInt(memo.size)], dict: new Map() }))],
                ["__wrapped__", f]]);
            return w;
        };
        fn(g, "lru_cache", -1, (a) => { const kw = kwOf(a, ["maxsize", "typed"]); if (a.length === 1 && a[0] !== null && typeof a[0] === "object" && (a[0].cls === T.function || a[0].cls === T.builtin_function_or_method)) return cache(a[0], 128n); let maxsize = a.length ? a[0] : kwget(kw, "maxsize", 128n); if (maxsize !== null) maxsize = needInt(maxsize); return builtin("lru_cache", 1, (b) => cache(b[0], maxsize)); });
        g.get("lru_cache").kwnames = true;
        fn(g, "cache", 1, (a) => cache(a[0], null));
        // Non-data descriptor: computes once, caches in instance.__dict__[attrname],
        // and allows overriding or `del obj.prop` to recompute.
        const CachedProp = rt.newType("cached_property", [rt.ObjectType], new Map(), "functools");
        rt.constructors.set(CachedProp, (a) => {
            if (a.length !== 1) fail(E.TypeError, "cached_property() takes 1 positional argument");
            const f = a[0];
            return { cls: CachedProp, dict: new Map([["func", f], ["attrname", null], ["__doc__", f !== null && typeof f === "object" && f.doc !== undefined ? f.doc : null]]) };
        });
        CachedProp.dict.set("__set_name__", builtin("__set_name__", 3, (a) => {
            const self = a[0], name = a[2];
            const prev = self.dict.get("attrname");
            if (prev === null) self.dict.set("attrname", name);
            else if (prev !== name) fail(E.TypeError, "Cannot assign the same cached_property to two different names (" + repr(prev) + " and " + repr(name) + ").");
            return null;
        }));
        CachedProp.dict.set("__get__", builtin("__get__", 3, (a) => {
            const self = a[0], inst = a[1];
            if (inst === null) return self;
            const name = self.dict.get("attrname");
            if (name === null) fail(E.TypeError, "Cannot use cached_property instance without calling __set_name__ on it.");
            if (inst.dict === undefined || inst.dict === null) fail(E.TypeError, "No '__dict__' attribute on '" + typeOf(inst).name + "' instance to cache '" + name + "' property.");
            if (inst.dict.has(name)) return inst.dict.get(name);
            const v = call(self.dict.get("func"), [inst], null);
            inst.dict.set(name, v);
            return v;
        }, 2));
        g.set("cached_property", CachedProp);
        fn(g, "total_ordering", 1, (a) => { const c = a[0]; rt.bumpEpoch(); const has = (n) => c.dict.has(n); const lt = (x, y) => truth(call(rt.getattr(x, "__lt__"), [y], null)); if (has("__lt__")) { if (!has("__gt__")) c.dict.set("__gt__", builtin("__gt__", 2, (b) => !lt(b[0], b[1]) && !eq(b[0], b[1]))); if (!has("__le__")) c.dict.set("__le__", builtin("__le__", 2, (b) => lt(b[0], b[1]) || eq(b[0], b[1]))); if (!has("__ge__")) c.dict.set("__ge__", builtin("__ge__", 2, (b) => !lt(b[0], b[1]))); } return c; });
        fn(g, "cmp_to_key", 1, (a) => { const K = pyClass("K", "functools", { __init__: (b) => { b[0].dict.set("obj", b[1]); return null; }, __lt__: (b) => asInt(call(a[0], [b[0].dict.get("obj"), b[1].dict.get("obj")], null)) < 0n, __eq__: (b) => asInt(call(a[0], [b[0].dict.get("obj"), b[1].dict.get("obj")], null)) === 0n }); return K; });
        fn(g, "singledispatch", 1, (a) => {
            const base = a[0];
            const reg = new Map([[rt.ObjectType, base]]);
            const dispatch = (cls) => {
                for (const c of cls.mro) {
                    const hit = reg.get(c);
                    if (hit !== undefined) return hit;
                }
                for (const [k, impl] of reg) {
                    if (k !== rt.ObjectType && isType(k) && rt.isSubclass(cls, k)) return impl;
                }
                return base;
            };
            const registerImpl = (cls, func) => {
                if (cls !== null && typeof cls === "object" && cls.cls === rt.UnionType) {
                    for (const t of cls.dict.get("__args__").items) reg.set(t === null ? T.NoneType : t, func);
                } else {
                    reg.set(cls, func);
                }
                return func;
            };
            const register = builtin("register", -1, (b) => {
                if (b.length === 1) {
                    const first = b[0];
                    if (isType(first) || (first !== null && typeof first === "object" && first.cls === rt.UnionType)) {
                        return builtin("register", 1, (c) => registerImpl(first, c[0]));
                    }
                    // @fn.register on a function with a type annotation on its first parameter.
                    const ann = first !== null && typeof first === "object" ? first.annotations : undefined;
                    if (ann instanceof Map && first.argnames && first.argnames.length > 0) {
                        const t = ann.get(first.argnames[0]);
                        if (t !== undefined) return registerImpl(t, first);
                    }
                    fail(E.TypeError, "Invalid first argument to `register()`. " + repr(first) + " is not a class or union type.");
                }
                if (b.length === 2) return registerImpl(b[0], b[1]);
                fail(E.TypeError, "register() takes 1 or 2 arguments");
            });
            const wrapper = builtin(base.name || "singledispatch", -1, (b) => {
                const kw = b[b.length - 1] instanceof Map ? b.pop() : null;
                if (!b.length) fail(E.TypeError, (base.name || "wrapper") + " requires at least 1 positional argument");
                return call(dispatch(typeOf(b[0])), b, kw);
            });
            wrapper.kwnames = true;
            wrapper.dict = new Map([
                ["register", register],
                ["dispatch", builtin("dispatch", 1, (b) => dispatch(b[0]))],
                ["registry", { cls: T.property, fget: builtin("registry", 1, () => { const d = dict(); for (const [k, v] of reg) dictSet(d, k, v); return rt.mappingProxy(d); }), fset: null, fdel: null, doc: null }],
                ["__wrapped__", base],
            ]);
            // Expose registry directly as a mapping proxy (functions don't run property descriptors on their own dict).
            wrapper.dict.set("registry", (() => {
                const proxy = { cls: rt.ObjectType, dict: new Map() };
                // Live view backed by `reg`.
                const View = pyClass("mappingproxy", "types", {
                    __getitem__: (c) => { const v = reg.get(c[1]); if (v === undefined) throw rt.makeExc(E.KeyError, [c[1]]); return v; },
                    __contains__: (c) => reg.has(c[1]),
                    __len__: () => BigInt(reg.size),
                    __iter__: () => iter(list(Array.from(reg.keys()))),
                    keys: () => list(Array.from(reg.keys())),
                    items: () => list(Array.from(reg.entries()).map((e) => tuple(e))),
                    get: (c) => { const v = reg.get(c[1]); return v === undefined ? (c[2] === undefined ? null : c[2]) : v; },
                });
                return { cls: View, dict: new Map() };
            })());
            return wrapper;
        });
    });
    mod("collections", (g) => {
        const DefaultDict = rt.newType("defaultdict", [T.dict], new Map(), "collections");
        DefaultDict.dict.set("__init__", (() => {
            const f = builtin("__init__", -1, (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; a[0].dict.set("default_factory", a[1] === undefined ? null : a[1]); if (a[2] !== undefined) for (const [k, v] of dictEntries(rt.asDict(a[2]))) dictSet(a[0], k, v); if (kw) for (const [k, v] of kw) dictSet(a[0], k, v); return null; });
            f.kwnames = true;
            f.c1 = (self) => { self.dict.set("default_factory", null); return null; };
            f.c2 = (self, factory) => { self.dict.set("default_factory", factory); return null; };
            return f;
        })());
        DefaultDict.dict.set("__missing__", builtin("__missing__", 2, (a) => { const f = a[0].dict.get("default_factory"); if (f === null || f === undefined) throw rt.makeExc(E.KeyError, [a[1]]); const v = call(f, [], null); dictSet(a[0], a[1], v); return v; }));
        DefaultDict.dict.set("__repr__", builtin("__repr__", 1, (a) => "defaultdict(" + repr(a[0].dict.get("default_factory") === undefined ? null : a[0].dict.get("default_factory")) + ", " + rt.baseRepr(a[0]) + ")"));
        DefaultDict.dict.set("default_factory", { cls: T.property, fget: builtin("default_factory", 1, (a) => { const f = a[0].dict.get("default_factory"); return f === undefined ? null : f; }), fset: null, fdel: null, doc: null });
        g.set("defaultdict", DefaultDict);
        const OrderedDict = rt.newType("OrderedDict", [T.dict], new Map(), "collections");
        OrderedDict.dict.set("move_to_end", builtin("move_to_end", -1, (a) => { const kw = kwOf(a, ["last"]); const last = a[2] !== undefined ? truth(a[2]) : truth(kwget(kw, "last", true)); const v = dictGet(a[0], a[1]); if (v === undefined) throw rt.makeExc(E.KeyError, [a[1]]); rt.dictDel(a[0], a[1]); if (last) dictSet(a[0], a[1], v); else { const entries = rt.dictEntryList(a[0]); rt.dictClear(a[0]); dictSet(a[0], a[1], v); for (const [k, x] of entries) dictSet(a[0], k, x); } return null; }));
        OrderedDict.dict.get("move_to_end").kwnames = true;
        OrderedDict.dict.set("__repr__", builtin("__repr__", 1, (a) => a[0].size ? "OrderedDict(" + rt.baseRepr(a[0]) + ")" : "OrderedDict()"));
        OrderedDict.dict.set("popitem", (() => { const f = builtin("popitem", -1, (a) => { const kw = kwOf(a, ["last"]); const last = a[1] !== undefined ? truth(a[1]) : truth(kwget(kw, "last", true)); const e = rt.dictPopItem(a[0], !last); if (e === undefined) fail(E.KeyError, "dictionary is empty"); return tuple([e[0], e[1]]); }); f.kwnames = true; return f; })());
        // Two OrderedDicts are equal only in the same order; against a plain
        // dict the order does not matter.
        OrderedDict.dict.set("__eq__", builtin("__eq__", 2, (a) => {
            const x = a[0], y = a[1];
            if (y === null || typeof y !== "object" || y.map === undefined || !isInstance(y, T.dict)) return NOTIMPL;
            if (!rt.dictEq(x, y)) return false;
            if (!isInstance(y, OrderedDict)) return true;
            const kx = rt.dictEntryList(x), ky = rt.dictEntryList(y);
            for (let i = 0; i < kx.length; i++) if (!eq(kx[i][0], ky[i][0])) return false;
            return true;
        }));
        g.set("OrderedDict", OrderedDict);
        const Counter = rt.newType("Counter", [T.dict], new Map(), "collections");
        Counter.dict.set("__init__", (() => { const f = builtin("__init__", -1, (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; if (a[1] !== undefined && a[1] !== null) { if (a[1].cls === T.dict || isInstance(a[1], T.dict)) for (const [k, v] of dictEntries(a[1])) dictSet(a[0], k, v); else for (const x of drain(a[1])) { const c = dictGet(a[0], x); dictSet(a[0], x, c === undefined ? 1n : R.binop("add", c, 1n)); } } if (kw) for (const [k, v] of kw) dictSet(a[0], k, v); return null; }); f.kwnames = true; return f; })());
        Counter.dict.set("__missing__", builtin("__missing__", 2, () => 0n));
        Counter.dict.set("most_common", builtin("most_common", 2, (a) => { const items = rt.dictEntryList(a[0]).map((e) => tuple([e[0], e[1]])); const sorted = rt.sortItems(items, builtin("k", 1, (b) => b[0].items[1]), true); return list(a[1] === undefined || a[1] === null ? sorted : sorted.slice(0, Number(needInt(a[1])))); }, 1));
        Counter.dict.set("elements", builtin("elements", 1, (a) => { const out = []; for (const [k, v] of dictEntries(a[0])) for (let i = 0n; i < asInt(v); i++) out.push(k); return iter(list(out)); }));
        Counter.dict.set("update", (() => { const f = builtin("update", -1, (a) => { const kw = a[a.length - 1] instanceof Map ? a.pop() : null; if (a[1] !== undefined) { if (a[1].cls === T.dict || isInstance(a[1], T.dict)) for (const [k, v] of dictEntries(a[1])) dictSet(a[0], k, R.binop("add", dictGet(a[0], k) || 0n, v)); else for (const x of drain(a[1])) dictSet(a[0], x, R.binop("add", dictGet(a[0], x) || 0n, 1n)); } if (kw) for (const [k, v] of kw) dictSet(a[0], k, R.binop("add", dictGet(a[0], k) || 0n, v)); return null; }); f.kwnames = true; return f; })());
        Counter.dict.set("subtract", builtin("subtract", 2, (a) => { const src = a[1].cls === T.dict || isInstance(a[1], T.dict) ? rt.dictEntryList(a[1]) : drain(a[1]).map((x) => [x, 1n]); for (const [k, v] of src) dictSet(a[0], k, R.binop("sub", dictGet(a[0], k) || 0n, v)); return null; }));
        Counter.dict.set("total", builtin("total", 1, (a) => { let t = 0n; for (const [, v] of dictEntries(a[0])) t = R.binop("add", t, v); return t; }));
        Counter.dict.set("__repr__", builtin("__repr__", 1, (a) => { if (!a[0].size) return "Counter()"; const items = rt.sortItems(rt.dictEntryList(a[0]).map((e) => tuple([e[0], e[1]])), builtin("k", 1, (b) => b[0].items[1]), true); return "Counter({" + items.map((t) => repr(t.items[0]) + ": " + repr(t.items[1])).join(", ") + "})"; }));
        Counter.dict.set("__pos__", builtin("__pos__", 1, (a) => { const out = rt.construct(Counter, [], null); for (const [k, v] of dictEntries(a[0])) if (cmp("gt", v, 0n)) dictSet(out, k, v); return out; }));
        Counter.dict.set("__neg__", builtin("__neg__", 1, (a) => { const out = rt.construct(Counter, [], null); for (const [k, v] of dictEntries(a[0])) if (cmp("lt", v, 0n)) dictSet(out, k, R.unop("neg", v)); return out; }));
        for (const [name, op] of [["__add__", "add"], ["__sub__", "sub"], ["__or__", "or"], ["__and__", "and"]]) Counter.dict.set(name, builtin(name, 2, (a) => { const out = rt.construct(Counter, [], null); const keys = new Set(); const all = []; for (const [k] of dictEntries(a[0])) { const kk = rt.keyOf(k); if (!keys.has(kk)) { keys.add(kk); all.push(k); } } for (const [k] of dictEntries(a[1])) { const kk = rt.keyOf(k); if (!keys.has(kk)) { keys.add(kk); all.push(k); } } for (const k of all) { const x = dictGet(a[0], k) || 0n, y = dictGet(a[1], k) || 0n; let v; if (op === "add") v = R.binop("add", x, y); else if (op === "sub") v = R.binop("sub", x, y); else if (op === "or") v = cmp("gt", x, y) ? x : y; else v = cmp("lt", x, y) ? x : y; if (cmp("gt", v, 0n)) dictSet(out, k, v); } return out; }));
        g.set("Counter", Counter);
        // Circular ring buffer for O(1) append/appendleft/pop/popleft.
        const dqArr = (d) => {
            const n = d.len, b = d.buf, m = b.length - 1, h = d.head;
            const out = new Array(n);
            for (let i = 0; i < n; i++) out[i] = b[(h + i) & m];
            return out;
        };
        const dqGrow = (d) => {
            const old = d.buf, cap = old.length, n = d.len, m = cap - 1, h = d.head;
            const nb = new Array(cap << 1);
            for (let i = 0; i < n; i++) nb[i] = old[(h + i) & m];
            d.buf = nb; d.head = 0;
        };
        const dqPush = (d, v) => {
            if (d.maxlen === 0) return;
            if (d.maxlen !== null && d.len === d.maxlen) {
                const mask = d.buf.length - 1;
                d.buf[d.head] = undefined;
                d.buf[(d.head + d.len) & mask] = v;
                d.head = (d.head + 1) & mask;
                return;
            }
            if (d.len === d.buf.length) dqGrow(d);
            d.buf[(d.head + d.len) & (d.buf.length - 1)] = v;
            d.len++;
        };
        const dqPushLeft = (d, v) => {
            if (d.maxlen === 0) return;
            if (d.maxlen !== null && d.len === d.maxlen) {
                const mask = d.buf.length - 1;
                d.buf[(d.head + d.len - 1) & mask] = undefined;
                d.head = (d.head - 1) & mask;
                d.buf[d.head] = v;
                return;
            }
            if (d.len === d.buf.length) dqGrow(d);
            d.head = (d.head - 1) & (d.buf.length - 1);
            d.buf[d.head] = v;
            d.len++;
        };
        const dqPop = (d) => {
            if (d.len === 0) fail(E.IndexError, "pop from an empty deque");
            d.len--;
            const idx = (d.head + d.len) & (d.buf.length - 1);
            const v = d.buf[idx]; d.buf[idx] = undefined;
            return v;
        };
        const dqPopLeft = (d) => {
            if (d.len === 0) fail(E.IndexError, "pop from an empty deque");
            const v = d.buf[d.head]; d.buf[d.head] = undefined;
            d.head = (d.head + 1) & (d.buf.length - 1);
            d.len--;
            return v;
        };
        const dqSetFrom = (d, arr) => {
            let cap = 8; while (cap < arr.length) cap <<= 1;
            const nb = new Array(cap);
            for (let i = 0; i < arr.length; i++) nb[i] = arr[i];
            d.buf = nb; d.head = 0; d.len = arr.length;
        };
        const Deque = pyClass("deque", "collections", {
            __init__: (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                const d = a[0];
                const ml = a[2] !== undefined ? a[2] : kwget(kw, "maxlen", null);
                const max = ml === null ? null : Number(needInt(ml));
                if (max !== null && max < 0) fail(E.ValueError, "maxlen must be non-negative");
                d.maxlen = max;
                const src = a[1] === undefined || a[1] === null ? [] : drain(a[1]);
                const trimmed = max !== null && src.length > max ? src.slice(src.length - max) : src;
                dqSetFrom(d, trimmed);
                return null;
            },
            append: (a) => { dqPush(a[0], a[1]); return null; },
            appendleft: (a) => { dqPushLeft(a[0], a[1]); return null; },
            pop: (a) => dqPop(a[0]),
            popleft: (a) => dqPopLeft(a[0]),
            extend: (a) => { const d = a[0]; for (const x of drain(a[1])) dqPush(d, x); return null; },
            extendleft: (a) => { const d = a[0]; for (const x of drain(a[1])) dqPushLeft(d, x); return null; },
            clear: (a) => { a[0].buf = new Array(8); a[0].head = 0; a[0].len = 0; return null; },
            rotate: (a) => {
                const d = a[0], n = a[1] === undefined ? 1 : Number(needInt(a[1]));
                if (d.len <= 1) return null;
                const k = ((n % d.len) + d.len) % d.len;
                if (k === 0) return null;
                if (k <= (d.len >> 1)) { for (let i = 0; i < k; i++) dqPushLeft(d, dqPop(d)); }
                else { for (let i = 0; i < d.len - k; i++) dqPush(d, dqPopLeft(d)); }
                return null;
            },
            __len__: (a) => BigInt(a[0].len),
            __iter__: (a) => iter(list(dqArr(a[0]))),
            __getitem__: (a) => {
                const d = a[0];
                if (isInt(a[1])) {
                    let i = Number(asInt(a[1]));
                    if (i < 0) i += d.len;
                    if (i < 0 || i >= d.len) fail(E.IndexError, "deque index out of range");
                    return d.buf[(d.head + i) & (d.buf.length - 1)];
                }
                return getitem(list(dqArr(d)), a[1]);
            },
            __setitem__: (a) => {
                const d = a[0];
                let i = Number(needInt(a[1]));
                if (i < 0) i += d.len;
                if (i < 0 || i >= d.len) fail(E.IndexError, "deque index out of range");
                d.buf[(d.head + i) & (d.buf.length - 1)] = a[2];
                return null;
            },
            __contains__: (a) => rt.contains(list(dqArr(a[0])), a[1]),
            __bool__: (a) => a[0].len > 0,
            __repr__: (a) => "deque(" + repr(list(dqArr(a[0]))) + (a[0].maxlen !== null ? ", maxlen=" + a[0].maxlen : "") + ")",
            __eq__: (a) => a[1] !== null && typeof a[1] === "object" && (a[1].cls === Deque || isInstance(a[1], Deque)) ? eq(list(dqArr(a[0])), list(dqArr(a[1]))) : NOTIMPL,
            count: (a) => BigInt(rt.acount(dqArr(a[0]), (x) => eq(x, a[1]))),
            index: (a) => { const i = rt.aindex(dqArr(a[0]), (x) => eq(x, a[1])); if (i < 0) fail(E.ValueError, "not in deque"); return BigInt(i); },
            remove: (a) => { const arr = dqArr(a[0]); const i = rt.aindex(arr, (x) => eq(x, a[1])); if (i < 0) fail(E.ValueError, "deque.remove(x): x not in deque"); arr.splice(i, 1); dqSetFrom(a[0], arr); return null; },
            reverse: (a) => { const arr = dqArr(a[0]); arr.reverse(); dqSetFrom(a[0], arr); return null; },
            copy: (a) => { const o = rt.construct(a[0].cls, [list(dqArr(a[0]))], null); o.maxlen = a[0].maxlen; return o; },
        });
        Deque.dict.get("__init__").kwnames = true;
        Deque.dict.get("append").c2 = function (d, v) { dqPush(d, v); return null; };
        Deque.dict.get("appendleft").c2 = function (d, v) { dqPushLeft(d, v); return null; };
        Deque.dict.get("pop").c1 = function (d) { return dqPop(d); };
        Deque.dict.get("popleft").c1 = function (d) { return dqPopLeft(d); };
        Deque.dict.get("__len__").c1 = function (d) { return BigInt(d.len); };
        Deque.dict.get("__bool__").c1 = function (d) { return d.len > 0; };
        rt.allocators.set(Deque, (cls) => ({ cls: cls, buf: new Array(8), head: 0, len: 0, maxlen: null }));
        Deque.dict.set("maxlen", { cls: T.property, fget: builtin("maxlen", 1, (a) => a[0].maxlen === null ? null : BigInt(a[0].maxlen)), fset: null, fdel: null, doc: null });
        g.set("deque", Deque);
        const cgCol = { cls: T.classmethod, func: builtin("__class_getitem__", 2, (a) => rt.makeGenericAlias(a[0], a[1])) };
        for (const ct of [DefaultDict, OrderedDict, Deque]) ct.dict.set("__class_getitem__", cgCol);
        fnkw(g, "namedtuple", (a) => {
            const kw = kwOf(a, ["defaults", "rename", "module"]);
            const name = needStr(a[0]);
            let fields = typeof a[1] === "string" ? a[1].split(/[,\s]+/).filter((x) => x) : drain(a[1]).map((x) => needStr(x));
            const defaults = drain(kwget(kw, "defaults", list([])));
            const cls = rt.newType(name, [T.tuple], new Map(), "main");
            cls.dict.set("_fields", tuple(fields.slice()));
            const init = builtin("__new__", -1, (b) => { const kwm = b[b.length - 1] instanceof Map ? b.pop() : null; const items = b.slice(1); if (kwm) for (const [k, v] of kwm) { const i = fields.indexOf(k); if (i < 0) fail(E.TypeError, "unexpected keyword " + k); items[i] = v; } for (let i = items.length; i < fields.length; i++) { const d = defaults[i - (fields.length - defaults.length)]; if (d === undefined) fail(E.TypeError, name + " missing required argument: '" + fields[i] + "'"); items[i] = d; } if (items.length > fields.length) fail(E.TypeError, name + " takes " + fields.length + " positional arguments but " + items.length + " were given"); const o = { cls: b[0], items: items, dict: rt.attrs(23) }; return o; });
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
        const ABCMeta = R.import("abc", null).globals.get("ABCMeta");
        const abcG = g.get("abc").globals;
        const hasMethods = (sub, names) => {
            for (const m of names) {
                const v = rt.lookupType(sub, m);
                if (v === undefined || v === null) return NOTIMPL;
            }
            return true;
        };
        const makeAbc = (name, bases, methods) => {
            const cls = rt.newType(name, bases, new Map(), "collections.abc");
            cls.cls = ABCMeta;
            cls.isABC = true;
            cls._abc_registry = new Set();
            cls.dict.set("__class_getitem__", cgCol);
            if (methods !== null) {
                cls.dict.set("__subclasshook__", { cls: T.classmethod, func: builtin("__subclasshook__", 2, (a) => a[0] === cls ? hasMethods(a[1], methods) : NOTIMPL) });
            }
            abcG.set(name, cls);
            return cls;
        };
        const Hashable = makeAbc("Hashable", [], ["__hash__"]);
        const Awaitable = makeAbc("Awaitable", [], ["__await__"]);
        const Coroutine = makeAbc("Coroutine", [Awaitable], ["send", "throw", "close", "__await__"]);
        const AsyncIterable = makeAbc("AsyncIterable", [], ["__aiter__"]);
        const AsyncIterator = makeAbc("AsyncIterator", [AsyncIterable], ["__anext__", "__aiter__"]);
        const AsyncGenerator = makeAbc("AsyncGenerator", [AsyncIterator], ["__aiter__", "__anext__", "asend", "athrow", "aclose"]);
        const Iterable = makeAbc("Iterable", [], ["__iter__"]);
        const Iterator = makeAbc("Iterator", [Iterable], ["__iter__", "__next__"]);
        const Reversible = makeAbc("Reversible", [Iterable], ["__reversed__", "__iter__"]);
        const Generator = makeAbc("Generator", [Iterator], ["__iter__", "__next__", "send", "throw", "close"]);
        const Sized = makeAbc("Sized", [], ["__len__"]);
        const Container = makeAbc("Container", [], ["__contains__"]);
        const Collection = makeAbc("Collection", [Sized, Iterable, Container], ["__len__", "__iter__", "__contains__"]);
        const Callable = makeAbc("Callable", [], ["__call__"]);
        const SetAbc = makeAbc("Set", [Collection], null);
        const MutableSet = makeAbc("MutableSet", [SetAbc], null);
        const Mapping = makeAbc("Mapping", [Collection], null);
        Mapping.dict.set("get", builtin("get", 3, (a) => { try { return getitem(a[0], a[1]); } catch (e) { if (e && e.cls && rt.isSubclass(e.cls, E.KeyError)) return a[2] === undefined ? null : a[2]; throw e; } }, 2));
        Mapping.dict.set("__contains__", builtin("__contains__", 2, (a) => { try { getitem(a[0], a[1]); return true; } catch (e) { if (e && e.cls && rt.isSubclass(e.cls, E.KeyError)) return false; throw e; } }));
        Mapping.dict.set("keys", builtin("keys", 1, (a) => list(drain(a[0]))));
        Mapping.dict.set("items", builtin("items", 1, (a) => list(drain(a[0]).map((k) => tuple([k, getitem(a[0], k)])))));
        Mapping.dict.set("values", builtin("values", 1, (a) => list(drain(a[0]).map((k) => getitem(a[0], k)))));
        Mapping.dict.set("__eq__", builtin("__eq__", 2, (a) => { if (!rt.call(rt.builtins.get("isinstance"), [a[1], Mapping], null)) return NOTIMPL; return eq(dictFromPairs(drain(a[0]).map((k) => [k, getitem(a[0], k)])), dictFromPairs(drain(a[1]).map((k) => [k, getitem(a[1], k)]))); }));
        Mapping.dict.set("__ne__", builtin("__ne__", 2, (a) => { const r = call(Mapping.dict.get("__eq__"), a, null); return r === NOTIMPL ? NOTIMPL : !truth(r); }));
        const dictFromPairs = (pairs) => { const d = dict(); for (const [k, v] of pairs) dictSet(d, k, v); return d; };
        const MutableMapping = makeAbc("MutableMapping", [Mapping], null);
        const MappingView = makeAbc("MappingView", [Sized], null);
        const KeysView = makeAbc("KeysView", [MappingView, SetAbc], null);
        const ItemsView = makeAbc("ItemsView", [MappingView, SetAbc], null);
        const ValuesView = makeAbc("ValuesView", [MappingView, Collection], null);
        const Sequence = makeAbc("Sequence", [Reversible, Collection], null);
        Sequence.dict.set("__contains__", builtin("__contains__", 2, (a) => { for (const x of drain(a[0])) if (eq(x, a[1])) return true; return false; }));
        Sequence.dict.set("__reversed__", builtin("__reversed__", 1, (a) => iter(list(drain(a[0]).reverse()))));
        Sequence.dict.set("index", builtin("index", 4, (a) => { const items = drain(a[0]); let i = a[2] !== undefined ? Number(needInt(a[2])) : 0; if (i < 0) i = Math.max(0, items.length + i); let stop = a[3] !== undefined ? Number(needInt(a[3])) : items.length; if (stop < 0) stop = Math.max(0, items.length + stop); for (; i < stop && i < items.length; i++) if (eq(items[i], a[1])) return BigInt(i); fail(E.ValueError, repr(a[1]) + " is not in sequence"); }, 2));
        Sequence.dict.set("count", builtin("count", 2, (a) => { let c = 0n; for (const x of drain(a[0])) if (eq(x, a[1])) c++; return c; }));
        const MutableSequence = makeAbc("MutableSequence", [Sequence], null);
        const ByteString = makeAbc("ByteString", [Sequence], null);
        const Buffer = makeAbc("Buffer", [], ["__buffer__"]);
        // Register concrete builtins with their corresponding ABCs.
        Callable._abc_registry.add(T.function);
        Callable._abc_registry.add(T.builtin_function_or_method);
        Callable._abc_registry.add(T.method);
        Callable._abc_registry.add(rt.TypeType);
        Iterable._abc_registry.add(T.str);
        Iterable._abc_registry.add(T.bytes);
        Iterable._abc_registry.add(T.range);
        Iterator._abc_registry.add(T.iterator);
        Iterator._abc_registry.add(T.list_iterator);
        Iterator._abc_registry.add(T.generator);
        Generator._abc_registry.add(T.generator);
        Coroutine._abc_registry.add(T.coroutine);
        AsyncGenerator._abc_registry.add(T.async_generator);
        MutableMapping._abc_registry.add(T.dict);
        MutableSequence._abc_registry.add(T.list);
        Sequence._abc_registry.add(T.tuple);
        Sequence._abc_registry.add(T.str);
        Sequence._abc_registry.add(T.bytes);
        Sequence._abc_registry.add(T.range);
        ByteString._abc_registry.add(T.bytes);
        SetAbc._abc_registry.add(T.frozenset);
        MutableSet._abc_registry.add(T.set);
        KeysView._abc_registry.add(T.dict_keys);
        ItemsView._abc_registry.add(T.dict_items);
        ValuesView._abc_registry.add(T.dict_values);
        const UserDict = rt.newType("UserDict", [MutableMapping], new Map([
            ["__init__", (() => {
                const f = builtin("__init__", -1, (a) => {
                    const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                    const d = dict();
                    a[0].dict.set("data", d);
                    if (a[1] !== undefined && a[1] !== null) call(rt.getattr(a[0], "update"), [a[1]], null);
                    if (kw && kw.size) call(rt.getattr(a[0], "update"), [], kw);
                    return null;
                });
                f.kwnames = true;
                return f;
            })()],
            ["__len__", builtin("__len__", 1, (a) => len(a[0].dict.get("data")))],
            ["__getitem__", builtin("__getitem__", 2, (a) => {
                const d = a[0].dict.get("data");
                if (rt.dictHas(d, a[1])) return dictGet(d, a[1]);
                const miss = rt.lookupType(typeOf(a[0]), "__missing__");
                if (miss !== undefined) return call(descrGet(miss, a[0], typeOf(a[0])), [a[1]], null);
                throw rt.makeExc(E.KeyError, [a[1]]);
            })],
            ["__setitem__", builtin("__setitem__", 3, (a) => { rt.setitem(a[0].dict.get("data"), a[1], a[2]); return null; })],
            ["__delitem__", builtin("__delitem__", 2, (a) => { rt.delitem(a[0].dict.get("data"), a[1]); return null; })],
            ["__iter__", builtin("__iter__", 1, (a) => iter(a[0].dict.get("data")))],
            ["__contains__", builtin("__contains__", 2, (a) => rt.contains(a[0].dict.get("data"), a[1]))],
            ["__repr__", builtin("__repr__", 1, (a) => repr(a[0].dict.get("data")))],
            ["copy", builtin("copy", 1, (a) => rt.construct(typeOf(a[0]), [rt.dictCopy(a[0].dict.get("data"))], null))],
            ["update", (() => {
                const f = builtin("update", -1, (a) => {
                    const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                    const self = a[0];
                    if (a[1] !== undefined) {
                        const src = a[1];
                        if (src !== null && typeof src === "object" && typeMethod(src, "keys") !== undefined) {
                            for (const k of drain(call(rt.getattr(src, "keys"), [], null))) rt.setitem(self, k, getitem(src, k));
                        } else {
                            for (const p of drain(src)) { const kv = drain(p); rt.setitem(self, kv[0], kv[1]); }
                        }
                    }
                    if (kw) for (const [k, v] of kw) rt.setitem(self, k, v);
                    return null;
                });
                f.kwnames = true;
                return f;
            })()],
            ["pop", builtin("pop", 3, (a) => call(rt.getattr(a[0].dict.get("data"), "pop"), a.slice(1), null), 2)],
            ["clear", builtin("clear", 1, (a) => { call(rt.getattr(a[0].dict.get("data"), "clear"), [], null); return null; })],
            ["setdefault", builtin("setdefault", 3, (a) => { const self = a[0]; if (!rt.contains(self, a[1])) rt.setitem(self, a[1], a[2] === undefined ? null : a[2]); return getitem(self, a[1]); }, 2)],
            ["__class_getitem__", cgCol],
        ]), "collections");
        const UserList = rt.newType("UserList", [MutableSequence], new Map([
            ["__init__", builtin("__init__", 2, (a) => { a[0].dict.set("data", a[1] === undefined || a[1] === null ? list([]) : list(drain(a[1]))); return null; }, 1)],
            ["__len__", builtin("__len__", 1, (a) => len(a[0].dict.get("data")))],
            ["__getitem__", builtin("__getitem__", 2, (a) => getitem(a[0].dict.get("data"), a[1]))],
            ["__setitem__", builtin("__setitem__", 3, (a) => { rt.setitem(a[0].dict.get("data"), a[1], a[2]); return null; })],
            ["__delitem__", builtin("__delitem__", 2, (a) => { rt.delitem(a[0].dict.get("data"), a[1]); return null; })],
            ["__iter__", builtin("__iter__", 1, (a) => iter(a[0].dict.get("data")))],
            ["__repr__", builtin("__repr__", 1, (a) => repr(a[0].dict.get("data")))],
            ["append", builtin("append", 2, (a) => { a[0].dict.get("data").items.push(a[1]); return null; })],
            ["extend", builtin("extend", 2, (a) => { for (const x of drain(a[1])) a[0].dict.get("data").items.push(x); return null; })],
            ["insert", builtin("insert", 3, (a) => call(rt.getattr(a[0].dict.get("data"), "insert"), [a[1], a[2]], null))],
            ["pop", builtin("pop", 2, (a) => call(rt.getattr(a[0].dict.get("data"), "pop"), a.slice(1), null), 1)],
            ["clear", builtin("clear", 1, (a) => { a[0].dict.get("data").items.length = 0; return null; })],
            ["copy", builtin("copy", 1, (a) => rt.construct(typeOf(a[0]), [a[0].dict.get("data")], null))],
            ["__class_getitem__", cgCol],
        ]), "collections");
        const UserString = rt.newType("UserString", [Sequence], new Map([
            ["__init__", builtin("__init__", 2, (a) => { a[0].dict.set("data", typeof a[1] === "string" ? a[1] : rt.str(a[1])); return null; })],
            ["__str__", builtin("__str__", 1, (a) => a[0].dict.get("data"))],
            ["__repr__", builtin("__repr__", 1, (a) => repr(a[0].dict.get("data")))],
            ["__len__", builtin("__len__", 1, (a) => len(a[0].dict.get("data")))],
            ["__getitem__", builtin("__getitem__", 2, (a) => getitem(a[0].dict.get("data"), a[1]))],
            ["__iter__", builtin("__iter__", 1, (a) => iter(a[0].dict.get("data")))],
            ["__contains__", builtin("__contains__", 2, (a) => rt.contains(a[0].dict.get("data"), a[1]))],
            ["__eq__", builtin("__eq__", 2, (a) => eq(a[0].dict.get("data"), isInstance(a[1], UserString) ? a[1].dict.get("data") : a[1]))],
            ["__hash__", builtin("__hash__", 1, (a) => rt.hashInt(a[0].dict.get("data")))],
        ]), "collections");
        g.set("UserDict", UserDict);
        g.set("UserList", UserList);
        g.set("UserString", UserString);
        rt.modules.set("collections.abc", g.get("abc"));
    });
    mod("heapq", (g) => {
        const lt = rt.fastLt;
        const up = (h, i) => { while (i > 0) { const p = (i - 1) >> 1; if (lt(h[i], h[p])) { [h[i], h[p]] = [h[p], h[i]]; i = p; } else break; } };
        const down = (h, i) => { const n = h.length; for (;;) { let m = i; const l = 2 * i + 1, r = l + 1; if (l < n && lt(h[l], h[m])) m = l; if (r < n && lt(h[r], h[m])) m = r; if (m === i) break; [h[i], h[m]] = [h[m], h[i]]; i = m; } };
        fn(g, "heappush", 2, (a) => { a[0].items.push(a[1]); up(a[0].items, a[0].items.length - 1); return null; });
        fn(g, "heappop", 1, (a) => { const h = a[0].items; if (!h.length) fail(E.IndexError, "index out of range"); const top = h[0]; const last = h.pop(); if (h.length) { h[0] = last; down(h, 0); } return top; });
        fn(g, "heapify", 1, (a) => { const h = a[0].items; for (let i = (h.length >> 1) - 1; i >= 0; i--) down(h, i); return null; });
        fn(g, "heappushpop", 2, (a) => { const h = a[0].items; if (h.length && lt(h[0], a[1])) { const top = h[0]; h[0] = a[1]; down(h, 0); return top; } return a[1]; });
        fn(g, "heapreplace", 2, (a) => { const h = a[0].items; const top = h[0]; h[0] = a[1]; down(h, 0); return top; });
        // Pushes and pops of plain items natively (`vm::py_str`, ops 12, 13).
        if (rt.nativeEntry !== null) { rt.nativeEntry(g.get("heappush"), 2, 12); rt.nativeEntry(g.get("heappop"), 1, 13); }
        const topk = (a, largest) => {
            const kw = kwOf(a, ["key"]), k = Number(needInt(a[0])), key = kwget(kw, "key", null);
            if (key === null && rt.PYORD !== null && k > 0 && k <= 64 && a[1] !== null && typeof a[1] === "object" && (a[1].cls === T.list || a[1].cls === T.tuple)) {
                const r = rt.PYORD(5, a[1].items, k, largest ? 1 : 0);
                if (r !== undefined) return list(r);
            }
            return list(rt.sortItems(drain(a[1]), key, largest).slice(0, k));
        };
        fnkw(g, "nlargest", (a) => topk(a, true));
        fnkw(g, "nsmallest", (a) => topk(a, false));
    });
    mod("bisect", (g) => {
        // bisect_*(a, x, lo=0, hi=None, *, key=None) over any sequence;
        // insort_* compares key(x) when a key is given.
        const bounds = (a, kw) => {
            const loV = a[2] !== undefined ? a[2] : kwget(kw, "lo", 0n), hiV = a[3] !== undefined ? a[3] : kwget(kw, "hi", null);
            const lo = Number(needInt(loV)); if (lo < 0) fail(E.ValueError, "lo must be non-negative");
            return [lo, hiV === null ? Number(len(a[0])) : Number(needInt(hiV))];
        };
        const bis = (seq, x, right, key, lo, hi) => {
            const items = seq !== null && typeof seq === "object" && seq.items !== undefined && (seq.cls === T.list || seq.cls === T.tuple) ? seq.items : null;
            while (lo < hi) {
                const mid = Math.floor((lo + hi) / 2);
                let v = items !== null ? items[mid] : getitem(seq, BigInt(mid));
                if (key !== null) v = call(key, [v], null);
                if (right ? rt.fastLt(x, v) : !rt.fastLt(v, x)) hi = mid; else lo = mid + 1;
            }
            return lo;
        };
        for (const [name, right] of [["bisect_left", false], ["bisect_right", true]]) {
            fnkw(g, name, (a) => { const kw = kwOf(a, ["lo", "hi", "key"]); const [lo, hi] = bounds(a, kw); return BigInt(bis(a[0], a[1], right, kwget(kw, "key", null), lo, hi)); });
        }
        for (const [name, right] of [["insort_left", false], ["insort_right", true]]) {
            fnkw(g, name, (a) => {
                const kw = kwOf(a, ["lo", "hi", "key"]); const [lo, hi] = bounds(a, kw); const key = kwget(kw, "key", null);
                const i = bis(a[0], key === null ? a[1] : call(key, [a[1]], null), right, key, lo, hi);
                if (a[0] !== null && typeof a[0] === "object" && a[0].cls === T.list) a[0].items.splice(i, 0, a[1]);
                else call(rt.getattr(a[0], "insert"), [BigInt(i), a[1]], null);
                return null;
            });
        }
        // Plain items natively (`vm::py_str`, ops 14 to 17).
        if (rt.nativeEntry !== null) {
            rt.nativeEntry(g.get("bisect_left"), 2, 14); rt.nativeEntry(g.get("bisect_right"), 2, 15);
            rt.nativeEntry(g.get("insort_left"), 2, 16); rt.nativeEntry(g.get("insort_right"), 2, 17);
        }
        g.set("bisect", g.get("bisect_right"));
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
        const FLAGS = { IGNORECASE: 2n, I: 2n, LOCALE: 4n, L: 4n, MULTILINE: 8n, M: 8n, DOTALL: 16n, S: 16n, UNICODE: 32n, U: 32n, VERBOSE: 64n, X: 64n, ASCII: 256n, A: 256n, NOFLAG: 0n };
        for (const [k, v] of Object.entries(FLAGS)) g.set(k, v);
        const error = E.ValueError;
        const INLINE = { a: 256n, i: 2n, L: 4n, m: 8n, s: 16n, u: 32n, x: 64n };
        // Python's str-pattern classes: \w is [\p{L}\p{N}_], \d is \p{Nd},
        // \s is str.isspace(); under ASCII they are the [A-Za-z0-9_] forms.
        const WORD = "\\p{L}\\p{N}_", SPACE = rt.PY_WS;
        const ASCII_SPACE = " \\t\\n\\r\\x0b\\x0c";
        const SYNTAX = "^$\\.*+?()[]{}|/";
        // Python pattern syntax -> a JavaScript `u` pattern. `.`, `^`, `$`,
        // `\A`, `\Z`, `\b` and the classes are spelled out, so only IGNORECASE
        // is left to a JS flag; scoped `(?flags:...)` groups keep a flag stack.
        // Returns {source, flags (with the inline global flags), names}.
        function translate(pattern, flags) {
            const p = pattern, n = p.length;
            let i = 0;
            const bad = (msg) => fail(error, msg + " at position " + i);
            // Leading global flags: (?aiLmsux)...
            for (;;) {
                const m = /^\(\?([aiLmsux]+)\)/.exec(p.slice(i));
                if (!m) break;
                for (const c of m[1]) flags |= INLINE[c];
                i += m[0].length;
            }
            let st = { i: (flags & 2n) !== 0n, m: (flags & 8n) !== 0n, s: (flags & 16n) !== 0n, x: (flags & 64n) !== 0n, a: (flags & 256n) !== 0n };
            const baseI = st.i;
            const stack = [];
            const names = new Map();
            let groups = 0, out = "";
            // {m}, {m,}, {,n}, {m,n}; any other brace is a literal.
            const quant = () => {
                const m = /^\{(\d*)(?:(,)(\d*))?\}/.exec(p.slice(i));
                if (!m || (m[1] === "" && m[2] === undefined)) return null;
                i += m[0].length;
                return "{" + (m[1] === "" ? "0" : m[1]) + (m[2] ? "," + m[3] : "") + "}";
            };
            function escape(inClass) {
                const c = p[i + 1];
                if (c === undefined) bad("bad escape (end of pattern)");
                i += 2;
                switch (c) {
                    case "w": return st.a ? (inClass ? "A-Za-z0-9_" : "[A-Za-z0-9_]") : (inClass ? WORD : "[" + WORD + "]");
                    case "W": return st.a ? (inClass ? "\\W" : "[^A-Za-z0-9_]") : (inClass ? "\\W" : "[^" + WORD + "]");
                    case "d": return st.a ? (inClass ? "0-9" : "[0-9]") : "\\p{Nd}";
                    case "D": return st.a ? (inClass ? "\\D" : "[^0-9]") : "\\P{Nd}";
                    case "s": return st.a ? (inClass ? ASCII_SPACE : "[" + ASCII_SPACE + "]") : (inClass ? SPACE : "[" + SPACE + "]");
                    case "S": return st.a ? (inClass ? "\\S" : "[^" + ASCII_SPACE + "]") : (inClass ? "\\S" : "[^" + SPACE + "]");
                    case "b": return inClass ? "\\x08" : st.a ? "\\b" : "(?:(?<=[" + WORD + "])(?![" + WORD + "])|(?<![" + WORD + "])(?=[" + WORD + "]))";
                    case "B": if (inClass) bad("bad escape \\B"); return st.a ? "\\B" : "(?:(?<=[" + WORD + "])(?=[" + WORD + "])|(?<![" + WORD + "])(?![" + WORD + "]))";
                    case "A": if (inClass) bad("bad escape \\A"); return "(?<![\\s\\S])";
                    case "Z": if (inClass) bad("bad escape \\Z"); return "(?![\\s\\S])";
                    case "n": return "\\n"; case "t": return "\\t"; case "r": return "\\r"; case "f": return "\\f"; case "v": return "\\v"; case "a": return "\\x07";
                    case "x": { const m = /^[0-9a-fA-F]{2}/.exec(p.slice(i)); if (!m) bad("incomplete escape \\x"); i += 2; return "\\u{" + m[0] + "}"; }
                    case "u": { const m = /^[0-9a-fA-F]{4}/.exec(p.slice(i)); if (!m) bad("incomplete escape \\u"); i += 4; return "\\u{" + m[0] + "}"; }
                    case "U": { const m = /^[0-9a-fA-F]{8}/.exec(p.slice(i)); if (!m) bad("incomplete escape \\U"); i += 8; return "\\u{" + m[0].replace(/^0+(?=.)/, "") + "}"; }
                }
                if (c >= "0" && c <= "9") {
                    // An octal escape (\0, \0nn, or three octal digits), else a group reference.
                    const m = /^[0-7]{0,2}/.exec(p.slice(i));
                    if (c === "0" || (c <= "7" && m[0].length === 2)) {
                        const digits = c === "0" ? /^[0-7]{0,2}/.exec(p.slice(i))[0] : m[0];
                        i += digits.length;
                        return "\\u{" + parseInt(c + digits, 8).toString(16) + "}";
                    }
                    let num = c;
                    if (p[i] >= "0" && p[i] <= "9") num += p[i++];
                    if (inClass) bad("bad escape \\" + num);
                    return "\\" + num;
                }
                if (/[A-Za-z]/.test(c)) { i -= 2; bad("bad escape \\" + c); }
                // Escaped punctuation is the literal character.
                if (SYNTAX.includes(c) || (inClass && c === "-")) return "\\" + c;
                return c;
            }
            function charClass() {
                let s = "[";
                i++;
                if (p[i] === "^") { s += "^"; i++; }
                // A leading ']' is a literal member.
                if (p[i] === "]") { s += "\\]"; i++; }
                for (;;) {
                    if (i >= n) bad("unterminated character set");
                    const c = p[i];
                    if (c === "]") { i++; return s + "]"; }
                    if (c === "\\") { s += escape(true); continue; }
                    if (c === "[") { s += "\\["; i++; continue; }
                    s += c; i++;
                }
            }
            while (i < n) {
                const c = p[i];
                if (st.x) {
                    if (c === " " || c === "\t" || c === "\n" || c === "\r" || c === "\x0b" || c === "\x0c") { i++; continue; }
                    if (c === "#") { while (i < n && p[i] !== "\n") i++; continue; }
                }
                if (c === "\\") { out += escape(false); continue; }
                if (c === "[") { out += charClass(); continue; }
                if (c === "(") {
                    const rest = p.slice(i);
                    let m;
                    if ((m = /^\(\?P<([A-Za-z_][A-Za-z0-9_]*)>/.exec(rest))) {
                        groups++; if (names.has(m[1])) bad("redefinition of group name " + repr(m[1]));
                        names.set(m[1], groups); out += "(?<" + m[1] + ">"; i += m[0].length; stack.push(st); continue;
                    }
                    if ((m = /^\(\?P=([A-Za-z_][A-Za-z0-9_]*)\)/.exec(rest))) { if (!names.has(m[1])) bad("unknown group name " + repr(m[1])); out += "\\k<" + m[1] + ">"; i += m[0].length; continue; }
                    if (rest.startsWith("(?#")) { const e = p.indexOf(")", i); if (e < 0) bad("missing ), unterminated comment"); i = e + 1; continue; }
                    if ((m = /^\(\?(<=|<!|=|!|:)/.exec(rest))) { out += m[0]; i += m[0].length; stack.push(st); continue; }
                    if (rest.startsWith("(?>")) { out += "(?:"; i += 3; stack.push(st); continue; }
                    if ((m = /^\(\?([aiLmsux]*)(?:-([imsx]+))?:/.exec(rest))) {
                        // A scoped flag group: `.`/`^`/`$`/\w follow the new
                        // state here; IGNORECASE becomes a JS modifier.
                        stack.push(st);
                        st = Object.assign({}, st);
                        for (const f of m[1]) if (f in st) st[f] = true;
                        if (m[2]) for (const f of m[2]) st[f] = false;
                        const on = m[1].includes("i") ? "i" : "", off = m[2] && m[2].includes("i") ? "i" : "";
                        out += on || off ? "(?" + on + (off ? "-" + off : "") + ":" : "(?:";
                        i += m[0].length; continue;
                    }
                    if (/^\(\?[aiLmsux-]*\)/.test(rest)) bad("global flags not at the start of the expression");
                    if (rest.startsWith("(?")) bad("unknown extension " + rest.slice(1, 3));
                    groups++; out += "("; i++; stack.push(st); continue;
                }
                if (c === ")") { if (!stack.length) bad("unbalanced parenthesis"); st = stack.pop(); out += ")"; i++; continue; }
                if (c === ".") { out += st.s ? "[\\s\\S]" : "[^\\n]"; i++; continue; }
                if (c === "^") { out += st.m ? "(?<![^\\n])" : "(?<![\\s\\S])"; i++; continue; }
                if (c === "$") { out += st.m ? "(?=\\n|(?![\\s\\S]))" : "(?=\\n?(?![\\s\\S]))"; i++; continue; }
                if (c === "*" || c === "+" || c === "?") {
                    out += c; i++;
                    if (p[i] === "?") { out += "?"; i++; }
                    else if (p[i] === "+") { i++; }
                    continue;
                }
                if (c === "{") {
                    const q = quant();
                    if (q !== null) {
                        out += q;
                        if (p[i] === "?") { out += "?"; i++; }
                        else if (p[i] === "+") { i++; }
                    } else { out += "\\{"; i++; }
                    continue;
                }
                if (c === "}" || c === "]") { out += "\\" + c; i++; continue; }
                out += c; i++;
            }
            if (stack.length) fail(error, "missing ), unterminated subpattern at position " + p.length);
            return { source: out, flags: flags, ignoreCase: baseI, groups: groups, names: names };
        }
        // Share `types.js`'s cached surrogate-pair index so str and re calls
        // alternating on the same string index it once.
        const cpFromUtf16 = rt.cpFromUtf16, utf16FromCp = rt.utf16FromCp, strInfo = rt.strInfo;
        const cpIndex = (M, off) => M.pairs !== null ? cpFromUtf16(M.pairs, off) : off;
        function groupIdx(M, gi) {
            if (gi === undefined) return 0;
            if (typeof gi === "string") { const k = M.p.names.get(gi); if (k === undefined) fail(E.IndexError, "no such group"); return k; }
            const k = isInt(gi) ? Number(asInt(gi)) : -1;
            if (k < 0 || k > M.p.ngroups) fail(E.IndexError, "no such group");
            return k;
        }
        const isBytesLike = (v) => v !== null && typeof v === "object" && (v.cls === T.bytes || isInstance(v, T.bytes));
        const bytesToLatin1 = (v) => {
            const items = v.items;
            let s = "";
            for (let i = 0; i < items.length; i += 4096) {
                const end = Math.min(items.length, i + 4096);
                let chunk = "";
                for (let j = i; j < end; j++) chunk += String.fromCharCode(items[j] & 255);
                s += chunk;
            }
            return s;
        };
        const latin1ToBytes = (s) => {
            const out = new Array(s.length);
            for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i) & 255;
            return rt.bytes(out);
        };
        const needSubject = (P, v) => {
            if (P.isBytes) {
                if (!isBytesLike(v)) fail(E.TypeError, "cannot use a bytes pattern on a string-like object");
                return bytesToLatin1(v);
            }
            if (isBytesLike(v)) fail(E.TypeError, "cannot use a string pattern on a bytes-like object");
            return needStr(v);
        };
        const RE_SPAN_MUL = 67108864;
        const getLazySpan = (M, k) => k === 0 ? M.g0 : k === 1 ? M.g1 : k === 2 ? M.g2 : k === 3 ? M.g3 : k === 4 ? M.g4 : M.g5;
        const lazyGroupStr = (M, k) => {
            const pk = getLazySpan(M, k);
            if (pk === undefined) return undefined;
            const gs = Math.floor(pk / RE_SPAN_MUL), ge = pk - gs * RE_SPAN_MUL;
            return M.t.slice(gs, ge);
        };
        function ensureM(M) {
            let m = M.m;
            if (m !== null) return m;
            const t = M.t, ng = M.ng;
            m = new Array(ng);
            for (let k = 0; k < ng; k++) {
                const pk = getLazySpan(M, k);
                if (pk !== undefined) {
                    const gs = Math.floor(pk / RE_SPAN_MUL), ge = pk - gs * RE_SPAN_MUL;
                    m[k] = t.slice(gs, ge);
                }
            }
            const p0 = M.g0;
            m.index = Math.floor(p0 / RE_SPAN_MUL);
            m.input = t;
            M.m = m;
            return m;
        }
        const wrapVal = (P, s) => s === null || s === undefined ? null : (P.isBytes ? latin1ToBytes(s) : s);
        const groupText = (M, gi) => { const k = groupIdx(M, gi); const v = M.m === null ? lazyGroupStr(M, k) : M.m[k]; return v === undefined ? null : wrapVal(M.p, v); };
        const groupTextRaw = (M, gi) => { const k = groupIdx(M, gi); const v = M.m === null ? lazyGroupStr(M, k) : M.m[k]; return v === undefined ? null : v; };
        // Scans run without the `d` flag (it slows every match); a Match
        // wanting group offsets reruns the pattern anchored where it matched
        // on the same text, which reproduces the same match.
        function indices(M) {
            if (M.m === null) {
                const ng = M.ng, ix = new Array(ng);
                for (let k = 0; k < ng; k++) {
                    const pk = getLazySpan(M, k);
                    if (pk !== undefined) {
                        const gs = Math.floor(pk / RE_SPAN_MUL), ge = pk - gs * RE_SPAN_MUL;
                        ix[k] = [gs, ge];
                    }
                }
                return ix;
            }
            const m = M.m;
            if (m.indices === undefined) { const r = dsticky(M.p); r.lastIndex = m.index; m.indices = r.exec(M.t).indices; }
            return m.indices;
        }
        const span = (M, gi) => {
            const k = groupIdx(M, gi);
            if (M.m === null) {
                const pk = getLazySpan(M, k);
                if (pk === undefined) return [-1, -1];
                const gs = Math.floor(pk / RE_SPAN_MUL), ge = pk - gs * RE_SPAN_MUL;
                return [cpIndex(M, gs), cpIndex(M, ge)];
            }
            if (k === 0) return [cpIndex(M, M.m.index), cpIndex(M, M.m.index + M.m[0].length)];
            const r = indices(M)[k]; return r === undefined ? [-1, -1] : [cpIndex(M, r[0]), cpIndex(M, r[1])];
        };
        function lastIndex(M) {
            let best = 0, end = -1;
            const len = M.m === null ? M.ng : M.m.length;
            if (len === 1) return 0;
            const ix = indices(M);
            for (let k = 1; k < ix.length; k++) { const r = ix[k]; if (r !== undefined && r[1] > end) { best = k; end = r[1]; } }
            return best;
        }
        function expand(M, tpl) {
            let out = "";
            for (let k = 0; k < tpl.length; k++) {
                const c = tpl[k];
                if (c !== "\\") { out += c; continue; }
                const d = tpl[++k];
                if (d === undefined) fail(error, "bad escape (end of pattern)");
                if (d === "g") {
                    const m = /^<([^>]*)>/.exec(tpl.slice(k + 1));
                    if (!m) fail(error, "missing <");
                    k += m[0].length;
                    const ref = /^\d+$/.test(m[1]) ? BigInt(m[1]) : m[1];
                    if (typeof ref === "string" && !M.p.names.has(ref)) fail(E.IndexError, "unknown group name " + repr(ref));
                    const v = groupTextRaw(M, ref); out += v === null ? "" : v; continue;
                }
                if (d >= "0" && d <= "9") {
                    const m = /^[0-7]{2}/.exec(tpl.slice(k + 1));
                    if (d === "0" || (d <= "7" && m)) { const digits = d === "0" ? /^[0-7]{0,2}/.exec(tpl.slice(k + 1))[0] : m[0]; k += digits.length; out += String.fromCodePoint(parseInt(d + digits, 8)); continue; }
                    let num = d; if (tpl[k + 1] >= "0" && tpl[k + 1] <= "9") num += tpl[++k];
                    if (Number(num) > M.p.ngroups) fail(error, "invalid group reference " + num);
                    const v = groupTextRaw(M, BigInt(num)); out += v === null ? "" : v; continue;
                }
                const e = { n: "\n", t: "\t", r: "\r", f: "\f", v: "\x0b", a: "\x07", b: "\b", "\\": "\\" }[d];
                if (e !== undefined) { out += e; continue; }
                if (/[A-Za-z]/.test(d)) fail(error, "bad escape \\" + d);
                out += "\\" + d;
            }
            return out;
        }
        const Match = pyClass("Match", "re", {
            group: (a) => { const M = a[0]; if (a.length === 1) return wrapVal(M.p, M.m === null ? lazyGroupStr(M, 0) : M.m[0]); return a.length === 2 ? groupText(M, a[1]) : tuple(a.slice(1).map((k) => groupText(M, k))); },
            groups: (a) => { const d = a[1] === undefined ? null : a[1]; const P = a[0].p, m = ensureM(a[0]); return tuple(m.slice(1).map((x) => x === undefined ? d : wrapVal(P, x))); },
            groupdict: (a) => { const M = a[0], m = ensureM(M), d = dict(); for (const [k, gi] of M.p.names) { const v = m[gi]; dictSet(d, k, v === undefined ? (a[1] === undefined ? null : a[1]) : wrapVal(M.p, v)); } return d; },
            start: (a) => BigInt(span(a[0], a[1])[0]),
            end: (a) => BigInt(span(a[0], a[1])[1]),
            span: (a) => tuple(span(a[0], a[1]).map(BigInt)),
            expand: (a) => { const M = a[0]; return wrapVal(M.p, expand(M, needSubject(M.p, a[1]))); },
            __getitem__: (a) => groupText(a[0], a[1]),
            __repr__: (a) => { const M = a[0]; const [s0, e0] = span(M, undefined); return "<re.Match object; span=(" + s0 + ", " + e0 + "), match=" + repr(wrapVal(M.p, M.m === null ? lazyGroupStr(M, 0) : M.m[0])) + ">"; },
        });
        const cgRe = { cls: T.classmethod, func: builtin("__class_getitem__", 2, (a) => rt.makeGenericAlias(a[0], a[1])) };
        Match.dict.set("__class_getitem__", cgRe);
        const prop = (cls, name, get) => cls.dict.set(name, { cls: T.property, fget: builtin(name, 1, (a) => get(a[0])), fset: null, fdel: null, doc: null });
        prop(Match, "string", (M) => M.raw);
        prop(Match, "re", (M) => M.p);
        prop(Match, "pos", (M) => BigInt(M.pos));
        prop(Match, "endpos", (M) => BigInt(M.endpos));
        prop(Match, "lastindex", (M) => { const k = lastIndex(M); return k ? BigInt(k) : null; });
        prop(Match, "lastgroup", (M) => { const k = lastIndex(M); for (const [name, gi] of M.p.names) if (gi === k && k) return name; return null; });
        prop(Match, "regs", (M) => { const len = M.m === null ? M.ng : M.m.length; const out = new Array(len); for (let k = 0; k < len; k++) out[k] = tuple(span(M, BigInt(k)).map(BigInt)); return tuple(out); });
        // t: the text the pattern ran on (the string cut at endpos).
        // Dict-less like CPython's re.Match: avoids allocating a Map per match
        // and lets PyMethod cache Match methods directly in `gb`.
        const mk = (m, s, p, pos, endpos, pairs, t, raw) => ({ cls: Match, m: m, s: s, raw: raw !== undefined ? raw : s, t: t, p: p, pos: pos, endpos: endpos, pairs: pairs, ng: 0, g0: undefined, g1: undefined, g2: undefined, g3: undefined, g4: undefined, g5: undefined });
        // pos/endpos (code points) -> the string to scan (cut at endpos, as
        // if it ended there) and the UTF-16 start.
        function window(s, a, kw) {
            strInfo(s);
            const n = rt.infoLen, pairs = rt.infoPairs;
            let pos = a[2] !== undefined ? a[2] : kwget(kw, "pos", 0n), end = a[3] !== undefined ? a[3] : kwget(kw, "endpos", null);
            pos = Number(needInt(pos)); end = end === null ? n : Number(needInt(end));
            if (pos < 0) pos = 0; if (pos > n) pos = n; if (end > n) end = n; if (end < 0) end = 0;
            const u0 = pairs !== null ? utf16FromCp(pairs, pos) : pos;
            const u1 = end < n ? (pairs !== null ? utf16FromCp(pairs, end) : end) : s.length;
            return { text: u1 < s.length ? s.slice(0, u1) : s, start: u0, pos: pos, end: end, ok: end >= pos, pairs: pairs };
        }
        // The regex objects are shared per pattern; each call sets lastIndex
        // right before its exec, and a scan keeps its own position, so a
        // callback that runs the same pattern cannot disturb it.
        function execAt(re, text, at) { re.lastIndex = at; return re.exec(text); }
        const sticky = (P) => P.sticky || (P.sticky = new RegExp(P.re.source, P.re.flags.replace("g", "y")));
        const dsticky = (P) => P.dsticky || (P.dsticky = new RegExp(P.re.source, P.re.flags.replace("g", "dy")));
        const full = (P) => P.full || (P.full = new RegExp("(?:" + P.re.source + ")(?![\\s\\S])", P.re.flags.replace("g", "dy")));
        function scan(P, text, start, each) {
            let pos = start, m;
            while (pos <= text.length && (m = execAt(P.re, text, pos)) !== null) {
                each(m);
                pos = m[0] === "" ? m.index + (m.index < text.length ? charWidth(text, m.index) : 1) : m.index + m[0].length;
            }
        }
        let RE_FINDALL_NATIVE = null;
        try { RE_FINDALL_NATIVE = typeof __zipp_py_re_findall === "function" ? __zipp_py_re_findall : null; } catch (e) { RE_FINDALL_NATIVE = null; }
        const FINDALL_TUPLE_TMPL = tuple([]);
        const findItem = (P, m) => {
            if (!P.isBytes) return m.length === 1 ? m[0] : m.length === 2 ? (m[1] === undefined ? "" : m[1]) : tuple(m.slice(1).map((x) => x === undefined ? "" : x));
            return m.length === 1 ? latin1ToBytes(m[0]) : m.length === 2 ? latin1ToBytes(m[1] === undefined ? "" : m[1]) : tuple(m.slice(1).map((x) => latin1ToBytes(x === undefined ? "" : x)));
        };
        const Pattern = pyClass("Pattern", "re", {
            search: (a) => {
                const P = a[0];
                if (a.length <= 3 && !(a[a.length - 1] instanceof Map)) {
                    const s = needSubject(P, a[1]); strInfo(s);
                    const n = rt.infoLen, pairs = rt.infoPairs;
                    let pos = a.length === 3 ? Number(needInt(a[2])) : 0;
                    if (pos < 0) pos = 0; if (pos > n) pos = n;
                    const u0 = pairs !== null ? utf16FromCp(pairs, pos) : pos;
                    const m = execAt(P.re, s, u0);
                    return m === null ? null : mk(m, s, P, pos, n, pairs, s, a[1]);
                }
                const kw = kwOf(a, ["pos", "endpos"]); const s = needSubject(P, a[1]); const w = window(s, a, kw); if (!w.ok) return null; const m = execAt(P.re, w.text, w.start); return m === null ? null : mk(m, s, P, w.pos, w.end, w.pairs, w.text, a[1]);
            },
            match: (a) => {
                const P = a[0];
                if (a.length <= 3 && !(a[a.length - 1] instanceof Map)) {
                    const s = needSubject(P, a[1]); strInfo(s);
                    const n = rt.infoLen, pairs = rt.infoPairs;
                    let pos = a.length === 3 ? Number(needInt(a[2])) : 0;
                    if (pos < 0) pos = 0; if (pos > n) pos = n;
                    const u0 = pairs !== null ? utf16FromCp(pairs, pos) : pos;
                    const m = execAt(sticky(P), s, u0);
                    return m === null ? null : mk(m, s, P, pos, n, pairs, s, a[1]);
                }
                const kw = kwOf(a, ["pos", "endpos"]); const s = needSubject(P, a[1]); const w = window(s, a, kw); if (!w.ok) return null; const m = execAt(sticky(P), w.text, w.start); return m === null ? null : mk(m, s, P, w.pos, w.end, w.pairs, w.text, a[1]);
            },
            fullmatch: (a) => { const P = a[0]; const kw = kwOf(a, ["pos", "endpos"]); const s = needSubject(P, a[1]); const w = window(s, a, kw); if (!w.ok) return null; const m = execAt(full(P), w.text, w.start); return m === null ? null : mk(m, s, P, w.pos, w.end, w.pairs, w.text, a[1]); },
            findall: (a) => {
                const P = a[0];
                if (a.length === 2 && !(a[1] instanceof Map)) {
                    const s = needSubject(P, a[1]);
                    if (!P.isBytes && RE_FINDALL_NATIVE !== null) {
                        const fast = RE_FINDALL_NATIVE(P.re, s, 0, FINDALL_TUPLE_TMPL);
                        if (fast !== undefined) return list(fast);
                    }
                    const out = []; scan(P, s, 0, (m) => out.push(findItem(P, m))); return list(out);
                }
                const kw = kwOf(a, ["pos", "endpos"]); const s = needSubject(P, a[1]); const w = window(s, a, kw);
                if (!w.ok) return list([]);
                if (!P.isBytes && RE_FINDALL_NATIVE !== null) {
                    const fast = RE_FINDALL_NATIVE(P.re, w.text, w.start, FINDALL_TUPLE_TMPL);
                    if (fast !== undefined) return list(fast);
                }
                const out = []; scan(P, w.text, w.start, (m) => out.push(findItem(P, m))); return list(out);
            },
            finditer: (a) => { const P = a[0]; const kw = kwOf(a, ["pos", "endpos"]); const s = needSubject(P, a[1]); const w = window(s, a, kw); const out = []; if (w.ok) scan(P, w.text, w.start, (m) => out.push(mk(m, s, P, w.pos, w.end, w.pairs, w.text, a[1]))); return iter(list(out)); },
            sub: (a) => { const kw = kwOf(a, ["count"]); return subImpl(a[0], a[1], a[2], Number(needInt(a[3] !== undefined ? a[3] : kwget(kw, "count", 0n))), false); },
            subn: (a) => { const kw = kwOf(a, ["count"]); return subImpl(a[0], a[1], a[2], Number(needInt(a[3] !== undefined ? a[3] : kwget(kw, "count", 0n))), true); },
            // CPython 3.7+: every match splits, empty ones included, but the
            // search after an empty match must move past its position.
            split: (a) => {
                const P = a[0];
                const kw = kwOf(a, ["maxsplit"]);
                const s = needSubject(P, a[1]); const max = Number(needInt(a[2] !== undefined ? a[2] : kwget(kw, "maxsplit", 0n)));
                const re = P.re, out = []; let last = 0, pos = 0, n = 0, mustAdvance = false;
                while (max <= 0 || n < max) {
                    let m = execAt(re, s, pos);
                    if (m !== null && mustAdvance && m[0] === "" && m.index === pos) {
                        if (pos >= s.length) break;
                        m = execAt(re, s, pos + charWidth(s, pos));
                    }
                    if (m === null) break;
                    out.push(wrapVal(P, s.slice(last, m.index)));
                    for (let k = 1; k < m.length; k++) out.push(m[k] === undefined ? null : wrapVal(P, m[k]));
                    n++;
                    const end = m.index + m[0].length;
                    mustAdvance = end === m.index;
                    last = pos = end;
                }
                out.push(wrapVal(P, s.slice(last)));
                return list(out);
            },
            __repr__: (a) => {
                const f = a[0].flagsOut, names = [];
                for (const [bit, name] of [[2n, "IGNORECASE"], [4n, "LOCALE"], [8n, "MULTILINE"], [16n, "DOTALL"], [64n, "VERBOSE"], [256n, "ASCII"]]) if (f & bit) names.push("re." + name);
                return "re.compile(" + repr(a[0].src) + (names.length ? ", " + names.join("|") : "") + ")";
            },
        });
        Pattern.dict.set("__class_getitem__", cgRe);
        for (const name of ["search", "match", "fullmatch", "findall", "finditer", "sub", "subn", "split"]) Pattern.dict.get(name).kwnames = true;
        {
            const patRun = (P, re, raw, posArg, hasPos) => {
                if (typeof raw !== "string" || P.isBytes || (hasPos && typeof posArg !== "bigint")) return undefined;
                if (!rt.hasSurrogate(raw)) {
                    const n = raw.length;
                    let pos = hasPos ? Number(posArg) : 0;
                    if (pos < 0) pos = 0; else if (pos > n) pos = n;
                    re.lastIndex = pos;
                    const m = re.exec(raw);
                    return m === null ? null : mk(m, raw, P, pos, n, null, raw, raw);
                }
                strInfo(raw);
                const n = rt.infoLen, pairs = rt.infoPairs;
                let pos = hasPos ? Number(posArg) : 0;
                if (pos < 0) pos = 0; else if (pos > n) pos = n;
                const u0 = pairs !== null ? utf16FromCp(pairs, pos) : pos;
                re.lastIndex = u0;
                const m = re.exec(raw);
                return m === null ? null : mk(m, raw, P, pos, n, pairs, raw, raw);
            };
            const pSearch = Pattern.dict.get("search"), pMatch = Pattern.dict.get("match"), pFull = Pattern.dict.get("fullmatch"), pFindall = Pattern.dict.get("findall"), pSplit = Pattern.dict.get("split"), pSub = Pattern.dict.get("sub");
            let RE_STRM = null;
            try { RE_STRM = typeof __zipp_py_strm === "function" ? __zipp_py_strm : null; } catch (e) { RE_STRM = null; }
            const mtmpl = { cls: Match, m: null, s: null, raw: null, t: null, p: null, pos: 0, endpos: 0, pairs: null, ng: 0, g0: undefined, g1: undefined, g2: undefined, g3: undefined, g4: undefined, g5: undefined };
            const sJ2 = function (P, s) { const r = patRun(P, P.re, s, 0n, false); return r !== undefined ? r : this.code([P, s]); };
            const sJ3 = function (P, s, pos) { const r = patRun(P, P.re, s, pos, true); return r !== undefined ? r : this.code([P, s, pos]); };
            const mJ2 = function (P, s) { const r = patRun(P, sticky(P), s, 0n, false); return r !== undefined ? r : this.code([P, s]); };
            const mJ3 = function (P, s, pos) { const r = patRun(P, sticky(P), s, pos, true); return r !== undefined ? r : this.code([P, s, pos]); };
            const fJ2 = function (P, s) { const r = patRun(P, full(P), s, 0n, false); return r !== undefined ? r : this.code([P, s]); };
            const subJ3 = function (P, repl, s) { return subImpl(P, repl, s, 0, false); };
            const subJ4 = function (P, repl, s, count) { return subImpl(P, repl, s, Number(needInt(count)), false); };
            if (RE_STRM !== null) {
                pSearch.strop = 38; pSearch.mtmpl = mtmpl; pSearch.j2 = sJ2; pSearch.j3 = sJ3; pSearch.c2 = RE_STRM; pSearch.c3 = RE_STRM;
                pMatch.strop = 39; pMatch.mtmpl = mtmpl; pMatch.j2 = mJ2; pMatch.j3 = mJ3; pMatch.c2 = RE_STRM; pMatch.c3 = RE_STRM;
                pFull.strop = 40; pFull.mtmpl = mtmpl; pFull.j2 = fJ2; pFull.c2 = RE_STRM;
                pSub.strop = 41; pSub.j3 = subJ3; pSub.j4 = subJ4; pSub.c3 = RE_STRM; pSub.c4 = RE_STRM;
            } else {
                pSearch.c2 = sJ2; pSearch.c3 = sJ3;
                pMatch.c2 = mJ2; pMatch.c3 = mJ3;
                pFull.c2 = fJ2;
                pSub.c3 = subJ3; pSub.c4 = subJ4;
            }
            pFindall.c2 = function (P, s) {
                if (typeof s === "string" && !P.isBytes && RE_FINDALL_NATIVE !== null) {
                    const fast = RE_FINDALL_NATIVE(P.re, s, 0, FINDALL_TUPLE_TMPL);
                    if (fast !== undefined) return list(fast);
                }
                return this.code([P, s]);
            };
            pSplit.c2 = function (P, s) { return this.code([P, s]); };
            const mStart = Match.dict.get("start"), mEnd = Match.dict.get("end"), mSpan = Match.dict.get("span"), mGroup = Match.dict.get("group"), mGroups = Match.dict.get("groups");
            const msJ1 = function (M) { if (M.m === null) { const gs = Math.floor(M.g0 / RE_SPAN_MUL); return BigInt(M.pairs !== null ? cpFromUtf16(M.pairs, gs) : gs); } const m = M.m; return BigInt(M.pairs !== null ? cpFromUtf16(M.pairs, m.index) : m.index); };
            const msJ2 = function (M, gi) { return BigInt(span(M, gi)[0]); };
            const meJ1 = function (M) { if (M.m === null) { const pk = M.g0, ge = pk - Math.floor(pk / RE_SPAN_MUL) * RE_SPAN_MUL; return BigInt(M.pairs !== null ? cpFromUtf16(M.pairs, ge) : ge); } const m = M.m; const u = m.index + m[0].length; return BigInt(M.pairs !== null ? cpFromUtf16(M.pairs, u) : u); };
            const meJ2 = function (M, gi) { return BigInt(span(M, gi)[1]); };
            const mspJ1 = function (M) { if (M.m === null) { const pk = M.g0, gs = Math.floor(pk / RE_SPAN_MUL), ge = pk - gs * RE_SPAN_MUL; return tuple(M.pairs !== null ? [BigInt(cpFromUtf16(M.pairs, gs)), BigInt(cpFromUtf16(M.pairs, ge))] : [BigInt(gs), BigInt(ge)]); } const m = M.m, u0 = m.index, u1 = u0 + m[0].length; return tuple(M.pairs !== null ? [BigInt(cpFromUtf16(M.pairs, u0)), BigInt(cpFromUtf16(M.pairs, u1))] : [BigInt(u0), BigInt(u1)]); };
            const mspJ2 = function (M, gi) { const sp = span(M, gi); return tuple([BigInt(sp[0]), BigInt(sp[1])]); };
            const mgJ1 = function (M) { const v = M.m === null ? lazyGroupStr(M, 0) : M.m[0]; return M.p.isBytes ? latin1ToBytes(v) : v; };
            const mgJ2 = function (M, gi) {
                if (typeof gi === "bigint") {
                    const k = Number(gi);
                    if (k >= 0 && k <= M.p.ngroups) { const v = M.m === null ? lazyGroupStr(M, k) : M.m[k]; return v === undefined ? null : (M.p.isBytes ? latin1ToBytes(v) : v); }
                }
                return this.code([M, gi]);
            };
            const mgsJ1 = function (M) { return this.code([M]); };
            const mgsJ2 = function (M, d) { return this.code([M, d]); };
            if (RE_STRM !== null) {
                mStart.strop = 42; mStart.j1 = msJ1; mStart.j2 = msJ2; mStart.c1 = RE_STRM; mStart.c2 = RE_STRM;
                mEnd.strop = 43; mEnd.j1 = meJ1; mEnd.j2 = meJ2; mEnd.c1 = RE_STRM; mEnd.c2 = RE_STRM;
                mSpan.strop = 44; mSpan.j1 = mspJ1; mSpan.j2 = mspJ2; mSpan.c1 = RE_STRM; mSpan.c2 = RE_STRM;
                mGroup.strop = 45; mGroup.j1 = mgJ1; mGroup.j2 = mgJ2; mGroup.c1 = RE_STRM; mGroup.c2 = RE_STRM;
                mGroups.strop = 46; mGroups.j1 = mgsJ1; mGroups.j2 = mgsJ2; mGroups.c1 = RE_STRM; mGroups.c2 = RE_STRM;
            } else {
                mStart.c1 = msJ1; mStart.c2 = msJ2;
                mEnd.c1 = meJ1; mEnd.c2 = meJ2;
                mSpan.c1 = mspJ1; mSpan.c2 = mspJ2;
                mGroup.c1 = mgJ1; mGroup.c2 = mgJ2;
                mGroups.c1 = mgsJ1; mGroups.c2 = mgsJ2;
            }
            mGroup.c4 = function (M, g1, g2, g3) {
                if (typeof g1 === "bigint" && typeof g2 === "bigint" && typeof g3 === "bigint" && !M.p.isBytes) {
                    const ng = M.p.ngroups, k1 = Number(g1), k2 = Number(g2), k3 = Number(g3);
                    if (k1 >= 0 && k1 <= ng && k2 >= 0 && k2 <= ng && k3 >= 0 && k3 <= ng) {
                        const v1 = M.m === null ? lazyGroupStr(M, k1) : M.m[k1];
                        const v2 = M.m === null ? lazyGroupStr(M, k2) : M.m[k2];
                        const v3 = M.m === null ? lazyGroupStr(M, k3) : M.m[k3];
                        return tuple([v1 === undefined ? null : v1, v2 === undefined ? null : v2, v3 === undefined ? null : v3]);
                    }
                }
                return this.code([M, g1, g2, g3]);
            };
        }
        prop(Pattern, "pattern", (P) => P.src);
        prop(Pattern, "flags", (P) => P.flagsOut);
        prop(Pattern, "groups", (P) => BigInt(P.ngroups));
        prop(Pattern, "groupindex", (P) => { const d = dict(); for (const [k, gi] of P.names) dictSet(d, k, BigInt(gi)); return d; });
        function subImpl(P, repl, rawSubject, count, withCount) {
            const s = needSubject(P, rawSubject);
            const re = P.re;
            let n = 0, out = "", last = 0, pos = 0, m;
            let replStr = null;
            if (P.isBytes) {
                if (isBytesLike(repl)) replStr = bytesToLatin1(repl);
                else if (typeof repl === "string") fail(E.TypeError, "cannot use a string replacement with a bytes pattern");
            } else {
                if (typeof repl === "string") replStr = repl;
                else if (isBytesLike(repl)) fail(E.TypeError, "cannot use a bytes replacement with a string pattern");
            }
            const literal = replStr !== null && !replStr.includes("\\");
            // Fast native regex replace for literal non-bytes unlimited `sub`:
            // avoids JS match-object / slice loop and hits Rust `regex_replace_ascii`.
            if (literal && count === 0 && !withCount && !P.isBytes) {
                const jsRepl = replStr.includes("$") ? replStr.replace(/\$/g, "$$$$") : replStr;
                return rt.checkedText(s.replace(re, jsRepl));
            }
            let pairs = null, n0 = 0;
            if (!literal) { strInfo(s); pairs = rt.infoPairs; n0 = rt.infoLen; }
            while (pos <= s.length) {
                if ((m = execAt(re, s, pos)) === null) break;
                pos = re.lastIndex;
                if (count > 0 && n >= count) break;
                out += s.slice(last, m.index);
                if (literal) out += replStr;
                else {
                    const M = mk(m, s, P, 0, n0, pairs, s, rawSubject);
                    out += replStr !== null ? expand(M, replStr) : needSubject(P, call(repl, [M], null));
                }
                last = m.index + m[0].length; n++;
                if (m[0] === "") { if (m.index >= s.length) break; const w = charWidth(s, m.index); out += s.slice(m.index, m.index + w); last = pos = m.index + w; }
            }
            out += s.slice(last);
            const finalOut = P.isBytes ? latin1ToBytes(out) : rt.checkedText(out);
            return withCount ? tuple([finalOut, BigInt(n)]) : finalOut;
        }
        // UTF-16 units of the code point at i: an empty match steps over a
        // whole astral character, never into the middle of its pair.
        function charWidth(s, i) { const c = s.charCodeAt(i); return c >= 0xd800 && c < 0xdc00 && i + 1 < s.length && (s.charCodeAt(i + 1) & 0xfc00) === 0xdc00 ? 2 : 1; }
        const cache = new Map();
        const strCache0 = new Map();
        function compile(pattern, flags) {
            if (flags === 0n && typeof pattern === "string") {
                const hit0 = strCache0.get(pattern);
                if (hit0 !== undefined) return hit0;
            }
            if (pattern !== null && typeof pattern === "object" && pattern.cls === Pattern) {
                if (flags !== 0n) fail(E.ValueError, "cannot process flags argument with a compiled pattern");
                return pattern;
            }
            const isB = isBytesLike(pattern);
            let srcText, srcVal, effFlags;
            if (isB) {
                if (flags & 32n) fail(E.ValueError, "cannot use UNICODE flag with a bytes pattern");
                srcText = bytesToLatin1(pattern);
                srcVal = rt.bytes(pattern.items.slice());
                effFlags = flags | 256n;
            } else {
                srcText = needStr(pattern, "first argument");
                srcVal = srcText;
                effFlags = flags;
            }
            const key = (isB ? "B:" : "S:") + srcText + " " + flags;
            const hit = cache.get(key);
            if (hit !== undefined) { if (!isB && flags === 0n) strCache0.set(srcText, hit); return hit; }
            const t = translate(srcText, effFlags);
            let re;
            try { re = new RegExp(t.source, (t.ignoreCase ? "i" : "") + "gu"); } catch (e) { fail(error, "bad pattern: " + (e && e.message)); }
            const flagsOut = isB ? (flags & ~32n) : (t.flags & 256n ? t.flags : t.flags | 32n);
            const p = { cls: Pattern, re: re, src: srcVal, isBytes: isB, flagsOut: flagsOut, ngroups: t.groups, names: t.names, sticky: null, dsticky: null, full: null };
            if (cache.size > 512) { cache.clear(); strCache0.clear(); }
            cache.set(key, p);
            if (!isB && flags === 0n) strCache0.set(srcText, p);
            return p;
        }
        const flagsOf = (a, k, kw) => { const f = a[k] !== undefined ? a[k] : kwget(kw, "flags", 0n); return isInt(f) ? asInt(f) : fail(E.TypeError, "flags must be an int"); };
        fnkw(g, "compile", (a) => { const kw = kwOf(a, ["flags"]); return compile(a[0], flagsOf(a, 1, kw)); });
        for (const name of ["search", "match", "fullmatch", "findall", "finditer"]) fnkw(g, name, (a) => { const kw = kwOf(a, ["flags"]); return call(Pattern.dict.get(name), [compile(a[0], flagsOf(a, 2, kw)), a[1]], null); });
        fnkw(g, "sub", (a) => { const kw = kwOf(a, ["count", "flags"]); return subImpl(compile(a[0], flagsOf(a, 4, kw)), a[1], a[2], Number(needInt(a[3] !== undefined ? a[3] : kwget(kw, "count", 0n))), false); });
        fnkw(g, "subn", (a) => { const kw = kwOf(a, ["count", "flags"]); return subImpl(compile(a[0], flagsOf(a, 4, kw)), a[1], a[2], Number(needInt(a[3] !== undefined ? a[3] : kwget(kw, "count", 0n))), true); });
        // re.split(pattern, string, maxsplit=0, flags=0)
        fnkw(g, "split", (a) => { const kw = kwOf(a, ["maxsplit", "flags"]); return call(Pattern.dict.get("split"), [compile(a[0], flagsOf(a, 3, kw)), a[1], a[2] !== undefined ? a[2] : kwget(kw, "maxsplit", 0n)], null); });
        // Positional fast entries on module-level `re.*` functions to bypass
        // `kwOf` / `call` trampolines on common calls.
        {
            const pSearch = Pattern.dict.get("search"), pMatch = Pattern.dict.get("match"), pFull = Pattern.dict.get("fullmatch"), pFindall = Pattern.dict.get("findall"), pSplit = Pattern.dict.get("split"), pSub = Pattern.dict.get("sub");
            let RE_STRM = null;
            try { RE_STRM = typeof __zipp_py_strm === "function" ? __zipp_py_strm : null; } catch (e) { RE_STRM = null; }
            const mtmpl = pSearch.mtmpl;
            const cCompile = g.get("compile");
            cCompile.c1 = function (pat) { return compile(pat, 0n); };
            cCompile.c2 = function (pat, fl) { return compile(pat, isInt(fl) ? asInt(fl) : fail(E.TypeError, "flags must be an int")); };
            const cSearch = g.get("search");
            const csJ2 = function (pat, s) { return pSearch.c2(compile(pat, 0n), s); };
            if (RE_STRM !== null && mtmpl !== undefined) {
                cSearch.strop = 49; cSearch.strCache0 = strCache0; cSearch.Pattern = Pattern; cSearch.mtmpl = mtmpl; cSearch.j2 = csJ2; cSearch.c2 = RE_STRM;
            } else {
                cSearch.c2 = csJ2;
            }
            cSearch.c3 = function (pat, s, fl) { return pSearch.c2(compile(pat, isInt(fl) ? asInt(fl) : fail(E.TypeError, "flags must be an int")), s); };
            const cMatch = g.get("match");
            const cmJ2 = function (pat, s) { return pMatch.c2(compile(pat, 0n), s); };
            if (RE_STRM !== null && mtmpl !== undefined) {
                cMatch.strop = 50; cMatch.strCache0 = strCache0; cMatch.Pattern = Pattern; cMatch.mtmpl = mtmpl; cMatch.j2 = cmJ2; cMatch.c2 = RE_STRM;
            } else {
                cMatch.c2 = cmJ2;
            }
            cMatch.c3 = function (pat, s, fl) { return pMatch.c2(compile(pat, isInt(fl) ? asInt(fl) : fail(E.TypeError, "flags must be an int")), s); };
            const cFull = g.get("fullmatch");
            const cfJ2 = function (pat, s) { return pFull.c2(compile(pat, 0n), s); };
            if (RE_STRM !== null && mtmpl !== undefined) {
                cFull.strop = 51; cFull.strCache0 = strCache0; cFull.Pattern = Pattern; cFull.mtmpl = mtmpl; cFull.j2 = cfJ2; cFull.c2 = RE_STRM;
            } else {
                cFull.c2 = cfJ2;
            }
            cFull.c3 = function (pat, s, fl) { return pFull.c2(compile(pat, isInt(fl) ? asInt(fl) : fail(E.TypeError, "flags must be an int")), s); };
            const cFindall = g.get("findall");
            cFindall.c2 = function (pat, s) { return pFindall.c2(compile(pat, 0n), s); };
            cFindall.c3 = function (pat, s, fl) { return pFindall.c2(compile(pat, isInt(fl) ? asInt(fl) : fail(E.TypeError, "flags must be an int")), s); };
            const cSub = g.get("sub");
            const csubJ3 = function (pat, repl, s) { return pSub.c3(compile(pat, 0n), repl, s); };
            const csubJ4 = function (pat, repl, s, cnt) { return pSub.c4(compile(pat, 0n), repl, s, cnt); };
            if (RE_STRM !== null) {
                cSub.strop = 48; cSub.strCache0 = strCache0; cSub.Pattern = Pattern; cSub.j3 = csubJ3; cSub.j4 = csubJ4; cSub.c3 = RE_STRM; cSub.c4 = RE_STRM;
            } else {
                cSub.c3 = csubJ3; cSub.c4 = csubJ4;
            }
            const cSubn = g.get("subn");
            cSubn.c3 = function (pat, repl, s) { return subImpl(compile(pat, 0n), repl, s, 0, true); };
            const cSplit = g.get("split");
            cSplit.c2 = function (pat, s) { return pSplit.code([compile(pat, 0n), s]); };
            cSplit.c3 = function (pat, s, max) { return pSplit.code([compile(pat, 0n), s, max]); };
        }
        // Only the characters special in a pattern are escaped (3.7+).
        fn(g, "escape", 1, (a) => {
            if (isBytesLike(a[0])) return latin1ToBytes(bytesToLatin1(a[0]).replace(/[()[\]{}?*+\-|^$\\.&~# \t\n\r\x0b\x0c]/g, (c) => "\\" + c));
            return needStr(a[0]).replace(/[()[\]{}?*+\-|^$\\.&~# \t\n\r\x0b\x0c]/g, (c) => "\\" + c);
        });
        fn(g, "purge", 0, () => { cache.clear(); strCache0.clear(); return null; });
        g.set("error", error); g.set("Pattern", Pattern); g.set("Match", Match);
    });

    // ---- dataclasses / enum (the common subsets) --------------------------------------------------------------------------------------------------
    mod("dataclasses", (g) => {
        const MISSING = { cls: pyClass("_MISSING_TYPE", "dataclasses", {}), dict: new Map() };
        g.set("MISSING", MISSING);
        const FrozenInstanceError = rt.newType("FrozenInstanceError", [E.AttributeError], new Map(), "dataclasses");
        rt.allocators.set(FrozenInstanceError, (cls) => rt.makeExc(cls, []));
        g.set("FrozenInstanceError", FrozenInstanceError);
        const Field = pyClass("Field", "dataclasses", { __repr__: (a) => "Field(" + repr(a[0].dict.get("name")) + ")" });
        // InitVar[T] marks an init-only parameter.
        const InitVar = pyClass("InitVar", "dataclasses", {});
        InitVar.dict.set("__class_getitem__", { cls: T.classmethod, func: builtin("__class_getitem__", 2, (a) => ({ cls: InitVar, dict: new Map([["type", a[1]]]) })) });
        g.set("InitVar", InitVar);
        // `ClassVar` / `ClassVar[int]` (a typing alias) or its string form.
        function annotationIs(a, module, name) {
            if (typeof a === "string") return new RegExp("^\\s*(?:" + module + "\\.)?" + name + "\\b").test(a);
            if (a === null || typeof a !== "object") return false;
            if (a.cls === InitVar) return name === "InitVar";
            return a.cls !== undefined && a.cls.module === module && a.dict !== undefined && a.dict.get("name") === name;
        }
        fnkw(g, "field", (a) => { const kw = kwOf(a, ["default", "default_factory", "init", "repr", "compare", "hash", "metadata", "kw_only"]); const f = { cls: Field, dict: new Map([["default", kwget(kw, "default", MISSING)], ["default_factory", kwget(kw, "default_factory", MISSING)], ["init", kwget(kw, "init", true)], ["repr", kwget(kw, "repr", true)], ["compare", kwget(kw, "compare", true)], ["name", null]]) }; return f; });
        function process(cls, kw) {
            const ann = cls.dict.get("__annotations__");
            const names = [];
            // Fields come from annotations when present, else from class attributes that are not callables.
            const annotated = ann && ann.cls === T.dict ? rt.dictEntryList(ann).map((e) => e[0]) : [];
            const fields = [];
            for (const c of cls.mro.slice().reverse()) { const fs = c.dict.get("__dataclass_fields__"); if (fs && c !== cls) for (const [n, f] of rt.dictEntryList(fs)) if (!fields.find((x) => x.name === n)) fields.push({ name: n, dflt: f.dict.get("default"), factory: f.dict.get("default_factory"), init: truth(f.dict.get("init")), repr: truth(f.dict.get("repr")), compare: truth(f.dict.get("compare")) }); }
            // __init__'s parameters: the fields plus InitVar pseudo-fields,
            // which go to __post_init__ instead of the instance.
            const params = fields.slice();
            for (const n of annotated) {
                if (n === "__slots__") continue;
                const a = dictGet(ann, n);
                if (annotationIs(a, "typing", "ClassVar")) continue; // stays a class attribute
                const v = cls.dict.get(n);
                let entry = { name: n, dflt: MISSING, factory: MISSING, init: true, repr: true, compare: true, initVar: a === InitVar || annotationIs(a, "dataclasses", "InitVar") };
                if (v !== undefined && v !== null && typeof v === "object" && v.cls === Field) { entry.dflt = v.dict.get("default"); entry.factory = v.dict.get("default_factory"); entry.init = truth(v.dict.get("init")); entry.repr = truth(v.dict.get("repr")); entry.compare = truth(v.dict.get("compare")); v.dict.set("name", n); cls.dict.delete(n); if (entry.dflt !== MISSING) cls.dict.set(n, entry.dflt); }
                else if (v !== undefined) entry.dflt = v;
                // An unhashable default (list, dict, set) would be shared by every instance.
                const d = entry.dflt;
                if (!entry.initVar && d !== MISSING && d !== null && typeof d === "object" && !isType(d) && typeMethod(d, "__hash__") === null) fail(E.ValueError, "mutable default " + repr(typeOf(d)) + " for field " + n + " is not allowed: use default_factory");
                if (entry.initVar) { cls.dict.delete(n); if (entry.dflt !== MISSING) cls.dict.set(n, entry.dflt); }
                const i = params.findIndex((x) => x.name === n); if (i >= 0) params[i] = entry; else params.push(entry);
            }
            fields.length = 0; for (const f of params) if (!f.initVar) fields.push(f);
            const fieldMap = dict(); for (const f of fields) dictSet(fieldMap, f.name, { cls: Field, dict: new Map([["name", f.name], ["default", f.dflt], ["default_factory", f.factory], ["init", f.init], ["repr", f.repr], ["compare", f.compare]]) });
            cls.dict.set("__dataclass_fields__", fieldMap);
            const frozen = truth(kwget(kw, "frozen", false));
            if (truth(kwget(kw, "init", true)) && !cls.dict.has("__init__")) {
                let seenDefault = null;
                for (const f of params) {
                    if (!f.init) continue;
                    if (f.dflt !== MISSING || f.factory !== MISSING) seenDefault = f;
                    else if (seenDefault !== null) fail(E.TypeError, "non-default argument " + repr(f.name) + " follows default argument " + repr(seenDefault.name));
                }
                const init = builtin("__init__", -1, (a) => {
                    const kwm = a[a.length - 1] instanceof Map ? a.pop() : null;
                    const self = a[0]; let pi = 1;
                    const initVars = [];
                    for (const f of params) {
                        let v;
                        if (!f.init) v = f.factory !== MISSING ? call(f.factory, [], null) : f.dflt;
                        else if (kwm && kwm.has(f.name)) v = kwm.get(f.name);
                        else if (pi < a.length) v = a[pi++];
                        else if (f.factory !== MISSING) v = call(f.factory, [], null);
                        else if (f.dflt !== MISSING) v = f.dflt;
                        else fail(E.TypeError, cls.name + ".__init__() missing required argument: '" + f.name + "'");
                        if (f.initVar) initVars.push(v); else self.dict.set(f.name, v);
                    }
                    if (pi < a.length) fail(E.TypeError, cls.name + ".__init__() takes " + (params.filter((f) => f.init).length + 1) + " positional arguments but " + a.length + " were given");
                    const post = rt.lookupType(cls, "__post_init__"); if (post !== undefined) call(descrGet(post, self, cls), initVars, null);
                    return null;
                });
                init.kwnames = true;
                // Positional fast entries when no InitVar and no __post_init__
                // (avoids args array + kwMap check on dataclass construction).
                if (params.every((f) => !f.initVar) && rt.lookupType(cls, "__post_init__") === undefined) {
                    const initParams = params.filter((f) => f.init);
                    const minReq = initParams.filter((f) => f.dflt === MISSING && f.factory === MISSING).length;
                    const maxPos = Math.min(initParams.length, 6);
                    const runPos = function (self, a1, a2, a3, a4, a5, a6, argc) {
                        const d = self.dict;
                        let pi = 0;
                        for (let i = 0; i < params.length; i++) {
                            const f = params[i];
                            let v;
                            if (!f.init) v = f.factory !== MISSING ? call(f.factory, [], null) : f.dflt;
                            else if (pi < argc) { const k = pi++; v = k === 0 ? a1 : k === 1 ? a2 : k === 2 ? a3 : k === 3 ? a4 : k === 4 ? a5 : a6; }
                            else if (f.factory !== MISSING) v = call(f.factory, [], null);
                            else v = f.dflt;
                            d.set(f.name, v);
                        }
                        return null;
                    };
                    for (let n = minReq; n <= maxPos; n++) {
                        if (n === 0) init.c1 = function (s) { return runPos(s, undefined, undefined, undefined, undefined, undefined, undefined, 0); };
                        else if (n === 1) init.c2 = function (s, a1) { return runPos(s, a1, undefined, undefined, undefined, undefined, undefined, 1); };
                        else if (n === 2) init.c3 = function (s, a1, a2) { return runPos(s, a1, a2, undefined, undefined, undefined, undefined, 2); };
                        else if (n === 3) init.c4 = function (s, a1, a2, a3) { return runPos(s, a1, a2, a3, undefined, undefined, undefined, 3); };
                        else if (n === 4) init.c5 = function (s, a1, a2, a3, a4) { return runPos(s, a1, a2, a3, a4, undefined, undefined, 4); };
                        else if (n === 5) init.c6 = function (s, a1, a2, a3, a4, a5) { return runPos(s, a1, a2, a3, a4, a5, undefined, 5); };
                        else if (n === 6) init.c7 = function (s, a1, a2, a3, a4, a5, a6) { return runPos(s, a1, a2, a3, a4, a5, a6, 6); };
                    }
                    // Single-keyword fast entries (`TagNameMatchRule(present=True)`, `TagNameMatchRule(string=obj)`).
                    for (let k = 0; k < initParams.length; k++) {
                        const kwField = initParams[k].name;
                        if (minReq === 0 || (minReq === 1 && k === 0)) {
                            init["k1:" + kwField] = function (self, val) {
                                const d = self.dict;
                                for (let i = 0; i < params.length; i++) {
                                    const f = params[i];
                                    let v;
                                    if (f.name === kwField) v = val;
                                    else if (f.factory !== MISSING) v = call(f.factory, [], null);
                                    else v = f.dflt;
                                    d.set(f.name, v);
                                }
                                return null;
                            };
                        }
                    }
                }
                cls.dict.set("__init__", init);
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
        // astuple recurses like asdict: into dataclasses, lists, tuples and dicts.
        fn(g, "astuple", 1, (a) => { const conv = (v) => { if (v !== null && typeof v === "object" && v.dict && typeOf(v).dict.has("__dataclass_fields__")) return tuple(rt.dictEntryList(typeOf(v).dict.get("__dataclass_fields__")).map((e) => conv(v.dict.get(e[0])))); if (v !== null && typeof v === "object" && (v.cls === T.list || v.cls === T.tuple)) return rt.sequence(v.cls, v.items.map(conv)); if (v !== null && typeof v === "object" && v.cls === T.dict) { const d = dict(); for (const [k, x] of dictEntries(v)) dictSet(d, conv(k), conv(x)); return d; } return v; }; return conv(a[0]); });
        fn(g, "is_dataclass", 1, (a) => { const c = isType(a[0]) ? a[0] : typeOf(a[0]); return c.dict.has("__dataclass_fields__") || c.mro.some((x) => x.dict.has("__dataclass_fields__")); });
        fnkw(g, "replace", (a) => { const kw = kwOf(a, null); const o = a[0]; const c = typeOf(o); const args = new Map(); for (const [n] of rt.dictEntryList(c.dict.get("__dataclass_fields__"))) args.set(n, kw && kw.has(n) ? kw.get(n) : o.dict.get(n)); return call(c, [], args); });
    });
    mod("enum", (g) => {
        const EnumMeta = rt.TypeType;
        const Enum = rt.newType("Enum", [], new Map(), "enum");
        // A Flag pseudo-member without named bits has no name: <P: 0>, P(0).
        Enum.dict.set("__repr__", builtin("__repr__", 1, (a) => { const n = a[0].dict.get("name"); return "<" + typeOf(a[0]).name + (n === null ? "" : "." + n) + ": " + repr(a[0].dict.get("value")) + ">"; }));
        Enum.dict.set("__str__", builtin("__str__", 1, (a) => { const n = a[0].dict.get("name"); return typeOf(a[0]).name + (n === null ? "(" + repr(a[0].dict.get("value")) + ")" : "." + n); }));
        Enum.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(a[0].dict.get("name"))));
        Enum.dict.set("name", { cls: T.property, fget: builtin("name", 1, (a) => a[0].dict.get("name")), fset: null, fdel: null, doc: null });
        Enum.dict.set("value", { cls: T.property, fget: builtin("value", 1, (a) => a[0].dict.get("value")), fset: null, fdel: null, doc: null });
        Enum.dict.set("__init_subclass__", { cls: T.classmethod, func: builtin("__init_subclass__", -1, (a) => {
            const cls = a[0];
            const members = [], named = [];
            // auto(): one past the largest int value so far; for a Flag, the
            // next power of two above every value so far; for StrEnum, lowercased name.
            const isFlag = rt.isSubclass(cls, Flag);
            const isIntMixin = rt.isSubclass(cls, T.int);
            const isStrMixin = rt.isSubclass(cls, T.str);
            const isFloatMixin = rt.isSubclass(cls, T.float);
            let high = 0n;
            for (const [k, v] of Array.from(cls.dict)) {
                if (k.startsWith("_") || (v !== null && typeof v === "object" && (v.cls === T.function || v.cls === T.property || v.cls === T.staticmethod || v.cls === T.classmethod || v.cls === T.builtin_function_or_method))) continue;
                let value = v;
                if (v !== null && typeof v === "object" && v.isAuto) {
                    if (isStrMixin) value = k.toLowerCase();
                    else if (isFlag) { value = 1n; while (value <= high) value <<= 1n; }
                    else value = members.length || high > 0n ? high + 1n : 1n;
                }
                if (isIntMixin && !isInt(value)) value = needInt(value);
                else if (isStrMixin && typeof value !== "string") value = str(value);
                if (isInt(value) && (members.length === 0 || asInt(value) > high)) high = asInt(value);
                const existing = members.find((m) => eq(m.dict.get("value"), value));
                if (existing) { cls.dict.set(k, existing); named.push([k, existing]); continue; }
                const m = { cls: cls, dict: new Map([["name", k], ["value", value]]) };
                if (isIntMixin) m.pyval = asInt(value);
                else if (isStrMixin) m.pyval = typeof value === "string" ? value : str(value);
                else if (isFloatMixin) m.pyval = toFloat(value);
                members.push(m); named.push([k, m]); cls.dict.set(k, m);
            }
            cls.members = members;
            // A user __init__ (the Planet pattern) sets each member up from
            // its value, a tuple value unpacked into the arguments.
            const init = rt.lookupType(cls, "__init__");
            if (init !== undefined && init !== null && typeof init === "object" && init.cls === T.function) {
                for (const m of members) { const v = m.dict.get("value"); call(descrGet(init, m, cls), v !== null && typeof v === "object" && v.cls === T.tuple ? v.items.slice() : [v], null); }
                // Enum(value) only looks a member up; the construction that
                // returns it must not run __init__ on it again.
                cls.dict.set("__init__", builtin("__init__", -1, () => null));
            }
            // __members__ lists aliases too, under their own names.
            cls.dict.set("__members__", (() => { const d = dict(); for (const [k, m] of named) dictSet(d, k, m); return d; })());
            const mixin = cls.mro.find((c) => c !== rt.ObjectType && rt.constructors.has(c));
            if (mixin !== undefined) {
                const enumIndex = cls.mro.indexOf(Enum), mixinIndex = cls.mro.indexOf(mixin);
                if (!cls.mro.slice(0, mixinIndex).some((c) => c.dict.has("__new__"))) {
                    cls.dict.set("__new__", Enum.dict.get("__new__"));
                }
                for (const name of ["__str__", "__repr__", "__format__"]) {
                    const owner = cls.mro.find((c) => c.dict.has(name));
                    if (owner !== undefined && cls.mro.indexOf(owner) === mixinIndex && mixinIndex < enumIndex) {
                        const fromEnum = cls.mro.slice(enumIndex).find((c) => c.dict.has(name));
                        if (fromEnum !== undefined) cls.dict.set(name, fromEnum.dict.get(name));
                    }
                }
                if (!cls.mro.slice(0, mixinIndex).some((c) => c.dict.has("__eq__"))) {
                    if (mixin !== T.int && mixin !== T.str && mixin !== T.float) {
                        cls.dict.set("__eq__", builtin("__eq__", 2, (a) => a[0] === a[1] || (isInstance(a[1], mixin) && eq(a[0].dict.get("value"), a[1]))));
                    }
                    cls.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(a[0].dict.get("value"))));
                }
            }
            rt.bumpEpoch();
            return null;
        }) });
        rt.constructors.set(Enum, null); rt.constructors.delete(Enum);
        // Enum(value) looks a member up (an int naming a combination of a
        // Flag's bits is its pseudo-member); Enum(name, names) is the
        // functional API. Iterating a class yields members.
        const lookup = (cls, args) => {
            if (args.length === 2) return functional(cls, args[0], args[1]);
            if (args.length !== 1) fail(E.TypeError, "Enum takes one argument");
            const v = args[0], members = cls.members || [];
            for (const m of members) if (eq(m.dict.get("value"), v)) return m;
            if (isInt(v) && members.length && rt.isSubclass(cls, Flag)) {
                let all = 0n; for (const m of members) { const b = m.dict.get("value"); if (isInt(b)) all |= asInt(b); }
                const i = asInt(v);
                if (rt.isSubclass(cls, IntFlag) || (i >= 0n && (i & ~all) === 0n)) return flagValue(cls, i);
                if (i >= 0n) {
                    // CPython's STRICT boundary message, bits aligned to the wider value.
                    const width = Math.max(i.toString(2).length, all.toString(2).length);
                    const bits = (x) => "0b0 " + x.toString(2).padStart(width, "0");
                    fail(E.ValueError, "<flag '" + cls.name + "'> invalid value " + i + "\n    given " + bits(i) + "\n  allowed " + bits(all));
                }
            }
            fail(E.ValueError, repr(v) + " is not a valid " + cls.name);
        };
        // Enum("E", "A B") / ["A", "B"] / [("A", 1), ...] / {"A": 1, ...}:
        // names alone are numbered from 1 (powers of two for a Flag).
        function functional(cls, name, names) {
            if ((cls.members || []).length) fail(E.TypeError, "<enum '" + needStr(name) + "'> cannot extend <enum '" + cls.name + "'>");
            const flag = rt.isSubclass(cls, Flag), ns = new Map();
            if (typeof names === "string") names = list(names.replace(/,/g, " ").split(/\s+/).filter((s) => s.length));
            if (names !== null && typeof names === "object" && names.map !== undefined && isInstance(names, T.dict)) {
                for (const [k, v] of dictEntries(names)) ns.set(needStr(k), v);
            } else {
                const items = drain(names);
                for (let i = 0; i < items.length; i++) {
                    const x = items[i];
                    if (typeof x === "string") ns.set(x, flag ? 1n << BigInt(i) : BigInt(i + 1));
                    else { const p = drain(x); if (p.length !== 2) fail(E.TypeError, "enum member definitions must be names or (name, value) pairs"); ns.set(needStr(p[0]), p[1]); }
                }
            }
            return rt.makeClass(rt.TypeType, needStr(name), [cls], ns, null);
        }
        Enum.dict.set("__new__", { cls: T.staticmethod, func: builtin("__new__", -1, (a) => lookup(a[0], a.slice(1))) });
        rt.enumLookup = lookup;
        Enum.dict.set("__iter__", { cls: T.classmethod, func: builtin("__iter__", 1, (a) => iter(list((a[0].members || []).slice()))) });
        Enum.dict.set("__contains__", { cls: T.classmethod, func: builtin("__contains__", 2, (a) => (a[0].members || []).includes(a[1])) });
        Enum.dict.set("__len__", { cls: T.classmethod, func: builtin("__len__", 1, (a) => BigInt((a[0].members || []).length)) });
        Enum.dict.set("__getitem__", { cls: T.classmethod, func: builtin("__getitem__", 2, (a) => { const ms = a[0].dict.get("__members__"); const m = ms === undefined || typeof a[1] !== "string" ? undefined : dictGet(ms, a[1]); if (m !== undefined) return m; throw rt.makeExc(E.KeyError, [a[1]]); }) });
        g.set("Enum", Enum);
        const IntEnum = rt.newType("IntEnum", [T.int, Enum], new Map(), "enum");
        IntEnum.dict.set("__new__", Enum.dict.get("__new__"));
        IntEnum.dict.set("__int__", builtin("__int__", 1, (a) => asInt(a[0].dict.get("value"))));
        IntEnum.dict.set("__str__", builtin("__str__", 1, (a) => str(a[0].dict.get("value"))));
        IntEnum.dict.set("__repr__", builtin("__repr__", 1, (a) => { const n = a[0].dict.get("name"); return "<" + typeOf(a[0]).name + (n === null ? "" : "." + n) + ": " + repr(a[0].dict.get("value")) + ">"; }));
        IntEnum.dict.set("__format__", builtin("__format__", 2, (a) => rt.formatValue(a[0].dict.get("value"), a[1])));
        IntEnum.dict.set("__index__", builtin("__index__", 1, (a) => asInt(a[0].dict.get("value"))));
        IntEnum.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(a[0].dict.get("value"))));
        g.set("IntEnum", IntEnum);
        const StrEnum = rt.newType("StrEnum", [T.str, Enum], new Map(), "enum");
        StrEnum.dict.set("__new__", Enum.dict.get("__new__"));
        StrEnum.dict.set("__str__", builtin("__str__", 1, (a) => a[0].dict.get("value")));
        StrEnum.dict.set("__repr__", builtin("__repr__", 1, (a) => { const n = a[0].dict.get("name"); return "<" + typeOf(a[0]).name + (n === null ? "" : "." + n) + ": " + repr(a[0].dict.get("value")) + ">"; }));
        StrEnum.dict.set("__format__", builtin("__format__", 2, (a) => rt.formatValue(a[0].dict.get("value"), a[1])));
        StrEnum.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(a[0].dict.get("value"))));
        g.set("StrEnum", StrEnum);
        // Flag members combine bitwise; a combination without its own member
        // is a pseudo-member named after the flags it contains.
        const Flag = rt.newType("Flag", [Enum], new Map(), "enum");
        const flagValue = (cls, v) => {
            for (const m of cls.members || []) if (m.dict.get("value") === v) return m;
            if (cls.composites === undefined) cls.composites = new Map();
            let m = cls.composites.get(v);
            if (m === undefined) {
                const named = (cls.members || []).filter((x) => { const b = x.dict.get("value"); return b !== 0n && (v & b) === b; });
                const parts = named.map((x) => x.dict.get("name"));
                // Bits no member names (an IntFlag keeps them) end the name as a number.
                let covered = 0n; for (const x of named) covered |= x.dict.get("value");
                if (parts.length && (v & ~covered) !== 0n) parts.push(String(v & ~covered));
                m = { cls: cls, dict: new Map([["name", parts.length ? parts.join("|") : null], ["value", v]]) };
                if (rt.isSubclass(cls, T.int)) m.pyval = v;
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
        IntFlag.dict.set("__eq__", builtin("__eq__", 2, (a) => {
            if (isInt(a[1])) return fv(a[0]) === asInt(a[1]);
            if (typeof a[1] === "number") return Number(fv(a[0])) === a[1];
            return a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.int) && fv(a[0]) === asInt(a[1]);
        }));
        IntFlag.dict.set("__str__", builtin("__str__", 1, (a) => str(fv(a[0]))));
        for (const n of ["__or__", "__and__", "__xor__", "__ror__", "__rand__", "__rxor__"]) IntFlag.dict.set(n, builtin(n, 2, (a) => { const other = a[1]; if (!isInt(other) && !(other !== null && typeof other === "object" && other.dict !== undefined && other.dict.has("value"))) return NOTIMPL; return call(Flag.dict.get(n), [a[0], other], null); }));
        g.set("IntFlag", IntFlag);
        fn(g, "auto", 0, () => ({ cls: rt.ObjectType, dict: new Map(), isAuto: true }));
        fn(g, "unique", 1, (a) => {
            const cls = a[0], dups = [];
            for (const [k, m] of rt.dictEntryList(cls.dict.get("__members__") || dict())) if (m.dict.get("name") !== k) dups.push(k + " -> " + m.dict.get("name"));
            if (dups.length) fail(E.ValueError, "duplicate values found in <enum '" + cls.name + "'>: " + dups.join(", "));
            return cls;
        });
    });

    // ---- types --------------------------------------------------------------------------------------
    mod("types", (g) => {
        const SimpleNamespace = rt.newType("SimpleNamespace", [rt.ObjectType], new Map(), "types");
        SimpleNamespace.dict.set("__init__", (() => {
            const f = builtin("__init__", -1, (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                const self = a[0];
                if (a.length > 2) fail(E.TypeError, "SimpleNamespace expected at most 1 arguments, got " + (a.length - 1));
                if (a.length === 2) {
                    const m = a[1];
                    if (m !== null && typeof m === "object" && m.map !== undefined && isInstance(m, T.dict)) {
                        for (const [k, v] of dictEntries(m)) self.dict.set(needStr(k, "keywords"), v);
                    } else {
                        for (const pair of drain(m)) {
                            const p = drain(pair);
                            if (p.length !== 2) fail(E.ValueError, "dictionary update sequence element has length " + p.length + "; 2 is required");
                            self.dict.set(needStr(p[0], "keywords"), p[1]);
                        }
                    }
                }
                if (kw) for (const [k, v] of kw) self.dict.set(k, v);
                return null;
            });
            f.kwnames = true;
            return f;
        })());
        SimpleNamespace.dict.set("__repr__", builtin("__repr__", 1, (a) => {
            const parts = [];
            for (const [k, v] of a[0].dict) parts.push(k + "=" + repr(v));
            const t = typeOf(a[0]);
            return (t === SimpleNamespace ? "namespace" : t.name) + "(" + parts.join(", ") + ")";
        }));
        SimpleNamespace.dict.set("__eq__", builtin("__eq__", 2, (a) => {
            if (!isInstance(a[1], SimpleNamespace)) return NOTIMPL;
            const d1 = a[0].dict, d2 = a[1].dict;
            if (d1.size !== d2.size) return false;
            for (const [k, v] of d1) if (!d2.has(k) || !eq(v, d2.get(k))) return false;
            return true;
        }));
        SimpleNamespace.dict.set("__replace__", (() => {
            const f = builtin("__replace__", -1, (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                const d = new Map(a[0].dict);
                if (kw) for (const [k, v] of kw) d.set(k, v);
                return { cls: typeOf(a[0]), dict: d };
            });
            f.kwnames = true;
            return f;
        })());
        g.set("SimpleNamespace", SimpleNamespace);
        const MappingProxyType = rt.newType("mappingproxy", [rt.ObjectType], new Map(), "types");
        rt.constructors.set(MappingProxyType, (a) => {
            if (a.length !== 1) fail(E.TypeError, "mappingproxy() takes exactly 1 argument");
            const src = a[0];
            if (src === null || typeof src !== "object" || (src.map === undefined && rt.lookupType(typeOf(src), "__getitem__") === undefined)) {
                fail(E.TypeError, "mappingproxy() argument must be a mapping, not " + typeOf(src).name);
            }
            return { cls: MappingProxyType, dict: new Map(), _src: src };
        });
        MappingProxyType.dict.set("__getitem__", builtin("__getitem__", 2, (a) => getitem(a[0]._src, a[1])));
        MappingProxyType.dict.set("__contains__", builtin("__contains__", 2, (a) => rt.contains(a[0]._src, a[1])));
        MappingProxyType.dict.set("__len__", builtin("__len__", 1, (a) => rt.len(a[0]._src)));
        MappingProxyType.dict.set("__iter__", builtin("__iter__", 1, (a) => iter(a[0]._src)));
        MappingProxyType.dict.set("get", builtin("get", 3, (a) => call(rt.getattr(a[0]._src, "get"), a.length >= 3 ? [a[1], a[2]] : [a[1]], null), 2));
        MappingProxyType.dict.set("keys", builtin("keys", 1, (a) => call(rt.getattr(a[0]._src, "keys"), [], null)));
        MappingProxyType.dict.set("values", builtin("values", 1, (a) => call(rt.getattr(a[0]._src, "values"), [], null)));
        MappingProxyType.dict.set("items", builtin("items", 1, (a) => call(rt.getattr(a[0]._src, "items"), [], null)));
        MappingProxyType.dict.set("copy", builtin("copy", 1, (a) => call(rt.getattr(a[0]._src, "copy"), [], null)));
        MappingProxyType.dict.set("__eq__", builtin("__eq__", 2, (a) => eq(a[0]._src, a[1] !== null && typeof a[1] === "object" && a[1].cls === MappingProxyType ? a[1]._src : a[1])));
        MappingProxyType.dict.set("__repr__", builtin("__repr__", 1, (a) => "mappingproxy(" + repr(a[0]._src) + ")"));
        g.set("MappingProxyType", MappingProxyType);
        g.set("FunctionType", T.function);
        g.set("LambdaType", T.function);
        g.set("MethodType", T.method);
        g.set("ModuleType", T.module);
        g.set("GeneratorType", T.generator);
        g.set("CoroutineType", T.coroutine);
        g.set("AsyncGeneratorType", T.async_generator);
        g.set("BuiltinFunctionType", T.builtin_function_or_method);
        g.set("BuiltinMethodType", T.builtin_function_or_method);
        g.set("GenericAlias", rt.GenericAlias);
        g.set("UnionType", rt.UnionType);
        g.set("NoneType", T.NoneType);
        g.set("EllipsisType", T.ellipsis);
        g.set("NotImplementedType", T.NotImplementedType);
        g.set("CellType", T.cell);
        g.set("CodeType", T.code);
        g.set("FrameType", T.frame);
        g.set("TracebackType", T.traceback);
        fnkw(g, "prepare_class", (a) => {
            const name = needStr(a[0]);
            const bases = a[1] === undefined ? [] : drain(a[1]);
            const kwds = a[2] !== undefined && a[2] !== null ? rt.asDict(a[2]) : dict();
            const kw = new Map();
            let meta = rt.TypeType;
            for (const [k, v] of dictEntries(kwds)) {
                if (k === "metaclass") meta = v;
                else kw.set(k, v);
            }
            for (const b of bases) {
                const m = b.cls || rt.TypeType;
                if (rt.isSubclass(m, meta)) meta = m;
            }
            const prep = rt.lookupType(meta, "__prepare__");
            const ns = prep !== undefined ? call(descrGet(prep, null, meta), [name, tuple(bases)], kw) : dict();
            return tuple([meta, ns, rt.dictFromMap(kw)]);
        });
        fnkw(g, "new_class", (a) => {
            const name = needStr(a[0]);
            const bases = a[1] === undefined ? [] : drain(a[1]);
            const kwds = a[2] !== undefined && a[2] !== null ? rt.asDict(a[2]) : null;
            const execBody = a[3] === undefined ? null : a[3];
            const res = call(g.get("prepare_class"), [name, tuple(bases), kwds], null).items;
            const meta = res[0], ns = res[1], kw = rt.mapFromDict(res[2]);
            if (execBody !== null) call(execBody, [ns], null);
            return call(meta, [name, tuple(bases), ns], kw);
        });
    });

    // ---- weakref ------------------------------------------------------------------------------------
    mod("weakref", (g) => {
        const hasWR = typeof WeakRef === "function";
        const mkWR = (obj) => hasWR ? new WeakRef(obj) : { deref: () => obj };
        const canWeakref = (obj) => {
            if (obj === null || typeof obj !== "object") return false;
            const c = obj.cls;
            if (c === T.list || c === T.tuple || c === T.dict || c === T.bytes || c === T.bytearray || c === T.range || c === T.slice || c === T.complex) return false;
            return true;
        };
        const refMap = typeof WeakMap === "function" ? new WeakMap() : null;
        const trackRef = (obj, r) => {
            if (refMap === null) return;
            let arr = refMap.get(obj);
            if (arr === undefined) { arr = []; refMap.set(obj, arr); }
            arr.push(mkWR(r));
        };
        const liveRefs = (obj) => {
            if (refMap === null || obj === null || typeof obj !== "object") return [];
            const arr = refMap.get(obj);
            if (arr === undefined) return [];
            const out = [];
            for (let i = 0; i < arr.length; i++) {
                const r = arr[i].deref();
                if (r !== undefined && r._wr.deref() === obj) out.push(r);
            }
            return out;
        };
        const defaultRefMap = typeof WeakMap === "function" ? new WeakMap() : null;
        const RefType = rt.newType("ReferenceType", [rt.ObjectType], new Map(), "weakref");
        rt.constructors.set(RefType, (a, _kw, cls) => {
            if (a.length < 1 || a.length > 2) fail(E.TypeError, "ref expected at most 2 arguments, got " + a.length);
            const obj = a[0], cb = a[1] === undefined ? null : a[1];
            if (!canWeakref(obj)) fail(E.TypeError, "cannot create weak reference to '" + typeOf(obj).name + "' object");
            const targetCls = cls || RefType;
            if (cb === null && targetCls === RefType && defaultRefMap !== null) {
                const cached = defaultRefMap.get(obj);
                if (cached !== undefined) {
                    const live = cached.deref();
                    if (live !== undefined && live._wr.deref() === obj) return live;
                }
            }
            const r = { cls: targetCls, dict: new Map(), _wr: mkWR(obj), _cb: cb, _hash: undefined };
            if (cb === null && targetCls === RefType && defaultRefMap !== null) defaultRefMap.set(obj, mkWR(r));
            trackRef(obj, r);
            return r;
        });
        RefType.dict.set("__call__", builtin("__call__", 1, (a) => { const v = a[0]._wr.deref(); return v === undefined ? null : v; }));
        RefType.dict.set("__callback__", { cls: T.property, fget: builtin("__callback__", 1, (a) => a[0]._cb), fset: null, fdel: null, doc: null });
        RefType.dict.set("__eq__", builtin("__eq__", 2, (a) => {
            if (!isInstance(a[1], RefType)) return NOTIMPL;
            const x = a[0]._wr.deref(), y = a[1]._wr.deref();
            if (x === undefined || y === undefined) return a[0] === a[1];
            return eq(x, y);
        }));
        RefType.dict.set("__hash__", builtin("__hash__", 1, (a) => {
            if (a[0]._hash !== undefined) return a[0]._hash;
            const v = a[0]._wr.deref();
            if (v === undefined) fail(E.TypeError, "weak object has gone away");
            a[0]._hash = rt.hashInt(v);
            return a[0]._hash;
        }));
        RefType.dict.set("__repr__", builtin("__repr__", 1, (a) => {
            const v = a[0]._wr.deref();
            const id = rt.ident(a[0]).toString(16).padStart(8, "0");
            if (v === undefined) return "<weakref at 0x" + id + "; dead>";
            const t = typeOf(v);
            const name = typeof v.name === "string" ? " '" + v.name + "'" : "";
            return "<weakref at 0x" + id + "; to '" + t.name + "'" + name + " at 0x" + rt.ident(v).toString(16).padStart(8, "0") + ">";
        }));
        g.set("ref", RefType);
        g.set("ReferenceType", RefType);

        const derefProxy = (p) => {
            const v = p._wr.deref();
            if (v === undefined) fail(E.ReferenceError, "weakly-referenced object no longer exists");
            return v;
        };
        const ProxyType = rt.newType("ProxyType", [rt.ObjectType], new Map(), "weakref");
        const CallableProxyType = rt.newType("CallableProxyType", [ProxyType], new Map(), "weakref");
        ProxyType.dict.set("__getattribute__", builtin("__getattribute__", 2, (a) => rt.getattr(derefProxy(a[0]), a[1])));
        ProxyType.dict.set("__setattr__", builtin("__setattr__", 3, (a) => rt.setattr(derefProxy(a[0]), a[1], a[2])));
        ProxyType.dict.set("__delattr__", builtin("__delattr__", 2, (a) => R.delattr(derefProxy(a[0]), a[1])));
        ProxyType.dict.set("__repr__", builtin("__repr__", 1, (a) => repr(derefProxy(a[0]))));
        ProxyType.dict.set("__str__", builtin("__str__", 1, (a) => str(derefProxy(a[0]))));
        ProxyType.dict.set("__eq__", builtin("__eq__", 2, (a) => eq(derefProxy(a[0]), a[1])));
        ProxyType.dict.set("__bool__", builtin("__bool__", 1, (a) => truth(derefProxy(a[0]))));
        ProxyType.dict.set("__len__", builtin("__len__", 1, (a) => rt.len(derefProxy(a[0]))));
        ProxyType.dict.set("__iter__", builtin("__iter__", 1, (a) => iter(derefProxy(a[0]))));
        ProxyType.dict.set("__getitem__", builtin("__getitem__", 2, (a) => getitem(derefProxy(a[0]), a[1])));
        ProxyType.dict.set("__setitem__", builtin("__setitem__", 3, (a) => { R.setitem(derefProxy(a[0]), a[1], a[2]); return null; }));
        ProxyType.dict.set("__delitem__", builtin("__delitem__", 2, (a) => { R.delitem(derefProxy(a[0]), a[1]); return null; }));
        ProxyType.dict.set("__contains__", builtin("__contains__", 2, (a) => rt.contains(derefProxy(a[0]), a[1])));
        CallableProxyType.dict.set("__call__", (() => {
            const f = builtin("__call__", -1, (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                return call(derefProxy(a[0]), a.slice(1), kw);
            });
            f.kwnames = true;
            return f;
        })());
        g.set("ProxyType", ProxyType);
        g.set("CallableProxyType", CallableProxyType);
        g.set("ProxyTypes", tuple([ProxyType, CallableProxyType]));
        fn(g, "proxy", 2, (a) => {
            const obj = a[0], cb = a[1] === undefined ? null : a[1];
            if (!canWeakref(obj)) fail(E.TypeError, "cannot create weak reference to '" + typeOf(obj).name + "' object");
            const cls = rt.isCallable(obj) ? CallableProxyType : ProxyType;
            const p = { cls: cls, dict: new Map(), _wr: mkWR(obj), _cb: cb };
            trackRef(obj, p);
            return p;
        }, 1);
        fn(g, "getweakrefcount", 1, (a) => BigInt(liveRefs(a[0]).length));
        fn(g, "getweakrefs", 1, (a) => list(liveRefs(a[0])));

        const WeakValueDictionary = pyClass("WeakValueDictionary", "weakref", {
            __init__: (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                a[0]._data = dict();
                if (a[1] !== undefined) {
                    const src = a[1];
                    const pairs = src !== null && typeof src === "object" && src.map !== undefined && isInstance(src, T.dict) ? dictEntries(src) : drain(src).map((p) => drain(p));
                    for (const [k, v] of pairs) dictSet(a[0]._data, k, call(RefType, [v], null));
                }
                if (kw) for (const [k, v] of kw) dictSet(a[0]._data, k, call(RefType, [v], null));
                return null;
            },
            __getitem__: (a) => { const r = dictGet(a[0]._data, a[1]); const v = r === undefined ? undefined : r._wr.deref(); if (v === undefined) throw rt.makeExc(E.KeyError, [a[1]]); return v; },
            __setitem__: (a) => { dictSet(a[0]._data, a[1], call(RefType, [a[2]], null)); return null; },
            __delitem__: (a) => { if (!rt.dictDel(a[0]._data, a[1])) throw rt.makeExc(E.KeyError, [a[1]]); return null; },
            __contains__: (a) => { const r = dictGet(a[0]._data, a[1]); return r !== undefined && r._wr.deref() !== undefined; },
            __len__: (a) => { let n = 0n; for (const [, r] of dictEntries(a[0]._data)) if (r._wr.deref() !== undefined) n++; return n; },
            __iter__: (a) => { const ks = []; for (const [k, r] of dictEntries(a[0]._data)) if (r._wr.deref() !== undefined) ks.push(k); return iter(list(ks)); },
            get: (a) => { const r = dictGet(a[0]._data, a[1]); const v = r === undefined ? undefined : r._wr.deref(); return v === undefined ? (a[2] === undefined ? null : a[2]) : v; },
            keys: (a) => { const ks = []; for (const [k, r] of dictEntries(a[0]._data)) if (r._wr.deref() !== undefined) ks.push(k); return list(ks); },
            values: (a) => { const vs = []; for (const [, r] of dictEntries(a[0]._data)) { const v = r._wr.deref(); if (v !== undefined) vs.push(v); } return list(vs); },
            items: (a) => { const ps = []; for (const [k, r] of dictEntries(a[0]._data)) { const v = r._wr.deref(); if (v !== undefined) ps.push(tuple([k, v])); } return list(ps); },
            pop: (a) => { const r = dictGet(a[0]._data, a[1]); const v = r === undefined ? undefined : r._wr.deref(); if (v !== undefined) { rt.dictDel(a[0]._data, a[1]); return v; } if (a.length >= 3) return a[2]; throw rt.makeExc(E.KeyError, [a[1]]); },
            clear: (a) => { rt.dictClear(a[0]._data); return null; },
        });
        WeakValueDictionary.dict.get("__init__").kwnames = true;
        g.set("WeakValueDictionary", WeakValueDictionary);

        const WeakKeyDictionary = pyClass("WeakKeyDictionary", "weakref", {
            __init__: (a) => {
                a[0]._entries = [];
                if (a[1] !== undefined) {
                    const src = a[1];
                    const pairs = src !== null && typeof src === "object" && src.map !== undefined && isInstance(src, T.dict) ? dictEntries(src) : drain(src).map((p) => drain(p));
                    for (const [k, v] of pairs) a[0]._entries.push([call(RefType, [k], null), v]);
                }
                return null;
            },
            __getitem__: (a) => { for (const [r, v] of a[0]._entries) { const k = r._wr.deref(); if (k !== undefined && eq(k, a[1])) return v; } throw rt.makeExc(E.KeyError, [a[1]]); },
            __setitem__: (a) => {
                const ref = call(RefType, [a[1]], null);
                for (let i = 0; i < a[0]._entries.length; i++) {
                    const k = a[0]._entries[i][0]._wr.deref();
                    if (k !== undefined && eq(k, a[1])) { a[0]._entries[i][1] = a[2]; return null; }
                }
                a[0]._entries.push([ref, a[2]]);
                return null;
            },
            __delitem__: (a) => {
                for (let i = 0; i < a[0]._entries.length; i++) {
                    const k = a[0]._entries[i][0]._wr.deref();
                    if (k !== undefined && eq(k, a[1])) { a[0]._entries.splice(i, 1); return null; }
                }
                throw rt.makeExc(E.KeyError, [a[1]]);
            },
            __contains__: (a) => { for (const [r] of a[0]._entries) { const k = r._wr.deref(); if (k !== undefined && eq(k, a[1])) return true; } return false; },
            __len__: (a) => { let n = 0n; for (const [r] of a[0]._entries) if (r._wr.deref() !== undefined) n++; return n; },
            __iter__: (a) => { const ks = []; for (const [r] of a[0]._entries) { const k = r._wr.deref(); if (k !== undefined) ks.push(k); } return iter(list(ks)); },
            get: (a) => { for (const [r, v] of a[0]._entries) { const k = r._wr.deref(); if (k !== undefined && eq(k, a[1])) return v; } return a[2] === undefined ? null : a[2]; },
            clear: (a) => { a[0]._entries.length = 0; return null; },
        });
        g.set("WeakKeyDictionary", WeakKeyDictionary);

        const WeakSet = pyClass("WeakSet", "weakref", {
            __init__: (a) => {
                a[0]._refs = [];
                if (a[1] !== undefined) for (const x of drain(a[1])) a[0]._refs.push(call(RefType, [x], null));
                return null;
            },
            add: (a) => {
                const ref = call(RefType, [a[1]], null);
                for (const r of a[0]._refs) { const v = r._wr.deref(); if (v !== undefined && eq(v, a[1])) return null; }
                a[0]._refs.push(ref);
                return null;
            },
            discard: (a) => {
                for (let i = 0; i < a[0]._refs.length; i++) {
                    const v = a[0]._refs[i]._wr.deref();
                    if (v !== undefined && eq(v, a[1])) { a[0]._refs.splice(i, 1); return null; }
                }
                return null;
            },
            remove: (a) => {
                for (let i = 0; i < a[0]._refs.length; i++) {
                    const v = a[0]._refs[i]._wr.deref();
                    if (v !== undefined && eq(v, a[1])) { a[0]._refs.splice(i, 1); return null; }
                }
                throw rt.makeExc(E.KeyError, [a[1]]);
            },
            __contains__: (a) => { for (const r of a[0]._refs) { const v = r._wr.deref(); if (v !== undefined && eq(v, a[1])) return true; } return false; },
            __len__: (a) => { let n = 0n; for (const r of a[0]._refs) if (r._wr.deref() !== undefined) n++; return n; },
            __iter__: (a) => { const vs = []; for (const r of a[0]._refs) { const v = r._wr.deref(); if (v !== undefined) vs.push(v); } return iter(list(vs)); },
            clear: (a) => { a[0]._refs.length = 0; return null; },
        });
        g.set("WeakSet", WeakSet);

        const Finalize = pyClass("finalize", "weakref", {
            __init__: (a) => {
                const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
                if (a.length < 3) fail(E.TypeError, "finalize() requires at least 2 arguments");
                a[0]._ref = call(RefType, [a[1]], null);
                a[0]._func = a[2];
                a[0]._args = a.slice(3);
                a[0]._kw = kw;
                a[0]._alive = true;
                return null;
            },
            __call__: (a) => {
                if (!a[0]._alive) return null;
                a[0]._alive = false;
                return call(a[0]._func, a[0]._args, a[0]._kw);
            },
            detach: (a) => {
                if (!a[0]._alive) return null;
                a[0]._alive = false;
                const obj = a[0]._ref._wr.deref();
                if (obj === undefined) return null;
                return tuple([obj, a[0]._func, tuple(a[0]._args), a[0]._kw ? rt.dictFromMap(a[0]._kw) : dict()]);
            },
            peek: (a) => {
                if (!a[0]._alive) return null;
                const obj = a[0]._ref._wr.deref();
                if (obj === undefined) return null;
                return tuple([obj, a[0]._func, tuple(a[0]._args), a[0]._kw ? rt.dictFromMap(a[0]._kw) : dict()]);
            },
        });
        Finalize.dict.get("__init__").kwnames = true;
        Finalize.dict.set("alive", { cls: T.property, fget: builtin("alive", 1, (a) => a[0]._alive && a[0]._ref._wr.deref() !== undefined), fset: null, fdel: null, doc: null });
        g.set("finalize", Finalize);
    });

    // ---- zlib ------------------------------------------------------------------------------------------------------------------------
    mod("zlib", (g) => {
        const ZlibError = pyClass("error", "zlib", {}, [E.Exception]);
        g.set("error", ZlibError);
        g.set("DEFLATED", 8n);
        g.set("MAX_WBITS", 15n);
        g.set("DEF_MEM_LEVEL", 8n);
        g.set("DEF_BUF_SIZE", 16384n);
        g.set("Z_DEFAULT_COMPRESSION", -1n);
        g.set("Z_NO_COMPRESSION", 0n);
        g.set("Z_BEST_SPEED", 1n);
        g.set("Z_BEST_COMPRESSION", 9n);
        g.set("Z_DEFAULT_STRATEGY", 0n);
        g.set("Z_FILTERED", 1n);
        g.set("Z_HUFFMAN_ONLY", 2n);
        g.set("Z_RLE", 3n);
        g.set("Z_FIXED", 4n);
        g.set("Z_NO_FLUSH", 0n);
        g.set("Z_PARTIAL_FLUSH", 1n);
        g.set("Z_SYNC_FLUSH", 2n);
        g.set("Z_FULL_FLUSH", 3n);
        g.set("Z_FINISH", 4n);
        let ZLIB_NATIVE = null;
        try { ZLIB_NATIVE = typeof __zipp_py_zlib === "function" ? __zipp_py_zlib : null; } catch (e) { ZLIB_NATIVE = null; }
        function needBytesItems(v) {
            if (v !== null && typeof v === "object" && (v.cls === T.bytes || v.cls === T.bytearray || v.cls === T.memoryview)) {
                return v.cls === T.memoryview ? v.items : v.items;
            }
            fail(E.TypeError, "a bytes-like object is required, not '" + typeOf(v).name + "'");
        }
        function callNative(op, items, a, b) {
            if (ZLIB_NATIVE === null) fail(ZlibError, "native zlib backend unavailable");
            try { return ZLIB_NATIVE(op, items, a, b); }
            catch (e) { fail(ZlibError, e && e.message ? String(e.message).replace(/^Error:\s*/, "") : String(e)); }
        }
        fnkw(g, "compress", (a) => {
            const kw = kwOf(a, ["level", "wbits"]);
            const items = needBytesItems(a[0]);
            const lvlV = a[1] !== undefined ? a[1] : kwget(kw, "level", -1n);
            const wbV = a[2] !== undefined ? a[2] : kwget(kw, "wbits", 15n);
            return rt.bytes(callNative(0, items, Number(needInt(lvlV)), Number(needInt(wbV))));
        });
        fnkw(g, "decompress", (a) => {
            const kw = kwOf(a, ["wbits", "bufsize"]);
            const items = needBytesItems(a[0]);
            const wbV = a[1] !== undefined ? a[1] : kwget(kw, "wbits", 15n);
            return rt.bytes(callNative(1, items, Number(needInt(wbV)), 0));
        });
        fn(g, "crc32", 2, (a) => {
            const items = needBytesItems(a[0]);
            const start = a[1] === undefined ? 0 : Number(needInt(a[1])) >>> 0;
            return BigInt(ZLIB_NATIVE !== null ? (ZLIB_NATIVE(2, items, start) >>> 0) : rt.crc32(items, start));
        }, 1);
        fn(g, "adler32", 2, (a) => {
            const items = needBytesItems(a[0]);
            const start = a[1] === undefined ? 1 : Number(needInt(a[1])) >>> 0;
            return BigInt(callNative(3, items, start) >>> 0);
        }, 1);
        const Compress = pyClass("Compress", "zlib", {
            compress: (a) => {
                const self = a[0];
                if (self._flushed) fail(ZlibError, "Compressor already flushed");
                const items = needBytesItems(a[1]);
                for (let i = 0; i < items.length; i++) self._buf.push(items[i]);
                return rt.bytes([]);
            },
            flush: (a) => {
                const self = a[0];
                if (self._flushed) return rt.bytes([]);
                const mode = a[1] === undefined ? 4 : Number(needInt(a[1]));
                if (mode === 4) {
                    self._flushed = true;
                    return rt.bytes(callNative(0, self._buf, self._level, self._wbits));
                }
                return rt.bytes([]);
            },
            copy: (a) => ({ cls: Compress, dict: new Map(), _buf: a[0]._buf.slice(), _level: a[0]._level, _wbits: a[0]._wbits, _flushed: a[0]._flushed }),
        });
        fnkw(g, "compressobj", (a) => {
            const kw = kwOf(a, ["level", "method", "wbits", "memLevel", "strategy", "zdict"]);
            const lvl = Number(needInt(a[0] !== undefined ? a[0] : kwget(kw, "level", -1n)));
            const wb = Number(needInt(a[2] !== undefined ? a[2] : kwget(kw, "wbits", 15n)));
            if ((lvl !== -1 && (lvl < 0 || lvl > 9))) fail(E.ValueError, "Invalid initialization option");
            return { cls: Compress, dict: new Map(), _buf: [], _level: lvl, _wbits: wb, _flushed: false };
        });
        const Decompress = pyClass("Decompress", "zlib", {
            decompress: (a) => {
                const self = a[0];
                const items = needBytesItems(a[1]);
                const maxLen = a[2] === undefined ? 0 : Number(needInt(a[2]));
                for (let i = 0; i < items.length; i++) self._buf.push(items[i]);
                const out = callNative(1, self._buf, self._wbits, maxLen);
                self._buf = [];
                self._eof = true;
                return rt.bytes(out);
            },
            flush: (a) => {
                const self = a[0];
                if (self._buf.length === 0) return rt.bytes([]);
                const out = callNative(1, self._buf, self._wbits, 0);
                self._buf = [];
                self._eof = true;
                return rt.bytes(out);
            },
        });
        Decompress.dict.set("eof", { cls: T.property, fget: builtin("eof", 1, (a) => a[0]._eof), fset: null, fdel: null, doc: null });
        Decompress.dict.set("unused_data", { cls: T.property, fget: builtin("unused_data", 1, () => rt.bytes([])), fset: null, fdel: null, doc: null });
        Decompress.dict.set("unconsumed_tail", { cls: T.property, fget: builtin("unconsumed_tail", 1, () => rt.bytes([])), fset: null, fdel: null, doc: null });
        fnkw(g, "decompressobj", (a) => {
            const kw = kwOf(a, ["wbits", "zdict"]);
            const wb = Number(needInt(a[0] !== undefined ? a[0] : kwget(kw, "wbits", 15n)));
            return { cls: Decompress, dict: new Map(), _buf: [], _wbits: wb, _eof: false };
        });
    });

    // ---- shutil ----------------------------------------------------------------------------------------------------------------------
    mod("shutil", (g) => {
        const ShutilError = pyClass("Error", "shutil", {}, [E.Exception]);
        const SameFileError = pyClass("SameFileError", "shutil", {}, [ShutilError]);
        g.set("Error", ShutilError);
        g.set("SameFileError", SameFileError);
        const fspath = (p) => {
            if (p !== null && typeof p === "object" && p.cls !== T.bytes && rt.typeMethod(p, "__fspath__") !== undefined) p = rt.callMethod(p, "__fspath__", []);
            return needStr(p);
        };
        const copyFileImpl = (src, dst) => {
            const sNorm = rt.vfs.norm(src), dNorm = rt.vfs.norm(dst);
            if (sNorm === dNorm) fail(SameFileError, repr(src) + " and " + repr(dst) + " are the same file");
            const data = rt.vfs.get(sNorm);
            if (data === undefined) fail(E.FileNotFoundError, "[Errno 2] No such file or directory: '" + src + "'");
            if (rt.vfs.isDir(dNorm) && !rt.vfs.has(dNorm)) fail(E.IsADirectoryError, "[Errno 21] Is a directory: '" + dst + "'");
            rt.vfs.set(dNorm, new Uint8Array(data));
            return dst;
        };
        fnkw(g, "copyfile", (a) => { kwOf(a, ["follow_symlinks"]); return copyFileImpl(fspath(a[0]), fspath(a[1])); });
        const copyImpl = (a) => {
            kwOf(a, ["follow_symlinks"]);
            const src = fspath(a[0]);
            let dst = fspath(a[1]);
            const dNorm = rt.vfs.norm(dst);
            if (rt.vfs.isDir(dNorm) && !rt.vfs.has(dNorm)) {
                const base = src.replace(/\/+$/, "").split("/").pop();
                dst = dst.endsWith("/") ? dst + base : dst + "/" + base;
            }
            return copyFileImpl(src, dst);
        };
        fnkw(g, "copy", copyImpl);
        fnkw(g, "copy2", copyImpl);
        fn(g, "copyfileobj", 3, (a) => {
            const fsrc = a[0], fdst = a[1], len = a[2] === undefined ? 65536n : needInt(a[2]);
            while (true) {
                const buf = rt.callMethod(fsrc, "read", [len]);
                if (!truth(buf)) break;
                rt.callMethod(fdst, "write", [buf]);
            }
            return null;
        }, 2);
        fnkw(g, "rmtree", (a) => {
            const kw = kwOf(a, ["ignore_errors", "onerror"]);
            const ignore = truth(a[1] !== undefined ? a[1] : kwget(kw, "ignore_errors", false));
            const root = fspath(a[0]), norm = rt.vfs.norm(root);
            if (!rt.vfs.isDir(norm) && !rt.vfs.has(norm)) {
                if (ignore) return null;
                fail(E.FileNotFoundError, "[Errno 2] No such file or directory: '" + root + "'");
            }
            const walk = (dir) => {
                for (const name of rt.vfs.listDir(dir)) {
                    const child = dir === "" || dir === "." ? name : dir + "/" + name;
                    if (rt.vfs.isDir(child) && !rt.vfs.has(child)) walk(child);
                    else rt.vfs.remove(child);
                }
                rt.vfs.rmdir(dir);
            };
            walk(norm);
            return null;
        });
        fnkw(g, "copytree", (a) => {
            const kw = kwOf(a, ["dirs_exist_ok"]);
            const dirsOk = truth(kwget(kw, "dirs_exist_ok", false));
            const src = fspath(a[0]), dst = fspath(a[1]);
            const sNorm = rt.vfs.norm(src), dNorm = rt.vfs.norm(dst);
            if (!rt.vfs.isDir(sNorm)) fail(E.FileNotFoundError, "[Errno 2] No such file or directory: '" + src + "'");
            if (rt.vfs.isDir(dNorm) && !dirsOk) fail(E.FileExistsError, "[Errno 17] File exists: '" + dst + "'");
            const walk = (sDir, dDir) => {
                rt.vfs.mkdir(dDir, true);
                for (const name of rt.vfs.listDir(sDir)) {
                    const sc = sDir === "" || sDir === "." ? name : sDir + "/" + name;
                    const dc = dDir === "" || dDir === "." ? name : dDir + "/" + name;
                    if (rt.vfs.isDir(sc) && !rt.vfs.has(sc)) walk(sc, dc);
                    else { const bytes = rt.vfs.get(sc); if (bytes !== undefined) rt.vfs.set(dc, new Uint8Array(bytes)); }
                }
            };
            walk(sNorm, dNorm);
            return dst;
        });
        fn(g, "move", 2, (a) => {
            const src = fspath(a[0]);
            let dst = fspath(a[1]);
            const sNorm = rt.vfs.norm(src), dNorm = rt.vfs.norm(dst);
            if (rt.vfs.isDir(dNorm) && !rt.vfs.has(dNorm)) {
                const base = src.replace(/\/+$/, "").split("/").pop();
                dst = dst.endsWith("/") ? dst + base : dst + "/" + base;
            }
            const finalDst = rt.vfs.norm(dst);
            const data = rt.vfs.get(sNorm);
            if (data === undefined) fail(E.FileNotFoundError, "[Errno 2] No such file or directory: '" + src + "'");
            rt.vfs.set(finalDst, data);
            rt.vfs.remove(sNorm);
            return dst;
        });
    });
})(__zipp_py);
