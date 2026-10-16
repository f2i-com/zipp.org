/* ZIPP Python runtime — host hooks and the program entry. Apache-2.0.
 *
 * The host reaches these top-level functions by global slot; guest Python
 * never can (its names resolve through the runtime's private tables).
 */
function __zipp_py_has(name) { return __zipp_py.__rt.hostHas(name); }
function __zipp_py_call(name, args) { return __zipp_py.__rt.hostCall(name, args); }
function __zipp_py_take_ui() { const out = __zipp_py_ui; __zipp_py_ui = []; return out; }
function __zipp_py_take_host() { return __zipp_py.__rt.takeHostRequests(); }
function __zipp_py_vfs_changed() { return __zipp_py.__rt.vfsChangedText(); }
function __zipp_py_exit_status() { return __zipp_py.__rt.exitStatus(); }
function __zipp_py_set_input(json) {
    const i = JSON.parse(json);
    __zipp_py_input = {
        mx: Number(i.mx) || 0, my: Number(i.my) || 0, down: !!i.down, clicked: !!i.clicked,
        keys: (i.keys !== null && typeof i.keys === "object") ? i.keys : {},
        w: Number(i.w) || 0, h: Number(i.h) || 0
    };
    return null;
}
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T, E = rt.E;
    // What the program's end asks of the process, for a host that owns one
    // (the CLI reads it through `__zipp_py_exit_status`): for a SystemExit,
    // an integer status, or, for any other code, the text CPython prints
    // before exiting with status 1; for any other exception leaving the
    // program, the traceback CPython prints before exiting with status 1
    // (`rt.formatException`, built when asked for). `null` until either.
    let exitStatus = null, uncaught = null;
    rt.exitStatus = function () {
        if (exitStatus === null && uncaught !== null) {
            const exc = uncaught;
            uncaught = null;
            let text;
            try { text = rt.formatException(exc); } catch (e) { text = rt.typeOf(exc).name + "\n"; }
            exitStatus = text.replace(/\n$/, "");
        }
        return exitStatus;
    };
    // A Python exception leaving the program: the VM reports `name: message`
    // from the exception object's own fields, formatted like a traceback tail.
    function hostError(e) {
        const exc = rt.normexc(e);
        if (rt.isSubclass(exc.cls, E.SystemExit)) {
            const items = exc.args.items;
            const code = items.length === 0 ? null : items.length === 1 ? items[0] : exc.args;
            if (code === null || code === 0n || code === false) return null;
            // CPython truncates a 64-bit code to a C int, and exits -1 when
            // the code does not fit 64 bits.
            exitStatus = typeof code === "bigint"
                ? (BigInt.asIntN(64, code) === code ? Number(BigInt.asIntN(32, code)) : -1)
                : code === true ? 1 : rt.str(code);
            const err = new Error(rt.str(code)); err.name = "SystemExit"; return err;
        }
        let message = rt.str(exc);
        let chain = "";
        let cause = exc.cause, ctx = exc.context;
        if (cause !== null) chain = "The direct cause: " + rt.typeOf(cause).name + ": " + rt.str(cause) + rt.excLocation(cause);
        else if (ctx !== null && !exc.suppress) chain = "While handling: " + rt.typeOf(ctx).name + ": " + rt.str(ctx) + rt.excLocation(ctx);
        // `name: message` is what the host prints; the frames follow on
        // their own lines, outermost first, like CPython.
        const frames = rt.tracebackText(exc);
        const err = new Error(message);
        err.name = exc.cls.name;
        err.message = (message ? message : "") + rt.excLocation(exc) + (frames ? "\n" + frames.replace(/\n$/, "") : "") + (chain ? "\n" + chain : "");
        err.pyexc = exc;
        return err;
    }
    rt.hostError = hostError;
    function fromHost(v, depth) {
        if (depth > 32) rt.fail(E.TypeError, "host value nesting limit exceeded");
        if (v === null || v === undefined) return null;
        if (typeof v === "boolean" || typeof v === "string") return v;
        if (typeof v === "number") return Number.isInteger(v) ? BigInt(v) : v;
        if (typeof v === "bigint") return v;
        // Binary tensor transport: the VM built this array from the host's
        // copy, so the storage can own it outright.
        if (v instanceof Float32Array) return rt.float32Storage(v);
        if (Array.isArray(v)) { const items = []; for (let i = 0; i < v.length; i++) items.push(fromHost(v[i], depth + 1)); return rt.list(items); }
        if (typeof v === "object") { const d = rt.dict(); const keys = Object.keys(v); for (let i = 0; i < keys.length; i++) rt.dictSet(d, keys[i], fromHost(v[keys[i]], depth + 1)); return d; }
        rt.fail(E.TypeError, "unsupported host value");
    }
    function toHost(v, depth) {
        if (depth > 32) rt.fail(E.TypeError, "value nesting limit exceeded");
        if (v === null || typeof v === "boolean" || typeof v === "string" || typeof v === "number") return v;
        if (typeof v === "bigint") return v >= BigInt(Number.MIN_SAFE_INTEGER) && v <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(v) : v.toString();
        // A float32 tensor storage leaves as its bytes (the VM copies them
        // into the host value), not one number per element.
        if (rt.isFloat32Storage(v)) return v.data;
        if (v.cls === T.list || v.cls === T.tuple) { const out = []; for (let i = 0; i < v.items.length; i++) out.push(toHost(v.items[i], depth + 1)); return out; }
        if (v.cls === T.dict) { const o = Object.create(null); const entries = rt.dictEntryList(v); for (let i = 0; i < entries.length; i++) o[rt.str(entries[i][0])] = toHost(entries[i][1], depth + 1); return o; }
        if (v.cls === T.set || v.cls === T.frozenset) { const items = rt.setList(v), out = []; for (let i = 0; i < items.length; i++) out.push(toHost(items[i], depth + 1)); return out; }
        return rt.str(v);
    }
    function entryGlobal(name) {
        const m = rt.entryModule();
        if (m === null) return undefined;
        return m.globals.get(name);
    }
    // Host requests leave as plain data; the answer to one comes back through
    // `__zipp_py_deliver(id, reply)`, which `pythonCall` reaches like a
    // program-defined hook.
    rt.takeHostRequests = (function (take) {
        return function () {
            // Converting a payload can run guest __str__; its exception leaves as a host error.
            try {
                const taken = take(), out = [];
                for (let i = 0; i < taken.length; i++) out.push({ id: taken[i].id, kind: taken[i].kind, payload: toHost(taken[i].payload, 0) });
                return out;
            } catch (e) {
                const err = hostError(e);
                if (err === null) return [];
                throw err;
            }
        };
    })(rt.takeHostRequests);
    const runtimeHooks = new Map([
        ["__zipp_py_deliver", function (id, reply) {
            const n = typeof id === "bigint" ? Number(id) : Number(id);
            if (!Number.isSafeInteger(n) || n < 1) rt.fail(E.TypeError, "deliver: a request id is a positive integer");
            return rt.deliverHost(n, reply);
        }],
        ["__zipp_py_pending_host", function () { return BigInt(rt.pendingHostRequests()); }],
        // Versioned JSON distinguishes deletion from an empty file, and escapes
        // path delimiters. Each change has either deleted:true or base64:string.
        ["__zipp_py_vfs_changed", function () { return rt.vfsChangedText(); }],
        ["__zipp_py_vfs_list", function () { return rt.vfs.list(); }],
    ]);
    rt.vfsChangedText = function () {
            // Handles the program left open are flushed, as at CPython's exit.
            rt.flushOpenFiles();
            const changes = [];
            for (const path of rt.vfs.changed()) {
                const bytes = rt.vfs.get(path);
                // Files are Uint8Arrays: encode natively, not byte by byte here.
                changes.push(bytes === undefined ? { path: path, deleted: true } : { path: path, base64: bytes instanceof Uint8Array ? bytes.toBase64() : base64(bytes) });
            }
            return JSON.stringify({ version: 1, changes: changes });
    };
    const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    function base64(bytes) {
        let out = "";
        for (let i = 0; i < bytes.length; i += 3) {
            const a = bytes[i], b = i + 1 < bytes.length ? bytes[i + 1] : 0, c = i + 2 < bytes.length ? bytes[i + 2] : 0;
            out += B64[a >> 2] + B64[((a & 3) << 4) | (b >> 4)] + (i + 1 < bytes.length ? B64[((b & 15) << 2) | (c >> 6)] : "=") + (i + 2 < bytes.length ? B64[c & 63] : "=");
        }
        return out;
    }
    rt.hostHas = function (name) {
        if (runtimeHooks.has(String(name))) return true;
        const f = entryGlobal(String(name));
        return f !== undefined && f !== null && typeof f === "object" && (f.cls === T.function || f.cls === T.builtin_function_or_method || f.cls === T.method || f.isType === true || rt.typeMethod(f, "__call__") !== undefined);
    };
    rt.hostCall = function (name, args) {
        try {
            const converted = [];
            if (args !== null && args !== undefined) {
                if (!Array.isArray(args)) rt.fail(E.TypeError, "call arguments must be an array");
                for (let i = 0; i < args.length; i++) converted.push(fromHost(args[i], 0));
            }
            const hook = runtimeHooks.get(String(name));
            if (hook !== undefined) {
                // A direct call: `apply` would re-enter the VM natively.
                const r = hook(converted[0], converted[1]);
                rt.flushOut();
                return toHost(r, 0);
            }
            const f = entryGlobal(String(name));
            if (f === undefined) rt.fail(E.NameError, "entry module has no function '" + name + "'");
            const result = toHost(rt.call(f, converted, null), 0);
            rt.flushOut();
            return result;
        } catch (e) {
            rt.flushOut();
            const err = hostError(e);
            if (err === null) return null;
            throw err;
        }
    };
    // The entry: run the main module and surface an uncaught exception as a
    // JS Error the engine reports as `Name: message`.
    rt.runEntry = function (body) {
        try {
            // The entry code object follows the Python ABI: `this` is a
            // function-shaped record, and the entry takes no parameters.
            // A member call keeps it on the VM's frame stack (no native
            // re-entry, which the wasm build allows only one level of).
            const holder = { code: body, globals: new Map(), cells: [] };
            holder.code();
            rt.flushOut();
            rt.flushOpenFiles();
            return null;
        } catch (e) {
            rt.flushOut();
            rt.flushOpenFiles();
            const err = hostError(e);
            if (err === null) return null;
            if (err.pyexc !== undefined && exitStatus === null) uncaught = err.pyexc;
            throw err;
        }
    };
})(__zipp_py);
// The engine's registry of the runtime (`vm::py_rt`), taken once everything
// above has set `R` up: `R`, an array `R` keeps (which holds what the
// registry names) and a dict record (its layout).
try { if (typeof __zipp_py_bind === "function") { __zipp_py.PINS = []; __zipp_py_bind(__zipp_py, __zipp_py.PINS, __zipp_py.__rt.dict()); } } catch (e) { }
// The Rust frontend replaces this prototype's body with Python bytecode that
// registers the modules and runs the entry module. Keeping the CALL in the
// bootstrap avoids relocating any JS jumps.
function __zipp_py_entry() {}
__zipp_py.__rt.runEntry(__zipp_py_entry);
