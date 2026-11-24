/* ZIPP Rust runtime — core value model, formatting, equality, ordering, cloning,
 * arithmetic, references, pattern helpers, and type/trait dispatch. Apache-2.0.
 */
var __zipp_rs = (function () {
    "use strict";
    const R = {};
    const rt = {};
    R.__rt = rt;

    const ITER_DONE = { __rs_done: true };
    R.ITER_DONE = ITER_DONE;

    // ---- Panic & Exit sentinels ----
    function RsPanic(msg) {
        this.isRsPanic = true;
        this.message = String(msg);
    }
    function RsExit(code) {
        this.isRsExit = true;
        this.code = code | 0;
    }
    rt.RsPanic = RsPanic;
    rt.RsExit = RsExit;

    function rsPanic(msg) {
        throw new RsPanic(msg);
    }
    R.panic = rsPanic;
    R.exit = function (code) {
        throw new RsExit(code === undefined ? 0 : (Number(deref(code)) | 0));
    };

    // ---- String / UTF-8 byte helpers ----
    function strVal(v) {
        v = deref(v);
        if (typeof v === "string") return v;
        if (v && typeof v === "object") {
            if (v.__rs === 4) return v.s;
            if (v.__rs === 5) return v.ch;
            if (v.__rs === 2 && v.__enum === "Method") return v.__var;
            if (v.__rs === 15) {
                if (v.kind === "path") return v.p;
                if (v.kind === "uri") return v.raw;
                if (v.kind === "header_value") return v.v;
                if (v.kind === "header_name") return v.n;
                if (v.kind === "uuid" || v.kind === "argon2_hash") return v.s;
                if (v.kind === "ip_addr") return v.ip;
                if (v.kind === "socket_addr") return v.ip.indexOf(":") >= 0 ? ("[" + v.ip + "]:" + v.port) : (v.ip + ":" + v.port);
                if (v.kind === "digest_bytes") return v.hex;
                if (v.kind === "anyhow_error" || v.kind === "io_error" || v.kind === "sqlx_db_error") return v.msg;
            }
        }
        return String(v);
    }
    rt.strVal = strVal;

    function mkString(s) {
        return { __rs: 4, s: typeof s === "string" ? s : strVal(s) };
    }
    R.mkString = mkString;

    function mkChar(c) {
        if (typeof c === "number") {
            const ch = String.fromCodePoint(c);
            return { __rs: 5, ch: ch, cp: c };
        }
        const s = String(c);
        return { __rs: 5, ch: s, cp: s.codePointAt(0) || 0 };
    }
    R.mkChar = mkChar;

    function mkDeque(init) {
        const n = init ? init.length : 0;
        let cap = 8;
        while (cap < n) cap <<= 1;
        const buf = new Array(cap);
        for (let i = 0; i < n; i++) buf[i] = init[i];
        return { __rs: 12, buf: buf, head: 0, len: n };
    }
    function dequeToArray(d) {
        const n = d.len, b = d.buf, m = b.length - 1, h = d.head;
        const out = new Array(n);
        for (let i = 0; i < n; i++) out[i] = b[(h + i) & m];
        return out;
    }
    rt.mkDeque = mkDeque;
    rt.dequeToArray = dequeToArray;

    function utf8ByteLen(s) {
        const n = s.length;
        let len = n;
        for (let i = 0; i < n; i++) {
            const c = s.charCodeAt(i);
            if (c >= 0x80) {
                if (c < 0x800) len += 1;
                else if (c >= 0xD800 && c <= 0xDBFF) { len += 2; i++; }
                else len += 2;
            }
        }
        return len;
    }
    rt.utf8ByteLen = utf8ByteLen;

    function strToUtf8Bytes(s) {
        const out = [];
        for (let i = 0; i < s.length; i++) {
            let cp = s.codePointAt(i);
            if (cp > 0xFFFF) i++;
            if (cp < 0x80) out.push(cp);
            else if (cp < 0x800) {
                out.push(0xC0 | (cp >> 6), 0x80 | (cp & 0x3F));
            } else if (cp < 0x10000) {
                out.push(0xE0 | (cp >> 12), 0x80 | ((cp >> 6) & 0x3F), 0x80 | (cp & 0x3F));
            } else {
                out.push(
                    0xF0 | (cp >> 18),
                    0x80 | ((cp >> 12) & 0x3F),
                    0x80 | ((cp >> 6) & 0x3F),
                    0x80 | (cp & 0x3F)
                );
            }
        }
        return out;
    }
    rt.strToUtf8Bytes = strToUtf8Bytes;

    function utf8BytesToStr(bytes) {
        let out = "";
        let i = 0;
        const n = bytes.length;
        while (i < n) {
            const b0 = bytes[i++] & 0xFF;
            if (b0 < 0x80) {
                out += String.fromCharCode(b0);
            } else if ((b0 & 0xE0) === 0xC0 && i < n) {
                const b1 = bytes[i++] & 0x3F;
                out += String.fromCharCode(((b0 & 0x1F) << 6) | b1);
            } else if ((b0 & 0xF0) === 0xE0 && i + 1 < n) {
                const b1 = bytes[i++] & 0x3F;
                const b2 = bytes[i++] & 0x3F;
                out += String.fromCharCode(((b0 & 0x0F) << 12) | (b1 << 6) | b2);
            } else if ((b0 & 0xF8) === 0xF0 && i + 2 < n) {
                const b1 = bytes[i++] & 0x3F;
                const b2 = bytes[i++] & 0x3F;
                const b3 = bytes[i++] & 0x3F;
                const cp = ((b0 & 0x07) << 18) | (b1 << 12) | (b2 << 6) | b3;
                out += (cp <= 0x10FFFF && (cp < 0xD800 || cp > 0xDFFF)) ? String.fromCodePoint(cp) : "\uFFFD";
            } else {
                out += "\uFFFD";
            }
        }
        return out;
    }
    rt.utf8BytesToStr = utf8BytesToStr;

    // ---- References & Smart-Pointer Auto-Deref ----
    function isRef(v) {
        return v !== null && typeof v === "object" && v.__rs === 6;
    }

    function derefOne(v) {
        if (v !== null && typeof v === "object") {
            if (v.__rs === 6) return v.get ? v.get() : v.v;
            if (v.__rs === 14 && (v.kind === "box" || v.kind === "rc" || v.kind === "arc" || v.kind === "guard")) {
                return v.get ? v.get() : v.v;
            }
        }
        return v;
    }

    function deref(v) {
        let guard = 0;
        while (v !== null && typeof v === "object" && guard++ < 16) {
            if (v.__rs === 6) {
                v = v.get ? v.get() : v.v;
            } else if (v.__rs === 14 && (v.kind === "box" || v.kind === "rc" || v.kind === "arc" || v.kind === "guard")) {
                v = v.get ? v.get() : v.v;
            } else {
                break;
            }
        }
        return v;
    }
    rt.deref = deref;
    rt.derefOne = derefOne;
    R.deref = deref;
    R.derefOne = derefOne;

    R.mkRef = function (v) {
        return { __rs: 6, v: v };
    };
    R.mkPropRef = function (obj, prop) {
        const target = deref(obj);
        return {
            __rs: 6,
            get: function () { return target[prop]; },
            set: function (val) { target[prop] = val; }
        };
    };
    R.mkTupleIdxRef = function (tup, idx) {
        const target = deref(tup);
        return {
            __rs: 6,
            get: function () { return target.__items ? target.__items[idx] : target[idx]; },
            set: function (val) {
                if (target.__items) target.__items[idx] = val;
                else target[idx] = val;
            }
        };
    };
    R.mkIdxRef = function (coll, idx) {
        const target = deref(coll);
        const k = deref(idx);
        if (target && target.__rs === 7) {
            const i = Number(k);
            return {
                __rs: 6,
                get: function () { return target.items[i]; },
                set: function (val) { target.items[i] = val; }
            };
        }
        if (target && target.__rs === 10) {
            return {
                __rs: 6,
                get: function () { return rt.mapGetRaw(target, k); },
                set: function (val) { rt.mapSetRaw(target, k, val); }
            };
        }
        return {
            __rs: 6,
            get: function () { return target[k]; },
            set: function (val) { target[k] = val; }
        };
    };
    R.rget = function (ref) {
        if (ref && typeof ref === "object" && ref.__rs === 6) {
            return ref.get ? ref.get() : ref.v;
        }
        if (ref && typeof ref === "object" && ref.__rs === 14) {
            return ref.get ? ref.get() : ref.v;
        }
        return ref;
    };
    R.rset = function (ref, val) {
        if (ref && typeof ref === "object" && ref.__rs === 6) {
            if (ref.set) ref.set(val);
            else ref.v = val;
            return undefined;
        }
        if (ref && typeof ref === "object" && ref.__rs === 14) {
            if (ref.set) ref.set(val);
            else ref.v = val;
            return undefined;
        }
        rsPanic("cannot assign through non-reference");
    };

    // ---- Core constructors: Struct, Enum, Tuple, Vec, Range, Option, Result ----
    R.mkStruct = function (typeName, fieldsObj, restObj) {
        const out = { __rs: 1, __type: typeName, __tuple: false };
        if (restObj !== undefined && restObj !== null) {
            const base = deref(restObj);
            if (base && typeof base === "object") {
                const keys = Object.keys(base);
                for (let i = 0; i < keys.length; i++) {
                    const k = keys[i];
                    if (k !== "__rs" && k !== "__type" && k !== "__tuple" && k !== "__len") {
                        out[k] = base[k];
                    }
                }
            }
        }
        if (fieldsObj) {
            const keys = Object.keys(fieldsObj);
            for (let i = 0; i < keys.length; i++) {
                const k = keys[i];
                out[k] = fieldsObj[k];
            }
        }
        const stMeta = structs[typeName];
        if (stMeta && stMeta.fields) {
            for (let i = 0; i < stMeta.fields.length; i++) {
                const fn = stMeta.fields[i][0];
                const ft = stMeta.fields[i][1];
                if (ft && (ft.indexOf("HashMap<") >= 0 || ft.indexOf("BTreeMap<") >= 0)) {
                    let cur = out[fn];
                    let guard = 0;
                    while (cur && typeof cur === "object" && (cur.__rs === 14 || cur.__rs === 6) && guard++ < 8) {
                        cur = cur.get ? cur.get() : cur.v;
                    }
                    if (cur && cur.__rs === 10 && !cur.valType) {
                        const mIdx = Math.max(ft.indexOf("HashMap<"), ft.indexOf("BTreeMap<"));
                        const sub = ft.slice(mIdx);
                        const lt = sub.indexOf("<");
                        if (lt >= 0) {
                            let depth = 0, comma = -1, end = sub.length;
                            for (let j = lt + 1; j < sub.length; j++) {
                                const ch = sub[j];
                                if (ch === "<") depth++;
                                else if (ch === ">") { if (depth === 0) { end = j; break; } depth--; }
                                else if (ch === "," && depth === 0 && comma < 0) comma = j;
                            }
                            if (comma >= 0) {
                                const vt = sub.slice(comma + 1, end).trim().split("<")[0].trim();
                                if (vt) cur.valType = vt;
                            }
                        }
                    }
                }
            }
        }
        return out;
    };

    R.mkTupleStruct = function (typeName, items) {
        const out = { __rs: 1, __type: typeName, __tuple: true, __len: items.length };
        for (let i = 0; i < items.length; i++) out[i] = items[i];
        return out;
    };

    R.mkEnumUnit = function (enumName, varName, disc) {
        return { __rs: 2, __enum: enumName, __var: varName, __kind: 0, __disc: disc === undefined ? null : disc };
    };
    R.mkEnumTuple = function (enumName, varName, items) {
        return { __rs: 2, __enum: enumName, __var: varName, __kind: 1, __data: items };
    };
    R.mkEnumStruct = function (enumName, varName, fieldsObj) {
        return { __rs: 2, __enum: enumName, __var: varName, __kind: 2, __data: fieldsObj };
    };

    R.Some = function (v) {
        return { __rs: 2, __enum: "Option", __var: "Some", __kind: 1, __data: [v] };
    };
    const NONE = { __rs: 2, __enum: "Option", __var: "None", __kind: 0, __disc: null };
    R.None = NONE;
    R.Ok = function (v) {
        return { __rs: 2, __enum: "Result", __var: "Ok", __kind: 1, __data: [v] };
    };
    R.Err = function (e) {
        return { __rs: 2, __enum: "Result", __var: "Err", __kind: 1, __data: [e] };
    };
    rt.Some = R.Some;
    rt.None = NONE;
    rt.Ok = R.Ok;
    rt.Err = R.Err;

    R.mkTuple = function (items) {
        return { __rs: 3, __items: items };
    };
    R.mkVec = function (items) {
        return { __rs: 7, items: items };
    };
    R.mkRepeat = function (elem, count) {
        const n = Number(deref(count)) | 0;
        if (n < 0) rsPanic("negative array repeat length");
        const arr = new Array(n);
        for (let i = 0; i < n; i++) arr[i] = cloneVal(elem);
        return { __rs: 7, items: arr };
    };
    R.mkRange = function (start, end, inclusive) {
        return {
            __rs: 9,
            start: start === undefined ? null : deref(start),
            end: end === undefined ? null : deref(end),
            inclusive: !!inclusive
        };
    };

    // ---- Async bridge (`block_on`) via VM microtask checkpoint ----
    R.blockOn = function (val) {
        if (!val || typeof val.then !== "function") return val;
        let done = false;
        let out = undefined;
        let hasErr = false;
        let err = undefined;
        val.then(
            function (v) { done = true; out = v; },
            function (e) { done = true; hasErr = true; err = e; }
        );
        if (typeof __zipp_rs_io === "function") {
            let spins = 0;
            while (!done && spins++ < 4096) {
                if (rt.__inWorkerBoot && rt.__workerBootReady) break;
                if (typeof rt.pollTimers === "function") rt.pollTimers();
                __zipp_rs_io("drain_microtasks", "");
                if (done || (rt.__inWorkerBoot && rt.__workerBootReady)) break;
                if (typeof rt.nextTimerMs === "function") {
                    const wait = rt.nextTimerMs();
                    if (wait !== null && wait <= 5000 && typeof __zipp_rs_time === "function") {
                        if (wait > 0) __zipp_rs_time("sleep_ms", Math.min(wait, 50));
                        rt.pollTimers();
                        __zipp_rs_io("drain_microtasks", "");
                    }
                }
            }
        }
        if (hasErr) throw err;
        return out;
    };

    R.selectRace = function (branches) {
        if (!branches || branches.length === 0) return Promise.resolve(undefined);
        const isCallbackForm = Array.isArray(branches[0]) && branches[0].length === 2 && typeof branches[0][1] === "function";
        const futs = branches.map(function (b) {
            return deref(isCallbackForm ? b[0] : b);
        });
        return new Promise(function (resolve, reject) {
            let settled = false;
            function cancelOthers(winIdx) {
                for (let i = 0; i < futs.length; i++) {
                    if (i === winIdx) continue;
                    const f = futs[i];
                    if (f && typeof f.__cancel === "function") {
                        try { f.__cancel(); } catch (_) {}
                    }
                }
            }
            for (let idx = 0; idx < futs.length; idx++) {
                (function (i, f) {
                    Promise.resolve(f).then(
                        function (val) {
                            if (settled) return;
                            settled = true;
                            cancelOthers(i);
                            if (isCallbackForm) {
                                try { resolve(branches[i][1](val)); } catch (e) { reject(e); }
                            } else {
                                resolve([i, val]);
                            }
                        },
                        function (err) {
                            if (settled) return;
                            settled = true;
                            cancelOthers(i);
                            reject(err);
                        }
                    );
                })(idx, futs[idx]);
            }
        });
    };

    // ---- Dynamic callable invocation (auto-derefs Box<dyn Fn> / Arc<dyn Fn> / refs) ----
    R.callFn = function (callee, args) {
        const fn = deref(callee);
        if (typeof fn !== "function") {
            rsPanic("attempted to call non-function value: " + fmtDebug(fn));
        }
        return fn.apply(null, args || []);
    };

    // ---- Closure binding ----
    R.mkClosure = function (fn, env) {
        return function () {
            switch (arguments.length) {
                case 0: return fn.call(env);
                case 1: return fn.call(env, arguments[0]);
                case 2: return fn.call(env, arguments[0], arguments[1]);
                case 3: return fn.call(env, arguments[0], arguments[1], arguments[2]);
                case 4: return fn.call(env, arguments[0], arguments[1], arguments[2], arguments[3]);
                default: return fn.apply(env, arguments);
            }
        };
    };

    // ---- `?` Try operator helpers (with automatic `From` error conversion!) ----
    R.tryOk = function (v) {
        v = deref(v);
        if (v && v.__rs === 2) {
            if (v.__var === "Ok" || v.__var === "Some") return true;
            if (v.__var === "Err" || v.__var === "None") return false;
        }
        rsPanic("`?` operator applied to non-Result/Option value: " + fmtDebug(v));
    };
    R.tryErr = function (v, targetErrType) {
        v = deref(v);
        if (targetErrType && v && v.__rs === 2 && v.__var === "Err") {
            const inner = v.__data[0];
            const srcTag = typeTagOf(deref(inner));
            if (srcTag !== targetErrType) {
                const conv = tryConvertFrom(targetErrType, inner);
                if (conv !== CONVERT_MISS) return R.Err(conv);
            }
        }
        return v;
    };
    R.tryUnwrap = function (v) {
        v = deref(v);
        return v.__data ? v.__data[0] : undefined;
    };

    // ---- Field, Tuple Index, and Indexing helpers ----
    R.getField = function (obj, field) {
        const v = deref(obj);
        if (v === null || v === undefined) rsPanic("field access `" + field + "` on unit/null");
        if (v.__rs === 1) return v[field];
        if (v.__rs === 15) {
            if (field in v) return v[field];
            if (v.kind === "json" && v.v && typeof v.v === "object") {
                return rt.mkJson(v.v[field]);
            }
        }
        return v[field];
    };
    R.setField = function (obj, field, val) {
        const v = deref(obj);
        if (v === null || v === undefined) rsPanic("field write `" + field + "` on unit/null");
        v[field] = val;
        return undefined;
    };
    R.getTupleIdx = function (obj, idx) {
        const v = deref(obj);
        if (v && v.__rs === 3) return v.__items[idx];
        if (v && v.__rs === 1 && v.__tuple) return v[idx];
        if (v && v.__rs === 2 && v.__kind === 1) return v.__data[idx];
        if (Array.isArray(v)) return v[idx];
        return v[idx];
    };
    R.setTupleIdx = function (obj, idx, val) {
        const v = deref(obj);
        if (v && v.__rs === 3) { v.__items[idx] = val; return undefined; }
        if (v && v.__rs === 1 && v.__tuple) { v[idx] = val; return undefined; }
        v[idx] = val;
        return undefined;
    };
    R.index = function (recv, idx) {
        const v = deref(recv);
        const k = deref(idx);
        if (k && typeof k === "object" && k.__rs === 9) {
            // Slicing with a Range!
            if (v && v.__rs === 7) {
                const len = v.items.length;
                const s = k.start === null ? 0 : Number(k.start);
                const e = k.end === null ? len : (k.inclusive ? Number(k.end) + 1 : Number(k.end));
                if (s < 0 || e > len || s > e) rsPanic("slice range " + s + ".." + e + " out of bounds for len " + len);
                return { __rs: 7, items: v.items.slice(s, e) };
            }
            if (typeof v === "string" || (v && v.__rs === 4)) {
                const s = typeof v === "string" ? v : v.s;
                const bytes = strToUtf8Bytes(s);
                const len = bytes.length;
                const st = k.start === null ? 0 : Number(k.start);
                const en = k.end === null ? len : (k.inclusive ? Number(k.end) + 1 : Number(k.end));
                if (st < 0 || en > len || st > en) rsPanic("string slice range " + st + ".." + en + " out of bounds for len " + len);
                return utf8BytesToStr(bytes.slice(st, en));
            }
        }
        if (v && v.__rs === 7) {
            const i = Number(k);
            if (i < 0 || i >= v.items.length) rsPanic("index out of bounds: the len is " + v.items.length + " but the index is " + i);
            return v.items[i];
        }
        if (v && v.__rs === 12) {
            const i = Number(k);
            if (i < 0 || i >= v.len) rsPanic("VecDeque index out of bounds: " + i);
            return v.buf[(v.head + i) & (v.buf.length - 1)];
        }
        if (v && v.__rs === 10) {
            const got = rt.mapGetOpt(v, k);
            if (got.__var === "None") rsPanic("no entry found for key: " + fmtDebug(k));
            return got.__data[0];
        }
        if (v && v.__rs === 15 && v.kind === "json") {
            const raw = v.v;
            if (Array.isArray(raw)) {
                const i = Number(k);
                return rt.mkJson(i >= 0 && i < raw.length ? raw[i] : null);
            }
            if (raw && typeof raw === "object") {
                const sk = strVal(k);
                return rt.mkJson(Object.prototype.hasOwnProperty.call(raw, sk) ? raw[sk] : null);
            }
            return rt.mkJson(null);
        }
        if (typeof v === "string" || (v && v.__rs === 4)) {
            const s = typeof v === "string" ? v : v.s;
            const i = Number(k);
            return s.charCodeAt(i);
        }
        if (Array.isArray(v)) return v[Number(k)];
        return v[k];
    };
    R.setIndex = function (recv, idx, val) {
        const v = deref(recv);
        const k = deref(idx);
        if (v && v.__rs === 7) {
            const i = Number(k);
            if (i < 0 || i >= v.items.length) rsPanic("index out of bounds: the len is " + v.items.length + " but the index is " + i);
            v.items[i] = val;
            return undefined;
        }
        if (v && v.__rs === 12) {
            const i = Number(k);
            if (i < 0 || i >= v.len) rsPanic("VecDeque index out of bounds: " + i);
            v.buf[(v.head + i) & (v.buf.length - 1)] = val;
            return undefined;
        }
        if (v && v.__rs === 10) {
            rt.mapSetRaw(v, k, val);
            return undefined;
        }
        if (v && v.__rs === 15 && v.kind === "json" && v.v && typeof v.v === "object") {
            const key = Array.isArray(v.v) ? Number(k) : strVal(k);
            v.v[key] = rt.toJsonRaw(val);
            return undefined;
        }
        v[k] = val;
        return undefined;
    };

    // ---- Type, Trait, Method, and Module registries ----
    const structs = Object.create(null);
    const enums = Object.create(null);
    const methods = Object.create(null);       // typeName -> { methodName -> fn }
    const traitImpls = Object.create(null);    // traitName -> { typeName -> { methodName -> fn } }
    const traitDefaults = Object.create(null); // traitName -> { methodName -> fn }
    const typeTraits = Object.create(null);    // typeName -> [traitName, ...]
    const fromImpls = Object.create(null);     // dstType -> [{ src, fn }, ...]
    const tryFromImpls = Object.create(null);  // dstType -> [{ src, fn }, ...]
    const consts = Object.create(null);        // qualifiedName -> val
    const localStatics = Object.create(null);  // uniqueKey -> val
    const tests = [];                          // [{ name, fn, shouldPanic, ignore }]
    let activeRouter = null;
    const CONVERT_MISS = { __rs_conv_miss: true };

    rt.structs = structs;
    rt.enums = enums;
    rt.methods = methods;
    rt.traitImpls = traitImpls;
    rt.traitDefaults = traitDefaults;
    rt.consts = consts;
    rt.tests = tests;
    rt.getRouter = function () { return activeRouter; };
    rt.setRouter = function (r) { activeRouter = r; };

    R.hasStatic = function (k) { return Object.prototype.hasOwnProperty.call(localStatics, k); };
    R.getStatic = function (k) { return localStatics[k]; };
    R.setStatic = function (k, v) { localStatics[k] = v; };

    R.isErr = function (v) {
        v = deref(v);
        return !!(v && v.__rs === 2 && v.__enum === "Result" && v.__var === "Err");
    };
    R.unwrapOk = function (v) {
        v = deref(v);
        return (v && v.__rs === 2 && v.__enum === "Result" && v.__var === "Ok") ? v.__data[0] : v;
    };
    R.regStruct = function (name, fields, isTuple, derives, serdeMeta) {
        const shortName = name.indexOf("::") >= 0 ? name.slice(name.lastIndexOf("::") + 2) : name;
        const info = {
            name: name,
            shortName: shortName,
            fields: fields,
            isTuple: !!isTuple,
            derives: derives || [],
            serde: serdeMeta || null
        };
        structs[name] = info;
        if (shortName !== name) structs[shortName] = info;
    };
    R.setSerdeDefaultFn = function (structName, fieldName, fn) {
        const st = structs[structName];
        if (!st) return;
        if (!st.serde) st.serde = { fields: {} };
        if (!st.serde.fields) st.serde.fields = {};
        if (!st.serde.fields[fieldName]) st.serde.fields[fieldName] = {};
        st.serde.fields[fieldName].default_fn = fn;
    };
    R.regEnum = function (name, variants, derives, serdeMeta) {
        enums[name] = {
            name: name,
            variants: variants,
            derives: derives || [],
            serde: serdeMeta || null
        };
    };
    R.regEnumFrom = function (enumName, varName, srcPath) {
        R.regFrom(enumName, srcPath, function (v) {
            return R.mkEnumTuple(enumName, varName, [v]);
        });
    };
    R.regMethod = function (typeName, methodName, fn, mutSelf) {
        if (mutSelf) fn.__rs_mut_self = true;
        if (!methods[typeName]) methods[typeName] = Object.create(null);
        methods[typeName][methodName] = fn;
    };
    R.regTraitDefault = function (traitName, methodName, fn, mutSelf) {
        if (mutSelf) fn.__rs_mut_self = true;
        if (!traitDefaults[traitName]) traitDefaults[traitName] = Object.create(null);
        traitDefaults[traitName][methodName] = fn;
    };
    R.regTraitImpl = function (traitName, typeName, methodName, fn, mutSelf) {
        if (mutSelf) fn.__rs_mut_self = true;
        if (!traitImpls[traitName]) traitImpls[traitName] = Object.create(null);
        if (!traitImpls[traitName][typeName]) traitImpls[traitName][typeName] = Object.create(null);
        traitImpls[traitName][typeName][methodName] = fn;
        if (!typeTraits[typeName]) typeTraits[typeName] = [];
        if (typeTraits[typeName].indexOf(traitName) < 0) typeTraits[typeName].push(traitName);
    };
    R.regFrom = function (dstType, srcType, fn) {
        if (!fromImpls[dstType]) fromImpls[dstType] = [];
        fromImpls[dstType].push({ src: srcType, fn: fn });
    };
    R.regTryFrom = function (dstType, srcType, fn) {
        if (!tryFromImpls[dstType]) tryFromImpls[dstType] = [];
        tryFromImpls[dstType].push({ src: srcType, fn: fn });
    };

    function matchesSrcType(val, srcName) {
        const v = deref(val);
        const tag = typeTagOf(v);
        if (tag === srcName) return true;
        if ((srcName === "str" || srcName === "&str" || srcName === "String") && (tag === "str" || tag === "String")) return true;
        if ((srcName === "sqlx::Error") && (v && v.__sqlx)) return true;
        if ((srcName === "anyhow::Error") && tag === "std:anyhow_error") return true;
        if ((srcName === "Error" || srcName === "io::Error" || srcName === "std::io::Error") && tag === "std:io_error") return true;
        if (srcName === "Error" && (v && v.__sqlx)) return true;
        if (srcName === "Error" && tag === "std:anyhow_error") return true;
        if (/^[iu](8|16|32|64|128|size)$|^f(32|64)$/.test(srcName) && (tag === "number" || tag === "bigint")) return true;
        if (srcName === "Value" && tag === "std:json") return true;
        return false;
    }

    function tryConvertFrom(dstType, val) {
        const shortDst = dstType && dstType.indexOf("::") >= 0 ? dstType.slice(dstType.lastIndexOf("::") + 2) : dstType;
        const list = fromImpls[dstType] || (shortDst !== dstType ? fromImpls[shortDst] : null);
        if (list) {
            for (let i = 0; i < list.length; i++) {
                if (matchesSrcType(val, list[i].src)) return list[i].fn(val);
            }
            // If converting an error to a thiserror enum with `anyhow::Error` fallback variant, convert via anyhow!
            for (let i = 0; i < list.length; i++) {
                if (list[i].src === "anyhow::Error") {
                    const ah = { __rs: 15, kind: "anyhow_error", msg: fmtDisplay(val), inner: val };
                    return list[i].fn(ah);
                }
            }
            if (list.length === 1) return list[0].fn(val);
        }
        if (dstType === "anyhow::Error") {
            const dv = deref(val);
            if (dv && dv.__rs === 15 && dv.kind === "anyhow_error") return dv;
            return { __rs: 15, kind: "anyhow_error", msg: fmtDisplay(dv), inner: dv };
        }
        if (shortDst === "String") return mkString(fmtDisplay(val));
        if (shortDst === "PathBuf") return { __rs: 15, kind: "path", p: strVal(val) };
        if (shortDst === "HeaderValue") return { __rs: 15, kind: "header_value", v: strVal(val) };
        if (shortDst === "Body") {
            const dv = deref(val);
            const s = (dv && dv.__rs === 7) ? utf8BytesToStr(dv.items) : strVal(dv);
            return { __rs: 15, kind: "body", data: s };
        }
        return CONVERT_MISS;
    }
    rt.tryConvertFrom = tryConvertFrom;
    rt.CONVERT_MISS = CONVERT_MISS;
    R.convertInto = function (val, dstType) {
        if (dstType) {
            const c = tryConvertFrom(dstType, val);
            if (c !== CONVERT_MISS) return c;
        }
        return deref(val);
    };
    rt.tryConvertTryFrom = function (dstType, val) {
        const shortDst = dstType && dstType.indexOf("::") >= 0 ? dstType.slice(dstType.lastIndexOf("::") + 2) : dstType;
        const list = tryFromImpls[dstType] || (shortDst !== dstType ? tryFromImpls[shortDst] : null);
        if (list) {
            for (let i = 0; i < list.length; i++) {
                if (matchesSrcType(val, list[i].src)) return list[i].fn(val);
            }
            if (list.length === 1) return list[0].fn(val);
        }
        return CONVERT_MISS;
    };
    R.regTraitImplEmpty = function (traitName, typeName) {
        if (!traitImpls[traitName]) traitImpls[traitName] = Object.create(null);
        if (!traitImpls[traitName][typeName]) traitImpls[traitName][typeName] = Object.create(null);
        if (!typeTraits[typeName]) typeTraits[typeName] = [];
        if (typeTraits[typeName].indexOf(traitName) < 0) typeTraits[typeName].push(traitName);
    };
    R.regConst = function (name, val) {
        consts[name] = val;
    };
    R.getConst = function (name) {
        return consts[name];
    };
    R.regTest = function (name, fn, shouldPanic, ignore) {
        tests.push({ name: name, fn: fn, shouldPanic: !!shouldPanic, ignore: !!ignore });
    };

    function typeTagOf(v) {
        if (v === undefined || v === null) return "()";
        const t = typeof v;
        if (t === "number") return "number";
        if (t === "bigint") return "bigint";
        if (t === "boolean") return "bool";
        if (t === "string") return "str";
        if (t === "function") return "fn";
        if (v.__rs === 1) return v.__type;
        if (v.__rs === 2) return v.__enum;
        if (v.__rs === 3) return "tuple";
        if (v.__rs === 4) return "String";
        if (v.__rs === 5) return "char";
        if (v.__rs === 7) return "Vec";
        if (v.__rs === 8) return "Iter";
        if (v.__rs === 9) return "Range";
        if (v.__rs === 10) return v.ordered ? "BTreeMap" : "HashMap";
        if (v.__rs === 11) return v.ordered ? "BTreeSet" : "HashSet";
        if (v.__rs === 12) return "VecDeque";
        if (v.__rs === 13) return "BinaryHeap";
        if (v.__rs === 14) return "smart:" + v.kind;
        if (v.__rs === 15) return "std:" + v.kind;
        return "object";
    }
    rt.typeTagOf = typeTagOf;

    function lookupUserMethod(typeName, methodName) {
        const m = methods[typeName];
        if (m && m[methodName]) return m[methodName];
        const ts = typeTraits[typeName];
        if (ts) {
            for (let i = 0; i < ts.length; i++) {
                const tr = ts[i];
                const impl = traitImpls[tr] && traitImpls[tr][typeName];
                if (impl && impl[methodName]) return impl[methodName];
                const def = traitDefaults[tr];
                if (def && def[methodName]) return def[methodName];
            }
        }
        return null;
    }
    rt.lookupUserMethod = lookupUserMethod;

    function lookupTraitMethod(traitName, typeName, methodName) {
        const impl = traitImpls[traitName] && traitImpls[traitName][typeName];
        if (impl && impl[methodName]) return impl[methodName];
        const def = traitDefaults[traitName];
        if (def && def[methodName]) return def[methodName];
        return null;
    }
    rt.lookupTraitMethod = lookupTraitMethod;

    // ---- Equality, Ordering, Hashing, Cloning, Default ----
    function eq(a, b) {
        a = deref(a);
        b = deref(b);
        if (a === b) return true;
        if (a === null || b === null || a === undefined || b === undefined) return a === b;
        const ta = typeof a, tb = typeof b;
        if ((ta === "number" && tb === "bigint") || (ta === "bigint" && tb === "number")) {
            return BigInt(Math.trunc(Number(a))) === BigInt(Math.trunc(Number(b))) && Number(a) == Number(b);
        }
        if ((ta === "string" || (a && (a.__rs === 4 || (a.__rs === 2 && a.__enum === "Method") || (a.__rs === 15 && (a.kind === "uri" || a.kind === "header_value" || a.kind === "header_name" || a.kind === "uuid")))))
            && (tb === "string" || (b && (b.__rs === 4 || (b.__rs === 2 && b.__enum === "Method") || (b.__rs === 15 && (b.kind === "uri" || b.kind === "header_value" || b.kind === "header_name" || b.kind === "uuid")))))) {
            return strVal(a) === strVal(b);
        }
        if (ta !== "object" || tb !== "object") return false;
        if (a.__rs !== b.__rs) return false;
        switch (a.__rs) {
            case 1: {
                if (a.__type !== b.__type) return false;
                const peq = lookupTraitMethod("PartialEq", a.__type, "eq");
                if (peq) return !!peq(a, b);
                if (a.__tuple) {
                    if (a.__len !== b.__len) return false;
                    for (let i = 0; i < a.__len; i++) if (!eq(a[i], b[i])) return false;
                    return true;
                }
                const ka = Object.keys(a), kb = Object.keys(b);
                if (ka.length !== kb.length) return false;
                for (let i = 0; i < ka.length; i++) {
                    const k = ka[i];
                    if (k === "__rs" || k === "__type" || k === "__tuple" || k === "__len") continue;
                    if (!eq(a[k], b[k])) return false;
                }
                return true;
            }
            case 2: {
                if (a.__enum !== b.__enum || a.__var !== b.__var || a.__kind !== b.__kind) return false;
                if (a.__kind === 0) return true;
                if (a.__kind === 1) {
                    if (a.__data.length !== b.__data.length) return false;
                    for (let i = 0; i < a.__data.length; i++) if (!eq(a.__data[i], b.__data[i])) return false;
                    return true;
                }
                const ka = Object.keys(a.__data), kb = Object.keys(b.__data);
                if (ka.length !== kb.length) return false;
                for (let i = 0; i < ka.length; i++) {
                    if (!eq(a.__data[ka[i]], b.__data[ka[i]])) return false;
                }
                return true;
            }
            case 3: {
                if (a.__items.length !== b.__items.length) return false;
                for (let i = 0; i < a.__items.length; i++) if (!eq(a.__items[i], b.__items[i])) return false;
                return true;
            }
            case 5: return a.cp === b.cp;
            case 7: {
                if (a.items.length !== b.items.length) return false;
                for (let i = 0; i < a.items.length; i++) if (!eq(a.items[i], b.items[i])) return false;
                return true;
            }
            case 12: {
                if (a.len !== b.len) return false;
                const ab = a.buf, am = ab.length - 1, ah = a.head;
                const bb = b.buf, bm = bb.length - 1, bh = b.head;
                for (let i = 0; i < a.len; i++) if (!eq(ab[(ah + i) & am], bb[(bh + i) & bm])) return false;
                return true;
            }
            case 9: return eq(a.start, b.start) && eq(a.end, b.end) && a.inclusive === b.inclusive;
            case 10: {
                if (a.map.size !== b.map.size) return false;
                for (const [, entry] of a.map) {
                    const got = rt.mapGetOpt(b, entry[0]);
                    if (got.__var === "None" || !eq(entry[1], got.__data[0])) return false;
                }
                return true;
            }
            case 11: {
                if (a.set.size !== b.set.size) return false;
                for (const [, val] of a.set) {
                    if (!rt.setHasRaw(b, val)) return false;
                }
                return true;
            }
            case 15: {
                if (a.kind !== b.kind) return false;
                if (a.kind === "path") return a.p === b.p;
                if (a.kind === "duration" || a.kind === "instant" || a.kind === "offset_datetime" || a.kind === "datetime") return a.ns === b.ns;
                if (a.kind === "socket_addr") return a.ip === b.ip && a.port === b.port;
                if (a.kind === "ip_addr") return a.ip === b.ip;
                if (a.kind === "uuid") return a.s === b.s;
                if (a.kind === "json") return JSON.stringify(a.v) === JSON.stringify(b.v);
                return a === b;
            }
            default:
                return a === b;
        }
    }
    R.eq = eq;
    R.ne = function (a, b) { return !eq(a, b); };
    rt.eq = eq;

    function cmpVal(a, b) {
        a = deref(a);
        b = deref(b);
        if (a === b) return 0;
        const ta = typeof a, tb = typeof b;
        if (ta === "number" && tb === "number") return a < b ? -1 : a > b ? 1 : 0;
        if (ta === "bigint" && tb === "bigint") return a < b ? -1 : a > b ? 1 : 0;
        if ((ta === "number" && tb === "bigint") || (ta === "bigint" && tb === "number")) {
            const fa = Number(a), fb = Number(b);
            return fa < fb ? -1 : fa > fb ? 1 : 0;
        }
        if (ta === "boolean" && tb === "boolean") return a === b ? 0 : (a ? 1 : -1);
        if ((ta === "string" || (a && a.__rs === 4)) && (tb === "string" || (b && b.__rs === 4))) {
            const sa = strVal(a), sb = strVal(b);
            return sa < sb ? -1 : sa > sb ? 1 : 0;
        }
        if (a && b && a.__rs === 5 && b.__rs === 5) return a.cp < b.cp ? -1 : a.cp > b.cp ? 1 : 0;
        if (a && b && a.__rs === 3 && b.__rs === 3) {
            const n = Math.min(a.__items.length, b.__items.length);
            for (let i = 0; i < n; i++) {
                const c = cmpVal(a.__items[i], b.__items[i]);
                if (c !== 0) return c;
            }
            return a.__items.length - b.__items.length;
        }
        if (a && b && a.__rs === 7 && b.__rs === 7) {
            const n = Math.min(a.items.length, b.items.length);
            for (let i = 0; i < n; i++) {
                const c = cmpVal(a.items[i], b.items[i]);
                if (c !== 0) return c;
            }
            return a.items.length - b.items.length;
        }
        if (a && b && a.__rs === 15 && b.__rs === 15 && a.kind === b.kind &&
            (a.kind === "duration" || a.kind === "instant" || a.kind === "offset_datetime" || a.kind === "datetime")) {
            return a.ns < b.ns ? -1 : a.ns > b.ns ? 1 : 0;
        }
        if (a && b && a.__rs === 1 && b.__rs === 1 && a.__type === "Reverse" && b.__type === "Reverse") {
            return cmpVal(b[0], a[0]);
        }
        if (a && b && a.__rs === 1 && b.__rs === 1 && a.__type === b.__type) {
            const ord = lookupTraitMethod("Ord", a.__type, "cmp");
            if (ord) {
                const r = ord(a, b);
                if (r && r.__rs === 2) return r.__var === "Less" ? -1 : r.__var === "Greater" ? 1 : 0;
            }
            const ks = Object.keys(a);
            for (let i = 0; i < ks.length; i++) {
                const k = ks[i];
                if (k === "__rs" || k === "__type" || k === "__tuple" || k === "__len") continue;
                const c = cmpVal(a[k], b[k]);
                if (c !== 0) return c;
            }
            return 0;
        }
        if (a && b && a.__rs === 2 && b.__rs === 2 && a.__enum === b.__enum) {
            if (a.__var !== b.__var) {
                if (a.__enum === "Option") return a.__var === "None" ? -1 : 1;
                return a.__var < b.__var ? -1 : 1;
            }
            if (a.__kind === 1) {
                for (let i = 0; i < a.__data.length; i++) {
                    const c = cmpVal(a.__data[i], b.__data[i]);
                    if (c !== 0) return c;
                }
            }
            return 0;
        }
        return 0;
    }
    rt.cmpVal = cmpVal;
    R.lt = function (a, b) { return cmpVal(a, b) < 0; };
    R.le = function (a, b) { return cmpVal(a, b) <= 0; };
    R.gt = function (a, b) { return cmpVal(a, b) > 0; };
    R.ge = function (a, b) { return cmpVal(a, b) >= 0; };

    function mkOrdering(c) {
        if (c < 0) return { __rs: 2, __enum: "Ordering", __var: "Less", __kind: 0, __disc: -1 };
        if (c > 0) return { __rs: 2, __enum: "Ordering", __var: "Greater", __kind: 0, __disc: 1 };
        return { __rs: 2, __enum: "Ordering", __var: "Equal", __kind: 0, __disc: 0 };
    }
    rt.mkOrdering = mkOrdering;

    function mapKeyOf(v) {
        v = deref(v);
        if (v === null || v === undefined) return "u:";
        const t = typeof v;
        if (t === "number") return "n:" + v;
        if (t === "bigint") return "n:" + v.toString();
        if (t === "boolean") return "b:" + (v ? "1" : "0");
        if (t === "string") return "s:" + v;
        if (v.__rs === 4) return "s:" + v.s;
        if (v.__rs === 5) return "c:" + v.cp;
        if (v.__rs === 3) return "t:[" + v.__items.map(mapKeyOf).join(",") + "]";
        if (v.__rs === 2) {
            if (v.__kind === 0) return "e:" + v.__enum + "::" + v.__var;
            if (v.__kind === 1) return "e:" + v.__enum + "::" + v.__var + "(" + v.__data.map(mapKeyOf).join(",") + ")";
            return "e:" + v.__enum + "::" + v.__var + "{" + Object.keys(v.__data).sort().map(function (k) { return k + ":" + mapKeyOf(v.__data[k]); }).join(",") + "}";
        }
        if (v.__rs === 1) {
            const ks = Object.keys(v).filter(function (k) { return k[0] !== "_"; }).sort();
            return "S:" + v.__type + "{" + ks.map(function (k) { return k + ":" + mapKeyOf(v[k]); }).join(",") + "}";
        }
        if (v.__rs === 7) return "v:[" + v.items.map(mapKeyOf).join(",") + "]";
        if (v.__rs === 15 && v.kind === "path") return "p:" + v.p;
        return "o:" + fmtDebug(v);
    }
    rt.mapKeyOf = mapKeyOf;

    function cloneVal(v) {
        v = deref(v);
        if (v === null || v === undefined || typeof v !== "object") return v;
        switch (v.__rs) {
            case 1: {
                const custom = lookupUserMethod(v.__type, "clone");
                if (custom) return custom(v);
                const out = { __rs: 1, __type: v.__type, __tuple: v.__tuple };
                if (v.__tuple) {
                    out.__len = v.__len;
                    for (let i = 0; i < v.__len; i++) out[i] = cloneVal(v[i]);
                } else {
                    const ks = Object.keys(v);
                    for (let i = 0; i < ks.length; i++) {
                        const k = ks[i];
                        if (k !== "__rs" && k !== "__type" && k !== "__tuple" && k !== "__len") {
                            out[k] = cloneVal(v[k]);
                        }
                    }
                }
                return out;
            }
            case 2: {
                if (v.__kind === 0) return v;
                if (v.__kind === 1) return { __rs: 2, __enum: v.__enum, __var: v.__var, __kind: 1, __data: v.__data.map(cloneVal) };
                const d = {};
                const ks = Object.keys(v.__data);
                for (let i = 0; i < ks.length; i++) d[ks[i]] = cloneVal(v.__data[ks[i]]);
                return { __rs: 2, __enum: v.__enum, __var: v.__var, __kind: 2, __data: d };
            }
            case 3: return { __rs: 3, __items: v.__items.map(cloneVal) };
            case 4: return { __rs: 4, s: v.s };
            case 5: return { __rs: 5, ch: v.ch, cp: v.cp };
            case 7: return { __rs: 7, items: v.items.map(cloneVal) };
            case 9: return { __rs: 9, start: v.start, end: v.end, inclusive: v.inclusive };
            case 10: {
                const m = new Map();
                for (const [mk, entry] of v.map) m.set(mk, [cloneVal(entry[0]), cloneVal(entry[1])]);
                return { __rs: 10, ordered: v.ordered, map: m };
            }
            case 11: {
                const s = new Map();
                for (const [mk, val] of v.set) s.set(mk, cloneVal(val));
                return { __rs: 11, ordered: v.ordered, set: s };
            }
            case 12: return mkDeque(dequeToArray(v).map(cloneVal));
            case 13: return { __rs: 13, items: v.items.map(cloneVal) };
            case 14: {
                // Arc and Rc share the same reference on clone!
                if (v.kind === "arc" || v.kind === "rc") return v;
                if (v.kind === "box") return { __rs: 14, kind: "box", v: cloneVal(v.v) };
                return v;
            }
            case 15: {
                if (v.kind === "path") return { __rs: 15, kind: "path", p: v.p };
                if (v.kind === "duration") return { __rs: 15, kind: "duration", ns: v.ns };
                if (v.kind === "json") return rt.mkJson(JSON.parse(JSON.stringify(v.v)));
                return v;
            }
            default:
                return v;
        }
    }
    R.clone = cloneVal;
    rt.cloneVal = cloneVal;

    function defaultOf(typeName) {
        const raw = typeName ? String(typeName).trim() : "";
        const base = raw.split("<")[0].trim();
        const shortBase = base.indexOf("::") >= 0 ? base.slice(base.lastIndexOf("::") + 2) : base;
        switch (shortBase) {
            case "i8": case "i16": case "i32": case "i64": case "isize":
            case "u8": case "u16": case "u32": case "u64": case "usize":
            case "f32": case "f64": return 0;
            case "i128": case "u128": return 0n;
            case "bool": return false;
            case "char": return mkChar("\0");
            case "String": return mkString("");
            case "str": case "&str": return "";
            case "Option": return NONE;
            case "Vec": return { __rs: 7, items: [] };
            case "HashMap":
            case "BTreeMap": {
                const m = rt.mkMap(shortBase === "BTreeMap");
                const lt = raw.indexOf("<");
                if (lt >= 0) {
                    let depth = 0, comma = -1, end = raw.length;
                    for (let j = lt + 1; j < raw.length; j++) {
                        const ch = raw[j];
                        if (ch === "<" || ch === "(" || ch === "[") depth++;
                        else if (ch === ">" || ch === ")" || ch === "]") {
                            if (ch === ">" && depth === 0) { end = j; break; }
                            if (depth > 0) depth--;
                        } else if (ch === "," && depth === 0 && comma < 0) comma = j;
                    }
                    if (comma >= 0) {
                        const vt = raw.slice(comma + 1, end).trim();
                        if (vt) m.valType = vt;
                    }
                }
                return m;
            }
            case "HashSet": return rt.mkSet(false);
            case "BTreeSet": return rt.mkSet(true);
            case "VecDeque": return mkDeque(null);
            case "BinaryHeap": return { __rs: 13, items: [] };
            case "()": return undefined;
        }
        const custom = lookupTraitMethod("Default", shortBase, "default") || lookupUserMethod(shortBase, "default");
        if (custom) return custom();
        const st = structs[shortBase] || structs[base] || structs[raw];
        if (st) {
            if (st.isTuple) {
                const items = [];
                for (let i = 0; i < st.fields.length; i++) items.push(defaultOf(st.fields[i][1]));
                return R.mkTupleStruct(shortBase, items);
            }
            const obj = {};
            for (let i = 0; i < st.fields.length; i++) obj[st.fields[i][0]] = defaultOf(st.fields[i][1]);
            return R.mkStruct(shortBase, obj);
        }
        return undefined;
    }
    R.defaultOf = defaultOf;
    rt.defaultOf = defaultOf;

    // ---- Arithmetic & Casts ----
    R.add = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "number" && typeof b === "number") return a + b;
        if (typeof a === "bigint" && typeof b === "bigint") return a + b;
        if ((a && a.__rs === 4) || typeof a === "string") return mkString(strVal(a) + strVal(b));
        if (a && a.__rs === 15 && a.kind === "duration" && b && b.__rs === 15 && b.kind === "duration") {
            return { __rs: 15, kind: "duration", ns: a.ns + b.ns };
        }
        if (a && a.__rs === 15 && a.kind === "instant" && b && b.__rs === 15 && b.kind === "duration") {
            return { __rs: 15, kind: "instant", ns: a.ns + b.ns };
        }
        if (a && a.__rs === 15 && a.kind === "offset_datetime" && b && b.__rs === 15 && b.kind === "duration") {
            return { __rs: 15, kind: "offset_datetime", ns: a.ns + b.ns };
        }
        if (a && a.__rs === 1) {
            const m = lookupTraitMethod("Add", a.__type, "add");
            if (m) return m(a, b);
        }
        if (typeof a === "bigint" || typeof b === "bigint") return BigInt(Math.trunc(Number(a))) + BigInt(Math.trunc(Number(b)));
        return a + b;
    };
    R.sub = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "number" && typeof b === "number") return a - b;
        if (typeof a === "bigint" && typeof b === "bigint") return a - b;
        if (a && a.__rs === 15 && a.kind === "duration" && b && b.__rs === 15 && b.kind === "duration") {
            return { __rs: 15, kind: "duration", ns: a.ns - b.ns };
        }
        if (a && a.__rs === 15 && a.kind === "instant" && b && b.__rs === 15 && b.kind === "instant") {
            return { __rs: 15, kind: "duration", ns: a.ns >= b.ns ? (a.ns - b.ns) : 0n };
        }
        if (a && a.__rs === 15 && a.kind === "instant" && b && b.__rs === 15 && b.kind === "duration") {
            return { __rs: 15, kind: "instant", ns: a.ns - b.ns };
        }
        if (a && a.__rs === 15 && a.kind === "offset_datetime" && b && b.__rs === 15 && b.kind === "duration") {
            return { __rs: 15, kind: "offset_datetime", ns: a.ns - b.ns };
        }
        if (a && a.__rs === 15 && a.kind === "offset_datetime" && b && b.__rs === 15 && b.kind === "offset_datetime") {
            return { __rs: 15, kind: "duration", ns: a.ns - b.ns };
        }
        if (a && a.__rs === 1) {
            const m = lookupTraitMethod("Sub", a.__type, "sub");
            if (m) return m(a, b);
        }
        if (typeof a === "bigint" || typeof b === "bigint") return BigInt(Math.trunc(Number(a))) - BigInt(Math.trunc(Number(b)));
        return a - b;
    };
    R.mul = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "number" && typeof b === "number") return a * b;
        if (typeof a === "bigint" && typeof b === "bigint") return a * b;
        if (a && a.__rs === 1) {
            const m = lookupTraitMethod("Mul", a.__type, "mul");
            if (m) return m(a, b);
        }
        if (typeof a === "bigint" || typeof b === "bigint") return BigInt(Math.trunc(Number(a))) * BigInt(Math.trunc(Number(b)));
        return a * b;
    };
    R.div = function (a, b, isInt) {
        a = deref(a); b = deref(b);
        if (typeof a === "bigint" || typeof b === "bigint") {
            const ba = BigInt(a), bb = BigInt(b);
            if (bb === 0n) rsPanic("attempt to divide by zero");
            return ba / bb;
        }
        if (a && a.__rs === 1) {
            const m = lookupTraitMethod("Div", a.__type, "div");
            if (m) return m(a, b);
        }
        if (isInt) {
            if (b === 0) rsPanic("attempt to divide by zero");
            return Math.trunc(a / b);
        }
        return a / b;
    };
    R.rem = function (a, b, isInt) {
        a = deref(a); b = deref(b);
        if (typeof a === "bigint" || typeof b === "bigint") {
            const ba = BigInt(a), bb = BigInt(b);
            if (bb === 0n) rsPanic("attempt to calculate the remainder with a divisor of zero");
            return ba % bb;
        }
        if (isInt && b === 0) rsPanic("attempt to calculate the remainder with a divisor of zero");
        return a % b;
    };
    R.neg = function (a) {
        a = deref(a);
        if (a && a.__rs === 1) {
            const m = lookupTraitMethod("Neg", a.__type, "neg");
            if (m) return m(a);
        }
        return -a;
    };
    R.not = function (a) {
        a = deref(a);
        if (typeof a === "boolean") return !a;
        if (typeof a === "bigint") return ~a;
        return ~a;
    };
    R.bitAnd = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "boolean" && typeof b === "boolean") return a && b;
        if (typeof a === "bigint" || typeof b === "bigint") return BigInt(a) & BigInt(b);
        return a & b;
    };
    R.bitOr = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "boolean" && typeof b === "boolean") return a || b;
        if (typeof a === "bigint" || typeof b === "bigint") return BigInt(a) | BigInt(b);
        return a | b;
    };
    R.bitXor = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "boolean" && typeof b === "boolean") return a !== b;
        if (typeof a === "bigint" || typeof b === "bigint") return BigInt(a) ^ BigInt(b);
        return a ^ b;
    };
    R.shl = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "bigint" || typeof b === "bigint") return BigInt(a) << BigInt(b);
        return a << b;
    };
    R.shr = function (a, b) {
        a = deref(a); b = deref(b);
        if (typeof a === "bigint" || typeof b === "bigint") return BigInt(a) >> BigInt(b);
        return a >> b;
    };

    R.cast = function (v, ty) {
        v = deref(v);
        let n;
        if (typeof v === "boolean") n = v ? 1 : 0;
        else if (v && v.__rs === 5) n = v.cp;
        else if (v && v.__rs === 2 && v.__disc !== null) n = v.__disc;
        else if (typeof v === "bigint") n = v;
        else n = Number(v);

        switch (ty) {
            case "i8": return ((Number(n) << 24) >> 24);
            case "i16": return ((Number(n) << 16) >> 16);
            case "i32": return (Number(n) | 0);
            case "u8": return (Number(n) & 0xFF);
            case "u16": return (Number(n) & 0xFFFF);
            case "u32": return (Number(n) >>> 0);
            case "i64": case "isize":
                if (typeof n === "bigint") return BigInt.asIntN(64, n);
                return Math.trunc(n);
            case "u64": case "usize":
                if (typeof n === "bigint") return BigInt.asUintN(64, n);
                return n < 0 ? (n >>> 0) : Math.trunc(n);
            case "i128": return BigInt.asIntN(128, typeof n === "bigint" ? n : BigInt(Math.trunc(n)));
            case "u128": return BigInt.asUintN(128, typeof n === "bigint" ? n : BigInt(Math.trunc(n)));
            case "f32": return Math.fround(Number(n));
            case "f64": return Number(n);
            case "char": return mkChar(Number(n) & 0x1FFFFF);
            case "bool": return !!n;
            default: return v;
        }
    };

    // ---- Formatting (`Display` / `Debug` / Rust `std::fmt` specifiers) ----
    function fmtDisplay(v) {
        v = deref(v);
        if (v === undefined || v === null) return "";
        const t = typeof v;
        if (t === "string") return v;
        if (t === "number") return Object.is(v, -0) ? "-0" : String(v);
        if (t === "bigint") return v.toString();
        if (t === "boolean") return v ? "true" : "false";
        if (v.__rs === 4) return v.s;
        if (v.__rs === 5) return v.ch;
        if (v.__rs === 1) {
            const disp = lookupTraitMethod("Display", v.__type, "fmt") || lookupUserMethod(v.__type, "to_string");
            if (disp) {
                const buf = mkString("");
                const res = disp(v, buf);
                if (res && (typeof res === "string" || res.__rs === 4)) return strVal(res);
                if (buf.s.length > 0) return buf.s;
            }
            return fmtDebug(v, false, 0);
        }
        if (v.__rs === 2) {
            if (v.__enum === "Method") return v.__var;
            if (v.__sqlx && v.msg) return v.msg;
            const disp = lookupTraitMethod("Display", v.__enum, "fmt");
            if (disp) {
                const buf = mkString("");
                disp(v, buf);
                if (buf.s.length > 0) return buf.s;
            }
            return fmtDebug(v, false, 0);
        }
        if (v.__rs === 15) {
            if (v.kind === "path") return v.p;
            if (v.kind === "io_error" || v.kind === "anyhow_error" || v.kind === "sqlx_db_error") return v.msg;
            if (v.kind === "json") return JSON.stringify(v.v);
            if (v.kind === "socket_addr") return v.ip.indexOf(":") >= 0 ? ("[" + v.ip + "]:" + v.port) : (v.ip + ":" + v.port);
            if (v.kind === "ip_addr") return v.ip;
            if (v.kind === "uri") return v.raw;
            if (v.kind === "header_value") return v.v;
            if (v.kind === "header_name") return v.n;
            if (v.kind === "uuid" || v.kind === "argon2_hash") return v.s;
            if (v.kind === "digest_bytes") return v.hex;
            if (v.kind === "offset_datetime" || v.kind === "datetime") {
                return rt.callStdMethod ? strVal(rt.callStdMethod(v, "to_rfc3339", [])) : String(v.ns);
            }
        }
        return fmtDebug(v, false, 0);
    }
    rt.fmtDisplay = fmtDisplay;
    R.fmtDisplay = fmtDisplay;

    function escapeRustStr(s) {
        let out = '"';
        for (let i = 0; i < s.length; i++) {
            const c = s[i];
            if (c === "\\") out += "\\\\";
            else if (c === '"') out += '\\"';
            else if (c === "\n") out += "\\n";
            else if (c === "\r") out += "\\r";
            else if (c === "\t") out += "\\t";
            else if (c === "\0") out += "\\0";
            else out += c;
        }
        return out + '"';
    }

    function fmtDebug(v, pretty, indent) {
        v = deref(v);
        pretty = !!pretty;
        indent = indent || 0;
        const pad = pretty ? " ".repeat(indent) : "";
        const pad1 = pretty ? " ".repeat(indent + 4) : "";
        if (v === undefined || v === null) return "()";
        const t = typeof v;
        if (t === "string") return escapeRustStr(v);
        if (t === "number") return Object.is(v, -0) ? "-0.0" : String(v);
        if (t === "bigint") return v.toString();
        if (t === "boolean") return v ? "true" : "false";
        if (t === "function") return "<fn>";
        switch (v.__rs) {
            case 4: return escapeRustStr(v.s);
            case 5: {
                const c = v.ch;
                if (c === "'") return "'\\''";
                if (c === "\\") return "'\\\\'";
                if (c === "\n") return "'\\n'";
                if (c === "\r") return "'\\r'";
                if (c === "\t") return "'\\t'";
                return "'" + c + "'";
            }
            case 1: {
                if (v.__tuple) {
                    if (v.__len === 0) return v.__type;
                    const parts = [];
                    for (let i = 0; i < v.__len; i++) parts.push(fmtDebug(v[i], pretty, indent + 4));
                    return pretty
                        ? v.__type + "(\n" + pad1 + parts.join(",\n" + pad1) + ",\n" + pad + ")"
                        : v.__type + "(" + parts.join(", ") + ")";
                }
                const ks = Object.keys(v).filter(function (k) {
                    return k !== "__rs" && k !== "__type" && k !== "__tuple" && k !== "__len";
                });
                if (ks.length === 0) return v.__type;
                const parts = ks.map(function (k) { return k + ": " + fmtDebug(v[k], pretty, indent + 4); });
                return pretty
                    ? v.__type + " {\n" + pad1 + parts.join(",\n" + pad1) + ",\n" + pad + "}"
                    : v.__type + " { " + parts.join(", ") + " }";
            }
            case 2: {
                if (v.__kind === 0) return v.__var;
                if (v.__kind === 1) {
                    const parts = v.__data.map(function (x) { return fmtDebug(x, pretty, indent + 4); });
                    return pretty
                        ? v.__var + "(\n" + pad1 + parts.join(",\n" + pad1) + ",\n" + pad + ")"
                        : v.__var + "(" + parts.join(", ") + ")";
                }
                const ks = Object.keys(v.__data);
                const parts = ks.map(function (k) { return k + ": " + fmtDebug(v.__data[k], pretty, indent + 4); });
                return pretty
                    ? v.__var + " {\n" + pad1 + parts.join(",\n" + pad1) + ",\n" + pad + "}"
                    : v.__var + " { " + parts.join(", ") + " }";
            }
            case 3: {
                if (v.__items.length === 0) return "()";
                if (v.__items.length === 1) return "(" + fmtDebug(v.__items[0], pretty, indent + 4) + ",)";
                const parts = v.__items.map(function (x) { return fmtDebug(x, pretty, indent + 4); });
                return pretty
                    ? "(\n" + pad1 + parts.join(",\n" + pad1) + ",\n" + pad + ")"
                    : "(" + parts.join(", ") + ")";
            }
            case 12: {
                if (v.len === 0) return "[]";
                const parts = dequeToArray(v).map(function (x) { return fmtDebug(x, pretty, indent + 4); });
                return pretty
                    ? "[\n" + pad1 + parts.join(",\n" + pad1) + ",\n" + pad + "]"
                    : "[" + parts.join(", ") + "]";
            }
            case 7:
            case 13: {
                if (v.items.length === 0) return "[]";
                const parts = v.items.map(function (x) { return fmtDebug(x, pretty, indent + 4); });
                return pretty
                    ? "[\n" + pad1 + parts.join(",\n" + pad1) + ",\n" + pad + "]"
                    : "[" + parts.join(", ") + "]";
            }
            case 9: {
                const s = v.start === null ? "" : fmtDebug(v.start);
                const e = v.end === null ? "" : fmtDebug(v.end);
                return s + (v.inclusive ? "..=" : "..") + e;
            }
            case 10: {
                const entries = rt.mapEntries(v);
                if (entries.length === 0) return "{}";
                const parts = entries.map(function (e) {
                    return fmtDebug(e[0], pretty, indent + 4) + ": " + fmtDebug(e[1], pretty, indent + 4);
                });
                return pretty
                    ? "{\n" + pad1 + parts.join(",\n" + pad1) + ",\n" + pad + "}"
                    : "{" + parts.join(", ") + "}";
            }
            case 11: {
                const items = rt.setItems(v);
                if (items.length === 0) return "{}";
                const parts = items.map(function (x) { return fmtDebug(x, pretty, indent + 4); });
                return "{" + parts.join(", ") + "}";
            }
            case 14: return fmtDebug(v.v, pretty, indent);
            case 15: {
                if (v.kind === "path") return escapeRustStr(v.p);
                if (v.kind === "duration") {
                    const ms = Number(v.ns) / 1e6;
                    return ms >= 1000 ? (ms / 1000).toFixed(3) + "s" : ms.toFixed(3) + "ms";
                }
                if (v.kind === "io_error") return 'Error { kind: ' + v.errKind + ', message: ' + escapeRustStr(v.msg) + ' }';
                if (v.kind === "json") return JSON.stringify(v.v, null, pretty ? 2 : 0);
                return "<" + v.kind + ">";
            }
            default:
                return String(v);
        }
    }
    rt.fmtDebug = fmtDebug;
    R.fmtDebug = fmtDebug;

    function formatOne(val, spec) {
        if (!spec) return fmtDisplay(val);
        // Parse Rust format spec: [[fill]align][sign][#][0][width][.prec][type]
        let s = spec;
        let fill = " ";
        let align = "";
        if (s.length >= 2 && (s[1] === "<" || s[1] === ">" || s[1] === "^")) {
            fill = s[0];
            align = s[1];
            s = s.slice(2);
        } else if (s.length >= 1 && (s[0] === "<" || s[0] === ">" || s[0] === "^")) {
            align = s[0];
            s = s.slice(1);
        }
        let signPlus = false;
        if (s[0] === "+") { signPlus = true; s = s.slice(1); }
        else if (s[0] === "-") { s = s.slice(1); }
        let alt = false;
        if (s[0] === "#") { alt = true; s = s.slice(1); }
        let zeroPad = false;
        if (s[0] === "0") { zeroPad = true; s = s.slice(1); }
        let width = 0;
        const wm = s.match(/^(\d+)/);
        if (wm) { width = parseInt(wm[1], 10); s = s.slice(wm[1].length); }
        let prec = -1;
        if (s[0] === ".") {
            s = s.slice(1);
            const pm = s.match(/^(\d+)/);
            if (pm) { prec = parseInt(pm[1], 10); s = s.slice(pm[1].length); }
        }
        const ty = s;
        const v = deref(val);
        let out;
        if (ty === "?") {
            out = fmtDebug(v, alt, 0);
        } else if (ty === "x" || ty === "X") {
            if (v && typeof v === "object" && v.__rs === 15 && v.kind === "digest_bytes") {
                out = ty === "X" ? v.hex.toUpperCase() : v.hex.toLowerCase();
            } else {
                let n = typeof v === "bigint" ? v : BigInt(Math.trunc(Number(v)));
                if (n < 0n) n = BigInt.asUintN(64, n);
                let h = n.toString(16);
                if (ty === "X") h = h.toUpperCase();
                out = (alt ? "0x" : "") + h;
            }
        } else if (ty === "b") {
            let n = typeof v === "bigint" ? v : BigInt(Math.trunc(Number(v)));
            if (n < 0n) n = BigInt.asUintN(64, n);
            out = (alt ? "0b" : "") + n.toString(2);
        } else if (ty === "o") {
            let n = typeof v === "bigint" ? v : BigInt(Math.trunc(Number(v)));
            if (n < 0n) n = BigInt.asUintN(64, n);
            out = (alt ? "0o" : "") + n.toString(8);
        } else if (ty === "e" || ty === "E") {
            const num = Number(v);
            out = prec >= 0 ? num.toExponential(prec) : num.toExponential();
            if (ty === "E") out = out.toUpperCase();
        } else if (prec >= 0 && typeof v === "number") {
            out = v.toFixed(prec);
        } else {
            out = fmtDisplay(v);
            if (prec >= 0 && typeof v === "string") out = out.slice(0, prec);
        }
        if (signPlus && typeof v === "number" && v >= 0 && out[0] !== "+" && out[0] !== "-") {
            out = "+" + out;
        }
        if (width > out.length) {
            const diff = width - out.length;
            if (zeroPad && !align) {
                if (out[0] === "-" || out[0] === "+") out = out[0] + "0".repeat(diff) + out.slice(1);
                else out = "0".repeat(diff) + out;
            } else if (align === "<") {
                out = out + fill.repeat(diff);
            } else if (align === "^") {
                const left = Math.floor(diff / 2);
                out = fill.repeat(left) + out + fill.repeat(diff - left);
            } else if (align === ">") {
                out = fill.repeat(diff) + out;
            } else {
                // Default: numbers right-align, strings left-align
                if (typeof v === "number" || typeof v === "bigint") out = fill.repeat(diff) + out;
                else out = out + fill.repeat(diff);
            }
        }
        return out;
    }

    function fmtTemplate(tmpl, posArgs, namedArgs) {
        const s = strVal(tmpl);
        let out = "";
        let i = 0;
        let nextPos = 0;
        const n = s.length;
        while (i < n) {
            const c = s[i];
            if (c === "{") {
                if (i + 1 < n && s[i + 1] === "{") {
                    out += "{";
                    i += 2;
                    continue;
                }
                const close = s.indexOf("}", i + 1);
                if (close < 0) rsPanic("invalid format string: unmatched `{`");
                const inside = s.slice(i + 1, close);
                const colon = inside.indexOf(":");
                const key = colon >= 0 ? inside.slice(0, colon) : inside;
                const spec = colon >= 0 ? inside.slice(colon + 1) : "";
                let val;
                if (key === "") {
                    val = posArgs[nextPos++];
                } else if (/^\d+$/.test(key)) {
                    val = posArgs[parseInt(key, 10)];
                } else if (namedArgs && Object.prototype.hasOwnProperty.call(namedArgs, key)) {
                    val = namedArgs[key];
                } else {
                    rsPanic("format argument `" + key + "` not provided");
                }
                out += formatOne(val, spec);
                i = close + 1;
            } else if (c === "}") {
                if (i + 1 < n && s[i + 1] === "}") {
                    out += "}";
                    i += 2;
                    continue;
                }
                rsPanic("invalid format string: unmatched `}`");
            } else {
                out += c;
                i++;
            }
        }
        return out;
    }
    rt.fmtTemplate = fmtTemplate;

    R.format = function (tmpl, posArgs, namedArgs) {
        return mkString(fmtTemplate(tmpl, posArgs || [], namedArgs || null));
    };

    function writeStdout(text) {
        if (typeof __zipp_rs_io === "function") {
            __zipp_rs_io("stdout", text);
        } else {
            console.log(text);
        }
    }
    function writeStderr(text) {
        if (typeof __zipp_rs_io === "function") {
            __zipp_rs_io("stderr", text);
        } else {
            console.error(text);
        }
    }
    rt.writeStdout = writeStdout;
    rt.writeStderr = writeStderr;

    R.print = function (tmpl, posArgs, namedArgs) {
        writeStdout(fmtTemplate(tmpl, posArgs || [], namedArgs || null));
        return undefined;
    };
    R.println = function (tmpl, posArgs, namedArgs) {
        if (tmpl === undefined) { writeStdout("\n"); return undefined; }
        writeStdout(fmtTemplate(tmpl, posArgs || [], namedArgs || null) + "\n");
        return undefined;
    };
    R.eprint = function (tmpl, posArgs, namedArgs) {
        writeStderr(fmtTemplate(tmpl, posArgs || [], namedArgs || null));
        return undefined;
    };
    R.eprintln = function (tmpl, posArgs, namedArgs) {
        if (tmpl === undefined) { writeStderr("\n"); return undefined; }
        writeStderr(fmtTemplate(tmpl, posArgs || [], namedArgs || null) + "\n");
        return undefined;
    };
    R.writeFmt = function (dst, tmpl, posArgs, namedArgs, newline) {
        const text = fmtTemplate(tmpl, posArgs || [], namedArgs || null) + (newline ? "\n" : "");
        const target = deref(dst);
        if (target && target.__rs === 4) {
            target.s += text;
            return R.Ok(undefined);
        }
        if (target && target.__rs === 7) {
            const bytes = strToUtf8Bytes(text);
            for (let i = 0; i < bytes.length; i++) target.items.push(bytes[i]);
            return R.Ok(undefined);
        }
        if (target && target.__rs === 15) {
            return rt.callStdMethod(target, "write_all", [{ __rs: 7, items: strToUtf8Bytes(text) }]);
        }
        rsPanic("write! target does not implement Write");
    };
    R.dbg = function (exprText, val) {
        writeStderr("[dbg] " + exprText + " = " + fmtDebug(val, true, 0) + "\n");
        return val;
    };
    R.logMsg = function (level, tmpl, posArgs, namedArgs) {
        const ts = typeof __zipp_rs_time === "function"
            ? __zipp_rs_time("rfc3339", Number(__zipp_rs_time("unix", 0) / 1000000000n))
            : "";
        const msg = fmtTemplate(tmpl, posArgs || [], namedArgs || null);
        writeStderr((ts ? ts + " " : "") + "[" + level + "] " + msg + "\n");
        return undefined;
    };

    // ---- Assertions ----
    R.assert = function (cond, msg) {
        if (!deref(cond)) {
            rsPanic(msg !== undefined ? strVal(msg) : "assertion failed");
        }
        return undefined;
    };
    R.assertEq = function (left, right, msg) {
        if (!eq(left, right)) {
            const detail = "assertion `left == right` failed" +
                (msg !== undefined ? ": " + strVal(msg) : "") +
                "\n  left: " + fmtDebug(left) + "\n right: " + fmtDebug(right);
            rsPanic(detail);
        }
        return undefined;
    };
    R.assertNe = function (left, right, msg) {
        if (eq(left, right)) {
            const detail = "assertion `left != right` failed" +
                (msg !== undefined ? ": " + strVal(msg) : "") +
                "\n  left: " + fmtDebug(left) + "\n right: " + fmtDebug(right);
            rsPanic(detail);
        }
        return undefined;
    };

    // ---- Pattern matching helpers ----
    R.patEnumMatch = function (val, enumName, varName) {
        const v = deref(val);
        if (!v || v.__rs !== 2) return false;
        if (v.__var !== varName) return false;
        if (enumName && v.__enum !== enumName) return false;
        return true;
    };
    R.patEnumField = function (val, idx) {
        const v = deref(val);
        return v.__data[idx];
    };
    R.patEnumNamed = function (val, name) {
        const v = deref(val);
        return v.__data[name];
    };
    R.patStructMatch = function (val, typeName) {
        const v = deref(val);
        if (!v) return false;
        if (v.__rs === 1) return !typeName || v.__type === typeName;
        if (v.__rs === 2) return v.__var === typeName;
        return false;
    };
    R.patSliceLen = function (val) {
        const v = deref(val);
        if (v && v.__rs === 7) return v.items.length;
        if (v && v.__rs === 3) return v.__items.length;
        return -1;
    };
    R.patSliceElem = function (val, idx) {
        const v = deref(val);
        if (v && v.__rs === 7) return v.items[idx];
        if (v && v.__rs === 3) return v.__items[idx];
        return undefined;
    };
    R.patSliceSub = function (val, start, end) {
        const v = deref(val);
        if (v && v.__rs === 7) return { __rs: 7, items: v.items.slice(start, end) };
        return { __rs: 7, items: [] };
    };
    R.patRangeMatch = function (val, start, end, inclusive) {
        const v = deref(val);
        if (start !== undefined && cmpVal(v, start) < 0) return false;
        if (end !== undefined) {
            const c = cmpVal(v, end);
            if (inclusive ? c > 0 : c >= 0) return false;
        }
        return true;
    };

    return R;
})();
