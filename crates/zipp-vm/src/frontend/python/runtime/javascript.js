/* Opt-in trusted JavaScript/Python interoperability inside this ZIPP VM.
 * This file is compiled by ZIPP, never executed by the browser's JS engine.
 * It shares the program's VM globals: it is not an isolation boundary between
 * mutually untrusted Python and JavaScript. The ordinary Python build omits it.
 */
(function (R) {
    "use strict";
    const rt = R.__rt, E = rt.E;
    // An indirect call to the VM's captured eval intrinsic, not browser eval.
    const vmEval = eval;
    function convert(value, depth, seen, budget) {
        if (++budget.count > 10000 || depth > 32) rt.fail(E.TypeError, "JavaScript result exceeds conversion limits");
        if (value === null || value === undefined) return null;
        if (typeof value === "boolean" || typeof value === "string" || typeof value === "bigint") return value;
        if (typeof value === "number") {
            if (!Number.isFinite(value)) rt.fail(E.TypeError, "JavaScript result must contain finite numbers");
            return Number.isInteger(value) ? BigInt(value) : value;
        }
        if (typeof value !== "object") rt.fail(E.TypeError, "JavaScript functions and symbols cannot cross as data");
        if (seen.has(value)) rt.fail(E.TypeError, "JavaScript result contains a cycle");
        seen.add(value);
        try {
            if (Array.isArray(value)) {
                if (value.length > 10000) rt.fail(E.TypeError, "JavaScript array exceeds conversion limits");
                const items = [];
                for (let i = 0; i < value.length; i++) {
                    const d = Object.getOwnPropertyDescriptor(value, String(i));
                    if (d && !("value" in d)) rt.fail(E.TypeError, "JavaScript accessors cannot cross as data");
                    items.push(convert(d ? d.value : undefined, depth + 1, seen, budget));
                }
                return rt.list(items);
            }
            const proto = Object.getPrototypeOf(value);
            if (proto !== null && proto !== Object.prototype) rt.fail(E.TypeError, "JavaScript result must be a plain data object");
            const keys = Reflect.ownKeys(value);
            if (keys.length > 10000) rt.fail(E.TypeError, "JavaScript object exceeds conversion limits");
            const out = rt.dict();
            for (const key of keys) {
                const d = Object.getOwnPropertyDescriptor(value, key);
                if (typeof key !== "string" || !d || !("value" in d)) rt.fail(E.TypeError, "JavaScript symbols/accessors cannot cross as data");
                rt.dictSet(out, key, convert(d.value, depth + 1, seen, budget));
            }
            return out;
        } finally { seen.delete(value); }
    }
    const evaluate = rt.builtin("eval", 1, (args) => {
        const source = rt.needStr(args[0]);
        if (source.length > 32768) rt.fail(E.ValueError, "JavaScript source exceeds 32768 UTF-16 units");
        let result;
        try { result = vmEval(source); }
        catch (error) { rt.fail(E.RuntimeError, "JavaScript: " + String(error)); }
        return convert(result, 0, new Set(), {count: 0});
    });
    for (const name of ["javascript", "js"]) rt.defineModule(name, (g) => {
        g.set("eval", evaluate);
        g.set("engine", "zipp-same-vm");
    });
})(__zipp_py);
