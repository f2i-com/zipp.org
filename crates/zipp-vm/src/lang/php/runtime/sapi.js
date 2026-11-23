/* ZIPP PHP runtime — HTTP SAPI (superglobals, header, http_response_code, setcookie,
 * sessions, include/require, Worker Mode) & environment. Apache-2.0. */
(function (R) {
    "use strict";
    const rt = R.__rt;
    const reg = rt.regFn;

    // ---- SAPI HTTP response state ----
    let statusCode = 200;
    let respHeaders = []; // [{ name, lower, value }]
    let sapiMode = "cli";
    let sessionActive = false;
    let sessionId = "";
    const sessionStore = new Map(); // in-memory session store (persists across worker requests!)
    let workerHandler = null;
    rt.rawInput = "";

    function resetSapiState() {
        statusCode = 200;
        respHeaders = [];
        sessionActive = false;
        sessionId = "";
        rt.rawInput = "";
        rt.resetOutput();
    }
    rt.resetSapiState = resetSapiState;

    function initSuperglobals(req) {
        const g = rt.globalScope;
        const mkArr = rt.makeArr;
        const set = rt.arrSetInPlace;

        const server = mkArr();
        const get = mkArr();
        const post = mkArr();
        const cookie = mkArr();
        const files = mkArr();
        const request = mkArr();
        const env = mkArr();

        // Populate $_ENV from native host env if available
        if (typeof __zipp_php_env === "function") {
            const envObj = __zipp_php_env();
            if (envObj && typeof envObj === "object") {
                for (const [k, v] of Object.entries(envObj)) set(env, k, String(v));
            }
        }

        if (req) {
            sapiMode = req.sapi || "zipp-server";
            rt.consts.set("PHP_SAPI", sapiMode);
            rt.rawInput = req.body || "";

            if (req.server) {
                for (const [k, v] of Object.entries(req.server)) set(server, k, String(v));
            }
            if (req.query) {
                const parsed = rt.parseQueryString(req.query);
                for (const [k, v] of parsed.m) { set(get, k, v); set(request, k, v); }
            }
            if (req.cookies) {
                for (const [k, v] of Object.entries(req.cookies)) { set(cookie, k, String(v)); set(request, k, String(v)); }
            }
            // Parse POST form body if application/x-www-form-urlencoded
            const ct = (req.contentType || "").toLowerCase();
            if (ct.includes("application/x-www-form-urlencoded") && rt.rawInput.length > 0) {
                const parsed = rt.parseQueryString(rt.rawInput);
                for (const [k, v] of parsed.m) { set(post, k, v); set(request, k, v); }
            }
            if (req.post) {
                for (const [k, v] of Object.entries(req.post)) { set(post, k, String(v)); set(request, k, String(v)); }
            }
            if (req.files) {
                for (const [field, f] of Object.entries(req.files)) {
                    const entry = mkArr();
                    set(entry, "name", f.name || "");
                    set(entry, "type", f.type || "application/octet-stream");
                    set(entry, "tmp_name", f.tmp_name || "");
                    set(entry, "error", BigInt(f.error || 0));
                    set(entry, "size", BigInt(f.size || 0));
                    set(files, field, entry);
                }
            }
        } else {
            // CLI defaults (REQUEST_METHOD / REQUEST_URI remain unset, matching PHP CLI)
            if (typeof __zipp_php_sapi === "function") {
                const cliInfo = __zipp_php_sapi("cli_info", "");
                if (cliInfo) {
                    try {
                        const info = JSON.parse(cliInfo);
                        if (info.script) {
                            set(server, "SCRIPT_FILENAME", info.script);
                            set(server, "PHP_SELF", info.script);
                        }
                        if (Array.isArray(info.argv)) {
                            const argvArr = mkArr();
                            for (const a of info.argv) rt.arrPushInPlace(argvArr, String(a));
                            set(server, "argv", argvArr);
                            set(server, "argc", BigInt(info.argv.length));
                            g.set("argv", rt.mkRef(argvArr));
                            g.set("argc", rt.mkRef(BigInt(info.argv.length)));
                        }
                    } catch (_) {}
                }
            }
        }

        g.set("_SERVER", rt.mkRef(server));
        g.set("_GET", rt.mkRef(get));
        g.set("_POST", rt.mkRef(post));
        g.set("_COOKIE", rt.mkRef(cookie));
        g.set("_FILES", rt.mkRef(files));
        g.set("_REQUEST", rt.mkRef(request));
        g.set("_ENV", rt.mkRef(env));
    }
    rt.initSuperglobals = initSuperglobals;

    // ---- HTTP SAPI builtins ----
    reg("php_sapi_name", function () { return sapiMode; });
    reg("http_response_code", function (code) {
        const prev = BigInt(statusCode);
        if (code !== undefined && rt.deref(code) !== null) statusCode = Number(rt.toInt(code));
        return prev;
    });
    reg("header", function (line, replace, code) {
        line = rt.toStr(line);
        const rep = replace === undefined ? true : rt.truthy(replace);
        if (code !== undefined && rt.deref(code) !== null) {
            const c = Number(rt.toInt(code));
            if (c > 0) statusCode = c;
        }
        // Status line: `HTTP/1.1 404 Not Found`
        const sm = /^HTTP\/\d+\.\d+\s+(\d{3})/i.exec(line);
        if (sm) { statusCode = Number(sm[1]); return null; }
        const colon = line.indexOf(":");
        if (colon < 0) return null;
        const name = line.slice(0, colon).trim();
        const lower = name.toLowerCase();
        const value = line.slice(colon + 1).trim();
        if (lower === "location" && statusCode === 200) statusCode = 302;
        if (rep) respHeaders = respHeaders.filter(h => h.lower !== lower);
        respHeaders.push({ name: name, lower: lower, value: value });
        return null;
    });
    reg("headers_sent", function () { return rt.headersSent(); });
    reg("headers_list", function () {
        const out = rt.makeArr();
        for (const h of respHeaders) rt.arrPushInPlace(out, h.name + ": " + h.value);
        return out;
    });
    reg("header_remove", function (name) {
        if (name === undefined || rt.deref(name) === null) { respHeaders = []; return null; }
        const lo = rt.toStr(name).toLowerCase();
        respHeaders = respHeaders.filter(h => h.lower !== lo);
        return null;
    });

    function setCookieImpl(name, value, expiresOrOpts, path, domain, secure, httponly, encodeVal) {
        const n = rt.toStr(name);
        let v = value === undefined ? "" : rt.toStr(value);
        if (encodeVal) v = rt.funcs.get("urlencode").builtin(v);
        let exp = 0, p = "", dom = "", sec = false, ho = false, samesite = "";
        const eo = rt.deref(expiresOrOpts);
        if (eo && eo.__php === 1) {
            if (eo.m.has("expires")) exp = Number(rt.toInt(eo.m.get("expires")));
            if (eo.m.has("path")) p = rt.toStr(eo.m.get("path"));
            if (eo.m.has("domain")) dom = rt.toStr(eo.m.get("domain"));
            if (eo.m.has("secure")) sec = rt.truthy(eo.m.get("secure"));
            if (eo.m.has("httponly")) ho = rt.truthy(eo.m.get("httponly"));
            if (eo.m.has("samesite")) samesite = rt.toStr(eo.m.get("samesite"));
        } else {
            if (eo !== undefined && eo !== null) exp = Number(rt.toInt(eo));
            if (path !== undefined) p = rt.toStr(path);
            if (domain !== undefined) dom = rt.toStr(domain);
            if (secure !== undefined) sec = rt.truthy(secure);
            if (httponly !== undefined) ho = rt.truthy(httponly);
        }
        let hdr = n + "=" + v;
        if (exp > 0) hdr += "; Expires=" + new Date(exp * 1000).toUTCString();
        if (p) hdr += "; Path=" + p;
        if (dom) hdr += "; Domain=" + dom;
        if (sec) hdr += "; Secure";
        if (ho) hdr += "; HttpOnly";
        if (samesite) hdr += "; SameSite=" + samesite;
        respHeaders.push({ name: "Set-Cookie", lower: "set-cookie", value: hdr });
        return true;
    }
    reg("setcookie", function (n, v, e, p, d, s, h) { return setCookieImpl(n, v, e, p, d, s, h, true); });
    reg("setrawcookie", function (n, v, e, p, d, s, h) { return setCookieImpl(n, v, e, p, d, s, h, false); });

    // ---- Sessions (`session_start`, `$_SESSION`) ----
    function persistSession() {
        if (!sessionActive || !sessionId) return;
        const cell = rt.globalScope.get("_SESSION");
        const data = cell ? cell.v : rt.makeArr();
        const serialized = rt.funcs.get("serialize").builtin(data);
        sessionStore.set(sessionId, serialized);
        // Also persist to temp dir when FS is available so shared-nothing requests share sessions!
        const tmp = rt.fsCall("tmpdir", "", "", "");
        if (tmp) {
            rt.fsCall("write", tmp + "/zipp_sess_" + sessionId, serialized, "");
        }
    }
    rt.persistSession = persistSession;

    reg("session_start", function () {
        if (sessionActive) return true;
        const cookies = rt.globalScope.get("_COOKIE");
        let sid = "";
        if (cookies && cookies.v && cookies.v.__php === 1 && cookies.v.m.has("PHPSESSID")) {
            sid = rt.toStr(cookies.v.m.get("PHPSESSID")).replace(/[^a-zA-Z0-9]/g, "");
        }
        if (!sid) {
            const raw = rt.funcs.get("random_bytes").builtin(16n);
            sid = rt.funcs.get("bin2hex").builtin(raw);
            setCookieImpl("PHPSESSID", sid, 0n, "/", "", false, true, false);
        }
        sessionId = sid;
        sessionActive = true;
        let data = rt.makeArr();
        let saved = sessionStore.get(sid);
        if (saved === undefined) {
            const tmp = rt.fsCall("tmpdir", "", "", "");
            if (tmp) {
                const r = rt.fsCall("read", tmp + "/zipp_sess_" + sid, "", "");
                if (typeof r === "string") saved = r;
            }
        }
        if (typeof saved === "string") {
            const parsed = rt.funcs.get("unserialize").builtin(saved);
            if (parsed && parsed.__php === 1) data = parsed;
        }
        rt.globalScope.set("_SESSION", rt.mkRef(data));
        return true;
    });
    let sessionName = "PHPSESSID";
    let sessionCacheLimiter = "nocache";
    let sessionCacheExpire = 180n;
    let sessionCookieParams = { lifetime: 0n, path: "/", domain: "", secure: false, httponly: false, samesite: "" };
    reg("session_id", function (id) {
        const prev = sessionId;
        if (id !== undefined) sessionId = rt.toStr(id);
        return prev;
    });
    reg("session_name", function (name) {
        const prev = sessionName;
        if (name !== undefined && rt.deref(name) !== null) sessionName = rt.toStr(name);
        return prev;
    });
    reg("session_status", function () { return sessionActive ? 2n : 1n; });
    reg("session_write_close", function () { persistSession(); sessionActive = false; return true; });
    reg("session_commit", function () { persistSession(); sessionActive = false; return true; });
    reg("session_abort", function () { sessionActive = false; return true; });
    reg("session_register_shutdown", function () { return null; });
    reg("session_cache_limiter", function (v) {
        const prev = sessionCacheLimiter;
        if (v !== undefined && rt.deref(v) !== null) sessionCacheLimiter = rt.toStr(v);
        return prev;
    });
    reg("session_cache_expire", function (v) {
        const prev = sessionCacheExpire;
        if (v !== undefined && rt.deref(v) !== null) sessionCacheExpire = rt.toInt(v);
        return prev;
    });
    reg("session_get_cookie_params", function () {
        const a = rt.makeArr();
        rt.arrSetInPlace(a, "lifetime", sessionCookieParams.lifetime);
        rt.arrSetInPlace(a, "path", sessionCookieParams.path);
        rt.arrSetInPlace(a, "domain", sessionCookieParams.domain);
        rt.arrSetInPlace(a, "secure", sessionCookieParams.secure);
        rt.arrSetInPlace(a, "httponly", sessionCookieParams.httponly);
        rt.arrSetInPlace(a, "samesite", sessionCookieParams.samesite);
        return a;
    });
    reg("session_set_cookie_params", function (lifetimeOrOpts, path, domain, secure, httponly) {
        if (lifetimeOrOpts && lifetimeOrOpts.__php === 1) {
            if (lifetimeOrOpts.m.has("lifetime")) sessionCookieParams.lifetime = rt.toInt(lifetimeOrOpts.m.get("lifetime"));
            if (lifetimeOrOpts.m.has("path")) sessionCookieParams.path = rt.toStr(lifetimeOrOpts.m.get("path"));
            if (lifetimeOrOpts.m.has("domain")) sessionCookieParams.domain = rt.toStr(lifetimeOrOpts.m.get("domain"));
            if (lifetimeOrOpts.m.has("secure")) sessionCookieParams.secure = rt.truthy(lifetimeOrOpts.m.get("secure"));
            if (lifetimeOrOpts.m.has("httponly")) sessionCookieParams.httponly = rt.truthy(lifetimeOrOpts.m.get("httponly"));
            if (lifetimeOrOpts.m.has("samesite")) sessionCookieParams.samesite = rt.toStr(lifetimeOrOpts.m.get("samesite"));
        } else {
            if (lifetimeOrOpts !== undefined) sessionCookieParams.lifetime = rt.toInt(lifetimeOrOpts);
            if (path !== undefined && rt.deref(path) !== null) sessionCookieParams.path = rt.toStr(path);
            if (domain !== undefined && rt.deref(domain) !== null) sessionCookieParams.domain = rt.toStr(domain);
            if (secure !== undefined && rt.deref(secure) !== null) sessionCookieParams.secure = rt.truthy(secure);
            if (httponly !== undefined && rt.deref(httponly) !== null) sessionCookieParams.httponly = rt.truthy(httponly);
        }
        return true;
    });
    reg("session_regenerate_id", function (deleteOld) {
        if (deleteOld && rt.truthy(deleteOld) && sessionId) sessionStore.delete(sessionId);
        const raw = rt.funcs.get("random_bytes").builtin(16n);
        sessionId = rt.funcs.get("bin2hex").builtin(raw);
        return true;
    });
    reg("session_unset", function () {
        rt.globalScope.set("_SESSION", rt.mkRef(rt.makeArr()));
        return true;
    });
    reg("session_destroy", function () {
        if (sessionId) {
            sessionStore.delete(sessionId);
            const tmp = rt.fsCall("tmpdir", "", "", "");
            if (tmp) rt.fsCall("unlink", tmp + "/zipp_sess_" + sessionId, "", "");
        }
        rt.globalScope.set("_SESSION", rt.mkRef(rt.makeArr()));
        sessionActive = false;
        return true;
    });

    // ---- Environment & error/shutdown hooks ----
    reg("getenv", function (name) {
        const envCell = rt.globalScope.get("_ENV");
        if (name === undefined || rt.deref(name) === null) return envCell ? rt.share(envCell.v) : rt.makeArr();
        const k = rt.toStr(name);
        if (envCell && envCell.v && envCell.v.__php === 1 && envCell.v.m.has(k)) {
            return rt.deref(envCell.v.m.get(k));
        }
        return false;
    });
    reg("putenv", function (spec) {
        const s = rt.toStr(spec);
        const eq = s.indexOf("=");
        const envCell = rt.globalScope.get("_ENV");
        if (!envCell || !envCell.v || envCell.v.__php !== 1) return false;
        if (eq < 0) { envCell.v._list = false; envCell.v._e = null; envCell.v.m.delete(s); }
        else rt.arrSetInPlace(envCell.v, s.slice(0, eq), s.slice(eq + 1));
        return true;
    });
    rt.consts.set("CONNECTION_NORMAL", 0n);
    rt.consts.set("CONNECTION_ABORTED", 1n);
    rt.consts.set("CONNECTION_TIMEOUT", 2n);
    reg("connection_status", function () { return 0n; });
    reg("connection_aborted", function () { return 0n; });
    reg("error_reporting", function (lvl) { return rt.errorReporting(lvl); });
    reg("set_error_handler", function (cb) { return rt.setErrorHandler(cb); });
    reg("restore_error_handler", function () { return rt.restoreErrorHandler(); });
    reg("set_exception_handler", function (cb) { return rt.setExceptionHandler(cb); });
    reg("restore_exception_handler", function () { return rt.restoreExceptionHandler(); });
    reg("register_shutdown_function", function (cb) { rt.registerShutdown(cb, Array.from(arguments).slice(1)); return null; });

    // ---- Worker Mode registration (`zipp_handle_requests(callable $handler)`) ----
    reg("zipp_handle_requests", function (cb) {
        workerHandler = cb;
        return true;
    });
    rt.getWorkerHandler = function () { return workerHandler; };

    // ---- Dynamic include / require / eval ----
    // kind: 0 = include, 1 = include_once, 2 = require, 3 = require_once
    R.include = function (kind, pathVal, curFile, curDir, scopeMap) {
        const reqPath = rt.toStr(pathVal);
        const once = kind === 1 || kind === 3;
        const required = kind === 2 || kind === 3;
        if (typeof __zipp_php_include !== "function") {
            if (required) rt.throwError("Error", "Failed opening required '" + reqPath + "' (dynamic include unavailable)");
            return false;
        }
        const compiled = __zipp_php_include(reqPath, curDir || "", once ? 1 : 0);
        if (compiled === null || compiled === undefined) {
            if (required) rt.throwError("Error", "Failed opening required '" + reqPath + "'");
            return false;
        }
        if (compiled === true) {
            // Already included once (`include_once` / `require_once`)
            return true;
        }
        rt.includedFiles.add(reqPath);
        // `compiled` is a JS function (`FuncProto`) expecting (args, phpThis, calledCls, scopeMap)
        const holder = { code: compiled, statics: null };
        const res = holder.code([], null, null, scopeMap || rt.globalScope);
        return res === undefined ? 1n : res;
    };

    R.evalCode = function (codeVal, curDir, scopeMap) {
        const src = "<?php " + rt.toStr(codeVal);
        if (typeof __zipp_php_include !== "function") {
            rt.throwError("Error", "eval() is not available in this build");
        }
        const compiled = __zipp_php_include(src, curDir || "", 2); // mode 2 = inline eval source
        const holder = { code: compiled, statics: null };
        const res = holder.code([], null, null, scopeMap || rt.globalScope);
        return res === undefined ? null : res;
    };

    rt.takeSapiResponse = function () {
        // Default Content-Type if none set
        let hasCt = false;
        for (const h of respHeaders) if (h.lower === "content-type") { hasCt = true; break; }
        if (!hasCt) respHeaders.unshift({ name: "Content-Type", lower: "content-type", value: "text/html; charset=UTF-8" });
        const body = rt.takeRawOutput();
        return JSON.stringify({
            status: statusCode,
            headers: respHeaders.map(h => [h.name, h.value]),
            body: body
        });
    };
})(__zipp_php);
