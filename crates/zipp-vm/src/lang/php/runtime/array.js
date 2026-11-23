/* ZIPP PHP runtime — ordered CoW PhpArray, path mutations, foreach, and array builtins. Apache-2.0. */
(function (R) {
    "use strict";
    const rt = R.__rt;

    // Canonical PHP array key: numeric string representing an exact i64 without
    // leading zeros or whitespace becomes a bigint key; booleans/floats/null coerce too.
    function arrKey(k) {
        k = rt.deref(k);
        if (typeof k === "bigint") return k;
        if (typeof k === "string") {
            if (k.length > 0 && k.length <= 20) {
                const c0 = k.charCodeAt(0);
                if ((c0 >= 48 && c0 <= 57) || c0 === 45) {
                    if (k === "0") return 0n;
                    if (/^(?:-?[1-9]\d*)$/.test(k)) {
                        const bi = BigInt(k);
                        if (bi >= rt.I64_MIN && bi <= rt.I64_MAX) return bi;
                    }
                }
            }
            return k;
        }
        if (typeof k === "number") return rt.floatToInt64(k);
        if (k === true) return 1n;
        if (k === false) return 0n;
        if (k === null || k === undefined) return "";
        if (typeof k === "object" && k.__php === 4) return BigInt(k.id || 1);
        rt.throwError("TypeError", "Illegal offset type");
    }
    rt.arrKey = arrKey;

    function makeArr() {
        return { __php: 1, m: new Map(), next: 0n, shared: false, hasRefs: false, pos: 0, _list: true, _e: null };
    }
    rt.makeArr = makeArr;
    R.__newMap = function () { return new Map(); };
    R.__mapKv = typeof __zipp_php_map_kv === "function"
        ? __zipp_php_map_kv
        : function (m) {
            const ks = [], vs = [];
            m.forEach(function (v, k) { ks.push(k); vs.push(v); });
            return [ks, vs, false];
        };

    function cloneArr(a) {
        const out = makeArr();
        out.next = a.next;
        out.pos = a.pos;
        out.hasRefs = a.hasRefs;
        out._list = a._list;
        if (!a.hasRefs) {
            for (const [k, v] of a.m) {
                if (v !== null && typeof v === "object" && v.__php === 1) v.shared = true;
                out.m.set(k, v);
            }
            return out;
        }
        for (const [k, v] of a.m) {
            // Preserve live PhpRef entries on copy only if same ref, otherwise mark shared CoW
            if (rt.isRef(v)) {
                out.m.set(k, v);
            } else {
                out.m.set(k, rt.share(v));
            }
        }
        return out;
    }
    rt.cloneArr = cloneArr;

    function cow(a) {
        if (!a.shared) return a;
        return cloneArr(a);
    }
    rt.cow = cow;

    function arrSetInPlace(a, key, val) {
        const k = arrKey(key);
        const v = (val !== null && typeof val === "object") ? rt.share(val) : val;
        if (a._list && !(typeof k === "bigint" && k >= 0n && k < a.next)) a._list = false;
        a._e = null;
        if (a.hasRefs) {
            const existing = a.m.get(k);
            if (rt.isRef(existing)) {
                existing.v = v;
            } else {
                a.m.set(k, v);
            }
        } else {
            a.m.set(k, v);
        }
        if (typeof k === "bigint" && k >= a.next) {
            a.next = k < rt.I64_MAX ? k + 1n : rt.I64_MAX;
        }
    }
    function arrPushInPlace(a, val) {
        const k = a.next;
        const v = (val !== null && typeof val === "object") ? rt.share(val) : val;
        if (k >= rt.I64_MAX) a._list = false;
        a._e = null;
        a.m.set(k, v);
        a.next = k < rt.I64_MAX ? k + 1n : rt.I64_MAX;
    }
    rt.arrSetInPlace = arrSetInPlace;
    rt.arrPushInPlace = arrPushInPlace;

    rt.toArray = function (v) {
        v = rt.deref(v);
        if (v && v.__php === 1) return rt.share(v);
        const out = makeArr();
        if (v === null || v === undefined) return out;
        if (v && v.__php === 2) {
            out._list = false;
            for (const k in v.props) out.m.set(k, rt.share(rt.deref(v.props[k])));
            return out;
        }
        arrPushInPlace(out, v);
        return out;
    };

    // ---- Literal builders ----
    R.arr = makeArr;
    R.arrPush = function (a, val) {
        arrPushInPlace(a, val);
        return a;
    };
    R.arrSetLit = function (a, key, val) {
        arrSetInPlace(a, key, val);
        return a;
    };
    R.arrRefLit = function (a, key, ref) {
        const r = rt.isRef(ref) ? ref : rt.mkRef(ref);
        a.hasRefs = true;
        a._e = null;
        if (key === undefined) {
            const k = a.next;
            if (k >= rt.I64_MAX) a._list = false;
            a.m.set(k, r);
            a.next = k < rt.I64_MAX ? k + 1n : rt.I64_MAX;
        } else {
            const k = arrKey(key);
            if (a._list && !(typeof k === "bigint" && k >= 0n && k < a.next)) a._list = false;
            a.m.set(k, r);
            if (typeof k === "bigint" && k >= a.next) a.next = k + 1n;
        }
        return a;
    };
    R.arrSpread = function (a, src) {
        src = rt.deref(src);
        if (src && src.__php === 1) {
            for (const [k, v] of src.m) {
                if (typeof k === "string") {
                    arrSetInPlace(a, k, v);
                } else {
                    arrPushInPlace(a, v);
                }
            }
            return a;
        }
        const it = R.iterStart(src, false);
        while (R.iterNext(it)) {
            if (typeof it.key === "string") arrSetInPlace(a, it.key, it.val);
            else arrPushInPlace(a, it.val);
        }
        return a;
    };

    // ---- Element read / isset / CoW write / nested path write ----
    R.idxGet = function (target, key, quiet) {
        if (target !== null && typeof target === "object" && target.__php === 1) {
            if (typeof key === "bigint") {
                const raw = target.m.get(key);
                if (raw === undefined) return null;
                return (raw !== null && typeof raw === "object" && raw.__php === 5) ? raw.v : raw;
            }
            if (typeof key === "string") {
                const len = key.length;
                if (len === 0 || len > 20) {
                    const raw = target.m.get(key);
                    if (raw === undefined) return null;
                    return (raw !== null && typeof raw === "object" && raw.__php === 5) ? raw.v : raw;
                }
                const c0 = key.charCodeAt(0);
                if (c0 > 57 || (c0 < 48 && c0 !== 45)) {
                    const raw = target.m.get(key);
                    if (raw === undefined) return null;
                    return (raw !== null && typeof raw === "object" && raw.__php === 5) ? raw.v : raw;
                }
            }
            const k = arrKey(key);
            const raw = target.m.get(k);
            if (raw === undefined) return null;
            return (raw !== null && typeof raw === "object" && raw.__php === 5) ? raw.v : raw;
        }
        target = rt.deref(target);
        key = rt.deref(key);
        if (target === null || target === undefined || typeof target === "boolean" || typeof target === "bigint" || typeof target === "number") {
            return null;
        }
        if (target.__php === 1) {
            const k = arrKey(key);
            const raw = target.m.get(k);
            if (raw === undefined) return null;
            return target.hasRefs ? rt.deref(raw) : raw;
        }
        if (typeof target === "string") {
            let idx = Number(rt.toInt(key));
            if (idx < 0) idx = target.length + idx;
            if (idx < 0 || idx >= target.length) return "";
            return target.charAt(idx);
        }
        if (target.__php === 2) {
            const m = rt.findMethod(target.cls, "offsetget");
            if (m) return rt.invokeMethod(target, m, [key], null, target.cls, target.cls, false);
            rt.throwError("Error", "Cannot use object of type " + target.cls.name + " as array");
        }
        return null;
    };

    R.idxIsset = function (target, key) {
        target = rt.deref(target);
        key = rt.deref(key);
        if (target === null || target === undefined) return false;
        if (target.__php === 1) {
            const k = arrKey(key);
            if (!target.m.has(k)) return false;
            return rt.deref(target.m.get(k)) !== null;
        }
        if (typeof target === "string") {
            let idx = Number(rt.toInt(key));
            if (idx < 0) idx = target.length + idx;
            return idx >= 0 && idx < target.length;
        }
        if (target.__php === 2) {
            const m = rt.findMethod(target.cls, "offsetexists");
            if (m) return rt.truthy(rt.invokeMethod(target, m, [key], null, target.cls, target.cls, false));
        }
        return false;
    };

    R.idxSet = function (container, key, val) {
        if (container !== null && typeof container === "object" && container.__php === 1) {
            const a = container.shared ? cloneArr(container) : container;
            if (!a.hasRefs) {
                if (typeof key === "string") {
                    const len = key.length;
                    const c0 = len > 0 ? key.charCodeAt(0) : 0;
                    if (len === 0 || len > 20 || c0 > 57 || (c0 < 48 && c0 !== 45)) {
                        if (val !== null && typeof val === "object") {
                            if (val.__php === 5) val = val.v;
                            if (val !== null && typeof val === "object" && val.__php === 1) val.shared = true;
                        }
                        a._list = false;
                        a._e = null;
                        a.m.set(key, val);
                        return a;
                    }
                } else if (typeof key === "bigint") {
                    if (val !== null && typeof val === "object") {
                        if (val.__php === 5) val = val.v;
                        if (val !== null && typeof val === "object" && val.__php === 1) val.shared = true;
                    }
                    if (a._list && (key < 0n || key >= a.next)) a._list = false;
                    a._e = null;
                    a.m.set(key, val);
                    if (key >= a.next) a.next = key < rt.I64_MAX ? key + 1n : rt.I64_MAX;
                    return a;
                }
            }
            arrSetInPlace(a, key, val);
            return a;
        }
        container = rt.deref(container);
        val = (val !== null && typeof val === "object") ? rt.share(val) : val;
        if (container === null || container === undefined || container === false || container === "") {
            container = makeArr();
        }
        if (container.__php === 1) {
            const a = cow(container);
            arrSetInPlace(a, key, val);
            return a;
        }
        if (typeof container === "string") {
            let idx = Number(rt.toInt(key));
            if (idx < 0) idx = container.length + idx;
            if (idx < 0) return container;
            const s = rt.toStr(val);
            const ch = s.length > 0 ? s.charAt(0) : "\0";
            if (idx >= container.length) {
                return container + " ".repeat(idx - container.length) + ch;
            }
            return container.slice(0, idx) + ch + container.slice(idx + 1);
        }
        if (container.__php === 2) {
            const m = rt.findMethod(container.cls, "offsetset");
            if (m) {
                rt.invokeMethod(container, m, [rt.deref(key), val], null, container.cls, container.cls, false);
                return container;
            }
            rt.throwError("Error", "Cannot use object of type " + container.cls.name + " as array");
        }
        rt.throwError("Error", "Cannot use a scalar value as an array");
    };

    R.idxPush = function (container, val) {
        if (container !== null && typeof container === "object" && container.__php === 1) {
            const a = container.shared ? cloneArr(container) : container;
            if (val !== null && typeof val === "object") {
                if (val.__php === 5) val = val.v;
                if (val !== null && typeof val === "object" && val.__php === 1) val.shared = true;
            }
            const k = a.next;
            if (k >= rt.I64_MAX) a._list = false;
            a._e = null;
            a.m.set(k, val);
            a.next = k < rt.I64_MAX ? k + 1n : rt.I64_MAX;
            return a;
        }
        container = rt.deref(container);
        val = rt.share(rt.deref(val));
        if (container === null || container === undefined || container === false || container === "") {
            container = makeArr();
        }
        if (container.__php === 1) {
            const a = cow(container);
            arrPushInPlace(a, val);
            return a;
        }
        if (container.__php === 2) {
            const m = rt.findMethod(container.cls, "offsetset");
            if (m) {
                rt.invokeMethod(container, m, [null, val], null, container.cls, container.cls, false);
                return container;
            }
            rt.throwError("Error", "Cannot use object of type " + container.cls.name + " as array");
        }
        rt.throwError("Error", "Cannot use a scalar value as an array");
    };

    R.idxUnset = function (container, key) {
        container = rt.deref(container);
        if (!container) return container;
        if (container.__php === 1) {
            const k = arrKey(key);
            if (!container.m.has(k)) return container;
            const a = cow(container);
            a._list = false;
            a._e = null;
            a.m.delete(k);
            return a;
        }
        if (container.__php === 2) {
            const m = rt.findMethod(container.cls, "offsetunset");
            if (m) {
                rt.invokeMethod(container, m, [rt.deref(key)], null, container.cls, container.cls, false);
                return container;
            }
        }
        return container;
    };

    // Nested CoW path write: `$a[$k0][$k1]... = $val` (last key === undefined means `[]` push)
    R.idxSetPath = function (container, keys, val) {
        function rec(cur, depth) {
            const k = keys[depth];
            if (depth === keys.length - 1) {
                return k === undefined ? R.idxPush(cur, val) : R.idxSet(cur, k, val);
            }
            cur = rt.deref(cur);
            if (cur === null || cur === undefined || cur === false || cur === "") {
                cur = makeArr();
            }
            if (cur.__php === 1) {
                const a = cow(cur);
                const ck = k === undefined ? a.next : arrKey(k);
                const child = a.m.has(ck) ? a.m.get(ck) : null;
                const updated = rec(child, depth + 1);
                if (rt.isRef(child)) child.v = updated;
                else {
                    if (a._list && !(typeof ck === "bigint" && ck >= 0n && ck <= a.next && ck < rt.I64_MAX)) a._list = false;
                    a._e = null;
                    a.m.set(ck, updated);
                }
                if (typeof ck === "bigint" && ck >= a.next) a.next = ck + 1n;
                return a;
            }
            if (cur.__php === 2) {
                const child = R.idxGet(cur, k, true);
                const updated = rec(child, depth + 1);
                return R.idxSet(cur, k, updated);
            }
            rt.throwError("Error", "Cannot use a scalar value as an array");
        }
        return rec(container, 0);
    };

    R.idxUnsetPath = function (container, keys) {
        function rec(cur, depth) {
            cur = rt.deref(cur);
            if (!cur) return cur;
            const k = keys[depth];
            if (depth === keys.length - 1) return R.idxUnset(cur, k);
            if (cur.__php === 1) {
                const ck = arrKey(k);
                if (!cur.m.has(ck)) return cur;
                const a = cow(cur);
                const child = a.m.get(ck);
                const updated = rec(child, depth + 1);
                if (rt.isRef(child)) child.v = updated;
                else { a._e = null; a.m.set(ck, updated); }
                return a;
            }
            return cur;
        }
        return rec(container, 0);
    };

    // Obtain a live PhpRef to an array element `$ref[$k0][$k1]...` (mutates containerRef via CoW)
    R.idxRefPath = function (containerRef, keys) {
        let cur = containerRef.v;
        if (cur === null || cur === undefined || cur === false || cur === "") {
            cur = makeArr();
            containerRef.v = cur;
        }
        let parentRef = containerRef;
        for (let i = 0; i < keys.length; i++) {
            let obj = rt.deref(parentRef.v);
            if (obj === null || obj === undefined || obj === false || obj === "") {
                obj = makeArr();
                parentRef.v = obj;
            }
            if (!obj || obj.__php !== 1) rt.throwError("Error", "Cannot create reference to non-array element");
            const a = cow(obj);
            parentRef.v = a;
            const k = keys[i] === undefined ? a.next : arrKey(keys[i]);
            let entry = a.m.get(k);
            if (!rt.isRef(entry)) {
                entry = rt.mkRef(entry === undefined ? null : entry);
                a.hasRefs = true;
                if (a._list && !(typeof k === "bigint" && k >= 0n && k <= a.next && k < rt.I64_MAX)) a._list = false;
                a._e = null;
                a.m.set(k, entry);
                if (typeof k === "bigint" && k >= a.next) a.next = k + 1n;
            }
            parentRef = entry;
        }
        return parentRef;
    };

    // ---- Zero-copy, zero-allocation Foreach iterator ----
    // Mutates `it.key` and `it.val` in place and returns `true` (has item) or `false` (done).
    R.iterStart = function (expr, byRef) {
        let refObj = null;
        if (byRef && rt.isRef(expr)) refObj = expr;
        const v = rt.deref(expr);
        if (v && v.__php === 1) {
            if (byRef) {
                const a = refObj ? (refObj.v = cow(v)) : v;
                const keys = Array.from(a.m.keys());
                return { kind: 1, arr: a, keys: keys, idx: 0, key: null, val: null };
            }
            // By-value foreach: mark `v.shared = true` and flatten once via Rust native
            // `__zipp_php_map_kv` so `foreach` runs in a zero-call bytecode loop with exact snapshot semantics!
            v.shared = true;
            if (typeof __zipp_php_map_kv === "function") {
                const kv = __zipp_php_map_kv(v.m);
                const keys = kv[0], vals = kv[1];
                if (kv[2]) {
                    for (let i = 0, n = vals.length; i < n; i++) {
                        let val = vals[i];
                        if (val !== null && typeof val === "object") {
                            if (val.__php === 5) { val = val.v; vals[i] = val; }
                            if (val !== null && typeof val === "object" && val.__php === 1) val.shared = true;
                        }
                    }
                }
                return { kind: 4, keys: keys, vals: vals, len: keys.length, idx: 0, key: null, val: null };
            }
            const keys = [], vals = [];
            v.m.forEach(function (val, k) {
                if (val !== null && typeof val === "object") {
                    if (val.__php === 5) val = val.v;
                    if (val !== null && typeof val === "object" && val.__php === 1) val.shared = true;
                }
                keys.push(k);
                vals.push(val);
            });
            return { kind: 4, keys: keys, vals: vals, len: keys.length, idx: 0, key: null, val: null };
        }
        if (v && v.__php === 2) {
            if (v.__gen) {
                return { kind: 2, gen: v, started: false, key: null, val: null };
            }
            const getIt = rt.findMethod(v.cls, "getiterator");
            if (getIt) {
                const sub = rt.invokeMethod(v, getIt, [], null, v.cls, v.cls, false);
                return R.iterStart(sub, byRef);
            }
            if (rt.findMethod(v.cls, "valid") && rt.findMethod(v.cls, "current")) {
                const rew = rt.findMethod(v.cls, "rewind");
                if (rew) rt.invokeMethod(v, rew, [], null, v.cls, v.cls, false);
                return { kind: 3, obj: v, first: true, key: null, val: null };
            }
            const keys = [], vals = [];
            for (const k in v.props) {
                if (k.charCodeAt(0) !== 0) { keys.push(k); vals.push(rt.share(rt.deref(v.props[k]))); }
            }
            return { kind: 4, keys: keys, vals: vals, len: keys.length, idx: 0, key: null, val: null };
        }
        return { kind: 4, keys: [], vals: [], len: 0, idx: 0, key: null, val: null };
    };

    R.iterNext = function (it) {
        const kind = it.kind;
        if (kind === 0) {
            const step = it.miter.next();
            if (step.done) return false;
            const pair = step.value;
            it.key = pair[0];
            let v = pair[1];
            if (v !== null && typeof v === "object") {
                if (v.__php === 5) v = v.v;
                if (v !== null && typeof v === "object" && v.__php === 1) v.shared = true;
            }
            it.val = v;
            return true;
        }
        if (kind === 4) {
            if (it.idx >= it.keys.length) return false;
            it.key = it.keys[it.idx];
            it.val = it.vals[it.idx];
            it.idx++;
            return true;
        }
        if (kind === 1) {
            while (it.idx < it.keys.length) {
                const k = it.keys[it.idx++];
                if (!it.arr.m.has(k)) continue;
                let entry = it.arr.m.get(k);
                if (!rt.isRef(entry)) {
                    entry = rt.mkRef(entry);
                    it.arr.hasRefs = true;
                    it.arr._e = null;
                    it.arr.m.set(k, entry);
                }
                it.key = k;
                it.val = entry;
                return true;
            }
            return false;
        }
        if (kind === 2) {
            const g = it.gen;
            if (!it.started) {
                it.started = true;
                rt.genEnsureStarted(g);
            } else {
                rt.genAdvance(g, null);
            }
            if (g.__gen.done) return false;
            it.key = g.__gen.key;
            it.val = g.__gen.val;
            return true;
        }
        if (kind === 3) {
            const o = it.obj;
            if (!it.first) {
                const nm = rt.findMethod(o.cls, "next");
                if (nm) rt.invokeMethod(o, nm, [], null, o.cls, o.cls, false);
            }
            it.first = false;
            const vm = rt.findMethod(o.cls, "valid");
            if (!rt.truthy(rt.invokeMethod(o, vm, [], null, o.cls, o.cls, false))) return false;
            const km = rt.findMethod(o.cls, "key");
            const cm = rt.findMethod(o.cls, "current");
            it.key = km ? rt.invokeMethod(o, km, [], null, o.cls, o.cls, false) : null;
            it.val = rt.invokeMethod(o, cm, [], null, o.cls, o.cls, false);
            return true;
        }
        return false;
    };

    // ---- Register array built-in functions ----
    function reg(name, fn, refMask) {
        rt.funcs.set(name.toLowerCase(), {
            __php: 3, name: name, builtin: fn, refMask: refMask || 0
        });
    }
    rt.regFn = reg;

    function asArr(v, fname) {
        v = rt.deref(v);
        if (!v || v.__php !== 1) rt.throwError("TypeError", fname + "(): Argument must be of type array, " + rt.getDebugType(v) + " given");
        return v;
    }

    reg("count", function (a) {
        a = rt.deref(a);
        if (a && a.__php === 1) return BigInt(a.m.size);
        if (a && a.__php === 2) {
            const m = rt.findMethod(a.cls, "count");
            if (m) return rt.toInt(rt.invokeMethod(a, m, [], null, a.cls, a.cls, false));
        }
        rt.throwError("TypeError", "count(): Argument #1 ($value) must be of type Countable|array");
    });
    reg("sizeof", function (a) { return rt.funcs.get("count").builtin(a); });
    reg("is_array", function (v) { v = rt.deref(v); return !!(v && v.__php === 1); });

    reg("array_key_exists", function (key, arr) {
        const a = asArr(arr, "array_key_exists");
        return a.m.has(arrKey(key));
    });
    reg("key_exists", function (key, arr) {
        const a = asArr(arr, "key_exists");
        return a.m.has(arrKey(key));
    });
    reg("in_array", function (needle, haystack, strict) {
        const a = asArr(haystack, "in_array");
        const st = strict !== undefined && rt.truthy(strict);
        for (const v of a.m.values()) {
            if (st ? rt.strictEq(needle, v) : rt.looseEq(needle, v)) return true;
        }
        return false;
    });
    reg("array_search", function (needle, haystack, strict) {
        const a = asArr(haystack, "array_search");
        const st = strict !== undefined && rt.truthy(strict);
        for (const [k, v] of a.m) {
            if (st ? rt.strictEq(needle, v) : rt.looseEq(needle, v)) return k;
        }
        return false;
    });
    reg("array_keys", function (arr, filter, strict) {
        const a = asArr(arr, "array_keys");
        const out = makeArr();
        const hasFilter = arguments.length >= 2 && filter !== undefined;
        const st = strict !== undefined && rt.truthy(strict);
        for (const [k, v] of a.m) {
            if (!hasFilter || (st ? rt.strictEq(v, filter) : rt.looseEq(v, filter))) {
                arrPushInPlace(out, k);
            }
        }
        return out;
    });
    reg("array_values", function (arr) {
        const a = asArr(arr, "array_values");
        const out = makeArr();
        for (const v of a.m.values()) arrPushInPlace(out, v);
        return out;
    });
    reg("array_key_first", function (arr) {
        const a = asArr(arr, "array_key_first");
        for (const k of a.m.keys()) return k;
        return null;
    });
    reg("array_key_last", function (arr) {
        const a = asArr(arr, "array_key_last");
        if (a.m.size === 0) return null;
        if (a._list) return a.next - 1n;
        if (a._e !== null && a._e !== undefined) return a._e[a._e.length - 1][0];
        let last = null;
        for (const k of a.m.keys()) last = k;
        return last;
    });
    reg("array_is_list", function (arr) {
        const a = asArr(arr, "array_is_list");
        if (a._list) return true;
        let i = 0n;
        for (const k of a.m.keys()) {
            if (k !== i) return false;
            i++;
        }
        return true;
    });

    // Mutating array functions take arg 0 by reference (`refMask = 1`)!
    reg("array_push", function (arrRef) {
        const a = cow(asArr(arrRef, "array_push"));
        for (let i = 1; i < arguments.length; i++) arrPushInPlace(a, arguments[i]);
        if (rt.isRef(arrRef)) arrRef.v = a;
        return BigInt(a.m.size);
    }, 1);
    reg("array_pop", function (arrRef) {
        const a = cow(asArr(arrRef, "array_pop"));
        if (a.m.size === 0) return null;
        a._e = null;
        let lastKey, lastVal;
        if (a._list) {
            lastKey = a.next - 1n;
            lastVal = a.m.get(lastKey);
            a.m.delete(lastKey);
            a.next = lastKey;
        } else {
            for (const [k, v] of a.m) { lastKey = k; lastVal = v; }
            a.m.delete(lastKey);
            if (typeof lastKey === "bigint" && lastKey + 1n === a.next) {
                // Recompute next max int key
                let mx = -1n;
                for (const k of a.m.keys()) if (typeof k === "bigint" && k > mx) mx = k;
                a.next = mx + 1n;
            }
        }
        if (rt.isRef(arrRef)) arrRef.v = a;
        return rt.share(rt.deref(lastVal));
    }, 1);
    reg("array_shift", function (arrRef) {
        const a = asArr(arrRef, "array_shift");
        if (a.m.size === 0) return null;
        const out = makeArr();
        let first = true, res = null;
        for (const [k, v] of a.m) {
            if (first) { first = false; res = rt.share(rt.deref(v)); continue; }
            if (typeof k === "bigint") arrPushInPlace(out, v);
            else arrSetInPlace(out, k, v);
        }
        if (rt.isRef(arrRef)) arrRef.v = out;
        return res;
    }, 1);
    reg("array_unshift", function (arrRef) {
        const a = asArr(arrRef, "array_unshift");
        const out = makeArr();
        for (let i = 1; i < arguments.length; i++) arrPushInPlace(out, arguments[i]);
        for (const [k, v] of a.m) {
            if (typeof k === "bigint") arrPushInPlace(out, v);
            else arrSetInPlace(out, k, v);
        }
        if (rt.isRef(arrRef)) arrRef.v = out;
        return BigInt(out.m.size);
    }, 1);

    reg("array_merge", function () {
        const out = makeArr();
        for (let i = 0; i < arguments.length; i++) {
            const a = asArr(arguments[i], "array_merge");
            for (const [k, v] of a.m) {
                if (typeof k === "string") arrSetInPlace(out, k, v);
                else arrPushInPlace(out, v);
            }
        }
        return out;
    });
    reg("array_replace", function () {
        const out = makeArr();
        for (let i = 0; i < arguments.length; i++) {
            const a = asArr(arguments[i], "array_replace");
            for (const [k, v] of a.m) arrSetInPlace(out, k, v);
        }
        return out;
    });
    reg("array_slice", function (arr, offset, length, preserve) {
        const a = asArr(arr, "array_slice");
        const entries = Array.from(a.m.entries());
        const n = entries.length;
        let off = Number(rt.toInt(offset));
        if (off < 0) off = Math.max(0, n + off);
        if (off > n) off = n;
        let end = n;
        if (length !== undefined && rt.deref(length) !== null) {
            const l = Number(rt.toInt(length));
            end = l < 0 ? Math.max(off, n + l) : Math.min(n, off + l);
        }
        const pk = preserve !== undefined && rt.truthy(preserve);
        const out = makeArr();
        for (let i = off; i < end; i++) {
            const [k, v] = entries[i];
            if (pk || typeof k === "string") arrSetInPlace(out, k, v);
            else arrPushInPlace(out, v);
        }
        return out;
    });
    reg("array_splice", function (arrRef, offset, length, replacement) {
        const a = asArr(arrRef, "array_splice");
        const entries = Array.from(a.m.entries());
        const n = entries.length;
        let off = Number(rt.toInt(offset));
        if (off < 0) off = Math.max(0, n + off);
        if (off > n) off = n;
        let end = n;
        if (length !== undefined && rt.deref(length) !== null) {
            const l = Number(rt.toInt(length));
            end = l < 0 ? Math.max(off, n + l) : Math.min(n, off + l);
        }
        const removed = makeArr();
        const kept = makeArr();
        for (let i = 0; i < off; i++) {
            const [k, v] = entries[i];
            if (typeof k === "string") arrSetInPlace(kept, k, v); else arrPushInPlace(kept, v);
        }
        if (replacement !== undefined) {
            const rep = rt.toArray(replacement);
            for (const v of rep.m.values()) arrPushInPlace(kept, v);
        }
        for (let i = off; i < end; i++) {
            const [k, v] = entries[i];
            if (typeof k === "string") arrSetInPlace(removed, k, v); else arrPushInPlace(removed, v);
        }
        for (let i = end; i < n; i++) {
            const [k, v] = entries[i];
            if (typeof k === "string") arrSetInPlace(kept, k, v); else arrPushInPlace(kept, v);
        }
        if (rt.isRef(arrRef)) arrRef.v = kept;
        return removed;
    }, 1);

    reg("array_map", function (cb) {
        cb = rt.deref(cb);
        const arrs = [];
        for (let i = 1; i < arguments.length; i++) arrs.push(asArr(arguments[i], "array_map"));
        const out = makeArr();
        if (arrs.length === 1) {
            for (const [k, v] of arrs[0].m) {
                const res = cb === null ? rt.share(rt.deref(v)) : rt.call(cb, [rt.deref(v)], null, null, false);
                arrSetInPlace(out, k, res);
            }
            return out;
        }
        const lists = arrs.map(a => Array.from(a.m.values()));
        let maxLen = 0;
        for (const l of lists) if (l.length > maxLen) maxLen = l.length;
        for (let i = 0; i < maxLen; i++) {
            const callArgs = lists.map(l => i < l.length ? rt.deref(l[i]) : null);
            const res = cb === null ? (function () { const r = makeArr(); for (const x of callArgs) arrPushInPlace(r, x); return r; })() : rt.call(cb, callArgs, null, null, false);
            arrPushInPlace(out, res);
        }
        return out;
    });
    reg("array_filter", function (arr, cb, mode) {
        const a = asArr(arr, "array_filter");
        cb = cb !== undefined ? rt.deref(cb) : null;
        const m = mode !== undefined ? Number(rt.toInt(mode)) : 0;
        const out = makeArr();
        for (const [k, v] of a.m) {
            const dv = rt.deref(v);
            let keep;
            if (cb === null) keep = rt.truthy(dv);
            else if (m === 2) keep = rt.truthy(rt.call(cb, [k], null, null, false)); // ARRAY_FILTER_USE_KEY
            else if (m === 1) keep = rt.truthy(rt.call(cb, [dv, k], null, null, false)); // ARRAY_FILTER_USE_BOTH
            else keep = rt.truthy(rt.call(cb, [dv], null, null, false));
            if (keep) arrSetInPlace(out, k, dv);
        }
        return out;
    });
    reg("array_reduce", function (arr, cb, initial) {
        const a = asArr(arr, "array_reduce");
        let acc = initial !== undefined ? rt.deref(initial) : null;
        for (const v of a.m.values()) {
            acc = rt.call(cb, [acc, rt.deref(v)], null, null, false);
        }
        return acc;
    });
    reg("array_walk", function (arrRef, cb, arg) {
        const a = cow(asArr(arrRef, "array_walk"));
        for (const [k, v] of a.m) {
            const r = rt.isRef(v) ? v : rt.mkRef(v);
            a.hasRefs = true;
            a._e = null;
            a.m.set(k, r);
            const args = arguments.length >= 3 ? [r, k, rt.deref(arg)] : [r, k];
            rt.call(cb, args, null, null, false);
        }
        if (rt.isRef(arrRef)) arrRef.v = a;
        return true;
    }, 1);

    reg("array_reverse", function (arr, preserve) {
        const a = asArr(arr, "array_reverse");
        const pk = preserve !== undefined && rt.truthy(preserve);
        const entries = Array.from(a.m.entries()).reverse();
        const out = makeArr();
        for (const [k, v] of entries) {
            if (pk || typeof k === "string") arrSetInPlace(out, k, v);
            else arrPushInPlace(out, v);
        }
        return out;
    });
    reg("array_unique", function (arr) {
        const a = asArr(arr, "array_unique");
        const seen = new Set();
        const out = makeArr();
        for (const [k, v] of a.m) {
            const s = rt.toStr(v);
            if (!seen.has(s)) {
                seen.add(s);
                arrSetInPlace(out, k, v);
            }
        }
        return out;
    });
    reg("array_flip", function (arr) {
        const a = asArr(arr, "array_flip");
        const out = makeArr();
        for (const [k, v] of a.m) arrSetInPlace(out, rt.deref(v), k);
        return out;
    });
    reg("array_chunk", function (arr, len, preserve) {
        const a = asArr(arr, "array_chunk");
        const n = Number(rt.toInt(len));
        if (n < 1) rt.throwError("ValueError", "array_chunk(): Argument #2 ($length) must be greater than 0");
        const pk = preserve !== undefined && rt.truthy(preserve);
        const out = makeArr();
        let cur = null, c = 0;
        for (const [k, v] of a.m) {
            if (cur === null) { cur = makeArr(); c = 0; }
            if (pk) arrSetInPlace(cur, k, v); else arrPushInPlace(cur, v);
            if (++c === n) { arrPushInPlace(out, cur); cur = null; }
        }
        if (cur !== null) arrPushInPlace(out, cur);
        return out;
    });
    reg("array_combine", function (keys, vals) {
        const ka = asArr(keys, "array_combine"), va = asArr(vals, "array_combine");
        if (ka.m.size !== va.m.size) {
            rt.throwError("ValueError", "array_combine(): Argument #1 ($keys) and argument #2 ($values) must have the same number of elements");
        }
        const out = makeArr();
        const kv = Array.from(ka.m.values()), vv = Array.from(va.m.values());
        for (let i = 0; i < kv.length; i++) arrSetInPlace(out, kv[i], vv[i]);
        return out;
    });
    reg("array_column", function (arr, colKey, idxKey) {
        const a = asArr(arr, "array_column");
        const ck = rt.deref(colKey);
        const ik = idxKey !== undefined ? rt.deref(idxKey) : null;
        const out = makeArr();
        for (const row of a.m.values()) {
            const r = rt.deref(row);
            let val;
            if (ck === null) val = r;
            else if (r && r.__php === 1) {
                const k = arrKey(ck);
                if (!r.m.has(k)) continue;
                val = rt.deref(r.m.get(k));
            } else if (r && r.__php === 2) {
                val = R.propGet(r, rt.toStr(ck), false, null, true);
            } else continue;
            if (ik !== null) {
                let k2 = null;
                if (r && r.__php === 1 && r.m.has(arrKey(ik))) k2 = rt.deref(r.m.get(arrKey(ik)));
                else if (r && r.__php === 2) k2 = R.propGet(r, rt.toStr(ik), false, null, true);
                if (k2 !== null) { arrSetInPlace(out, k2, val); continue; }
            }
            arrPushInPlace(out, val);
        }
        return out;
    });
    reg("array_diff", function (first) {
        const a = asArr(first, "array_diff");
        const excl = new Set();
        for (let i = 1; i < arguments.length; i++) {
            const b = asArr(arguments[i], "array_diff");
            for (const v of b.m.values()) excl.add(rt.toStr(v));
        }
        const out = makeArr();
        for (const [k, v] of a.m) if (!excl.has(rt.toStr(v))) arrSetInPlace(out, k, v);
        return out;
    });
    reg("array_diff_key", function (first) {
        const a = asArr(first, "array_diff_key");
        const out = makeArr();
        for (const [k, v] of a.m) {
            let found = false;
            for (let i = 1; i < arguments.length; i++) {
                if (asArr(arguments[i], "array_diff_key").m.has(k)) { found = true; break; }
            }
            if (!found) arrSetInPlace(out, k, v);
        }
        return out;
    });
    reg("array_intersect", function (first) {
        const a = asArr(first, "array_intersect");
        const sets = [];
        for (let i = 1; i < arguments.length; i++) {
            const s = new Set();
            for (const v of asArr(arguments[i], "array_intersect").m.values()) s.add(rt.toStr(v));
            sets.push(s);
        }
        const out = makeArr();
        for (const [k, v] of a.m) {
            const sv = rt.toStr(v);
            if (sets.every(s => s.has(sv))) arrSetInPlace(out, k, v);
        }
        return out;
    });
    reg("array_intersect_key", function (first) {
        const a = asArr(first, "array_intersect_key");
        const out = makeArr();
        for (const [k, v] of a.m) {
            let ok = true;
            for (let i = 1; i < arguments.length; i++) {
                if (!asArr(arguments[i], "array_intersect_key").m.has(k)) { ok = false; break; }
            }
            if (ok) arrSetInPlace(out, k, v);
        }
        return out;
    });
    reg("array_fill", function (start, count, val) {
        let s = rt.toInt(start);
        const c = Number(rt.toInt(count));
        if (c < 0) rt.throwError("ValueError", "array_fill(): Argument #2 ($count) must be greater than or equal to 0");
        const out = makeArr();
        if (c === 0) return out;
        arrSetInPlace(out, s, val);
        if (s < 0n) s = 0n; else s++;
        for (let i = 1; i < c; i++, s++) arrSetInPlace(out, s, val);
        return out;
    });
    reg("array_fill_keys", function (keys, val) {
        const ka = asArr(keys, "array_fill_keys");
        const out = makeArr();
        for (const k of ka.m.values()) arrSetInPlace(out, k, val);
        return out;
    });
    reg("array_pad", function (arr, size, val) {
        const a = asArr(arr, "array_pad");
        const sz = Number(rt.toInt(size));
        const absSz = Math.abs(sz);
        if (absSz <= a.m.size) return cloneArr(a);
        const padCount = absSz - a.m.size;
        const out = makeArr();
        if (sz < 0) {
            for (let i = 0; i < padCount; i++) arrPushInPlace(out, val);
            for (const [k, v] of a.m) { if (typeof k === "string") arrSetInPlace(out, k, v); else arrPushInPlace(out, v); }
        } else {
            for (const [k, v] of a.m) { if (typeof k === "string") arrSetInPlace(out, k, v); else arrPushInPlace(out, v); }
            for (let i = 0; i < padCount; i++) arrPushInPlace(out, val);
        }
        return out;
    });
    reg("array_sum", function (arr) {
        const a = asArr(arr, "array_sum");
        let acc = 0n;
        for (const v of a.m.values()) acc = R.add(acc, v);
        return acc;
    });
    reg("array_product", function (arr) {
        const a = asArr(arr, "array_product");
        let acc = 1n;
        for (const v of a.m.values()) acc = R.mul(acc, v);
        return acc;
    });
    reg("array_any", function (arr, cb) {
        const a = asArr(arr, "array_any");
        for (const [k, v] of a.m) if (rt.truthy(rt.call(cb, [rt.deref(v), k], null, null, false))) return true;
        return false;
    });
    reg("array_all", function (arr, cb) {
        const a = asArr(arr, "array_all");
        for (const [k, v] of a.m) if (!rt.truthy(rt.call(cb, [rt.deref(v), k], null, null, false))) return false;
        return true;
    });
    reg("array_find", function (arr, cb) {
        const a = asArr(arr, "array_find");
        for (const [k, v] of a.m) if (rt.truthy(rt.call(cb, [rt.deref(v), k], null, null, false))) return rt.deref(v);
        return null;
    });
    reg("range", function (start, end, step) {
        start = rt.deref(start); end = rt.deref(end);
        const out = makeArr();
        if (typeof start === "string" && typeof end === "string" && start.length === 1 && end.length === 1 && !/^\d$/.test(start) && !/^\d$/.test(end)) {
            const s = start.charCodeAt(0), e = end.charCodeAt(0);
            const st = step !== undefined ? Math.abs(Number(rt.toInt(step))) : 1;
            if (s <= e) for (let c = s; c <= e; c += st) arrPushInPlace(out, String.fromCharCode(c));
            else for (let c = s; c >= e; c -= st) arrPushInPlace(out, String.fromCharCode(c));
            return out;
        }
        const st = step !== undefined ? rt.deref(step) : 1n;
        const isFloat = typeof start === "number" || typeof end === "number" || typeof st === "number";
        if (!isFloat) {
            const s = rt.toInt(start), e = rt.toInt(end);
            let d = rt.toInt(st);
            if (d === 0n) rt.throwError("ValueError", "range(): Argument #3 ($step) cannot be 0");
            if (d < 0n) d = -d;
            if (s <= e) for (let x = s; x <= e; x += d) arrPushInPlace(out, x);
            else for (let x = s; x >= e; x -= d) arrPushInPlace(out, x);
        } else {
            const s = rt.toFloat(start), e = rt.toFloat(end);
            const d = Math.abs(rt.toFloat(st));
            if (d === 0) rt.throwError("ValueError", "range(): Argument #3 ($step) cannot be 0");
            if (s <= e) for (let x = s; x <= e + 1e-12; x += d) arrPushInPlace(out, x);
            else for (let x = s; x >= e - 1e-12; x -= d) arrPushInPlace(out, x);
        }
        return out;
    });

    // ---- Sorting (sort, rsort, asort, arsort, ksort, krsort, usort, uasort, uksort) ----
    function sortHelper(arrRef, cmpFn, keepKeys, byKey, reverse) {
        const a = asArr(arrRef, "sort");
        const entries = Array.from(a.m.entries());
        entries.sort(function (ea, eb) {
            const x = byKey ? ea[0] : rt.deref(ea[1]);
            const y = byKey ? eb[0] : rt.deref(eb[1]);
            let r;
            if (cmpFn) {
                r = Number(rt.toInt(rt.call(cmpFn, [x, y], null, null, false)));
            } else {
                const c = rt.phpCmp(x, y);
                r = Number.isNaN(c) ? 0 : Number(c);
            }
            return reverse ? -r : r;
        });
        const out = makeArr();
        for (const [k, v] of entries) {
            if (keepKeys) arrSetInPlace(out, k, v);
            else arrPushInPlace(out, v);
        }
        if (rt.isRef(arrRef)) arrRef.v = out;
        return true;
    }
    reg("sort", function (a) { return sortHelper(a, null, false, false, false); }, 1);
    reg("rsort", function (a) { return sortHelper(a, null, false, false, true); }, 1);
    reg("asort", function (a) { return sortHelper(a, null, true, false, false); }, 1);
    reg("arsort", function (a) { return sortHelper(a, null, true, false, true); }, 1);
    reg("ksort", function (a) { return sortHelper(a, null, true, true, false); }, 1);
    reg("krsort", function (a) { return sortHelper(a, null, true, true, true); }, 1);
    reg("usort", function (a, cb) { return sortHelper(a, cb, false, false, false); }, 1);
    reg("uasort", function (a, cb) { return sortHelper(a, cb, true, false, false); }, 1);
    reg("uksort", function (a, cb) { return sortHelper(a, cb, true, true, false); }, 1);

    // ---- Internal array cursor ----
    function cursorEntries(a) {
        if (a._e === null || a._e === undefined) a._e = Array.from(a.m.entries());
        return a._e;
    }
    reg("reset", function (arrRef) {
        const a = cow(asArr(arrRef, "reset"));
        a.pos = 0;
        if (rt.isRef(arrRef)) arrRef.v = a;
        const e = cursorEntries(a);
        return e.length > 0 ? rt.deref(e[0][1]) : false;
    }, 1);
    reg("end", function (arrRef) {
        const a = cow(asArr(arrRef, "end"));
        a.pos = Math.max(0, a.m.size - 1);
        if (rt.isRef(arrRef)) arrRef.v = a;
        const e = cursorEntries(a);
        return e.length > 0 ? rt.deref(e[e.length - 1][1]) : false;
    }, 1);
    reg("current", function (arr) {
        const a = asArr(arr, "current");
        const e = cursorEntries(a);
        return (a.pos >= 0 && a.pos < e.length) ? rt.deref(e[a.pos][1]) : false;
    });
    reg("key", function (arr) {
        const a = asArr(arr, "key");
        const e = cursorEntries(a);
        return (a.pos >= 0 && a.pos < e.length) ? e[a.pos][0] : null;
    });
    reg("next", function (arrRef) {
        const a = cow(asArr(arrRef, "next"));
        a.pos++;
        if (rt.isRef(arrRef)) arrRef.v = a;
        const e = cursorEntries(a);
        return (a.pos >= 0 && a.pos < e.length) ? rt.deref(e[a.pos][1]) : false;
    }, 1);
    reg("prev", function (arrRef) {
        const a = cow(asArr(arrRef, "prev"));
        a.pos--;
        if (rt.isRef(arrRef)) arrRef.v = a;
        const e = cursorEntries(a);
        return (a.pos >= 0 && a.pos < e.length) ? rt.deref(e[a.pos][1]) : false;
    }, 1);

    // ---- Scope-aware compact / extract / get_defined_vars helpers ----
    R.compact = function (scope, names) {
        const out = makeArr();
        function add(x) {
            x = rt.deref(x);
            if (x && x.__php === 1) {
                for (const v of x.m.values()) add(v);
            } else {
                const n = rt.toStr(x);
                if (scope.has(n)) arrSetInPlace(out, n, scope.get(n).v);
            }
        }
        for (let i = 0; i < names.length; i++) add(names[i]);
        return out;
    };
    R.extract = function (scope, arr) {
        const a = asArr(arr, "extract");
        let count = 0n;
        for (const [k, v] of a.m) {
            if (typeof k === "string" && /^[a-zA-Z_\x80-\xff][a-zA-Z0-9_\x80-\xff]*$/.test(k)) {
                R.sset(scope, k, v);
                count++;
            }
        }
        return count;
    };
    R.getDefinedVars = function (scope) {
        const out = makeArr();
        for (const [k, cell] of scope) arrSetInPlace(out, k, cell.v);
        return out;
    };
})(__zipp_php);
