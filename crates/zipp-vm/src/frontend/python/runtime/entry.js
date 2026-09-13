/* ZIPP Python runtime — host hooks and the program entry. Apache-2.0.
 *
 * The host reaches these top-level functions by global slot; guest Python
 * never can (its names resolve through the runtime's private tables).
 */
function __zipp_py_has(name) { return __zipp_py.__rt.hostHas(name); }
function __zipp_py_call(name, args) { return __zipp_py.__rt.hostCall(name, args); }
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
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T, E = rt.E;
    // A Python exception leaving the program: the VM reports `name: message`
    // from the exception object's own fields, formatted like a traceback tail.
    function hostError(e) {
        const exc = rt.normexc(e);
        if (exc.cls === E.SystemExit) {
            const code = exc.args.items.length ? exc.args.items[0] : 0n;
            if (code === null || code === 0n || code === false) return null;
            const err = new Error(typeof code === "string" ? code : "SystemExit: " + rt.str(code)); err.name = "SystemExit"; return err;
        }
        let message = rt.str(exc);
        let chain = "";
        let cause = exc.cause, ctx = exc.context;
        if (cause !== null) chain = rt.typeOf(cause).name + ": " + rt.str(cause) + "\n\nThe above exception was the direct cause of the following exception:\n\n";
        else if (ctx !== null && !exc.suppress) chain = rt.typeOf(ctx).name + ": " + rt.str(ctx) + "\n\nDuring handling of the above exception, another exception occurred:\n\n";
        const where = exc.traceback ? "  File " + exc.traceback.replace(/^ \((.*):(\d+)\)$/, '"$1", line $2') + "\n" : "";
        const err = new Error(chain + "Traceback (most recent call last):\n" + where + exc.cls.name + (message ? ": " + message : ""));
        err.name = exc.cls.name;
        err.message = (message ? message : "") + (exc.traceback || "");
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
        if (Array.isArray(v)) return rt.list(v.map((x) => fromHost(x, depth + 1)));
        if (typeof v === "object") { const d = rt.dict(); for (const k of Object.keys(v)) rt.dictSet(d, k, fromHost(v[k], depth + 1)); return d; }
        rt.fail(E.TypeError, "unsupported host value");
    }
    function toHost(v, depth) {
        if (depth > 32) rt.fail(E.TypeError, "value nesting limit exceeded");
        if (v === null || typeof v === "boolean" || typeof v === "string" || typeof v === "number") return v;
        if (typeof v === "bigint") return v >= BigInt(Number.MIN_SAFE_INTEGER) && v <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(v) : v.toString();
        if (v.cls === T.list || v.cls === T.tuple) return v.items.map((x) => toHost(x, depth + 1));
        if (v.cls === T.dict) { const o = {}; for (const [k, x] of rt.dictEntries(v)) o[rt.str(k)] = toHost(x, depth + 1); return o; }
        if (v.cls === T.set || v.cls === T.frozenset) return rt.setList(v).map((x) => toHost(x, depth + 1));
        return rt.str(v);
    }
    function entryGlobal(name) {
        const m = rt.entryModule();
        if (m === null) return undefined;
        return m.globals.get(name);
    }
    rt.hostHas = function (name) {
        const f = entryGlobal(String(name));
        return f !== undefined && f !== null && typeof f === "object" && (f.cls === T.function || f.cls === T.builtin_function_or_method || f.cls === T.method || f.isType === true || rt.typeMethod(f, "__call__") !== undefined);
    };
    rt.hostCall = function (name, args) {
        try {
            const f = entryGlobal(String(name));
            if (f === undefined) rt.fail(E.NameError, "entry module has no function '" + name + "'");
            const converted = [];
            if (args !== null && args !== undefined) {
                if (!Array.isArray(args)) rt.fail(E.TypeError, "call arguments must be an array");
                for (let i = 0; i < args.length; i++) converted.push(fromHost(args[i], 0));
            }
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
            // function-shaped record and the only argument is the args array.
            // A member call keeps it on the VM's frame stack (no native
            // re-entry, which the wasm build allows only one level of).
            const holder = { code: body, globals: new Map(), cells: [] };
            holder.code([]);
            rt.flushOut();
            return null;
        } catch (e) {
            rt.flushOut();
            const err = hostError(e);
            if (err === null) return null;
            throw err;
        }
    };
})(__zipp_py);
// The Rust frontend replaces this prototype's body with Python bytecode that
// registers the modules and runs the entry module. Keeping the CALL in the
// bootstrap avoids relocating any JS jumps.
function __zipp_py_entry() {}
__zipp_py.__rt.runEntry(__zipp_py_entry);
