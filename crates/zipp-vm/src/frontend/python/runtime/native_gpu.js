/* ZIPP Python runtime — the native CLI's synchronous GPU bridge. Apache-2.0.
 *
 * Compiled in only with the `python-native-gpu` feature (the native `zipp`
 * CLI). It adds two functions to `_zipp_gpu`:
 *
 *   native_open()          -> bool: whether the embedder runs graphs on a
 *                             GPU (asked once, on a program's first graph;
 *                             the embedder starts its GPU then, not before)
 *   native(kind, payload)  -> the embedder's reply dict ({"ok": True,
 *                             "value": ...} or {"ok": False, "error": ...}),
 *                             for the same five request kinds a browser
 *                             host serves, answered before it returns
 *   native_step(session, ids, readback, arrays)
 *                          -> a prepared session's run of steps in binary
 *                             form, for the embedder's native replay of it:
 *                             `arrays` each step's fed float32 storages in
 *                             the order of `ids` (comma-separated input ids),
 *                             `readback` the output names one per line.
 *                             None when the replay does not serve it (send
 *                             `gpu.session.run`, which then serves it as
 *                             always); (False, code, message) when it failed
 *                             once device work began (the session is
 *                             poisoned); else (True, first, step, names,
 *                             shapes, values, stats): `values[s][o]` step s's
 *                             output `names[o]` as float32 storage, `stats`
 *                             the run's statistics as JSON text (decoded by
 *                             whoever reads them)
 *
 * Both go through `__zippHostCall`, which throws when the embedder installed
 * no host (any other build, a sandbox, the browser engine answering "unknown
 * host call"): `native_open()` is then False and zipp_gpu evaluates on the
 * CPU tensor kernels exactly as it always has. Nothing here runs a guest
 * callback: zipp_gpu calls it and runs callbacks where its CPU path does.
 *
 * Transport: the payload crosses as JSON text, with every float32 tensor
 * storage replaced by `{"$f32": [byteOffset, length]}` into one byte buffer
 * the host reads in place (`__zipp_ngpu_up`); the reply comes back the same
 * way through `__zipp_ngpu_down`. Both are reused and grown geometrically,
 * because a buffer the host has resolved stays pinned for the VM's lifetime.
 */
var __zipp_ngpu_up = new Uint8Array(1 << 16);
var __zipp_ngpu_down = new Uint8Array(1 << 16);
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T, E = rt.E;
    let opened = null;
    function grow(buffer, size) {
        let next = buffer.length;
        while (next < size) next *= 2;
        return next === buffer.length ? buffer : new Uint8Array(next);
    }
    // Python value -> JSON-able JavaScript, tensors out of line (the same
    // conversion a browser host's request goes through).
    function encode(v, depth, blobs, state) {
        if (depth > 32) rt.fail(E.TypeError, "GPU request nesting limit exceeded");
        if (v === null || typeof v === "boolean" || typeof v === "string") return v;
        if (typeof v === "number") return v;
        if (typeof v === "bigint") return v >= BigInt(Number.MIN_SAFE_INTEGER) && v <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(v) : v.toString();
        if (rt.isFloat32Storage(v)) {
            const d = v.data, at = state.bytes;
            blobs.push(new Uint8Array(d.buffer, d.byteOffset, d.byteLength));
            state.bytes += d.byteLength;
            return { $f32: [at, d.length] };
        }
        if (v.cls === T.list || v.cls === T.tuple) { const out = []; for (let i = 0; i < v.items.length; i++) out.push(encode(v.items[i], depth + 1, blobs, state)); return out; }
        if (v.cls === T.dict) { const o = {}; const entries = rt.dictEntryList(v); for (let i = 0; i < entries.length; i++) o[rt.str(entries[i][0])] = encode(entries[i][1], depth + 1, blobs, state); return o; }
        if (v.cls === T.set || v.cls === T.frozenset) { const items = rt.setList(v), out = []; for (let i = 0; i < items.length; i++) out.push(encode(items[i], depth + 1, blobs, state)); return out; }
        return rt.str(v);
    }
    // Reply JavaScript -> Python, as a browser host's reply is delivered:
    // integral numbers become ints, float32 arrays tensor storage.
    function decode(v, depth) {
        if (depth > 32) rt.fail(E.TypeError, "GPU reply nesting limit exceeded");
        if (v === null || v === undefined) return null;
        if (typeof v === "boolean" || typeof v === "string") return v;
        if (typeof v === "number") return Number.isInteger(v) ? BigInt(v) : v;
        if (Array.isArray(v)) { const items = []; for (let i = 0; i < v.length; i++) items.push(decode(v[i], depth + 1)); return rt.list(items); }
        const f32 = v.$f32;
        if (f32 !== undefined && Array.isArray(f32) && Object.keys(v).length === 1) {
            const at = f32[0], length = f32[1];
            return rt.float32Storage(new Float32Array(__zipp_ngpu_down.slice(at, at + length * 4).buffer));
        }
        const d = rt.dict(), keys = Object.keys(v);
        for (let i = 0; i < keys.length; i++) rt.dictSet(d, keys[i], decode(v[keys[i]], depth + 1));
        return d;
    }
    function open() {
        if (opened === null) {
            try { opened = String(__zippHostCall("zipp.gpu.open")) === "1"; } catch (e) { opened = false; }
        }
        return opened;
    }
    function request(kind, payload) {
        const blobs = [], state = { bytes: 0 };
        const text = JSON.stringify(encode(payload, 0, blobs, state));
        __zipp_ngpu_up = grow(__zipp_ngpu_up, state.bytes);
        let at = 0;
        for (let i = 0; i < blobs.length; i++) { __zipp_ngpu_up.set(blobs[i], at); at += blobs[i].byteLength; }
        // The reply's JSON, then its bytes copied in once the buffer fits them.
        const head = String(__zippHostCall("zipp.gpu.request", kind, text, state.bytes));
        const split = head.indexOf("\n");
        const bytes = Number(head.slice(0, split));
        if (bytes > 0) {
            __zipp_ngpu_down = grow(__zipp_ngpu_down, bytes);
            __zippHostCall("zipp.gpu.fetch");
        }
        return decode(JSON.parse(head.slice(split + 1)), 0);
    }
    // The binary prepared step (see the header): no JSON either way.
    function step(token, ids, readback, arrays) {
        const items = arrays.items;
        let bytes = 0;
        for (let i = 0; i < items.length; i++) {
            if (!rt.isFloat32Storage(items[i])) return null;
            bytes += items[i].data.byteLength;
        }
        __zipp_ngpu_up = grow(__zipp_ngpu_up, bytes);
        let at = 0;
        for (let i = 0; i < items.length; i++) {
            const d = items[i].data;
            __zipp_ngpu_up.set(new Uint8Array(d.buffer, d.byteOffset, d.byteLength), at);
            at += d.byteLength;
        }
        const head = String(__zippHostCall("zipp.gpu.step", token, ids, readback, bytes));
        if (head.length === 0) return null;
        if (head.charCodeAt(0) === 69) { // "E\n<code>\n<message>"
            const split = head.indexOf("\n", 2);
            return rt.tuple([false, head.slice(2, split), head.slice(split + 1)]);
        }
        // "R\n<bytes>\n<fetch>\n<json>\n<stats>"
        const a = head.indexOf("\n", 2), b = head.indexOf("\n", a + 1), c = head.indexOf("\n", b + 1);
        const size = Number(head.slice(2, a));
        if (head.charCodeAt(a + 1) === 49) {
            __zipp_ngpu_down = grow(__zipp_ngpu_down, size);
            __zippHostCall("zipp.gpu.fetch");
        }
        const reply = JSON.parse(head.slice(b + 1, c)), outputs = reply.outputs;
        const names = [], shapes = [];
        let stepBytes = 0;
        for (let o = 0; o < outputs.length; o++) {
            names.push(outputs[o][0]);
            shapes.push(decode(outputs[o][2], 1));
            stepBytes += outputs[o][1] * 4;
        }
        const values = [];
        for (let s = 0, off = 0; off < size; s++) {
            const row = [];
            for (let o = 0; o < outputs.length; o++) {
                const n = outputs[o][1] * 4;
                row.push(rt.float32Storage(new Float32Array(__zipp_ngpu_down.slice(off, off + n).buffer)));
                off += n;
            }
            values.push(rt.list(row));
            if (stepBytes === 0) break;
        }
        return rt.tuple([true, decode(reply.first, 1), decode(reply.step, 1), rt.list(names), rt.list(shapes),
            rt.list(values), head.slice(c + 1)]);
    }
    const factory = rt.builtinModules.get("_zipp_gpu");
    if (factory === undefined) return;
    rt.builtinModules.set("_zipp_gpu", () => {
        const m = factory();
        m.globals.set("native_open", rt.builtin("native_open", 0, () => open()));
        m.globals.set("native", rt.builtin("native", 2, (a) => {
            if (typeof a[0] !== "string") rt.fail(E.TypeError, "native() needs a request kind");
            if (!open()) rt.fail(E.RuntimeError, "no native GPU host");
            return request(a[0], a[1]);
        }));
        m.globals.set("native_step", rt.builtin("native_step", 4, (a) => {
            if (typeof a[0] !== "string" || typeof a[1] !== "string" || typeof a[2] !== "string" ||
                a[3] === null || typeof a[3] !== "object" || a[3].cls !== T.list) rt.fail(E.TypeError, "native_step(session, ids, readback, arrays)");
            if (!open()) rt.fail(E.RuntimeError, "no native GPU host");
            return step(a[0], a[1], a[2], a[3]);
        }));
        return m;
    });
})(__zipp_py);
