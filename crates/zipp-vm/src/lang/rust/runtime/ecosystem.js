/* ZIPP Rust runtime — production backend ecosystem:
 * sqlx (native SQLite), axum 0.7 (extractors, middleware, serve), tower_http,
 * tokio (sync channels, async locks, timers, spawn/spawn_blocking, TcpListener),
 * tracing_subscriber, argon2, jsonwebtoken, time, uuid, sha2, hmac, base64, hex,
 * rand, std::net, fs2, reqwest, anyhow, and safe OS-API fallbacks. Apache-2.0.
 */
(function (R) {
    "use strict";
    const rt = R.__rt;
    const deref = rt.deref;
    const strVal = rt.strVal;
    const cloneVal = rt.cloneVal;
    const Some = R.Some;
    const None = R.None;
    const Ok = R.Ok;
    const Err = R.Err;
    const ECO_MISS = { __rs_eco_miss: true };
    rt.ECO_MISS = ECO_MISS;

    // ---- Native SQLite & Crypto helpers ----
    function sqliteCall(op, a, b, c) {
        if (typeof __zipp_rs_sqlite !== "function") {
            return JSON.stringify({ error: "SQLite native support unavailable in this build" });
        }
        return __zipp_rs_sqlite(
            op,
            a === undefined ? "" : String(a),
            b === undefined ? "" : String(b),
            c === undefined ? "" : String(c)
        );
    }

    function bytesToHex(val) {
        const v = deref(val);
        let bytes;
        if (v && v.__rs === 7) bytes = v.items;
        else if (v && v.__rs === 15 && v.kind === "digest_bytes") return v.hex;
        else bytes = rt.strToUtf8Bytes(strVal(v));
        let out = "";
        for (let i = 0; i < bytes.length; i++) {
            out += (Number(deref(bytes[i])) & 0xFF).toString(16).padStart(2, "0");
        }
        return out;
    }

    function hexToBytes(hex) {
        const out = [];
        for (let i = 0; i + 1 < hex.length; i += 2) {
            out.push(parseInt(hex.slice(i, i + 2), 16));
        }
        return out;
    }

    function bytesToStr(val) {
        const v = deref(val);
        if (v && v.__rs === 7) return rt.utf8BytesToStr(v.items);
        return strVal(v);
    }

    // ---- Cooperative Timer Queue & Spawn Resource Tracking ----
    const timers = [];
    const recentSpawnResources = [];
    let spawnClearScheduled = false;
    function trackSpawnResource(res) {
        recentSpawnResources.push(res);
        if (!spawnClearScheduled) {
            spawnClearScheduled = true;
            Promise.resolve().then(function () {
                spawnClearScheduled = false;
                recentSpawnResources.length = 0;
            });
        }
    }
    function scheduleTimer(ms) {
        const delay = Math.max(0, Number(ms) || 0);
        const deadline = Date.now() + delay;
        let entry = null;
        const p = new Promise(function (resolve) {
            entry = { deadline: deadline, resolve: resolve, cancelled: false };
            timers.push(entry);
        });
        p.__cancel = function () {
            if (!entry || entry.cancelled) return;
            entry.cancelled = true;
            const idx = timers.indexOf(entry);
            if (idx >= 0) timers.splice(idx, 1);
        };
        return p;
    }
    function pollTimers() {
        if (timers.length === 0) return 0;
        const now = Date.now();
        let fired = 0;
        // Collect due timers first (FIFO order) so callbacks that re-schedule don't disturb iteration.
        const due = [];
        for (let i = 0; i < timers.length; ) {
            if (timers[i].deadline <= now) {
                due.push(timers.splice(i, 1)[0]);
            } else {
                i++;
            }
        }
        for (let i = 0; i < due.length; i++) {
            if (!due[i].cancelled) {
                fired++;
                due[i].resolve(undefined);
            }
        }
        return fired;
    }
    function nextTimerMs() {
        if (timers.length === 0) return null;
        const now = Date.now();
        let best = Infinity;
        for (let i = 0; i < timers.length; i++) {
            const rem = timers[i].deadline - now;
            if (rem < best) best = rem;
        }
        return best < 0 ? 0 : best;
    }
    rt.pollTimers = pollTimers;
    rt.nextTimerMs = nextTimerMs;

    // ---- Virtual Axum WebSockets ----
    const wsSockets = new Map();
    const wsOutbox = [];

    function pushWsFrame(wsId, frame) {
        wsOutbox.push(Object.assign({ wsId: String(wsId) }, frame));
    }

    function sendWsMessage(ws, msgVal) {
        if (!ws || ws.closed) {
            return Promise.resolve(Err(R.mkString("websocket closed")));
        }
        const m = deref(msgVal);
        if (m && m.__rs === 2) {
            const variant = m.__var;
            const d0 = m.__data ? m.__data[0] : undefined;
            if (variant === "Text") {
                pushWsFrame(ws.id, { type: "text", data: strVal(d0) });
                return Promise.resolve(Ok(undefined));
            }
            if (variant === "Binary") {
                pushWsFrame(ws.id, { type: "binary", data: bytesToHex(d0) });
                return Promise.resolve(Ok(undefined));
            }
            if (variant === "Ping") {
                pushWsFrame(ws.id, { type: "ping", data: bytesToHex(d0) });
                return Promise.resolve(Ok(undefined));
            }
            if (variant === "Pong") {
                pushWsFrame(ws.id, { type: "pong", data: bytesToHex(d0) });
                return Promise.resolve(Ok(undefined));
            }
            if (variant === "Close") {
                let code = 1000;
                let reason = "";
                const opt = deref(d0);
                if (opt && opt.__rs === 2 && opt.__var === "Some") {
                    const cf = deref(opt.__data[0]);
                    if (cf && typeof cf === "object") {
                        if (cf.code !== undefined) code = Number(deref(cf.code)) | 0;
                        if (cf.reason !== undefined) reason = strVal(cf.reason);
                    }
                }
                closeWsInternal(ws, code, reason, true);
                return Promise.resolve(Ok(undefined));
            }
        }
        pushWsFrame(ws.id, { type: "text", data: strVal(m) });
        return Promise.resolve(Ok(undefined));
    }

    function closeWsInternal(ws, code, reason, notifyClient) {
        if (!ws || ws.closed) return;
        ws.closed = true;
        wsSockets.delete(String(ws.id));
        const waiters = ws.inWaiters.splice(0);
        for (let i = 0; i < waiters.length; i++) {
            waiters[i](None);
        }
        if (notifyClient) {
            pushWsFrame(ws.id, { type: "close", code: code || 1000, reason: reason || "" });
        }
    }

    function recvWsMessage(ws) {
        if (!ws) return Promise.resolve(None);
        if (ws.inQueue.length > 0) {
            return Promise.resolve(ws.inQueue.shift());
        }
        if (ws.closed) {
            return Promise.resolve(None);
        }
        trackSpawnResource({ kind: "ws_receiver", ws: ws });
        let waiter = null;
        const p = new Promise(function (resolve) {
            waiter = resolve;
            ws.inWaiters.push(waiter);
        });
        p.__cancel = function () {
            const idx = ws.inWaiters.indexOf(waiter);
            if (idx >= 0) ws.inWaiters.splice(idx, 1);
        };
        return p;
    }

    rt.wsStartUpgrade = function (ws) {
        if (!ws || !ws.onUpgradeFn) return;
        wsSockets.set(String(ws.id), ws);
        const socketObj = { __rs: 15, kind: "ws_socket", ws: ws };
        Promise.resolve().then(function () {
            return ws.onUpgradeFn(socketObj);
        }).then(
            function () {
                closeWsInternal(ws, 1000, "", true);
            },
            function (err) {
                if (typeof rt.writeStderr === "function") {
                    const msg = err && err.isRsPanic ? err.message : String(err);
                    rt.writeStderr("[axum ws] " + msg + "\n");
                }
                closeWsInternal(ws, 1011, "internal error", true);
            }
        );
    };

    rt.wsReceiveFromClient = function (wsId, data, binary) {
        const ws = wsSockets.get(String(wsId));
        if (!ws || ws.closed) return false;
        const msg = binary
            ? R.mkEnumTuple("Message", "Binary", [{ __rs: 7, items: hexToBytes(String(data || "")) }])
            : R.mkEnumTuple("Message", "Text", [R.mkString(String(data === undefined ? "" : data))]);
        const item = Some(Ok(msg));
        if (ws.inWaiters.length > 0) {
            ws.inWaiters.shift()(item);
        } else {
            ws.inQueue.push(item);
        }
        return true;
    };

    rt.wsCloseFromClient = function (wsId, code, reason) {
        const ws = wsSockets.get(String(wsId));
        if (!ws) return false;
        if (!ws.closed) {
            const cf = R.mkStruct("CloseFrame", { code: Number(code) || 1000, reason: R.mkString(String(reason || "")) });
            const msg = R.mkEnumTuple("Message", "Close", [Some(cf)]);
            const item = Some(Ok(msg));
            if (ws.inWaiters.length > 0) {
                ws.inWaiters.shift()(item);
            } else {
                ws.inQueue.push(item);
            }
            closeWsInternal(ws, code || 1000, reason || "", false);
        }
        return true;
    };

    rt.wsDrainOutbox = function () {
        if (wsOutbox.length === 0) return [];
        return wsOutbox.splice(0);
    };

    // ---- std::net::IpAddr & SocketAddr ----
    function mkIpAddr(ip) {
        return { __rs: 15, kind: "ip_addr", ip: String(ip) };
    }
    function mkSocketAddr(ip, port) {
        return { __rs: 15, kind: "socket_addr", ip: String(ip), port: Number(port) | 0 };
    }
    function parseIpAddr(s, target) {
        const t = String(s).trim();
        const isV4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.test(t);
        if (isV4) {
            const parts = t.split(".").map(Number);
            if (parts.every(function (n) { return n >= 0 && n <= 255; })) {
                if (target === "Ipv6Addr") return Err(R.mkString("invalid IPv6 address: " + t));
                return Ok(mkIpAddr(t));
            }
            return Err(R.mkString("invalid IPv4 address: " + t));
        }
        if (t.indexOf(":") >= 0) {
            if (target === "Ipv4Addr") return Err(R.mkString("invalid IPv4 address: " + t));
            return Ok(mkIpAddr(t));
        }
        return Err(R.mkString("invalid IP address: " + t));
    }
    function parseSocketAddr(s) {
        const t = String(s).trim();
        let host = "", portStr = "";
        if (t.startsWith("[")) {
            const close = t.indexOf("]:");
            if (close < 0) return Err(R.mkString("invalid socket address: " + t));
            host = t.slice(1, close);
            portStr = t.slice(close + 2);
        } else {
            const col = t.lastIndexOf(":");
            if (col < 0) return Err(R.mkString("invalid socket address: " + t));
            host = t.slice(0, col);
            portStr = t.slice(col + 1);
        }
        if (!/^\d+$/.test(portStr)) return Err(R.mkString("invalid port in socket address: " + t));
        const port = Number(portStr);
        if (port < 0 || port > 65535) return Err(R.mkString("port out of range: " + t));
        const ipRes = parseIpAddr(host, "IpAddr");
        if (ipRes.__var !== "Ok") return Err(R.mkString("invalid socket address: " + t));
        return Ok(mkSocketAddr(host, port));
    }
    function ipIsLoopback(ip) {
        return ip === "127.0.0.1" || ip.startsWith("127.") || ip === "::1" || ip === "0:0:0:0:0:0:0:1";
    }
    function ipIsPrivate(ip) {
        if (ip.startsWith("10.")) return true;
        if (ip.startsWith("192.168.")) return true;
        const m = /^172\.(\d+)\./.exec(ip);
        if (m) { const b = Number(m[1]); return b >= 16 && b <= 31; }
        return false;
    }
    function ipIsLinkLocal(ip) {
        return ip.startsWith("169.254.") || ip.toLowerCase().startsWith("fe80:");
    }
    rt.mkIpAddr = mkIpAddr;
    rt.mkSocketAddr = mkSocketAddr;
    rt.parseIpAddr = parseIpAddr;
    rt.parseSocketAddr = parseSocketAddr;

    // ---- UUID ----
    function mkUuid(s) {
        return { __rs: 15, kind: "uuid", s: String(s) };
    }
    rt.parseUuid = function (s) {
        const t = String(s).trim().toLowerCase();
        if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(t) || /^[0-9a-f]{32}$/.test(t)) {
            return Ok(mkUuid(t));
        }
        return Err(R.mkString("invalid UUID: " + s));
    };

    // ---- time::OffsetDateTime & formatting ----
    function mkOffsetDateTime(ns) {
        return { __rs: 15, kind: "offset_datetime", ns: BigInt(ns) };
    }
    function datePartsUtc(ns) {
        const ms = Number(BigInt(ns) / 1000000n);
        const d = new Date(ms);
        return {
            year: d.getUTCFullYear(),
            month: d.getUTCMonth() + 1,
            day: d.getUTCDate(),
            hour: d.getUTCHours(),
            minute: d.getUTCMinutes(),
            second: d.getUTCSeconds(),
            ms: d.getUTCMilliseconds()
        };
    }
    function pad2(n) { return String(n).padStart(2, "0"); }
    function pad4(n) { return String(n).padStart(4, "0"); }
    function formatOffsetDateTime(ns, fmtDesc) {
        const p = datePartsUtc(ns);
        const fmt = fmtDesc ? strVal(fmtDesc) : "rfc3339";
        if (fmt === "rfc3339" || fmt === "Rfc3339") {
            return pad4(p.year) + "-" + pad2(p.month) + "-" + pad2(p.day) + "T" +
                pad2(p.hour) + ":" + pad2(p.minute) + ":" + pad2(p.second) + "Z";
        }
        return fmt
            .replace(/\[year[^\]]*\]/g, pad4(p.year))
            .replace(/\[month[^\]]*\]/g, pad2(p.month))
            .replace(/\[day[^\]]*\]/g, pad2(p.day))
            .replace(/\[hour[^\]]*\]/g, pad2(p.hour))
            .replace(/\[minute[^\]]*\]/g, pad2(p.minute))
            .replace(/\[second[^\]]*\]/g, pad2(p.second))
            .replace(/\[subsecond[^\]]*\]/g, String(p.ms).padStart(3, "0"))
            .replace(/\[offset_hour[^\]]*\]/g, "+00")
            .replace(/\[offset_minute[^\]]*\]/g, "00");
    }

    // ---- sqlx (SQLite over __zipp_rs_sqlite) ----
    function mkSqlxRowNotFound() {
        return {
            __rs: 2,
            __enum: "Error",
            __var: "RowNotFound",
            __kind: 0,
            __disc: null,
            __sqlx: true,
            msg: "no rows returned by a query that expected to return at least one row",
            dbErr: null
        };
    }
    function mkSqlxDbErr(msg, code, unique) {
        const dbErr = {
            __rs: 15,
            kind: "sqlx_db_error",
            msg: String(msg),
            code: code ? String(code) : "",
            unique: !!unique
        };
        return {
            __rs: 2,
            __enum: "Error",
            __var: "Database",
            __kind: 1,
            __data: [dbErr],
            __sqlx: true,
            msg: "error returned from database: " + String(msg),
            dbErr: dbErr
        };
    }

    function encodeSqlParam(v) {
        v = deref(v);
        if (v === null || v === undefined) return null;
        const t = typeof v;
        if (t === "boolean") return v ? 1 : 0;
        if (t === "number") return Number.isFinite(v) ? v : null;
        if (t === "bigint") return Number(v);
        if (t === "string") return v;
        if (v.__rs === 4) return v.s;
        if (v.__rs === 5) return v.ch;
        if (v.__rs === 2 && v.__enum === "Option") {
            return v.__var === "Some" ? encodeSqlParam(v.__data[0]) : null;
        }
        if (v.__rs === 7) return { __blob: v.items.map(function (b) { return Number(deref(b)) & 0xFF; }) };
        if (v.__rs === 15) {
            if (v.kind === "json") return JSON.stringify(v.v);
            if (v.kind === "path") return v.p;
            if (v.kind === "uuid") return v.s;
            if (v.kind === "offset_datetime" || v.kind === "datetime") return formatOffsetDateTime(v.ns, "rfc3339");
            if (v.kind === "argon2_hash") return v.s;
        }
        return rt.fmtDisplay(v);
    }

    function unwrapOuterType(desc, wrapper) {
        if (!desc) return "";
        const d = desc.trim();
        if (d.startsWith(wrapper + "<") && d.endsWith(">")) {
            return d.slice(wrapper.length + 1, -1).trim();
        }
        return d;
    }

    function splitTupleDesc(desc) {
        if (!desc) return [];
        const d = desc.trim();
        if (!d.startsWith("(") || !d.endsWith(")")) return [d];
        const inner = d.slice(1, -1);
        const parts = [];
        let depth = 0, cur = "";
        for (let i = 0; i < inner.length; i++) {
            const c = inner[i];
            if (c === "(" || c === "<") { depth++; cur += c; }
            else if (c === ")" || c === ">") { depth--; cur += c; }
            else if (c === "," && depth === 0) { parts.push(cur.trim()); cur = ""; }
            else cur += c;
        }
        if (cur.trim()) parts.push(cur.trim());
        return parts;
    }

    function decodeSqlCell(raw, typeDesc) {
        const desc = (typeDesc || "").trim();
        const td = rt.parseTypeDesc(desc);
        const base = td.base;
        if (base === "Option") {
            if (raw === null || raw === undefined) return None;
            return Some(decodeSqlCell(raw, td.args[0] || ""));
        }
        if (raw === null || raw === undefined) {
            if (base === "String") return R.mkString("");
            if (base === "bool") return false;
            if (/^[iu](8|16|32|64|128|size)$|^f(32|64)$/.test(base)) return 0;
            if (base === "Value" || base === "serde_json::Value") return R.mkJson(null);
            return null;
        }
        if (raw && typeof raw === "object" && Array.isArray(raw.__blob)) {
            if (base === "String") return R.mkString(rt.utf8BytesToStr(raw.__blob));
            return { __rs: 7, items: raw.__blob };
        }
        if (base === "bool") {
            if (typeof raw === "boolean") return raw;
            if (typeof raw === "number") return raw !== 0;
            if (typeof raw === "string") return raw !== "" && raw !== "0" && raw.toLowerCase() !== "false";
            return !!raw;
        }
        if (/^[iu](8|16|32|64|size)$/.test(base)) {
            return typeof raw === "number" ? Math.trunc(raw) : (parseInt(String(raw), 10) || 0);
        }
        if (base === "i128" || base === "u128") {
            return BigInt(Math.trunc(Number(raw) || 0));
        }
        if (base === "f32" || base === "f64") {
            return typeof raw === "number" ? raw : (parseFloat(String(raw)) || 0);
        }
        if (base === "String") {
            return R.mkString(String(raw));
        }
        if (base === "Value" || base === "serde_json::Value") {
            if (typeof raw === "string") {
                try { return R.mkJson(JSON.parse(raw)); } catch (_) { return R.mkJson(raw); }
            }
            return R.mkJson(raw);
        }
        if (base === "Uuid") return mkUuid(String(raw));
        // Untyped fallback: wrap strings as Rust String, keep numbers as numbers
        if (typeof raw === "string") return R.mkString(raw);
        return raw;
    }

    function decodeSqlRow(columns, rawRow, mode, rowType) {
        if (mode === "raw") {
            return { __rs: 15, kind: "sqlx_row", columns: columns, values: rawRow };
        }
        if (mode === "scalar") {
            return decodeSqlCell(rawRow[0], rowType);
        }
        const rtDesc = (rowType || "").trim();
        if (rtDesc.startsWith("(") && rtDesc.endsWith(")")) {
            const elemDescs = splitTupleDesc(rtDesc);
            const items = [];
            for (let i = 0; i < elemDescs.length; i++) {
                items.push(decodeSqlCell(rawRow[i], elemDescs[i]));
            }
            return R.mkTuple(items);
        }
        const base = rtDesc.split("<")[0].trim();
        if (base && rt.structs[base]) {
            const st = rt.structs[base];
            const colMap = Object.create(null);
            for (let i = 0; i < columns.length; i++) colMap[columns[i]] = i;
            if (st.isTuple) {
                const items = [];
                for (let i = 0; i < st.fields.length; i++) {
                    items.push(decodeSqlCell(rawRow[i], st.fields[i][1]));
                }
                return R.mkTupleStruct(base, items);
            }
            const obj = { __rs: 1, __type: base, __tuple: false };
            for (let i = 0; i < st.fields.length; i++) {
                const fname = st.fields[i][0];
                const ftype = st.fields[i][1];
                const cidx = Object.prototype.hasOwnProperty.call(colMap, fname) ? colMap[fname] : i;
                obj[fname] = decodeSqlCell(rawRow[cidx], ftype);
            }
            return obj;
        }
        if (rawRow.length === 1) return decodeSqlCell(rawRow[0], rtDesc);
        return R.mkTuple(rawRow.map(function (v) { return decodeSqlCell(v, ""); }));
    }

    function resolveSqlConnId(execArg) {
        const ex = deref(execArg);
        if (!ex || typeof ex !== "object") return -1;
        if (ex.__rs === 15 && (ex.kind === "sqlite_pool" || ex.kind === "sqlite_tx" || ex.kind === "sqlite_conn")) {
            return ex.connId;
        }
        return -1;
    }

    function openSqlitePool(opts) {
        const url = opts.url || ":memory:";
        const create = opts.createIfMissing !== false ? "1" : "0";
        const raw = sqliteCall("open", url, create, "");
        let parsed;
        try { parsed = JSON.parse(raw); } catch (_) { parsed = { error: String(raw) }; }
        if (parsed.error || !(parsed.id >= 0)) {
            return Err(mkSqlxDbErr(parsed.error || "failed to open sqlite", parsed.code, parsed.unique));
        }
        const connId = parsed.id;
        const execPragma = function (sql) {
            sqliteCall("execute", String(connId), sql, "[]");
        };
        if (opts.journalMode) execPragma("PRAGMA journal_mode = " + opts.journalMode);
        if (opts.synchronous) execPragma("PRAGMA synchronous = " + opts.synchronous);
        if (opts.foreignKeys !== undefined) execPragma("PRAGMA foreign_keys = " + (opts.foreignKeys ? "ON" : "OFF"));
        if (opts.busyTimeoutMs !== undefined) execPragma("PRAGMA busy_timeout = " + Math.trunc(opts.busyTimeoutMs));
        if (opts.pragmas) {
            for (let i = 0; i < opts.pragmas.length; i++) {
                execPragma("PRAGMA " + opts.pragmas[i][0] + " = " + opts.pragmas[i][1]);
            }
        }
        return Ok({
            __rs: 15,
            kind: "sqlite_pool",
            connId: connId,
            closed: false,
            txDepth: 0
        });
    }

    // ---- Axum Extractors & Server ----
    function coerceQueryScalar(str, typeDesc) {
        const td = rt.parseTypeDesc(typeDesc || "");
        const base = td.base;
        if (base === "Option") {
            if (str === undefined || str === null) return None;
            return Some(coerceQueryScalar(str, td.args[0] || ""));
        }
        if (base === "bool") return str === "true" || str === "1" || str === "yes";
        if (/^[iu](8|16|32|64|size)$/.test(base)) {
            const n = parseInt(str, 10);
            return Number.isNaN(n) ? 0 : n;
        }
        if (base === "f32" || base === "f64") {
            const f = parseFloat(str);
            return Number.isNaN(f) ? 0 : f;
        }
        if (base === "String") return R.mkString(str);
        const enShort = base && base.indexOf("::") >= 0 ? base.slice(base.lastIndexOf("::") + 2) : base;
        if (base && (rt.enums[base] || rt.enums[enShort])) {
            return rt.fromJsonRaw(str, typeDesc);
        }
        return R.mkString(str);
    }

    function fromQueryMap(qmap, typeDesc) {
        const td = rt.parseTypeDesc(typeDesc || "");
        const base = td.base;
        const rawObj = {};
        const entries = rt.mapEntries(qmap);
        for (let i = 0; i < entries.length; i++) {
            rawObj[strVal(entries[i][0])] = strVal(entries[i][1]);
        }
        if (base === "HashMap" || base === "BTreeMap") {
            return qmap;
        }
        const shortBase = base && base.indexOf("::") >= 0 ? base.slice(base.lastIndexOf("::") + 2) : base;
        const stLookup = base ? (rt.structs[base] || rt.structs[shortBase]) : null;
        if (stLookup) {
            const st = stLookup;
            const cMeta = st.serde || null;
            const fMetas = (cMeta && cMeta.fields) ? cMeta.fields : null;
            const obj = { __rs: 1, __type: st.shortName || shortBase, __tuple: false };
            for (let i = 0; i < st.fields.length; i++) {
                const fname = st.fields[i][0];
                const ftype = st.fields[i][1];
                const fm = fMetas ? fMetas[fname] : null;
                const qkey = (fm && fm.rename) ? fm.rename : fname;
                if (Object.prototype.hasOwnProperty.call(rawObj, qkey)) {
                    obj[fname] = coerceQueryScalar(rawObj[qkey], ftype);
                } else if (Object.prototype.hasOwnProperty.call(rawObj, fname)) {
                    obj[fname] = coerceQueryScalar(rawObj[fname], ftype);
                } else if (fm && fm.alias && Object.prototype.hasOwnProperty.call(rawObj, fm.alias)) {
                    obj[fname] = coerceQueryScalar(rawObj[fm.alias], ftype);
                } else if (fm && typeof fm.default_fn === "function") {
                    obj[fname] = fm.default_fn();
                } else {
                    obj[fname] = rt.defaultOf(rt.parseTypeDesc(ftype).base);
                }
            }
            return obj;
        }
        return rt.mkJson(rawObj);
    }

    function decodePathParams(list, typeDesc) {
        const d = (typeDesc || "").trim();
        if (d.startsWith("(") && d.endsWith(")")) {
            const parts = splitTupleDesc(d);
            const items = [];
            for (let i = 0; i < parts.length; i++) {
                items.push(coerceQueryScalar(list[i] || "", parts[i]));
            }
            return R.mkTuple(items);
        }
        return coerceQueryScalar(list[0] || "", d);
    }

    async function extractOneAxumParam(kind, inner, req, state, nextObj) {
        if (kind.startsWith("Option:")) {
            const subKind = kind.slice(7);
            try {
                if (subKind === "Json" && (!req.body || !req.body.trim())) return None;
                if (subKind === "Extension") {
                    const key = inner.split("<")[0].trim();
                    const skey = key.indexOf("::") >= 0 ? key.slice(key.lastIndexOf("::") + 2) : key;
                    if (!req.extensions || (!Object.prototype.hasOwnProperty.call(req.extensions, key) && !Object.prototype.hasOwnProperty.call(req.extensions, skey))) return None;
                }
                return Some(await extractOneAxumParam(subKind, inner, req, state, nextObj));
            } catch (_) {
                return None;
            }
        }
        switch (kind) {
            case "State": return R.mkTupleStruct("State", [state]);
            case "Path": return R.mkTupleStruct("Path", [decodePathParams(req.pathParamList || [], inner)]);
            case "Query": return R.mkTupleStruct("Query", [fromQueryMap(req.queryMap, inner)]);
            case "Json": {
                const raw = JSON.parse(req.body || "null");
                return R.mkTupleStruct("Json", [rt.fromJsonRaw(raw, inner)]);
            }
            case "Extension": {
                const key = inner.split("<")[0].trim();
                const skey = key.indexOf("::") >= 0 ? key.slice(key.lastIndexOf("::") + 2) : key;
                const ext = req.extensions
                    ? (Object.prototype.hasOwnProperty.call(req.extensions, key) ? req.extensions[key] : req.extensions[skey])
                    : undefined;
                return R.mkTupleStruct("Extension", [ext]);
            }
            case "ConnectInfo": {
                const addr = req.peerAddr || mkSocketAddr("127.0.0.1", 0);
                return R.mkTupleStruct("ConnectInfo", [addr]);
            }
            case "HeaderMap": return { __rs: 15, kind: "header_map", map: req.headers };
            case "Request": return req;
            case "Next": return nextObj;
            case "Method": return R.mkEnumUnit("Method", req.method, null);
            case "Uri":
            case "OriginalUri": return { __rs: 15, kind: "uri", raw: req.uri, path: req.path, query: req.queryStr };
            case "RawQuery": return R.mkTupleStruct("RawQuery", [req.queryStr ? Some(R.mkString(req.queryStr)) : None]);
            case "Body": return inner === "Bytes" ? { __rs: 7, items: rt.strToUtf8Bytes(req.body) } : R.mkString(req.body);
            case "WebSocketUpgrade": return { __rs: 15, kind: "ws_upgrade", req: req };
            default: {
                const typeStr = (kind === "Unknown" ? inner : kind) || "";
                const base = typeStr.split("<")[0].trim();
                const shortBase = base.indexOf("::") >= 0 ? base.slice(base.lastIndexOf("::") + 2) : base;
                const fromParts = (rt.lookupTraitMethod && (rt.lookupTraitMethod("FromRequestParts", base, "from_request_parts") || rt.lookupTraitMethod("FromRequestParts", shortBase, "from_request_parts")))
                    || (rt.lookupUserMethod && (rt.lookupUserMethod(base, "from_request_parts") || rt.lookupUserMethod(shortBase, "from_request_parts")));
                if (fromParts) {
                    const parts = {
                        __rs: 15,
                        kind: "request_parts",
                        req: req,
                        headers: { __rs: 15, kind: "header_map", map: req.headers },
                        method: R.mkEnumUnit("Method", req.method, null),
                        uri: { __rs: 15, kind: "uri", raw: req.uri, path: req.path, query: req.queryStr },
                        extensions: { __rs: 15, kind: "extensions", store: req.extensions },
                        queryMap: req.queryMap
                    };
                    const res = await fromParts(parts, state);
                    const u = deref(res);
                    if (u && u.__rs === 2 && u.__enum === "Result") {
                        if (u.__var === "Ok") return u.__data[0];
                        throw { __axum_rejection: u.__data[0] };
                    }
                    return res;
                }
                return req;
            }
        }
    }

    rt.invokeAxumHandler = async function (fn, req, state, nextObj) {
        const specs = fn.__rs_params;
        const args = [];
        for (let i = 0; i < specs.length; i++) {
            try {
                args.push(await extractOneAxumParam(specs[i][0], specs[i][1], req, state, nextObj));
            } catch (e) {
                if (e && Object.prototype.hasOwnProperty.call(e, "__axum_rejection")) {
                    return rt.coerceHandlerValue ? rt.coerceHandlerValue(e.__axum_rejection) : rt.mkResponse(400, String(e.__axum_rejection), "text/plain");
                }
                return rt.mkResponse(
                    400,
                    JSON.stringify({ title: "bad_request", detail: "failed to extract " + specs[i][0] + ": " + String(e) }),
                    "application/json; charset=utf-8"
                );
            }
        }
        return await fn.apply(null, args);
    };

    async function runAxumServe(serveObj) {
        if (rt.__inWorkerBoot || (serveObj.listener && serveObj.listener.virtual)) {
            rt.setRouter(serveObj.router);
            rt.__workerBootReady = true;
            if (serveObj.shutdownFut && typeof serveObj.shutdownFut.then === "function") {
                await serveObj.shutdownFut;
                return Ok(undefined);
            }
            return new Promise(function () {});
        }
        const lid = serveObj.listener.id;
        rt.netCall("set_nonblocking", String(lid), "1");
        let stopped = false;
        if (serveObj.shutdownFut && typeof serveObj.shutdownFut.then === "function") {
            serveObj.shutdownFut.then(function () { stopped = true; }, function () { stopped = true; });
        }
        while (!stopped) {
            pollTimers();
            if (typeof __zipp_rs_io === "function") __zipp_rs_io("drain_microtasks", "");
            if (stopped) break;
            const acc = rt.netCall("try_accept", String(lid));
            if (acc === null || acc === undefined) break;
            if (acc === "") {
                rt.timeCall("sleep_ms", 2);
                continue;
            }
            const sep = acc.indexOf("|");
            const sid = Number(sep >= 0 ? acc.slice(0, sep) : acc);
            const peerStr = sep >= 0 ? acc.slice(sep + 1) : "127.0.0.1:0";
            if (!(sid >= 0)) continue;
            try {
                const rawReq = rt.netCall("read_http", String(sid));
                if (typeof rawReq === "string" && rawReq.length > 0) {
                    const parsed = JSON.parse(rawReq);
                    const req = rt.mkRequest(parsed.method, parsed.uri, parsed.headers, parsed.body, null);
                    const pAddr = parseSocketAddr(peerStr);
                    req.peerAddr = pAddr.__var === "Ok" ? pAddr.__data[0] : mkSocketAddr("127.0.0.1", 0);
                    const resp = await rt.routerDispatchAsync(serveObj.router, req);
                    const bodyStr = resp.body || "";
                    const bodyBytes = rt.strToUtf8Bytes(bodyStr);
                    let wire = "HTTP/1.1 " + resp.status + " " + rt.statusReason(resp.status) + "\r\n";
                    let hasCt = false, hasCl = false;
                    const entries = rt.mapEntries(resp.headers);
                    for (let i = 0; i < entries.length; i++) {
                        const hk = strVal(entries[i][0]).replace(/[\r\n]/g, "");
                        const hv = strVal(entries[i][1]).replace(/[\r\n]/g, "");
                        if (hk.toLowerCase() === "content-type") hasCt = true;
                        if (hk.toLowerCase() === "content-length") hasCl = true;
                        wire += hk + ": " + hv + "\r\n";
                    }
                    if (!hasCt && resp.status !== 204) wire += "Content-Type: text/plain; charset=utf-8\r\n";
                    if (!hasCl) wire += "Content-Length: " + bodyBytes.length + "\r\n";
                    wire += "Connection: close\r\n\r\n" + bodyStr;
                    rt.netCall("write_str", String(sid), wire);
                }
            } catch (e) {
                const msg = e && e.isRsPanic ? e.message : String(e);
                if (typeof rt.writeStderr === "function") {
                    rt.writeStderr("[axum 500] " + msg + (e && e.stack ? "\n" + e.stack : "") + "\n");
                }
                const errWire = "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: " +
                    rt.utf8ByteLen(msg) + "\r\nConnection: close\r\n\r\n" + msg;
                rt.netCall("write_str", String(sid), errWire);
            } finally {
                rt.netCall("close_stream", String(sid));
            }
        }
        rt.netCall("close_listener", String(lid));
        return Ok(undefined);
    }

    // ---- Ecosystem Object Methods ----
    rt.callEcoMethod = function (obj, name, args, typeArgs) {
        switch (obj.kind) {
            case "tracing_fmt_builder":
                if (name === "init") return undefined;
                if (name === "try_init") return Ok(undefined);
                return obj;
            case "env_filter":
                if (name === "add_directive") return obj;
                return obj;
            case "sqlite_connect_opts":
                if (name === "filename") { obj.url = strVal(args[0]); return obj; }
                if (name === "create_if_missing") { obj.createIfMissing = !!deref(args[0]); return obj; }
                if (name === "journal_mode") {
                    const m = deref(args[0]);
                    obj.journalMode = (m && m.__rs === 2) ? m.__var.toUpperCase() : strVal(m);
                    return obj;
                }
                if (name === "synchronous") {
                    const s = deref(args[0]);
                    obj.synchronous = (s && s.__rs === 2) ? s.__var.toUpperCase() : strVal(s);
                    return obj;
                }
                if (name === "foreign_keys") { obj.foreignKeys = !!deref(args[0]); return obj; }
                if (name === "busy_timeout") {
                    const d = deref(args[0]);
                    obj.busyTimeoutMs = (d && d.kind === "duration") ? Number(d.ns / 1000000n) : Number(d);
                    return obj;
                }
                if (name === "pragma") {
                    obj.pragmas.push([strVal(args[0]), strVal(args[1])]);
                    return obj;
                }
                if (name === "log_statements" || name === "log_slow_statements" || name === "disable_statement_logging"
                    || name === "read_only" || name === "in_memory" || name === "shared_cache") {
                    if (name === "in_memory" && deref(args[0])) obj.url = ":memory:";
                    return obj;
                }
                if (name === "clone") {
                    return Object.assign({}, obj, { pragmas: obj.pragmas.slice() });
                }
                break;
            case "sqlite_pool_opts":
                if (name === "max_connections" || name === "min_connections" || name === "acquire_timeout"
                    || name === "idle_timeout" || name === "max_lifetime" || name === "test_before_acquire") {
                    return obj;
                }
                if (name === "connect" || name === "connect_lazy") {
                    return Promise.resolve(openSqlitePool({ url: strVal(args[0]), createIfMissing: true, pragmas: [] }));
                }
                if (name === "connect_with") {
                    return Promise.resolve(openSqlitePool(deref(args[0])));
                }
                break;
            case "sqlite_pool":
                if (name === "clone") return obj;
                if (name === "is_closed") return !!obj.closed;
                if (name === "close") {
                    if (!obj.closed) {
                        sqliteCall("execute", String(obj.connId), "PRAGMA wal_checkpoint(TRUNCATE)", "[]");
                        sqliteCall("close", String(obj.connId), "", "");
                        obj.closed = true;
                    }
                    return Promise.resolve(undefined);
                }
                if (name === "acquire") {
                    return Promise.resolve(Ok({ __rs: 15, kind: "sqlite_conn", connId: obj.connId }));
                }
                if (name === "begin") {
                    const depth = obj.txDepth || 0;
                    const sql = depth === 0 ? "BEGIN IMMEDIATE" : ("SAVEPOINT __zipp_sp_" + depth);
                    const raw = sqliteCall("execute", String(obj.connId), sql, "[]");
                    const p = JSON.parse(raw);
                    if (p.error) return Promise.resolve(Err(mkSqlxDbErr(p.error, p.code, p.unique)));
                    obj.txDepth = depth + 1;
                    return Promise.resolve(Ok({
                        __rs: 15,
                        kind: "sqlite_tx",
                        connId: obj.connId,
                        pool: obj,
                        depth: depth,
                        active: true
                    }));
                }
                break;
            case "sqlite_tx":
                if (name === "commit") {
                    if (obj.active) {
                        const sql = obj.depth === 0 ? "COMMIT" : ("RELEASE SAVEPOINT __zipp_sp_" + obj.depth);
                        const raw = sqliteCall("execute", String(obj.connId), sql, "[]");
                        obj.active = false;
                        if (obj.pool && obj.pool.txDepth > 0) obj.pool.txDepth--;
                        const p = JSON.parse(raw);
                        if (p.error) return Promise.resolve(Err(mkSqlxDbErr(p.error, p.code, p.unique)));
                    }
                    return Promise.resolve(Ok(undefined));
                }
                if (name === "rollback") {
                    if (obj.active) {
                        const sql = obj.depth === 0 ? "ROLLBACK" : ("ROLLBACK TO SAVEPOINT __zipp_sp_" + obj.depth);
                        sqliteCall("execute", String(obj.connId), sql, "[]");
                        if (obj.depth > 0) {
                            sqliteCall("execute", String(obj.connId), "RELEASE SAVEPOINT __zipp_sp_" + obj.depth, "[]");
                        }
                        obj.active = false;
                        if (obj.pool && obj.pool.txDepth > 0) obj.pool.txDepth--;
                    }
                    return Promise.resolve(Ok(undefined));
                }
                if (name === "begin") {
                    const d = (obj.depth || 0) + 1;
                    sqliteCall("execute", String(obj.connId), "SAVEPOINT __zipp_sp_" + d, "[]");
                    return Promise.resolve(Ok({
                        __rs: 15,
                        kind: "sqlite_tx",
                        connId: obj.connId,
                        pool: obj.pool,
                        depth: d,
                        active: true
                    }));
                }
                if (name === "deref" || name === "deref_mut") return obj;
                break;
            case "sqlite_conn":
                if (name === "close") return Promise.resolve(Ok(undefined));
                if (name === "deref" || name === "deref_mut") return obj;
                break;
            case "sqlx_query": {
                if (name === "bind") {
                    obj.params.push(encodeSqlParam(args[0]));
                    return obj;
                }
                if (name === "execute") {
                    const cid = resolveSqlConnId(args[0]);
                    if (cid < 0) return Promise.resolve(Err(mkSqlxDbErr("invalid sqlite executor", 0, false)));
                    const raw = sqliteCall("execute", String(cid), obj.sql, JSON.stringify(obj.params));
                    const p = JSON.parse(raw);
                    if (p.error) return Promise.resolve(Err(mkSqlxDbErr(p.error, p.code, p.unique)));
                    return Promise.resolve(Ok({
                        __rs: 15,
                        kind: "sqlx_query_result",
                        rows_affected: p.rows_affected || 0,
                        last_insert_rowid: p.last_insert_rowid || 0
                    }));
                }
                if (name === "fetch_one" || name === "fetch_optional" || name === "fetch_all") {
                    const cid = resolveSqlConnId(args[0]);
                    if (cid < 0) return Promise.resolve(Err(mkSqlxDbErr("invalid sqlite executor", 0, false)));
                    const raw = sqliteCall("query", String(cid), obj.sql, JSON.stringify(obj.params));
                    const p = JSON.parse(raw);
                    if (p.error) return Promise.resolve(Err(mkSqlxDbErr(p.error, p.code, p.unique)));
                    let rowType = obj.rowType || "";
                    if (!rowType && typeArgs && typeArgs[0]) {
                        let hint = typeArgs[0];
                        if (name === "fetch_all") hint = unwrapOuterType(hint, "Vec");
                        else if (name === "fetch_optional") hint = unwrapOuterType(hint, "Option");
                        rowType = hint;
                    }
                    const cols = p.columns || [];
                    const rows = p.rows || [];
                    if (name === "fetch_one") {
                        if (rows.length === 0) return Promise.resolve(Err(mkSqlxRowNotFound()));
                        return Promise.resolve(Ok(decodeSqlRow(cols, rows[0], obj.mode, rowType)));
                    }
                    if (name === "fetch_optional") {
                        if (rows.length === 0) return Promise.resolve(Ok(None));
                        return Promise.resolve(Ok(Some(decodeSqlRow(cols, rows[0], obj.mode, rowType))));
                    }
                    const out = [];
                    for (let i = 0; i < rows.length; i++) {
                        out.push(decodeSqlRow(cols, rows[i], obj.mode, rowType));
                    }
                    return Promise.resolve(Ok({ __rs: 7, items: out }));
                }
                break;
            }
            case "sqlx_query_result":
                if (name === "rows_affected") return obj.rows_affected;
                if (name === "last_insert_rowid") return obj.last_insert_rowid;
                break;
            case "sqlx_row": {
                const lookupCell = function (keyArg) {
                    const k = deref(keyArg);
                    if (typeof k === "number") return obj.values[k];
                    const nameStr = strVal(k);
                    const idx = obj.columns.indexOf(nameStr);
                    return idx >= 0 ? obj.values[idx] : undefined;
                };
                if (name === "get") {
                    const raw = lookupCell(args[0]);
                    return decodeSqlCell(raw, typeArgs && typeArgs[0] ? typeArgs[0] : "");
                }
                if (name === "try_get") {
                    const raw = lookupCell(args[0]);
                    if (raw === undefined) return Err(mkSqlxDbErr("column not found", 0, false));
                    return Ok(decodeSqlCell(raw, typeArgs && typeArgs[0] ? typeArgs[0] : ""));
                }
                if (name === "len") return obj.values.length;
                break;
            }
            case "sqlx_db_error":
                if (name === "is_unique_violation") return !!obj.unique;
                if (name === "message") return obj.msg;
                if (name === "code") return obj.code ? Some(R.mkString(obj.code)) : None;
                if (name === "to_string") return R.mkString(obj.msg);
                break;
            case "anyhow_error":
                if (name === "to_string") return R.mkString(obj.msg);
                if (name === "root_cause") return obj.inner || obj;
                break;
            case "socket_addr":
                if (name === "ip") return mkIpAddr(obj.ip);
                if (name === "port") return obj.port;
                if (name === "is_ipv4") return obj.ip.indexOf(":") < 0;
                if (name === "is_ipv6") return obj.ip.indexOf(":") >= 0;
                if (name === "to_string") return R.mkString(strVal(obj));
                if (name === "clone") return mkSocketAddr(obj.ip, obj.port);
                break;
            case "ip_addr":
                if (name === "is_loopback") return ipIsLoopback(obj.ip);
                if (name === "is_unspecified") return obj.ip === "0.0.0.0" || obj.ip === "::";
                if (name === "is_private") return ipIsPrivate(obj.ip);
                if (name === "is_link_local") return ipIsLinkLocal(obj.ip);
                if (name === "is_ipv4") return obj.ip.indexOf(":") < 0;
                if (name === "is_ipv6") return obj.ip.indexOf(":") >= 0;
                if (name === "to_string") return R.mkString(obj.ip);
                if (name === "clone") return mkIpAddr(obj.ip);
                break;
            case "uri":
                if (name === "path") return obj.path;
                if (name === "query") return obj.query ? Some(obj.query) : None;
                if (name === "to_string") return R.mkString(obj.raw);
                if (name === "clone") return { __rs: 15, kind: "uri", raw: obj.raw, path: obj.path, query: obj.query };
                break;
            case "header_map": {
                const normKey = function (k) { return R.mkString(strVal(k).toLowerCase()); };
                if (name === "get") {
                    const v = rt.mapGetRaw(obj.map, normKey(args[0]));
                    return v === undefined ? None : Some({ __rs: 15, kind: "header_value", v: strVal(v) });
                }
                if (name === "insert" || name === "append") {
                    rt.mapSetRaw(obj.map, normKey(args[0]), R.mkString(strVal(args[1])));
                    return None;
                }
                if (name === "remove") {
                    const mk = rt.mapKeyOf(normKey(args[0]));
                    const prev = obj.map.map.get(mk);
                    obj.map.map.delete(mk);
                    return prev ? Some({ __rs: 15, kind: "header_value", v: strVal(prev[1]) }) : None;
                }
                if (name === "contains_key") return obj.map.map.has(rt.mapKeyOf(normKey(args[0])));
                if (name === "len") return obj.map.map.size;
                if (name === "is_empty") return obj.map.map.size === 0;
                if (name === "iter") {
                    const es = rt.mapEntries(obj.map);
                    let i = 0;
                    return rt.mkIter(function () {
                        if (i >= es.length) return R.ITER_DONE;
                        const e = es[i++];
                        return R.mkTuple([
                            { __rs: 15, kind: "header_name", n: strVal(e[0]) },
                            { __rs: 15, kind: "header_value", v: strVal(e[1]) }
                        ]);
                    });
                }
                break;
            }
            case "header_value":
                if (name === "to_str") return Ok(obj.v);
                if (name === "as_bytes") return { __rs: 7, items: rt.strToUtf8Bytes(obj.v) };
                if (name === "to_string") return R.mkString(obj.v);
                if (name === "clone") return { __rs: 15, kind: "header_value", v: obj.v };
                break;
            case "header_name":
                if (name === "as_str") return obj.n;
                if (name === "to_string") return R.mkString(obj.n);
                if (name === "clone") return { __rs: 15, kind: "header_name", n: obj.n };
                break;
            case "extensions":
                if (name === "insert") {
                    const v = deref(args[0]);
                    const tag = rt.typeTagOf(v);
                    const prev = Object.prototype.hasOwnProperty.call(obj.store, tag) ? Some(obj.store[tag]) : None;
                    obj.store[tag] = v;
                    return prev;
                }
                if (name === "get") {
                    const key = (typeArgs && typeArgs[0]) ? typeArgs[0].split("<")[0].trim() : "";
                    const skey = key.indexOf("::") >= 0 ? key.slice(key.lastIndexOf("::") + 2) : key;
                    if (key && Object.prototype.hasOwnProperty.call(obj.store, key)) {
                        return Some(obj.store[key]);
                    }
                    if (skey && Object.prototype.hasOwnProperty.call(obj.store, skey)) {
                        return Some(obj.store[skey]);
                    }
                    return None;
                }
                break;
            case "axum_next":
                if (name === "run") return Promise.resolve(obj.runFn(deref(args[0])));
                break;
            case "resp_builder":
                if (name === "status") { obj.status = Number(deref(args[0])) | 0; return obj; }
                if (name === "header") { obj.headers.push([strVal(args[0]), strVal(args[1])]); return obj; }
                if (name === "body") {
                    const b = deref(args[0]);
                    const bodyStr = (b && b.__rs === 15 && b.kind === "body") ? b.data
                        : (b && b.__rs === 7 ? rt.utf8BytesToStr(b.items) : strVal(b));
                    const resp = rt.mkResponse(obj.status, bodyStr, null);
                    for (let i = 0; i < obj.headers.length; i++) {
                        rt.mapSetRaw(resp.headers, R.mkString(obj.headers[i][0]), R.mkString(obj.headers[i][1]));
                    }
                    return Ok(resp);
                }
                break;
            case "body":
                if (name === "into_bytes") return { __rs: 7, items: rt.strToUtf8Bytes(obj.data) };
                break;
            case "cors_layer":
                if (name === "allow_origin") {
                    const a = deref(args[0]);
                    if (a && a.__rs === 15 && a.kind === "allow_origin") {
                        obj.allowAny = !!a.any;
                        obj.predicate = a.predicate || null;
                        obj.origins = a.origins || null;
                    }
                    return obj;
                }
                if (name === "allow_methods") {
                    const a = deref(args[0]);
                    if (a && a.__rs === 7) obj.methods = a.items.map(strVal);
                    return obj;
                }
                if (name === "allow_headers") {
                    const a = deref(args[0]);
                    if (a && a.__rs === 7) obj.headers = a.items.map(strVal);
                    return obj;
                }
                if (name === "allow_credentials" || name === "expose_headers" || name === "max_age") return obj;
                break;
            case "axum_serve":
                if (name === "with_graceful_shutdown") {
                    obj.shutdownFut = Promise.resolve(args[0]);
                    return obj;
                }
                break;
            case "join_handle":
                if (name === "abort") {
                    obj.aborted = true;
                    if (obj.resources && obj.resources.length > 0) {
                        for (let i = 0; i < obj.resources.length; i++) {
                            const r = obj.resources[i];
                            if (!r) continue;
                            if (r.kind === "tokio_interval") {
                                r.aborted = true;
                                if (r.pendingTimer && typeof r.pendingTimer.__cancel === "function") {
                                    r.pendingTimer.__cancel();
                                    r.pendingTimer = null;
                                }
                            } else if (r.kind === "broadcast_rx" && r.sub) {
                                r.sub.closed = true;
                                if (r.bus && r.bus.subs) {
                                    const bi = r.bus.subs.indexOf(r.sub);
                                    if (bi >= 0) r.bus.subs.splice(bi, 1);
                                }
                                const ws = r.sub.waiters.splice(0);
                                for (let j = 0; j < ws.length; j++) {
                                    ws[j](Err(R.mkEnumUnit("RecvError", "Closed", null)));
                                }
                            } else if (r.kind === "mpsc_rx" && r.ch) {
                                r.ch.closed = true;
                                const ws = r.ch.waiters.splice(0);
                                for (let j = 0; j < ws.length; j++) ws[j](None);
                            } else if (r.kind === "ws_receiver" && r.ws) {
                                closeWsInternal(r.ws, 1000, "", true);
                            }
                        }
                        obj.resources.length = 0;
                    }
                    return undefined;
                }
                if (name === "is_finished") return !!obj.finished;
                break;
            case "watch_tx":
                if (name === "send") {
                    obj.cell.val = args[0];
                    obj.cell.version++;
                    const ws = obj.cell.waiters.splice(0);
                    for (let i = 0; i < ws.length; i++) ws[i](Ok(undefined));
                    return Ok(undefined);
                }
                if (name === "send_replace") {
                    const old = obj.cell.val;
                    obj.cell.val = args[0];
                    obj.cell.version++;
                    const ws = obj.cell.waiters.splice(0);
                    for (let i = 0; i < ws.length; i++) ws[i](Ok(undefined));
                    return old;
                }
                if (name === "subscribe") {
                    return { __rs: 15, kind: "watch_rx", cell: obj.cell, seen: obj.cell.version };
                }
                if (name === "borrow") {
                    return { __rs: 6, get: function () { return obj.cell.val; }, set: function (x) { obj.cell.val = x; } };
                }
                if (name === "clone") return { __rs: 15, kind: "watch_tx", cell: obj.cell };
                break;
            case "watch_rx":
                if (name === "borrow" || name === "borrow_and_update") {
                    obj.seen = obj.cell.version;
                    return { __rs: 6, get: function () { return obj.cell.val; }, set: function (x) { obj.cell.val = x; } };
                }
                if (name === "changed") {
                    if (obj.seen !== obj.cell.version) {
                        obj.seen = obj.cell.version;
                        return Promise.resolve(Ok(undefined));
                    }
                    let waiter = null;
                    const p = new Promise(function (resolve) {
                        waiter = function (res) {
                            obj.seen = obj.cell.version;
                            resolve(res);
                        };
                        obj.cell.waiters.push(waiter);
                    });
                    p.__cancel = function () {
                        const idx = obj.cell.waiters.indexOf(waiter);
                        if (idx >= 0) obj.cell.waiters.splice(idx, 1);
                    };
                    return p;
                }
                if (name === "clone") return { __rs: 15, kind: "watch_rx", cell: obj.cell, seen: obj.seen };
                break;
            case "broadcast_tx":
                if (name === "send") {
                    const msg = args[0];
                    const subs = obj.bus.subs;
                    if (subs.length === 0) return Err(R.mkString("no active receivers"));
                    for (let i = 0; i < subs.length; i++) {
                        const sub = subs[i];
                        const item = cloneVal(msg);
                        if (sub.waiters.length > 0) {
                            sub.waiters.shift()(Ok(item));
                        } else {
                            if (sub.queue.length - sub.qhead >= obj.bus.cap) {
                                sub.queue[sub.qhead++] = undefined;
                                if (sub.qhead >= 64 && sub.qhead * 2 >= sub.queue.length) {
                                    sub.queue = sub.queue.slice(sub.qhead);
                                    sub.qhead = 0;
                                }
                                sub.lagged++;
                            }
                            sub.queue.push(item);
                        }
                    }
                    return Ok(subs.length);
                }
                if (name === "subscribe") {
                    const sub = { queue: [], qhead: 0, waiters: [], lagged: 0, closed: false };
                    obj.bus.subs.push(sub);
                    return { __rs: 15, kind: "broadcast_rx", bus: obj.bus, sub: sub };
                }
                if (name === "receiver_count") return obj.bus.subs.length;
                if (name === "clone") return { __rs: 15, kind: "broadcast_tx", bus: obj.bus };
                break;
            case "broadcast_rx":
                if (name === "recv") {
                    const sub = obj.sub;
                    trackSpawnResource(obj);
                    if (sub.lagged > 0) {
                        const n = sub.lagged;
                        sub.lagged = 0;
                        return Promise.resolve(Err(R.mkEnumTuple("RecvError", "Lagged", [n])));
                    }
                    if (sub.qhead < sub.queue.length) {
                        const v = sub.queue[sub.qhead];
                        sub.queue[sub.qhead++] = undefined;
                        if (sub.qhead === sub.queue.length) { sub.queue.length = 0; sub.qhead = 0; }
                        else if (sub.qhead >= 64 && sub.qhead * 2 >= sub.queue.length) { sub.queue = sub.queue.slice(sub.qhead); sub.qhead = 0; }
                        return Promise.resolve(Ok(v));
                    }
                    if (sub.closed) return Promise.resolve(Err(R.mkEnumUnit("RecvError", "Closed", null)));
                    let waiter = null;
                    const p = new Promise(function (resolve) {
                        waiter = resolve;
                        sub.waiters.push(waiter);
                    });
                    p.__cancel = function () {
                        const idx = sub.waiters.indexOf(waiter);
                        if (idx >= 0) sub.waiters.splice(idx, 1);
                    };
                    return p;
                }
                break;
            case "mpsc_tx":
                if (name === "send" || name === "try_send") {
                    const ch = obj.ch;
                    if (ch.closed) {
                        return name === "send" ? Promise.resolve(Err(R.mkString("channel closed"))) : Err(R.mkString("channel closed"));
                    }
                    if (ch.waiters.length > 0) ch.waiters.shift()(Some(args[0]));
                    else ch.queue.push(args[0]);
                    return name === "send" ? Promise.resolve(Ok(undefined)) : Ok(undefined);
                }
                if (name === "clone") return { __rs: 15, kind: "mpsc_tx", ch: obj.ch };
                break;
            case "mpsc_rx":
                if (name === "recv") {
                    const ch = obj.ch;
                    trackSpawnResource(obj);
                    if (ch.qhead < ch.queue.length) {
                        const v = ch.queue[ch.qhead];
                        ch.queue[ch.qhead++] = undefined;
                        if (ch.qhead === ch.queue.length) { ch.queue.length = 0; ch.qhead = 0; }
                        else if (ch.qhead >= 64 && ch.qhead * 2 >= ch.queue.length) { ch.queue = ch.queue.slice(ch.qhead); ch.qhead = 0; }
                        return Promise.resolve(Some(v));
                    }
                    if (ch.closed) return Promise.resolve(None);
                    let waiter = null;
                    const p = new Promise(function (resolve) {
                        waiter = resolve;
                        ch.waiters.push(waiter);
                    });
                    p.__cancel = function () {
                        const idx = ch.waiters.indexOf(waiter);
                        if (idx >= 0) ch.waiters.splice(idx, 1);
                    };
                    return p;
                }
                if (name === "try_recv") {
                    const ch = obj.ch;
                    if (ch.qhead < ch.queue.length) {
                        const v = ch.queue[ch.qhead];
                        ch.queue[ch.qhead++] = undefined;
                        if (ch.qhead === ch.queue.length) { ch.queue.length = 0; ch.qhead = 0; }
                        else if (ch.qhead >= 64 && ch.qhead * 2 >= ch.queue.length) { ch.queue = ch.queue.slice(ch.qhead); ch.qhead = 0; }
                        return Ok(v);
                    }
                    return Err(R.mkEnumUnit("TryRecvError", "Empty", null));
                }
                break;
            case "tokio_interval":
                if (name === "tick") {
                    if (obj.aborted) {
                        return new Promise(function () {});
                    }
                    if (obj.first) { obj.first = false; return Promise.resolve({ __rs: 15, kind: "instant", ns: rt.timeCall("mono") }); }
                    const tp = scheduleTimer(obj.ms);
                    obj.pendingTimer = tp;
                    const p = tp.then(function () {
                        if (obj.pendingTimer === tp) obj.pendingTimer = null;
                        if (obj.aborted) return new Promise(function () {});
                        return { __rs: 15, kind: "instant", ns: rt.timeCall("mono") };
                    });
                    p.__cancel = tp.__cancel;
                    return p;
                }
                if (name === "set_missed_tick_behavior") return undefined;
                break;
            case "offset_datetime":
                if (name === "unix_timestamp") return Number(obj.ns / 1000000000n);
                if (name === "unix_timestamp_nanos") return obj.ns;
                if (name === "format") return Ok(R.mkString(formatOffsetDateTime(obj.ns, args[0])));
                if (name === "to_rfc3339" || name === "to_string") return R.mkString(formatOffsetDateTime(obj.ns, "rfc3339"));
                if (name === "year") return datePartsUtc(obj.ns).year;
                if (name === "month") return datePartsUtc(obj.ns).month;
                if (name === "day") return datePartsUtc(obj.ns).day;
                if (name === "hour") return datePartsUtc(obj.ns).hour;
                if (name === "minute") return datePartsUtc(obj.ns).minute;
                if (name === "second") return datePartsUtc(obj.ns).second;
                if (name === "clone") return mkOffsetDateTime(obj.ns);
                break;
            case "uuid":
                if (name === "to_string" || name === "hyphenated") return R.mkString(obj.s);
                if (name === "simple") return R.mkString(obj.s.replace(/-/g, ""));
                if (name === "as_bytes") return { __rs: 7, items: hexToBytes(obj.s.replace(/-/g, "")) };
                if (name === "clone") return mkUuid(obj.s);
                break;
            case "argon2":
                if (name === "hash_password") {
                    const pw = bytesToStr(args[0]);
                    const pstr = obj.m + "," + obj.t + "," + obj.p;
                    const phc = rt.cryptoCall("argon2_hash", pw, pstr);
                    return typeof phc === "string" && phc.length > 0
                        ? Ok({ __rs: 15, kind: "argon2_hash", s: phc })
                        : Err(R.mkString("argon2 hash failed"));
                }
                if (name === "verify_password") {
                    const pw = bytesToStr(args[0]);
                    const ph = deref(args[1]);
                    const hashStr = ph && ph.s ? ph.s : strVal(ph);
                    const ok = rt.cryptoCall("argon2_verify", pw, hashStr) === true;
                    return ok ? Ok(undefined) : Err(R.mkString("invalid password"));
                }
                break;
            case "argon2_hash":
                if (name === "to_string") return R.mkString(obj.s);
                if (name === "clone") return { __rs: 15, kind: "argon2_hash", s: obj.s };
                break;
            case "sha256_hasher":
                if (name === "update") {
                    obj.chunks.push(bytesToHex(args[0]));
                    return undefined;
                }
                if (name === "finalize") {
                    const hex = rt.cryptoCall("sha256_bytes_hex", obj.chunks.join(""));
                    const h = typeof hex === "string" ? hex : "";
                    return { __rs: 15, kind: "digest_bytes", hex: h, bytes: hexToBytes(h) };
                }
                break;
            case "hmac_sha256":
                if (name === "update") {
                    obj.chunks.push(bytesToHex(args[0]));
                    return undefined;
                }
                if (name === "finalize") {
                    const hex = rt.cryptoCall("hmac_sha256_hex", obj.keyHex, obj.chunks.join(""));
                    const h = typeof hex === "string" ? hex : "";
                    return { __rs: 15, kind: "digest_bytes", hex: h, bytes: hexToBytes(h) };
                }
                break;
            case "digest_bytes":
                if (name === "into_bytes" || name === "as_slice" || name === "to_vec") {
                    return { __rs: 7, items: obj.bytes.slice() };
                }
                if (name === "to_string") return R.mkString(obj.hex);
                break;
            case "base64_engine":
                if (name === "encode") {
                    const op = obj.url ? "base64url_encode_hex" : "base64_encode_hex";
                    return R.mkString(rt.cryptoCall(op, bytesToHex(args[0])) || "");
                }
                if (name === "decode") {
                    const op = obj.url ? "base64url_decode_hex" : "base64_decode_hex";
                    const hx = rt.cryptoCall(op, strVal(args[0]));
                    return typeof hx === "string" ? Ok({ __rs: 7, items: hexToBytes(hx) }) : Err(R.mkString("invalid base64"));
                }
                break;
            case "rng":
                if (name === "fill_bytes") {
                    const target = deref(args[0]);
                    if (target && target.__rs === 7) {
                        const n = target.items.length;
                        const hx = rt.cryptoCall("random_bytes", String(n));
                        if (typeof hx === "string") {
                            for (let i = 0; i < n && i * 2 + 1 < hx.length; i++) {
                                target.items[i] = parseInt(hx.slice(i * 2, i * 2 + 2), 16);
                            }
                        }
                    }
                    return undefined;
                }
                if (name === "gen") {
                    return randForType(typeArgs && typeArgs[0] ? typeArgs[0] : "");
                }
                if (name === "gen_range") {
                    const r = deref(args[0]);
                    if (r && r.__rs === 9) {
                        const lo = Number(r.start || 0);
                        const hi = Number(r.end || 1);
                        const span = r.inclusive ? (hi - lo + 1) : (hi - lo);
                        if (Number.isInteger(lo) && Number.isInteger(hi)) {
                            return lo + Math.floor(Math.random() * Math.max(1, span));
                        }
                        return lo + Math.random() * (hi - lo);
                    }
                    return Math.random();
                }
                break;
            case "http_client_builder":
                if (name === "timeout" || name === "connect_timeout" || name === "user_agent" || name === "default_headers") {
                    return obj;
                }
                if (name === "build") return Ok({ __rs: 15, kind: "http_client" });
                break;
            case "thread_builder":
                if (name === "name" || name === "stack_size") return obj;
                if (name === "spawn") return Ok({ __rs: 15, kind: "join_handle", finished: true, aborted: false });
                break;
            case "ws_upgrade":
                if (name === "on_upgrade") {
                    const cb = args[0];
                    if (obj.req && obj.req.__wsId) {
                        const ws = {
                            id: String(obj.req.__wsId),
                            closed: false,
                            inQueue: [],
                            inWaiters: [],
                            onUpgradeFn: cb
                        };
                        const resp = rt.mkResponse(101, "", null);
                        resp.__wsUpgraded = ws;
                        return resp;
                    }
                    return rt.mkResponse(426, "WebSocket upgrade required", "text/plain");
                }
                if (name === "max_message_size" || name === "max_frame_size" || name === "protocols"
                    || name === "write_buffer_size" || name === "max_write_buffer_size"
                    || name === "accept_unmasked_frames" || name === "on_failed_upgrade") {
                    return obj;
                }
                break;
            case "ws_socket":
                if (name === "split") {
                    return R.mkTuple([
                        { __rs: 15, kind: "ws_sender", ws: obj.ws },
                        { __rs: 15, kind: "ws_receiver", ws: obj.ws }
                    ]);
                }
                if (name === "send") return sendWsMessage(obj.ws, args[0]);
                if (name === "recv" || name === "next") return recvWsMessage(obj.ws);
                if (name === "close") {
                    closeWsInternal(obj.ws, 1000, "", true);
                    return Promise.resolve(Ok(undefined));
                }
                if (name === "protocol") return None;
                break;
            case "ws_sender":
                if (name === "send") return sendWsMessage(obj.ws, args[0]);
                if (name === "close") {
                    closeWsInternal(obj.ws, 1000, "", true);
                    return Promise.resolve(Ok(undefined));
                }
                if (name === "flush") return Promise.resolve(Ok(undefined));
                break;
            case "ws_receiver":
                if (name === "next" || name === "recv") return recvWsMessage(obj.ws);
                break;
            case "duration":
                if (name === "whole_seconds") return Number(obj.ns / 1000000000n);
                if (name === "whole_milliseconds") return Number(obj.ns / 1000000n);
                break;
        }
        return ECO_MISS;
    };

    function randForType(ty) {
        const t = (ty || "").trim();
        if (t === "f64" || t === "f32") return Math.random();
        if (t === "bool") return Math.random() < 0.5;
        if (t === "u8") return Math.floor(Math.random() * 256);
        if (t === "i8") return Math.floor(Math.random() * 256) - 128;
        if (t === "u16") return Math.floor(Math.random() * 65536);
        if (t === "i16") return Math.floor(Math.random() * 65536) - 32768;
        const hx = rt.cryptoCall("random_bytes", "4");
        const u32 = typeof hx === "string" ? (parseInt(hx, 16) >>> 0) : Math.floor(Math.random() * 0x100000000);
        if (t === "i32" || t === "i64" || t === "isize") return u32 | 0;
        return u32;
    }

    // Make `axum_serve` and `join_handle` proper JS thenables so `.await` works seamlessly!
    function mkAxumServe(listener, router) {
        const obj = {
            __rs: 15,
            kind: "axum_serve",
            listener: deref(listener),
            router: deref(router),
            shutdownFut: null
        };
        obj.then = function (onFulfilled, onRejected) {
            return runAxumServe(obj).then(onFulfilled, onRejected);
        };
        return obj;
    }

    function mkJoinHandle(promise) {
        const resources = recentSpawnResources.splice(0);
        const obj = { __rs: 15, kind: "join_handle", finished: false, aborted: false, resources: resources };
        const p = Promise.resolve(promise).then(
            function (v) { obj.finished = true; return Ok(v); },
            function (e) { obj.finished = true; return Err(R.mkString(String(e))); }
        );
        obj.then = function (onFulfilled, onRejected) {
            return p.then(onFulfilled, onRejected);
        };
        return obj;
    }

    // ---- Ecosystem Associated Functions & Free Functions ----
    rt.callEcoAssoc = function (pathParts, args, typeArgs) {
        const last = pathParts[pathParts.length - 1];
        const parent = pathParts.length >= 2 ? pathParts[pathParts.length - 2] : "";
        const root = pathParts[0] || "";

        // ---- tracing_subscriber / tracing ----
        if (parent === "tracing_subscriber" && last === "fmt") {
            return { __rs: 15, kind: "tracing_fmt_builder" };
        }
        if (parent === "EnvFilter") {
            if (last === "try_from_default_env") {
                const v = rt.envCall("var", "RUST_LOG");
                return typeof v === "string"
                    ? Ok({ __rs: 15, kind: "env_filter", spec: v })
                    : Err(R.mkString("RUST_LOG not set"));
            }
            if (last === "new" || last === "from") return { __rs: 15, kind: "env_filter", spec: strVal(args[0]) };
            if (last === "try_new") return Ok({ __rs: 15, kind: "env_filter", spec: strVal(args[0]) });
        }

        // ---- sqlx ----
        if (parent === "SqliteConnectOptions") {
            if (last === "new") return { __rs: 15, kind: "sqlite_connect_opts", url: ":memory:", createIfMissing: false, pragmas: [] };
            if (last === "from_str") {
                return Ok({ __rs: 15, kind: "sqlite_connect_opts", url: strVal(args[0]), createIfMissing: false, pragmas: [] });
            }
        }
        if (parent === "SqlitePoolOptions" && last === "new") {
            return { __rs: 15, kind: "sqlite_pool_opts" };
        }
        if (parent === "SqlitePool") {
            if (last === "connect") return Promise.resolve(openSqlitePool({ url: strVal(args[0]), createIfMissing: true, pragmas: [] }));
            if (last === "connect_with") return Promise.resolve(openSqlitePool(deref(args[0])));
        }
        if (parent === "sqlx" || (pathParts.length === 1 && (last === "query" || last === "query_scalar" || last === "query_as"))) {
            if (last === "query") {
                return { __rs: 15, kind: "sqlx_query", mode: "raw", sql: strVal(args[0]), params: [], rowType: "" };
            }
            if (last === "query_scalar") {
                let rowType = "";
                if (typeArgs && typeArgs.length > 0) {
                    rowType = typeArgs.length >= 2 ? typeArgs[1] : typeArgs[0];
                    if (rowType === "_") rowType = "";
                }
                return { __rs: 15, kind: "sqlx_query", mode: "scalar", sql: strVal(args[0]), params: [], rowType: rowType };
            }
            if (last === "query_as") {
                let rowType = "";
                if (typeArgs && typeArgs.length > 0) {
                    rowType = typeArgs.length >= 2 ? typeArgs[1] : typeArgs[0];
                    if (rowType === "_") rowType = "";
                }
                return { __rs: 15, kind: "sqlx_query", mode: "as", sql: strVal(args[0]), params: [], rowType: rowType };
            }
        }

        // ---- axum & tower_http ----
        if (parent === "axum" && last === "serve") {
            return mkAxumServe(args[0], args[1]);
        }
        if (parent === "middleware") {
            if (last === "from_fn_with_state") {
                return { __rs: 15, kind: "axum_mw", state: args[0], fn: args[1], hasState: true };
            }
            if (last === "from_fn") {
                return { __rs: 15, kind: "axum_mw", state: undefined, fn: args[0], hasState: false };
            }
        }
        if (parent === "routing") {
            if (last === "get") return { __rs: 15, kind: "method_router", entries: [["GET", args[0]]] };
            if (last === "post") return { __rs: 15, kind: "method_router", entries: [["POST", args[0]]] };
            if (last === "put") return { __rs: 15, kind: "method_router", entries: [["PUT", args[0]]] };
            if (last === "patch") return { __rs: 15, kind: "method_router", entries: [["PATCH", args[0]]] };
            if (last === "delete") return { __rs: 15, kind: "method_router", entries: [["DELETE", args[0]]] };
            if (last === "any") return { __rs: 15, kind: "method_router", entries: [["ANY", args[0]]] };
        }
        if (parent === "Message" && (last === "Text" || last === "Binary" || last === "Ping" || last === "Pong" || last === "Close")) {
            return R.mkEnumTuple("Message", last, args);
        }
        if (last === "Json" && (pathParts.length === 1 || parent === "axum" || parent === "extract" || parent === "response")) {
            return R.mkTupleStruct("Json", [args[0]]);
        }
        if (last === "State" && (pathParts.length === 1 || parent === "axum" || parent === "extract")) {
            return R.mkTupleStruct("State", [args[0]]);
        }
        if (last === "Path" && (pathParts.length === 1 || parent === "axum" || parent === "extract")) {
            return R.mkTupleStruct("Path", [args[0]]);
        }
        if (last === "Query" && (pathParts.length === 1 || parent === "axum" || parent === "extract")) {
            return R.mkTupleStruct("Query", [args[0]]);
        }
        if (last === "Extension" && (pathParts.length === 1 || parent === "axum" || parent === "extract")) {
            return R.mkTupleStruct("Extension", [args[0]]);
        }
        if (last === "ConnectInfo" && (pathParts.length === 1 || parent === "axum" || parent === "extract")) {
            return R.mkTupleStruct("ConnectInfo", [args[0]]);
        }
        if (parent === "CorsLayer") {
            if (last === "new" || last === "permissive") {
                return { __rs: 15, kind: "cors_layer", allowAny: last === "permissive", predicate: null, origins: null, methods: [], headers: [] };
            }
        }
        if (parent === "AllowOrigin") {
            if (last === "predicate") return { __rs: 15, kind: "allow_origin", any: false, predicate: args[0] };
            if (last === "any") return { __rs: 15, kind: "allow_origin", any: true };
            if (last === "exact") return { __rs: 15, kind: "allow_origin", any: false, origins: [strVal(args[0])] };
            if (last === "list") {
                const it = R.intoIter(args[0]);
                const arr = [];
                for (;;) { const x = it.next(); if (x === R.ITER_DONE) break; arr.push(strVal(x)); }
                return { __rs: 15, kind: "allow_origin", any: false, origins: arr };
            }
        }
        if (parent === "CompressionLayer" && last === "new") return { __rs: 15, kind: "noop_layer" };
        if (parent === "TraceLayer" && (last === "new_for_http" || last === "new")) return { __rs: 15, kind: "noop_layer" };
        if (parent === "HeaderName" && (last === "from_static" || last === "from_bytes")) {
            const s = last === "from_bytes" ? bytesToStr(args[0]) : strVal(args[0]);
            const hn = { __rs: 15, kind: "header_name", n: s.toLowerCase() };
            return last === "from_bytes" ? Ok(hn) : hn;
        }
        if (parent === "HeaderValue") {
            if (last === "from_static") return { __rs: 15, kind: "header_value", v: strVal(args[0]) };
            if (last === "from_str") return Ok({ __rs: 15, kind: "header_value", v: strVal(args[0]) });
        }
        if (parent === "Query" && last === "from_request_parts") {
            const parts = deref(args[0]);
            const td = typeArgs && typeArgs[0] ? typeArgs[0] : "Value";
            return Promise.resolve(Ok(R.mkTupleStruct("Query", [fromQueryMap(parts && parts.queryMap, td)])));
        }
        if (parent === "Map" && (last === "new" || last === "with_capacity")) {
            return rt.mkMap(true);
        }
        if (parent === "Value") {
            if (last === "Object") return rt.mkJson(rt.toJsonRaw(args[0]));
            if (last === "Array") return rt.mkJson(rt.toJsonRaw(args[0]));
            if (last === "String") return rt.mkJson(strVal(args[0]));
            if (last === "Bool") return rt.mkJson(!!deref(args[0]));
            if (last === "Number") return rt.mkJson(Number(deref(args[0])));
        }
        if (parent === "Number" && (last === "from_f64" || last === "from")) {
            const n = Number(deref(args[0]));
            return last === "from_f64" ? (Number.isFinite(n) ? Some(n) : None) : n;
        }
        if (parent === "HeaderMap" && last === "new") return { __rs: 15, kind: "header_map", map: rt.mkMap(false) };
        if (parent === "StatusCode" && last === "from_u16") {
            const n = Number(deref(args[0])) | 0;
            return (n >= 100 && n <= 999) ? Ok(n) : Err(R.mkString("invalid status code"));
        }
        if (parent === "Body") {
            if (last === "empty") return { __rs: 15, kind: "body", data: "" };
            if (last === "from") return { __rs: 15, kind: "body", data: bytesToStr(args[0]) };
        }
        if (parent === "body" && last === "to_bytes") {
            const b = deref(args[0]);
            const data = (b && b.__rs === 15 && b.kind === "body") ? b.data : strVal(b);
            const bytes = rt.strToUtf8Bytes(data);
            const limit = args.length > 1 ? Number(deref(args[1])) : Infinity;
            if (bytes.length > limit) return Promise.resolve(Err(R.mkString("body exceeds limit")));
            return Promise.resolve(Ok({ __rs: 7, items: bytes }));
        }
        if (parent === "Request" && last === "from_parts") {
            const parts = deref(args[0]);
            const body = deref(args[1]);
            const req = parts && parts.req ? parts.req : rt.mkRequest("GET", "/", null, "", null);
            req.body = (body && body.__rs === 15 && body.kind === "body") ? body.data : bytesToStr(body);
            return req;
        }
        if (parent === "Response") {
            if (last === "builder") return { __rs: 15, kind: "resp_builder", status: 200, headers: [] };
            if (last === "new") {
                const b = deref(args[0]);
                const s = (b && b.__rs === 15 && b.kind === "body") ? b.data : strVal(b);
                return rt.mkResponse(200, s, null);
            }
            if (last === "from_parts") {
                const parts = deref(args[0]);
                const body = deref(args[1]);
                const resp = parts && parts.resp ? parts.resp : rt.mkResponse(200, "", null);
                resp.body = (body && body.__rs === 15 && body.kind === "body") ? body.data : bytesToStr(body);
                return resp;
            }
        }

        // ---- tokio ----
        if (root === "tokio" && parent === "Mutex" && last === "new") {
            return { __rs: 14, kind: "tokio_mutex", v: args[0] };
        }
        if (root === "tokio" && parent === "RwLock" && last === "new") {
            return { __rs: 14, kind: "tokio_rwlock", v: args[0] };
        }
        if (root === "tokio" && parent === "TcpListener" && last === "bind") {
            const addr = strVal(args[0]);
            if (rt.__inWorkerBoot || (typeof __zipp_rs_io === "function" && __zipp_rs_io("mode", "") === "worker_boot")) {
                return Promise.resolve(Ok({ __rs: 15, kind: "tcp_listener", id: 0, virtual: true, addr: addr }));
            }
            const id = rt.netCall("bind", addr);
            return Promise.resolve(
                (typeof id === "number" && id >= 0)
                    ? Ok({ __rs: 15, kind: "tcp_listener", id: id })
                    : Err(rt.mkIoErr("AddrInUse", "failed to bind " + addr))
            );
        }
        if (parent === "tokio" && last === "spawn") {
            return mkJoinHandle(args[0]);
        }
        if (parent === "task" && last === "spawn_blocking") {
            const fn = args[0];
            return mkJoinHandle(new Promise(function (resolve, reject) {
                try { resolve(fn()); } catch (e) { reject(e); }
            }));
        }
        if (parent === "watch" && last === "channel") {
            const cell = { val: args[0], version: 0, waiters: [] };
            return R.mkTuple([
                { __rs: 15, kind: "watch_tx", cell: cell },
                { __rs: 15, kind: "watch_rx", cell: cell, seen: 0 }
            ]);
        }
        if (parent === "broadcast" && last === "channel") {
            const bus = { cap: Math.max(1, Number(deref(args[0])) | 0), subs: [] };
            const sub = { queue: [], qhead: 0, waiters: [], lagged: 0, closed: false };
            bus.subs.push(sub);
            return R.mkTuple([
                { __rs: 15, kind: "broadcast_tx", bus: bus },
                { __rs: 15, kind: "broadcast_rx", bus: bus, sub: sub }
            ]);
        }
        if (parent === "mpsc" && (last === "channel" || last === "unbounded_channel")) {
            const ch = { queue: [], qhead: 0, waiters: [] };
            return R.mkTuple([
                { __rs: 15, kind: "mpsc_tx", ch: ch },
                { __rs: 15, kind: "mpsc_rx", ch: ch }
            ]);
        }
        if (root === "tokio" && parent === "time") {
            if (last === "sleep") {
                const dur = deref(args[0]);
                const ms = dur && dur.kind === "duration" ? Number(dur.ns / 1000000n) : Number(dur);
                return scheduleTimer(ms);
            }
            if (last === "interval") {
                const dur = deref(args[0]);
                const ms = dur && dur.kind === "duration" ? Number(dur.ns / 1000000n) : Number(dur);
                const iv = { __rs: 15, kind: "tokio_interval", ms: Math.max(1, ms), first: true, aborted: false, pendingTimer: null };
                trackSpawnResource(iv);
                return iv;
            }
        }
        if (parent === "signal" && last === "ctrl_c") {
            // Pending Promise that stays quiescent until process termination
            return new Promise(function () {});
        }

        // ---- argon2 ----
        if (parent === "Params" && last === "new") {
            return Ok({
                __rs: 15,
                kind: "argon2_params",
                m: Number(deref(args[0])) || 19456,
                t: Number(deref(args[1])) || 2,
                p: Number(deref(args[2])) || 1
            });
        }
        if (parent === "Argon2") {
            if (last === "new") {
                const pr = deref(args[2]);
                return { __rs: 15, kind: "argon2", m: pr ? pr.m : 19456, t: pr ? pr.t : 2, p: pr ? pr.p : 1 };
            }
            if (last === "default") return { __rs: 15, kind: "argon2", m: 19456, t: 2, p: 1 };
        }
        if (parent === "SaltString" && last === "generate") {
            return { __rs: 15, kind: "salt_string", s: rt.cryptoCall("random_bytes", "16") || "" };
        }
        if (parent === "PasswordHash" && last === "new") {
            const s = strVal(args[0]);
            if (s.startsWith("$argon2") || s.startsWith("$zsha256$")) {
                return Ok({ __rs: 15, kind: "argon2_hash", s: s });
            }
            return Err(R.mkString("invalid password hash format"));
        }

        // ---- jsonwebtoken ----
        if (parent === "Header" && (last === "default" || last === "new")) {
            return { __rs: 15, kind: "jwt_header", alg: "HS256" };
        }
        if (parent === "EncodingKey" && last === "from_secret") {
            return { __rs: 15, kind: "jwt_key", hex: bytesToHex(args[0]) };
        }
        if (parent === "DecodingKey" && last === "from_secret") {
            return { __rs: 15, kind: "jwt_key", hex: bytesToHex(args[0]) };
        }
        if (parent === "Validation" && (last === "default" || last === "new")) {
            return { __rs: 15, kind: "jwt_validation", leeway: 60, validate_exp: true };
        }
        if ((parent === "jsonwebtoken" || pathParts.length === 1) && last === "encode" && args.length === 3) {
            const claimsJson = JSON.stringify(rt.toJsonRaw(args[1]));
            const key = deref(args[2]);
            const tok = rt.cryptoCall("jwt_sign_hex", claimsJson, key ? key.hex : "");
            return typeof tok === "string" && tok.length > 0
                ? Ok(R.mkString(tok))
                : Err(R.mkString("jwt encode failed"));
        }
        if ((parent === "jsonwebtoken" || pathParts.length === 1) && last === "decode" && args.length === 3) {
            const tok = strVal(args[0]);
            const key = deref(args[1]);
            const val = deref(args[2]);
            const raw = rt.cryptoCall("jwt_verify_hex", tok, key ? key.hex : "");
            if (typeof raw !== "string") return Err(R.mkString("invalid JWT signature"));
            try {
                const parsed = JSON.parse(raw);
                if (parsed && typeof parsed.exp === "number" && (!val || val.validate_exp !== false)) {
                    const nowSec = Number(rt.timeCall("unix") / 1000000000n);
                    const leeway = val && typeof val.leeway === "number" ? val.leeway : 60;
                    if (nowSec > parsed.exp + leeway) return Err(R.mkString("JWT expired"));
                }
                const claims = rt.fromJsonRaw(parsed, typeArgs && typeArgs[0] ? typeArgs[0] : "");
                return Ok({ __rs: 1, __type: "TokenData", __tuple: false, header: { __rs: 15, kind: "jwt_header" }, claims: claims });
            } catch (e) {
                return Err(R.mkString("invalid JWT payload: " + String(e)));
            }
        }

        // ---- time ----
        if (parent === "OffsetDateTime") {
            if (last === "now_utc") return mkOffsetDateTime(rt.timeCall("unix"));
            if (last === "from_unix_timestamp") {
                const s = BigInt(Math.trunc(Number(deref(args[0]))));
                return Ok(mkOffsetDateTime(s * 1000000000n));
            }
            if (last === "from_unix_timestamp_nanos") {
                const raw = deref(args[0]);
                const ns = typeof raw === "bigint" ? raw : BigInt(Math.trunc(Number(raw)));
                return Ok(mkOffsetDateTime(ns));
            }
            if (last === "parse") {
                const raw = strVal(args[0]).trim();
                const norm = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}/.test(raw) ? raw.replace(" ", "T") + "Z" : raw;
                const ms = Date.parse(norm);
                return Number.isNaN(ms) ? Err(R.mkString("invalid datetime: " + raw)) : Ok(mkOffsetDateTime(BigInt(ms) * 1000000n));
            }
        }
        if (root === "time" && parent === "Duration") {
            if (last === "seconds") return { __rs: 15, kind: "duration", ns: BigInt(Math.trunc(Number(deref(args[0])))) * 1000000000n };
            if (last === "minutes") return { __rs: 15, kind: "duration", ns: BigInt(Math.trunc(Number(deref(args[0])))) * 60000000000n };
            if (last === "hours") return { __rs: 15, kind: "duration", ns: BigInt(Math.trunc(Number(deref(args[0])))) * 3600000000000n };
            if (last === "days") return { __rs: 15, kind: "duration", ns: BigInt(Math.trunc(Number(deref(args[0])))) * 86400000000000n };
            if (last === "milliseconds") return { __rs: 15, kind: "duration", ns: BigInt(Math.trunc(Number(deref(args[0])))) * 1000000n };
        }

        // ---- uuid / sha2 / hmac / hex / rand / base64 ----
        if (parent === "Uuid") {
            if (last === "new_v4") return mkUuid(rt.cryptoCall("uuid_v4", "") || "00000000-0000-4000-8000-000000000000");
            if (last === "parse_str") return rt.parseUuid(strVal(args[0]));
        }
        if (parent === "Sha256") {
            if (last === "new") return { __rs: 15, kind: "sha256_hasher", chunks: [] };
            if (last === "digest") {
                const hx = rt.cryptoCall("sha256_bytes_hex", bytesToHex(args[0])) || "";
                return { __rs: 15, kind: "digest_bytes", hex: hx, bytes: hexToBytes(hx) };
            }
        }
        if (parent === "Hmac" && last === "new_from_slice") {
            return Ok({ __rs: 15, kind: "hmac_sha256", keyHex: bytesToHex(args[0]), chunks: [] });
        }
        if (parent === "hex") {
            if (last === "encode") return R.mkString(bytesToHex(args[0]));
            if (last === "decode") return Ok({ __rs: 7, items: hexToBytes(strVal(args[0])) });
        }
        if (parent === "rand") {
            if (last === "thread_rng") return { __rs: 15, kind: "rng" };
            if (last === "random") return randForType(typeArgs && typeArgs[0] ? typeArgs[0] : "f64");
        }

        // ---- reqwest / fs2 / anyhow / std::net / std::cmp::Reverse / std::thread::Builder / std::ptr / windows_sys ----
        if (parent === "Client" && last === "builder") {
            return { __rs: 15, kind: "http_client_builder" };
        }
        if (parent === "fs2") {
            if (last === "available_space") return Ok(10000000000);
            if (last === "total_space") return Ok(100000000000);
        }
        if (parent === "anyhow") {
            if (last === "Ok") return Ok(args[0]);
            if (last === "anyhow") return { __rs: 15, kind: "anyhow_error", msg: strVal(args[0]), inner: null };
        }
        if (parent === "Error" && last === "last_os_error") {
            return rt.mkIoErr("Other", "OS API unavailable in ZIPP VM");
        }
        if (parent === "Error" && last === "from_raw_os_error") {
            return rt.mkIoErr("Other", "OS error " + String(deref(args[0])));
        }
        if (parent === "Error" && last === "msg") {
            return { __rs: 15, kind: "anyhow_error", msg: strVal(args[0]), inner: null };
        }
        if (parent === "SocketAddr" && (last === "new" || last === "from")) {
            if (args.length === 2) return mkSocketAddr(strVal(args[0]), Number(deref(args[1])));
            const a = deref(args[0]);
            if (a && a.__rs === 3 && a.__items.length === 2) {
                const ip = deref(a.__items[0]);
                const ipStr = (ip && ip.__rs === 7) ? ip.items.join(".") : strVal(ip);
                return mkSocketAddr(ipStr, Number(deref(a.__items[1])));
            }
            return mkSocketAddr("127.0.0.1", 0);
        }
        if ((parent === "cmp" || pathParts.length === 1) && last === "Reverse") {
            return R.mkTupleStruct("Reverse", [args[0]]);
        }
        if (parent === "Builder" && last === "new") {
            return { __rs: 15, kind: "thread_builder" };
        }
        if (parent === "ptr" && (last === "null" || last === "null_mut")) {
            return null;
        }
        if (root === "windows_sys") {
            // Safe no-op fallback for raw Win32 FFI calls (e.g. OpenProcessToken / OpenProcess)
            return 0;
        }
        if (parent === "serde_json" && last === "from_value") {
            const v = deref(args[0]);
            const raw = (v && v.__rs === 15 && v.kind === "json") ? v.v : rt.toJsonRaw(v);
            try { return Ok(rt.fromJsonRaw(raw, typeArgs && typeArgs[0] ? typeArgs[0] : "")); }
            catch (e) { return Err(R.mkString(String(e))); }
        }
        if (parent === "serde_json" && last === "from_slice") {
            try {
                const raw = JSON.parse(bytesToStr(args[0]));
                return Ok(rt.fromJsonRaw(raw, typeArgs && typeArgs[0] ? typeArgs[0] : ""));
            } catch (e) {
                return Err(R.mkString(String(e)));
            }
        }
        // `str::to_owned` / `String::from` passed as a function value or called as associated fn
        if (parent === "str" && last === "to_owned") return R.mkString(strVal(args[0]));

        return ECO_MISS;
    };

    // ---- Ecosystem Constants & Function Values ----
    rt.resolveEcoConst = function (pathParts) {
        const last = pathParts[pathParts.length - 1];
        const parent = pathParts.length >= 2 ? pathParts[pathParts.length - 2] : "";
        const root = pathParts[0] || "";
        if (parent === "Method") {
            return R.mkEnumUnit("Method", last, null);
        }
        if (parent === "header") {
            const hdrs = {
                AUTHORIZATION: "authorization",
                CONTENT_TYPE: "content-type",
                ACCEPT: "accept",
                ORIGIN: "origin",
                RETRY_AFTER: "retry-after",
                LOCATION: "location",
                CACHE_CONTROL: "cache-control",
                CONTENT_DISPOSITION: "content-disposition",
                CONTENT_LENGTH: "content-length",
                SET_COOKIE: "set-cookie",
                COOKIE: "cookie",
                USER_AGENT: "user-agent",
                HOST: "host",
                VARY: "vary",
                ETAG: "etag"
            };
            if (Object.prototype.hasOwnProperty.call(hdrs, last)) {
                return { __rs: 15, kind: "header_name", n: hdrs[last] };
            }
            return { __rs: 15, kind: "header_name", n: last.toLowerCase().replace(/_/g, "-") };
        }
        if (parent === "SqliteJournalMode" || parent === "SqliteSynchronous" || parent === "LevelFilter"
            || parent === "Algorithm" || parent === "Version") {
            return R.mkEnumUnit(parent, last, null);
        }
        if (last === "Rfc3339") return R.mkString("rfc3339");
        if (last === "OsRng") return { __rs: 15, kind: "rng" };
        if (last === "STANDARD") return { __rs: 15, kind: "base64_engine", url: false };
        if (last === "URL_SAFE_NO_PAD" || last === "URL_SAFE") return { __rs: 15, kind: "base64_engine", url: true };
        // Function items passed by value (e.g. `.map(str::to_owned)`, `.map(PathBuf::from)`, `.filter_map(Result::ok)`)
        if (parent === "str" && last === "to_owned") return function (s) { return R.mkString(strVal(s)); };
        if (parent === "PathBuf" && last === "from") return function (p) { return rt.mkPath(p); };
        if (parent === "String" && last === "from") return function (s) { return R.mkString(strVal(s)); };
        if (parent === "Result" && last === "ok") return function (r) {
            const dr = deref(r);
            return (dr && dr.__rs === 2 && dr.__var === "Ok") ? Some(dr.__data[0]) : None;
        };
        if (parent === "Result" && last === "err") return function (r) {
            const dr = deref(r);
            return (dr && dr.__rs === 2 && dr.__var === "Err") ? Some(dr.__data[0]) : None;
        };
        if (parent === "Option" && last === "is_some") return function (o) {
            const d = deref(o);
            return !!(d && d.__rs === 2 && d.__var === "Some");
        };
        if (parent === "Option" && last === "is_none") return function (o) {
            const d = deref(o);
            return !(d && d.__rs === 2 && d.__var === "Some");
        };
        if (root === "windows_sys") return 0;
        return ECO_MISS;
    };
})(__zipp_rs);
