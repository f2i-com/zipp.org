/* Experimental integer-centric Python runtime. Apache-2.0.
 * This trusted, fixed bootstrap is compiled once per program by ZIPP itself.
 * Guest Python is NEVER translated to JavaScript source or sent to host eval.
 * No Python imports of the host, JavaScript escape hatch, filesystem or
 * networking exists. Project-local modules are compiled ahead of time by the
 * Rust frontend and registered through `module`; `ui` is the one built-in
 * module, and its only effect is to append commands to a buffer the host
 * drains through `__zipp_py_take_ui`.
 */
// The host-facing hooks below are ordinary top-level globals so an embedder
// can reach them by slot: the UI command buffer, the input snapshot, and the
// call/has/take/set functions. Guest Python can never name them (its names
// resolve through the private tables inside the runtime closure).
var __zipp_py_ui = [];
var __zipp_py_input = { mx: 0, my: 0, down: false, clicked: false, keys: {}, w: 640, h: 480 };
var __zipp_py = (function () {
    "use strict";
    const globals = new Map();       // "module.name" -> value
    const builtins = new Map();      // bare builtin name -> function wrapper
    const inits = new Map();         // module name -> compiled init code
    const modules = new Map();       // module name -> module object (loaded or loading)
    const builtinModules = new Map();
    let entryName = "main";
    const UNBOUND = Symbol("python-unbound");
    const LIST = Symbol("python-list"), TUPLE = Symbol("python-tuple");
    const RANGE = Symbol("python-range"), FN = Symbol("python-function");
    const MODULE = Symbol("python-module");
    const MAX_ITEMS = 65536, MAX_TEXT = 1048576, MAX_BITS = 262144;
    const MAX_UI_COMMANDS = 100000;
    function fail(name, message) { const e = new Error(message); e.name = name; throw e; }
    function tag(v, kind) { return v !== null && typeof v === "object" && v.kind === kind; }
    function seq(v) { return tag(v, LIST) || tag(v, TUPLE); }
    function integer(v) {
        if (typeof v === "bigint") return v;
        if (typeof v === "boolean") return v ? 1n : 0n;
        fail("TypeError", "an integer is required in this Python subset");
    }
    function checked(v) {
        if ((v < 0n ? -v : v).toString(2).length > MAX_BITS)
            fail("OverflowError", "experimental Python integer-size limit exceeded");
        return v;
    }
    function text(v) {
        if (v.length > MAX_TEXT) fail("OverflowError", "experimental Python string-size limit exceeded");
        return v;
    }
    function sequence(kind, items) {
        if (items.length > MAX_ITEMS) fail("OverflowError", "experimental Python sequence limit exceeded");
        return { kind: kind, items: items };
    }
    function truth(v) {
        if (v === null || v === false) return false;
        if (typeof v === "bigint") return v !== 0n;
        if (typeof v === "string") return v.length !== 0;
        if (seq(v)) return v.items.length !== 0;
        if (tag(v, RANGE)) return rangeLength(v) !== 0n;
        if (v === true || tag(v, FN) || tag(v, MODULE)) return true;
        fail("TypeError", "unsupported Python value");
    }
    function quote(s) {
        // ASCII/control quoting plus literal Unicode; not a full CPython repr formatter.
        return "'" + s.replace(/\\/g, "\\\\").replace(/'/g, "\\'")
            .replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t") + "'";
    }
    function repr(v, seen, depth) {
        if (depth > 64) fail("RecursionError", "representation nesting limit exceeded");
        if (v === null) return "None";
        if (typeof v === "boolean") return v ? "True" : "False";
        if (typeof v === "bigint") return text(v.toString());
        if (typeof v === "string") return text(quote(v));
        if (tag(v, RANGE)) return "range(" + v.start + ", " + v.stop +
            (v.step === 1n ? "" : ", " + v.step) + ")";
        if (tag(v, FN)) return "<function " + v.name + ">";
        if (tag(v, MODULE)) return "<module '" + v.name + "'>";
        if (seq(v)) {
            const tuple = tag(v, TUPLE);
            if (seen.has(v)) return tuple ? "(...)" : "[...]";
            seen.add(v);
            let parts = [], length = 2;
            for (const item of v.items) {
                const part = repr(item, seen, depth + 1);
                length += part.length + 2;
                if (length > MAX_TEXT) fail("OverflowError", "representation limit exceeded");
                parts.push(part);
            }
            seen.delete(v);
            return (tuple ? "(" : "[") + parts.join(", ") +
                (tuple && parts.length === 1 ? "," : "") + (tuple ? ")" : "]");
        }
        fail("TypeError", "unsupported Python value");
    }
    function str(v) { return typeof v === "string" ? v : repr(v, new Set(), 0); }
    function eq(a, b, depth) {
        if (depth > 64) fail("RecursionError", "comparison nesting limit exceeded");
        if (a === b) return true;
        if ((typeof a === "bigint" || typeof a === "boolean") &&
            (typeof b === "bigint" || typeof b === "boolean")) return integer(a) === integer(b);
        if (seq(a) && seq(b) && a.kind === b.kind) {
            if (a.items.length !== b.items.length) return false;
            for (let i = 0; i < a.items.length; i++) if (!eq(a.items[i], b.items[i], depth + 1)) return false;
            return true;
        }
        if (tag(a, RANGE) && tag(b, RANGE)) {
            const n = rangeLength(a);
            return n === rangeLength(b) && (n === 0n ||
                (a.start === b.start && (n === 1n || a.step === b.step)));
        }
        return false;
    }
    function order(a, b, depth) {
        if (depth > 64) fail("RecursionError", "comparison nesting limit exceeded");
        if ((typeof a === "bigint" || typeof a === "boolean") &&
            (typeof b === "bigint" || typeof b === "boolean")) {
            a = integer(a); b = integer(b); return a < b ? -1 : a > b ? 1 : 0;
        }
        if (typeof a === "string" && typeof b === "string") {
            const x = Array.from(a), y = Array.from(b);
            for (let i = 0; i < Math.min(x.length, y.length); i++) {
                const u = x[i].codePointAt(0), v = y[i].codePointAt(0);
                if (u !== v) return u < v ? -1 : 1;
            }
            return x.length < y.length ? -1 : x.length > y.length ? 1 : 0;
        }
        if (seq(a) && seq(b) && a.kind === b.kind) {
            for (let i = 0; i < Math.min(a.items.length, b.items.length); i++)
                if (!eq(a.items[i], b.items[i], depth + 1)) return order(a.items[i], b.items[i], depth + 1);
            return a.items.length < b.items.length ? -1 : a.items.length > b.items.length ? 1 : 0;
        }
        fail("TypeError", "these Python values cannot be ordered");
    }
    function contains(container, needle) {
        if (typeof container === "string") {
            if (typeof needle !== "string") fail("TypeError", "string membership requires a string");
            return container.includes(needle);
        }
        if (seq(container)) { for (const x of container.items) if (eq(x, needle, 0)) return true; return false; }
        if (tag(container, RANGE)) {
            if (typeof needle !== "bigint" && typeof needle !== "boolean") return false;
            const x = integer(needle), r = container;
            return (r.step > 0n ? x >= r.start && x < r.stop : x <= r.start && x > r.stop) &&
                (x - r.start) % r.step === 0n;
        }
        fail("TypeError", "value does not support membership");
    }
    function compare(op, a, b) {
        switch (op) {
            case "eq": return eq(a, b, 0);
            case "ne": return !eq(a, b, 0);
            case "lt": return order(a, b, 0) < 0;
            case "le": return order(a, b, 0) <= 0;
            case "gt": return order(a, b, 0) > 0;
            case "ge": return order(a, b, 0) >= 0;
            case "in": return contains(b, a);
            case "notin": return !contains(b, a);
            case "is": case "isnot": {
                if ((a !== null && typeof a !== "boolean" && typeof a !== "object") ||
                    (b !== null && typeof b !== "boolean" && typeof b !== "object"))
                    fail("NotImplementedError", "scalar identity is outside this Python subset");
                return op === "is" ? a === b : a !== b;
            }
        }
        fail("NotImplementedError", "unsupported comparison");
    }
    function repeat(value, n) {
        n = integer(n); if (n < 0n) n = 0n;
        if (typeof value === "string") {
            if (value.length === 0) return "";
            if (n * BigInt(value.length) > BigInt(MAX_TEXT)) fail("OverflowError", "string limit exceeded");
            return value.repeat(Number(n));
        }
        if (seq(value)) {
            if (value.items.length === 0) return sequence(value.kind, []);
            if (n * BigInt(value.items.length) > BigInt(MAX_ITEMS)) fail("OverflowError", "sequence limit exceeded");
            const result = [];
            for (let i = 0; i < Number(n); i++) for (const x of value.items) result.push(x);
            return sequence(value.kind, result);
        }
        fail("TypeError", "cannot repeat this Python value");
    }
    function binary(op, a, b) {
        if (op === "add" || op === "iadd") {
            if (typeof a === "string" && typeof b === "string") {
                if (a.length + b.length > MAX_TEXT) fail("OverflowError", "string limit exceeded");
                return a + b;
            }
            if (seq(a) && seq(b) && a.kind === b.kind) {
                if (a.items.length + b.items.length > MAX_ITEMS) fail("OverflowError", "sequence limit exceeded");
                if (op === "iadd" && tag(a, LIST)) {
                    const n = b.items.length; for (let i = 0; i < n; i++) a.items.push(b.items[i]); return a;
                }
                return sequence(a.kind, a.items.concat(b.items));
            }
            return checked(integer(a) + integer(b));
        }
        if (op === "mul") {
            if (typeof a === "string" || seq(a)) return repeat(a, b);
            if (typeof b === "string" || seq(b)) return repeat(b, a);
        }
        a = integer(a); b = integer(b);
        switch (op) {
            case "sub": return checked(a - b);
            case "mul": return checked(a * b);
            case "floordiv": case "mod": {
                if (b === 0n) fail("ZeroDivisionError", "integer division or modulo by zero");
                let q = a / b, r = a % b;
                if (r !== 0n && (r < 0n) !== (b < 0n)) { q -= 1n; r += b; }
                return op === "floordiv" ? q : r;
            }
        }
        fail("NotImplementedError", "operator is outside the integer Python subset");
    }
    function unary(op, value) {
        if (op === "not") return !truth(value);
        if (op === "neg") return -integer(value);
        if (op === "pos") return integer(value);
        fail("NotImplementedError", "unsupported unary operator");
    }
    function rangeLength(r) {
        if (r.step > 0n) return r.start >= r.stop ? 0n : (r.stop - r.start - 1n) / r.step + 1n;
        return r.start <= r.stop ? 0n : (r.start - r.stop - 1n) / (-r.step) + 1n;
    }
    function index(value, key) {
        let i = integer(key);
        if (tag(value, RANGE)) {
            const n = rangeLength(value); if (i < 0n) i += n;
            if (i < 0n || i >= n) fail("IndexError", "range index out of range");
            return checked(value.start + i * value.step);
        }
        const items = typeof value === "string" ? Array.from(value) : seq(value) ? value.items : null;
        if (items === null) fail("TypeError", "value is not subscriptable");
        const n = BigInt(items.length); if (i < 0n) i += n;
        if (i < 0n || i >= n) fail("IndexError", "index out of range");
        return items[Number(i)];
    }
    function setindex(value, key, item) {
        if (!tag(value, LIST)) fail("TypeError", "only lists support item assignment in this subset");
        let i = integer(key), n = BigInt(value.items.length); if (i < 0n) i += n;
        if (i < 0n || i >= n) fail("IndexError", "list assignment index out of range");
        value.items[Number(i)] = item; return null;
    }
    function iter(value) {
        if (tag(value, RANGE)) return { range: value, position: value.start };
        if (typeof value === "string") return { items: Array.from(value), index: 0 };
        if (seq(value)) return { items: value.items, index: 0 };
        fail("TypeError", "value is not iterable");
    }
    function next(it) {
        if (it.range) {
            const r = it.range, v = it.position;
            if (r.step > 0n ? v >= r.stop : v <= r.stop) return { done: true, value: null };
            it.position = checked(v + r.step); return { done: false, value: v };
        }
        return it.index >= it.items.length ? { done: true, value: null } :
            { done: false, value: it.items[it.index++] };
    }
    function unpack(value, count) {
        // Consume exactly count+1 items. Reject before ANY target store occurs.
        count = Number(integer(count)); const iterator = iter(value), items = [];
        for (let i = 0; i <= count; i++) {
            const step = next(iterator);
            if (step.done) {
                if (i !== count) fail("ValueError", "not enough values to unpack");
                return sequence(TUPLE, items);
            }
            if (i === count) fail("ValueError", "too many values to unpack");
            items.push(step.value);
        }
    }
    function fn(code, name, arity) { return { kind: FN, code: code, name: name, arity: Number(arity) }; }
    function callable(f, argc) {
        // Validate the wrapper and arity; the emitter then issues a direct VM
        // Call on the returned code (no native re-entry per Python call).
        if (!tag(f, FN)) fail("TypeError", "object is not callable");
        argc = Number(argc);
        if (f.arity >= 0 && argc !== f.arity)
            fail("TypeError", f.name + " expects " + f.arity + " positional arguments, got " + argc);
        return f.code;
    }
    function invoke(f, args) { return callable(f, args.length).apply(undefined, args); }
    function install(name, arity, code) { builtins.set(name, fn(code, name, arity)); }
    install("print", -1, function () {
        const parts = []; let total = 0;
        for (let i = 0; i < arguments.length; i++) {
            const part = str(arguments[i]); total += part.length + 1;
            if (total > MAX_TEXT) fail("OverflowError", "print limit exceeded");
            parts.push(part);
        }
        console.log(parts.join(" ")); return null;
    });
    install("len", 1, function (v) {
        if (typeof v === "string") return BigInt(Array.from(v).length);
        if (seq(v)) return BigInt(v.items.length);
        if (tag(v, RANGE)) return rangeLength(v);
        fail("TypeError", "object has no len()");
    });
    install("bool", 1, truth); install("str", 1, str);
    install("abs", 1, function (v) { v = integer(v); return v < 0n ? -v : v; });
    install("min", 2, function (a, b) { return order(a, b, 0) <= 0 ? a : b; });
    install("max", 2, function (a, b) { return order(a, b, 0) >= 0 ? a : b; });
    install("int", 1, function (v) {
        if (typeof v === "bigint" || typeof v === "boolean") return integer(v);
        if (typeof v === "string") {
            const s = v.trim();
            if (!/^[+-]?[0-9]+$/.test(s)) fail("ValueError", "invalid literal for int(): " + quote(v));
            return checked(BigInt(s));
        }
        fail("TypeError", "int() argument must be a string, an integer or a bool");
    });
    install("range", -1, function () {
        if (arguments.length < 1 || arguments.length > 3) fail("TypeError", "range expects 1 to 3 arguments");
        const start = arguments.length === 1 ? 0n : integer(arguments[0]);
        const stop = integer(arguments[arguments.length === 1 ? 0 : 1]);
        const step = arguments.length === 3 ? integer(arguments[2]) : 1n;
        if (step === 0n) fail("ValueError", "range step cannot be zero");
        return { kind: RANGE, start: start, stop: stop, step: step };
    });
    function collect(value, kind) {
        const result = [], iterator = iter(value);
        while (true) {
            const n = next(iterator); if (n.done) break;
            if (result.length === MAX_ITEMS) fail("OverflowError", "sequence limit exceeded");
            result.push(n.value);
        }
        return sequence(kind, result);
    }
    install("list", 1, function (v) { return collect(v, LIST); });
    install("tuple", 1, function (v) { return collect(v, TUPLE); });
    function attr(value, name) {
        if (tag(value, MODULE)) {
            if (value.table) {
                if (value.table.has(name)) return value.table.get(name);
            } else {
                const key = value.name + "." + name;
                if (globals.has(key)) return globals.get(key);
            }
            fail("AttributeError", "module '" + value.name + "' has no attribute '" + name + "'");
        }
        if (tag(value, LIST)) {
            if (name === "append") return fn(function (x) {
                if (value.items.length === MAX_ITEMS) fail("OverflowError", "sequence limit exceeded");
                value.items.push(x); return null;
            }, "append", 1);
            if (name === "pop") return fn(function () {
                if (value.items.length === 0) fail("IndexError", "pop from empty list");
                return value.items.pop();
            }, "pop", 0);
        }
        fail("AttributeError", "attribute is outside this Python subset: " + name);
    }

    // ---- the `ui` built-in module: every call appends one command to the
    // host-drained buffer; nothing here draws or reads the host directly.
    function num(v) { return Number(integer(v)); }
    function color(v) {
        if (typeof v !== "string" || v.length > 64) fail("TypeError", "a color is a short string such as '#ff8800' or 'red'");
        return v;
    }
    function emit(command) {
        if (__zipp_py_ui.length >= MAX_UI_COMMANDS) fail("OverflowError", "ui command limit exceeded for one frame");
        __zipp_py_ui.push(command); return null;
    }
    function builtinModule(name, entries) {
        const table = new Map();
        for (const entry of entries) table.set(entry[0], fn(entry[2], entry[0], entry[1]));
        builtinModules.set(name, { kind: MODULE, name: name, table: table });
    }
    builtinModule("ui", [
        ["canvas", 2, function (w, h) { return emit(["canvas", num(w), num(h)]); }],
        ["clear", 1, function (c) { return emit(["clear", color(c)]); }],
        ["rect", 5, function (x, y, w, h, c) { return emit(["rect", num(x), num(y), num(w), num(h), color(c)]); }],
        ["circle", 4, function (x, y, r, c) { return emit(["circle", num(x), num(y), num(r), color(c)]); }],
        ["line", 5, function (x1, y1, x2, y2, c) { return emit(["line", num(x1), num(y1), num(x2), num(y2), color(c)]); }],
        ["text", 4, function (x, y, s, c) { return emit(["text", num(x), num(y), text(str(s)), color(c)]); }],
        ["font", 1, function (size) { return emit(["font", num(size)]); }],
        ["button", 5, function (x, y, w, h, label) {
            x = num(x); y = num(y); w = num(w); h = num(h);
            emit(["button", x, y, w, h, text(str(label))]);
            const i = __zipp_py_input;
            return !!i.clicked && i.mx >= x && i.mx < x + w && i.my >= y && i.my < y + h;
        }],
        ["mouse", 0, function () {
            const i = __zipp_py_input;
            return sequence(TUPLE, [BigInt(Math.trunc(i.mx)), BigInt(Math.trunc(i.my)), !!i.down]);
        }],
        ["clicked", 0, function () { return !!__zipp_py_input.clicked; }],
        ["key", 1, function (name) {
            if (typeof name !== "string") fail("TypeError", "key name must be a string");
            return __zipp_py_input.keys[name] === true;
        }],
        ["width", 0, function () { return BigInt(Math.trunc(__zipp_py_input.w)); }],
        ["height", 0, function () { return BigInt(Math.trunc(__zipp_py_input.h)); }],
    ]);

    // ---- host-value conversion for the call hook (numbers are the host's
    // integers; anything fractional is outside the subset and is refused).
    function fromHost(v, depth) {
        if (depth > 32) fail("TypeError", "host value nesting limit exceeded");
        if (v === null || v === undefined) return null;
        if (typeof v === "boolean" || typeof v === "string") return v;
        if (typeof v === "number") {
            if (!Number.isInteger(v)) fail("TypeError", "only integer numbers can be passed to Python in this subset");
            return BigInt(v);
        }
        if (typeof v === "bigint") return v;
        if (Array.isArray(v)) {
            const items = [];
            for (let i = 0; i < v.length; i++) items.push(fromHost(v[i], depth + 1));
            return sequence(LIST, items);
        }
        fail("TypeError", "unsupported host value");
    }
    function toHost(v, depth) {
        if (depth > 32) fail("TypeError", "value nesting limit exceeded");
        if (v === null || typeof v === "boolean" || typeof v === "string") return v;
        if (typeof v === "bigint") {
            if (v >= BigInt(Number.MIN_SAFE_INTEGER) && v <= BigInt(Number.MAX_SAFE_INTEGER)) return Number(v);
            return v.toString();
        }
        if (seq(v)) { const out = []; for (const x of v.items) out.push(toHost(x, depth + 1)); return out; }
        if (tag(v, RANGE)) return "range(" + v.start + ", " + v.stop + ", " + v.step + ")";
        if (tag(v, FN)) return "<function " + v.name + ">";
        if (tag(v, MODULE)) return "<module '" + v.name + "'>";
        return null;
    }
    function lookupEntry(name) {
        if (typeof name !== "string") fail("TypeError", "function name must be a string");
        return globals.get(entryName + "." + name);
    }
    return Object.freeze({
        unbound: UNBOUND, truth: truth, binary: binary, unary: unary, compare: compare,
        list: function (a) { return sequence(LIST, a); },
        tuple: function (a) { return sequence(TUPLE, a); },
        index: index, setindex: setindex, iter: iter, next: next, unpack: unpack,
        fn: fn, callable: callable, invoke: invoke, attr: attr,
        // Names: every module-level binding is keyed "module.name"; a lookup
        // falls back to the bare builtin name and never to any host global.
        load: function (key, bare) {
            if (globals.has(key)) return globals.get(key);
            if (builtins.has(bare)) return builtins.get(bare);
            fail("NameError", "name '" + bare + "' is not defined");
        },
        store: function (key, value) { globals.set(key, value); return null; },
        local: function (value, name) {
            if (value === UNBOUND) fail("UnboundLocalError", "local variable '" + name + "' is unbound");
            return value;
        },
        assertion: function (message) { fail("AssertionError", str(message)); },
        // Modules: the frontend registers each project module's init code up
        // front, names the entry, then imports it. A module object is inserted
        // before its body runs, so an import cycle resolves like CPython's
        // (a partially initialised module) instead of recursing.
        module: function (name, code) { inits.set(name, code); return null; },
        entry: function (name) { entryName = name; return null; },
        import: function (name) {
            if (modules.has(name)) return modules.get(name);
            if (builtinModules.has(name)) return builtinModules.get(name);
            const init = inits.get(name);
            if (init === undefined) fail("ModuleNotFoundError", "No module named '" + name + "'");
            const m = { kind: MODULE, name: name };
            modules.set(name, m);
            init();
            return m;
        },
        // Host hooks (see the top-level functions below).
        has: function (name) { return tag(lookupEntry(name), FN); },
        call: function (name, args) {
            const f = lookupEntry(name);
            if (!tag(f, FN)) fail("NameError", "entry module has no function '" + name + "'");
            const converted = [];
            if (args !== null && args !== undefined) {
                if (!Array.isArray(args)) fail("TypeError", "call arguments must be an array");
                for (let i = 0; i < args.length; i++) converted.push(fromHost(args[i], 0));
            }
            return toHost(invoke(f, converted), 0);
        }
    });
})();
// Host hooks: reached by global slot from an embedder, never by guest Python.
function __zipp_py_has(name) { return __zipp_py.has(name); }
function __zipp_py_call(name, args) { return __zipp_py.call(name, args); }
function __zipp_py_take_ui() { const out = __zipp_py_ui; __zipp_py_ui = []; return out; }
function __zipp_py_set_input(json) {
    const i = JSON.parse(json);
    __zipp_py_input = {
        mx: Number(i.mx) || 0, my: Number(i.my) || 0, down: !!i.down, clicked: !!i.clicked,
        keys: (i.keys !== null && typeof i.keys === "object") ? i.keys : {},
        w: Number(i.w) || 0, h: Number(i.h) || 0
    };
    return null;
}
// The Rust frontend replaces this prototype's body with Python bytecode.
// Keeping the entry CALL in the bootstrap avoids relocating any JS jumps.
function __zipp_py_entry() {}
__zipp_py_entry();
