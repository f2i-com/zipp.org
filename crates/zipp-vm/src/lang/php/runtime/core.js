/* ZIPP PHP runtime — core value model, arithmetic, comparisons, scopes, output buffering. Apache-2.0.
 *
 * Value representation on ZIPP's JS VM:
 *   null    -> PHP null
 *   boolean -> PHP bool
 *   bigint  -> PHP int (exact signed 64-bit; arithmetic overflow promotes to float)
 *   number  -> PHP float (IEEE-754 f64)
 *   string  -> PHP byte-string (Latin-1 code units U+0000..=U+00FF; O(1) strlen/index/rope concat)
 *   object with __php === 1 -> PhpArray (ordered int/string map, sticky-shared CoW)
 *   object with __php === 2 -> PhpObject (class/enum instance)
 *   object with __php === 3 -> PhpClosure / PhpFunc
 *   object with __php === 4 -> PhpResource
 *   object with __php === 5 -> PhpRef (reference cell { __php: 5, v })
 */
var __zipp_php = (function () {
    "use strict";
    const R = {};
    const rt = {};
    R.__rt = rt;

    const I64_MIN = -9223372036854775808n;
    const I64_MAX = 9223372036854775807n;
    const U64_MASK = 0xFFFFFFFFFFFFFFFFn;
    rt.I64_MIN = I64_MIN;
    rt.I64_MAX = I64_MAX;

    // ---- Internal exit signal ----
    function PhpExit(code) {
        this.isPhpExit = true;
        this.code = code;
    }
    rt.PhpExit = PhpExit;

    // ---- Byte-string <-> UTF-8 helpers ----
    // Pure ASCII strings (the common case) pass through in O(1) after a quick scan.
    function bytesToUtf8(s) {
        let ascii = true;
        for (let i = 0; i < s.length; i++) {
            if (s.charCodeAt(i) >= 0x80) { ascii = false; break; }
        }
        if (ascii) return s;
        const bytes = new Uint8Array(s.length);
        for (let i = 0; i < s.length; i++) bytes[i] = s.charCodeAt(i) & 0xFF;
        return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
    }
    function utf8ToBytes(s) {
        let ascii = true;
        for (let i = 0; i < s.length; i++) {
            if (s.charCodeAt(i) >= 0x80) { ascii = false; break; }
        }
        if (ascii) return s;
        const bytes = new TextEncoder().encode(s);
        let out = "";
        for (let i = 0; i < bytes.length; i++) out += String.fromCharCode(bytes[i]);
        return out;
    }
    rt.bytesToUtf8 = bytesToUtf8;
    rt.utf8ToBytes = utf8ToBytes;

    // ---- References & CoW sharing ----
    function isRef(v) { return v !== null && typeof v === "object" && v.__php === 5; }
    function deref(v) { return (v !== null && typeof v === "object" && v.__php === 5) ? v.v : v; }
    function mkRef(v) { return { __php: 5, v: deref(v) }; }
    function share(v) {
        if (v !== null && typeof v === "object") {
            if (v.__php === 5) v = v.v;
            if (v !== null && typeof v === "object" && v.__php === 1) v.shared = true;
        }
        return v;
    }
    R.ref = mkRef;
    R.deref = deref;
    R.share = share;
    R.rset = function (ref, v) {
        v = share(v);
        ref.v = v;
        return v;
    };
    R.rsetOwned = function (ref, v) {
        ref.v = v;
        return v;
    };
    rt.isRef = isRef;
    rt.deref = deref;
    rt.mkRef = mkRef;
    rt.share = share;

    // ---- Error suppression & registries ----
    let suppressDepth = 0;
    let errorReportingLevel = 32767n; // E_ALL
    let errorHandler = null;
    const errorHandlerStack = [];
    let exceptionHandler = null;
    const exceptionHandlerStack = [];
    const shutdownFns = [];
    rt.suppressDepth = function () { return suppressDepth; };
    rt.errorReporting = function (v) {
        const prev = errorReportingLevel;
        if (v !== undefined) errorReportingLevel = toInt(v);
        return prev;
    };
    rt.setErrorHandler = function (cb) {
        const prev = errorHandler;
        errorHandlerStack.push(prev);
        errorHandler = cb;
        return prev;
    };
    rt.restoreErrorHandler = function () {
        errorHandler = errorHandlerStack.length > 0 ? errorHandlerStack.pop() : null;
        return true;
    };
    rt.setExceptionHandler = function (cb) {
        const prev = exceptionHandler;
        exceptionHandlerStack.push(prev);
        exceptionHandler = cb;
        return prev;
    };
    rt.restoreExceptionHandler = function () {
        exceptionHandler = exceptionHandlerStack.length > 0 ? exceptionHandlerStack.pop() : null;
        return true;
    };
    rt.getExceptionHandler = function () { return exceptionHandler; };
    rt.registerShutdown = function (cb, args) { shutdownFns.push({ cb: cb, args: args }); };
    rt.runShutdown = function () {
        while (shutdownFns.length > 0) {
            const item = shutdownFns.shift();
            try { rt.call(item.cb, item.args, null, null, false); } catch (e) {
                if (e && e.isPhpExit) break;
            }
        }
    };
    R.supPush = function () { suppressDepth++; };
    R.supPop = function () { if (suppressDepth > 0) suppressDepth--; };

    // ---- Output buffering ----
    const obStack = [];
    let rawOutputBuf = "";
    let headersSentFlag = false;
    rt.obStack = obStack;
    rt.headersSent = function () { return headersSentFlag; };
    rt.markHeadersSent = function () { headersSentFlag = true; };
    rt.resetOutput = function () {
        obStack.length = 0;
        rawOutputBuf = "";
        headersSentFlag = false;
    };
    rt.takeRawOutput = function () {
        const out = rawOutputBuf;
        rawOutputBuf = "";
        return out;
    };
    function writeRaw(s) {
        if (s.length === 0) return;
        headersSentFlag = true;
        if (typeof __zipp_php_output === "function") {
            __zipp_php_output(s);
        } else {
            rawOutputBuf += s;
        }
    }
    function echoStr(s) {
        if (s.length === 0) return;
        if (obStack.length > 0) {
            obStack[obStack.length - 1].chunks.push(s);
        } else {
            writeRaw(s);
        }
    }
    rt.writeRaw = writeRaw;
    rt.echoStr = echoStr;
    rt.flushAllOb = function () {
        while (obStack.length > 0) {
            const top = obStack.pop();
            let text = top.chunks.join("");
            if (top.cb !== null) {
                try { text = toStr(rt.call(top.cb, [text, 9n], null, null, false)); } catch (_) {}
            }
            if (obStack.length > 0) {
                obStack[obStack.length - 1].chunks.push(text);
            } else {
                writeRaw(text);
            }
        }
    };
    R.echo = function (v) {
        echoStr(toStr(deref(v)));
    };
    R.print = function (v) {
        echoStr(toStr(deref(v)));
        return 1n;
    };
    R.exit = function (v) {
        v = deref(v);
        if (typeof v === "string") {
            echoStr(v);
            throw new PhpExit(0);
        }
        const code = (v === null || v === undefined) ? 0 : Number(toInt(v));
        throw new PhpExit(code);
    };

    // ---- Global scopes & superglobals ----
    const globalScope = new Map();
    const superglobals = new Set([
        "GLOBALS", "_SERVER", "_GET", "_POST", "_FILES",
        "_COOKIE", "_SESSION", "_REQUEST", "_ENV"
    ]);
    rt.globalScope = globalScope;
    rt.superglobals = superglobals;

    function getGlobalRef(name) {
        let cell = globalScope.get(name);
        if (cell === undefined) {
            cell = mkRef(null);
            globalScope.set(name, cell);
        }
        return cell;
    }
    rt.getGlobalRef = getGlobalRef;
    R.gref = getGlobalRef;
    R.newScope = function () { return new Map(); };

    // `$GLOBALS` returns a live PhpArray snapshot/proxy over globalScope.
    function makeGlobalsArray() {
        const arr = rt.makeArr();
        arr.hasRefs = true;
        for (const [k, cell] of globalScope) {
            if (k === "GLOBALS") continue;
            arr._list = false; arr._e = null;
            arr.m.set(k, cell);
        }
        return arr;
    }

    R.sget = function (scope, name, quiet) {
        if (name === "GLOBALS") return makeGlobalsArray();
        const target = superglobals.has(name) ? globalScope : scope;
        const cell = target.get(name);
        if (cell === undefined) return null;
        return cell.v;
    };
    R.sref = function (scope, name) {
        const target = superglobals.has(name) ? globalScope : scope;
        let cell = target.get(name);
        if (cell === undefined) {
            cell = mkRef(null);
            target.set(name, cell);
        }
        return cell;
    };
    R.sset = function (scope, name, val) {
        val = share(val);
        const target = superglobals.has(name) ? globalScope : scope;
        let cell = target.get(name);
        if (cell === undefined) {
            cell = { __php: 5, v: val };
            target.set(name, cell);
        } else {
            cell.v = val;
        }
        return val;
    };
    R.ssetOwned = function (scope, name, val) {
        const target = superglobals.has(name) ? globalScope : scope;
        let cell = target.get(name);
        if (cell === undefined) {
            cell = { __php: 5, v: val };
            target.set(name, cell);
        } else {
            cell.v = val;
        }
        return val;
    };
    R.sbind = function (scope, name, ref) {
        const target = superglobals.has(name) ? globalScope : scope;
        target.set(name, isRef(ref) ? ref : mkRef(ref));
    };
    R.sunset = function (scope, name) {
        const target = superglobals.has(name) ? globalScope : scope;
        target.delete(name);
    };
    R.stref = function (fnObj, name, initFn) {
        if (!fnObj.statics) fnObj.statics = new Map();
        let cell = fnObj.statics.get(name);
        if (cell === undefined) {
            const val = initFn ? share(deref(initFn())) : null;
            cell = { __php: 5, v: val };
            fnObj.statics.set(name, cell);
        }
        return cell;
    };

    // ---- Constants & Functions registries ----
    const funcs = new Map();       // canonical lowercase fqn -> PhpFunc/builtin
    const consts = new Map();      // fqn (case-sensitive in PHP 8) -> value
    const includedFiles = new Set();
    rt.funcs = funcs;
    rt.consts = consts;
    rt.includedFiles = includedFiles;

    R.defConst = function (name, val) {
        consts.set(name, share(deref(val)));
    };
    R.getConst = function (fqn, fallback) {
        if (consts.has(fqn)) return consts.get(fqn);
        if (fallback !== null && consts.has(fallback)) return consts.get(fallback);
        // Case-insensitive lookup for true/false/null just in case
        const lo = fqn.toLowerCase();
        if (lo === "true") return true;
        if (lo === "false") return false;
        if (lo === "null") return null;
        rt.throwError("Error", 'Undefined constant "' + fqn + '"');
    };

    // ---- Type conversions (PHP 8.x) ----
    function truthy(v) {
        v = deref(v);
        if (v === null || v === undefined || v === false) return false;
        if (v === true) return true;
        if (typeof v === "bigint") return v !== 0n;
        if (typeof v === "number") return v !== 0.0; // Note: NaN is truthy in PHP!
        if (typeof v === "string") return v.length > 0 && v !== "0";
        if (typeof v === "object") {
            if (v.__php === 1) return v.m.size > 0;
            return true;
        }
        return true;
    }

    function parseNumericString(s) {
        // Returns { kind: 0 (not numeric), 1 (int, val: bigint), 2 (float, val: number) }
        let i = 0, n = s.length;
        while (i < n && (s.charCodeAt(i) === 32 || s.charCodeAt(i) === 9 || s.charCodeAt(i) === 10 || s.charCodeAt(i) === 13 || s.charCodeAt(i) === 11 || s.charCodeAt(i) === 12)) i++;
        let j = n;
        while (j > i && (s.charCodeAt(j - 1) === 32 || s.charCodeAt(j - 1) === 9 || s.charCodeAt(j - 1) === 10 || s.charCodeAt(j - 1) === 13 || s.charCodeAt(j - 1) === 11 || s.charCodeAt(j - 1) === 12)) j--;
        if (i === j) return null;
        const t = s.slice(i, j);
        if (/^[+-]?\d+$/.test(t)) {
            try {
                const bi = BigInt(t);
                if (bi >= I64_MIN && bi <= I64_MAX) return { isInt: true, iv: bi, fv: Number(bi) };
                return { isInt: false, iv: 0n, fv: Number(t) };
            } catch (_) {
                return { isInt: false, iv: 0n, fv: Number(t) };
            }
        }
        if (/^[+-]?(\d+\.\d*|\.\d+|\d+)([eE][+-]?\d+)?$/.test(t)) {
            return { isInt: false, iv: 0n, fv: Number(t) };
        }
        return null;
    }
    rt.parseNumericString = parseNumericString;

    function parseLeadingNumeric(s) {
        // Used for `(int)` / `(float)` casts on strings (leading whitespace + sign + digits).
        let i = 0, n = s.length;
        while (i < n && (s.charCodeAt(i) === 32 || s.charCodeAt(i) === 9 || s.charCodeAt(i) === 10 || s.charCodeAt(i) === 13 || s.charCodeAt(i) === 11 || s.charCodeAt(i) === 12)) i++;
        const rest = s.slice(i);
        const mf = /^[+-]?(\d+\.\d*|\.\d+|\d+)([eE][+-]?\d+)?/.exec(rest);
        if (!mf) return { isInt: true, iv: 0n, fv: 0.0 };
        const tok = mf[0];
        if (!tok.includes(".") && !tok.includes("e") && !tok.includes("E")) {
            try {
                const bi = BigInt(tok);
                if (bi >= I64_MIN && bi <= I64_MAX) return { isInt: true, iv: bi, fv: Number(bi) };
            } catch (_) {}
        }
        const fv = Number(tok);
        return { isInt: false, iv: floatToInt64(fv), fv: fv };
    }

    function floatToInt64(f) {
        if (!Number.isFinite(f)) return 0n;
        const t = Math.trunc(f);
        if (t === 0) return 0n;
        // PHP wraps floats >= 2^63 via two's complement mod 2^64 on x86-64/standard builds or clamps
        if (t >= -9223372036854775808 && t < 9223372036854775808) {
            return BigInt(t);
        }
        return BigInt.asIntN(64, BigInt(Math.trunc(t)));
    }
    rt.floatToInt64 = floatToInt64;

    function toInt(v) {
        v = deref(v);
        if (typeof v === "bigint") return v;
        if (typeof v === "number") return floatToInt64(v);
        if (v === true) return 1n;
        if (v === false || v === null || v === undefined) return 0n;
        if (typeof v === "string") {
            const p = parseLeadingNumeric(v);
            return p.isInt ? p.iv : floatToInt64(p.fv);
        }
        if (typeof v === "object") {
            if (v.__php === 1) return v.m.size > 0 ? 1n : 0n;
            if (v.__php === 4) return BigInt(v.id || 1);
            return 1n;
        }
        return 0n;
    }

    function toFloat(v) {
        v = deref(v);
        if (typeof v === "number") return v;
        if (typeof v === "bigint") return Number(v);
        if (v === true) return 1.0;
        if (v === false || v === null || v === undefined) return 0.0;
        if (typeof v === "string") return parseLeadingNumeric(v).fv;
        if (typeof v === "object") {
            if (v.__php === 1) return v.m.size > 0 ? 1.0 : 0.0;
            return 1.0;
        }
        return 0.0;
    }

    function formatFloat(n) {
        if (Number.isNaN(n)) return "NAN";
        if (n === Infinity) return "INF";
        if (n === -Infinity) return "-INF";
        if (Object.is(n, -0)) return "-0";
        if (n === 0) return "0";
        // Match PHP's default precision=14 formatting closely
        const s = Number(n.toPrecision(14)).toString();
        return s.replace(/e([+-])(\d)$/, "E$10$2").replace("e", "E");
    }
    rt.formatFloat = formatFloat;

    function toStr(v) {
        v = deref(v);
        if (typeof v === "string") return v;
        if (v === null || v === undefined || v === false) return "";
        if (v === true) return "1";
        if (typeof v === "bigint") return v.toString();
        if (typeof v === "number") return formatFloat(v);
        if (typeof v === "object") {
            if (v.__php === 1) return "Array";
            if (v.__php === 2) {
                const m = rt.findMethod(v.cls, "__tostring");
                if (m) {
                    const res = rt.invokeMethod(v, m, [], null, v.cls, v.cls, false);
                    if (typeof res !== "string") {
                        rt.throwError("Error", v.cls.name + "::__toString(): Return value must be of type string");
                    }
                    return res;
                }
                rt.throwError("Error", "Object of class " + v.cls.name + " could not be converted to string");
            }
            if (v.__php === 3) {
                rt.throwError("Error", "Object of class Closure could not be converted to string");
            }
            if (v.__php === 4) return "Resource id #" + (v.id || 1);
        }
        return String(v);
    }

    function toNumOperand(v) {
        v = deref(v);
        if (typeof v === "bigint") return v;
        if (typeof v === "number") return v;
        if (v === true) return 1n;
        if (v === false || v === null || v === undefined) return 0n;
        if (typeof v === "string") {
            const p = parseNumericString(v);
            if (p !== null) return p.isInt ? p.iv : p.fv;
            const lead = parseLeadingNumeric(v);
            if (/^\s*[+-]?(\d|\.\d)/.test(v)) return lead.isInt ? lead.iv : lead.fv;
            rt.throwError("TypeError", "Unsupported operand types: string");
        }
        rt.throwError("TypeError", "Unsupported operand types");
    }

    function clampInt64OrFloat(bi) {
        if (bi >= I64_MIN && bi <= I64_MAX) return bi;
        return Number(bi);
    }
    rt.clampInt64OrFloat = clampInt64OrFloat;

    // ---- Arithmetic ----
    R.add = function (a, b) {
        a = deref(a); b = deref(b);
        // Array union: `$a + $b` keeps keys from `$a`, appends missing keys from `$b`.
        if (a && a.__php === 1 && b && b.__php === 1) {
            const out = rt.cloneArr(a);
            for (const [k, v] of b.m) {
                if (!out.m.has(k)) {
                    out._list = false; out._e = null;
                    out.m.set(k, share(deref(v)));
                    if (typeof k === "bigint" && k >= out.next) out.next = k + 1n;
                }
            }
            return out;
        }
        const na = toNumOperand(a), nb = toNumOperand(b);
        if (typeof na === "bigint" && typeof nb === "bigint") return clampInt64OrFloat(na + nb);
        return Number(na) + Number(nb);
    };
    R.sub = function (a, b) {
        const na = toNumOperand(a), nb = toNumOperand(b);
        if (typeof na === "bigint" && typeof nb === "bigint") return clampInt64OrFloat(na - nb);
        return Number(na) - Number(nb);
    };
    R.mul = function (a, b) {
        const na = toNumOperand(a), nb = toNumOperand(b);
        if (typeof na === "bigint" && typeof nb === "bigint") return clampInt64OrFloat(na * nb);
        return Number(na) * Number(nb);
    };
    R.div = function (a, b) {
        const na = toNumOperand(a), nb = toNumOperand(b);
        if ((typeof nb === "bigint" && nb === 0n) || (typeof nb === "number" && nb === 0)) {
            rt.throwError("DivisionByZeroError", "Division by zero");
        }
        if (typeof na === "bigint" && typeof nb === "bigint") {
            if (na === I64_MIN && nb === -1n) return Number(na) / -1.0;
            if (na % nb === 0n) return na / nb;
            return Number(na) / Number(nb);
        }
        return Number(na) / Number(nb);
    };
    R.mod = function (a, b) {
        const ia = toInt(a), ib = toInt(b);
        if (ib === 0n) rt.throwError("DivisionByZeroError", "Modulo by zero");
        if (ia === I64_MIN && ib === -1n) return 0n;
        return ia % ib;
    };
    R.pow = function (a, b) {
        const na = toNumOperand(a), nb = toNumOperand(b);
        if (typeof na === "bigint" && typeof nb === "bigint" && nb >= 0n) {
            if (nb === 0n) return 1n;
            if (na === 0n) return 0n;
            if (na === 1n) return 1n;
            if (na === -1n) return (nb & 1n) === 0n ? 1n : -1n;
            if (nb <= 63n) {
                const r = na ** nb;
                if (r >= I64_MIN && r <= I64_MAX) return r;
            }
            return Math.pow(Number(na), Number(nb));
        }
        return Math.pow(Number(na), Number(nb));
    };
    R.concat = function (a, b) { return toStr(a) + toStr(b); };
    R.uplus = function (a) { return toNumOperand(a); };
    R.uneg = function (a) {
        const na = toNumOperand(a);
        if (typeof na === "bigint") return clampInt64OrFloat(-na);
        return -na;
    };

    // ---- Bitwise (64-bit int, or byte-wise on two strings) ----
    function strBitOp(a, b, op) {
        const n = Math.min(a.length, b.length);
        let out = "";
        for (let i = 0; i < n; i++) {
            const x = a.charCodeAt(i), y = b.charCodeAt(i);
            out += String.fromCharCode(op === 0 ? (x & y) : op === 1 ? (x | y) : (x ^ y));
        }
        return out;
    }
    R.band = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "string" && typeof b === "string") return strBitOp(a, b, 0);
        return BigInt.asIntN(64, toInt(a) & toInt(b));
    };
    R.bor = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "string" && typeof b === "string") return strBitOp(a, b, 1);
        return BigInt.asIntN(64, toInt(a) | toInt(b));
    };
    R.bxor = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "string" && typeof b === "string") return strBitOp(a, b, 2);
        return BigInt.asIntN(64, toInt(a) ^ toInt(b));
    };
    R.bnot = function (a) {
        a = deref(a);
        if (typeof a === "string") {
            let out = "";
            for (let i = 0; i < a.length; i++) out += String.fromCharCode((~a.charCodeAt(i)) & 0xFF);
            return out;
        }
        return BigInt.asIntN(64, ~toInt(a));
    };
    R.shl = function (a, b) {
        const ia = toInt(a), ib = toInt(b);
        if (ib < 0n) rt.throwError("ArithmeticError", "Bit shift by negative number");
        if (ib >= 64n) return 0n;
        return BigInt.asIntN(64, (ia & U64_MASK) << ib);
    };
    R.shr = function (a, b) {
        const ia = toInt(a), ib = toInt(b);
        if (ib < 0n) rt.throwError("ArithmeticError", "Bit shift by negative number");
        if (ib >= 64n) return ia < 0n ? -1n : 0n;
        return BigInt.asIntN(64, ia >> ib);
    };

    // ---- Increment / Decrement (PHP semantics!) ----
    R.inc = function (v) {
        v = deref(v);
        if (typeof v === "bigint") return clampInt64OrFloat(v + 1n);
        if (typeof v === "number") return v + 1.0;
        if (v === null || v === undefined) return 1n;
        if (typeof v === "boolean") return v;
        if (typeof v === "string") {
            if (v === "") return "1";
            const p = parseNumericString(v);
            if (p !== null) return p.isInt ? clampInt64OrFloat(p.iv + 1n) : p.fv + 1.0;
            // PHP alphanumeric string increment ('a' -> 'b', 'z' -> 'aa', 'a9' -> 'b0')
            const chars = v.split("");
            let carry = true;
            for (let i = chars.length - 1; i >= 0 && carry; i--) {
                const c = chars[i].charCodeAt(0);
                if (c >= 48 && c <= 56) { chars[i] = String.fromCharCode(c + 1); carry = false; }
                else if (c === 57) { chars[i] = "0"; }
                else if (c >= 97 && c <= 121) { chars[i] = String.fromCharCode(c + 1); carry = false; }
                else if (c === 122) { chars[i] = "a"; }
                else if (c >= 65 && c <= 89) { chars[i] = String.fromCharCode(c + 1); carry = false; }
                else if (c === 90) { chars[i] = "A"; }
                else { carry = false; }
            }
            if (carry) {
                const first = chars[0].charCodeAt(0);
                chars.unshift(first === 48 ? "1" : first === 97 ? "a" : "A");
            }
            return chars.join("");
        }
        return v;
    };
    R.dec = function (v) {
        v = deref(v);
        if (typeof v === "bigint") return clampInt64OrFloat(v - 1n);
        if (typeof v === "number") return v - 1.0;
        if (v === null || v === undefined || typeof v === "boolean") return v;
        if (typeof v === "string") {
            const p = parseNumericString(v);
            if (p !== null) return p.isInt ? clampInt64OrFloat(p.iv - 1n) : p.fv - 1.0;
        }
        return v;
    };

    // ---- PHP 8.x Comparisons (==, ===, <=>, <, <=, >, >=) ----
    function strictEq(a, b) {
        a = deref(a); b = deref(b);
        if (a === b) return true;
        if (a && a.__php === 1 && b && b.__php === 1) {
            if (a.m.size !== b.m.size) return false;
            const ia = a.m.entries(), ib = b.m.entries();
            for (let i = 0; i < a.m.size; i++) {
                const ea = ia.next().value, eb = ib.next().value;
                if (ea[0] !== eb[0]) return false;
                if (!strictEq(ea[1], eb[1])) return false;
            }
            return true;
        }
        return false;
    }

    function cmpNum(a, b) {
        // Exact comparison between bigint and bigint/number
        if (typeof a === "bigint" && typeof b === "bigint") {
            return a < b ? -1n : a > b ? 1n : 0n;
        }
        if (typeof a === "number" && typeof b === "number") {
            if (Number.isNaN(a) || Number.isNaN(b)) return NaN;
            return a < b ? -1n : a > b ? 1n : 0n;
        }
        if (typeof a === "bigint") {
            if (Number.isNaN(b)) return NaN;
            if (!Number.isFinite(b)) return b > 0 ? -1n : 1n;
            const t = Math.trunc(b);
            if (t === b && t >= -9223372036854775808 && t < 9223372036854775808) {
                const bb = BigInt(t);
                return a < bb ? -1n : a > bb ? 1n : 0n;
            }
            const fa = Number(a);
            return fa < b ? -1n : fa > b ? 1n : 0n;
        }
        const r = cmpNum(b, a);
        return Number.isNaN(r) ? NaN : -r;
    }

    function phpCmp(a, b) {
        a = deref(a); b = deref(b);
        // Same scalar type fast paths
        if (typeof a === "bigint" && typeof b === "bigint") return a < b ? -1n : a > b ? 1n : 0n;
        if (typeof a === "number" && typeof b === "number") return cmpNum(a, b);
        if (typeof a === "string" && typeof b === "string") {
            const pa = parseNumericString(a), pb = parseNumericString(b);
            if (pa !== null && pb !== null) {
                return cmpNum(pa.isInt ? pa.iv : pa.fv, pb.isInt ? pb.iv : pb.fv);
            }
            return a < b ? -1n : a > b ? 1n : 0n;
        }
        // Null or bool vs anything (except two arrays/objects handled below)
        if (typeof a === "boolean" || typeof b === "boolean") {
            const ba = truthy(a) ? 1n : 0n, bb = truthy(b) ? 1n : 0n;
            return ba < bb ? -1n : ba > bb ? 1n : 0n;
        }
        if (a === null || b === null) {
            if (a === null && b === null) return 0n;
            if (a === null) {
                if (typeof b === "string") return "" < b ? -1n : "" > b ? 1n : 0n;
                if (b && b.__php === 1) return b.m.size === 0 ? 0n : -1n;
                if (b && b.__php === 2) return -1n;
                return cmpNum(0n, toNumOperand(b));
            }
            const r = phpCmp(b, a);
            return Number.isNaN(r) ? NaN : -r;
        }
        // Number vs Number or Numeric String (PHP 8.0+ rule!)
        const aIsNum = typeof a === "bigint" || typeof a === "number";
        const bIsNum = typeof b === "bigint" || typeof b === "number";
        if (aIsNum && bIsNum) return cmpNum(a, b);
        if (aIsNum && typeof b === "string") {
            const pb = parseNumericString(b);
            if (pb !== null) return cmpNum(a, pb.isInt ? pb.iv : pb.fv);
            // PHP 8.0+: number vs non-numeric string compares as string!
            const sa = toStr(a);
            return sa < b ? -1n : sa > b ? 1n : 0n;
        }
        if (typeof a === "string" && bIsNum) {
            const pa = parseNumericString(a);
            if (pa !== null) return cmpNum(pa.isInt ? pa.iv : pa.fv, b);
            const sb = toStr(b);
            return a < sb ? -1n : a > sb ? 1n : 0n;
        }
        // Arrays
        if (a && a.__php === 1 && b && b.__php === 1) {
            if (a.m.size !== b.m.size) return a.m.size < b.m.size ? -1n : 1n;
            for (const [k, va] of a.m) {
                if (!b.m.has(k)) return NaN; // incomparable
                const c = phpCmp(va, b.m.get(k));
                if (c !== 0n) return c;
            }
            return 0n;
        }
        // Objects of same class: compare properties
        if (a && a.__php === 2 && b && b.__php === 2) {
            if (a === b) return 0n;
            if (a.cls !== b.cls) return 1n;
            const ka = Object.keys(a.props), kb = Object.keys(b.props);
            if (ka.length !== kb.length) return ka.length < kb.length ? -1n : 1n;
            for (let i = 0; i < ka.length; i++) {
                const k = ka[i];
                if (b.props[k] === undefined) return 1n;
                const c = phpCmp(a.props[k], b.props[k]);
                if (c !== 0n) return c;
            }
            return 0n;
        }
        if (a && a.__php === 1) return 1n;
        if (b && b.__php === 1) return -1n;
        if (a && a.__php === 2) return 1n;
        if (b && b.__php === 2) return -1n;
        return 0n;
    }

    function looseEq(a, b) {
        a = deref(a); b = deref(b);
        if (a === b) {
            return !(typeof a === "number" && Number.isNaN(a));
        }
        if (a && a.__php === 2 && b && b.__php === 2) {
            if (a.cls !== b.cls) return false;
            const ka = Object.keys(a.props), kb = Object.keys(b.props);
            if (ka.length !== kb.length) return false;
            for (let i = 0; i < ka.length; i++) {
                const k = ka[i];
                if (b.props[k] === undefined || !looseEq(a.props[k], b.props[k])) return false;
            }
            return true;
        }
        const c = phpCmp(a, b);
        return c === 0n;
    }

    R.truthy = truthy;
    R.not = function (v) { return !truthy(v); };
    R.empty = function (v) { return !truthy(v); };
    R.eq = looseEq;
    R.neq = function (a, b) { return !looseEq(a, b); };
    R.seq = strictEq;
    R.sneq = function (a, b) { return !strictEq(a, b); };
    R.cmp = function (a, b) {
        const c = phpCmp(a, b);
        return Number.isNaN(c) ? 1n : c;
    };
    R.lt = function (a, b) { const c = phpCmp(a, b); return c === -1n; };
    R.lte = function (a, b) { const c = phpCmp(a, b); return c === -1n || c === 0n; };
    R.gt = function (a, b) { const c = phpCmp(a, b); return c === 1n; };
    R.gte = function (a, b) { const c = phpCmp(a, b); return c === 1n || c === 0n; };

    R.cast = function (kind, v) {
        v = deref(v);
        switch (kind) {
            case 0: return toInt(v);
            case 1: return toFloat(v);
            case 2: return toStr(v);
            case 3: return truthy(v);
            case 4: return rt.toArray(v);
            case 5: return rt.toObject(v);
            case 6: return null;
            default: return v;
        }
    };

    rt.truthy = truthy;
    rt.toInt = toInt;
    rt.toFloat = toFloat;
    rt.toStr = toStr;
    rt.strictEq = strictEq;
    rt.looseEq = looseEq;
    rt.phpCmp = phpCmp;

    return R;
})();
