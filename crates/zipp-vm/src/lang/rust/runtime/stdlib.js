/* ZIPP Rust runtime — standard library & backend ecosystem:
 * std::io, std::fs, std::path, std::env, std::process, std::time, std::thread,
 * std::mem, std::cmp, std::iter, std::panic, std::net (TCP), serde_json, regex,
 * crypto, kv, and built-in HTTP/1.1 Router / Request / Response / Client / Server. Apache-2.0.
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
    const Some = R.Some;
    const None = R.None;
    const Ok = R.Ok;
    const Err = R.Err;
    const ITER_DONE = R.ITER_DONE;

    // ---- io::Error ----
    function mkIoErr(kind, msg) {
        return { __rs: 15, kind: "io_error", errKind: kind || "Other", msg: String(msg) };
    }
    rt.mkIoErr = mkIoErr;

    // ---- Serde case renaming & nested JSON serialization/deserialization ----
    function applyRenameAll(name, rule) {
        if (!rule) return name;
        switch (rule) {
            case "camelCase":
                return name.replace(/_([a-zA-Z0-9])/g, function (_, c) { return c.toUpperCase(); });
            case "PascalCase": {
                const c = name.replace(/_([a-zA-Z0-9])/g, function (_, ch) { return ch.toUpperCase(); });
                return c.length > 0 ? c[0].toUpperCase() + c.slice(1) : c;
            }
            case "snake_case":
                return name.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
            case "kebab-case":
                return name.replace(/_/g, "-").replace(/([a-z0-9])([A-Z])/g, "$1-$2").toLowerCase();
            case "UPPERCASE":
                return name.toUpperCase();
            case "lowercase":
                return name.toLowerCase();
            case "SCREAMING_SNAKE_CASE":
                return name.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toUpperCase();
            default:
                return name;
        }
    }

    function serdeFieldKey(fieldName, fieldMeta, containerMeta) {
        if (fieldMeta && fieldMeta.rename) return fieldMeta.rename;
        if (containerMeta && containerMeta.rename_all) return applyRenameAll(fieldName, containerMeta.rename_all);
        return fieldName;
    }

    function mkJson(v) {
        return { __rs: 15, kind: "json", v: v === undefined ? null : v };
    }
    rt.mkJson = mkJson;
    R.mkJson = mkJson;

    function toJsonRaw(v) {
        v = deref(v);
        if (v === undefined || v === null) return null;
        const t = typeof v;
        if (t === "boolean" || t === "number" || t === "string") return v;
        if (t === "bigint") return Number(v);
        if (t !== "object") return null;
        switch (v.__rs) {
            case 4: return v.s;
            case 5: return v.ch;
            case 12:
                return rt.dequeToArray(v).map(toJsonRaw);
            case 7:
            case 13:
                return v.items.map(toJsonRaw);
            case 3:
                return v.__items.map(toJsonRaw);
            case 1: {
                if (v.__tuple) {
                    const arr = [];
                    for (let i = 0; i < v.__len; i++) arr.push(toJsonRaw(v[i]));
                    return arr;
                }
                const st = rt.structs[v.__type];
                const cMeta = st && st.serde ? st.serde : null;
                const fMetas = (cMeta && cMeta.fields) ? cMeta.fields : null;
                const out = {};
                const ks = Object.keys(v);
                for (let i = 0; i < ks.length; i++) {
                    const k = ks[i];
                    if (k === "__rs" || k === "__type" || k === "__tuple" || k === "__len") continue;
                    const fm = fMetas ? fMetas[k] : null;
                    if (fm && (fm.skip || fm.skip_serializing)) continue;
                    const fv = v[k];
                    if (fm && fm.skip_serializing_if === "Option::is_none") {
                        const dv = deref(fv);
                        if (dv && dv.__rs === 2 && dv.__enum === "Option" && dv.__var === "None") continue;
                    }
                    if (fm && fm.flatten) {
                        const sub = toJsonRaw(fv);
                        if (sub && typeof sub === "object" && !Array.isArray(sub)) Object.assign(out, sub);
                        continue;
                    }
                    const jsonKey = serdeFieldKey(k, fm, cMeta);
                    out[jsonKey] = toJsonRaw(fv);
                }
                return out;
            }
            case 2: {
                if (v.__enum === "Option") {
                    return v.__var === "Some" ? toJsonRaw(v.__data[0]) : null;
                }
                const en = rt.enums[v.__enum];
                const cMeta = en && en.serde ? en.serde : null;
                const vKey = (cMeta && cMeta.rename_all) ? applyRenameAll(v.__var, cMeta.rename_all) : v.__var;
                if (v.__kind === 0) return vKey;
                if (v.__kind === 1) {
                    const o = {};
                    o[vKey] = v.__data.length === 1 ? toJsonRaw(v.__data[0]) : v.__data.map(toJsonRaw);
                    return o;
                }
                const o = {}, inner = {};
                const ks = Object.keys(v.__data);
                for (let i = 0; i < ks.length; i++) inner[ks[i]] = toJsonRaw(v.__data[ks[i]]);
                o[vKey] = inner;
                return o;
            }
            case 10: {
                const out = {};
                const entries = rt.mapEntries(v);
                for (let i = 0; i < entries.length; i++) {
                    out[strVal(entries[i][0])] = toJsonRaw(entries[i][1]);
                }
                return out;
            }
            case 11:
                return rt.setItems(v).map(toJsonRaw);
            case 15:
                if (v.kind === "json") return v.v;
                if (v.kind === "path") return v.p;
                if (v.kind === "datetime") return callStdMethod(v, "to_rfc3339", []).s;
                return null;
            default:
                return v;
        }
    }
    rt.toJsonRaw = toJsonRaw;

    function parseTypeDesc(desc) {
        if (!desc) return { base: "", args: [] };
        const lt = desc.indexOf("<");
        if (lt < 0) return { base: desc.trim(), args: [] };
        const base = desc.slice(0, lt).trim();
        const inner = desc.slice(lt + 1, desc.lastIndexOf(">"));
        const args = [];
        let depth = 0, cur = "";
        for (let i = 0; i < inner.length; i++) {
            const c = inner[i];
            if (c === "<") { depth++; cur += c; }
            else if (c === ">") { depth--; cur += c; }
            else if (c === "," && depth === 0) { args.push(cur.trim()); cur = ""; }
            else cur += c;
        }
        if (cur.trim()) args.push(cur.trim());
        return { base: base, args: args };
    }

    function fromJsonRaw(raw, targetDesc) {
        const td = parseTypeDesc(targetDesc);
        const targetType = td.base;
        const shortTarget = targetType && targetType.indexOf("::") >= 0 ? targetType.slice(targetType.lastIndexOf("::") + 2) : targetType;
        const stLookup = targetType ? (rt.structs[targetType] || rt.structs[shortTarget]) : null;
        if (stLookup && raw && typeof raw === "object" && !Array.isArray(raw)) {
            const st = stLookup;
            const cMeta = st.serde || null;
            const fMetas = (cMeta && cMeta.fields) ? cMeta.fields : null;
            const obj = { __rs: 1, __type: st.shortName || shortTarget, __tuple: false };
            for (let i = 0; i < st.fields.length; i++) {
                const fname = st.fields[i][0];
                const ftype = st.fields[i][1];
                const fm = fMetas ? fMetas[fname] : null;
                const jkey = serdeFieldKey(fname, fm, cMeta);
                if (fm && (fm.skip || fm.skip_deserializing)) {
                    obj[fname] = rt.defaultOf(parseTypeDesc(ftype).base);
                } else if (Object.prototype.hasOwnProperty.call(raw, jkey)) {
                    obj[fname] = fromJsonRaw(raw[jkey], ftype);
                } else if (Object.prototype.hasOwnProperty.call(raw, fname)) {
                    obj[fname] = fromJsonRaw(raw[fname], ftype);
                } else if (fm && fm.alias && Object.prototype.hasOwnProperty.call(raw, fm.alias)) {
                    obj[fname] = fromJsonRaw(raw[fm.alias], ftype);
                } else if (fm && typeof fm.default_fn === "function") {
                    obj[fname] = fm.default_fn();
                } else {
                    obj[fname] = rt.defaultOf(parseTypeDesc(ftype).base);
                }
            }
            return obj;
        }
        const enLookup = targetType ? (rt.enums[targetType] || rt.enums[shortTarget]) : null;
        if (enLookup) {
            const en = enLookup;
            const enName = en.shortName || shortTarget;
            const cMeta = en.serde || null;
            const matchVariant = function (k) {
                const vs = Object.keys(en.variants || {});
                for (let i = 0; i < vs.length; i++) {
                    const vn = vs[i];
                    if (vn === k || (cMeta && cMeta.rename_all && applyRenameAll(vn, cMeta.rename_all) === k)) {
                        return vn;
                    }
                }
                return null;
            };
            if (typeof raw === "string") {
                const vn = matchVariant(raw);
                if (vn === null) throw new Error("unknown variant `" + raw + "` for `" + enName + "`");
                const disc = en.variants && Object.prototype.hasOwnProperty.call(en.variants, vn) ? en.variants[vn] : null;
                return R.mkEnumUnit(enName, vn, disc);
            }
            if (raw && typeof raw === "object" && !Array.isArray(raw)) {
                const ks = Object.keys(raw);
                if (ks.length === 1) {
                    const vn = matchVariant(ks[0]);
                    if (vn === null) throw new Error("unknown variant `" + ks[0] + "` for `" + enName + "`");
                    const val = raw[ks[0]];
                    if (Array.isArray(val)) return R.mkEnumTuple(enName, vn, val.map(function (x) { return fromJsonRaw(x, ""); }));
                    if (val && typeof val === "object") {
                        const d = {};
                        const iks = Object.keys(val);
                        for (let i = 0; i < iks.length; i++) d[iks[i]] = fromJsonRaw(val[iks[i]], "");
                        return R.mkEnumStruct(enName, vn, d);
                    }
                    return R.mkEnumTuple(enName, vn, [fromJsonRaw(val, "")]);
                }
            }
        }
        if (targetType === "Option") {
            return raw === null || raw === undefined ? None : Some(fromJsonRaw(raw, td.args[0] || ""));
        }
        if (targetType === "String") return R.mkString(raw === null || raw === undefined ? "" : String(raw));
        if (targetType === "Vec" && Array.isArray(raw)) {
            const elemDesc = td.args[0] || "";
            return { __rs: 7, items: raw.map(function (x) { return fromJsonRaw(x, elemDesc); }) };
        }
        if ((targetType === "HashMap" || targetType === "BTreeMap") && raw && typeof raw === "object" && !Array.isArray(raw)) {
            const m = rt.mkMap(targetType === "BTreeMap");
            const valDesc = td.args[1] || "";
            const ks = Object.keys(raw);
            for (let i = 0; i < ks.length; i++) rt.mapSetRaw(m, R.mkString(ks[i]), fromJsonRaw(raw[ks[i]], valDesc));
            return m;
        }
        if (targetType === "Value" || targetType === "serde_json::Value" || !targetType) {
            return mkJson(raw);
        }
        if (typeof raw === "string") return R.mkString(raw);
        if (Array.isArray(raw)) return { __rs: 7, items: raw.map(function (x) { return fromJsonRaw(x, ""); }) };
        return raw;
    }

    rt.parseTypeDesc = parseTypeDesc;
    rt.fromJsonRaw = fromJsonRaw;

    // ---- Path helpers ----
    function mkPath(p) {
        return { __rs: 15, kind: "path", p: strVal(p) };
    }
    function pathJoin(a, b) {
        const sb = strVal(b);
        if (sb.startsWith("/") || /^[A-Za-z]:[\\/]/.test(sb)) return sb;
        const sa = strVal(a).replace(/[\\/]+$/, "");
        return sa === "" ? sb : sa + "/" + sb;
    }
    function pathBase(p) {
        const clean = p.replace(/[\\/]+$/, "");
        const idx = Math.max(clean.lastIndexOf("/"), clean.lastIndexOf("\\"));
        return idx < 0 ? clean : clean.slice(idx + 1);
    }

    // ---- Native bridge helpers ----
    function fsCall(op, a, b, c) {
        if (typeof __zipp_rs_fs !== "function") return null;
        return __zipp_rs_fs(op, a === undefined ? "" : a, b === undefined ? "" : b, c === undefined ? "" : c);
    }
    function envCall(op, a, b) {
        if (typeof __zipp_rs_env !== "function") return null;
        return __zipp_rs_env(op, a === undefined ? "" : a, b === undefined ? "" : b);
    }
    function timeCall(op, a) {
        if (typeof __zipp_rs_time !== "function") {
            if (op === "mono" || op === "unix") return BigInt(Date.now()) * 1000000n;
            return null;
        }
        return __zipp_rs_time(op, a === undefined ? 0 : a);
    }
    function netCall(op, a, b, c) {
        if (typeof __zipp_rs_net !== "function") return null;
        return __zipp_rs_net(op, a === undefined ? "" : a, b === undefined ? "" : b, c === undefined ? "" : c);
    }
    function cryptoCall(op, a, b) {
        if (typeof __zipp_rs_crypto !== "function") return null;
        return __zipp_rs_crypto(op, a === undefined ? "" : a, b === undefined ? "" : b);
    }
    function procCall(op, a, b, c) {
        if (typeof __zipp_rs_proc !== "function") return null;
        return __zipp_rs_proc(op, a === undefined ? "" : a, b === undefined ? "" : b, c === undefined ? "" : c);
    }

    rt.fsCall = fsCall;
    rt.envCall = envCall;
    rt.timeCall = timeCall;
    rt.netCall = netCall;
    rt.cryptoCall = cryptoCall;
    rt.procCall = procCall;
    rt.mkPath = mkPath;
    rt.statusReason = statusReason;
    rt.compileRoutePattern = compileRoutePattern;
    rt.matchRoute = matchRoute;
    rt.joinRoutePrefix = joinRoutePrefix;
    rt.mimeForPath = mimeForPath;
    rt.parseQueryString = parseQueryString;
    rt.parseCookies = parseCookies;

    // ---- TCP incoming iterator ----
    function tcpIncomingIter(listener) {
        return rt.mkIter(function () {
            const res = callStdMethod(listener, "accept", []);
            if (res.__var === "Ok") return Ok(res.__data[0].__items[0]);
            return res;
        });
    }
    rt.tcpIncomingIter = tcpIncomingIter;

    // ---- Built-in HTTP/1.1 Request, Response, Router & Server ----
    function parseQueryString(qs) {
        const m = rt.mkMap(false);
        if (!qs) return m;
        const parts = qs.split("&");
        for (let i = 0; i < parts.length; i++) {
            const p = parts[i];
            if (!p) continue;
            const eqi = p.indexOf("=");
            const k = decodeURIComponent((eqi >= 0 ? p.slice(0, eqi) : p).replace(/\+/g, " "));
            const v = eqi >= 0 ? decodeURIComponent(p.slice(eqi + 1).replace(/\+/g, " ")) : "";
            rt.mapSetRaw(m, R.mkString(k), R.mkString(v));
        }
        return m;
    }

    function mkRequest(method, uri, headersObj, bodyStr, paramsObj) {
        const qidx = uri.indexOf("?");
        const path = qidx >= 0 ? uri.slice(0, qidx) : uri;
        const queryStr = qidx >= 0 ? uri.slice(qidx + 1) : "";
        const headers = rt.mkMap(false);
        if (headersObj) {
            if (Array.isArray(headersObj)) {
                for (let i = 0; i < headersObj.length; i++) {
                    const pair = headersObj[i];
                    if (Array.isArray(pair) && pair.length >= 2) {
                        rt.mapSetRaw(headers, R.mkString(String(pair[0]).toLowerCase()), R.mkString(String(pair[1])));
                    }
                }
            } else {
                const ks = Object.keys(headersObj);
                for (let i = 0; i < ks.length; i++) {
                    rt.mapSetRaw(headers, R.mkString(ks[i].toLowerCase()), R.mkString(headersObj[ks[i]]));
                }
            }
        }
        const params = rt.mkMap(false);
        if (paramsObj) {
            const ks = Object.keys(paramsObj);
            for (let i = 0; i < ks.length; i++) {
                rt.mapSetRaw(params, R.mkString(ks[i]), R.mkString(paramsObj[ks[i]]));
            }
        }
        return {
            __rs: 15,
            kind: "request",
            method: String(method || "GET").toUpperCase(),
            uri: String(uri || "/"),
            path: path,
            queryStr: queryStr,
            queryMap: parseQueryString(queryStr),
            headers: headers,
            params: params,
            pathParamList: [],
            extensions: Object.create(null),
            peerAddr: null,
            body: typeof bodyStr === "string" ? bodyStr : strVal(bodyStr || ""),
            state: undefined
        };
    }
    rt.mkRequest = mkRequest;

    function mkResponse(status, body, contentType) {
        const headers = rt.mkMap(false);
        if (contentType) {
            rt.mapSetRaw(headers, R.mkString("Content-Type"), R.mkString(contentType));
        }
        return {
            __rs: 15,
            kind: "response",
            status: status | 0,
            headers: headers,
            body: typeof body === "string" ? body : strVal(body || "")
        };
    }
    rt.mkResponse = mkResponse;

    function statusReason(code) {
        switch (code) {
            case 200: return "OK";
            case 201: return "Created";
            case 204: return "No Content";
            case 301: return "Moved Permanently";
            case 302: return "Found";
            case 304: return "Not Modified";
            case 400: return "Bad Request";
            case 401: return "Unauthorized";
            case 403: return "Forbidden";
            case 404: return "Not Found";
            case 405: return "Method Not Allowed";
            case 409: return "Conflict";
            case 422: return "Unprocessable Entity";
            case 429: return "Too Many Requests";
            case 500: return "Internal Server Error";
            case 502: return "Bad Gateway";
            case 503: return "Service Unavailable";
            default: return "OK";
        }
    }

    function compileRoutePattern(pattern) {
        const clean = pattern.replace(/\/+$/, "") || "/";
        const segs = clean === "/" ? [] : clean.slice(1).split("/");
        return { raw: clean, segs: segs };
    }

    function matchRoute(route, reqPath) {
        const clean = reqPath.replace(/\/+$/, "") || "/";
        const segs = clean === "/" ? [] : clean.slice(1).split("/");
        const rsegs = route.pat.segs;
        const params = {};
        const paramList = [];
        let i = 0;
        for (; i < rsegs.length; i++) {
            const rs = rsegs[i];
            if (rs.startsWith("*")) {
                const v = segs.slice(i).join("/");
                params[rs.slice(1) || "wildcard"] = v;
                paramList.push(v);
                return { map: params, list: paramList };
            }
            if (i >= segs.length) return null;
            if (rs.startsWith(":")) {
                const v = decodeURIComponent(segs[i]);
                params[rs.slice(1)] = v;
                paramList.push(v);
            } else if (rs.startsWith("{") && rs.endsWith("}")) {
                const v = decodeURIComponent(segs[i]);
                params[rs.slice(1, -1)] = v;
                paramList.push(v);
            } else if (rs !== segs[i]) {
                return null;
            }
        }
        return i === segs.length ? { map: params, list: paramList } : null;
    }

    function coerceHandlerResult(res) {
        res = R.blockOn(res);
        return coerceHandlerValue(res);
    }
    function coerceHandlerValue(res) {
        res = deref(res);
        if (res && res.__rs === 15 && res.kind === "response") return res;
        if (res && res.__rs === 2 && res.__enum === "Result") {
            if (res.__var === "Ok") return coerceHandlerValue(res.__data[0]);
            const errVal = deref(res.__data[0]);
            if (errVal && (errVal.__rs === 1 || errVal.__rs === 2)) {
                const tag = errVal.__rs === 1 ? errVal.__type : errVal.__enum;
                const intoResp = rt.lookupUserMethod(tag, "into_response");
                if (intoResp) return coerceHandlerValue(intoResp(errVal));
            }
            return mkResponse(500, rt.fmtDisplay(errVal), "text/plain; charset=utf-8");
        }
        if (res && res.__rs === 1 && res.__type === "Json" && res.__tuple) {
            return mkResponse(200, JSON.stringify(toJsonRaw(res[0])), "application/json; charset=utf-8");
        }
        if (res && (res.__rs === 1 || res.__rs === 2)) {
            const tag = res.__rs === 1 ? res.__type : res.__enum;
            const intoResp = rt.lookupUserMethod(tag, "into_response");
            if (intoResp) return coerceHandlerValue(intoResp(res));
        }
        if (res && res.__rs === 3 && res.__items.length >= 2 && typeof deref(res.__items[0]) === "number") {
            const code = Number(deref(res.__items[0])) | 0;
            if (res.__items.length === 2) {
                const bodyResp = coerceHandlerValue(res.__items[1]);
                bodyResp.status = code;
                return bodyResp;
            }
            if (res.__items.length === 3) {
                const hdrs = deref(res.__items[1]);
                const bodyResp = coerceHandlerValue(res.__items[2]);
                bodyResp.status = code;
                if (hdrs && hdrs.__rs === 7) {
                    for (let i = 0; i < hdrs.items.length; i++) {
                        const pair = deref(hdrs.items[i]);
                        if (pair && pair.__rs === 3) {
                            rt.mapSetRaw(bodyResp.headers, R.mkString(strVal(pair.__items[0])), R.mkString(strVal(pair.__items[1])));
                        }
                    }
                } else if (hdrs && hdrs.__rs === 15 && hdrs.kind === "header_map") {
                    const es = rt.mapEntries(hdrs.map);
                    for (let i = 0; i < es.length; i++) rt.mapSetRaw(bodyResp.headers, es[i][0], es[i][1]);
                }
                return bodyResp;
            }
        }
        if (typeof res === "number") {
            return mkResponse(res | 0, "", "text/plain; charset=utf-8");
        }
        if (typeof res === "string" || (res && res.__rs === 4)) {
            return mkResponse(200, strVal(res), "text/plain; charset=utf-8");
        }
        if (res && res.__rs === 15 && res.kind === "json") {
            return mkResponse(200, JSON.stringify(res.v), "application/json; charset=utf-8");
        }
        if (res === undefined || res === null) {
            return mkResponse(204, "", "text/plain; charset=utf-8");
        }
        return mkResponse(200, JSON.stringify(toJsonRaw(res)), "application/json; charset=utf-8");
    }
    rt.intoResponse = coerceHandlerValue;
    rt.coerceHandlerValue = coerceHandlerValue;

    function mimeForPath(p) {
        const lower = p.toLowerCase();
        if (lower.endsWith(".html") || lower.endsWith(".htm")) return "text/html; charset=utf-8";
        if (lower.endsWith(".css")) return "text/css; charset=utf-8";
        if (lower.endsWith(".js") || lower.endsWith(".mjs")) return "application/javascript; charset=utf-8";
        if (lower.endsWith(".json")) return "application/json; charset=utf-8";
        if (lower.endsWith(".svg")) return "image/svg+xml";
        if (lower.endsWith(".png")) return "image/png";
        if (lower.endsWith(".jpg") || lower.endsWith(".jpeg")) return "image/jpeg";
        if (lower.endsWith(".ico")) return "image/x-icon";
        if (lower.endsWith(".wasm")) return "application/wasm";
        return "text/plain; charset=utf-8";
    }

    function parseCookies(cookieHdr) {
        const m = rt.mkMap(false);
        if (!cookieHdr) return m;
        const parts = cookieHdr.split(";");
        for (let i = 0; i < parts.length; i++) {
            const p = parts[i].trim();
            const eqi = p.indexOf("=");
            if (eqi > 0) {
                rt.mapSetRaw(m, R.mkString(p.slice(0, eqi).trim()), R.mkString(p.slice(eqi + 1).trim()));
            }
        }
        return m;
    }

    function joinRoutePrefix(prefix, pat) {
        const p = prefix.replace(/\/+$/, "");
        const s = pat.startsWith("/") ? pat : "/" + pat;
        if (!p || p === "/") return s;
        if (s === "/") return p;
        return p + s;
    }

    function routerDispatch(router, req) {
        return R.blockOn(routerDispatchAsync(router, req));
    }
    async function routerDispatchAsync(router, req) {
        req.state = router.state;
        const routes = router.routes;
        let pathMatched = false;
        let matchedRoute = null;
        let matchedParams = null;
        for (let i = 0; i < routes.length; i++) {
            const route = routes[i];
            const m = matchRoute(route, req.path);
            if (m !== null) {
                pathMatched = true;
                if (route.method === "ANY" || route.method === req.method) {
                    matchedRoute = route;
                    matchedParams = m;
                    break;
                }
            }
        }
        if (matchedParams) {
            const ks = Object.keys(matchedParams.map);
            for (let i = 0; i < ks.length; i++) {
                rt.mapSetRaw(req.params, R.mkString(ks[i]), R.mkString(matchedParams.map[ks[i]]));
            }
            req.pathParamList = matchedParams.list;
        }
        let exec = async function (r) {
            if (matchedRoute) {
                const st = matchedRoute.hasOwnState ? matchedRoute.state : router.state;
                const useState = matchedRoute.hasOwnState || router.hasState;
                r.state = st;
                const handler = matchedRoute.handler;
                if (handler && handler.__rs_params && rt.invokeAxumHandler) {
                    const out = await rt.invokeAxumHandler(handler, r, st, null);
                    return coerceHandlerValue(out);
                }
                const out = await (useState ? handler(r, st) : handler(r));
                return coerceHandlerValue(out);
            }
            // Check static file mounts (`serve_dir`)
            if (router.staticDirs && (r.method === "GET" || r.method === "HEAD")) {
                for (let i = 0; i < router.staticDirs.length; i++) {
                    const sd = router.staticDirs[i];
                    if (r.path === sd.prefix || r.path.startsWith(sd.prefix === "/" ? "/" : sd.prefix + "/")) {
                        let rel = sd.prefix === "/" ? r.path.slice(1) : r.path.slice(sd.prefix.length).replace(/^\/+/, "");
                        if (rel === "" || rel.endsWith("/")) rel += "index.html";
                        if (rel.indexOf("..") >= 0 || rel.indexOf("\\") >= 0) {
                            return mkResponse(403, "Forbidden", "text/plain; charset=utf-8");
                        }
                        const full = pathJoin(sd.dir, rel);
                        const txt = fsCall("read", full);
                        if (typeof txt === "string") {
                            return mkResponse(200, txt, mimeForPath(full));
                        }
                    }
                }
            }
            if (router.fallback) {
                const out = await (router.hasState ? router.fallback(r, router.state) : router.fallback(r));
                return coerceHandlerValue(out);
            }
            if (pathMatched) return mkResponse(405, "Method Not Allowed", "text/plain; charset=utf-8");
            return mkResponse(404, "Not Found", "text/plain; charset=utf-8");
        };
        // Wrap route-specific layers (innermost first; `route.layers` stores outermost at index 0)
        const wrapLayer = function (layer, innerExec) {
            const mw = deref(layer);
            if (!mw) return innerExec;
            if (typeof mw === "function") {
                return async function (r) {
                    const out = await mw(r, function (r2) { return R.blockOn(innerExec(r2)); });
                    return coerceHandlerValue(out);
                };
            }
            if (mw.__rs === 15 && mw.kind === "axum_mw") {
                return async function (r) {
                    const nextObj = { __rs: 15, kind: "axum_next", runFn: innerExec };
                    const st = mw.hasState ? mw.state : router.state;
                    if (mw.fn && mw.fn.__rs_params && rt.invokeAxumHandler) {
                        const out = await rt.invokeAxumHandler(mw.fn, r, st, nextObj);
                        return coerceHandlerValue(out);
                    }
                    const out = await mw.fn(R.mkTupleStruct("State", [st]), r, nextObj);
                    return coerceHandlerValue(out);
                };
            }
            if (mw.__rs === 15 && mw.kind === "cors_layer") {
                return async function (r) {
                    const originHdr = rt.mapGetRaw(r.headers, R.mkString("origin"));
                    const originStr = originHdr ? strVal(originHdr) : null;
                    let allowed = false;
                    if (originStr) {
                        if (mw.allowAny) allowed = true;
                        else if (mw.predicate) {
                            allowed = !!deref(mw.predicate({ __rs: 15, kind: "header_value", v: originStr }, r));
                        } else if (mw.origins && mw.origins.indexOf(originStr) >= 0) {
                            allowed = true;
                        }
                    }
                    if (r.method === "OPTIONS" && originStr) {
                        const pre = mkResponse(204, "", null);
                        if (allowed) {
                            rt.mapSetRaw(pre.headers, R.mkString("access-control-allow-origin"), R.mkString(originStr));
                            if (mw.methods && mw.methods.length) {
                                rt.mapSetRaw(pre.headers, R.mkString("access-control-allow-methods"), R.mkString(mw.methods.join(", ")));
                            }
                            if (mw.headers && mw.headers.length) {
                                rt.mapSetRaw(pre.headers, R.mkString("access-control-allow-headers"), R.mkString(mw.headers.join(", ")));
                            }
                            rt.mapSetRaw(pre.headers, R.mkString("vary"), R.mkString("origin"));
                        }
                        return pre;
                    }
                    const resp = await innerExec(r);
                    if (allowed && originStr) {
                        rt.mapSetRaw(resp.headers, R.mkString("access-control-allow-origin"), R.mkString(originStr));
                        rt.mapSetRaw(resp.headers, R.mkString("vary"), R.mkString("origin"));
                    }
                    return resp;
                };
            }
            // TraceLayer, CompressionLayer, or unknown pass-through layer
            return innerExec;
        };
        if (matchedRoute && matchedRoute.layers && matchedRoute.layers.length > 0) {
            for (let i = matchedRoute.layers.length - 1; i >= 0; i--) {
                exec = wrapLayer(matchedRoute.layers[i], exec);
            }
        }
        // Wrap router-wide layers/middlewares (outermost first)
        for (let i = router.middlewares.length - 1; i >= 0; i--) {
            exec = wrapLayer(router.middlewares[i], exec);
        }
        return await exec(req);
    }
    rt.routerDispatch = routerDispatch;
    rt.routerDispatchAsync = routerDispatchAsync;

    function responseToJson(resp) {
        const hdrs = [];
        const entries = rt.mapEntries(resp.headers);
        for (let i = 0; i < entries.length; i++) {
            hdrs.push([strVal(entries[i][0]), strVal(entries[i][1])]);
        }
        return JSON.stringify({
            status: resp.status,
            headers: hdrs,
            body: resp.body
        });
    }
    rt.responseToJson = responseToJson;

    function serveRouterOverTcp(router, addrStr) {
        const lid = netCall("bind", addrStr);
        if (typeof lid !== "number" || lid < 0) {
            return Err(mkIoErr("AddrInUse", "failed to bind " + addrStr));
        }
        for (;;) {
            const acc = netCall("accept", String(lid));
            if (!acc || typeof acc !== "string") continue;
            const sep = acc.indexOf("|");
            const sid = Number(sep >= 0 ? acc.slice(0, sep) : acc);
            if (!(sid >= 0)) continue;
            try {
                const rawReq = netCall("read_http", String(sid));
                if (typeof rawReq === "string" && rawReq.length > 0) {
                    const parsed = JSON.parse(rawReq);
                    const req = mkRequest(parsed.method, parsed.uri, parsed.headers, parsed.body, null);
                    const resp = routerDispatch(router, req);
                    const bodyBytes = rt.strToUtf8Bytes(resp.body);
                    let wire = "HTTP/1.1 " + resp.status + " " + statusReason(resp.status) + "\r\n";
                    let hasCt = false, hasCl = false;
                    const entries = rt.mapEntries(resp.headers);
                    for (let i = 0; i < entries.length; i++) {
                        const hk = strVal(entries[i][0]);
                        const hv = strVal(entries[i][1]);
                        if (hk.toLowerCase() === "content-type") hasCt = true;
                        if (hk.toLowerCase() === "content-length") hasCl = true;
                        wire += hk + ": " + hv + "\r\n";
                    }
                    if (!hasCt) wire += "Content-Type: text/plain; charset=utf-8\r\n";
                    if (!hasCl) wire += "Content-Length: " + bodyBytes.length + "\r\n";
                    wire += "Connection: close\r\n\r\n" + resp.body;
                    netCall("write_str", String(sid), wire);
                }
            } catch (e) {
                const msg = e && e.isRsPanic ? e.message : String(e);
                const errWire = "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: " +
                    rt.utf8ByteLen(msg) + "\r\nConnection: close\r\n\r\n" + msg;
                netCall("write_str", String(sid), errWire);
            } finally {
                netCall("close_stream", String(sid));
            }
        }
    }

    function httpClientRequest(method, urlStr, headersMap, bodyStr) {
        const m = /^http:\/\/([^\/:]+)(?::(\d+))?(\/.*)?$/i.exec(urlStr);
        if (!m) return Err(mkIoErr("InvalidInput", "http::Client supports http://host[:port][/path] URLs: " + urlStr));
        const host = m[1];
        const port = m[2] ? Number(m[2]) : 80;
        const path = m[3] || "/";
        const sid = netCall("connect", host + ":" + port);
        if (typeof sid !== "number" || sid < 0) {
            return Err(mkIoErr("ConnectionRefused", "failed to connect to " + host + ":" + port));
        }
        try {
            const body = bodyStr || "";
            let wire = method.toUpperCase() + " " + path + " HTTP/1.1\r\nHost: " + host + (port === 80 ? "" : ":" + port) + "\r\nConnection: close\r\n";
            if (headersMap) {
                const entries = rt.mapEntries(headersMap);
                for (let i = 0; i < entries.length; i++) {
                    wire += strVal(entries[i][0]) + ": " + strVal(entries[i][1]) + "\r\n";
                }
            }
            if (body.length > 0) {
                wire += "Content-Length: " + rt.utf8ByteLen(body) + "\r\n";
            }
            wire += "\r\n" + body;
            netCall("write_str", String(sid), wire);
            const raw = netCall("read_all_str", String(sid));
            if (typeof raw !== "string") return Err(mkIoErr("UnexpectedEof", "empty HTTP response"));
            const split = raw.indexOf("\r\n\r\n");
            const head = split >= 0 ? raw.slice(0, split) : raw;
            const respBody = split >= 0 ? raw.slice(split + 4) : "";
            const lines = head.split("\r\n");
            const statusLine = lines[0] || "HTTP/1.1 200 OK";
            const status = parseInt((statusLine.split(" ")[1]) || "200", 10) || 200;
            const resp = mkResponse(status, respBody, null);
            for (let i = 1; i < lines.length; i++) {
                const c = lines[i].indexOf(":");
                if (c > 0) {
                    rt.mapSetRaw(resp.headers, R.mkString(lines[i].slice(0, c).trim().toLowerCase()), R.mkString(lines[i].slice(c + 1).trim()));
                }
            }
            return Ok(resp);
        } finally {
            netCall("close_stream", String(sid));
        }
    }

    // ---- Stdlib object method dispatch (`__rs === 15`) ----
    function callStdMethod(obj, name, args, typeArgs) {
        switch (obj.kind) {
            case "entry": {
                const m = obj.map, k = obj.key, mk = obj.mk;
                const refOf = function () {
                    return {
                        __rs: 6,
                        get: function () { return m.map.get(mk)[1]; },
                        set: function (val) { m.map.get(mk)[1] = val; }
                    };
                };
                if (name === "or_insert") {
                    if (!m.map.has(mk)) m.map.set(mk, [k, args[0]]);
                    return refOf();
                }
                if (name === "or_insert_with") {
                    if (!m.map.has(mk)) m.map.set(mk, [k, args[0]()]);
                    return refOf();
                }
                if (name === "or_default") {
                    if (!m.map.has(mk)) {
                        const defVal = m.valType ? rt.defaultOf(m.valType) : 0;
                        m.map.set(mk, [k, defVal === undefined ? 0 : defVal]);
                    }
                    return refOf();
                }
                if (name === "and_modify") {
                    if (m.map.has(mk)) args[0](refOf());
                    return obj;
                }
                if (name === "key") return k;
                break;
            }
            case "path": {
                const p = obj.p;
                switch (name) {
                    case "join": return mkPath(pathJoin(p, args[0]));
                    case "push": obj.p = pathJoin(p, args[0]); return undefined;
                    case "parent": {
                        const clean = p.replace(/[\\/]+$/, "");
                        const idx = Math.max(clean.lastIndexOf("/"), clean.lastIndexOf("\\"));
                        return idx < 0 ? None : Some(mkPath(idx === 0 ? "/" : clean.slice(0, idx)));
                    }
                    case "file_name": {
                        const b = pathBase(p);
                        return b === "" ? None : Some(R.mkString(b));
                    }
                    case "file_stem": {
                        const b = pathBase(p);
                        if (!b) return None;
                        const d = b.lastIndexOf(".");
                        return Some(R.mkString(d > 0 ? b.slice(0, d) : b));
                    }
                    case "extension": {
                        const b = pathBase(p);
                        const d = b.lastIndexOf(".");
                        return (d > 0 && d + 1 < b.length) ? Some(R.mkString(b.slice(d + 1))) : None;
                    }
                    case "with_extension": {
                        const ext = strVal(args[0]);
                        const b = pathBase(p);
                        const d = b.lastIndexOf(".");
                        const basePrefix = p.slice(0, p.length - b.length);
                        const stem = d > 0 ? b.slice(0, d) : b;
                        return mkPath(basePrefix + stem + (ext ? "." + ext : ""));
                    }
                    case "set_file_name": {
                        const fnm = strVal(args[0]);
                        const b = pathBase(p);
                        const basePrefix = p.slice(0, p.length - b.length);
                        obj.p = basePrefix + fnm;
                        return undefined;
                    }
                    case "as_os_str": return R.mkString(p);
                    case "as_path": return obj;
                    case "exists": return fsCall("exists", p) === true;
                    case "is_file": return fsCall("is_file", p) === true;
                    case "is_dir": return fsCall("is_dir", p) === true;
                    case "to_str": return Some(p);
                    case "to_string_lossy": case "to_string": return R.mkString(p);
                    case "display": return R.mkString(p);
                    case "to_path_buf": case "clone": return mkPath(p);
                    case "starts_with": return p.startsWith(strVal(args[0]));
                    case "ends_with": return p.endsWith(strVal(args[0]));
                    case "is_absolute": return p.startsWith("/") || /^[A-Za-z]:[\\/]/.test(p);
                }
                break;
            }
            case "metadata": {
                if (name === "len") return obj.len;
                if (name === "is_file") return obj.is_file;
                if (name === "is_dir") return obj.is_dir;
                break;
            }
            case "dir_entry": {
                if (name === "path") return mkPath(obj.path);
                if (name === "file_name") return R.mkString(obj.name);
                break;
            }
            case "file": {
                if (name === "read_to_string") {
                    const txt = fsCall("read", obj.path);
                    if (typeof txt !== "string") return Err(mkIoErr("NotFound", "cannot read " + obj.path));
                    const dst = deref(args[0]);
                    if (dst && dst.__rs === 4) dst.s += txt;
                    return Ok(rt.utf8ByteLen(txt));
                }
                if (name === "write_all" || name === "write") {
                    const data = deref(args[0]);
                    if (data && data.__rs === 7) {
                        let hx = "";
                        for (let i = 0; i < data.items.length; i++) hx += (Number(data.items[i]) & 0xFF).toString(16).padStart(2, "0");
                        const opHex = obj.append ? "append_bytes_hex" : "write_bytes_hex";
                        const okHex = fsCall(opHex, obj.path, hx);
                        if (okHex === true) {
                            obj.append = true;
                            return name === "write" ? Ok(data.items.length) : Ok(undefined);
                        }
                        if (okHex === false) return Err(mkIoErr("PermissionDenied", "failed to write " + obj.path));
                    }
                    const s = (data && data.__rs === 7) ? rt.utf8BytesToStr(data.items) : strVal(data);
                    const op = obj.append ? "append" : "write";
                    const ok = fsCall(op, obj.path, s);
                    if (!ok) return Err(mkIoErr("PermissionDenied", "failed to write " + obj.path));
                    obj.append = true; // subsequent writes append
                    return name === "write" ? Ok(rt.utf8ByteLen(s)) : Ok(undefined);
                }
                if (name === "flush") return Ok(undefined);
                break;
            }
            case "open_options": {
                if (name === "read" || name === "write" || name === "append" || name === "create" || name === "create_new" || name === "truncate") {
                    obj[name] = !!deref(args[0]);
                    return obj;
                }
                if (name === "mode" || name === "custom_flags") {
                    obj[name] = deref(args[0]);
                    return obj;
                }
                if (name === "open") {
                    const p = strVal(args[0]);
                    if (obj.create && !fsCall("exists", p)) fsCall("write", p, "");
                    if (obj.truncate) fsCall("write", p, "");
                    return Ok({ __rs: 15, kind: "file", path: p, append: !!obj.append });
                }
                break;
            }
            case "stdout":
                if (name === "write_all" || name === "write") {
                    const d = deref(args[0]);
                    const s = (d && d.__rs === 7) ? rt.utf8BytesToStr(d.items) : strVal(d);
                    rt.writeStdout(s);
                    return name === "write" ? Ok(rt.utf8ByteLen(s)) : Ok(undefined);
                }
                if (name === "flush") return Ok(undefined);
                break;
            case "stderr":
                if (name === "write_all" || name === "write") {
                    const d = deref(args[0]);
                    const s = (d && d.__rs === 7) ? rt.utf8BytesToStr(d.items) : strVal(d);
                    rt.writeStderr(s);
                    return name === "write" ? Ok(rt.utf8ByteLen(s)) : Ok(undefined);
                }
                if (name === "flush") return Ok(undefined);
                break;
            case "stdin":
                if (name === "read_line") {
                    const line = typeof __zipp_rs_io === "function" ? __zipp_rs_io("stdin_line", "") : "";
                    const dst = deref(args[0]);
                    if (dst && dst.__rs === 4 && typeof line === "string") dst.s += line;
                    return Ok(typeof line === "string" ? rt.utf8ByteLen(line) : 0);
                }
                break;
            case "instant": {
                if (name === "elapsed") {
                    const now = timeCall("mono");
                    return { __rs: 15, kind: "duration", ns: now - obj.ns };
                }
                if (name === "duration_since") {
                    const earlier = deref(args[0]);
                    return { __rs: 15, kind: "duration", ns: obj.ns - earlier.ns };
                }
                break;
            }
            case "system_time": {
                if (name === "duration_since") {
                    const earlier = deref(args[0]);
                    const diff = obj.ns - earlier.ns;
                    if (diff < 0n) return Err(mkIoErr("Other", "second time provided was later than self"));
                    return Ok({ __rs: 15, kind: "duration", ns: diff });
                }
                break;
            }
            case "duration": {
                const ns = obj.ns;
                if (name === "as_secs") return Number(ns / 1000000000n);
                if (name === "as_millis") return Number(ns / 1000000n);
                if (name === "as_micros") return Number(ns / 1000n);
                if (name === "as_nanos") return ns > 9007199254740991n ? ns : Number(ns);
                if (name === "as_secs_f64" || name === "as_secs_f32") return Number(ns) / 1e9;
                if (name === "subsec_nanos") return Number(ns % 1000000000n);
                if (name === "subsec_millis") return Number((ns % 1000000000n) / 1000000n);
                if (name === "is_zero") return ns === 0n;
                if (name === "clone") return { __rs: 15, kind: "duration", ns: ns };
                break;
            }
            case "tcp_listener": {
                if (name === "local_addr") {
                    if (obj.virtual) {
                        const s = obj.addr || "127.0.0.1:0";
                        return typeof rt.mkSocketAddr === "function"
                            ? Ok(rt.mkSocketAddr(s.split(":")[0] || "127.0.0.1", Number(s.split(":")[1] || 0)))
                            : Ok(R.mkString(s));
                    }
                    const addr = netCall("local_addr", String(obj.id));
                    return typeof addr === "string" ? Ok(R.mkString(addr)) : Err(mkIoErr("Other", "closed listener"));
                }
                if (name === "accept") {
                    const acc = netCall("accept", String(obj.id));
                    if (typeof acc !== "string") return Err(mkIoErr("Other", "accept failed"));
                    const sep = acc.indexOf("|");
                    const sid = Number(sep >= 0 ? acc.slice(0, sep) : acc);
                    const peer = sep >= 0 ? acc.slice(sep + 1) : "";
                    return Ok(R.mkTuple([{ __rs: 15, kind: "tcp_stream", id: sid }, R.mkString(peer)]));
                }
                if (name === "incoming") return tcpIncomingIter(obj);
                if (name === "close") { netCall("close_listener", String(obj.id)); return undefined; }
                break;
            }
            case "tcp_stream": {
                if (name === "peer_addr") {
                    const a = netCall("peer_addr", String(obj.id));
                    return typeof a === "string" ? Ok(R.mkString(a)) : Err(mkIoErr("Other", "peer_addr failed"));
                }
                if (name === "read") {
                    const buf = deref(args[0]);
                    const maxLen = buf && buf.__rs === 7 ? buf.items.length : 4096;
                    const s = netCall("read_str", String(obj.id), String(maxLen));
                    if (typeof s !== "string") return Err(mkIoErr("Other", "read failed"));
                    const bytes = rt.strToUtf8Bytes(s);
                    if (buf && buf.__rs === 7) {
                        const n = Math.min(bytes.length, buf.items.length);
                        for (let i = 0; i < n; i++) buf.items[i] = bytes[i];
                        return Ok(n);
                    }
                    return Ok(bytes.length);
                }
                if (name === "read_to_string") {
                    const s = netCall("read_all_str", String(obj.id));
                    if (typeof s !== "string") return Err(mkIoErr("Other", "read_to_string failed"));
                    const dst = deref(args[0]);
                    if (dst && dst.__rs === 4) dst.s += s;
                    return Ok(rt.utf8ByteLen(s));
                }
                if (name === "write" || name === "write_all") {
                    const d = deref(args[0]);
                    const s = (d && d.__rs === 7) ? rt.utf8BytesToStr(d.items) : strVal(d);
                    const n = netCall("write_str", String(obj.id), s);
                    if (typeof n !== "number" || n < 0) return Err(mkIoErr("BrokenPipe", "write failed"));
                    return name === "write" ? Ok(n) : Ok(undefined);
                }
                if (name === "flush") return Ok(undefined);
                if (name === "shutdown" || name === "close") {
                    netCall("close_stream", String(obj.id));
                    return Ok(undefined);
                }
                break;
            }
            case "regex": {
                const re = obj.re;
                const s = args.length > 0 ? strVal(args[0]) : "";
                if (name === "is_match") return new RegExp(re.source, re.flags.replace("g", "")).test(s);
                if (name === "find") {
                    const m = new RegExp(re.source, re.flags.replace("g", "")).exec(s);
                    if (!m) return None;
                    return Some({ __rs: 15, kind: "regex_match", text: m[0], start: m.index, end: m.index + m[0].length });
                }
                if (name === "captures") {
                    const m = new RegExp(re.source, re.flags.replace("g", "")).exec(s);
                    if (!m) return None;
                    return Some({ __rs: 15, kind: "regex_caps", groups: Array.from(m) });
                }
                if (name === "replace") {
                    const rep = strVal(args[1]);
                    return R.mkString(s.replace(new RegExp(re.source, re.flags.replace("g", "")), rep));
                }
                if (name === "replace_all") {
                    const rep = strVal(args[1]);
                    const flags = re.flags.indexOf("g") >= 0 ? re.flags : re.flags + "g";
                    return R.mkString(s.replace(new RegExp(re.source, flags), rep));
                }
                break;
            }
            case "regex_match":
                if (name === "as_str") return obj.text;
                if (name === "start") return obj.start;
                if (name === "end") return obj.end;
                break;
            case "regex_caps":
                if (name === "get") {
                    const i = Number(deref(args[0]));
                    const g = obj.groups[i];
                    return g === undefined ? None : Some({ __rs: 15, kind: "regex_match", text: g, start: 0, end: g.length });
                }
                break;
            case "json": {
                const raw = obj.v;
                if (name === "is_null") return raw === null;
                if (name === "is_boolean") return typeof raw === "boolean";
                if (name === "is_number") return typeof raw === "number";
                if (name === "is_string") return typeof raw === "string";
                if (name === "is_array") return Array.isArray(raw);
                if (name === "is_object") return raw !== null && typeof raw === "object" && !Array.isArray(raw);
                if (name === "as_bool") return typeof raw === "boolean" ? Some(raw) : None;
                if (name === "as_i64" || name === "as_u64") return typeof raw === "number" ? Some(Math.trunc(raw)) : None;
                if (name === "as_f64") return typeof raw === "number" ? Some(raw) : None;
                if (name === "as_str") return typeof raw === "string" ? Some(raw) : None;
                if (name === "as_array") return Array.isArray(raw) ? Some({ __rs: 7, items: raw.map(mkJson) }) : None;
                if (name === "as_object") {
                    if (!raw || typeof raw !== "object" || Array.isArray(raw)) return None;
                    const m = rt.mkMap(false);
                    const ks = Object.keys(raw);
                    for (let i = 0; i < ks.length; i++) rt.mapSetRaw(m, R.mkString(ks[i]), mkJson(raw[ks[i]]));
                    return Some(m);
                }
                if (name === "get") {
                    const k = deref(args[0]);
                    if (Array.isArray(raw)) {
                        const i = Number(k);
                        return (i >= 0 && i < raw.length) ? Some(mkJson(raw[i])) : None;
                    }
                    if (raw && typeof raw === "object") {
                        const sk = strVal(k);
                        return Object.prototype.hasOwnProperty.call(raw, sk) ? Some(mkJson(raw[sk])) : None;
                    }
                    return None;
                }
                if (name === "to_string") return R.mkString(JSON.stringify(raw));
                if (name === "clone") return mkJson(JSON.parse(JSON.stringify(raw)));
                break;
            }
            case "router": {
                const addRoute = function (method, pat, handler, ownState, hasOwnState, layers) {
                    obj.routes.push({
                        method: method,
                        pat: compileRoutePattern(strVal(pat)),
                        handler: handler,
                        state: ownState,
                        hasOwnState: !!hasOwnState,
                        layers: layers ? layers.slice() : []
                    });
                    rt.setRouter(obj);
                    return obj;
                };
                if (name === "get") return addRoute("GET", args[0], args[1]);
                if (name === "post") return addRoute("POST", args[0], args[1]);
                if (name === "put") return addRoute("PUT", args[0], args[1]);
                if (name === "delete") return addRoute("DELETE", args[0], args[1]);
                if (name === "patch") return addRoute("PATCH", args[0], args[1]);
                if (name === "any") return addRoute("ANY", args[0], args[1]);
                if (name === "route") {
                    const pat = strVal(args[0]);
                    const spec = deref(args[1]);
                    if (spec && spec.__rs === 15 && spec.kind === "method_router") {
                        for (let i = 0; i < spec.entries.length; i++) {
                            addRoute(spec.entries[i][0], pat, spec.entries[i][1]);
                        }
                    } else if (typeof spec === "function") {
                        addRoute("ANY", pat, spec);
                    }
                    return obj;
                }
                if (name === "nest") {
                    const prefix = strVal(args[0]);
                    const sub = deref(args[1]);
                    if (sub && sub.__rs === 15 && sub.kind === "router") {
                        for (let i = 0; i < sub.routes.length; i++) {
                            const r = sub.routes[i];
                            const fullPat = joinRoutePrefix(prefix, r.pat.raw);
                            const hasSt = r.hasOwnState || sub.hasState;
                            const st = r.hasOwnState ? r.state : sub.state;
                            const layers = sub.middlewares.concat(r.layers || []);
                            addRoute(r.method, fullPat, r.handler, st, hasSt, layers);
                        }
                    }
                    return obj;
                }
                if (name === "merge") {
                    const sub = deref(args[0]);
                    if (sub && sub.__rs === 15 && sub.kind === "router") {
                        for (let i = 0; i < sub.routes.length; i++) {
                            const r = sub.routes[i];
                            const hasSt = r.hasOwnState || sub.hasState;
                            const st = r.hasOwnState ? r.state : sub.state;
                            const layers = sub.middlewares.concat(r.layers || []);
                            addRoute(r.method, r.pat.raw, r.handler, st, hasSt, layers);
                        }
                    }
                    return obj;
                }
                if (name === "route_layer") {
                    // Axum `.route_layer(L)`: wraps only routes currently in this router, outermost first!
                    const layer = args[0];
                    for (let i = 0; i < obj.routes.length; i++) {
                        if (!obj.routes[i].layers) obj.routes[i].layers = [];
                        obj.routes[i].layers.unshift(layer);
                    }
                    return obj;
                }
                if (name === "into_make_service" || name === "into_make_service_with_connect_info") {
                    return obj;
                }
                if (name === "serve_dir") {
                    if (!obj.staticDirs) obj.staticDirs = [];
                    obj.staticDirs.push({
                        prefix: strVal(args[0]).replace(/\/+$/, "") || "/",
                        dir: strVal(args[1])
                    });
                    rt.setRouter(obj);
                    return obj;
                }
                if (name === "serve_file") {
                    const routePat = strVal(args[0]);
                    const filePath = strVal(args[1]);
                    return addRoute("GET", routePat, function () {
                        const txt = fsCall("read", filePath);
                        if (typeof txt !== "string") return mkResponse(404, "Not Found", "text/plain; charset=utf-8");
                        return mkResponse(200, txt, mimeForPath(filePath));
                    });
                }
                if (name === "with_state") {
                    obj.state = args[0];
                    obj.hasState = true;
                    rt.setRouter(obj);
                    return obj;
                }
                if (name === "layer") {
                    obj.middlewares.unshift(args[0]);
                    return obj;
                }
                if (name === "middleware") {
                    obj.middlewares.push(args[0]);
                    return obj;
                }
                if (name === "fallback") {
                    obj.fallback = args[0];
                    return obj;
                }
                if (name === "handle") {
                    return routerDispatch(obj, deref(args[0]));
                }
                if (name === "register") {
                    rt.setRouter(obj);
                    return obj;
                }
                if (name === "serve") {
                    rt.setRouter(obj);
                    return serveRouterOverTcp(obj, strVal(args[0]));
                }
                break;
            }
            case "method_router": {
                if (name === "get" || name === "post" || name === "put" || name === "delete" || name === "patch") {
                    obj.entries.push([name.toUpperCase(), args[0]]);
                    return obj;
                }
                break;
            }
            case "request": {
                if (name === "method") return R.mkEnumUnit("Method", obj.method, null);
                if (name === "uri") return { __rs: 15, kind: "uri", raw: obj.uri, path: obj.path, query: obj.queryStr };
                if (name === "path") return obj.path;
                if (name === "body") return obj.body;
                if (name === "bytes") return { __rs: 7, items: rt.strToUtf8Bytes(obj.body) };
                if (name === "query") return rt.mapGetOpt(obj.queryMap, R.mkString(strVal(args[0])));
                if (name === "query_params") return obj.queryMap;
                if (name === "param") return rt.mapGetOpt(obj.params, R.mkString(strVal(args[0])));
                if (name === "params") return obj.params;
                if (name === "header") return rt.mapGetOpt(obj.headers, R.mkString(strVal(args[0]).toLowerCase()));
                if (name === "headers") return { __rs: 15, kind: "header_map", map: obj.headers };
                if (name === "extensions" || name === "extensions_mut") {
                    if (!obj.extensions) obj.extensions = Object.create(null);
                    return { __rs: 15, kind: "extensions", store: obj.extensions };
                }
                if (name === "into_parts") {
                    const parts = { __rs: 15, kind: "req_parts", req: obj };
                    const body = { __rs: 15, kind: "body", data: obj.body };
                    return R.mkTuple([parts, body]);
                }
                if (name === "bearer_token") {
                    const auth = rt.mapGetRaw(obj.headers, R.mkString("authorization"));
                    if (!auth) return None;
                    const s = strVal(auth).trim();
                    if (s.toLowerCase().startsWith("bearer ")) return Some(R.mkString(s.slice(7).trim()));
                    return None;
                }
                if (name === "cookies") {
                    const ch = rt.mapGetRaw(obj.headers, R.mkString("cookie"));
                    return parseCookies(ch ? strVal(ch) : "");
                }
                if (name === "cookie") {
                    const ch = rt.mapGetRaw(obj.headers, R.mkString("cookie"));
                    return rt.mapGetOpt(parseCookies(ch ? strVal(ch) : ""), R.mkString(strVal(args[0])));
                }
                if (name === "form") return parseQueryString(obj.body);
                if (name === "state") return obj.state;
                if (name === "json") {
                    try {
                        const raw = JSON.parse(obj.body || "null");
                        return Ok(fromJsonRaw(raw, typeArgs && typeArgs[0] ? typeArgs[0] : ""));
                    } catch (e) {
                        return Err(R.mkString("invalid JSON body: " + String(e)));
                    }
                }
                break;
            }
            case "response": {
                if (name === "status") return obj.status;
                if (name === "body") return obj.body;
                if (name === "header") return rt.mapGetOpt(obj.headers, R.mkString(strVal(args[0])));
                if (name === "headers" || name === "headers_mut") return { __rs: 15, kind: "header_map", map: obj.headers };
                if (name === "into_parts") {
                    const parts = { __rs: 15, kind: "resp_parts", resp: obj };
                    const body = { __rs: 15, kind: "body", data: obj.body };
                    return R.mkTuple([parts, body]);
                }
                if (name === "into_response") return obj;
                if (name === "with_status") { obj.status = Number(deref(args[0])) | 0; return obj; }
                if (name === "with_header") {
                    rt.mapSetRaw(obj.headers, R.mkString(strVal(args[0])), R.mkString(strVal(args[1])));
                    return obj;
                }
                if (name === "with_cookie") {
                    const c = strVal(args[0]) + "=" + strVal(args[1]) + (args[2] ? "; " + strVal(args[2]) : "; Path=/; HttpOnly");
                    rt.mapSetRaw(obj.headers, R.mkString("Set-Cookie"), R.mkString(c));
                    return obj;
                }
                if (name === "json") {
                    try {
                        const raw = JSON.parse(obj.body || "null");
                        return Ok(fromJsonRaw(raw, typeArgs && typeArgs[0] ? typeArgs[0] : ""));
                    } catch (e) {
                        return Err(R.mkString("invalid JSON: " + String(e)));
                    }
                }
                break;
            }
            case "datetime": {
                if (name === "timestamp") return Number(obj.ns / 1000000000n);
                if (name === "timestamp_millis") return Number(obj.ns / 1000000n);
                if (name === "to_rfc3339" || name === "to_string") {
                    const secs = Number(obj.ns / 1000000000n);
                    const s = timeCall("rfc3339", secs);
                    return R.mkString(typeof s === "string" ? s : new Date(secs * 1000).toISOString());
                }
                break;
            }
            case "buf_reader":
            case "cursor": {
                if (name === "read_line") {
                    if (obj.pos >= obj.text.length) return Ok(0);
                    const nl = obj.text.indexOf("\n", obj.pos);
                    const end = nl < 0 ? obj.text.length : nl + 1;
                    const line = obj.text.slice(obj.pos, end);
                    obj.pos = end;
                    const dst = deref(args[0]);
                    if (dst && dst.__rs === 4) dst.s += line;
                    return Ok(rt.utf8ByteLen(line));
                }
                if (name === "lines") {
                    const rest = obj.text.slice(obj.pos);
                    obj.pos = obj.text.length;
                    const arr = rest.length === 0 ? [] : rest.replace(/\r?\n$/, "").split(/\r?\n/);
                    let i = 0;
                    return rt.mkIter(function () {
                        return i >= arr.length ? ITER_DONE : Ok(R.mkString(arr[i++]));
                    });
                }
                if (name === "read_to_string") {
                    const rest = obj.text.slice(obj.pos);
                    obj.pos = obj.text.length;
                    const dst = deref(args[0]);
                    if (dst && dst.__rs === 4) dst.s += rest;
                    return Ok(rt.utf8ByteLen(rest));
                }
                break;
            }
            case "runtime": {
                if (name === "block_on") return R.blockOn(args[0]);
                break;
            }
            case "http_client": {
                if (name === "get" || name === "post" || name === "put" || name === "delete" || name === "patch") {
                    return {
                        __rs: 15,
                        kind: "http_req_builder",
                        method: name.toUpperCase(),
                        url: strVal(args[0]),
                        headers: rt.mkMap(false),
                        body: ""
                    };
                }
                break;
            }
            case "http_req_builder": {
                if (name === "header") {
                    rt.mapSetRaw(obj.headers, R.mkString(strVal(args[0])), R.mkString(strVal(args[1])));
                    return obj;
                }
                if (name === "body") { obj.body = strVal(args[0]); return obj; }
                if (name === "json") {
                    rt.mapSetRaw(obj.headers, R.mkString("Content-Type"), R.mkString("application/json"));
                    obj.body = JSON.stringify(toJsonRaw(args[0]));
                    return obj;
                }
                if (name === "send") return httpClientRequest(obj.method, obj.url, obj.headers, obj.body);
                break;
            }
            case "command": {
                if (name === "arg") { obj.args.push(strVal(args[0])); return obj; }
                if (name === "args") {
                    const it = R.intoIter(args[0]);
                    for (;;) { const x = it.next(); if (x === ITER_DONE) break; obj.args.push(strVal(x)); }
                    return obj;
                }
                if (name === "current_dir") { obj.cwd = strVal(args[0]); return obj; }
                if (name === "env") { obj.env[strVal(args[0])] = strVal(args[1]); return obj; }
                if (name === "output" || name === "status") {
                    const raw = procCall("run", JSON.stringify({ program: obj.program, args: obj.args, cwd: obj.cwd, env: obj.env }));
                    if (typeof raw !== "string") return Err(mkIoErr("PermissionDenied", "Command execution not permitted"));
                    const parsed = JSON.parse(raw);
                    if (parsed.error) return Err(mkIoErr("NotFound", parsed.error));
                    const statusObj = { __rs: 15, kind: "exit_status", code: parsed.code };
                    if (name === "status") return Ok(statusObj);
                    return Ok({
                        __rs: 1,
                        __type: "Output",
                        __tuple: false,
                        status: statusObj,
                        stdout: { __rs: 7, items: rt.strToUtf8Bytes(parsed.stdout || "") },
                        stderr: { __rs: 7, items: rt.strToUtf8Bytes(parsed.stderr || "") }
                    });
                }
                break;
            }
            case "exit_status":
                if (name === "success") return obj.code === 0;
                if (name === "code") return Some(obj.code);
                break;
            case "hasher":
                if (name === "write") {
                    const d = deref(args[0]);
                    const bytes = (d && d.__rs === 7) ? d.items : rt.strToUtf8Bytes(strVal(d));
                    let h = obj.h;
                    for (let i = 0; i < bytes.length; i++) {
                        h = (h ^ BigInt(bytes[i] & 0xFF)) * 0x100000001b3n;
                        h = BigInt.asUintN(64, h);
                    }
                    obj.h = h;
                    return undefined;
                }
                if (name === "finish") return obj.h;
                break;
            case "kv": {
                if (name === "get") {
                    const k = strVal(args[0]);
                    return Object.prototype.hasOwnProperty.call(obj.data, k) ? Some(R.mkString(obj.data[k])) : None;
                }
                if (name === "set") {
                    obj.data[strVal(args[0])] = strVal(args[1]);
                    if (obj.path) fsCall("write", obj.path, JSON.stringify(obj.data));
                    return Ok(undefined);
                }
                if (name === "delete") {
                    const k = strVal(args[0]);
                    const had = Object.prototype.hasOwnProperty.call(obj.data, k);
                    delete obj.data[k];
                    if (obj.path) fsCall("write", obj.path, JSON.stringify(obj.data));
                    return had;
                }
                if (name === "keys") {
                    return { __rs: 7, items: Object.keys(obj.data).map(R.mkString) };
                }
                if (name === "len") return Object.keys(obj.data).length;
                break;
            }
            case "io_error":
                if (name === "to_string") return R.mkString(obj.msg);
                if (name === "kind") return R.mkEnumUnit("ErrorKind", obj.errKind);
                break;
        }
        if (rt.callEcoMethod) {
            const ecoRes = rt.callEcoMethod(obj, name, args, typeArgs);
            if (ecoRes !== rt.ECO_MISS) return ecoRes;
        }
        R.panic("unknown stdlib method `" + name + "` on `" + obj.kind + "`");
    }
    rt.callStdMethod = callStdMethod;

    // ---- Associated functions & stdlib module path calls ----
    R.callAssoc = function (pathParts, args, typeArgs) {
        args = args || [];
        typeArgs = typeArgs || [];
        const full = pathParts.join("::");
        const last = pathParts[pathParts.length - 1];
        const parent = pathParts.length >= 2 ? pathParts[pathParts.length - 2] : "";
        const prefix = pathParts.slice(0, -1).join("::");

        // 1. User-defined `From::from` / `TryFrom::try_from` multi-impl dispatch
        if (last === "from" && parent && args.length === 1) {
            const c = rt.tryConvertFrom(parent, args[0]);
            if (c !== rt.CONVERT_MISS) return c;
        }
        if (last === "try_from" && parent && args.length === 1) {
            const c = rt.tryConvertTryFrom(parent, args[0]);
            if (c !== rt.CONVERT_MISS) return c;
        }
        // 2. User-defined struct/enum inherent or trait associated functions
        const userFn = rt.lookupUserMethod(parent, last) || rt.lookupUserMethod(prefix, last);
        if (userFn) return userFn.apply(null, args);

        // 2. Ecosystem crates (sqlx, axum, tower_http, tokio, tracing, argon2, jsonwebtoken, time, uuid, sha2, hmac, base64, hex, rand, reqwest, fs2, std::net, windows_sys)
        if (rt.callEcoAssoc) {
            const ecoRes = rt.callEcoAssoc(pathParts, args, typeArgs);
            if (ecoRes !== rt.ECO_MISS) return ecoRes;
        }

        // 3. Built-in prelude / std / ecosystem types & functions
        switch (parent) {
            case "String":
                if (last === "new" || last === "with_capacity" || last === "default") return R.mkString("");
                if (last === "from") return R.mkString(args.length > 0 ? strVal(args[0]) : "");
                if (last === "from_utf8") {
                    const d = deref(args[0]);
                    const bytes = d && d.__rs === 7 ? d.items : [];
                    return Ok(R.mkString(rt.utf8BytesToStr(bytes)));
                }
                if (last === "from_utf8_lossy") {
                    const d = deref(args[0]);
                    const bytes = d && d.__rs === 7 ? d.items : [];
                    return R.mkString(rt.utf8BytesToStr(bytes));
                }
                break;
            case "Vec":
                if (last === "new" || last === "with_capacity" || last === "default") return { __rs: 7, items: [] };
                if (last === "from") {
                    const a = deref(args[0]);
                    if (a && a.__rs === 7) return cloneVal(a);
                    if (typeof a === "string" || (a && a.__rs === 4)) return { __rs: 7, items: rt.strToUtf8Bytes(strVal(a)) };
                    return { __rs: 7, items: [] };
                }
                break;
            case "HashMap":
            case "BTreeMap": {
                const ordered = parent === "BTreeMap";
                if (last === "new" || last === "with_capacity" || last === "default") return rt.mkMap(ordered);
                if (last === "from") {
                    const m = rt.mkMap(ordered);
                    const it = R.intoIter(args[0]);
                    for (;;) {
                        const x = it.next();
                        if (x === ITER_DONE) break;
                        const t = deref(x);
                        if (t && t.__rs === 3) rt.mapSetRaw(m, t.__items[0], t.__items[1]);
                    }
                    return m;
                }
                break;
            }
            case "HashSet":
            case "BTreeSet": {
                const ordered = parent === "BTreeSet";
                if (last === "new" || last === "with_capacity" || last === "default") return rt.mkSet(ordered);
                if (last === "from") {
                    const s = rt.mkSet(ordered);
                    const it = R.intoIter(args[0]);
                    for (;;) {
                        const x = it.next();
                        if (x === ITER_DONE) break;
                        s.set.set(mapKeyOf(x), x);
                    }
                    return s;
                }
                break;
            }
            case "VecDeque":
                if (last === "new" || last === "with_capacity" || last === "default") return rt.mkDeque(null);
                if (last === "from") {
                    const a = deref(args[0]);
                    return rt.mkDeque(a && a.__rs === 7 ? a.items.map(cloneVal) : null);
                }
                break;
            case "BinaryHeap":
                if (last === "new" || last === "with_capacity" || last === "default") return { __rs: 13, items: [] };
                if (last === "from") {
                    const h = { __rs: 13, items: [] };
                    const it = R.intoIter(args[0]);
                    for (;;) { const x = it.next(); if (x === ITER_DONE) break; R.callMethod(h, "push", [x]); }
                    return h;
                }
                break;
            case "Option":
                if (last === "Some") return Some(args[0]);
                if (last === "None") return None;
                break;
            case "Result":
                if (last === "Ok") return Ok(args[0]);
                if (last === "Err") return Err(args[0]);
                break;
            case "Box":
                if (last === "new") return { __rs: 14, kind: "box", v: args[0] };
                break;
            case "Rc":
                if (last === "new") return { __rs: 14, kind: "rc", v: args[0] };
                if (last === "clone") return args[0];
                break;
            case "Arc":
                if (last === "new") return { __rs: 14, kind: "arc", v: args[0] };
                if (last === "clone") return args[0];
                break;
            case "Cell":
                if (last === "new") return { __rs: 14, kind: "cell", v: args[0] };
                break;
            case "RefCell":
                if (last === "new") return { __rs: 14, kind: "refcell", v: args[0] };
                break;
            case "Mutex":
                if (last === "new") return { __rs: 14, kind: "mutex", v: args[0] };
                break;
            case "RwLock":
                if (last === "new") return { __rs: 14, kind: "rwlock", v: args[0] };
                break;
            case "OnceLock":
                if (last === "new") return { __rs: 14, kind: "oncelock", has: false, v: undefined };
                break;
            case "LazyLock":
                if (last === "new") return { __rs: 14, kind: "lazylock", has: false, v: undefined, init: args[0] };
                break;
            case "AtomicBool": case "AtomicIsize": case "AtomicUsize":
            case "AtomicI32": case "AtomicU32": case "AtomicI64": case "AtomicU64":
                if (last === "new") return { __rs: 14, kind: "atomic", v: args[0] };
                break;
            case "Default":
                if (last === "default") return rt.defaultOf(typeArgs[0] || "");
                break;
            case "mem":
                if (last === "swap") {
                    const tmp = R.rget(args[0]);
                    R.rset(args[0], R.rget(args[1]));
                    R.rset(args[1], tmp);
                    return undefined;
                }
                if (last === "replace") {
                    const old = R.rget(args[0]);
                    R.rset(args[0], args[1]);
                    return old;
                }
                if (last === "take") {
                    const old = R.rget(args[0]);
                    R.rset(args[0], rt.defaultOf(rt.typeTagOf(old)));
                    return old;
                }
                if (last === "drop" || last === "forget") return undefined;
                if (last === "size_of") return 8;
                break;
            case "cmp":
                if (last === "min") return cmpVal(args[0], args[1]) <= 0 ? args[0] : args[1];
                if (last === "max") return cmpVal(args[0], args[1]) >= 0 ? args[0] : args[1];
                break;
            case "iter":
                if (last === "empty") return rt.mkIter(function () { return ITER_DONE; });
                if (last === "once") {
                    let done = false;
                    const val = args[0];
                    return rt.mkIter(function () { if (done) return ITER_DONE; done = true; return val; });
                }
                if (last === "repeat") {
                    const val = args[0];
                    return rt.mkIter(function () { return cloneVal(val); });
                }
                if (last === "from_fn") {
                    const f = args[0];
                    return rt.mkIter(function () {
                        const r = deref(f());
                        return (r && r.__rs === 2 && r.__var === "Some") ? r.__data[0] : ITER_DONE;
                    });
                }
                if (last === "successors") {
                    let cur = deref(args[0]);
                    const f = args[1];
                    return rt.mkIter(function () {
                        if (!cur || cur.__rs !== 2 || cur.__var === "None") return ITER_DONE;
                        const out = cur.__data[0];
                        cur = deref(f(out));
                        return out;
                    });
                }
                break;
            case "panic":
                if (last === "catch_unwind") {
                    try {
                        return Ok(args[0]());
                    } catch (e) {
                        if (e && e.isRsPanic) return Err(R.mkString(e.message));
                        return Err(R.mkString(String(e)));
                    }
                }
                break;
            case "char":
                if (last === "from_u32") {
                    const n = Number(deref(args[0]));
                    if (n < 0 || n > 0x10FFFF || (n >= 0xD800 && n <= 0xDFFF)) return None;
                    return Some(R.mkChar(n));
                }
                break;
            case "i8": case "i16": case "i32": case "i64": case "isize":
            case "u8": case "u16": case "u32": case "u64": case "usize":
                if (last === "from_str_radix") {
                    const s = strVal(args[0]).trim();
                    const r = Number(deref(args[1]));
                    const v = parseInt(s, r);
                    return Number.isNaN(v) ? Err(R.mkString("invalid digit")) : Ok(v);
                }
                if (last === "from_le_bytes") {
                    const b = deref(args[0]).items;
                    return (b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24)) | 0;
                }
                if (last === "from_be_bytes") {
                    const b = deref(args[0]).items;
                    return ((b[0] << 24) | (b[1] << 16) | (b[2] << 8) | b[3]) | 0;
                }
                break;
            case "io":
                if (last === "stdout") return { __rs: 15, kind: "stdout" };
                if (last === "stderr") return { __rs: 15, kind: "stderr" };
                if (last === "stdin") return { __rs: 15, kind: "stdin" };
                break;
            case "Error":
                if (last === "new") return mkIoErr(rt.fmtDebug(args[0]), strVal(args[1]));
                if (last === "other") return mkIoErr("Other", strVal(args[0]));
                break;
            case "fs":
                if (last === "read_to_string") {
                    const txt = fsCall("read", strVal(args[0]));
                    return typeof txt === "string" ? Ok(R.mkString(txt)) : Err(mkIoErr("NotFound", "cannot read " + strVal(args[0])));
                }
                if (last === "read") {
                    const hex = fsCall("read_bytes_hex", strVal(args[0]));
                    if (typeof hex === "string") {
                        const out = [];
                        for (let i = 0; i + 1 < hex.length; i += 2) out.push(parseInt(hex.slice(i, i + 2), 16));
                        return Ok({ __rs: 7, items: out });
                    }
                    const txt = fsCall("read", strVal(args[0]));
                    return typeof txt === "string" ? Ok({ __rs: 7, items: rt.strToUtf8Bytes(txt) }) : Err(mkIoErr("NotFound", "cannot read " + strVal(args[0])));
                }
                if (last === "write") {
                    const d = deref(args[1]);
                    if (d && d.__rs === 7) {
                        let hx = "";
                        for (let i = 0; i < d.items.length; i++) hx += (Number(d.items[i]) & 0xFF).toString(16).padStart(2, "0");
                        const okHex = fsCall("write_bytes_hex", strVal(args[0]), hx);
                        if (okHex === true) return Ok(undefined);
                        if (okHex === false) return Err(mkIoErr("PermissionDenied", "failed to write " + strVal(args[0])));
                    }
                    const s = (d && d.__rs === 7) ? rt.utf8BytesToStr(d.items) : strVal(d);
                    return fsCall("write", strVal(args[0]), s) ? Ok(undefined) : Err(mkIoErr("PermissionDenied", "failed to write " + strVal(args[0])));
                }
                if (last === "exists") return Ok(fsCall("exists", strVal(args[0])) === true);
                if (last === "create_dir") return fsCall("mkdir", strVal(args[0])) ? Ok(undefined) : Err(mkIoErr("Other", "mkdir failed"));
                if (last === "create_dir_all") return fsCall("mkdir_all", strVal(args[0])) ? Ok(undefined) : Err(mkIoErr("Other", "mkdir_all failed"));
                if (last === "remove_file") {
                    const p = strVal(args[0]);
                    const fsOk = fsCall("remove_file", p) === true;
                    let sqlOk = false;
                    if (typeof __zipp_rs_sqlite === "function" && /\.(?:db|sqlite3?|db-wal|db-shm)$/i.test(p)) {
                        try {
                            const raw = __zipp_rs_sqlite("remove", p, "", "");
                            const parsed = typeof raw === "string" ? JSON.parse(raw) : null;
                            if (parsed && !parsed.error && parsed.removed) sqlOk = true;
                        } catch (_) {}
                    }
                    return (fsOk || sqlOk) ? Ok(undefined) : Err(mkIoErr("NotFound", "remove_file failed"));
                }
                if (last === "remove_dir") return fsCall("remove_dir", strVal(args[0])) ? Ok(undefined) : Err(mkIoErr("Other", "remove_dir failed"));
                if (last === "remove_dir_all") return fsCall("remove_dir_all", strVal(args[0])) ? Ok(undefined) : Err(mkIoErr("Other", "remove_dir_all failed"));
                if (last === "rename") return fsCall("rename", strVal(args[0]), strVal(args[1])) ? Ok(undefined) : Err(mkIoErr("Other", "rename failed"));
                if (last === "copy") {
                    const from = strVal(args[0]);
                    const to = strVal(args[1]);
                    const n = fsCall("copy", from, to);
                    if (typeof n === "number") return Ok(n);
                    if (typeof __zipp_rs_sqlite === "function" && (/\.(?:db|sqlite3?)$/i.test(from) || /\.(?:db|sqlite3?)$/i.test(to))) {
                        try {
                            const raw = __zipp_rs_sqlite("copy", from, to, "");
                            const parsed = typeof raw === "string" ? JSON.parse(raw) : null;
                            if (parsed && !parsed.error && typeof parsed.bytes === "number") {
                                return Ok(parsed.bytes);
                            }
                        } catch (_) {}
                    }
                    return Err(mkIoErr("Other", "copy failed"));
                }
                if (last === "canonicalize") {
                    const c = fsCall("canonicalize", strVal(args[0]));
                    return typeof c === "string" ? Ok(mkPath(c)) : Err(mkIoErr("NotFound", "canonicalize failed"));
                }
                if (last === "metadata") {
                    const raw = fsCall("metadata", strVal(args[0]));
                    if (typeof raw !== "string") return Err(mkIoErr("NotFound", "metadata failed"));
                    const m = JSON.parse(raw);
                    return Ok({ __rs: 15, kind: "metadata", len: m.len, is_file: m.is_file, is_dir: m.is_dir });
                }
                if (last === "read_dir") {
                    const raw = fsCall("read_dir", strVal(args[0]));
                    if (typeof raw !== "string") return Err(mkIoErr("NotFound", "read_dir failed"));
                    const list = JSON.parse(raw);
                    let i = 0;
                    return Ok(rt.mkIter(function () {
                        if (i >= list.length) return ITER_DONE;
                        const e = list[i++];
                        return Ok({ __rs: 15, kind: "dir_entry", name: e.name, path: e.path });
                    }));
                }
                break;
            case "File":
                if (last === "open") {
                    const p = strVal(args[0]);
                    if (fsCall("is_file", p) !== true) return Err(mkIoErr("NotFound", "No such file: " + p));
                    return Ok({ __rs: 15, kind: "file", path: p, append: false });
                }
                if (last === "create") {
                    const p = strVal(args[0]);
                    if (!fsCall("write", p, "")) return Err(mkIoErr("PermissionDenied", "Cannot create file: " + p));
                    return Ok({ __rs: 15, kind: "file", path: p, append: false });
                }
                break;
            case "OpenOptions":
                if (last === "new") return { __rs: 15, kind: "open_options", read: false, write: false, append: false, create: false, truncate: false };
                break;
            case "Path":
            case "PathBuf":
                if (last === "new") return mkPath(args.length > 0 ? args[0] : "");
                if (last === "from") return mkPath(args[0]);
                break;
            case "env":
                if (last === "args") {
                    const raw = envCall("args");
                    const list = typeof raw === "string" ? JSON.parse(raw) : [];
                    let i = 0;
                    return rt.mkIter(function () { return i >= list.length ? ITER_DONE : R.mkString(list[i++]); });
                }
                if (last === "var" || last === "var_os") {
                    const val = envCall("var", strVal(args[0]));
                    if (last === "var_os") return typeof val === "string" ? Some(R.mkString(val)) : None;
                    return typeof val === "string" ? Ok(R.mkString(val)) : Err(R.mkEnumUnit("VarError", "NotPresent"));
                }
                if (last === "vars") {
                    const raw = envCall("vars");
                    const pairs = typeof raw === "string" ? JSON.parse(raw) : [];
                    let i = 0;
                    return rt.mkIter(function () {
                        if (i >= pairs.length) return ITER_DONE;
                        const p = pairs[i++];
                        return R.mkTuple([R.mkString(p[0]), R.mkString(p[1])]);
                    });
                }
                if (last === "set_var") { envCall("set_var", strVal(args[0]), strVal(args[1])); return undefined; }
                if (last === "remove_var") { envCall("remove_var", strVal(args[0])); return undefined; }
                if (last === "current_dir") {
                    const d = envCall("current_dir");
                    return typeof d === "string" ? Ok(mkPath(d)) : Err(mkIoErr("Other", "current_dir unavailable"));
                }
                if (last === "temp_dir") {
                    const d = envCall("temp_dir");
                    return mkPath(typeof d === "string" ? d : "/tmp");
                }
                break;
            case "process":
                if (last === "exit") R.exit(args[0]);
                if (last === "id") return Number(envCall("pid") || 0);
                break;
            case "Command":
                if (last === "new") return { __rs: 15, kind: "command", program: strVal(args[0]), args: [], cwd: "", env: {} };
                break;
            case "Instant":
                if (last === "now") return { __rs: 15, kind: "instant", ns: timeCall("mono") };
                break;
            case "SystemTime":
                if (last === "now") return { __rs: 15, kind: "system_time", ns: timeCall("unix") };
                break;
            case "Duration":
                if (last === "from_secs") return { __rs: 15, kind: "duration", ns: BigInt(Math.trunc(Number(deref(args[0])))) * 1000000000n };
                if (last === "from_millis") return { __rs: 15, kind: "duration", ns: BigInt(Math.trunc(Number(deref(args[0])))) * 1000000n };
                if (last === "from_micros") return { __rs: 15, kind: "duration", ns: BigInt(Math.trunc(Number(deref(args[0])))) * 1000n };
                if (last === "from_nanos") return { __rs: 15, kind: "duration", ns: BigInt(Math.trunc(Number(deref(args[0])))) };
                if (last === "from_secs_f64" || last === "from_secs_f32") {
                    return { __rs: 15, kind: "duration", ns: BigInt(Math.round(Number(deref(args[0])) * 1e9)) };
                }
                break;
            case "thread":
                if (last === "sleep") {
                    const dur = deref(args[0]);
                    const ms = dur && dur.kind === "duration" ? Number(dur.ns / 1000000n) : Number(dur);
                    if (ms > 0) timeCall("sleep_ms", ms);
                    return undefined;
                }
                break;
            case "time":
                // `tokio::time::sleep(dur)` -> resolved Promise after sleep
                if (last === "sleep") {
                    const dur = deref(args[0]);
                    const ms = dur && dur.kind === "duration" ? Number(dur.ns / 1000000n) : Number(dur);
                    if (ms > 0) timeCall("sleep_ms", ms);
                    return Promise.resolve(undefined);
                }
                break;
            case "TcpListener":
                if (last === "bind") {
                    const id = netCall("bind", strVal(args[0]));
                    return (typeof id === "number" && id >= 0)
                        ? Ok({ __rs: 15, kind: "tcp_listener", id: id })
                        : Err(mkIoErr("AddrInUse", "failed to bind " + strVal(args[0])));
                }
                break;
            case "TcpStream":
                if (last === "connect") {
                    const id = netCall("connect", strVal(args[0]));
                    return (typeof id === "number" && id >= 0)
                        ? Ok({ __rs: 15, kind: "tcp_stream", id: id })
                        : Err(mkIoErr("ConnectionRefused", "failed to connect " + strVal(args[0])));
                }
                break;
            case "DefaultHasher":
                if (last === "new") return { __rs: 15, kind: "hasher", h: 0xcbf29ce484222325n };
                break;
            case "serde_json":
                if (last === "to_string") return Ok(R.mkString(JSON.stringify(toJsonRaw(args[0]))));
                if (last === "to_string_pretty") return Ok(R.mkString(JSON.stringify(toJsonRaw(args[0]), null, 2)));
                if (last === "to_value") return Ok(mkJson(toJsonRaw(args[0])));
                if (last === "from_str") {
                    try {
                        const raw = JSON.parse(strVal(args[0]));
                        return Ok(fromJsonRaw(raw, typeArgs && typeArgs[0] ? typeArgs[0] : ""));
                    } catch (e) {
                        return Err(R.mkString(String(e)));
                    }
                }
                break;
            case "Regex":
                if (last === "new") {
                    try {
                        return Ok({ __rs: 15, kind: "regex", re: new RegExp(strVal(args[0])) });
                    } catch (e) {
                        return Err(R.mkString(String(e)));
                    }
                }
                break;
            case "crypto":
                if (last === "sha256" || last === "sha512" || last === "md5"
                    || last === "hex_encode" || last === "hex_decode"
                    || last === "base64_encode" || last === "base64_decode"
                    || last === "base64url_encode" || last === "base64url_decode"
                    || last === "hash_password") {
                    const res = cryptoCall(last, strVal(args[0]));
                    return typeof res === "string" ? R.mkString(res) : R.mkString("");
                }
                if (last === "verify_password") {
                    return cryptoCall("verify_password", strVal(args[0]), strVal(args[1])) === true;
                }
                if (last === "hmac_sha256") {
                    const res = cryptoCall("hmac_sha256", strVal(args[0]), strVal(args[1]));
                    return typeof res === "string" ? R.mkString(res) : R.mkString("");
                }
                if (last === "jwt_sign") {
                    const payload = typeof args[0] === "string" || (args[0] && args[0].__rs === 4)
                        ? strVal(args[0])
                        : JSON.stringify(toJsonRaw(args[0]));
                    const tok = cryptoCall("jwt_sign", payload, strVal(args[1]));
                    return typeof tok === "string" && tok.length > 0
                        ? Ok(R.mkString(tok))
                        : Err(R.mkString("failed to sign JWT"));
                }
                if (last === "jwt_verify") {
                    const raw = cryptoCall("jwt_verify", strVal(args[0]), strVal(args[1]));
                    if (typeof raw !== "string") return Err(R.mkString("invalid JWT signature"));
                    try {
                        const parsed = JSON.parse(raw);
                        if (parsed && typeof parsed.exp === "number") {
                            const nowSec = Number(timeCall("unix") / 1000000000n);
                            if (nowSec > parsed.exp) return Err(R.mkString("JWT expired"));
                        }
                        return Ok(fromJsonRaw(parsed, typeArgs && typeArgs[0] ? typeArgs[0] : ""));
                    } catch (e) {
                        return Err(R.mkString("invalid JWT payload"));
                    }
                }
                if (last === "random_bytes") {
                    const hex = cryptoCall("random_bytes", String(Number(deref(args[0]))));
                    const out = [];
                    if (typeof hex === "string") {
                        for (let i = 0; i + 1 < hex.length; i += 2) out.push(parseInt(hex.slice(i, i + 2), 16));
                    }
                    return { __rs: 7, items: out };
                }
                if (last === "uuid_v4") {
                    const u = cryptoCall("uuid_v4", "");
                    return R.mkString(typeof u === "string" ? u : "00000000-0000-4000-8000-000000000000");
                }
                break;
            case "Utc":
                if (last === "now") return { __rs: 15, kind: "datetime", ns: timeCall("unix") };
                break;
            case "html":
                if (last === "escape") {
                    const s = strVal(args[0])
                        .replace(/&/g, "&amp;")
                        .replace(/</g, "&lt;")
                        .replace(/>/g, "&gt;")
                        .replace(/"/g, "&quot;")
                        .replace(/'/g, "&#39;");
                    return R.mkString(s);
                }
                break;
            case "BufReader":
            case "Cursor":
                if (last === "new") {
                    const src = deref(args[0]);
                    let txt = "";
                    if (src && src.__rs === 15 && src.kind === "file") {
                        const r = fsCall("read", src.path);
                        txt = typeof r === "string" ? r : "";
                    } else if (src && src.__rs === 15 && (src.kind === "cursor" || src.kind === "buf_reader")) {
                        txt = src.text;
                    } else if (src && src.__rs === 7) {
                        txt = rt.utf8BytesToStr(src.items);
                    } else {
                        txt = strVal(src);
                    }
                    return { __rs: 15, kind: parent === "Cursor" ? "cursor" : "buf_reader", text: txt, pos: 0 };
                }
                break;
            case "Runtime":
                if (last === "new") return Ok({ __rs: 15, kind: "runtime" });
                break;
            case "executor":
                if (last === "block_on") return R.blockOn(args[0]);
                break;
            case "Router":
                if (last === "new") {
                    const r = { __rs: 15, kind: "router", routes: [], middlewares: [], staticDirs: [], fallback: null, state: undefined, hasState: false };
                    rt.setRouter(r);
                    return r;
                }
                break;
            case "Request":
                if (last === "new") return mkRequest(strVal(args[0]), strVal(args[1]), null, args[2] ? strVal(args[2]) : "", null);
                if (last === "get") return mkRequest("GET", strVal(args[0]), null, "", null);
                if (last === "post") return mkRequest("POST", strVal(args[0]), { "content-type": "application/json" }, args[1] ? strVal(args[1]) : "", null);
                break;
            case "Response": {
                const mkAuto = function (code, arg, fallback) {
                    if (arg === undefined) return mkResponse(code, fallback, "text/plain; charset=utf-8");
                    const v = deref(arg);
                    if (typeof v === "string" || (v && v.__rs === 4)) {
                        return mkResponse(code, strVal(v), "text/plain; charset=utf-8");
                    }
                    return mkResponse(code, JSON.stringify(toJsonRaw(v)), "application/json; charset=utf-8");
                };
                if (last === "ok") return mkAuto(200, args[0], "");
                if (last === "text") return mkResponse(200, strVal(args[0]), "text/plain; charset=utf-8");
                if (last === "created") return mkAuto(201, args[0], "");
                if (last === "no_content") return mkResponse(204, "", "text/plain; charset=utf-8");
                if (last === "html") return mkResponse(200, strVal(args[0]), "text/html; charset=utf-8");
                if (last === "json") return mkResponse(200, JSON.stringify(toJsonRaw(args[0])), "application/json; charset=utf-8");
                if (last === "status") return mkAuto(Number(deref(args[0])), args[1], "");
                if (last === "bad_request") return mkAuto(400, args[0], "Bad Request");
                if (last === "unauthorized") return mkAuto(401, args[0], "Unauthorized");
                if (last === "forbidden") return mkAuto(403, args[0], "Forbidden");
                if (last === "not_found") return mkAuto(404, args[0], "Not Found");
                if (last === "conflict") return mkAuto(409, args[0], "Conflict");
                if (last === "internal_error") return mkAuto(500, args[0], "Internal Server Error");
                if (last === "redirect") {
                    const resp = mkResponse(302, "", "text/plain; charset=utf-8");
                    rt.mapSetRaw(resp.headers, R.mkString("Location"), R.mkString(strVal(args[0])));
                    return resp;
                }
                break;
            }
            case "Client":
                if (last === "new") return { __rs: 15, kind: "http_client" };
                break;
            case "http":
                if (last === "get") {
                    if (typeof args[0] === "function") return { __rs: 15, kind: "method_router", entries: [["GET", args[0]]] };
                    return httpClientRequest("GET", strVal(args[0]), null, "");
                }
                if (last === "post") {
                    if (typeof args[0] === "function") return { __rs: 15, kind: "method_router", entries: [["POST", args[0]]] };
                    return httpClientRequest("POST", strVal(args[0]), null, args[1] ? strVal(args[1]) : "");
                }
                if (last === "put") return { __rs: 15, kind: "method_router", entries: [["PUT", args[0]]] };
                if (last === "delete") return { __rs: 15, kind: "method_router", entries: [["DELETE", args[0]]] };
                if (last === "patch") return { __rs: 15, kind: "method_router", entries: [["PATCH", args[0]]] };
                break;
            case "Store":
                if (last === "new") return { __rs: 15, kind: "kv", path: null, data: Object.create(null) };
                if (last === "open") {
                    const p = strVal(args[0]);
                    let data = Object.create(null);
                    const raw = fsCall("read", p);
                    if (typeof raw === "string" && raw.trim().length > 0) {
                        try { data = Object.assign(Object.create(null), JSON.parse(raw)); } catch (_) {}
                    }
                    return Ok({ __rs: 15, kind: "kv", path: p, data: data });
                }
                break;
            case "tokio":
                if (last === "spawn") return Promise.resolve(args[0]);
                break;
        }

        // Enum tuple constructor fallback (e.g. `MyEnum::Variant(a, b)`)
        if (rt.enums[parent]) {
            return R.mkEnumTuple(parent, last, args);
        }

        // Single-segment free stdlib helpers (e.g. `use http::{get, post}` or `use std::mem::swap`)
        if (pathParts.length === 1) {
            if (last === "Some") return Some(args[0]);
            if (last === "Ok") return Ok(args[0]);
            if (last === "Err") return Err(args[0]);
            if (last === "drop") return undefined;
            if (last === "get") return typeof args[0] === "function" ? { __rs: 15, kind: "method_router", entries: [["GET", args[0]]] } : httpClientRequest("GET", strVal(args[0]), null, "");
            if (last === "post") return typeof args[0] === "function" ? { __rs: 15, kind: "method_router", entries: [["POST", args[0]]] } : httpClientRequest("POST", strVal(args[0]), null, args[1] ? strVal(args[1]) : "");
            if (last === "put") return { __rs: 15, kind: "method_router", entries: [["PUT", args[0]]] };
            if (last === "delete") return { __rs: 15, kind: "method_router", entries: [["DELETE", args[0]]] };
            if (last === "patch") return { __rs: 15, kind: "method_router", entries: [["PATCH", args[0]]] };
        }

        R.panic("unresolved associated function `" + full + "`");
    };

    // ---- Path constant / unit-variant lookup ----
    R.resolveConstPath = function (pathParts) {
        const full = pathParts.join("::");
        if (Object.prototype.hasOwnProperty.call(rt.consts || {}, full)) return R.getConst(full);
        const last = pathParts[pathParts.length - 1];
        const parent = pathParts.length >= 2 ? pathParts[pathParts.length - 2] : "";
        if (parent === "Option" && last === "None") return None;
        if (parent === "Ordering") {
            if (last === "Less") return R.mkEnumUnit("Ordering", "Less", -1);
            if (last === "Equal") return R.mkEnumUnit("Ordering", "Equal", 0);
            if (last === "Greater") return R.mkEnumUnit("Ordering", "Greater", 1);
            // `std::sync::atomic::Ordering::SeqCst` etc.
            return R.mkEnumUnit("Ordering", last, 0);
        }
        if (last === "UNIX_EPOCH") return { __rs: 15, kind: "system_time", ns: 0n };
        if (parent === "Duration" && last === "ZERO") return { __rs: 15, kind: "duration", ns: 0n };
        if (parent === "ErrorKind") return R.mkEnumUnit("ErrorKind", last, 0);
        if (parent === "Shutdown") return R.mkEnumUnit("Shutdown", last, 0);
        if (parent === "StatusCode") {
            const codes = {
                OK: 200, CREATED: 201, ACCEPTED: 202, NO_CONTENT: 204,
                MOVED_PERMANENTLY: 301, FOUND: 302, NOT_MODIFIED: 304,
                BAD_REQUEST: 400, UNAUTHORIZED: 401, FORBIDDEN: 403, NOT_FOUND: 404,
                METHOD_NOT_ALLOWED: 405, CONFLICT: 409, GONE: 410,
                PAYLOAD_TOO_LARGE: 413, UNSUPPORTED_MEDIA_TYPE: 415,
                UNPROCESSABLE_ENTITY: 422, TOO_MANY_REQUESTS: 429,
                INTERNAL_SERVER_ERROR: 500, NOT_IMPLEMENTED: 501,
                BAD_GATEWAY: 502, SERVICE_UNAVAILABLE: 503, GATEWAY_TIMEOUT: 504
            };
            if (Object.prototype.hasOwnProperty.call(codes, last)) return codes[last];
        }
        if (parent === "Value" && last === "Null") return mkJson(null);
        if (parent === "consts") {
            if (last === "PI") return Math.PI;
            if (last === "TAU") return Math.PI * 2;
            if (last === "E") return Math.E;
            if (last === "SQRT_2") return Math.SQRT2;
            if (last === "FRAC_PI_2") return Math.PI / 2;
            if (last === "FRAC_PI_4") return Math.PI / 4;
            if (last === "LN_2") return Math.LN2;
            if (last === "LN_10") return Math.LN10;
        }
        if (parent === "i8") { if (last === "MIN") return -128; if (last === "MAX") return 127; }
        if (parent === "i16") { if (last === "MIN") return -32768; if (last === "MAX") return 32767; }
        if (parent === "i32") { if (last === "MIN") return -2147483648; if (last === "MAX") return 2147483647; }
        if (parent === "i64" || parent === "isize") { if (last === "MIN") return -9007199254740991; if (last === "MAX") return 9007199254740991; }
        if (parent === "u8") { if (last === "MIN") return 0; if (last === "MAX") return 255; }
        if (parent === "u16") { if (last === "MIN") return 0; if (last === "MAX") return 65535; }
        if (parent === "u32") { if (last === "MIN") return 0; if (last === "MAX") return 4294967295; }
        if (parent === "u64" || parent === "usize") { if (last === "MIN") return 0; if (last === "MAX") return 9007199254740991; }
        if (parent === "f64" || parent === "f32") {
            if (last === "INFINITY") return Infinity;
            if (last === "NEG_INFINITY") return -Infinity;
            if (last === "NAN") return NaN;
            if (last === "EPSILON") return Number.EPSILON;
            if (last === "MAX") return Number.MAX_VALUE;
            if (last === "MIN") return -Number.MAX_VALUE;
        }
        if (parent === "str") {
            if (last === "to_owned" || last === "to_string") return function (x) { return R.mkString(strVal(x)); };
            if (last === "trim") return function (x) { return strVal(x).trim(); };
            if (last === "to_lowercase") return function (x) { return R.mkString(strVal(x).toLowerCase()); };
            if (last === "to_uppercase") return function (x) { return R.mkString(strVal(x).toUpperCase()); };
            if (last === "len") return function (x) { return rt.utf8ByteLen(strVal(x)); };
            if (last === "is_empty") return function (x) { return strVal(x).length === 0; };
            if (last === "as_bytes") return function (x) { return { __rs: 7, items: rt.strToUtf8Bytes(strVal(x)) }; };
        }
        if ((parent === "ToOwned" && last === "to_owned") || (parent === "ToString" && last === "to_string")) {
            return function (x) { return R.callMethod(x, last, []); };
        }
        if (parent === "String" && last === "from") return function (x) { return R.mkString(strVal(x)); };
        if (parent === "Clone" && last === "clone") return function (x) { return rt.cloneVal(x); };
        if (parent === "Option" && last === "Some") return Some;
        if (parent === "Result" && last === "Ok") return Ok;
        if (parent === "Result" && last === "Err") return Err;
        if ((parent === "i64" || parent === "i32" || parent === "u64" || parent === "u32" || parent === "usize" || parent === "f64") && last === "from") {
            return function (x) { return Number(deref(x)); };
        }
        if (rt.resolveEcoConst) {
            const ecoC = rt.resolveEcoConst(pathParts);
            if (ecoC !== rt.ECO_MISS) return ecoC;
        }
        if (rt.enums[parent]) {
            const info = rt.enums[parent];
            const disc = info.variants && Object.prototype.hasOwnProperty.call(info.variants, last) ? info.variants[last] : null;
            return R.mkEnumUnit(parent, last, disc);
        }
        const c = R.getConst(full);
        if (c !== undefined) return c;
        const c2 = R.getConst(last);
        if (c2 !== undefined) return c2;
        return R.mkEnumUnit(parent || last, last, null);
    };
})(__zipp_rs);
