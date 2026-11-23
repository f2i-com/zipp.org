/* ZIPP PHP runtime — Full Reflection API, SPL classes/iterators, Closure::bind/fromCallable,
 * ctype, extended mbstring/array/string/PCRE/filesystem/runtime builtins for real PHP frameworks
 * (Laravel/Illuminate, Symfony, Composer, PSR). Apache-2.0. */
(function (R) {
    "use strict";
    const rt = R.__rt;
    const reg = rt.regFn;
    const defC = function (k, v) { rt.consts.set(k, v); };

    // ---- Additional standard PHP constants ----
    defC("PHP_OS", "Linux");
    defC("PHP_OS_FAMILY", "Linux");
    defC("PHP_BINARY", "zipp");
    defC("PHP_EXTRA_VERSION", "-zipp");
    defC("PHP_ZTS", 0n);
    defC("PHP_DEBUG", 0n);
    defC("PHP_MAXPATHLEN", 4096n);
    defC("DEFAULT_INCLUDE_PATH", ".");
    defC("ZEND_THREAD_SAFE", false);
    defC("E_CORE_ERROR", 16n);
    defC("E_CORE_WARNING", 32n);
    defC("E_COMPILE_ERROR", 64n);
    defC("E_COMPILE_WARNING", 128n);
    defC("E_STRICT", 2048n);
    defC("E_RECOVERABLE_ERROR", 4096n);
    defC("E_USER_DEPRECATED", 16384n);
    defC("SORT_LOCALE_STRING", 5n);
    defC("SORT_NATURAL", 6n);
    defC("SORT_FLAG_CASE", 8n);
    defC("COUNT_NORMAL", 0n);
    defC("COUNT_RECURSIVE", 1n);
    defC("PHP_QUERY_RFC1738", 1n);
    defC("PHP_QUERY_RFC3986", 2n);
    defC("ENT_XHTML", 32n);
    defC("ENT_XML1", 16n);
    defC("ENT_DISALLOWED", 128n);
    defC("JSON_PRESERVE_ZERO_FRACTION", 1024n);
    defC("JSON_PARTIAL_OUTPUT_ON_ERROR", 512n);
    defC("JSON_INVALID_UTF8_IGNORE", 1048576n);
    defC("JSON_INVALID_UTF8_SUBSTITUTE", 2097152n);
    defC("JSON_BIGINT_AS_STRING", 2n);
    defC("JSON_OBJECT_AS_ARRAY", 1n);
    defC("PREG_NO_ERROR", 0n);
    defC("PREG_INTERNAL_ERROR", 1n);
    defC("PREG_BACKTRACK_LIMIT_ERROR", 2n);
    defC("PREG_RECURSION_LIMIT_ERROR", 3n);
    defC("PREG_BAD_UTF8_ERROR", 4n);
    defC("PREG_GREP_INVERT", 1n);
    defC("DATE_ATOM", "Y-m-d\\TH:i:sP");
    defC("DATE_COOKIE", "l, d-M-Y H:i:s T");
    defC("DATE_ISO8601", "Y-m-d\\TH:i:sO");
    defC("DATE_ISO8601_EXPANDED", "X-m-d\\TH:i:sP");
    defC("DATE_RFC822", "D, d M y H:i:s O");
    defC("DATE_RFC850", "l, d-M-y H:i:s T");
    defC("DATE_RFC1036", "D, d M y H:i:s O");
    defC("DATE_RFC1123", "D, d M Y H:i:s O");
    defC("DATE_RFC2822", "D, d M Y H:i:s O");
    defC("DATE_RFC3339", "Y-m-d\\TH:i:sP");
    defC("DATE_RFC3339_EXTENDED", "Y-m-d\\TH:i:s.vP");
    defC("DATE_RFC7231", "D, d M Y H:i:s \\G\\M\\T");
    defC("DATE_RSS", "D, d M Y H:i:s O");
    defC("DATE_W3C", "Y-m-d\\TH:i:sP");
    defC("DEBUG_BACKTRACE_PROVIDE_OBJECT", 1n);
    defC("DEBUG_BACKTRACE_IGNORE_ARGS", 2n);
    defC("GLOB_MARK", 2n);
    defC("GLOB_NOSORT", 4n);
    defC("GLOB_NOCHECK", 16n);
    defC("GLOB_NOESCAPE", 64n);
    defC("GLOB_BRACE", 1024n);
    defC("GLOB_ONLYDIR", 8192n);
    defC("GLOB_ERR", 1n);
    defC("MB_CASE_UPPER", 0n);
    defC("MB_CASE_LOWER", 1n);
    defC("MB_CASE_TITLE", 2n);
    defC("MB_CASE_FOLD", 3n);
    defC("INPUT_POST", 0n);
    defC("INPUT_GET", 1n);
    defC("INPUT_COOKIE", 2n);
    defC("INPUT_ENV", 4n);
    defC("INPUT_SERVER", 5n);
    defC("FILTER_FLAG_NONE", 0n);
    defC("FILTER_REQUIRE_SCALAR", 33554432n);
    defC("FILTER_REQUIRE_ARRAY", 16777216n);
    defC("FILTER_FORCE_ARRAY", 67108864n);
    defC("FILTER_NULL_ON_FAILURE", 134217728n);
    defC("FILTER_VALIDATE_REGEXP", 272n);
    defC("FILTER_VALIDATE_DOMAIN", 277n);
    defC("FILTER_VALIDATE_MAC", 276n);
    defC("FILTER_DEFAULT", 516n);
    defC("FILTER_UNSAFE_RAW", 516n);
    defC("FILTER_SANITIZE_STRING", 513n);
    defC("FILTER_SANITIZE_STRIPPED", 513n);
    defC("FILTER_SANITIZE_ENCODED", 514n);
    defC("FILTER_SANITIZE_SPECIAL_CHARS", 515n);
    defC("FILTER_SANITIZE_FULL_SPECIAL_CHARS", 522n);
    defC("FILTER_SANITIZE_EMAIL", 517n);
    defC("FILTER_SANITIZE_URL", 518n);
    defC("FILTER_SANITIZE_NUMBER_INT", 519n);
    defC("FILTER_SANITIZE_NUMBER_FLOAT", 520n);
    defC("FILTER_SANITIZE_ADD_SLASHES", 523n);
    defC("FILTER_CALLBACK", 1024n);
    defC("FILTER_FLAG_ALLOW_OCTAL", 1n);
    defC("FILTER_FLAG_ALLOW_HEX", 2n);
    defC("FILTER_FLAG_STRIP_LOW", 4n);
    defC("FILTER_FLAG_STRIP_HIGH", 8n);
    defC("FILTER_FLAG_STRIP_BACKTICK", 512n);
    defC("FILTER_FLAG_ENCODE_LOW", 16n);
    defC("FILTER_FLAG_ENCODE_HIGH", 32n);
    defC("FILTER_FLAG_ENCODE_AMP", 64n);
    defC("FILTER_FLAG_NO_ENCODE_QUOTES", 128n);
    defC("FILTER_FLAG_EMPTY_STRING_NULL", 256n);
    defC("FILTER_FLAG_ALLOW_FRACTION", 4096n);
    defC("FILTER_FLAG_ALLOW_THOUSAND", 8192n);
    defC("FILTER_FLAG_ALLOW_SCIENTIFIC", 16384n);
    defC("FILTER_FLAG_PATH_REQUIRED", 262144n);
    defC("FILTER_FLAG_QUERY_REQUIRED", 524288n);
    defC("FILTER_FLAG_IPV4", 1048576n);
    defC("FILTER_FLAG_IPV6", 2097152n);
    defC("FILTER_FLAG_NO_RES_RANGE", 4194304n);
    defC("FILTER_FLAG_NO_PRIV_RANGE", 8388608n);
    defC("FILTER_FLAG_GLOBAL_RANGE", 268435456n);
    defC("FILTER_FLAG_HOSTNAME", 1048576n);
    defC("FILTER_FLAG_EMAIL_UNICODE", 1048576n);
    defC("PASSWORD_ARGON2I", "argon2i");
    defC("PASSWORD_ARGON2ID", "argon2id");
    defC("EXTR_OVERWRITE", 0n);
    defC("EXTR_SKIP", 1n);
    defC("EXTR_PREFIX_SAME", 2n);
    defC("EXTR_PREFIX_ALL", 3n);
    defC("LOCK_SH", 1n);
    defC("LOCK_UN", 3n);
    defC("LOCK_NB", 4n);
    defC("HTML_SPECIALCHARS", 0n);
    defC("HTML_ENTITIES", 1n);
    defC("LC_ALL", 0n);
    defC("LC_CTYPE", 2n);
    defC("LC_TIME", 5n);
    defC("LC_NUMERIC", 4n);
    defC("LC_COLLATE", 1n);
    defC("LC_MONETARY", 3n);
    defC("STR_SPLIT", 0n);
    defC("FNM_NOESCAPE", 2n);
    defC("FNM_PATHNAME", 1n);
    defC("FNM_PERIOD", 4n);
    defC("FNM_CASEFOLD", 16n);
    defC("STREAM_FILTER_READ", 1n);
    defC("STREAM_FILTER_WRITE", 2n);
    defC("STREAM_FILTER_ALL", 3n);

    // ---- Additional SPL / Core Exceptions, Interfaces & DateTime (lazily materialized) ----
    function defExc(name, parent) {
        if (!rt.classes.has(name.toLowerCase())) {
            R.defClass({ name: name, kind: "class", extends: parent });
        }
    }
    rt.registerLazyClass([
        "badfunctioncallexception", "badmethodcallexception", "lengthexception",
        "overflowexception", "rangeexception", "underflowexception",
        "reflectionexception", "compileerror", "serializable",
        "sessionhandlerinterface", "sessionidinterface", "sessionupdatetimestamphandlerinterface", "sessionhandler"
    ], function () {
        defExc("BadFunctionCallException", "LogicException");
        defExc("BadMethodCallException", "BadFunctionCallException");
        defExc("LengthException", "LogicException");
        defExc("OverflowException", "RuntimeException");
        defExc("RangeException", "RuntimeException");
        defExc("UnderflowException", "RuntimeException");
        defExc("ReflectionException", "Exception");
        defExc("CompileError", "Error");
        R.defInterface({ name: "Serializable", kind: "interface" });
        R.defInterface({ name: "SessionHandlerInterface", kind: "interface" });
        R.defInterface({ name: "SessionIdInterface", kind: "interface" });
        R.defInterface({ name: "SessionUpdateTimestampHandlerInterface", kind: "interface" });
        R.defClass({
            name: "SessionHandler",
            kind: "class",
            implements: ["SessionHandlerInterface", "SessionIdInterface"],
            methods: [
                { name: "open", vis: 0, builtin: function () { return true; } },
                { name: "close", vis: 0, builtin: function () { return true; } },
                { name: "read", vis: 0, builtin: function () { return ""; } },
                { name: "write", vis: 0, builtin: function () { return true; } },
                { name: "destroy", vis: 0, builtin: function () { return true; } },
                { name: "gc", vis: 0, builtin: function () { return 0n; } },
                { name: "create_sid", vis: 0, builtin: function () { return rt.funcs.get("bin2hex").builtin(rt.funcs.get("random_bytes").builtin(16n)); } },
            ],
        });
    });

    rt.registerLazyClass([
        "datetime", "datetimeimmutable", "datetimeinterface", "datetimezone",
        "dateinterval", "dateexception", "datemalformedstringexception"
    ], function () {
        defExc("DateException", "Exception");
        defExc("DateMalformedStringException", "DateException");
        R.defInterface({ name: "DateTimeInterface", kind: "interface" });
        if (typeof rt._makeDateTimeClass === "function") {
            rt._makeDateTimeClass("DateTime", false);
            rt._makeDateTimeClass("DateTimeImmutable", true);
        }
        // Attach DateTimeInterface to DateTime & DateTimeImmutable + extra methods
        for (const dtn of ["datetime", "datetimeimmutable"]) {
        const c = rt.classes.get(dtn);
        if (c) {
            c.allIfaces.add("datetimeinterface");
            c.consts.set("ATOM", { vis: 0, owner: c, val: "Y-m-d\\TH:i:sP" });
            c.consts.set("RFC3339", { vis: 0, owner: c, val: "Y-m-d\\TH:i:sP" });
            c.consts.set("RFC7231", { vis: 0, owner: c, val: "D, d M Y H:i:s \\G\\M\\T" });
            c.consts.set("ISO8601", { vis: 0, owner: c, val: "Y-m-d\\TH:i:sO" });
            c.consts.set("W3C", { vis: 0, owner: c, val: "Y-m-d\\TH:i:sP" });
            if (!c.methods.has("settimestamp")) {
                c.methods.set("settimestamp", {
                    name: "setTimestamp", vis: 0, owner: c,
                    builtin: function (ts) {
                        const t = Number(rt.toInt(ts));
                        if (dtn === "datetimeimmutable") {
                            const copy = rt.allocInstance(this.cls);
                            copy.__ts = t;
                            return copy;
                        }
                        this.__ts = t;
                        return this;
                    }
                });
            }
            if (!c.methods.has("settimezone")) {
                c.methods.set("settimezone", {
                    name: "setTimezone", vis: 0, owner: c,
                    builtin: function (tz) {
                        if (dtn === "datetimeimmutable") {
                            const copy = rt.allocInstance(this.cls);
                            copy.__ts = this.__ts;
                            copy.__tz = tz;
                            return copy;
                        }
                        this.__tz = tz;
                        return this;
                    }
                });
            }
            if (!c.methods.has("gettimezone")) {
                c.methods.set("gettimezone", {
                    name: "getTimezone", vis: 0, owner: c,
                    builtin: function () {
                        return this.__tz || R.newObj("DateTimeZone", ["UTC"], null, null, null, false);
                    }
                });
            }
            if (!c.methods.has("getoffset")) {
                c.methods.set("getoffset", { name: "getOffset", vis: 0, owner: c, builtin: function () { return 0n; } });
            }
            if (!c.methods.has("createfromformat")) {
                c.methods.set("createfromformat", {
                    name: "createFromFormat", vis: 0, isStatic: true, owner: c,
                    builtin: function (fmt, timeStr) {
                        const obj = rt.allocInstance(c);
                        const s = rt.toStr(timeStr);
                        const parsed = Date.parse(s);
                        obj.__ts = Number.isNaN(parsed) ? Math.floor(Date.now() / 1000) : Math.floor(parsed / 1000);
                        return obj;
                    }
                });
            }
            if (!c.methods.has("createfrominterface")) {
                c.methods.set("createfrominterface", {
                    name: "createFromInterface", vis: 0, isStatic: true, owner: c,
                    builtin: function (other) {
                        const obj = rt.allocInstance(c);
                        obj.__ts = (other && other.__ts !== undefined) ? other.__ts : Math.floor(Date.now() / 1000);
                        return obj;
                    }
                });
            }
        }
    }

    R.defClass({
        name: "DateTimeZone", kind: "class",
        methods: [
            { name: "__construct", vis: 0, builtin: function (tz) { this.__name = rt.toStr(tz || "UTC"); } },
            { name: "getName", vis: 0, builtin: function () { return this.__name || "UTC"; } },
            { name: "getOffset", vis: 0, builtin: function () { return 0n; } },
        ]
    });
    R.defClass({
        name: "DateInterval", kind: "class",
        props: [
            { name: "y", vis: 0, hasDefault: true, defaultVal: 0n },
            { name: "m", vis: 0, hasDefault: true, defaultVal: 0n },
            { name: "d", vis: 0, hasDefault: true, defaultVal: 0n },
            { name: "h", vis: 0, hasDefault: true, defaultVal: 0n },
            { name: "i", vis: 0, hasDefault: true, defaultVal: 0n },
            { name: "s", vis: 0, hasDefault: true, defaultVal: 0n },
            { name: "invert", vis: 0, hasDefault: true, defaultVal: 0n },
            { name: "days", vis: 0, hasDefault: true, defaultVal: false },
        ],
        methods: [
            {
                name: "__construct", vis: 0,
                builtin: function (spec) {
                    const s = rt.toStr(spec);
                    const m = /^P(?:(\d+)Y)?(?:(\d+)M)?(?:(\d+)W)?(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?(?:(\d+)S)?)?$/.exec(s);
                    if (m) {
                        this.props["y"] = BigInt(m[1] || 0);
                        this.props["m"] = BigInt(m[2] || 0);
                        this.props["d"] = BigInt((Number(m[3] || 0) * 7) + Number(m[4] || 0));
                        this.props["h"] = BigInt(m[5] || 0);
                        this.props["i"] = BigInt(m[6] || 0);
                        this.props["s"] = BigInt(m[7] || 0);
                    }
                }
            },
            {
                name: "createFromDateString", vis: 0, isStatic: true,
                builtin: function (str) {
                    const obj = rt.allocInstance(rt.classes.get("dateinterval"));
                    obj.__dateStr = rt.toStr(str);
                    return obj;
                }
            }
        ]
    });
    });

    // ---- Closure::bind and Closure::fromCallable ----
    const ClosureClass = rt.classes.get("closure");
    if (ClosureClass) {
        ClosureClass.methods.set("bind", {
            name: "bind", vis: 0, isStatic: true, owner: ClosureClass,
            builtin: function (closure, newThis, newScope) {
                closure = rt.deref(closure);
                if (!closure || closure.__php !== 3) rt.throwError("TypeError", "Closure::bind(): Argument #1 ($closure) must be of type Closure");
                newThis = rt.deref(newThis);
                let scopeCls = closure.scopeCls;
                if (newScope !== undefined) {
                    const ns = rt.deref(newScope);
                    if (ns === null) scopeCls = null;
                    else if (typeof ns === "string" && ns.toLowerCase() === "static") scopeCls = closure.scopeCls;
                    else scopeCls = rt.resolveClass(ns, null, null, true);
                } else if (newThis && newThis.__php === 2) {
                    scopeCls = newThis.cls;
                }
                return Object.assign({}, closure, { boundThis: newThis || null, scopeCls: scopeCls });
            }
        });
        ClosureClass.methods.set("fromcallable", {
            name: "fromCallable", vis: 0, isStatic: true, owner: ClosureClass,
            builtin: function (cb) {
                cb = rt.deref(cb);
                if (cb && cb.__php === 3) return cb;
                if (!rt.isCallable(cb)) rt.throwError("TypeError", "Failed to create closure from callable");
                return {
                    __php: 3,
                    name: "{closure}",
                    params: [],
                    builtin: function () {
                        return rt.call(cb, Array.from(arguments), null, null, false);
                    }
                };
            }
        });
    }

    // ---- Full Reflection API (lazily materialized on first use!) ----
    rt.registerLazyClass([
        "reflector", "reflectionattribute", "reflectionmethod", "reflectionclass",
        "reflectionobject", "reflectiontype", "reflectionnamedtype",
        "reflectionuniontype", "reflectionintersectiontype", "reflectionparameter",
        "reflectionfunctionabstract", "reflectionfunction", "reflectionproperty"
    ], function () {
    R.defInterface({ name: "Reflector", kind: "interface", extends: ["Stringable"] });
    if (typeof rt._initBasicReflection === "function") rt._initBasicReflection();
    const BUILTIN_TYPES = new Set([
        "int", "float", "string", "bool", "array", "object", "callable",
        "iterable", "mixed", "void", "never", "null", "true", "false", "self", "parent", "static"
    ]);

    const RefTypeClass = R.defClass({ name: "ReflectionType", kind: "class", isAbstract: true });

    const RefNamedTypeClass = R.defClass({
        name: "ReflectionNamedType", kind: "class", extends: "ReflectionType",
        methods: [
            { name: "getName", vis: 0, builtin: function () { return this.__name; } },
            { name: "isBuiltin", vis: 0, builtin: function () { return BUILTIN_TYPES.has(this.__name.toLowerCase()); } },
            { name: "allowsNull", vis: 0, builtin: function () { return !!this.__nullable; } },
            { name: "__toString", vis: 0, builtin: function () { return (this.__nullable && this.__name !== "mixed" && this.__name !== "null" ? "?" : "") + this.__name; } },
        ]
    });

    const RefUnionTypeClass = R.defClass({
        name: "ReflectionUnionType", kind: "class", extends: "ReflectionType",
        methods: [
            {
                name: "getTypes", vis: 0,
                builtin: function () {
                    const out = rt.makeArr();
                    for (const t of this.__types) rt.arrPushInPlace(out, t);
                    return out;
                }
            },
            { name: "allowsNull", vis: 0, builtin: function () { return !!this.__nullable; } },
            { name: "__toString", vis: 0, builtin: function () { return this.__raw; } },
        ]
    });

    const RefIntersectionTypeClass = R.defClass({
        name: "ReflectionIntersectionType", kind: "class", extends: "ReflectionType",
        methods: [
            {
                name: "getTypes", vis: 0,
                builtin: function () {
                    const out = rt.makeArr();
                    for (const t of this.__types) rt.arrPushInPlace(out, t);
                    return out;
                }
            },
            { name: "allowsNull", vis: 0, builtin: function () { return false; } },
            { name: "__toString", vis: 0, builtin: function () { return this.__raw; } },
        ]
    });

    function makeRefNamedType(name, nullable) {
        const o = rt.allocInstance(RefNamedTypeClass);
        const clean = name.startsWith("\\") ? name.slice(1) : name;
        o.__name = clean;
        o.__nullable = !!nullable || clean.toLowerCase() === "mixed" || clean.toLowerCase() === "null";
        return o;
    }

    function parseRefType(tyStr, allowsNullFromDefault) {
        if (!tyStr) return null;
        const s = String(tyStr).trim();
        if (!s) return null;
        if (s.startsWith("?")) {
            return makeRefNamedType(s.slice(1), true);
        }
        if (s.includes("|")) {
            const parts = s.split("|").map(p => p.trim()).filter(Boolean);
            const hasNull = parts.some(p => p.toLowerCase() === "null");
            const nonNull = parts.filter(p => p.toLowerCase() !== "null");
            if (parts.length === 2 && hasNull && !nonNull[0].includes("&")) {
                return makeRefNamedType(nonNull[0], true);
            }
            const o = rt.allocInstance(RefUnionTypeClass);
            o.__raw = s;
            o.__nullable = hasNull;
            o.__types = parts.map(p => {
                if (p.startsWith("(") && p.endsWith(")")) p = p.slice(1, -1);
                if (p.includes("&")) return parseRefType(p, false);
                return makeRefNamedType(p, p.toLowerCase() === "null");
            });
            return o;
        }
        if (s.includes("&")) {
            const inner = (s.startsWith("(") && s.endsWith(")")) ? s.slice(1, -1) : s;
            const parts = inner.split("&").map(p => p.trim()).filter(Boolean);
            const o = rt.allocInstance(RefIntersectionTypeClass);
            o.__raw = s;
            o.__types = parts.map(p => makeRefNamedType(p, false));
            return o;
        }
        return makeRefNamedType(s, !!allowsNullFromDefault);
    }

    function wrapRefAttrs(list, filterName, flags) {
        const RefAttrClass = rt.classes.get("reflectionattribute");
        const out = rt.makeArr();
        const isInst = flags !== undefined && (Number(rt.toInt(flags)) & 2) !== 0;
        const fStr = (filterName !== undefined && rt.deref(filterName) !== null) ? rt.toStr(filterName) : null;
        for (const a of (list || [])) {
            if (fStr !== null) {
                if (isInst) {
                    const ac = rt.resolveClass(a.name, null, null, true);
                    const fc = rt.resolveClass(fStr, null, null, true);
                    if (!ac || !fc || !rt.isSubtype(ac, fc)) continue;
                } else if (rt.canon(a.name) !== rt.canon(fStr)) {
                    continue;
                }
            }
            const o = rt.allocInstance(RefAttrClass);
            o.__meta = a;
            rt.arrPushInPlace(out, o);
        }
        return out;
    }

    // Add IS_INSTANCEOF constant to ReflectionAttribute
    const RefAttrCls = rt.classes.get("reflectionattribute");
    if (RefAttrCls) {
        RefAttrCls.consts.set("IS_INSTANCEOF", { vis: 0, owner: RefAttrCls, val: 2n });
        RefAttrCls.allIfaces.add("reflector");
    }

    const RefParamClass = R.defClass({
        name: "ReflectionParameter", kind: "class", implements: ["Reflector"],
        methods: [
            { name: "getName", vis: 0, builtin: function () { return this.__param.name; } },
            { name: "getPosition", vis: 0, builtin: function () { return BigInt(this.__pos); } },
            { name: "hasType", vis: 0, builtin: function () { return !!this.__param.ty; } },
            {
                name: "getType", vis: 0,
                builtin: function () {
                    const p = this.__param;
                    if (!p.ty) return null;
                    let defNull = false;
                    if (p.hasDefault) {
                        const dv = typeof p.defaultFn === "function" ? p.defaultFn() : p.defaultVal;
                        if (rt.deref(dv) === null) defNull = true;
                    }
                    return parseRefType(p.ty, defNull);
                }
            },
            {
                name: "allowsNull", vis: 0,
                builtin: function () {
                    const p = this.__param;
                    if (!p.ty) return true;
                    const t = rt.invokeMethod(this, rt.findMethod(this.cls, "gettype"), [], null, this.cls, this.cls, false);
                    return t ? !!t.__nullable : true;
                }
            },
            {
                name: "isOptional", vis: 0,
                builtin: function () {
                    return !!(this.__param.hasDefault || this.__param.variadic || this.__trailingOptional);
                }
            },
            { name: "isDefaultValueAvailable", vis: 0, builtin: function () { return !!this.__param.hasDefault; } },
            {
                name: "getDefaultValue", vis: 0,
                builtin: function () {
                    const p = this.__param;
                    if (!p.hasDefault) rt.throwError("ReflectionException", "Parameter $" + p.name + " has no default value");
                    return typeof p.defaultFn === "function" ? p.defaultFn.call(null, [], null, this.__ownerCls, null) : (p.defaultVal === undefined ? null : p.defaultVal);
                }
            },
            { name: "isDefaultValueConstant", vis: 0, builtin: function () { return false; } },
            { name: "getDefaultValueConstantName", vis: 0, builtin: function () { return null; } },
            { name: "isPassedByReference", vis: 0, builtin: function () { return !!this.__param.byRef; } },
            { name: "canBePassedByValue", vis: 0, builtin: function () { return !this.__param.byRef; } },
            { name: "isVariadic", vis: 0, builtin: function () { return !!this.__param.variadic; } },
            { name: "isPromoted", vis: 0, builtin: function () { return !!this.__param.promoted; } },
            {
                name: "getDeclaringClass", vis: 0,
                builtin: function () {
                    if (!this.__ownerCls) return null;
                    const o = rt.allocInstance(rt.classes.get("reflectionclass"));
                    o.__meta = this.__ownerCls;
                    return o;
                }
            },
            {
                name: "getDeclaringFunction", vis: 0,
                builtin: function () {
                    return this.__declFn || null;
                }
            },
            {
                name: "getClass", vis: 0,
                builtin: function () {
                    const p = this.__param;
                    if (!p.ty) return null;
                    const clean = p.ty.replace(/^\?/, "");
                    if (clean.includes("|") || clean.includes("&") || BUILTIN_TYPES.has(clean.toLowerCase())) return null;
                    const c = rt.resolveClass(clean, this.__ownerCls, this.__ownerCls, true);
                    if (!c) return null;
                    const o = rt.allocInstance(rt.classes.get("reflectionclass"));
                    o.__meta = c;
                    return o;
                }
            },
            { name: "getAttributes", vis: 0, builtin: function (n, f) { return wrapRefAttrs(this.__param.attrs, n, f); } },
            { name: "__toString", vis: 0, builtin: function () { return "Parameter #" + this.__pos + " [ $" + this.__param.name + " ]"; } },
        ]
    });

    function buildRefParams(fnMeta, declFnObj, ownerCls) {
        const params = fnMeta.params || [];
        const out = rt.makeArr();
        // A parameter is optional if it and all following non-variadic params have defaults
        let allOptFromRight = true;
        const optFlags = new Array(params.length);
        for (let i = params.length - 1; i >= 0; i--) {
            if (!params[i].hasDefault && !params[i].variadic) allOptFromRight = false;
            optFlags[i] = allOptFromRight;
        }
        for (let i = 0; i < params.length; i++) {
            const o = rt.allocInstance(RefParamClass);
            o.__param = params[i];
            o.__pos = i;
            o.__ownerCls = ownerCls || fnMeta.owner || null;
            o.__declFn = declFnObj;
            o.__trailingOptional = optFlags[i];
            rt.arrPushInPlace(out, o);
        }
        return out;
    }

    function countRequiredParams(fnMeta) {
        const params = fnMeta.params || [];
        let req = 0;
        for (let i = 0; i < params.length; i++) {
            if (!params[i].hasDefault && !params[i].variadic) req = i + 1;
        }
        return BigInt(req);
    }

    const RefFnAbstract = R.defClass({
        name: "ReflectionFunctionAbstract", kind: "class", isAbstract: true, implements: ["Reflector"],
        methods: [
            { name: "getName", vis: 0, builtin: function () { return this.__meta.name || "{closure}"; } },
            {
                name: "getShortName", vis: 0,
                builtin: function () {
                    const n = this.__meta.name || "{closure}";
                    const i = n.lastIndexOf("\\");
                    return i >= 0 ? n.slice(i + 1) : n;
                }
            },
            {
                name: "getNamespaceName", vis: 0,
                builtin: function () {
                    const n = this.__meta.name || "";
                    const i = n.lastIndexOf("\\");
                    return i >= 0 ? n.slice(0, i) : "";
                }
            },
            { name: "isClosure", vis: 0, builtin: function () { return this.__meta.name === "{closure}"; } },
            { name: "isInternal", vis: 0, builtin: function () { return !!this.__meta.builtin; } },
            { name: "isUserDefined", vis: 0, builtin: function () { return !this.__meta.builtin; } },
            { name: "isGenerator", vis: 0, builtin: function () { return !!this.__meta.isGen; } },
            { name: "isVariadic", vis: 0, builtin: function () { const ps = this.__meta.params || []; return ps.length > 0 && !!ps[ps.length - 1].variadic; } },
            { name: "returnsReference", vis: 0, builtin: function () { return !!this.__meta.byRef; } },
            { name: "getParameters", vis: 0, builtin: function () { return buildRefParams(this.__meta, this, this.__meta.owner || null); } },
            { name: "getNumberOfParameters", vis: 0, builtin: function () { return BigInt((this.__meta.params || []).length); } },
            { name: "getNumberOfRequiredParameters", vis: 0, builtin: function () { return countRequiredParams(this.__meta); } },
            { name: "hasReturnType", vis: 0, builtin: function () { return !!this.__meta.retType; } },
            { name: "getReturnType", vis: 0, builtin: function () { return parseRefType(this.__meta.retType, false); } },
            { name: "getClosureThis", vis: 0, builtin: function () { return this.__meta.boundThis || null; } },
            {
                name: "getClosureScopeClass", vis: 0,
                builtin: function () {
                    if (!this.__meta.scopeCls) return null;
                    const o = rt.allocInstance(rt.classes.get("reflectionclass"));
                    o.__meta = this.__meta.scopeCls;
                    return o;
                }
            },
            { name: "getAttributes", vis: 0, builtin: function (n, f) { return wrapRefAttrs(this.__meta.attrs, n, f); } },
            { name: "getFileName", vis: 0, builtin: function () { return this.__meta.file || false; } },
            { name: "getStartLine", vis: 0, builtin: function () { return BigInt(this.__meta.startLine || 1); } },
            { name: "getEndLine", vis: 0, builtin: function () { return BigInt(this.__meta.endLine || 1); } },
            { name: "getDocComment", vis: 0, builtin: function () { return false; } },
            { name: "__toString", vis: 0, builtin: function () { return "Function [ " + (this.__meta.name || "{closure}") + " ]"; } },
        ]
    });

    const RefFunctionClass = R.defClass({
        name: "ReflectionFunction", kind: "class", extends: "ReflectionFunctionAbstract",
        methods: [
            {
                name: "__construct", vis: 0,
                builtin: function (fn) {
                    fn = rt.deref(fn);
                    if (fn && fn.__php === 3) { this.__meta = fn; return; }
                    if (typeof fn === "string") {
                        const f = rt.funcs.get(rt.canon(fn));
                        if (!f) rt.throwError("ReflectionException", "Function " + fn + "() does not exist");
                        this.__meta = f;
                        return;
                    }
                    rt.throwError("ReflectionException", "Invalid function provided to ReflectionFunction");
                }
            },
            {
                name: "invoke", vis: 0,
                builtin: function () {
                    return rt.call(this.__meta, Array.from(arguments), null, null, false);
                }
            },
            {
                name: "invokeArgs", vis: 0,
                builtin: function (argsArr) {
                    const list = Array.from(rt.toArray(argsArr).m.values()).map(rt.deref);
                    return rt.call(this.__meta, list, null, null, false);
                }
            },
            { name: "getClosure", vis: 0, builtin: function () { return this.__meta; } },
        ]
    });

    // Upgrade existing ReflectionMethod to extend ReflectionFunctionAbstract with full methods & constants!
    const RefMethodCls = rt.classes.get("reflectionmethod");
    if (RefMethodCls) {
        RefMethodCls.parent = RefFnAbstract;
        for (const [k, v] of RefFnAbstract.methods) {
            if (!RefMethodCls.methods.has(k)) RefMethodCls.methods.set(k, v);
        }
        RefMethodCls.allIfaces.add("reflector");
        RefMethodCls.consts.set("IS_STATIC", { vis: 0, owner: RefMethodCls, val: 16n });
        RefMethodCls.consts.set("IS_PUBLIC", { vis: 0, owner: RefMethodCls, val: 1n });
        RefMethodCls.consts.set("IS_PROTECTED", { vis: 0, owner: RefMethodCls, val: 2n });
        RefMethodCls.consts.set("IS_PRIVATE", { vis: 0, owner: RefMethodCls, val: 4n });
        RefMethodCls.consts.set("IS_ABSTRACT", { vis: 0, owner: RefMethodCls, val: 64n });
        RefMethodCls.consts.set("IS_FINAL", { vis: 0, owner: RefMethodCls, val: 32n });

        const addM = (name, fn, isStatic) => RefMethodCls.methods.set(name.toLowerCase(), { name: name, vis: 0, isStatic: !!isStatic, owner: RefMethodCls, builtin: fn });
        addM("__construct", function (clsOrStr, mname) {
            let c, mn;
            if (mname === undefined || rt.deref(mname) === null) {
                const s = rt.toStr(clsOrStr);
                const idx = s.indexOf("::");
                if (idx < 0) rt.throwError("ReflectionException", "Invalid method name " + s);
                c = rt.requireClass(s.slice(0, idx), null, null);
                mn = s.slice(idx + 2);
            } else {
                c = rt.requireClass(clsOrStr, null, null);
                mn = rt.toStr(mname);
            }
            const m = c.methods.get(mn.toLowerCase());
            if (!m) rt.throwError("ReflectionException", "Method " + c.name + "::" + mn + "() does not exist");
            this.__meta = m;
            this.__cls = c;
        });
        addM("isAbstract", function () { return !!this.__meta.isAbstract; });
        addM("isFinal", function () { return !!this.__meta.isFinal; });
        addM("isConstructor", function () { return this.__meta.name.toLowerCase() === "__construct"; });
        addM("isDestructor", function () { return this.__meta.name.toLowerCase() === "__destruct"; });
        addM("setAccessible", function () { return null; });
        addM("getModifiers", function () {
            let mod = this.__meta.vis === 2 ? 4 : (this.__meta.vis === 1 ? 2 : 1);
            if (this.__meta.isStatic) mod |= 16;
            if (this.__meta.isFinal) mod |= 32;
            if (this.__meta.isAbstract) mod |= 64;
            return BigInt(mod);
        });
        addM("getDeclaringClass", function () {
            const o = rt.allocInstance(rt.classes.get("reflectionclass"));
            o.__meta = this.__meta.owner || this.__cls;
            return o;
        });
        addM("invokeArgs", function (obj, argsArr) {
            const list = Array.from(rt.toArray(argsArr).m.values()).map(rt.deref);
            const target = rt.deref(obj);
            return rt.invokeMethod(target, this.__meta, list, null, this.__meta.owner, target ? target.cls : this.__meta.owner, false);
        });
        addM("getClosure", function (obj) {
            const target = rt.deref(obj);
            const m = this.__meta;
            return {
                __php: 3, name: "{closure}", params: m.params || [],
                builtin: function () {
                    return rt.invokeMethod(target, m, Array.from(arguments), null, m.owner, target ? target.cls : m.owner, false);
                }
            };
        });
        addM("getAttributes", function (n, f) { return wrapRefAttrs(this.__meta.attrs, n, f); });
    }

    const RefPropClass = R.defClass({
        name: "ReflectionProperty", kind: "class", implements: ["Reflector"],
        consts: [
            { name: "IS_STATIC", vis: 0, val: 16n },
            { name: "IS_PUBLIC", vis: 0, val: 1n },
            { name: "IS_PROTECTED", vis: 0, val: 2n },
            { name: "IS_PRIVATE", vis: 0, val: 4n },
            { name: "IS_READONLY", vis: 0, val: 128n },
        ],
        methods: [
            {
                name: "__construct", vis: 0,
                builtin: function (cls, pname) {
                    const c = rt.requireClass(cls, null, null);
                    const pn = rt.toStr(pname);
                    const p = c.props.get(pn) || c.props.get("\0" + c.name + "\0" + pn) || c.staticProps.get(pn);
                    if (!p) rt.throwError("ReflectionException", "Property " + c.name + "::$" + pn + " does not exist");
                    this.__prop = p;
                    this.__name = pn;
                    this.__isStatic = c.staticProps.has(pn) && !c.props.has(pn) && !c.props.has("\0" + c.name + "\0" + pn);
                    this.__cls = c;
                }
            },
            { name: "getName", vis: 0, builtin: function () { return this.__name; } },
            { name: "isPublic", vis: 0, builtin: function () { return (this.__prop.vis || 0) === 0; } },
            { name: "isProtected", vis: 0, builtin: function () { return this.__prop.vis === 1; } },
            { name: "isPrivate", vis: 0, builtin: function () { return this.__prop.vis === 2; } },
            { name: "isStatic", vis: 0, builtin: function () { return !!this.__isStatic; } },
            { name: "isReadOnly", vis: 0, builtin: function () { return !!this.__prop.readonly; } },
            { name: "isDefault", vis: 0, builtin: function () { return true; } },
            { name: "isPromoted", vis: 0, builtin: function () { return !!this.__prop.promoted; } },
            { name: "hasType", vis: 0, builtin: function () { return !!this.__prop.ty; } },
            { name: "getType", vis: 0, builtin: function () { return parseRefType(this.__prop.ty, false); } },
            { name: "hasDefaultValue", vis: 0, builtin: function () { return !!this.__prop.hasDefault || !this.__prop.ty; } },
            { name: "getDefaultValue", vis: 0, builtin: function () { return this.__prop.hasDefault ? this.__prop.defaultVal : null; } },
            { name: "setAccessible", vis: 0, builtin: function () { return null; } },
            {
                name: "isInitialized", vis: 0,
                builtin: function (obj) {
                    if (this.__isStatic) return true;
                    obj = rt.deref(obj);
                    return !!(obj && obj.__php === 2 && (obj.props[this.__prop.key] !== undefined || !this.__prop.ty));
                }
            },
            {
                name: "getValue", vis: 0,
                builtin: function (obj) {
                    if (this.__isStatic) return this.__prop.ref.v;
                    obj = rt.deref(obj);
                    if (!obj || obj.__php !== 2) return null;
                    const v = obj.props[this.__prop.key];
                    return v !== undefined ? rt.deref(v) : null;
                }
            },
            {
                name: "setValue", vis: 0,
                builtin: function (objOrVal, val) {
                    if (this.__isStatic) {
                        this.__prop.ref.v = rt.share(rt.deref(arguments.length >= 2 ? val : objOrVal));
                        return null;
                    }
                    const obj = rt.deref(objOrVal);
                    if (obj && obj.__php === 2) obj.props[this.__prop.key] = rt.share(rt.deref(val));
                    return null;
                }
            },
            {
                name: "getDeclaringClass", vis: 0,
                builtin: function () {
                    const o = rt.allocInstance(rt.classes.get("reflectionclass"));
                    o.__meta = this.__prop.owner || this.__cls;
                    return o;
                }
            },
            { name: "getAttributes", vis: 0, builtin: function (n, f) { return wrapRefAttrs(this.__prop.attrs, n, f); } },
            { name: "getModifiers", vis: 0, builtin: function () {
                let m = this.__prop.vis === 2 ? 4 : (this.__prop.vis === 1 ? 2 : 1);
                if (this.__isStatic) m |= 16;
                if (this.__prop.readonly) m |= 128;
                return BigInt(m);
            }},
        ]
    });

    // Upgrade ReflectionClass with the full Laravel/Symfony-required method set!
    const RefCls = rt.classes.get("reflectionclass");
    if (RefCls) {
        RefCls.allIfaces.add("reflector");
        RefCls.consts.set("IS_IMPLICIT_ABSTRACT", { vis: 0, owner: RefCls, val: 16n });
        RefCls.consts.set("IS_EXPLICIT_ABSTRACT", { vis: 0, owner: RefCls, val: 64n });
        RefCls.consts.set("IS_FINAL", { vis: 0, owner: RefCls, val: 32n });
        RefCls.consts.set("IS_READONLY", { vis: 0, owner: RefCls, val: 65536n });

        const addC = (name, fn) => RefCls.methods.set(name.toLowerCase(), { name: name, vis: 0, owner: RefCls, builtin: fn });
        addC("__construct", function (target) {
            const c = rt.resolveClass(target, null, null, true);
            if (!c) rt.throwError("ReflectionException", 'Class "' + rt.toStr(target) + '" does not exist');
            this.__meta = c;
        });
        addC("isInstantiable", function () {
            const m = this.__meta;
            if (m.isAbstract || m.kind !== "class") return false;
            const ctor = m.methods.get("__construct");
            if (ctor && ctor.vis !== 0) return false;
            return true;
        });
        addC("isAbstract", function () { return !!this.__meta.isAbstract || this.__meta.kind === "interface"; });
        addC("isFinal", function () { return !!this.__meta.isFinal; });
        addC("isReadOnly", function () { return !!this.__meta.isReadonly; });
        addC("isInterface", function () { return this.__meta.kind === "interface"; });
        addC("isTrait", function () { return this.__meta.kind === "trait"; });
        addC("isEnum", function () { return this.__meta.kind === "enum"; });
        addC("isInternal", function () { return !this.__meta.file; });
        addC("isUserDefined", function () { return !!this.__meta.file; });
        addC("isCloneable", function () { return this.__meta.kind === "class" && !this.__meta.isAbstract; });
        addC("isIterable", function () { return rt.isSubtype(this.__meta, rt.classes.get("traversable")); });
        addC("isSubclassOf", function (cls) {
            const t = rt.resolveClass(cls, null, null, true);
            return !!(t && this.__meta !== t && rt.isSubtype(this.__meta, t));
        });
        addC("isInstance", function (obj) {
            obj = rt.deref(obj);
            if (!obj || (obj.__php !== 2 && obj.__php !== 3)) return false;
            const c = obj.__php === 3 ? ClosureClass : obj.cls;
            return rt.isSubtype(c, this.__meta);
        });
        addC("getFileName", function () { return this.__meta.file || false; });
        addC("getStartLine", function () { return 1n; });
        addC("getEndLine", function () { return 1n; });
        addC("getDocComment", function () { return false; });
        addC("getParentClass", function () {
            if (!this.__meta.parent) return false;
            const o = rt.allocInstance(RefCls);
            o.__meta = this.__meta.parent;
            return o;
        });
        addC("getInterfaceNames", function () {
            const out = rt.makeArr();
            for (const c of this.__meta.allIfaces) {
                const ic = rt.classes.get(c);
                if (ic) rt.arrPushInPlace(out, ic.name);
            }
            return out;
        });
        addC("getInterfaces", function () {
            const out = rt.makeArr();
            for (const c of this.__meta.allIfaces) {
                const ic = rt.classes.get(c);
                if (ic) {
                    const o = rt.allocInstance(RefCls);
                    o.__meta = ic;
                    rt.arrSetInPlace(out, ic.name, o);
                }
            }
            return out;
        });
        addC("getTraitNames", function () {
            const out = rt.makeArr();
            for (const t of this.__meta.traits) rt.arrPushInPlace(out, t.name);
            return out;
        });
        addC("getTraits", function () {
            const out = rt.makeArr();
            for (const t of this.__meta.traits) {
                const o = rt.allocInstance(RefCls);
                o.__meta = t;
                rt.arrSetInPlace(out, t.name, o);
            }
            return out;
        });
        addC("getConstructor", function () {
            const m = this.__meta.methods.get("__construct");
            if (!m) return null;
            const o = rt.allocInstance(RefMethodCls);
            o.__meta = m;
            o.__cls = this.__meta;
            return o;
        });
        addC("getMethods", function (filter) {
            const out = rt.makeArr();
            const f = (filter !== undefined && rt.deref(filter) !== null) ? Number(rt.toInt(filter)) : null;
            for (const m of this.__meta.methods.values()) {
                if (f !== null) {
                    let mod = m.vis === 2 ? 4 : (m.vis === 1 ? 2 : 1);
                    if (m.isStatic) mod |= 16;
                    if (m.isFinal) mod |= 32;
                    if (m.isAbstract) mod |= 64;
                    if ((mod & f) === 0) continue;
                }
                const o = rt.allocInstance(RefMethodCls);
                o.__meta = m;
                o.__cls = this.__meta;
                rt.arrPushInPlace(out, o);
            }
            return out;
        });
        addC("hasProperty", function (n) {
            const pn = rt.toStr(n);
            return this.__meta.props.has(pn) || this.__meta.props.has("\0" + this.__meta.name + "\0" + pn) || this.__meta.staticProps.has(pn);
        });
        addC("getProperty", function (n) {
            return R.newObj(RefPropClass, [this.__meta, n], null, null, null, false);
        });
        addC("getProperties", function (filter) {
            const out = rt.makeArr();
            const f = (filter !== undefined && rt.deref(filter) !== null) ? Number(rt.toInt(filter)) : null;
            for (const pm of this.__meta.props.values()) {
                if (f !== null) {
                    let mod = pm.vis === 2 ? 4 : (pm.vis === 1 ? 2 : 1);
                    if (pm.readonly) mod |= 128;
                    if ((mod & f) === 0) continue;
                }
                const o = rt.allocInstance(RefPropClass);
                o.__prop = pm; o.__name = pm.name; o.__isStatic = false; o.__cls = this.__meta;
                rt.arrPushInPlace(out, o);
            }
            for (const [spn, sp] of this.__meta.staticProps) {
                if (f !== null) {
                    let mod = (sp.vis === 2 ? 4 : (sp.vis === 1 ? 2 : 1)) | 16;
                    if ((mod & f) === 0) continue;
                }
                const o = rt.allocInstance(RefPropClass);
                o.__prop = sp; o.__name = spn; o.__isStatic = true; o.__cls = this.__meta;
                rt.arrPushInPlace(out, o);
            }
            return out;
        });
        addC("hasConstant", function (n) { return this.__meta.consts.has(rt.toStr(n)); });
        addC("getConstant", function (n) {
            const c = this.__meta.consts.get(rt.toStr(n));
            return c ? c.val : false;
        });
        addC("getConstants", function () {
            const out = rt.makeArr();
            for (const [k, v] of this.__meta.consts) rt.arrSetInPlace(out, k, v.val);
            return out;
        });
        addC("getAttributes", function (n, f) { return wrapRefAttrs(this.__meta.attrs, n, f); });
        addC("newInstance", function () {
            return R.newObj(this.__meta, Array.from(arguments), null, null, null, false);
        });
        addC("newInstanceWithoutConstructor", function () {
            return rt.allocInstance(this.__meta);
        });
        addC("getDefaultProperties", function () {
            const out = rt.makeArr();
            for (const pm of this.__meta.props.values()) {
                rt.arrSetInPlace(out, pm.name, pm.hasDefault ? pm.defaultVal : null);
            }
            for (const [spn, sp] of this.__meta.staticProps) {
                rt.arrSetInPlace(out, spn, sp.ref.v);
            }
            return out;
        });
        addC("getStaticPropertyValue", function (n, def) {
            const sp = this.__meta.staticProps.get(rt.toStr(n));
            if (sp) return sp.ref.v;
            if (arguments.length >= 2) return rt.deref(def);
            rt.throwError("ReflectionException", "Static property does not exist");
        });
        addC("setStaticPropertyValue", function (n, v) {
            const sp = this.__meta.staticProps.get(rt.toStr(n));
            if (!sp) rt.throwError("ReflectionException", "Static property does not exist");
            sp.ref.v = rt.share(rt.deref(v));
            return null;
        });
    }

    R.defClass({ name: "ReflectionObject", kind: "class", extends: "ReflectionClass" });
    });

    // ---- SPL Data Structures & Iterators (lazily materialized on first use!) ----
    rt.registerLazyClass([
        "outeriterator", "seekableiterator", "recursiveiterator",
        "arrayiterator", "arrayobject", "splobjectstorage", "weakmap",
        "emptyiterator", "iteratoriterator", "filteriterator", "callbackfilteriterator",
        "norewinditerator", "limititerator", "appenditerator", "cachingiterator",
        "recursivearrayiterator", "recursiveiteratoriterator", "splfileinfo"
    ], function () {
    R.defInterface({ name: "OuterIterator", kind: "interface", extends: ["Iterator"] });
    R.defInterface({ name: "SeekableIterator", kind: "interface", extends: ["Iterator"] });
    R.defInterface({ name: "RecursiveIterator", kind: "interface", extends: ["Iterator"] });
    const ArrayIteratorClass = R.defClass({
        name: "ArrayIterator", kind: "class", implements: ["SeekableIterator", "ArrayAccess", "Countable"],
        methods: [
            {
                name: "__construct", vis: 0,
                builtin: function (arr) {
                    this.__arr = arr ? rt.cloneArr(rt.toArray(arr)) : rt.makeArr();
                    this.__keys = Array.from(this.__arr.m.keys());
                    this.__idx = 0;
                }
            },
            { name: "rewind", vis: 0, builtin: function () { this.__keys = Array.from(this.__arr.m.keys()); this.__idx = 0; } },
            { name: "valid", vis: 0, builtin: function () { return this.__idx < this.__keys.length; } },
            { name: "current", vis: 0, builtin: function () { return this.__idx < this.__keys.length ? rt.deref(this.__arr.m.get(this.__keys[this.__idx])) : null; } },
            { name: "key", vis: 0, builtin: function () { return this.__idx < this.__keys.length ? this.__keys[this.__idx] : null; } },
            { name: "next", vis: 0, builtin: function () { this.__idx++; } },
            { name: "seek", vis: 0, builtin: function (pos) { this.__idx = Number(rt.toInt(pos)); } },
            { name: "count", vis: 0, builtin: function () { return BigInt(this.__arr.m.size); } },
            { name: "getArrayCopy", vis: 0, builtin: function () { return rt.cloneArr(this.__arr); } },
            { name: "offsetExists", vis: 0, builtin: function (k) { return R.idxIsset(this.__arr, k); } },
            { name: "offsetGet", vis: 0, builtin: function (k) { return R.idxGet(this.__arr, k, true); } },
            { name: "offsetSet", vis: 0, builtin: function (k, v) { if (k === null) rt.arrPushInPlace(this.__arr, v); else rt.arrSetInPlace(this.__arr, k, v); } },
            { name: "offsetUnset", vis: 0, builtin: function (k) { this.__arr._list = false; this.__arr._e = null; this.__arr.m.delete(rt.arrKey(k)); } },
        ]
    });

    R.defClass({
        name: "ArrayObject", kind: "class", implements: ["IteratorAggregate", "ArrayAccess", "Countable"],
        consts: [
            { name: "STD_PROP_LIST", vis: 0, val: 1n },
            { name: "ARRAY_AS_PROPS", vis: 0, val: 2n },
        ],
        methods: [
            {
                name: "__construct", vis: 0,
                builtin: function (input, flags) {
                    this.__arr = input ? rt.cloneArr(rt.toArray(input)) : rt.makeArr();
                    this.__flags = flags ? Number(rt.toInt(flags)) : 0;
                }
            },
            { name: "getIterator", vis: 0, builtin: function () { return R.newObj(ArrayIteratorClass, [this.__arr], null, null, null, false); } },
            { name: "count", vis: 0, builtin: function () { return BigInt(this.__arr.m.size); } },
            { name: "getArrayCopy", vis: 0, builtin: function () { return rt.cloneArr(this.__arr); } },
            { name: "exchangeArray", vis: 0, builtin: function (input) { const old = this.__arr; this.__arr = rt.cloneArr(rt.toArray(input)); return old; } },
            { name: "offsetExists", vis: 0, builtin: function (k) { return R.idxIsset(this.__arr, k); } },
            { name: "offsetGet", vis: 0, builtin: function (k) { return R.idxGet(this.__arr, k, true); } },
            { name: "offsetSet", vis: 0, builtin: function (k, v) { if (k === null) rt.arrPushInPlace(this.__arr, v); else rt.arrSetInPlace(this.__arr, k, v); } },
            { name: "offsetUnset", vis: 0, builtin: function (k) { this.__arr._list = false; this.__arr._e = null; this.__arr.m.delete(rt.arrKey(k)); } },
            { name: "append", vis: 0, builtin: function (v) { rt.arrPushInPlace(this.__arr, v); } },
        ]
    });

    R.defClass({
        name: "SplObjectStorage", kind: "class", implements: ["Countable", "Iterator", "ArrayAccess"],
        methods: [
            { name: "__construct", vis: 0, builtin: function () { this.__map = new Map(); this.__keys = []; this.__idx = 0; } },
            { name: "attach", vis: 0, builtin: function (o, info) { this.__map.set(rt.deref(o), info === undefined ? null : rt.deref(info)); } },
            { name: "detach", vis: 0, builtin: function (o) { this.__map.delete(rt.deref(o)); } },
            { name: "contains", vis: 0, builtin: function (o) { return this.__map.has(rt.deref(o)); } },
            { name: "count", vis: 0, builtin: function () { return BigInt(this.__map.size); } },
            { name: "offsetExists", vis: 0, builtin: function (o) { return this.__map.has(rt.deref(o)); } },
            { name: "offsetGet", vis: 0, builtin: function (o) { return this.__map.get(rt.deref(o)); } },
            { name: "offsetSet", vis: 0, builtin: function (o, v) { this.__map.set(rt.deref(o), rt.deref(v)); } },
            { name: "offsetUnset", vis: 0, builtin: function (o) { this.__map.delete(rt.deref(o)); } },
            { name: "rewind", vis: 0, builtin: function () { this.__keys = Array.from(this.__map.keys()); this.__idx = 0; } },
            { name: "valid", vis: 0, builtin: function () { return this.__idx < this.__keys.length; } },
            { name: "current", vis: 0, builtin: function () { return this.__keys[this.__idx] || null; } },
            { name: "getInfo", vis: 0, builtin: function () { return this.__map.get(this.__keys[this.__idx]); } },
            { name: "setInfo", vis: 0, builtin: function (v) { this.__map.set(this.__keys[this.__idx], rt.deref(v)); } },
            { name: "key", vis: 0, builtin: function () { return BigInt(this.__idx); } },
            { name: "next", vis: 0, builtin: function () { this.__idx++; } },
        ]
    });

    R.defClass({
        name: "WeakMap", kind: "class", implements: ["ArrayAccess", "Countable", "IteratorAggregate"],
        methods: [
            { name: "__construct", vis: 0, builtin: function () { this.__map = new Map(); } },
            { name: "offsetExists", vis: 0, builtin: function (k) { return this.__map.has(rt.deref(k)); } },
            { name: "offsetGet", vis: 0, builtin: function (k) {
                k = rt.deref(k);
                if (!this.__map.has(k)) rt.throwError("Error", "Object not found in WeakMap");
                return this.__map.get(k);
            }},
            { name: "offsetSet", vis: 0, builtin: function (k, v) { this.__map.set(rt.deref(k), rt.deref(v)); } },
            { name: "offsetUnset", vis: 0, builtin: function (k) { this.__map.delete(rt.deref(k)); } },
            { name: "count", vis: 0, builtin: function () { return BigInt(this.__map.size); } },
            { name: "getIterator", vis: 0, builtin: function () {
                const a = rt.makeArr();
                for (const [k, v] of this.__map) rt.arrPushInPlace(a, v);
                return R.newObj(ArrayIteratorClass, [a], null, null, null, false);
            }},
        ]
    });

    R.defClass({
        name: "EmptyIterator", kind: "class", implements: ["Iterator"],
        methods: [
            { name: "rewind", vis: 0, builtin: function () {} },
            { name: "valid", vis: 0, builtin: function () { return false; } },
            { name: "current", vis: 0, builtin: function () { return null; } },
            { name: "key", vis: 0, builtin: function () { return null; } },
            { name: "next", vis: 0, builtin: function () {} },
        ]
    });

    function unwrapTraversable(tr) {
        tr = rt.deref(tr);
        let guard = 0;
        while (tr && tr.__php === 2 && !tr.cls.allIfaces.has("iterator") && tr.cls.allIfaces.has("iteratoraggregate") && guard++ < 16) {
            tr = R.methodCall(tr, "getIterator", false, [], null, null, false);
        }
        return tr;
    }

    R.defClass({
        name: "IteratorIterator", kind: "class", implements: ["OuterIterator"],
        methods: [
            { name: "__construct", vis: 0, builtin: function (iterator) { this.__inner = unwrapTraversable(iterator); } },
            { name: "getInnerIterator", vis: 0, builtin: function () { return this.__inner || null; } },
            { name: "rewind", vis: 0, builtin: function () { if (this.__inner) R.methodCall(this.__inner, "rewind", false, [], null, null, false); } },
            { name: "valid", vis: 0, builtin: function () { return this.__inner ? rt.truthy(R.methodCall(this.__inner, "valid", false, [], null, null, false)) : false; } },
            { name: "current", vis: 0, builtin: function () { return this.__inner ? R.methodCall(this.__inner, "current", false, [], null, null, false) : null; } },
            { name: "key", vis: 0, builtin: function () { return this.__inner ? R.methodCall(this.__inner, "key", false, [], null, null, false) : null; } },
            { name: "next", vis: 0, builtin: function () { if (this.__inner) R.methodCall(this.__inner, "next", false, [], null, null, false); } },
        ]
    });

    R.defClass({
        name: "FilterIterator", kind: "class", isAbstract: true, extends: "IteratorIterator",
        methods: [
            {
                name: "rewind", vis: 0,
                builtin: function () {
                    if (!this.__inner) return;
                    R.methodCall(this.__inner, "rewind", false, [], null, null, false);
                    while (rt.truthy(R.methodCall(this.__inner, "valid", false, [], null, null, false))
                        && !rt.truthy(R.methodCall(this, "accept", false, [], null, null, false))) {
                        R.methodCall(this.__inner, "next", false, [], null, null, false);
                    }
                }
            },
            {
                name: "next", vis: 0,
                builtin: function () {
                    if (!this.__inner) return;
                    R.methodCall(this.__inner, "next", false, [], null, null, false);
                    while (rt.truthy(R.methodCall(this.__inner, "valid", false, [], null, null, false))
                        && !rt.truthy(R.methodCall(this, "accept", false, [], null, null, false))) {
                        R.methodCall(this.__inner, "next", false, [], null, null, false);
                    }
                }
            },
        ]
    });

    R.defClass({
        name: "CallbackFilterIterator", kind: "class", extends: "FilterIterator",
        methods: [
            {
                name: "__construct", vis: 0,
                builtin: function (iterator, cb) {
                    this.__inner = unwrapTraversable(iterator);
                    this.__cb = rt.deref(cb);
                }
            },
            {
                name: "accept", vis: 0,
                builtin: function () {
                    const cur = R.methodCall(this, "current", false, [], null, null, false);
                    const key = R.methodCall(this, "key", false, [], null, null, false);
                    return rt.truthy(rt.call(this.__cb, [cur, key, this.__inner], null, null, false));
                }
            },
        ]
    });

    R.defClass({
        name: "NoRewindIterator", kind: "class", extends: "IteratorIterator",
        methods: [
            { name: "rewind", vis: 0, builtin: function () {} },
        ]
    });

    R.defClass({
        name: "LimitIterator", kind: "class", extends: "IteratorIterator",
        methods: [
            {
                name: "__construct", vis: 0,
                builtin: function (iterator, offset, limit) {
                    this.__inner = unwrapTraversable(iterator);
                    this.__offset = offset !== undefined ? Number(rt.toInt(offset)) : 0;
                    this.__limit = limit !== undefined ? Number(rt.toInt(limit)) : -1;
                    this.__pos = 0;
                }
            },
            {
                name: "rewind", vis: 0,
                builtin: function () {
                    if (!this.__inner) return;
                    R.methodCall(this.__inner, "rewind", false, [], null, null, false);
                    this.__pos = 0;
                    while (this.__pos < this.__offset && rt.truthy(R.methodCall(this.__inner, "valid", false, [], null, null, false))) {
                        R.methodCall(this.__inner, "next", false, [], null, null, false);
                        this.__pos++;
                    }
                }
            },
            {
                name: "valid", vis: 0,
                builtin: function () {
                    if (!this.__inner) return false;
                    if (this.__limit >= 0 && (this.__pos - this.__offset) >= this.__limit) return false;
                    return rt.truthy(R.methodCall(this.__inner, "valid", false, [], null, null, false));
                }
            },
            {
                name: "next", vis: 0,
                builtin: function () {
                    if (!this.__inner) return;
                    R.methodCall(this.__inner, "next", false, [], null, null, false);
                    this.__pos++;
                }
            },
            { name: "getPosition", vis: 0, builtin: function () { return BigInt(this.__pos); } },
        ]
    });

    R.defClass({
        name: "AppendIterator", kind: "class", extends: "IteratorIterator",
        methods: [
            {
                name: "__construct", vis: 0,
                builtin: function () { this.__iters = []; this.__curIdx = 0; this.__inner = null; }
            },
            {
                name: "append", vis: 0,
                builtin: function (it) {
                    const u = unwrapTraversable(it);
                    this.__iters.push(u);
                    if (this.__iters.length === 1) {
                        this.__inner = u;
                        R.methodCall(u, "rewind", false, [], null, null, false);
                    }
                }
            },
            {
                name: "rewind", vis: 0,
                builtin: function () {
                    this.__curIdx = 0;
                    while (this.__curIdx < this.__iters.length) {
                        const it = this.__iters[this.__curIdx];
                        R.methodCall(it, "rewind", false, [], null, null, false);
                        if (rt.truthy(R.methodCall(it, "valid", false, [], null, null, false))) {
                            this.__inner = it;
                            return;
                        }
                        this.__curIdx++;
                    }
                    this.__inner = null;
                }
            },
            {
                name: "valid", vis: 0,
                builtin: function () {
                    return this.__inner ? rt.truthy(R.methodCall(this.__inner, "valid", false, [], null, null, false)) : false;
                }
            },
            {
                name: "next", vis: 0,
                builtin: function () {
                    if (!this.__inner) return;
                    R.methodCall(this.__inner, "next", false, [], null, null, false);
                    while (!rt.truthy(R.methodCall(this.__inner, "valid", false, [], null, null, false))) {
                        this.__curIdx++;
                        if (this.__curIdx >= this.__iters.length) { this.__inner = null; return; }
                        this.__inner = this.__iters[this.__curIdx];
                        R.methodCall(this.__inner, "rewind", false, [], null, null, false);
                    }
                }
            },
            { name: "getIteratorIndex", vis: 0, builtin: function () { return BigInt(this.__curIdx); } },
        ]
    });

    R.defClass({
        name: "CachingIterator", kind: "class", extends: "IteratorIterator",
        consts: [
            { name: "CALL_TOSTRING", vis: 0, val: 1n },
            { name: "CATCH_GET_CHILD", vis: 0, val: 16n },
            { name: "TOSTRING_USE_KEY", vis: 0, val: 2n },
            { name: "TOSTRING_USE_CURRENT", vis: 0, val: 4n },
            { name: "TOSTRING_USE_INNER", vis: 0, val: 8n },
            { name: "FULL_CACHE", vis: 0, val: 256n },
        ],
        methods: [
            {
                name: "__construct", vis: 0,
                builtin: function (it, flags) {
                    this.__inner = unwrapTraversable(it);
                    this.__flags = flags !== undefined ? Number(rt.toInt(flags)) : 1;
                    this.__valid = false;
                    this.__cur = null;
                    this.__key = null;
                    this.__cache = rt.makeArr();
                }
            },
            {
                name: "rewind", vis: 0,
                builtin: function () {
                    this.__cache = rt.makeArr();
                    if (!this.__inner) { this.__valid = false; return; }
                    R.methodCall(this.__inner, "rewind", false, [], null, null, false);
                    R.methodCall(this, "next", false, [], null, null, false);
                }
            },
            {
                name: "next", vis: 0,
                builtin: function () {
                    if (!this.__inner || !rt.truthy(R.methodCall(this.__inner, "valid", false, [], null, null, false))) {
                        this.__valid = false; this.__cur = null; this.__key = null; return;
                    }
                    this.__valid = true;
                    this.__cur = R.methodCall(this.__inner, "current", false, [], null, null, false);
                    this.__key = R.methodCall(this.__inner, "key", false, [], null, null, false);
                    if ((this.__flags & 256) !== 0) rt.arrSetInPlace(this.__cache, this.__key, this.__cur);
                    R.methodCall(this.__inner, "next", false, [], null, null, false);
                }
            },
            { name: "valid", vis: 0, builtin: function () { return !!this.__valid; } },
            { name: "current", vis: 0, builtin: function () { return this.__cur; } },
            { name: "key", vis: 0, builtin: function () { return this.__key; } },
            { name: "hasNext", vis: 0, builtin: function () { return !!(this.__inner && rt.truthy(R.methodCall(this.__inner, "valid", false, [], null, null, false))); } },
            { name: "getCache", vis: 0, builtin: function () { return rt.cloneArr(this.__cache); } },
        ]
    });

    const RecursiveArrayIteratorClass = R.defClass({
        name: "RecursiveArrayIterator", kind: "class", extends: "ArrayIterator", implements: ["RecursiveIterator"],
        methods: [
            {
                name: "hasChildren", vis: 0,
                builtin: function () {
                    const v = R.methodCall(this, "current", false, [], null, null, false);
                    return !!(v && (v.__php === 1 || v.__php === 2));
                }
            },
            {
                name: "getChildren", vis: 0,
                builtin: function () {
                    const v = R.methodCall(this, "current", false, [], null, null, false);
                    return R.newObj(RecursiveArrayIteratorClass, [v], null, null, null, false);
                }
            },
        ]
    });

    R.defClass({
        name: "RecursiveIteratorIterator", kind: "class", implements: ["OuterIterator"],
        consts: [
            { name: "LEAVES_ONLY", vis: 0, val: 0n },
            { name: "SELF_FIRST", vis: 0, val: 1n },
            { name: "CHILD_FIRST", vis: 0, val: 2n },
            { name: "CATCH_GET_CHILD", vis: 0, val: 16n },
        ],
        methods: [
            {
                name: "__construct", vis: 0,
                builtin: function (it, mode, flags) {
                    this.__root = unwrapTraversable(it);
                    this.__mode = mode !== undefined ? Number(rt.toInt(mode)) : 0;
                    this.__flags = flags !== undefined ? Number(rt.toInt(flags)) : 0;
                    this.__entries = [];
                    this.__idx = 0;
                }
            },
            {
                name: "rewind", vis: 0,
                builtin: function () {
                    this.__entries = [];
                    this.__idx = 0;
                    const mode = this.__mode;
                    const walk = (it, depth) => {
                        R.methodCall(it, "rewind", false, [], null, null, false);
                        while (rt.truthy(R.methodCall(it, "valid", false, [], null, null, false))) {
                            const k = R.methodCall(it, "key", false, [], null, null, false);
                            const v = R.methodCall(it, "current", false, [], null, null, false);
                            const hasCh = it.cls.allIfaces.has("recursiveiterator")
                                && rt.truthy(R.methodCall(it, "hasChildren", false, [], null, null, false));
                            if (hasCh) {
                                if (mode === 1) this.__entries.push({ k, v, depth, sub: it });
                                const ch = R.methodCall(it, "getChildren", false, [], null, null, false);
                                if (ch) walk(ch, depth + 1);
                                if (mode === 2) this.__entries.push({ k, v, depth, sub: it });
                            } else {
                                this.__entries.push({ k, v, depth, sub: it });
                            }
                            R.methodCall(it, "next", false, [], null, null, false);
                        }
                    };
                    if (this.__root) walk(this.__root, 0);
                }
            },
            { name: "valid", vis: 0, builtin: function () { return this.__idx < this.__entries.length; } },
            { name: "current", vis: 0, builtin: function () { return this.__idx < this.__entries.length ? this.__entries[this.__idx].v : null; } },
            { name: "key", vis: 0, builtin: function () { return this.__idx < this.__entries.length ? this.__entries[this.__idx].k : null; } },
            { name: "next", vis: 0, builtin: function () { this.__idx++; } },
            { name: "getDepth", vis: 0, builtin: function () { return this.__idx < this.__entries.length ? BigInt(this.__entries[this.__idx].depth) : 0n; } },
            { name: "getInnerIterator", vis: 0, builtin: function () { return this.__idx < this.__entries.length ? this.__entries[this.__idx].sub : this.__root; } },
            { name: "getSubIterator", vis: 0, builtin: function () { return this.__idx < this.__entries.length ? this.__entries[this.__idx].sub : this.__root; } },
        ]
    });

    R.defClass({
        name: "SplFileInfo", kind: "class",
        methods: [
            { name: "__construct", vis: 0, builtin: function (path) { this.__path = rt.toStr(path); } },
            { name: "getPathname", vis: 0, builtin: function () { return this.__path; } },
            { name: "getFilename", vis: 0, builtin: function () { return rt.funcs.get("basename").builtin(this.__path); } },
            { name: "getBasename", vis: 0, builtin: function (suf) { return rt.funcs.get("basename").builtin(this.__path, suf); } },
            { name: "getPath", vis: 0, builtin: function () { return rt.funcs.get("dirname").builtin(this.__path); } },
            { name: "getExtension", vis: 0, builtin: function () { return rt.funcs.get("pathinfo").builtin(this.__path, 4n); } },
            { name: "getRealPath", vis: 0, builtin: function () { return rt.funcs.get("realpath").builtin(this.__path); } },
            { name: "getSize", vis: 0, builtin: function () { return rt.funcs.get("filesize").builtin(this.__path); } },
            { name: "getMTime", vis: 0, builtin: function () { return rt.funcs.get("filemtime").builtin(this.__path); } },
            { name: "isFile", vis: 0, builtin: function () { return rt.funcs.get("is_file").builtin(this.__path); } },
            { name: "isDir", vis: 0, builtin: function () { return rt.funcs.get("is_dir").builtin(this.__path); } },
            { name: "isReadable", vis: 0, builtin: function () { return rt.funcs.get("is_readable").builtin(this.__path); } },
            { name: "isWritable", vis: 0, builtin: function () { return rt.funcs.get("is_writable").builtin(this.__path); } },
            { name: "__toString", vis: 0, builtin: function () { return this.__path; } },
        ]
    });
    });

    // ---- INI / Runtime / Reflection / Class helper functions ----
    const iniStore = new Map([
        ["default_charset", "UTF-8"],
        ["mbstring.internal_encoding", "UTF-8"],
        ["memory_limit", "512M"],
        ["max_execution_time", "30"],
        ["display_errors", "1"],
        ["error_reporting", "32767"],
        ["date.timezone", "UTC"],
        ["include_path", "."],
        ["zend.assertions", "1"],
        ["arg_separator.output", "&"],
        ["variables_order", "EGPCS"],
        ["pcre.jit", "1"],
        ["precision", "14"],
        ["serialize_precision", "-1"],
        ["session.name", "PHPSESSID"],
        ["session.save_path", "/tmp"],
        ["open_basedir", ""],
    ]);
    let lastError = null;
    const loadedExts = new Set([
        "core", "standard", "date", "json", "pcre", "hash", "spl",
        "reflection", "session", "filter", "ctype", "mbstring", "random", "tokenizer",
        "pdo", "pdo_sqlite", "pdo_mysql", "sqlite3", "mysqli", "openssl", "fileinfo"
    ]);

    reg("ini_get", function (k) {
        const key = rt.toStr(k);
        return iniStore.has(key) ? iniStore.get(key) : "";
    });
    reg("ini_set", function (k, v) {
        const key = rt.toStr(k);
        const prev = iniStore.has(key) ? iniStore.get(key) : "";
        iniStore.set(key, rt.toStr(v));
        return prev;
    });
    reg("ini_restore", function () { return null; });
    reg("set_include_path", function (p) {
        const prev = iniStore.get("include_path") || ".";
        iniStore.set("include_path", rt.toStr(p));
        return prev;
    });
    reg("get_include_path", function () { return iniStore.get("include_path") || "."; });
    reg("extension_loaded", function (name) { return loadedExts.has(rt.toStr(name).toLowerCase()); });
    reg("get_loaded_extensions", function () {
        const out = rt.makeArr();
        for (const e of loadedExts) rt.arrPushInPlace(out, e);
        return out;
    });
    reg("phpversion", function () { return "8.4.15-zipp"; });
    reg("zend_version", function () { return "4.4.0"; });
    reg("version_compare", function (a, b, op) {
        const norm = s => rt.toStr(s).replace(/[-_+]/g, ".").replace(/([a-zA-Z]+)(\d)/g, "$1.$2").replace(/(\d)([a-zA-Z]+)/g, "$1.$2").split(".");
        const rank = s => {
            const lo = s.toLowerCase();
            if (lo === "dev") return -5;
            if (lo === "alpha" || lo === "a") return -4;
            if (lo === "beta" || lo === "b") return -3;
            if (lo === "rc") return -2;
            if (lo === "#" || lo === "") return -1;
            if (lo === "pl" || lo === "p") return 1;
            return null;
        };
        const pa = norm(a), pb = norm(b);
        const len = Math.max(pa.length, pb.length);
        let cmp = 0;
        for (let i = 0; i < len; i++) {
            const sa = pa[i] || "0", sb = pb[i] || "0";
            if (sa === sb) continue;
            const na = /^\d+$/.test(sa) ? Number(sa) : null;
            const nb = /^\d+$/.test(sb) ? Number(sb) : null;
            if (na !== null && nb !== null) {
                if (na !== nb) { cmp = na < nb ? -1 : 1; break; }
            } else {
                const ra = na !== null ? 0 : (rank(sa) ?? -1);
                const rb = nb !== null ? 0 : (rank(sb) ?? -1);
                if (ra !== rb) { cmp = ra < rb ? -1 : 1; break; }
                if (sa !== sb) { cmp = sa < sb ? -1 : 1; break; }
            }
        }
        if (op === undefined || rt.deref(op) === null) return BigInt(cmp);
        const o = rt.toStr(op);
        if (o === "<" || o === "lt") return cmp < 0;
        if (o === "<=" || o === "le") return cmp <= 0;
        if (o === ">" || o === "gt") return cmp > 0;
        if (o === ">=" || o === "ge") return cmp >= 0;
        if (o === "==" || o === "=" || o === "eq") return cmp === 0;
        if (o === "!=" || o === "<>" || o === "ne") return cmp !== 0;
        return null;
    });
    reg("memory_get_usage", function () { return 4194304n; });
    reg("memory_get_peak_usage", function () { return 8388608n; });
    reg("gc_collect_cycles", function () { return 0n; });
    reg("set_time_limit", function () { return true; });
    reg("ignore_user_abort", function () { return 0n; });
    reg("setlocale", function () { return "C.UTF-8"; });
    reg("trigger_error", function (msg, level) {
        const m = rt.toStr(msg);
        const lv = level !== undefined ? Number(rt.toInt(level)) : 1024;
        lastError = { type: BigInt(lv), message: m, file: "", line: 0n };
        if (lv === 256) rt.throwError("ErrorException", m);
        return true;
    });
    reg("user_error", function (m, l) { return rt.funcs.get("trigger_error").builtin(m, l); });
    reg("error_get_last", function () {
        if (!lastError) return null;
        const a = rt.makeArr();
        rt.arrSetInPlace(a, "type", lastError.type);
        rt.arrSetInPlace(a, "message", lastError.message);
        rt.arrSetInPlace(a, "file", lastError.file);
        rt.arrSetInPlace(a, "line", lastError.line);
        return a;
    });
    reg("error_clear_last", function () { lastError = null; return null; });
    reg("debug_backtrace", function () { return rt.makeArr(); });
    reg("class_alias", function (orig, alias, autoload) {
        const c = rt.resolveClass(orig, null, null, autoload === undefined ? true : rt.truthy(autoload));
        if (!c) return false;
        rt.classes.set(rt.canon(rt.toStr(alias)), c);
        return true;
    });
    reg("class_uses", function (cls, autoload) {
        const c = rt.resolveClass(cls, null, null, autoload === undefined ? true : rt.truthy(autoload));
        if (!c) return false;
        const out = rt.makeArr();
        for (const t of c.traits) rt.arrSetInPlace(out, t.name, t.name);
        return out;
    });
    reg("class_parents", function (cls, autoload) {
        const c = rt.resolveClass(cls, null, null, autoload === undefined ? true : rt.truthy(autoload));
        if (!c) return false;
        const out = rt.makeArr();
        let cur = c.parent;
        while (cur) { rt.arrSetInPlace(out, cur.name, cur.name); cur = cur.parent; }
        return out;
    });
    reg("class_implements", function (cls, autoload) {
        const c = rt.resolveClass(cls, null, null, autoload === undefined ? true : rt.truthy(autoload));
        if (!c) return false;
        const out = rt.makeArr();
        for (const ic of c.allIfaces) {
            const meta = rt.classes.get(ic);
            if (meta) rt.arrSetInPlace(out, meta.name, meta.name);
        }
        return out;
    });
    reg("get_class_methods", function (cls) {
        const c = rt.resolveClass(cls, null, null, true);
        if (!c) return null;
        const out = rt.makeArr();
        for (const m of c.methods.values()) if (m.vis === 0) rt.arrPushInPlace(out, m.name);
        return out;
    });
    reg("get_class_vars", function (cls) {
        const c = rt.resolveClass(cls, null, null, true);
        if (!c) return false;
        const out = rt.makeArr();
        for (const pm of c.props.values()) if (pm.vis === 0) rt.arrSetInPlace(out, pm.name, pm.hasDefault ? pm.defaultVal : null);
        return out;
    });
    reg("get_declared_classes", function () {
        rt.materializeAllLazyClasses();
        const out = rt.makeArr();
        for (const c of rt.classes.values()) if (c.kind === "class" || c.kind === "enum") rt.arrPushInPlace(out, c.name);
        return out;
    });
    reg("get_declared_interfaces", function () {
        rt.materializeAllLazyClasses();
        const out = rt.makeArr();
        for (const c of rt.classes.values()) if (c.kind === "interface") rt.arrPushInPlace(out, c.name);
        return out;
    });
    reg("get_declared_traits", function () {
        rt.materializeAllLazyClasses();
        const out = rt.makeArr();
        for (const c of rt.classes.values()) if (c.kind === "trait") rt.arrPushInPlace(out, c.name);
        return out;
    });
    reg("get_included_files", function () {
        const out = rt.makeArr();
        for (const f of rt.includedFiles) rt.arrPushInPlace(out, f);
        return out;
    });
    reg("get_required_files", function () { return rt.funcs.get("get_included_files").builtin(); });
    reg("spl_object_hash", function (o) {
        o = rt.deref(o);
        const id = (o && o.id) ? o.id : 0;
        return id.toString(16).padStart(32, "0");
    });
    reg("spl_autoload_functions", function () {
        const out = rt.makeArr();
        for (const cb of rt.autoloaders) rt.arrPushInPlace(out, cb);
        return out;
    });
    reg("spl_autoload_call", function (cls) {
        rt.resolveClass(cls, null, null, true);
        return null;
    });
    reg("is_countable", function (v) {
        v = rt.deref(v);
        return !!(v && (v.__php === 1 || (v.__php === 2 && rt.isSubtype(v.cls, rt.classes.get("countable")))));
    });
    reg("iterator_to_array", function (iter, preserveKeys) {
        const pk = preserveKeys === undefined ? true : rt.truthy(preserveKeys);
        const it = R.iterStart(iter, false);
        const out = rt.makeArr();
        while (R.iterNext(it)) {
            if (pk) rt.arrSetInPlace(out, it.key, it.val);
            else rt.arrPushInPlace(out, it.val);
        }
        return out;
    });
    reg("iterator_count", function (iter) {
        const it = R.iterStart(iter, false);
        let n = 0n;
        while (R.iterNext(it)) n++;
        return n;
    });

    // ---- func_get_args / func_num_args / func_get_arg runtime helpers ----
    R.funcGetArgs = function (boundArgs) {
        const src = (boundArgs && boundArgs._raw) ? boundArgs._raw : (boundArgs || []);
        const out = rt.makeArr();
        for (let i = 0; i < src.length; i++) rt.arrPushInPlace(out, rt.deref(src[i]));
        return out;
    };
    R.funcNumArgs = function (boundArgs) {
        const src = (boundArgs && boundArgs._raw) ? boundArgs._raw : (boundArgs || []);
        return BigInt(src.length);
    };
    R.funcGetArg = function (boundArgs, pos) {
        const src = (boundArgs && boundArgs._raw) ? boundArgs._raw : (boundArgs || []);
        const i = Number(rt.toInt(pos));
        if (i < 0 || i >= src.length) rt.throwError("ValueError", "func_get_arg(): Argument #1 ($position) must be less than the number of the arguments passed");
        return rt.deref(src[i]);
    };

    // ---- ctype_* ----
    function ctypeHelper(s, re) {
        s = rt.deref(s);
        if (typeof s === "bigint") {
            const n = Number(s);
            if (n >= -128 && n <= 255) s = String.fromCharCode(n & 0xFF);
            else s = String(n);
        } else if (typeof s !== "string") {
            return false;
        }
        return s.length > 0 && re.test(s);
    }
    reg("ctype_alpha", s => ctypeHelper(s, /^[A-Za-z]+$/));
    reg("ctype_alnum", s => ctypeHelper(s, /^[A-Za-z0-9]+$/));
    reg("ctype_digit", s => ctypeHelper(s, /^[0-9]+$/));
    reg("ctype_xdigit", s => ctypeHelper(s, /^[0-9A-Fa-f]+$/));
    reg("ctype_lower", s => ctypeHelper(s, /^[a-z]+$/));
    reg("ctype_upper", s => ctypeHelper(s, /^[A-Z]+$/));
    reg("ctype_space", s => ctypeHelper(s, /^[\t\n\v\f\r ]+$/));
    reg("ctype_punct", s => ctypeHelper(s, /^[!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~]+$/));
    reg("ctype_print", s => ctypeHelper(s, /^[\x20-\x7E]+$/));
    reg("ctype_graph", s => ctypeHelper(s, /^[\x21-\x7E]+$/));
    reg("ctype_cntrl", s => ctypeHelper(s, /^[\x00-\x1F\x7F]+$/));

    // ---- Extended mbstring ----
    let mbEnc = "UTF-8";
    reg("mb_internal_encoding", function (enc) {
        if (enc === undefined || rt.deref(enc) === null) return mbEnc;
        mbEnc = rt.toStr(enc);
        return true;
    });
    reg("mb_check_encoding", function (val, enc) { return true; });
    reg("mb_detect_encoding", function (str) { return "UTF-8"; });
    reg("mb_convert_encoding", function (str, to, from) { return rt.toStr(str); });
    reg("mb_stripos", function (h, n, off) {
        return rt.funcs.get("mb_strpos").builtin(rt.funcs.get("mb_strtolower").builtin(h), rt.funcs.get("mb_strtolower").builtin(n), off);
    });
    reg("mb_strrpos", function (h, n, off) {
        const hu = Array.from(rt.bytesToUtf8(rt.toStr(h))), nu = Array.from(rt.bytesToUtf8(rt.toStr(n)));
        const hs = hu.join(""), ns = nu.join("");
        const idx = hs.lastIndexOf(ns);
        if (idx < 0) return false;
        return BigInt(Array.from(hs.slice(0, idx)).length);
    });
    reg("mb_strripos", function (h, n, off) {
        return rt.funcs.get("mb_strrpos").builtin(rt.funcs.get("mb_strtolower").builtin(h), rt.funcs.get("mb_strtolower").builtin(n), off);
    });
    reg("mb_str_split", function (s, len) {
        const cps = Array.from(rt.bytesToUtf8(rt.toStr(s)));
        const n = len === undefined ? 1 : Math.max(1, Number(rt.toInt(len)));
        const out = rt.makeArr();
        for (let i = 0; i < cps.length; i += n) rt.arrPushInPlace(out, rt.utf8ToBytes(cps.slice(i, i + n).join("")));
        return out;
    });
    reg("mb_substr_count", function (h, n) {
        const hs = rt.bytesToUtf8(rt.toStr(h)), ns = rt.bytesToUtf8(rt.toStr(n));
        if (!ns) return 0n;
        let c = 0n, pos = 0;
        while ((pos = hs.indexOf(ns, pos)) >= 0) { c++; pos += ns.length; }
        return c;
    });
    reg("mb_ord", function (s) {
        const u = rt.bytesToUtf8(rt.toStr(s));
        return u.length === 0 ? false : BigInt(u.codePointAt(0));
    });
    reg("mb_chr", function (cp) { return rt.utf8ToBytes(String.fromCodePoint(Number(rt.toInt(cp)))); });
    reg("mb_convert_case", function (s, mode) {
        const u = rt.bytesToUtf8(rt.toStr(s));
        const m = Number(rt.toInt(mode));
        if (m === 0) return rt.utf8ToBytes(u.toUpperCase());
        if (m === 1 || m === 3) return rt.utf8ToBytes(u.toLowerCase());
        return rt.utf8ToBytes(u.toLowerCase().replace(/(^|\s)\S/g, ch => ch.toUpperCase()));
    });
    reg("mb_ucfirst", function (s) {
        const cps = Array.from(rt.bytesToUtf8(rt.toStr(s)));
        if (cps.length === 0) return "";
        cps[0] = cps[0].toUpperCase();
        return rt.utf8ToBytes(cps.join(""));
    });
    reg("mb_lcfirst", function (s) {
        const cps = Array.from(rt.bytesToUtf8(rt.toStr(s)));
        if (cps.length === 0) return "";
        cps[0] = cps[0].toLowerCase();
        return rt.utf8ToBytes(cps.join(""));
    });
    reg("mb_trim", function (s) { return rt.utf8ToBytes(rt.bytesToUtf8(rt.toStr(s)).trim()); });
    reg("mb_ltrim", function (s) { return rt.utf8ToBytes(rt.bytesToUtf8(rt.toStr(s)).trimStart()); });
    reg("mb_rtrim", function (s) { return rt.utf8ToBytes(rt.bytesToUtf8(rt.toStr(s)).trimEnd()); });
    reg("mb_strwidth", function (s) { return BigInt(Array.from(rt.bytesToUtf8(rt.toStr(s))).length); });
    reg("mb_strimwidth", function (s, start, width, marker) {
        const cps = Array.from(rt.bytesToUtf8(rt.toStr(s)));
        const st = Number(rt.toInt(start));
        const w = Number(rt.toInt(width));
        const mk = marker !== undefined ? rt.bytesToUtf8(rt.toStr(marker)) : "";
        const slice = cps.slice(st);
        if (slice.length <= w) return rt.utf8ToBytes(slice.join(""));
        return rt.utf8ToBytes(slice.slice(0, Math.max(0, w - Array.from(mk).length)).join("") + mk);
    });
    reg("mb_split", function (pat, str, limit) {
        const p = rt.bytesToUtf8(rt.toStr(pat));
        const s = rt.bytesToUtf8(rt.toStr(str));
        const lim = limit !== undefined ? Number(rt.toInt(limit)) : -1;
        const re = new RegExp(p, "gu");
        const parts = s.split(re);
        const out = rt.makeArr();
        if (lim > 0 && parts.length > lim) {
            for (let i = 0; i < lim - 1; i++) rt.arrPushInPlace(out, rt.utf8ToBytes(parts[i]));
            // Re-join remainder roughly
            rt.arrPushInPlace(out, rt.utf8ToBytes(parts.slice(lim - 1).join("")));
        } else {
            for (let i = 0; i < parts.length; i++) rt.arrPushInPlace(out, rt.utf8ToBytes(parts[i]));
        }
        return out;
    });
    reg("mb_strstr", function (h, n, before) {
        const hu = rt.bytesToUtf8(rt.toStr(h)), nu = rt.bytesToUtf8(rt.toStr(n));
        const idx = hu.indexOf(nu);
        if (idx < 0) return false;
        return rt.utf8ToBytes(before && rt.truthy(before) ? hu.slice(0, idx) : hu.slice(idx));
    });
    reg("mb_stristr", function (h, n, before) {
        const hu = rt.bytesToUtf8(rt.toStr(h)), nu = rt.bytesToUtf8(rt.toStr(n));
        const idx = hu.toLowerCase().indexOf(nu.toLowerCase());
        if (idx < 0) return false;
        return rt.utf8ToBytes(before && rt.truthy(before) ? hu.slice(0, idx) : hu.slice(idx));
    });
    reg("mb_ereg_replace", function (pat, rep, str) {
        const re = new RegExp(rt.bytesToUtf8(rt.toStr(pat)), "gu");
        return rt.utf8ToBytes(rt.bytesToUtf8(rt.toStr(str)).replace(re, rt.bytesToUtf8(rt.toStr(rep))));
    });
    reg("mb_str_pad", function (s, len, padStr, padType) {
        const cps = Array.from(rt.bytesToUtf8(rt.toStr(s)));
        const target = Number(rt.toInt(len));
        if (cps.length >= target) return rt.toStr(s);
        const p = Array.from(rt.bytesToUtf8(padStr !== undefined ? rt.toStr(padStr) : " "));
        const need = target - cps.length;
        const makePad = n => { let o = ""; for (let i = 0; i < n; i++) o += p[i % p.length]; return o; };
        const t = padType !== undefined ? Number(rt.toInt(padType)) : 1;
        const orig = cps.join("");
        if (t === 0) return rt.utf8ToBytes(makePad(need) + orig);
        if (t === 2) { const l = Math.floor(need / 2); return rt.utf8ToBytes(makePad(l) + orig + makePad(need - l)); }
        return rt.utf8ToBytes(orig + makePad(need));
    });
    reg("iconv", function (from, to, str) { return rt.toStr(str); });

    // ---- Extended String functions ----
    reg("rawurlencode", function (s) {
        s = rt.toStr(s);
        let out = "";
        for (let i = 0; i < s.length; i++) {
            const b = s.charCodeAt(i) & 0xFF;
            if ((b >= 65 && b <= 90) || (b >= 97 && b <= 122) || (b >= 48 && b <= 57) || b === 45 || b === 95 || b === 46 || b === 126) {
                out += s[i];
            } else {
                out += "%" + b.toString(16).toUpperCase().padStart(2, "0");
            }
        }
        return out;
    });
    reg("rawurldecode", function (s) {
        s = rt.toStr(s);
        let out = "";
        for (let i = 0; i < s.length; i++) {
            if (s[i] === "%" && i + 2 < s.length) {
                const hex = parseInt(s.slice(i + 1, i + 3), 16);
                if (!Number.isNaN(hex)) { out += String.fromCharCode(hex); i += 2; continue; }
            }
            out += s[i];
        }
        return out;
    });
    reg("strtr", function (str, from, to) {
        str = rt.toStr(str);
        if (arguments.length === 2 && from && from.__php === 1) {
            const pairs = [];
            for (const [k, v] of from.m) {
                const ks = String(k);
                if (ks.length > 0) pairs.push([ks, rt.toStr(v)]);
            }
            pairs.sort((a, b) => b[0].length - a[0].length);
            let out = "", i = 0;
            while (i < str.length) {
                let matched = false;
                for (let j = 0; j < pairs.length; j++) {
                    const p = pairs[j];
                    if (str.startsWith(p[0], i)) { out += p[1]; i += p[0].length; matched = true; break; }
                }
                if (!matched) { out += str[i++]; }
            }
            return out;
        }
        const f = rt.toStr(from), t = rt.toStr(to);
        const len = Math.min(f.length, t.length);
        const map = new Map();
        for (let i = 0; i < len; i++) map.set(f[i], t[i]);
        let out = "";
        for (let i = 0; i < str.length; i++) out += map.has(str[i]) ? map.get(str[i]) : str[i];
        return out;
    });
    reg("strpbrk", function (h, chars) {
        h = rt.toStr(h); chars = rt.toStr(chars);
        for (let i = 0; i < h.length; i++) if (chars.includes(h[i])) return h.slice(i);
        return false;
    });
    reg("strspn", function (s, mask, start, len) {
        s = rt.toStr(s); mask = rt.toStr(mask);
        let st = start !== undefined ? Number(rt.toInt(start)) : 0;
        if (st < 0) st = Math.max(0, s.length + st);
        const end = len !== undefined ? Math.min(s.length, st + Number(rt.toInt(len))) : s.length;
        let c = 0n;
        for (let i = st; i < end; i++) { if (mask.includes(s[i])) c++; else break; }
        return c;
    });
    reg("strcspn", function (s, mask, start, len) {
        s = rt.toStr(s); mask = rt.toStr(mask);
        let st = start !== undefined ? Number(rt.toInt(start)) : 0;
        if (st < 0) st = Math.max(0, s.length + st);
        const end = len !== undefined ? Math.min(s.length, st + Number(rt.toInt(len))) : s.length;
        let c = 0n;
        for (let i = st; i < end; i++) { if (!mask.includes(s[i])) c++; else break; }
        return c;
    });
    reg("lcfirst", function (s) { s = rt.toStr(s); return s ? s[0].toLowerCase() + s.slice(1) : ""; });
    reg("ucfirst", function (s) { s = rt.toStr(s); return s ? s[0].toUpperCase() + s.slice(1) : ""; });
    reg("ucwords", function (s, seps) {
        s = rt.toStr(s);
        const sepSet = seps !== undefined ? rt.toStr(seps) : " \t\r\n\f\v";
        let out = "", cap = true;
        for (let i = 0; i < s.length; i++) {
            const c = s[i];
            if (sepSet.includes(c)) { out += c; cap = true; }
            else { out += cap ? c.toUpperCase() : c; cap = false; }
        }
        return out;
    });
    reg("substr_replace", function (s, rep, off, len) {
        s = rt.toStr(s); rep = rt.toStr(rep);
        let st = Number(rt.toInt(off));
        if (st < 0) st = Math.max(0, s.length + st);
        let end;
        if (len === undefined || rt.deref(len) === null) end = s.length;
        else {
            const l = Number(rt.toInt(len));
            end = l < 0 ? Math.max(st, s.length + l) : st + l;
        }
        return s.slice(0, st) + rep + s.slice(end);
    });
    reg("addslashes", function (s) { return rt.toStr(s).replace(/[\\"'\0]/g, c => c === "\0" ? "\\0" : "\\" + c); });
    reg("stripslashes", function (s) { return rt.toStr(s).replace(/\\(.)/g, (_, c) => c === "0" ? "\0" : c); });
    reg("addcslashes", function (s, chars) {
        s = rt.toStr(s); const cs = rt.toStr(chars);
        let out = "";
        for (let i = 0; i < s.length; i++) out += cs.includes(s[i]) ? "\\" + s[i] : s[i];
        return out;
    });
    reg("stripcslashes", function (s) {
        return rt.toStr(s).replace(/\\(n|r|t|v|f|a|b|\\|"|'|[0-7]{1,3}|x[0-9a-fA-F]{1,2})/g, (_, e) => {
            if (e === "n") return "\n"; if (e === "r") return "\r"; if (e === "t") return "\t";
            if (e === "v") return "\v"; if (e === "f") return "\f";
            if (e[0] === "x") return String.fromCharCode(parseInt(e.slice(1), 16));
            if (/^[0-7]/.test(e)) return String.fromCharCode(parseInt(e, 8));
            return e;
        });
    });
    reg("nl2br", function (s, xhtml) {
        const br = (xhtml === undefined || rt.truthy(xhtml)) ? "<br />" : "<br>";
        return rt.toStr(s).replace(/\r\n|\n\r|\r|\n/g, m => br + m);
    });
    reg("strip_tags", function (s) { return rt.toStr(s).replace(/<[^>]*>/g, ""); });
    reg("htmlentities", function (s, f) { return rt.funcs.get("htmlspecialchars").builtin(s, f); });
    reg("html_entity_decode", function (s) {
        return rt.toStr(s)
            .replace(/&quot;/g, '"').replace(/&#039;|&apos;/g, "'")
            .replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&");
    });
    reg("str_ireplace", function (search, replace, subject, countRef) {
        const sArr = (search && search.__php === 1) ? Array.from(search.m.values()).map(rt.toStr) : [rt.toStr(search)];
        const rArr = (replace && replace.__php === 1) ? Array.from(replace.m.values()).map(rt.toStr) : sArr.map(() => rt.toStr(replace));
        let cnt = 0;
        function one(str) {
            let cur = rt.toStr(str);
            for (let i = 0; i < sArr.length; i++) {
                if (!sArr[i]) continue;
                const esc = sArr[i].replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
                const re = new RegExp(esc, "gi");
                const rep = rArr[i] !== undefined ? rArr[i] : "";
                cur = cur.replace(re, () => { cnt++; return rep; });
            }
            return cur;
        }
        if (subject && subject.__php === 1) {
            const out = rt.makeArr();
            for (const [k, v] of subject.m) rt.arrSetInPlace(out, k, one(v));
            if (countRef && rt.isRef(countRef)) countRef.v = BigInt(cnt);
            return out;
        }
        const res = one(subject);
        if (countRef && rt.isRef(countRef)) countRef.v = BigInt(cnt);
        return res;
    }, 8);
    reg("chunk_split", function (s, len, end) {
        s = rt.toStr(s);
        const l = len === undefined ? 76 : Math.max(1, Number(rt.toInt(len)));
        const e = end === undefined ? "\r\n" : rt.toStr(end);
        let out = "";
        for (let i = 0; i < s.length; i += l) out += s.slice(i, i + l) + e;
        return out;
    });
    reg("strnatcmp", function (a, b) {
        a = rt.toStr(a); b = rt.toStr(b);
        return BigInt(a.localeCompare(b, undefined, { numeric: true, sensitivity: "variant" }));
    });
    reg("strnatcasecmp", function (a, b) {
        a = rt.toStr(a).toLowerCase(); b = rt.toStr(b).toLowerCase();
        return BigInt(a.localeCompare(b, undefined, { numeric: true }));
    });
    reg("quotemeta", function (s) { return rt.toStr(s).replace(/[.\\+*?\[\^\]$()]/g, "\\$&"); });
    reg("utf8_encode", function (s) { return rt.utf8ToBytes(rt.toStr(s)); });
    reg("utf8_decode", function (s) { return rt.bytesToUtf8(rt.toStr(s)); });

    // ---- Extended Array functions (PHP 8.1-8.4 + Laravel/Symfony staples) ----
    function needArr(v, fn) {
        v = rt.deref(v);
        if (!v || v.__php !== 1) rt.throwError("TypeError", fn + "(): Argument must be of type array");
        return v;
    }
    reg("array_is_list", function (a) {
        a = needArr(a, "array_is_list");
        let i = 0n;
        for (const k of a.m.keys()) { if (k !== i) return false; i++; }
        return true;
    });
    reg("array_key_first", function (a) {
        a = needArr(a, "array_key_first");
        for (const k of a.m.keys()) return k;
        return null;
    });
    reg("array_key_last", function (a) {
        a = needArr(a, "array_key_last");
        let last = null;
        for (const k of a.m.keys()) last = k;
        return last;
    });
    reg("array_find", function (a, cb) {
        a = needArr(a, "array_find");
        for (const [k, v] of a.m) {
            const dv = rt.deref(v);
            if (rt.truthy(rt.call(cb, [dv, k], null, null, false))) return dv;
        }
        return null;
    });
    reg("array_find_key", function (a, cb) {
        a = needArr(a, "array_find_key");
        for (const [k, v] of a.m) {
            if (rt.truthy(rt.call(cb, [rt.deref(v), k], null, null, false))) return k;
        }
        return null;
    });
    reg("array_any", function (a, cb) {
        a = needArr(a, "array_any");
        for (const [k, v] of a.m) {
            if (rt.truthy(rt.call(cb, [rt.deref(v), k], null, null, false))) return true;
        }
        return false;
    });
    reg("array_all", function (a, cb) {
        a = needArr(a, "array_all");
        for (const [k, v] of a.m) {
            if (!rt.truthy(rt.call(cb, [rt.deref(v), k], null, null, false))) return false;
        }
        return true;
    });
    reg("array_replace", function () {
        const out = rt.makeArr();
        for (let i = 0; i < arguments.length; i++) {
            const a = needArr(arguments[i], "array_replace");
            for (const [k, v] of a.m) rt.arrSetInPlace(out, k, v);
        }
        return out;
    });
    reg("array_replace_recursive", function () {
        function mergeRec(base, add) {
            for (const [k, v] of add.m) {
                const dv = rt.deref(v);
                if (dv && dv.__php === 1 && base.m.has(k)) {
                    const cur = rt.deref(base.m.get(k));
                    if (cur && cur.__php === 1) {
                        const sub = rt.cloneArr(cur);
                        mergeRec(sub, dv);
                        rt.arrSetInPlace(base, k, sub);
                        continue;
                    }
                }
                rt.arrSetInPlace(base, k, dv);
            }
        }
        const out = rt.makeArr();
        for (let i = 0; i < arguments.length; i++) mergeRec(out, needArr(arguments[i], "array_replace_recursive"));
        return out;
    });
    reg("array_merge_recursive", function () {
        function mergeRec(base, add) {
            for (const [k, v] of add.m) {
                const dv = rt.deref(v);
                if (typeof k === "bigint") {
                    rt.arrPushInPlace(base, dv);
                } else if (base.m.has(k)) {
                    const cur = rt.deref(base.m.get(k));
                    if (cur && cur.__php === 1 && dv && dv.__php === 1) {
                        const sub = rt.cloneArr(cur);
                        mergeRec(sub, dv);
                        rt.arrSetInPlace(base, k, sub);
                    } else {
                        const sub = rt.makeArr();
                        if (cur && cur.__php === 1) { for (const [, sv] of cur.m) rt.arrPushInPlace(sub, sv); }
                        else rt.arrPushInPlace(sub, cur);
                        if (dv && dv.__php === 1) { for (const [, sv] of dv.m) rt.arrPushInPlace(sub, sv); }
                        else rt.arrPushInPlace(sub, dv);
                        rt.arrSetInPlace(base, k, sub);
                    }
                } else {
                    rt.arrSetInPlace(base, k, dv);
                }
            }
        }
        const out = rt.makeArr();
        for (let i = 0; i < arguments.length; i++) mergeRec(out, needArr(arguments[i], "array_merge_recursive"));
        return out;
    });
    reg("array_diff_key", function (base) {
        base = needArr(base, "array_diff_key");
        const others = Array.from(arguments).slice(1).map(a => needArr(a, "array_diff_key"));
        const out = rt.makeArr();
        for (const [k, v] of base.m) {
            if (!others.some(o => o.m.has(k))) rt.arrSetInPlace(out, k, v);
        }
        return out;
    });
    reg("array_intersect_key", function (base) {
        base = needArr(base, "array_intersect_key");
        const others = Array.from(arguments).slice(1).map(a => needArr(a, "array_intersect_key"));
        const out = rt.makeArr();
        for (const [k, v] of base.m) {
            if (others.every(o => o.m.has(k))) rt.arrSetInPlace(out, k, v);
        }
        return out;
    });
    reg("array_diff_assoc", function (base) {
        base = needArr(base, "array_diff_assoc");
        const others = Array.from(arguments).slice(1).map(a => needArr(a, "array_diff_assoc"));
        const out = rt.makeArr();
        for (const [k, v] of base.m) {
            const sv = rt.toStr(v);
            if (!others.some(o => o.m.has(k) && rt.toStr(o.m.get(k)) === sv)) rt.arrSetInPlace(out, k, v);
        }
        return out;
    });
    reg("array_intersect_assoc", function (base) {
        base = needArr(base, "array_intersect_assoc");
        const others = Array.from(arguments).slice(1).map(a => needArr(a, "array_intersect_assoc"));
        const out = rt.makeArr();
        for (const [k, v] of base.m) {
            const sv = rt.toStr(v);
            if (others.every(o => o.m.has(k) && rt.toStr(o.m.get(k)) === sv)) rt.arrSetInPlace(out, k, v);
        }
        return out;
    });
    reg("array_change_key_case", function (a, cs) {
        a = needArr(a, "array_change_key_case");
        const up = cs !== undefined && Number(rt.toInt(cs)) === 1;
        const out = rt.makeArr();
        for (const [k, v] of a.m) {
            const nk = typeof k === "string" ? (up ? k.toUpperCase() : k.toLowerCase()) : k;
            rt.arrSetInPlace(out, nk, v);
        }
        return out;
    });
    reg("array_fill", function (start, count, val) {
        const st = rt.toInt(start), cnt = Number(rt.toInt(count));
        const out = rt.makeArr();
        for (let i = 0; i < cnt; i++) rt.arrSetInPlace(out, i === 0 ? st : (st < 0n ? BigInt(i - 1) : st + BigInt(i)), val);
        return out;
    });
    reg("array_fill_keys", function (keys, val) {
        keys = needArr(keys, "array_fill_keys");
        const out = rt.makeArr();
        for (const v of keys.m.values()) rt.arrSetInPlace(out, rt.deref(v), val);
        return out;
    });
    reg("array_pad", function (a, size, val) {
        a = needArr(a, "array_pad");
        const sz = Number(rt.toInt(size));
        const need = Math.abs(sz) - a.m.size;
        const out = rt.makeArr();
        if (need <= 0) return rt.cloneArr(a);
        if (sz < 0) for (let i = 0; i < need; i++) rt.arrPushInPlace(out, val);
        for (const [k, v] of a.m) {
            if (typeof k === "string") rt.arrSetInPlace(out, k, v);
            else rt.arrPushInPlace(out, v);
        }
        if (sz > 0) for (let i = 0; i < need; i++) rt.arrPushInPlace(out, val);
        return out;
    });
    reg("array_chunk", function (a, len, preserve) {
        a = needArr(a, "array_chunk");
        const l = Math.max(1, Number(rt.toInt(len)));
        const pk = preserve !== undefined && rt.truthy(preserve);
        const out = rt.makeArr();
        let cur = null, c = 0;
        for (const [k, v] of a.m) {
            if (cur === null || c >= l) { cur = rt.makeArr(); rt.arrPushInPlace(out, cur); c = 0; }
            if (pk) rt.arrSetInPlace(cur, k, v); else rt.arrPushInPlace(cur, v);
            c++;
        }
        return out;
    });
    reg("array_combine", function (keys, vals) {
        keys = needArr(keys, "array_combine");
        vals = needArr(vals, "array_combine");
        if (keys.m.size !== vals.m.size) rt.throwError("ValueError", "array_combine(): Argument #1 ($keys) and argument #2 ($values) must have the same number of elements");
        const out = rt.makeArr();
        const ka = Array.from(keys.m.values()), va = Array.from(vals.m.values());
        for (let i = 0; i < ka.length; i++) rt.arrSetInPlace(out, rt.toStr(ka[i]), va[i]);
        return out;
    });
    reg("array_flip", function (a) {
        a = needArr(a, "array_flip");
        const out = rt.makeArr();
        for (const [k, v] of a.m) rt.arrSetInPlace(out, rt.deref(v), k);
        return out;
    });
    reg("array_count_values", function (a) {
        a = needArr(a, "array_count_values");
        const out = rt.makeArr();
        for (const v of a.m.values()) {
            const k = rt.arrKey(rt.deref(v));
            out._list = false; out._e = null;
            out.m.set(k, (out.m.get(k) || 0n) + 1n);
        }
        return out;
    });
    reg("array_sum", function (a) {
        a = needArr(a, "array_sum");
        let acc = 0n;
        for (const v of a.m.values()) acc = R.add(acc, rt.deref(v));
        return acc;
    });
    reg("array_product", function (a) {
        a = needArr(a, "array_product");
        let acc = 1n;
        for (const v of a.m.values()) acc = R.mul(acc, rt.deref(v));
        return acc;
    });
    reg("range", function (start, end, step) {
        start = rt.deref(start); end = rt.deref(end);
        const out = rt.makeArr();
        if (typeof start === "string" && typeof end === "string" && start.length === 1 && end.length === 1 && !/^\d$/.test(start)) {
            const a = start.charCodeAt(0), b = end.charCodeAt(0);
            const st = step !== undefined ? Math.max(1, Math.abs(Number(rt.toInt(step)))) : 1;
            if (a <= b) for (let c = a; c <= b; c += st) rt.arrPushInPlace(out, String.fromCharCode(c));
            else for (let c = a; c >= b; c -= st) rt.arrPushInPlace(out, String.fromCharCode(c));
            return out;
        }
        const isFloat = typeof start === "number" || typeof end === "number" || typeof step === "number";
        if (isFloat) {
            const a = rt.toFloat(start), b = rt.toFloat(end);
            const st = step !== undefined ? Math.abs(rt.toFloat(step)) : 1.0;
            if (a <= b) for (let x = a; x <= b + 1e-12; x += st) rt.arrPushInPlace(out, x);
            else for (let x = a; x >= b - 1e-12; x -= st) rt.arrPushInPlace(out, x);
            return out;
        }
        const a = rt.toInt(start), b = rt.toInt(end);
        let st = step !== undefined ? rt.toInt(step) : 1n;
        if (st < 0n) st = -st;
        if (st === 0n) rt.throwError("ValueError", "range(): Argument #3 ($step) cannot be 0");
        if (a <= b) for (let x = a; x <= b; x += st) rt.arrPushInPlace(out, x);
        else for (let x = a; x >= b; x -= st) rt.arrPushInPlace(out, x);
        return out;
    });
    reg("uasort", function (arrRef, cb) {
        const a = rt.cow(needArr(arrRef, "uasort"));
        const entries = Array.from(a.m.entries());
        entries.sort((x, y) => Number(rt.toInt(rt.call(cb, [rt.deref(x[1]), rt.deref(y[1])], null, null, false))));
        a.m = new Map(entries);
        if (rt.isRef(arrRef)) arrRef.v = a;
        return true;
    }, 1);
    reg("uksort", function (arrRef, cb) {
        const a = rt.cow(needArr(arrRef, "uksort"));
        const entries = Array.from(a.m.entries());
        entries.sort((x, y) => Number(rt.toInt(rt.call(cb, [x[0], y[0]], null, null, false))));
        a.m = new Map(entries);
        if (rt.isRef(arrRef)) arrRef.v = a;
        return true;
    }, 1);
    reg("natsort", function (arrRef) {
        const a = rt.cow(needArr(arrRef, "natsort"));
        const entries = Array.from(a.m.entries());
        entries.sort((x, y) => rt.toStr(x[1]).localeCompare(rt.toStr(y[1]), undefined, { numeric: true }));
        a.m = new Map(entries);
        if (rt.isRef(arrRef)) arrRef.v = a;
        return true;
    }, 1);
    reg("natcasesort", function (arrRef) {
        const a = rt.cow(needArr(arrRef, "natcasesort"));
        const entries = Array.from(a.m.entries());
        entries.sort((x, y) => rt.toStr(x[1]).toLowerCase().localeCompare(rt.toStr(y[1]).toLowerCase(), undefined, { numeric: true }));
        a.m = new Map(entries);
        if (rt.isRef(arrRef)) arrRef.v = a;
        return true;
    }, 1);
    reg("shuffle", function (arrRef) {
        const a = rt.cow(needArr(arrRef, "shuffle"));
        const vals = Array.from(a.m.values());
        for (let i = vals.length - 1; i > 0; i--) {
            const j = Math.floor(Math.random() * (i + 1));
            const tmp = vals[i]; vals[i] = vals[j]; vals[j] = tmp;
        }
        const out = rt.makeArr();
        for (const v of vals) rt.arrPushInPlace(out, v);
        if (rt.isRef(arrRef)) arrRef.v = out;
        return true;
    }, 1);
    reg("array_rand", function (a, num) {
        a = needArr(a, "array_rand");
        const keys = Array.from(a.m.keys());
        const n = num === undefined ? 1 : Number(rt.toInt(num));
        if (n === 1) return keys[Math.floor(Math.random() * keys.length)];
        const out = rt.makeArr();
        const picked = new Set();
        while (picked.size < Math.min(n, keys.length)) picked.add(Math.floor(Math.random() * keys.length));
        for (const idx of picked) rt.arrPushInPlace(out, keys[idx]);
        return out;
    });
    reg("array_walk_recursive", function (arrRef, cb, arg) {
        function walk(arr) {
            for (const [k, v] of arr.m) {
                const dv = rt.deref(v);
                if (dv && dv.__php === 1) { walk(dv); continue; }
                const cell = rt.isRef(v) ? v : rt.mkRef(dv);
                arr.hasRefs = true;
                arr._e = null;
                arr.m.set(k, cell);
                const callArgs = arg !== undefined ? [cell, k, arg] : [cell, k];
                rt.call(cb, callArgs, null, null, false);
            }
        }
        const a = rt.cow(needArr(arrRef, "array_walk_recursive"));
        walk(a);
        if (rt.isRef(arrRef)) arrRef.v = a;
        return true;
    }, 1);

    // ---- Extended PCRE ----
    reg("preg_last_error", function () { return 0n; });
    reg("preg_last_error_msg", function () { return "No error"; });
    reg("preg_grep", function (pat, arr, flags) {
        const a = needArr(arr, "preg_grep");
        const inv = flags !== undefined && (Number(rt.toInt(flags)) & 1) !== 0;
        const pm = rt.funcs.get("preg_match").builtin;
        const out = rt.makeArr();
        for (const [k, v] of a.m) {
            const matched = pm(pat, v) === 1n;
            if (matched !== inv) rt.arrSetInPlace(out, k, v);
        }
        return out;
    });
    reg("preg_replace_callback_array", function (map, subj, limit, countRef) {
        const m = needArr(map, "preg_replace_callback_array");
        const prc = rt.funcs.get("preg_replace_callback").builtin;
        let cur = subj;
        let total = 0n;
        for (const [pat, cb] of m.m) {
            const cRef = rt.mkRef(0n);
            cur = prc(pat, cb, cur, limit, cRef);
            total += rt.toInt(cRef.v);
        }
        if (countRef && rt.isRef(countRef)) countRef.v = total;
        return cur;
    }, 8);

    // ---- Extended Filesystem / Streams / HTTP helpers ----
    reg("clearstatcache", function () { return null; });
    reg("is_link", function () { return false; });
    reg("is_executable", function (p) { return rt.funcs.get("is_file").builtin(p); });
    reg("file", function (path, flags) {
        const data = rt.funcs.get("file_get_contents").builtin(path);
        if (data === false) return false;
        const f = flags !== undefined ? Number(rt.toInt(flags)) : 0;
        const ignoreNl = (f & 2) !== 0;
        const skipEmpty = (f & 4) !== 0;
        const out = rt.makeArr();
        if (data.length === 0) return out;
        const raw = data.split("\n");
        for (let i = 0; i < raw.length; i++) {
            let line = raw[i];
            if (i === raw.length - 1 && line === "") continue;
            if (!ignoreNl && i < raw.length - 1) line += "\n";
            if (ignoreNl && line.endsWith("\r")) line = line.slice(0, -1);
            if (skipEmpty && line.replace(/\r?\n$/, "").length === 0) continue;
            rt.arrPushInPlace(out, line);
        }
        return out;
    });
    reg("readfile", function (path) {
        const data = rt.funcs.get("file_get_contents").builtin(path);
        if (data === false) return false;
        rt.echoStr(data);
        return BigInt(data.length);
    });
    reg("stream_get_contents", function (res, len, off) {
        res = rt.deref(res);
        if (!res || res.__php !== 4 || res.closed) return false;
        if (off !== undefined && Number(rt.toInt(off)) >= 0) res.pos = Number(rt.toInt(off));
        const max = (len === undefined || rt.deref(len) === null || Number(rt.toInt(len)) < 0)
            ? res.data.length - res.pos : Number(rt.toInt(len));
        const chunk = res.data.slice(res.pos, res.pos + max);
        res.pos += chunk.length;
        return chunk;
    });
    reg("stream_get_meta_data", function (res) {
        res = rt.deref(res);
        const out = rt.makeArr();
        rt.arrSetInPlace(out, "uri", res ? (res.path || "") : "");
        rt.arrSetInPlace(out, "mode", res ? (res.mode || "r") : "r");
        rt.arrSetInPlace(out, "seekable", true);
        rt.arrSetInPlace(out, "eof", !res || res.pos >= res.data.length);
        return out;
    });
    reg("stream_set_blocking", function () { return true; });
    reg("stream_is_local", function () { return true; });
    reg("stream_resolve_include_path", function (p) {
        const s = rt.toStr(p);
        return rt.funcs.get("file_exists").builtin(s) ? (rt.funcs.get("realpath").builtin(s) || s) : false;
    });
    reg("fflush", function () { return true; });
    reg("flock", function () { return true; });
    reg("ftruncate", function (res, size) {
        res = rt.deref(res);
        if (!res || res.__php !== 4 || res.closed) return false;
        res.data = res.data.slice(0, Math.max(0, Number(rt.toInt(size))));
        res.dirty = true;
        return true;
    });
    reg("touch", function (p) {
        if (!rt.funcs.get("file_exists").builtin(p)) rt.funcs.get("file_put_contents").builtin(p, "");
        return true;
    });
    reg("chmod", function () { return true; });
    reg("tempnam", function (dir, pfx) {
        const d = rt.toStr(dir) || rt.funcs.get("sys_get_temp_dir").builtin();
        const p = d.replace(/[\\/]+$/, "") + "/" + rt.toStr(pfx || "php") + Math.floor(Math.random() * 1e9).toString(36);
        rt.funcs.get("file_put_contents").builtin(p, "");
        return p;
    });
    reg("getallheaders", function () {
        const out = rt.makeArr();
        const srv = rt.globalScope.get("_SERVER");
        if (srv && srv.v && srv.v.__php === 1) {
            for (const [k, v] of srv.v.m) {
                if (typeof k === "string" && k.startsWith("HTTP_")) {
                    const name = k.slice(5).toLowerCase().replace(/(^|_)([a-z])/g, (_, a, b) => (a ? "-" : "") + b.toUpperCase());
                    rt.arrSetInPlace(out, name, rt.deref(v));
                }
            }
        }
        return out;
    });
    reg("apache_request_headers", function () { return rt.funcs.get("getallheaders").builtin(); });

    // Basic `pack` / `unpack` for common formats (H*, h*, C*, n, N, v, V, a*, A*)
    reg("pack", function (fmt) {
        fmt = rt.toStr(fmt);
        let out = "", ai = 1;
        for (let i = 0; i < fmt.length; i++) {
            const code = fmt[i];
            if (code === " " || code === "\t") continue;
            let rep = 1, star = false;
            if (fmt[i + 1] === "*") { star = true; i++; }
            else {
                let num = "";
                while (i + 1 < fmt.length && fmt[i + 1] >= "0" && fmt[i + 1] <= "9") num += fmt[++i];
                if (num) rep = Number(num);
            }
            if (code === "H" || code === "h") {
                const s = rt.toStr(arguments[ai++]);
                const lim = star ? s.length : Math.min(rep, s.length);
                for (let j = 0; j < lim; j += 2) {
                    const hi = parseInt(s[j] || "0", 16) & 0xF;
                    const lo = parseInt(s[j + 1] || "0", 16) & 0xF;
                    out += String.fromCharCode(code === "H" ? ((hi << 4) | lo) : ((lo << 4) | hi));
                }
            } else if (code === "a" || code === "A") {
                const s = rt.toStr(arguments[ai++]);
                const pad = code === "a" ? "\0" : " ";
                if (star) out += s;
                else out += s.length >= rep ? s.slice(0, rep) : s + pad.repeat(rep - s.length);
            } else if (code === "C" || code === "c") {
                const n = star ? (arguments.length - ai) : rep;
                for (let j = 0; j < n; j++) out += String.fromCharCode(Number(rt.toInt(arguments[ai++])) & 0xFF);
            } else if (code === "n" || code === "v") {
                const n = star ? (arguments.length - ai) : rep;
                for (let j = 0; j < n; j++) {
                    const v = Number(rt.toInt(arguments[ai++])) & 0xFFFF;
                    out += code === "n"
                        ? String.fromCharCode((v >> 8) & 0xFF, v & 0xFF)
                        : String.fromCharCode(v & 0xFF, (v >> 8) & 0xFF);
                }
            } else if (code === "N" || code === "V") {
                const n = star ? (arguments.length - ai) : rep;
                for (let j = 0; j < n; j++) {
                    const v = Number(BigInt.asUintN(32, rt.toInt(arguments[ai++])));
                    out += code === "N"
                        ? String.fromCharCode((v >>> 24) & 0xFF, (v >>> 16) & 0xFF, (v >>> 8) & 0xFF, v & 0xFF)
                        : String.fromCharCode(v & 0xFF, (v >>> 8) & 0xFF, (v >>> 16) & 0xFF, (v >>> 24) & 0xFF);
                }
            }
        }
        return out;
    });

    // ---- Lightweight `intl` shim (`Transliterator`, `Locale`, `MessageFormatter`) ----
    function applyTranslit(id, str) {
        let s = rt.bytesToUtf8(rt.toStr(str));
        const spec = rt.toStr(id || "");
        try { if (typeof s.normalize === "function") s = s.normalize("NFD").replace(/[\u0300-\u036f]/g, ""); } catch (_) {}
        if (/lower/i.test(spec)) s = s.toLowerCase();
        if (/upper/i.test(spec)) s = s.toUpperCase();
        if (/remove/i.test(spec)) s = s.replace(/[^\x00-\x7f]/g, "");
        return rt.utf8ToBytes(s);
    }
    let curLocale = "en_US";
    function fmtIcuMessage(pattern, args) {
        const map = new Map();
        if (args && args.__php === 1) {
            for (const [k, v] of args.m) map.set(String(k), rt.toStr(rt.deref(v)));
        }
        return rt.toStr(pattern).replace(/\{([^},]+)(?:,[^}]*)?\}/g, function (m, k) {
            const key = k.trim();
            return map.has(key) ? map.get(key) : m;
        });
    }
    rt.registerLazyClass(["transliterator", "locale", "messageformatter"], function () {
        const TransliteratorClass = R.defClass({
            name: "Transliterator", kind: "class",
            consts: [
                { name: "FORWARD", vis: 0, val: 0n },
                { name: "REVERSE", vis: 0, val: 1n },
            ],
            props: [{ name: "id", vis: 0, hasDefault: true, defaultVal: "" }],
            methods: [
                {
                    name: "create", vis: 0, isStatic: true,
                    builtin: function (id) {
                        const obj = rt.allocInstance(TransliteratorClass);
                        obj.props["id"] = rt.toStr(id);
                        return obj;
                    }
                },
                {
                    name: "createFromRules", vis: 0, isStatic: true,
                    builtin: function (rules) {
                        const obj = rt.allocInstance(TransliteratorClass);
                        obj.props["id"] = rt.toStr(rules);
                        return obj;
                    }
                },
                {
                    name: "transliterate", vis: 0,
                    builtin: function (str, start, end) {
                        let s = rt.toStr(str);
                        const st = start !== undefined ? Number(rt.toInt(start)) : 0;
                        const en = end !== undefined && Number(rt.toInt(end)) >= 0 ? Number(rt.toInt(end)) : s.length;
                        return s.slice(0, st) + applyTranslit(this.props["id"] || "", s.slice(st, en)) + s.slice(en);
                    }
                },
            ]
        });
        R.defClass({
            name: "Locale", kind: "class",
            methods: [
                { name: "getDefault", vis: 0, isStatic: true, builtin: function () { return curLocale; } },
                { name: "setDefault", vis: 0, isStatic: true, builtin: function (loc) { curLocale = rt.toStr(loc) || "en_US"; return true; } },
                { name: "canonicalize", vis: 0, isStatic: true, builtin: function (loc) { return rt.toStr(loc).replace(/-/g, "_"); } },
                {
                    name: "parseLocale", vis: 0, isStatic: true,
                    builtin: function (loc) {
                        const parts = rt.toStr(loc).replace(/-/g, "_").split("_");
                        const a = rt.makeArr();
                        if (parts[0]) rt.arrSetInPlace(a, "language", parts[0].toLowerCase());
                        if (parts[1]) rt.arrSetInPlace(a, "region", parts[1].toUpperCase());
                        return a;
                    }
                },
                { name: "getPrimaryLanguage", vis: 0, isStatic: true, builtin: function (loc) { return rt.toStr(loc).replace(/-/g, "_").split("_")[0].toLowerCase(); } },
                { name: "getRegion", vis: 0, isStatic: true, builtin: function (loc) { const p = rt.toStr(loc).replace(/-/g, "_").split("_"); return p[1] ? p[1].toUpperCase() : ""; } },
                { name: "acceptFromHttp", vis: 0, isStatic: true, builtin: function (hdr) { const first = rt.toStr(hdr).split(",")[0].split(";")[0].trim(); return first ? first.replace(/-/g, "_") : curLocale; } },
            ]
        });
        const MessageFormatterClass = R.defClass({
            name: "MessageFormatter", kind: "class",
            methods: [
                { name: "__construct", vis: 0, builtin: function (loc, pat) { this.__loc = rt.toStr(loc); this.__pat = rt.toStr(pat); } },
                { name: "create", vis: 0, isStatic: true, builtin: function (loc, pat) { return R.newObj(MessageFormatterClass, [loc, pat], null, null, null, false); } },
                { name: "format", vis: 0, builtin: function (args) { return fmtIcuMessage(this.__pat, args); } },
                { name: "formatMessage", vis: 0, isStatic: true, builtin: function (loc, pat, args) { return fmtIcuMessage(pat, args); } },
                { name: "getErrorCode", vis: 0, builtin: function () { return 0n; } },
                { name: "getErrorMessage", vis: 0, builtin: function () { return "U_ZERO_ERROR"; } },
            ]
        });
    });
    reg("transliterator_create", function (id) {
        const cls = rt.resolveClass("Transliterator", null, null, false);
        return cls.methods.get("create").builtin(id);
    });
    reg("transliterator_transliterate", function (translit, str, start, end) {
        translit = rt.deref(translit);
        const id = (translit && translit.__php === 2) ? (translit.props["id"] || "") : rt.toStr(translit);
        let s = rt.toStr(str);
        const st = start !== undefined ? Number(rt.toInt(start)) : 0;
        const en = end !== undefined && Number(rt.toInt(end)) >= 0 ? Number(rt.toInt(end)) : s.length;
        return s.slice(0, st) + applyTranslit(id, s.slice(st, en)) + s.slice(en);
    });
    reg("msgfmt_format_message", function (loc, pat, args) { return fmtIcuMessage(pat, args); });
})(__zipp_php);
