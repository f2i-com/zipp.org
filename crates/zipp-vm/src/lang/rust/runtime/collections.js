/* ZIPP Rust runtime — String/&str, char, numbers, Option, Result, Vec/slice,
 * Iterator adapters/consumers, HashMap/BTreeMap (with Entry API), HashSet/BTreeSet,
 * VecDeque, BinaryHeap, Range, and unified method/associated-function dispatch. Apache-2.0.
 */
(function (R) {
    "use strict";
    const rt = R.__rt;
    const deref = rt.deref;
    const strVal = rt.strVal;
    const eq = rt.eq;
    const cmpVal = rt.cmpVal;
    const cloneVal = rt.cloneVal;
    const mapKeyOf = rt.mapKeyOf;
    const ITER_DONE = R.ITER_DONE;
    const Some = R.Some;
    const None = R.None;
    const Ok = R.Ok;
    const Err = R.Err;

    // ---- Map & Set low-level helpers ----
    function mkMap(ordered) {
        return { __rs: 10, ordered: !!ordered, map: new Map() };
    }
    function mkSet(ordered) {
        return { __rs: 11, ordered: !!ordered, set: new Map() };
    }
    function mapGetRaw(m, k) {
        const e = m.map.get(mapKeyOf(k));
        return e ? e[1] : undefined;
    }
    function typedNone(valType) {
        return valType ? { __rs: 2, __enum: "Option", __var: "None", __data: [], __defaultType: valType } : None;
    }
    function mapGetOpt(m, k) {
        const e = m.map.get(mapKeyOf(k));
        return e ? Some(e[1]) : typedNone(m.valType);
    }
    function mapSetRaw(m, k, v) {
        const mk = mapKeyOf(k);
        const prev = m.map.get(mk);
        m.map.set(mk, [k, v]);
        return prev ? Some(prev[1]) : None;
    }
    function mapEntries(m) {
        const out = Array.from(m.map.values());
        if (m.ordered) out.sort(function (a, b) { return cmpVal(a[0], b[0]); });
        return out;
    }
    function setHasRaw(s, k) {
        return s.set.has(mapKeyOf(k));
    }
    function setItems(s) {
        const out = Array.from(s.set.values());
        if (s.ordered) out.sort(cmpVal);
        return out;
    }
    rt.mkMap = mkMap;
    rt.mkSet = mkSet;
    rt.mapGetRaw = mapGetRaw;
    rt.mapGetOpt = mapGetOpt;
    rt.mapSetRaw = mapSetRaw;
    rt.mapEntries = mapEntries;
    rt.setHasRaw = setHasRaw;
    rt.setItems = setItems;

    // ---- Iterators ----
    function mkIter(nextFn) {
        return { __rs: 8, next: nextFn };
    }
    rt.mkIter = mkIter;

    function intoIter(v) {
        const raw = v;
        v = deref(v);
        if (v && v.__rs === 8) return v;
        if (v && v.__rs === 7) {
            let i = 0;
            const items = v.items;
            return mkIter(function () {
                if (i >= items.length) return ITER_DONE;
                return items[i++];
            });
        }
        if (v && v.__rs === 12) {
            let i = 0;
            const n = v.len, b = v.buf, m = b.length - 1, h = v.head;
            return mkIter(function () {
                if (i >= n) return ITER_DONE;
                return b[(h + i++) & m];
            });
        }
        if (v && v.__rs === 13) {
            const copy = v.items.slice().sort(function (a, b) { return cmpVal(b, a); });
            let i = 0;
            return mkIter(function () {
                if (i >= copy.length) return ITER_DONE;
                return copy[i++];
            });
        }
        if (v && v.__rs === 9) {
            // Range / RangeInclusive (supports numbers, BigInt, and char!)
            if (v.start && v.start.__rs === 5) {
                let cur = v.start.cp;
                const end = v.end ? v.end.cp : 0x10FFFF;
                const inc = v.inclusive;
                return mkIter(function () {
                    if (inc ? cur > end : cur >= end) return ITER_DONE;
                    return R.mkChar(cur++);
                });
            }
            if (typeof v.start === "bigint" || typeof v.end === "bigint") {
                let cur = v.start === null ? 0n : BigInt(v.start);
                const end = v.end === null ? null : BigInt(v.end);
                const inc = v.inclusive;
                return mkIter(function () {
                    if (end !== null && (inc ? cur > end : cur >= end)) return ITER_DONE;
                    const out = cur;
                    cur += 1n;
                    return out;
                });
            }
            let cur = v.start === null ? 0 : Number(v.start);
            const end = v.end === null ? Infinity : Number(v.end);
            const inc = v.inclusive;
            return mkIter(function () {
                if (inc ? cur > end : cur >= end) return ITER_DONE;
                return cur++;
            });
        }
        if (v && v.__rs === 10) {
            const entries = mapEntries(v);
            let i = 0;
            return mkIter(function () {
                if (i >= entries.length) return ITER_DONE;
                const e = entries[i++];
                return R.mkTuple([e[0], e[1]]);
            });
        }
        if (v && v.__rs === 11) {
            const items = setItems(v);
            let i = 0;
            return mkIter(function () {
                if (i >= items.length) return ITER_DONE;
                return items[i++];
            });
        }
        if (v && v.__rs === 2 && v.__enum === "Option") {
            let done = v.__var === "None";
            const item = done ? undefined : v.__data[0];
            return mkIter(function () {
                if (done) return ITER_DONE;
                done = true;
                return item;
            });
        }
        if (v && v.__rs === 15 && v.kind === "tcp_listener") {
            return rt.tcpIncomingIter(v);
        }
        if (v && v.__rs === 1) {
            const fn = rt.lookupUserMethod(v.__type, "into_iter") || rt.lookupUserMethod(v.__type, "next");
            if (fn) {
                if (rt.lookupUserMethod(v.__type, "into_iter")) return intoIter(fn(raw));
                return mkIter(function () {
                    const r = fn(raw);
                    if (r && r.__rs === 2 && r.__var === "Some") return r.__data[0];
                    return ITER_DONE;
                });
            }
        }
        R.panic("value is not iterable: " + rt.fmtDebug(v));
    }
    R.intoIter = intoIter;
    R.iterNext = function (it) {
        return it.next();
    };

    function drainIter(it) {
        const out = [];
        for (;;) {
            const x = it.next();
            if (x === ITER_DONE) break;
            out.push(x);
        }
        return out;
    }

    function callIterMethod(it, name, args, typeArgs) {
        switch (name) {
            case "next": {
                const x = it.next();
                return x === ITER_DONE ? None : Some(x);
            }
            case "map": {
                const f = args[0];
                return mkIter(function () {
                    const x = it.next();
                    return x === ITER_DONE ? ITER_DONE : f(x);
                });
            }
            case "filter": {
                const f = args[0];
                return mkIter(function () {
                    for (;;) {
                        const x = it.next();
                        if (x === ITER_DONE) return ITER_DONE;
                        if (deref(f(x))) return x;
                    }
                });
            }
            case "filter_map": {
                const f = args[0];
                return mkIter(function () {
                    for (;;) {
                        const x = it.next();
                        if (x === ITER_DONE) return ITER_DONE;
                        const r = deref(f(x));
                        if (r && r.__rs === 2 && r.__var === "Some") return r.__data[0];
                    }
                });
            }
            case "flat_map": {
                const f = args[0];
                let inner = null;
                return mkIter(function () {
                    for (;;) {
                        if (inner) {
                            const y = inner.next();
                            if (y !== ITER_DONE) return y;
                            inner = null;
                        }
                        const x = it.next();
                        if (x === ITER_DONE) return ITER_DONE;
                        inner = intoIter(f(x));
                    }
                });
            }
            case "flatten": {
                let inner = null;
                return mkIter(function () {
                    for (;;) {
                        if (inner) {
                            const y = inner.next();
                            if (y !== ITER_DONE) return y;
                            inner = null;
                        }
                        const x = it.next();
                        if (x === ITER_DONE) return ITER_DONE;
                        inner = intoIter(x);
                    }
                });
            }
            case "enumerate": {
                let idx = 0;
                return mkIter(function () {
                    const x = it.next();
                    if (x === ITER_DONE) return ITER_DONE;
                    return R.mkTuple([idx++, x]);
                });
            }
            case "zip": {
                const other = intoIter(args[0]);
                return mkIter(function () {
                    const a = it.next();
                    if (a === ITER_DONE) return ITER_DONE;
                    const b = other.next();
                    if (b === ITER_DONE) return ITER_DONE;
                    return R.mkTuple([a, b]);
                });
            }
            case "chain": {
                const other = intoIter(args[0]);
                let firstDone = false;
                return mkIter(function () {
                    if (!firstDone) {
                        const a = it.next();
                        if (a !== ITER_DONE) return a;
                        firstDone = true;
                    }
                    return other.next();
                });
            }
            case "take": {
                let rem = Number(deref(args[0]));
                return mkIter(function () {
                    if (rem <= 0) return ITER_DONE;
                    rem--;
                    return it.next();
                });
            }
            case "skip": {
                let rem = Number(deref(args[0]));
                return mkIter(function () {
                    while (rem > 0) {
                        rem--;
                        if (it.next() === ITER_DONE) return ITER_DONE;
                    }
                    return it.next();
                });
            }
            case "take_while": {
                const f = args[0];
                let stopped = false;
                return mkIter(function () {
                    if (stopped) return ITER_DONE;
                    const x = it.next();
                    if (x === ITER_DONE || !deref(f(x))) { stopped = true; return ITER_DONE; }
                    return x;
                });
            }
            case "skip_while": {
                const f = args[0];
                let skipping = true;
                return mkIter(function () {
                    for (;;) {
                        const x = it.next();
                        if (x === ITER_DONE) return ITER_DONE;
                        if (skipping) {
                            if (deref(f(x))) continue;
                            skipping = false;
                        }
                        return x;
                    }
                });
            }
            case "step_by": {
                const step = Number(deref(args[0]));
                if (step <= 0) R.panic("step_by(0)");
                let first = true;
                return mkIter(function () {
                    if (first) { first = false; return it.next(); }
                    for (let i = 1; i < step; i++) {
                        if (it.next() === ITER_DONE) return ITER_DONE;
                    }
                    return it.next();
                });
            }
            case "rev": {
                const all = drainIter(it).reverse();
                let i = 0;
                return mkIter(function () { return i >= all.length ? ITER_DONE : all[i++]; });
            }
            case "cycle": {
                const buf = [];
                let filled = false;
                let i = 0;
                return mkIter(function () {
                    if (!filled) {
                        const x = it.next();
                        if (x !== ITER_DONE) { buf.push(x); return x; }
                        filled = true;
                        if (buf.length === 0) return ITER_DONE;
                    }
                    const out = buf[i];
                    i = (i + 1) % buf.length;
                    return out;
                });
            }
            case "inspect": {
                const f = args[0];
                return mkIter(function () {
                    const x = it.next();
                    if (x === ITER_DONE) return ITER_DONE;
                    f(x);
                    return x;
                });
            }
            case "cloned":
            case "copied":
                return mkIter(function () {
                    const x = it.next();
                    return x === ITER_DONE ? ITER_DONE : cloneVal(x);
                });
            case "fuse":
                return it;
            case "peekable": {
                let hasPeek = false;
                let peekVal = ITER_DONE;
                const p = mkIter(function () {
                    if (hasPeek) { hasPeek = false; return peekVal; }
                    return it.next();
                });
                p.peek = function () {
                    if (!hasPeek) { peekVal = it.next(); hasPeek = true; }
                    return peekVal === ITER_DONE ? None : Some(peekVal);
                };
                return p;
            }
            case "peek":
                return it.peek ? it.peek() : None;
            case "collect": {
                const all = drainIter(it);
                const rawTarget = typeArgs && typeArgs.length > 0 ? typeArgs[0] : "";
                const target = rawTarget.split("<")[0].trim();
                if (target === "String") {
                    return R.mkString(all.map(strVal).join(""));
                }
                if (target === "HashMap" || target === "BTreeMap") {
                    const m = mkMap(target === "BTreeMap");
                    for (let i = 0; i < all.length; i++) {
                        const pair = deref(all[i]);
                        if (pair && pair.__rs === 3) mapSetRaw(m, pair.__items[0], pair.__items[1]);
                    }
                    return m;
                }
                if (target === "HashSet" || target === "BTreeSet") {
                    const s = mkSet(target === "BTreeSet");
                    for (let i = 0; i < all.length; i++) s.set.set(mapKeyOf(all[i]), all[i]);
                    return s;
                }
                if (target === "VecDeque") return rt.mkDeque(all);
                if (target === "Result") {
                    const vals = [];
                    for (let i = 0; i < all.length; i++) {
                        const r = deref(all[i]);
                        if (r && r.__rs === 2 && r.__var === "Err") return r;
                        vals.push(r && r.__rs === 2 && r.__var === "Ok" ? r.__data[0] : r);
                    }
                    return Ok({ __rs: 7, items: vals });
                }
                if (target === "Option") {
                    const vals = [];
                    for (let i = 0; i < all.length; i++) {
                        const r = deref(all[i]);
                        if (r && r.__rs === 2 && r.__var === "None") return None;
                        vals.push(r && r.__rs === 2 && r.__var === "Some" ? r.__data[0] : r);
                    }
                    return Some({ __rs: 7, items: vals });
                }
                return { __rs: 7, items: all };
            }
            case "sum": {
                let acc = 0;
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    acc = R.add(acc, x);
                }
                return acc;
            }
            case "product": {
                let acc = 1;
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    acc = R.mul(acc, x);
                }
                return acc;
            }
            case "count": {
                let c = 0;
                while (it.next() !== ITER_DONE) c++;
                return c;
            }
            case "last": {
                let last = ITER_DONE;
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    last = x;
                }
                return last === ITER_DONE ? None : Some(last);
            }
            case "nth": {
                const idx = Number(deref(args[0]));
                for (let i = 0; i < idx; i++) {
                    if (it.next() === ITER_DONE) return None;
                }
                const x = it.next();
                return x === ITER_DONE ? None : Some(x);
            }
            case "fold": {
                let acc = args[0];
                const f = args[1];
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    acc = f(acc, x);
                }
                return acc;
            }
            case "reduce": {
                const f = args[0];
                let acc = it.next();
                if (acc === ITER_DONE) return None;
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    acc = f(acc, x);
                }
                return Some(acc);
            }
            case "for_each": {
                const f = args[0];
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    f(x);
                }
                return undefined;
            }
            case "any": {
                const f = args[0];
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) return false;
                    if (deref(f(x))) return true;
                }
            }
            case "all": {
                const f = args[0];
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) return true;
                    if (!deref(f(x))) return false;
                }
            }
            case "find": {
                const f = args[0];
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) return None;
                    if (deref(f(x))) return Some(x);
                }
            }
            case "find_map": {
                const f = args[0];
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) return None;
                    const r = deref(f(x));
                    if (r && r.__rs === 2 && r.__var === "Some") return r;
                }
            }
            case "position": {
                const f = args[0];
                let i = 0;
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) return None;
                    if (deref(f(x))) return Some(i);
                    i++;
                }
            }
            case "max": {
                let best = it.next();
                if (best === ITER_DONE) return None;
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    if (cmpVal(x, best) > 0) best = x;
                }
                return Some(best);
            }
            case "min": {
                let best = it.next();
                if (best === ITER_DONE) return None;
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    if (cmpVal(x, best) < 0) best = x;
                }
                return Some(best);
            }
            case "max_by_key": {
                const f = args[0];
                let best = it.next();
                if (best === ITER_DONE) return None;
                let bestK = f(best);
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    const k = f(x);
                    if (cmpVal(k, bestK) >= 0) { best = x; bestK = k; }
                }
                return Some(best);
            }
            case "min_by_key": {
                const f = args[0];
                let best = it.next();
                if (best === ITER_DONE) return None;
                let bestK = f(best);
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    const k = f(x);
                    if (cmpVal(k, bestK) < 0) { best = x; bestK = k; }
                }
                return Some(best);
            }
            case "max_by": {
                const f = args[0];
                let best = it.next();
                if (best === ITER_DONE) return None;
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    const ord = deref(f(x, best));
                    if (ord && ord.__rs === 2 && ord.__var === "Greater") best = x;
                }
                return Some(best);
            }
            case "min_by": {
                const f = args[0];
                let best = it.next();
                if (best === ITER_DONE) return None;
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    const ord = deref(f(x, best));
                    if (ord && ord.__rs === 2 && ord.__var === "Less") best = x;
                }
                return Some(best);
            }
            case "rposition": {
                const f = args[0];
                const all = drainIter(it);
                for (let i = all.length - 1; i >= 0; i--) {
                    if (deref(f(all[i]))) return Some(i);
                }
                return None;
            }
            case "try_fold": {
                let acc = args[0];
                const f = args[1];
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) return Ok(acc);
                    const r = deref(f(acc, x));
                    if (r && r.__rs === 2 && (r.__var === "Err" || r.__var === "None")) return r;
                    acc = r && r.__rs === 2 ? r.__data[0] : r;
                }
            }
            case "try_for_each": {
                const f = args[0];
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) return Ok(undefined);
                    const r = deref(f(x));
                    if (r && r.__rs === 2 && (r.__var === "Err" || r.__var === "None")) return r;
                }
            }
            case "scan": {
                const state = R.mkRef(args[0]);
                const f = args[1];
                return mkIter(function () {
                    const x = it.next();
                    if (x === ITER_DONE) return ITER_DONE;
                    const r = deref(f(state, x));
                    return (r && r.__rs === 2 && r.__var === "Some") ? r.__data[0] : ITER_DONE;
                });
            }
            case "partition": {
                const f = args[0];
                const left = [], right = [];
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    if (deref(f(x))) left.push(x); else right.push(x);
                }
                return R.mkTuple([{ __rs: 7, items: left }, { __rs: 7, items: right }]);
            }
            case "unzip": {
                const left = [], right = [];
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    const t = deref(x);
                    left.push(t.__items[0]);
                    right.push(t.__items[1]);
                }
                return R.mkTuple([{ __rs: 7, items: left }, { __rs: 7, items: right }]);
            }
            case "into_iter":
                return it;
        }
        R.panic("unknown Iterator method `" + name + "`");
    }

    // ---- String / &str methods ----
    function parseStrAs(s, target) {
        const t = s.trim();
        if (target === "bool") {
            if (t === "true") return Ok(true);
            if (t === "false") return Ok(false);
            return Err(R.mkString("provided string was not `true` or `false`"));
        }
        if (target === "f32" || target === "f64") {
            if (t === "") return Err(R.mkString("cannot parse float from empty string"));
            const n = Number(t);
            if (Number.isNaN(n) && t.toLowerCase() !== "nan") return Err(R.mkString("invalid float literal"));
            return Ok(target === "f32" ? Math.fround(n) : n);
        }
        if (target === "String") return Ok(R.mkString(s));
        if (target === "SocketAddr" || target === "std::net::SocketAddr") {
            return rt.parseSocketAddr ? rt.parseSocketAddr(t) : Err(R.mkString("invalid socket address: " + s));
        }
        if (target === "IpAddr" || target === "Ipv4Addr" || target === "Ipv6Addr") {
            return rt.parseIpAddr ? rt.parseIpAddr(t, target) : Err(R.mkString("invalid ip address: " + s));
        }
        if (target === "Uuid" || target === "uuid::Uuid") {
            return rt.parseUuid ? rt.parseUuid(t) : Ok({ __rs: 15, kind: "uuid", s: t });
        }
        if (target === "i128" || target === "u128") {
            try { return Ok(BigInt(t)); } catch (_) { return Err(R.mkString("invalid digit found in string")); }
        }
        // Integer parse (default if no turbofish or i32/i64/usize/u32/u64/etc.)
        if (/^[+-]?\d+$/.test(t)) {
            const n = Number(t);
            if ((target && target[0] === "u") && n < 0) {
                return Err(R.mkString("invalid digit found in string"));
            }
            return Ok(Math.trunc(n));
        }
        if (!target) {
            const f = Number(t);
            if (t !== "" && !Number.isNaN(f)) return Ok(f);
        }
        return Err(R.mkString("invalid digit found in string: " + s));
    }

    function matchCharPat(ch, pat) {
        if (typeof pat === "function") return !!deref(pat(R.mkChar(ch)));
        const dp = deref(pat);
        if (dp && dp.__rs === 7) {
            for (let i = 0; i < dp.items.length; i++) {
                if (strVal(dp.items[i]) === ch) return true;
            }
            return false;
        }
        return strVal(dp).indexOf(ch) >= 0;
    }

    function splitByPat(s, patArg) {
        const dp = deref(patArg);
        if (typeof patArg === "function" || (dp && dp.__rs === 7)) {
            const parts = [];
            let cur = "";
            const chars = Array.from(s);
            for (let i = 0; i < chars.length; i++) {
                if (matchCharPat(chars[i], patArg)) {
                    parts.push(cur);
                    cur = "";
                } else {
                    cur += chars[i];
                }
            }
            parts.push(cur);
            return parts;
        }
        return s.split(strVal(dp));
    }

    function callStrMethod(obj, name, args, typeArgs) {
        const isOwned = obj && typeof obj === "object" && obj.__rs === 4;
        const s = isOwned ? obj.s : obj;
        switch (name) {
            case "len": return rt.utf8ByteLen(s);
            case "is_empty": return s.length === 0;
            case "as_str": return s;
            case "to_string":
            case "to_owned":
            case "clone": return R.mkString(s);
            case "push_str":
                if (!isOwned) R.panic("cannot call push_str on &str");
                obj.s += strVal(args[0]);
                return undefined;
            case "push":
                if (!isOwned) R.panic("cannot call push on &str");
                obj.s += strVal(args[0]);
                return undefined;
            case "pop":
                if (!isOwned) R.panic("cannot call pop on &str");
                {
                    const n = obj.s.length;
                    if (n === 0) return None;
                    let take = 1;
                    const lo = obj.s.charCodeAt(n - 1);
                    if (lo >= 0xDC00 && lo <= 0xDFFF && n >= 2) {
                        const hi = obj.s.charCodeAt(n - 2);
                        if (hi >= 0xD800 && hi <= 0xDBFF) take = 2;
                    }
                    const last = obj.s.slice(n - take);
                    obj.s = obj.s.slice(0, n - take);
                    return Some(R.mkChar(last));
                }
            case "clear":
                if (isOwned) obj.s = "";
                return undefined;
            case "truncate":
                if (isOwned) {
                    const n = Number(deref(args[0]));
                    obj.s = rt.utf8BytesToStr(rt.strToUtf8Bytes(obj.s).slice(0, n));
                }
                return undefined;
            case "trim": return s.trim();
            case "trim_start": case "trim_left": return s.trimStart();
            case "trim_end": case "trim_right": return s.trimEnd();
            case "trim_matches": {
                const chars = Array.from(s);
                let start = 0, end = chars.length;
                while (start < end && matchCharPat(chars[start], args[0])) start++;
                while (end > start && matchCharPat(chars[end - 1], args[0])) end--;
                return chars.slice(start, end).join("");
            }
            case "trim_start_matches": {
                const dp = deref(args[0]);
                if (typeof args[0] === "function" || (dp && dp.__rs === 7)) {
                    const chars = Array.from(s);
                    let start = 0;
                    while (start < chars.length && matchCharPat(chars[start], args[0])) start++;
                    return chars.slice(start).join("");
                }
                const pat = strVal(dp);
                let out = s;
                while (pat.length > 0 && out.startsWith(pat)) out = out.slice(pat.length);
                return out;
            }
            case "trim_end_matches": {
                const dp = deref(args[0]);
                if (typeof args[0] === "function" || (dp && dp.__rs === 7)) {
                    const chars = Array.from(s);
                    let end = chars.length;
                    while (end > 0 && matchCharPat(chars[end - 1], args[0])) end--;
                    return chars.slice(0, end).join("");
                }
                const pat = strVal(dp);
                let out = s;
                while (pat.length > 0 && out.endsWith(pat)) out = out.slice(0, -pat.length);
                return out;
            }
            case "insert": {
                if (isOwned) {
                    const idx = Number(deref(args[0]));
                    obj.s = obj.s.slice(0, idx) + strVal(args[1]) + obj.s.slice(idx);
                }
                return undefined;
            }
            case "insert_str": {
                if (isOwned) {
                    const idx = Number(deref(args[0]));
                    obj.s = obj.s.slice(0, idx) + strVal(args[1]) + obj.s.slice(idx);
                }
                return undefined;
            }
            case "retain": {
                if (isOwned) {
                    const f = args[0];
                    const arr = Array.from(obj.s);
                    let out = "";
                    for (let i = 0; i < arr.length; i++) {
                        if (deref(f(R.mkChar(arr[i])))) out += arr[i];
                    }
                    obj.s = out;
                }
                return undefined;
            }
            case "matches": {
                const pat = strVal(args[0]);
                const out = [];
                if (pat.length > 0) {
                    let pos = 0;
                    for (;;) {
                        const idx = s.indexOf(pat, pos);
                        if (idx < 0) break;
                        out.push(pat);
                        pos = idx + pat.length;
                    }
                }
                let i = 0;
                return mkIter(function () { return i >= out.length ? ITER_DONE : out[i++]; });
            }
            case "to_lowercase": case "to_ascii_lowercase": return R.mkString(s.toLowerCase());
            case "to_uppercase": case "to_ascii_uppercase": return R.mkString(s.toUpperCase());
            case "eq_ignore_ascii_case": return s.toLowerCase() === strVal(args[0]).toLowerCase();
            case "is_ascii": {
                for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) > 127) return false;
                return true;
            }
            case "contains": return s.indexOf(strVal(args[0])) >= 0;
            case "starts_with": return s.startsWith(strVal(args[0]));
            case "ends_with": return s.endsWith(strVal(args[0]));
            case "find": {
                const idx = s.indexOf(strVal(args[0]));
                return idx < 0 ? None : Some(rt.utf8ByteLen(s.slice(0, idx)));
            }
            case "rfind": {
                const idx = s.lastIndexOf(strVal(args[0]));
                return idx < 0 ? None : Some(rt.utf8ByteLen(s.slice(0, idx)));
            }
            case "strip_prefix": {
                const p = strVal(args[0]);
                return s.startsWith(p) ? Some(s.slice(p.length)) : None;
            }
            case "strip_suffix": {
                const p = strVal(args[0]);
                return s.endsWith(p) ? Some(s.slice(0, s.length - p.length)) : None;
            }
            case "replace": {
                const dp = deref(args[0]);
                const to = strVal(args[1]);
                if (typeof args[0] === "function" || (dp && dp.__rs === 7)) {
                    const chars = Array.from(s);
                    let out = "";
                    for (let i = 0; i < chars.length; i++) {
                        out += matchCharPat(chars[i], args[0]) ? to : chars[i];
                    }
                    return R.mkString(out);
                }
                return R.mkString(s.split(strVal(dp)).join(to));
            }
            case "replacen": {
                const from = strVal(args[0]), to = strVal(args[1]);
                let count = Number(deref(args[2]));
                if (count <= 0 || from.length === 0) return R.mkString(s);
                let out = "", pos = 0;
                while (count > 0) {
                    const idx = s.indexOf(from, pos);
                    if (idx < 0) break;
                    out += s.slice(pos, idx) + to;
                    pos = idx + from.length;
                    count--;
                }
                return R.mkString(pos === 0 ? s : out + s.slice(pos));
            }
            case "repeat": return R.mkString(s.repeat(Number(deref(args[0]))));
            case "split": {
                const parts = splitByPat(s, args[0]);
                let i = 0;
                return mkIter(function () { return i >= parts.length ? ITER_DONE : parts[i++]; });
            }
            case "rsplit": {
                const parts = splitByPat(s, args[0]).reverse();
                let i = 0;
                return mkIter(function () { return i >= parts.length ? ITER_DONE : parts[i++]; });
            }
            case "splitn": {
                const n = Number(deref(args[0]));
                const sep = strVal(args[1]);
                const parts = [];
                let rest = s;
                for (let k = 1; k < n; k++) {
                    const idx = rest.indexOf(sep);
                    if (idx < 0) break;
                    parts.push(rest.slice(0, idx));
                    rest = rest.slice(idx + sep.length);
                }
                if (n > 0) parts.push(rest);
                let i = 0;
                return mkIter(function () { return i >= parts.length ? ITER_DONE : parts[i++]; });
            }
            case "split_once": {
                const sep = strVal(args[0]);
                const idx = s.indexOf(sep);
                if (idx < 0) return None;
                return Some(R.mkTuple([s.slice(0, idx), s.slice(idx + sep.length)]));
            }
            case "rsplit_once": {
                const sep = strVal(args[0]);
                const idx = s.lastIndexOf(sep);
                if (idx < 0) return None;
                return Some(R.mkTuple([s.slice(0, idx), s.slice(idx + sep.length)]));
            }
            case "split_whitespace": {
                const parts = s.trim() === "" ? [] : s.trim().split(/\s+/);
                let i = 0;
                return mkIter(function () { return i >= parts.length ? ITER_DONE : parts[i++]; });
            }
            case "lines": {
                if (s.length === 0) return mkIter(function () { return ITER_DONE; });
                const trimmed = s.endsWith("\r\n") ? s.slice(0, -2) : (s.endsWith("\n") ? s.slice(0, -1) : s);
                const parts = trimmed.split(/\r?\n/);
                let i = 0;
                return mkIter(function () { return i >= parts.length ? ITER_DONE : parts[i++]; });
            }
            case "chars": {
                const arr = Array.from(s);
                let i = 0;
                return mkIter(function () { return i >= arr.length ? ITER_DONE : R.mkChar(arr[i++]); });
            }
            case "char_indices": {
                const arr = Array.from(s);
                let i = 0, byteOff = 0;
                return mkIter(function () {
                    if (i >= arr.length) return ITER_DONE;
                    const ch = arr[i++];
                    const cur = byteOff;
                    byteOff += rt.utf8ByteLen(ch);
                    return R.mkTuple([cur, R.mkChar(ch)]);
                });
            }
            case "bytes": {
                const bs = rt.strToUtf8Bytes(s);
                let i = 0;
                return mkIter(function () { return i >= bs.length ? ITER_DONE : bs[i++]; });
            }
            case "as_bytes":
            case "into_bytes":
                return { __rs: 7, items: rt.strToUtf8Bytes(s) };
            case "as_os_str":
            case "to_string_lossy":
            case "into_owned":
                return R.mkString(s);
            case "encode_wide": {
                const codes = [];
                for (let i = 0; i < s.length; i++) codes.push(s.charCodeAt(i));
                let idx = 0;
                return mkIter(function () { return idx >= codes.length ? ITER_DONE : codes[idx++]; });
            }
            case "into_response":
                return rt.mkResponse(200, s, "text/plain; charset=utf-8");
            case "into": {
                const dst = typeArgs && typeArgs[0] ? typeArgs[0] : "";
                return dst ? R.convertInto(obj, dst) : R.mkString(s);
            }
            case "parse":
                return parseStrAs(s, typeArgs && typeArgs[0] ? typeArgs[0] : "");
            case "cmp": return rt.mkOrdering(cmpVal(s, args[0]));
            case "partial_cmp": return Some(rt.mkOrdering(cmpVal(s, args[0])));
            case "get": {
                const k = deref(args[0]);
                if (k && k.__rs === 9) {
                    const bytes = rt.strToUtf8Bytes(s);
                    const st = k.start === null ? 0 : Number(k.start);
                    const en = k.end === null ? bytes.length : (k.inclusive ? Number(k.end) + 1 : Number(k.end));
                    if (st < 0 || en > bytes.length || st > en) return None;
                    return Some(rt.utf8BytesToStr(bytes.slice(st, en)));
                }
                return None;
            }
        }
        R.panic("unknown String/str method `" + name + "`");
    }

    // ---- char methods ----
    function callCharMethod(c, name, args) {
        const ch = c.ch, cp = c.cp;
        switch (name) {
            case "is_alphabetic": return /[a-zA-Z\u00C0-\u024F]/.test(ch);
            case "is_numeric": return /[0-9]/.test(ch);
            case "is_alphanumeric": return /[a-zA-Z0-9\u00C0-\u024F]/.test(ch);
            case "is_whitespace": return /\s/.test(ch);
            case "is_ascii": return cp <= 0x7F;
            case "is_ascii_alphabetic": return (cp >= 65 && cp <= 90) || (cp >= 97 && cp <= 122);
            case "is_ascii_alphanumeric": return (cp >= 48 && cp <= 57) || (cp >= 65 && cp <= 90) || (cp >= 97 && cp <= 122);
            case "is_ascii_digit": return cp >= 48 && cp <= 57;
            case "is_ascii_hexdigit": return (cp >= 48 && cp <= 57) || (cp >= 65 && cp <= 70) || (cp >= 97 && cp <= 102);
            case "is_ascii_whitespace": return cp === 32 || cp === 9 || cp === 10 || cp === 12 || cp === 13;
            case "is_ascii_lowercase": case "is_lowercase": return ch === ch.toLowerCase() && ch !== ch.toUpperCase();
            case "is_ascii_uppercase": case "is_uppercase": return ch === ch.toUpperCase() && ch !== ch.toLowerCase();
            case "to_ascii_lowercase": return R.mkChar(ch.toLowerCase());
            case "to_ascii_uppercase": return R.mkChar(ch.toUpperCase());
            case "to_lowercase": return R.mkString(ch.toLowerCase());
            case "to_uppercase": return R.mkString(ch.toUpperCase());
            case "to_digit": {
                const radix = Number(deref(args[0]));
                const v = parseInt(ch, radix);
                return Number.isNaN(v) ? None : Some(v);
            }
            case "len_utf8": return rt.utf8ByteLen(ch);
            case "to_string": return R.mkString(ch);
            case "clone": return R.mkChar(cp);
        }
        R.panic("unknown char method `" + name + "`");
    }

    // ---- Numeric methods ----
    function callNumMethod(v, name, args) {
        const n = Number(v);
        const a0 = args.length > 0 ? deref(args[0]) : undefined;
        const a1 = args.length > 1 ? deref(args[1]) : undefined;
        switch (name) {
            case "abs": return typeof v === "bigint" ? (v < 0n ? -v : v) : Math.abs(n);
            case "abs_diff": return Math.abs(n - Number(a0));
            case "ilog2": return Math.floor(Math.log2(n));
            case "ilog10": return Math.floor(Math.log10(n));
            case "isqrt": return Math.floor(Math.sqrt(n));
            case "signum": return n > 0 ? 1 : n < 0 ? -1 : 0;
            case "pow": return typeof v === "bigint" ? (v ** BigInt(a0)) : Math.pow(n, Number(a0));
            case "powi": case "powf": return Math.pow(n, Number(a0));
            case "sqrt": return Math.sqrt(n);
            case "cbrt": return Math.cbrt(n);
            case "floor": return Math.floor(n);
            case "ceil": return Math.ceil(n);
            case "round": return Math.round(n);
            case "trunc": return Math.trunc(n);
            case "fract": return n - Math.trunc(n);
            case "exp": return Math.exp(n);
            case "ln": return Math.log(n);
            case "log2": return Math.log2(n);
            case "log10": return Math.log10(n);
            case "sin": return Math.sin(n);
            case "cos": return Math.cos(n);
            case "tan": return Math.tan(n);
            case "asin": return Math.asin(n);
            case "acos": return Math.acos(n);
            case "atan": return Math.atan(n);
            case "atan2": return Math.atan2(n, Number(a0));
            case "to_degrees": return n * (180 / Math.PI);
            case "to_radians": return n * (Math.PI / 180);
            case "min": return cmpVal(v, a0) <= 0 ? v : a0;
            case "max": return cmpVal(v, a0) >= 0 ? v : a0;
            case "clamp": {
                if (cmpVal(v, a0) < 0) return a0;
                if (cmpVal(v, a1) > 0) return a1;
                return v;
            }
            case "rem_euclid": {
                const m = Number(a0);
                const r = n % m;
                return r < 0 ? r + Math.abs(m) : r;
            }
            case "div_euclid": {
                const m = Number(a0);
                let q = Math.trunc(n / m);
                if (n % m < 0) q += m > 0 ? -1 : 1;
                return q;
            }
            case "wrapping_add": return (n + Number(a0)) | 0;
            case "wrapping_sub": return (n - Number(a0)) | 0;
            case "wrapping_mul": return Math.imul(n, Number(a0));
            case "wrapping_neg": return (-n) | 0;
            case "saturating_add": return n + Number(a0);
            case "saturating_sub": return Math.max(0, n - Number(a0));
            case "saturating_mul": return n * Number(a0);
            case "checked_add": return Some(n + Number(a0));
            case "checked_sub": return Some(n - Number(a0));
            case "checked_mul": return Some(n * Number(a0));
            case "checked_div": return Number(a0) === 0 ? None : Some(Math.trunc(n / Number(a0)));
            case "checked_rem": return Number(a0) === 0 ? None : Some(n % Number(a0));
            case "count_ones": {
                let x = n >>> 0, c = 0;
                while (x) { x &= x - 1; c++; }
                return c;
            }
            case "leading_zeros": return Math.clz32(n >>> 0);
            case "trailing_zeros": {
                let x = n >>> 0;
                if (x === 0) return 32;
                let c = 0;
                while ((x & 1) === 0) { x >>>= 1; c++; }
                return c;
            }
            case "is_power_of_two": return n > 0 && (n & (n - 1)) === 0;
            case "next_power_of_two": {
                let p = 1;
                while (p < n) p *= 2;
                return p;
            }
            case "is_nan": return Number.isNaN(n);
            case "is_infinite": return !Number.isFinite(n) && !Number.isNaN(n);
            case "is_finite": return Number.isFinite(n);
            case "is_sign_positive": return n > 0 || Object.is(n, 0);
            case "is_sign_negative": return n < 0 || Object.is(n, -0);
            case "to_le_bytes": {
                const x = n >>> 0;
                return { __rs: 7, items: [x & 0xFF, (x >>> 8) & 0xFF, (x >>> 16) & 0xFF, (x >>> 24) & 0xFF] };
            }
            case "to_be_bytes": {
                const x = n >>> 0;
                return { __rs: 7, items: [(x >>> 24) & 0xFF, (x >>> 16) & 0xFF, (x >>> 8) & 0xFF, x & 0xFF] };
            }
            case "to_string": return R.mkString(String(v));
            case "clone": return v;
            case "cmp": return rt.mkOrdering(cmpVal(v, a0));
            case "partial_cmp": return Number.isNaN(n) || Number.isNaN(Number(a0)) ? None : Some(rt.mkOrdering(cmpVal(v, a0)));
            // `http::StatusCode` methods (represented as u16 JS numbers)
            case "as_u16": return n | 0;
            case "as_str": return String(n | 0);
            case "is_informational": return n >= 100 && n < 200;
            case "is_success": return n >= 200 && n < 300;
            case "is_redirection": return n >= 300 && n < 400;
            case "is_client_error": return n >= 400 && n < 500;
            case "is_server_error": return n >= 500 && n < 600;
            case "canonical_reason": return rt.statusReason ? Some(rt.statusReason(n | 0)) : None;
        }
        R.panic("unknown numeric method `" + name + "`");
    }

    // ---- Option & Result methods ----
    function callOptionMethod(opt, name, args, rawRecv, typeArgs) {
        const isSome = opt.__var === "Some";
        const inner = isSome ? opt.__data[0] : undefined;
        switch (name) {
            case "is_some": return isSome;
            case "is_none": return !isSome;
            case "is_some_and": return isSome && !!deref(args[0](inner));
            case "is_none_or": return !isSome || !!deref(args[0](inner));
            case "context":
                return isSome ? Ok(inner) : Err({ __rs: 15, kind: "anyhow_error", msg: strVal(args[0]), inner: null });
            case "with_context":
                return isSome ? Ok(inner) : Err({ __rs: 15, kind: "anyhow_error", msg: strVal(args[0]()), inner: null });
            case "unwrap":
                if (!isSome) R.panic("called `Option::unwrap()` on a `None` value");
                return inner;
            case "expect":
                if (!isSome) R.panic(strVal(args[0]));
                return inner;
            case "unwrap_or": return isSome ? inner : args[0];
            case "unwrap_or_else": return isSome ? inner : args[0]();
            case "unwrap_or_default": return isSome ? inner : rt.defaultOf((typeArgs && typeArgs[0]) || opt.__defaultType || "");
            case "map": return isSome ? Some(args[0](inner)) : None;
            case "map_or": return isSome ? args[1](inner) : args[0];
            case "map_or_else": return isSome ? args[1](inner) : args[0]();
            case "and_then": return isSome ? args[0](inner) : None;
            case "or": return isSome ? opt : args[0];
            case "or_else": return isSome ? opt : args[0]();
            case "and": return isSome ? args[0] : None;
            case "ok_or": return isSome ? Ok(inner) : Err(args[0]);
            case "ok_or_else": return isSome ? Ok(inner) : Err(args[0]());
            case "filter": return (isSome && deref(args[0](inner))) ? opt : None;
            case "flatten": return isSome ? inner : None;
            case "transpose": {
                if (!isSome) return Ok(None);
                const r = deref(inner);
                if (r && r.__rs === 2 && r.__enum === "Result") {
                    return r.__var === "Ok" ? Ok(Some(r.__data[0])) : Err(r.__data[0]);
                }
                return Ok(Some(inner));
            }
            case "zip": {
                const other = deref(args[0]);
                return (isSome && other && other.__rs === 2 && other.__var === "Some")
                    ? Some(R.mkTuple([inner, other.__data[0]])) : None;
            }
            case "as_ref":
            case "as_deref":
            case "copied": return isSome ? Some(cloneVal(inner)) : None;
            case "cloned": return isSome ? Some(cloneVal(inner)) : None;
            case "as_mut": {
                if (!isSome) return None;
                return Some({
                    __rs: 6,
                    get: function () { return opt.__data[0]; },
                    set: function (v) { opt.__data[0] = v; }
                });
            }
            case "take": {
                if (!isSome) return None;
                const old = Some(inner);
                R.rset(rawRecv, None);
                return old;
            }
            case "replace": {
                const old = isSome ? Some(inner) : None;
                R.rset(rawRecv, Some(args[0]));
                return old;
            }
            case "inspect": { if (isSome) args[0](inner); return opt; }
            case "iter": return intoIter(opt);
            case "clone": return cloneVal(opt);
        }
        R.panic("unknown Option method `" + name + "`");
    }

    function callResultMethod(res, name, args, typeArgs) {
        const isOk = res.__var === "Ok";
        const inner = res.__data[0];
        switch (name) {
            case "is_ok": return isOk;
            case "is_err": return !isOk;
            case "is_ok_and": return isOk && !!deref(args[0](inner));
            case "is_err_and": return !isOk && !!deref(args[0](inner));
            case "context":
                return isOk ? res : Err({ __rs: 15, kind: "anyhow_error", msg: strVal(args[0]) + ": " + rt.fmtDisplay(inner), inner: inner });
            case "with_context":
                return isOk ? res : Err({ __rs: 15, kind: "anyhow_error", msg: strVal(args[0]()) + ": " + rt.fmtDisplay(inner), inner: inner });
            case "as_deref":
                return isOk ? Ok(deref(inner)) : res;
            case "unwrap":
                if (!isOk) R.panic("called `Result::unwrap()` on an `Err` value: " + rt.fmtDebug(inner));
                return inner;
            case "expect":
                if (!isOk) R.panic(strVal(args[0]) + ": " + rt.fmtDebug(inner));
                return inner;
            case "unwrap_err":
                if (isOk) R.panic("called `Result::unwrap_err()` on an `Ok` value: " + rt.fmtDebug(inner));
                return inner;
            case "expect_err":
                if (isOk) R.panic(strVal(args[0]) + ": " + rt.fmtDebug(inner));
                return inner;
            case "unwrap_or": return isOk ? inner : args[0];
            case "unwrap_or_else": return isOk ? inner : args[0](inner);
            case "unwrap_or_default": return isOk ? inner : rt.defaultOf(typeArgs && typeArgs[0] ? typeArgs[0] : "");
            case "map": return isOk ? Ok(args[0](inner)) : res;
            case "map_err": return isOk ? res : Err(args[0](inner));
            case "map_or": return isOk ? args[1](inner) : args[0];
            case "map_or_else": return isOk ? args[1](inner) : args[0](inner);
            case "and_then": return isOk ? args[0](inner) : res;
            case "or": return isOk ? res : args[0];
            case "or_else": return isOk ? res : args[0](inner);
            case "ok": return isOk ? Some(inner) : None;
            case "err": return isOk ? None : Some(inner);
            case "transpose": {
                if (!isOk) return Some(Err(inner));
                const o = deref(inner);
                if (o && o.__rs === 2 && o.__enum === "Option") {
                    return o.__var === "Some" ? Some(Ok(o.__data[0])) : None;
                }
                return Some(Ok(inner));
            }
            case "as_ref": return res;
            case "copied":
            case "cloned": return cloneVal(res);
            case "inspect": { if (isOk) args[0](inner); return res; }
            case "inspect_err": { if (!isOk) args[0](inner); return res; }
            case "clone": return cloneVal(res);
        }
        R.panic("unknown Result method `" + name + "`");
    }

    // ---- Vec / Slice methods ----
    function callVecMethod(v, name, args) {
        const items = v.items;
        switch (name) {
            case "len": return items.length;
            case "is_empty": return items.length === 0;
            case "capacity": return items.length;
            case "push": items.push(args[0]); return undefined;
            case "pop": return items.length === 0 ? None : Some(items.pop());
            case "insert": {
                const idx = Number(deref(args[0]));
                items.splice(idx, 0, args[1]);
                return undefined;
            }
            case "remove": {
                const idx = Number(deref(args[0]));
                if (idx < 0 || idx >= items.length) R.panic("removal index out of bounds: " + idx);
                return items.splice(idx, 1)[0];
            }
            case "swap_remove": {
                const idx = Number(deref(args[0]));
                const val = items[idx];
                const last = items.pop();
                if (idx < items.length) items[idx] = last;
                return val;
            }
            case "clear": items.length = 0; return undefined;
            case "truncate": {
                const n = Number(deref(args[0]));
                if (n < items.length) items.length = n;
                return undefined;
            }
            case "resize": {
                const n = Number(deref(args[0]));
                const fill = args[1];
                if (n < items.length) items.length = n;
                else while (items.length < n) items.push(cloneVal(fill));
                return undefined;
            }
            case "extend": {
                const it = intoIter(args[0]);
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    items.push(x);
                }
                return undefined;
            }
            case "extend_from_slice": {
                const other = deref(args[0]);
                if (other && other.__rs === 7) {
                    for (let i = 0; i < other.items.length; i++) items.push(cloneVal(other.items[i]));
                }
                return undefined;
            }
            case "append": {
                const other = deref(args[0]);
                if (other && other.__rs === 7) {
                    for (let i = 0; i < other.items.length; i++) items.push(other.items[i]);
                    other.items.length = 0;
                }
                return undefined;
            }
            case "retain": {
                const f = args[0];
                let w = 0;
                for (let i = 0; i < items.length; i++) {
                    if (deref(f(items[i]))) items[w++] = items[i];
                }
                items.length = w;
                return undefined;
            }
            case "dedup": {
                if (items.length <= 1) return undefined;
                let w = 1;
                for (let i = 1; i < items.length; i++) {
                    if (!eq(items[i], items[w - 1])) items[w++] = items[i];
                }
                items.length = w;
                return undefined;
            }
            case "reverse": items.reverse(); return undefined;
            case "sort":
            case "sort_unstable":
                items.sort(cmpVal);
                return undefined;
            case "sort_by":
            case "sort_unstable_by": {
                const f = args[0];
                items.sort(function (a, b) {
                    const ord = deref(f(a, b));
                    if (ord && ord.__rs === 2) return ord.__var === "Less" ? -1 : ord.__var === "Greater" ? 1 : 0;
                    return Number(ord);
                });
                return undefined;
            }
            case "sort_by_key":
            case "sort_unstable_by_key": {
                const f = args[0];
                items.sort(function (a, b) { return cmpVal(f(a), f(b)); });
                return undefined;
            }
            case "dedup_by_key": {
                if (items.length <= 1) return undefined;
                const f = args[0];
                let w = 1, prevK = f(items[0]);
                for (let i = 1; i < items.length; i++) {
                    const k = f(items[i]);
                    if (!eq(k, prevK)) { items[w++] = items[i]; prevK = k; }
                }
                items.length = w;
                return undefined;
            }
            case "chunks_exact": {
                const sz = Number(deref(args[0]));
                if (sz <= 0) R.panic("chunk size must be non-zero");
                let pos = 0;
                return mkIter(function () {
                    if (pos + sz > items.length) return ITER_DONE;
                    const chunk = { __rs: 7, items: items.slice(pos, pos + sz) };
                    pos += sz;
                    return chunk;
                });
            }
            case "binary_search": {
                const target = deref(args[0]);
                let lo = 0, hi = items.length;
                while (lo < hi) {
                    const mid = (lo + hi) >>> 1;
                    const c = cmpVal(items[mid], target);
                    if (c === 0) return Ok(mid);
                    if (c < 0) lo = mid + 1; else hi = mid;
                }
                return Err(lo);
            }
            case "contains": {
                const target = args[0];
                for (let i = 0; i < items.length; i++) if (eq(items[i], target)) return true;
                return false;
            }
            case "first": return items.length === 0 ? None : Some(items[0]);
            case "last": return items.length === 0 ? None : Some(items[items.length - 1]);
            case "first_mut": return items.length === 0 ? None : Some(R.mkIdxRef(v, 0));
            case "last_mut": return items.length === 0 ? None : Some(R.mkIdxRef(v, items.length - 1));
            case "get": {
                const i = Number(deref(args[0]));
                return (i >= 0 && i < items.length) ? Some(items[i]) : None;
            }
            case "get_mut": {
                const i = Number(deref(args[0]));
                return (i >= 0 && i < items.length) ? Some(R.mkIdxRef(v, i)) : None;
            }
            case "swap": {
                const a = Number(deref(args[0])), b = Number(deref(args[1]));
                const tmp = items[a]; items[a] = items[b]; items[b] = tmp;
                return undefined;
            }
            case "split_at": {
                const mid = Number(deref(args[0]));
                return R.mkTuple([{ __rs: 7, items: items.slice(0, mid) }, { __rs: 7, items: items.slice(mid) }]);
            }
            case "split_off": {
                const at = Number(deref(args[0]));
                return { __rs: 7, items: items.splice(at) };
            }
            case "chunks": {
                const sz = Number(deref(args[0]));
                if (sz <= 0) R.panic("chunk size must be non-zero");
                let pos = 0;
                return mkIter(function () {
                    if (pos >= items.length) return ITER_DONE;
                    const chunk = { __rs: 7, items: items.slice(pos, pos + sz) };
                    pos += sz;
                    return chunk;
                });
            }
            case "windows": {
                const sz = Number(deref(args[0]));
                if (sz <= 0) R.panic("window size must be non-zero");
                let pos = 0;
                return mkIter(function () {
                    if (pos + sz > items.length) return ITER_DONE;
                    const win = { __rs: 7, items: items.slice(pos, pos + sz) };
                    pos += 1;
                    return win;
                });
            }
            case "join": {
                const sep = strVal(args[0]);
                return R.mkString(items.map(strVal).join(sep));
            }
            case "concat":
                return R.mkString(items.map(strVal).join(""));
            case "starts_with": {
                const needle = deref(args[0]);
                if (!needle || needle.__rs !== 7 || needle.items.length > items.length) return false;
                for (let i = 0; i < needle.items.length; i++) if (!eq(items[i], needle.items[i])) return false;
                return true;
            }
            case "ends_with": {
                const needle = deref(args[0]);
                if (!needle || needle.__rs !== 7 || needle.items.length > items.length) return false;
                const off = items.length - needle.items.length;
                for (let i = 0; i < needle.items.length; i++) if (!eq(items[off + i], needle.items[i])) return false;
                return true;
            }
            case "iter":
            case "into_iter":
                return intoIter(v);
            case "iter_mut": {
                let i = 0;
                return mkIter(function () {
                    if (i >= items.length) return ITER_DONE;
                    return R.mkIdxRef(v, i++);
                });
            }
            case "drain": {
                const drained = items.splice(0);
                let i = 0;
                return mkIter(function () { return i >= drained.length ? ITER_DONE : drained[i++]; });
            }
            case "as_slice":
            case "as_ptr":
            case "as_mut_ptr":
            case "into_boxed_slice":
                return v;
            case "to_vec":
            case "to_owned":
            case "clone": return cloneVal(v);
            case "reserve":
            case "shrink_to_fit":
                return undefined;
            case "fill": {
                const val = args[0];
                for (let i = 0; i < items.length; i++) items[i] = cloneVal(val);
                return undefined;
            }
            case "repeat": {
                const n = Number(deref(args[0]));
                const out = [];
                for (let k = 0; k < n; k++) for (let i = 0; i < items.length; i++) out.push(cloneVal(items[i]));
                return { __rs: 7, items: out };
            }
        }
        R.panic("unknown Vec/slice method `" + name + "`");
    }

    // ---- HashMap / BTreeMap methods (including Entry API) ----
    function callMapMethod(m, name, args) {
        switch (name) {
            case "len": return m.map.size;
            case "is_empty": return m.map.size === 0;
            case "clear": m.map.clear(); return undefined;
            case "insert": return mapSetRaw(m, args[0], args[1]);
            case "get": return mapGetOpt(m, args[0]);
            case "get_mut": {
                const mk = mapKeyOf(args[0]);
                if (!m.map.has(mk)) return typedNone(m.valType);
                return Some({
                    __rs: 6,
                    get: function () { return m.map.get(mk)[1]; },
                    set: function (val) { m.map.get(mk)[1] = val; }
                });
            }
            case "remove": {
                const mk = mapKeyOf(args[0]);
                const prev = m.map.get(mk);
                if (!prev) return typedNone(m.valType);
                m.map.delete(mk);
                return Some(prev[1]);
            }
            case "contains_key": return m.map.has(mapKeyOf(args[0]));
            case "keys":
            case "into_keys": {
                const entries = mapEntries(m);
                let i = 0;
                return mkIter(function () { return i >= entries.length ? ITER_DONE : entries[i++][0]; });
            }
            case "values":
            case "into_values": {
                const entries = mapEntries(m);
                let i = 0;
                return mkIter(function () { return i >= entries.length ? ITER_DONE : entries[i++][1]; });
            }
            case "first_key_value": {
                let best = null;
                for (const e of m.map.values()) {
                    if (best === null || cmpVal(e[0], best[0]) < 0) best = e;
                }
                return best === null ? None : Some(R.mkTuple([best[0], best[1]]));
            }
            case "last_key_value": {
                let best = null;
                for (const e of m.map.values()) {
                    if (best === null || cmpVal(e[0], best[0]) > 0) best = e;
                }
                return best === null ? None : Some(R.mkTuple([best[0], best[1]]));
            }
            case "pop_first": {
                let bestMk = null, best = null;
                for (const [mk, e] of m.map) {
                    if (best === null || cmpVal(e[0], best[0]) < 0) { bestMk = mk; best = e; }
                }
                if (best === null) return None;
                m.map.delete(bestMk);
                return Some(R.mkTuple([best[0], best[1]]));
            }
            case "pop_last": {
                let bestMk = null, best = null;
                for (const [mk, e] of m.map) {
                    if (best === null || cmpVal(e[0], best[0]) > 0) { bestMk = mk; best = e; }
                }
                if (best === null) return None;
                m.map.delete(bestMk);
                return Some(R.mkTuple([best[0], best[1]]));
            }
            case "iter":
            case "into_iter":
                return intoIter(m);
            case "iter_mut": {
                const entries = mapEntries(m);
                let i = 0;
                return mkIter(function () {
                    if (i >= entries.length) return ITER_DONE;
                    const e = entries[i++];
                    const mk = mapKeyOf(e[0]);
                    return R.mkTuple([e[0], {
                        __rs: 6,
                        get: function () { return m.map.get(mk)[1]; },
                        set: function (val) { m.map.get(mk)[1] = val; }
                    }]);
                });
            }
            case "extend": {
                const it = intoIter(args[0]);
                for (;;) {
                    const x = it.next();
                    if (x === ITER_DONE) break;
                    const t = deref(x);
                    if (t && t.__rs === 3) mapSetRaw(m, t.__items[0], t.__items[1]);
                }
                return undefined;
            }
            case "retain": {
                const f = args[0];
                for (const [mk, e] of Array.from(m.map.entries())) {
                    if (!deref(f(e[0], e[1]))) m.map.delete(mk);
                }
                return undefined;
            }
            case "entry": {
                const key = args[0];
                const mk = mapKeyOf(key);
                return { __rs: 15, kind: "entry", map: m, key: key, mk: mk };
            }
            case "clone": return cloneVal(m);
        }
        R.panic("unknown HashMap/BTreeMap method `" + name + "`");
    }

    // ---- HashSet / BTreeSet methods ----
    function callSetMethod(s, name, args) {
        switch (name) {
            case "len": return s.set.size;
            case "is_empty": return s.set.size === 0;
            case "clear": s.set.clear(); return undefined;
            case "insert": {
                const mk = mapKeyOf(args[0]);
                if (s.set.has(mk)) return false;
                s.set.set(mk, args[0]);
                return true;
            }
            case "remove": return s.set.delete(mapKeyOf(args[0]));
            case "contains": return s.set.has(mapKeyOf(args[0]));
            case "iter":
            case "into_iter": return intoIter(s);
            case "union": {
                const other = deref(args[0]);
                const seen = new Set();
                const out = [];
                for (const [mk, v] of s.set) { seen.add(mk); out.push(v); }
                for (const [mk, v] of other.set) if (!seen.has(mk)) out.push(v);
                let i = 0;
                return mkIter(function () { return i >= out.length ? ITER_DONE : out[i++]; });
            }
            case "intersection": {
                const other = deref(args[0]);
                const out = [];
                for (const [mk, v] of s.set) if (other.set.has(mk)) out.push(v);
                let i = 0;
                return mkIter(function () { return i >= out.length ? ITER_DONE : out[i++]; });
            }
            case "difference": {
                const other = deref(args[0]);
                const out = [];
                for (const [mk, v] of s.set) if (!other.set.has(mk)) out.push(v);
                let i = 0;
                return mkIter(function () { return i >= out.length ? ITER_DONE : out[i++]; });
            }
            case "is_subset": {
                const other = deref(args[0]);
                for (const mk of s.set.keys()) if (!other.set.has(mk)) return false;
                return true;
            }
            case "is_superset": {
                const other = deref(args[0]);
                for (const mk of other.set.keys()) if (!s.set.has(mk)) return false;
                return true;
            }
            case "is_disjoint": {
                const other = deref(args[0]);
                for (const mk of s.set.keys()) if (other.set.has(mk)) return false;
                return true;
            }
            case "clone": return cloneVal(s);
        }
        R.panic("unknown HashSet/BTreeSet method `" + name + "`");
    }

    // ---- VecDeque & BinaryHeap methods ----
    function dequeGrow(d) {
        const old = d.buf, cap = old.length, n = d.len, m = cap - 1, h = d.head;
        const nb = new Array(cap << 1);
        for (let i = 0; i < n; i++) nb[i] = old[(h + i) & m];
        d.buf = nb;
        d.head = 0;
    }
    function callDequeMethod(d, name, args) {
        switch (name) {
            case "len": return d.len;
            case "is_empty": return d.len === 0;
            case "clear": d.buf = new Array(8); d.head = 0; d.len = 0; return undefined;
            case "push_back":
                if (d.len === d.buf.length) dequeGrow(d);
                d.buf[(d.head + d.len) & (d.buf.length - 1)] = args[0];
                d.len++;
                return undefined;
            case "push_front":
                if (d.len === d.buf.length) dequeGrow(d);
                d.head = (d.head - 1) & (d.buf.length - 1);
                d.buf[d.head] = args[0];
                d.len++;
                return undefined;
            case "pop_back": {
                if (d.len === 0) return None;
                d.len--;
                const idx = (d.head + d.len) & (d.buf.length - 1);
                const v = d.buf[idx];
                d.buf[idx] = undefined;
                return Some(v);
            }
            case "pop_front": {
                if (d.len === 0) return None;
                const v = d.buf[d.head];
                d.buf[d.head] = undefined;
                d.head = (d.head + 1) & (d.buf.length - 1);
                d.len--;
                return Some(v);
            }
            case "front": return d.len === 0 ? None : Some(d.buf[d.head]);
            case "back": return d.len === 0 ? None : Some(d.buf[(d.head + d.len - 1) & (d.buf.length - 1)]);
            case "get": {
                const i = Number(deref(args[0]));
                return (i >= 0 && i < d.len) ? Some(d.buf[(d.head + i) & (d.buf.length - 1)]) : None;
            }
            case "iter":
            case "into_iter": return intoIter(d);
            case "clone": return cloneVal(d);
        }
        R.panic("unknown VecDeque method `" + name + "`");
    }

    function callHeapMethod(h, name, args) {
        switch (name) {
            case "len": return h.items.length;
            case "is_empty": return h.items.length === 0;
            case "clear": h.items.length = 0; return undefined;
            case "push": {
                h.items.push(args[0]);
                let i = h.items.length - 1;
                while (i > 0) {
                    const p = (i - 1) >> 1;
                    if (cmpVal(h.items[i], h.items[p]) > 0) {
                        const tmp = h.items[i]; h.items[i] = h.items[p]; h.items[p] = tmp;
                        i = p;
                    } else break;
                }
                return undefined;
            }
            case "pop": {
                if (h.items.length === 0) return None;
                const top = h.items[0];
                const last = h.items.pop();
                if (h.items.length > 0) {
                    h.items[0] = last;
                    let i = 0;
                    const n = h.items.length;
                    for (;;) {
                        let largest = i;
                        const l = 2 * i + 1, r = 2 * i + 2;
                        if (l < n && cmpVal(h.items[l], h.items[largest]) > 0) largest = l;
                        if (r < n && cmpVal(h.items[r], h.items[largest]) > 0) largest = r;
                        if (largest === i) break;
                        const tmp = h.items[i]; h.items[i] = h.items[largest]; h.items[largest] = tmp;
                        i = largest;
                    }
                }
                return Some(top);
            }
            case "peek": return h.items.length === 0 ? None : Some(h.items[0]);
            case "into_sorted_vec": {
                const out = h.items.slice().sort(cmpVal);
                h.items.length = 0;
                return { __rs: 7, items: out };
            }
            case "clone": return cloneVal(h);
        }
        R.panic("unknown BinaryHeap method `" + name + "`");
    }

    // ---- Smart pointers, Mutex, RwLock, Cell, RefCell, Atomic ----
    function callSmartMethod(sp, name, args) {
        switch (sp.kind) {
            case "box":
            case "rc":
            case "arc":
                if (name === "clone") return sp.kind === "box" ? { __rs: 14, kind: "box", v: cloneVal(sp.v) } : sp;
                if (name === "as_ref" || name === "deref") return sp.v;
                // Auto-deref method call to inner value!
                return R.callMethod(sp.v, name, args, []);
            case "cell":
                if (name === "get") return sp.v;
                if (name === "set") { sp.v = args[0]; return undefined; }
                if (name === "replace") { const old = sp.v; sp.v = args[0]; return old; }
                if (name === "into_inner") return sp.v;
                break;
            case "refcell":
                if (name === "borrow" || name === "borrow_mut") {
                    return { __rs: 14, kind: "guard", get: function () { return sp.v; }, set: function (x) { sp.v = x; } };
                }
                if (name === "replace") { const old = sp.v; sp.v = args[0]; return old; }
                if (name === "into_inner") return sp.v;
                break;
            case "mutex":
                if (name === "lock") {
                    return Ok({ __rs: 14, kind: "guard", get: function () { return sp.v; }, set: function (x) { sp.v = x; } });
                }
                if (name === "into_inner") return Ok(sp.v);
                break;
            case "tokio_mutex":
                if (name === "lock") {
                    return Promise.resolve({ __rs: 14, kind: "guard", get: function () { return sp.v; }, set: function (x) { sp.v = x; } });
                }
                if (name === "try_lock") {
                    return Ok({ __rs: 14, kind: "guard", get: function () { return sp.v; }, set: function (x) { sp.v = x; } });
                }
                if (name === "into_inner") return sp.v;
                break;
            case "rwlock":
                if (name === "read" || name === "write") {
                    return Ok({ __rs: 14, kind: "guard", get: function () { return sp.v; }, set: function (x) { sp.v = x; } });
                }
                if (name === "into_inner") return Ok(sp.v);
                break;
            case "tokio_rwlock":
                if (name === "read" || name === "write") {
                    return Promise.resolve({ __rs: 14, kind: "guard", get: function () { return sp.v; }, set: function (x) { sp.v = x; } });
                }
                if (name === "try_read" || name === "try_write") {
                    return Ok({ __rs: 14, kind: "guard", get: function () { return sp.v; }, set: function (x) { sp.v = x; } });
                }
                if (name === "into_inner") return sp.v;
                break;
            case "guard":
                if (name === "clone") return cloneVal(sp.get());
                if (name === "deref") return sp.get();
                return R.callMethod(sp.get(), name, args, []);
            case "oncelock":
                if (name === "get") return sp.has ? Some(sp.v) : None;
                if (name === "set") {
                    if (sp.has) return Err(args[0]);
                    sp.v = args[0]; sp.has = true; return Ok(undefined);
                }
                if (name === "get_or_init") {
                    if (!sp.has) { sp.v = args[0](); sp.has = true; }
                    return sp.v;
                }
                break;
            case "lazylock":
                if (!sp.has) { sp.v = sp.init(); sp.has = true; }
                return R.callMethod(sp.v, name, args, []);
            case "atomic":
                if (name === "load") return sp.v;
                if (name === "store") { sp.v = args[0]; return undefined; }
                if (name === "swap") { const old = sp.v; sp.v = args[0]; return old; }
                if (name === "fetch_add") { const old = sp.v; sp.v = R.add(sp.v, args[0]); return old; }
                if (name === "fetch_sub") { const old = sp.v; sp.v = R.sub(sp.v, args[0]); return old; }
                if (name === "compare_exchange") {
                    if (eq(sp.v, args[0])) { const old = sp.v; sp.v = args[1]; return Ok(old); }
                    return Err(sp.v);
                }
                break;
        }
        R.panic("unknown " + sp.kind + " method `" + name + "`");
    }

    // ---- Unified Method Dispatch ----
    R.callMethod = function (rawRecv, name, args, typeArgs) {
        args = args || [];
        typeArgs = typeArgs || [];
        // Keep `rawRecv` before full deref when smart pointers or mut refs own the method directly
        if (rawRecv && typeof rawRecv === "object" && rawRecv.__rs === 14) {
            if (rawRecv.kind !== "box" && rawRecv.kind !== "rc" && rawRecv.kind !== "arc" && rawRecv.kind !== "guard") {
                return callSmartMethod(rawRecv, name, args);
            }
            if (name === "clone" || name === "as_ref") {
                return callSmartMethod(rawRecv, name, args);
            }
        }
        // Check `Option::take` / `Option::replace` which mutate the binding through `rawRecv`
        const v = deref(rawRecv);
        if (v === undefined || v === null) {
            if (name === "clone") return undefined;
            R.panic("method `" + name + "` called on unit `()`");
        }
        const t = typeof v;
        if (t === "string") return callStrMethod(v, name, args, typeArgs);
        if (t === "number" || t === "bigint") {
            if (name === "into_response") return rt.mkResponse(Number(v) | 0, "", null);
            if (name === "as_u16") return Number(v) & 0xFFFF;
            return callNumMethod(v, name, args);
        }
        if (t === "boolean") {
            if (name === "then") return v ? Some(args[0]()) : None;
            if (name === "then_some") return v ? Some(args[0]) : None;
            if (name === "to_string") return R.mkString(v ? "true" : "false");
            if (name === "clone") return v;
        }
        if (t === "object") {
            switch (v.__rs) {
                case 1: {
                    const m = rt.lookupUserMethod(v.__type, name);
                    if (m) {
                        const selfArg = m.__rs_mut_self ? rawRecv : v;
                        return m.apply(null, [selfArg].concat(args));
                    }
                    if (name === "clone") return cloneVal(v);
                    if (name === "to_string") return R.mkString(rt.fmtDisplay(v));
                    if (name === "into") return R.convertInto(v, typeArgs && typeArgs[0]);
                    if (name === "as_ref") return v;
                    if (name === "into_response" && rt.intoResponse) return rt.intoResponse(v);
                    if (name === "eq") return eq(v, args[0]);
                    if (name === "cmp") return rt.mkOrdering(cmpVal(v, args[0]));
                    if (name === "partial_cmp") return Some(rt.mkOrdering(cmpVal(v, args[0])));
                    // Inherit ALL Iterator adapters/consumers when struct implements `next(&mut self)`!
                    if (rt.lookupUserMethod(v.__type, "next")) {
                        return callIterMethod(intoIter(rawRecv), name, args, typeArgs);
                    }
                    // Also allow calling a closure field `(obj.cb)(args)` if field is a function
                    if (name === "into") return R.convertInto(v, typeArgs && typeArgs[0]);
                    if (name === "as_ref") return v;
                    if (typeof v[name] === "function") return v[name].apply(null, args);
                    R.panic("no method `" + name + "` found for struct `" + v.__type + "`");
                }
                case 2: {
                    if (v.__enum === "Option") return callOptionMethod(v, name, args, rawRecv, typeArgs);
                    if (v.__enum === "Result") return callResultMethod(v, name, args, typeArgs);
                    if (v.__sqlx) {
                        if (name === "as_database_error") return v.dbErr ? Some(v.dbErr) : None;
                        if (name === "to_string") return R.mkString(v.msg || "database error");
                    }
                    if (v.__enum === "Method") {
                        if (name === "as_str") return v.__var;
                        if (name === "to_string") return R.mkString(v.__var);
                        if (name === "clone") return v;
                    }
                    const m = rt.lookupUserMethod(v.__enum, name);
                    if (m) {
                        const selfArg = m.__rs_mut_self ? rawRecv : v;
                        return m.apply(null, [selfArg].concat(args));
                    }
                    if (name === "clone") return cloneVal(v);
                    if (name === "to_string") return R.mkString(rt.fmtDisplay(v));
                    if (name === "into") return R.convertInto(v, typeArgs && typeArgs[0]);
                    if (name === "as_ref") return v;
                    if (name === "into_response" && rt.intoResponse) return rt.intoResponse(v);
                    if (name === "eq") return eq(v, args[0]);
                    if (rt.lookupUserMethod(v.__enum, "next")) {
                        return callIterMethod(intoIter(rawRecv), name, args, typeArgs);
                    }
                    R.panic("no method `" + name + "` found for enum `" + v.__enum + "`");
                }
                case 3:
                    if (name === "clone") return cloneVal(v);
                    if (name === "into") return R.convertInto(v, typeArgs && typeArgs[0]);
                    if (name === "as_ref") return v;
                    if (name === "into_response" && rt.intoResponse) return rt.intoResponse(v);
                    break;
                case 4: return callStrMethod(v, name, args, typeArgs);
                case 5: return callCharMethod(v, name, args);
                case 7: return callVecMethod(v, name, args);
                case 8: return callIterMethod(v, name, args, typeArgs);
                case 9:
                    if (name === "contains") {
                        const x = deref(args[0]);
                        return R.patRangeMatch(x, v.start === null ? undefined : v.start, v.end === null ? undefined : v.end, v.inclusive);
                    }
                    if (name === "rev" || name === "map" || name === "filter" || name === "collect"
                        || name === "sum" || name === "product" || name === "count" || name === "fold"
                        || name === "enumerate" || name === "step_by" || name === "take" || name === "skip"
                        || name === "any" || name === "all" || name === "find" || name === "for_each"
                        || name === "zip" || name === "chain") {
                        return callIterMethod(intoIter(v), name, args, typeArgs);
                    }
                    if (name === "into_iter") return intoIter(v);
                    if (name === "is_empty") return v.start !== null && v.end !== null && (v.inclusive ? cmpVal(v.start, v.end) > 0 : cmpVal(v.start, v.end) >= 0);
                    break;
                case 10: return callMapMethod(v, name, args);
                case 11: return callSetMethod(v, name, args);
                case 12: return callDequeMethod(v, name, args);
                case 13: return callHeapMethod(v, name, args);
                case 14: return callSmartMethod(v, name, args);
                case 15: return rt.callStdMethod(v, name, args, typeArgs);
            }
        }
        // Fallback for universal traits: `.into()`, `.try_into()`, `.to_string()`, `.clone()`
        if (name === "into") {
            const dst = typeArgs && typeArgs[0] ? typeArgs[0] : "";
            return dst ? R.convertInto(rawRecv, dst) : v;
        }
        if (name === "try_into") {
            const dst = typeArgs && typeArgs[0] ? typeArgs[0] : "";
            if (dst) {
                const c = rt.tryConvertTryFrom(dst, rawRecv);
                if (c !== rt.CONVERT_MISS) return c;
            }
            return Ok(v);
        }
        if (name === "to_string") return R.mkString(rt.fmtDisplay(v));
        if (name === "clone") return cloneVal(v);
        if (name === "as_ref") return v;
        R.panic("no method `" + name + "` found on value of type `" + rt.typeTagOf(v) + "`");
    };
})(__zipp_rs);
