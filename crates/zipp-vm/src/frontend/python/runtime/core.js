/* ZIPP Python runtime — core object model. Apache-2.0.
 *
 * Trusted JavaScript compiled by ZIPP once per program. Guest Python is never
 * translated to JavaScript source; the Rust emitter lowers it to bytecode that
 * calls into the `__zipp_py` helper object defined here.
 *
 * Value model: int = BigInt, float = number, bool = boolean, str = string,
 * None = null. Everything else is a JS object with a `cls` field pointing at
 * its Python class object; instances keep attributes in `dict` (a Map).
 *
 * Code ABI: every Python code object is called as `code.call(fnobj, args)`
 * where `fnobj` is the Python function object (`this`) and `args` is the
 * array of bound positional values `bind` produced. The emitter calls
 * `bind`/`bindmethod` and then issues a direct VM call, so Python frames stay
 * on the VM's explicit frame stack.
 */
var __zipp_py_line = 0;
var __zipp_py_ui = [];
var __zipp_py_input = { mx: 0, my: 0, down: false, clicked: false, keys: {}, w: 640, h: 480 };
var __zipp_py = (function () {
    "use strict";
    const R = {};                       // the helper object handed to the emitter
    const rt = {};                      // internal API shared with the other runtime files
    R.__rt = rt;
    const STOP = { stop: true };
    const UNBOUND = { unbound: true };
    const NOTIMPL = { notimplemented: true };
    const ELLIPSIS = { ellipsis: true };
    R.STOP = STOP; R.UNBOUND = UNBOUND; R.NI = NOTIMPL; R.ELLIPSIS = ELLIPSIS;
    const MAX_ITEMS = 1 << 24, MAX_TEXT = 1 << 26;
    const files = [];                   // module index -> file name (for tracebacks)
    let nextId = 1;
    rt.STOP = STOP; rt.UNBOUND = UNBOUND; rt.NOTIMPL = NOTIMPL; rt.ELLIPSIS = ELLIPSIS;
    rt.MAX_ITEMS = MAX_ITEMS; rt.MAX_TEXT = MAX_TEXT;

    // ---- type objects -----------------------------------------------------------------------
    // A class is `{cls: TypeType, name, qualname, module, bases, mro, dict: Map, id}`.
    // Built-in value types (int, str, list, ...) are class objects too; their
    // `dict` holds builtin functions taking `(self, ...args)` as one array.
    // Direct entry points for the emitter's positional-call path (`fast`):
    // called as `f.fast(args)` with `this` = the callable.
    function typeFast(args) { return this.code([this, args, null]); }
    function builtinFast(args) { return this.code(builtinArgs(this, args, null)); }
    function makeType(name, bases, dict, module) {
        // `code` makes a class callable through the same member-call path as
        // functions (`prepare` returns self = the class, code = constructCode).
        const t = { cls: null, name: name, qualname: name, module: module || "builtins",
            bases: bases, mro: null, dict: dict || new Map(), id: nextId++, isType: true, code: constructCode, fast: typeFast };
        t.mro = computeMro(t);
        return t;
    }
    // C3 linearisation, as CPython.
    function computeMro(t) {
        const seqs = t.bases.map((b) => b.mro.slice());
        seqs.push(t.bases.slice());
        const result = [t];
        for (;;) {
            const live = seqs.filter((s) => s.length > 0);
            if (live.length === 0) return result;
            let head = null;
            for (const s of live) {
                const cand = s[0];
                if (!live.some((o) => o.indexOf(cand) > 0)) { head = cand; break; }
            }
            if (head === null) fail(TypeError, "Cannot create a consistent method resolution order (MRO)");
            result.push(head);
            for (const s of live) if (s[0] === head) s.shift();
        }
    }
    const TypeType = makeType("type", [], new Map());
    TypeType.cls = TypeType;
    const ObjectType = makeType("object", [], new Map());
    ObjectType.cls = TypeType;
    TypeType.bases = [ObjectType]; TypeType.mro = [TypeType, ObjectType];
    function newType(name, bases, dict, module) {
        const t = makeType(name, bases.length ? bases : [ObjectType], dict, module);
        t.cls = TypeType;
        return t;
    }
    rt.TypeType = TypeType; rt.ObjectType = ObjectType; rt.newType = newType; rt.computeMro = computeMro;
    const T = {};                        // builtin type objects by name
    rt.T = T;
    for (const name of ["NoneType", "bool", "int", "float", "str", "list", "tuple", "dict", "set",
        "frozenset", "range", "function", "builtin_function_or_method", "method", "module",
        "generator", "NotImplementedType", "ellipsis", "slice", "property", "staticmethod",
        "classmethod", "super", "cell", "bytes", "list_iterator", "dict_keys", "dict_values",
        "dict_items", "enumerate", "zip", "map", "filter", "reversed", "iterator", "code"]) {
        T[name] = newType(name, [], new Map());
    }
    T.bool.bases = [T.int]; T.bool.mro = [T.bool, T.int, ObjectType];
    function typeOf(v) {
        const t = typeof v;
        if (t === "object") return v === null ? T.NoneType : (v.cls || ObjectType);
        if (t === "bigint") return T.int;
        if (t === "string") return T.str;
        if (t === "number") return T.float;
        if (t === "boolean") return T.bool;
        if (t === "function") return T.builtin_function_or_method;
        return T.NoneType;
    }
    rt.typeOf = typeOf;
    function isInstance(v, t) {
        return typeOf(v).mro.indexOf(t) >= 0;
    }
    function isSubclass(a, b) { return a.mro.indexOf(b) >= 0; }
    rt.isInstance = isInstance; rt.isSubclass = isSubclass;
    function isType(v) { return v !== null && typeof v === "object" && v.isType === true; }
    rt.isType = isType;
    function ident(v) {
        if (v !== null && typeof v === "object") { if (!v.id) v.id = nextId++; return v.id; }
        return 0;
    }
    rt.ident = ident;

    // ---- exceptions ---------------------------------------------------------------------------
    // Exception instances are ordinary objects carrying `args` plus `name` and
    // `message` data properties so an uncaught one prints as "Name: message"
    // at the host boundary.
    const E = {};
    rt.E = E;
    function defExc(name, base) { E[name] = newType(name, [base || E.Exception], new Map()); return E[name]; }
    E.BaseException = newType("BaseException", [ObjectType], new Map());
    defExc("Exception", E.BaseException);
    defExc("SystemExit", E.BaseException); defExc("KeyboardInterrupt", E.BaseException); defExc("GeneratorExit", E.BaseException);
    for (const n of ["ArithmeticError", "AssertionError", "AttributeError", "EOFError", "ImportError", "LookupError",
        "MemoryError", "NameError", "OSError", "RuntimeError", "StopIteration", "SyntaxError", "TypeError",
        "ValueError", "Warning", "BufferError", "ReferenceError"]) defExc(n);
    defExc("ModuleNotFoundError", E.ImportError); defExc("IndexError", E.LookupError); defExc("KeyError", E.LookupError);
    defExc("UnboundLocalError", E.NameError); defExc("NotImplementedError", E.RuntimeError); defExc("RecursionError", E.RuntimeError);
    defExc("OverflowError", E.ArithmeticError); defExc("ZeroDivisionError", E.ArithmeticError); defExc("FloatingPointError", E.ArithmeticError);
    defExc("FileNotFoundError", E.OSError); defExc("PermissionError", E.OSError); defExc("TimeoutError", E.OSError);
    defExc("UnicodeError", E.ValueError); defExc("UnicodeDecodeError", E.UnicodeError); defExc("UnicodeEncodeError", E.UnicodeError);
    defExc("IndentationError", E.SyntaxError); defExc("DeprecationWarning", E.Warning); defExc("UserWarning", E.Warning);
    defExc("StopAsyncIteration"); defExc("JSONDecodeError", E.ValueError);
    defExc("BaseExceptionGroup", E.BaseException); defExc("ExceptionGroup", E.BaseExceptionGroup);
    E.ExceptionGroup.bases = [E.BaseExceptionGroup, E.Exception]; E.ExceptionGroup.mro = computeMro(E.ExceptionGroup);
    function excMessage(e) {
        const a = e.args;
        if (!a || a.items.length === 0) return "";
        if (a.items.length === 1) return e.cls === E.KeyError ? repr(a.items[0]) : str(a.items[0]);
        return repr(a);
    }
    function locationText() {
        const enc = __zipp_py_line;
        if (!enc) return "";
        const file = files[Math.floor(enc / 1000000)] || "?";
        return " (" + file + ":" + (enc % 1000000) + ")";
    }
    function makeExc(cls, args) {
        const e = { cls: cls, dict: new Map(), args: sequence(T.tuple, args), cause: null, context: null,
            traceback: locationText(), suppress: false };
        refreshExc(e);
        return e;
    }
    function refreshExc(e) {
        // Host-visible: the VM prints `name: message` for an uncaught throw.
        e.name = e.cls.name;
        e.message = excMessage(e) + (e.traceback || "");
    }
    rt.makeExc = makeExc; rt.refreshExc = refreshExc;
    function fail(cls, message) { throw makeExc(cls, message === undefined ? [] : [message]); }
    rt.fail = fail;
    const TypeError = E.TypeError, ValueError = E.ValueError;
    // Errors thrown by the VM itself (a JS TypeError from a helper bug, the
    // call-stack RangeError) become Python exceptions on the way to a handler.
    function normexc(e) {
        if (e !== null && typeof e === "object" && e.cls && isSubclass(e.cls, E.BaseException)) return e;
        if (e instanceof RangeError && /call stack/i.test(String(e.message))) {
            return makeExc(E.RecursionError, ["maximum recursion depth exceeded"]);
        }
        if (e instanceof Error) {
            const kind = e.name === "RangeError" ? E.OverflowError : e.name === "TypeError" ? E.TypeError : E.RuntimeError;
            return makeExc(kind, [String(e.message)]);
        }
        return makeExc(E.RuntimeError, [String(e)]);
    }
    R.normexc = normexc; rt.normexc = normexc;
    // Frame guards (one per function and module body) call this while an
    // exception propagates: it records the frame, innermost first.
    // Innermost first; a runaway recursion keeps the innermost frames and
    // the outermost ones (the rest are counted).
    const KEEP_INNER = 1000, KEEP_OUTER = 12;
    R.addframe = function (e, f, line) {
        const exc = normexc(e);
        let frames = exc.frames;
        if (frames === undefined) frames = exc.frames = [];
        const enc = typeof line === "number" ? line : 0;
        const name = f !== null && typeof f === "object" && typeof f.name === "string" ? f.name : "<module>";
        const frame = { file: files[Math.floor(enc / 1000000)] || "?", line: enc % 1000000, name: name };
        if (frames.length < KEEP_INNER) frames.push(frame);
        else {
            let outer = exc.outerFrames;
            if (outer === undefined) outer = exc.outerFrames = [];
            outer.push(frame);
            if (outer.length > KEEP_OUTER) { outer.shift(); exc.dropped = (exc.dropped || 0) + 1; }
        }
        return exc;
    };
    rt.tracebackText = function (exc) {
        const frames = exc.frames;
        if (frames === undefined || frames.length === 0) return "";
        const line = (fr) => "  File \"" + fr.file + "\", line " + fr.line + ", in " + fr.name + "\n";
        let out = "Traceback (most recent call last):\n";
        const outer = exc.outerFrames || [];
        for (let i = outer.length - 1; i >= 0; i--) out += line(outer[i]);
        if (exc.dropped) out += "  [" + exc.dropped + " more frame" + (exc.dropped === 1 ? "" : "s") + "]\n";
        // A long run of one repeated frame prints once with a count.
        let i = frames.length - 1;
        while (i >= 0) {
            const fr = frames[i];
            let j = i;
            while (j > 0 && frames[j - 1].file === fr.file && frames[j - 1].line === fr.line && frames[j - 1].name === fr.name) j--;
            const repeats = i - j + 1;
            out += line(fr);
            if (repeats > 3) out += "  [Previous line repeated " + (repeats - 1) + " more times]\n";
            else for (let k = 1; k < repeats; k++) out += line(fr);
            i = j - 1;
        }
        return out;
    };
    const excStack = [];
    R.pushexc = function (e) { excStack.push(e); return null; };
    R.popexc = function () { excStack.pop(); return null; };
    rt.currentExc = function () { return excStack.length ? excStack[excStack.length - 1] : null; };
    R.excmatch = function (e, spec) {
        if (isType(spec)) return isInstance(e, spec);
        if (spec !== null && typeof spec === "object" && spec.cls === T.tuple) {
            for (const t of spec.items) if (isType(t) && isInstance(e, t)) return true;
            return false;
        }
        fail(TypeError, "catching classes that do not inherit from BaseException is not allowed");
    };
    R.raise = function (exc, cause) {
        if (exc === null) {
            const cur = rt.currentExc();
            if (cur === null) fail(E.RuntimeError, "No active exception to reraise");
            throw cur;
        }
        let e = exc;
        if (isType(exc)) {
            if (!isSubclass(exc, E.BaseException)) fail(TypeError, "exceptions must derive from BaseException");
            e = rt.call(exc, [], null);
        } else if (!(exc !== null && typeof exc === "object" && exc.cls && isSubclass(exc.cls, E.BaseException))) {
            fail(TypeError, "exceptions must derive from BaseException");
        }
        if (cause !== undefined) {
            e.cause = cause === null ? null : isType(cause) ? rt.call(cause, [], null) : cause;
            e.suppress = true;
        }
        const cur = rt.currentExc();
        if (cur !== null && cur !== e) e.context = cur;
        e.traceback = locationText();
        refreshExc(e);
        throw e;
    };
    R.assertfail = function (msg) { throw makeExc(E.AssertionError, msg === null ? [] : [msg]); };
    R.unboundlocal = function (name) {
        fail(E.UnboundLocalError, "cannot access local variable '" + name + "' where it is not associated with a value");
    };

    // ---- sequences and the primitive helpers the other files share -------------------------------
    function sequence(type, items) {
        if (items.length > MAX_ITEMS) fail(E.MemoryError, "sequence limit exceeded");
        return { cls: type, items: items };
    }
    rt.sequence = sequence;
    function list(items) { return sequence(T.list, items); }
    function tuple(items) { return sequence(T.tuple, items); }
    rt.list = list; rt.tuple = tuple;
    // The emitter's literal builders; a comprehension starts from an empty one.
    R.list = function (items) { return sequence(T.list, items === undefined ? [] : items); };
    R.tuple = function (items) { return sequence(T.tuple, items === undefined ? [] : items); };
    function isSeq(v, t) { return v !== null && typeof v === "object" && v.cls === t; }
    rt.isList = (v) => isSeq(v, T.list); rt.isTuple = (v) => isSeq(v, T.tuple);
    function checkedText(s) { if (s.length > MAX_TEXT) fail(E.MemoryError, "string limit exceeded"); return s; }
    rt.checkedText = checkedText;

    // ---- attribute protocol -------------------------------------------------------------------
    // MRO lookups are cached per type. Any class-dict mutation bumps the
    // epoch, which drops every cache (a base-class change affects subclasses).
    let typeEpoch = 1;
    const MISSING_ATTR = { missing: true };
    rt.bumpEpoch = function () { typeEpoch++; };
    function lookupType(t, name) {
        let cache = t.lcache;
        if (cache === undefined || t.lepoch !== typeEpoch) { cache = t.lcache = new Map(); t.lepoch = typeEpoch; }
        const hit = cache.get(name);
        if (hit !== undefined) return hit === MISSING_ATTR ? undefined : hit;
        let found;
        for (const c of t.mro) { const v = c.dict.get(name); if (v !== undefined) { found = v; break; } }
        cache.set(name, found === undefined ? MISSING_ATTR : found);
        return found;
    }
    rt.lookupType = lookupType;
    function isFunction(v) { return v !== null && typeof v === "object" && (v.cls === T.function || v.cls === T.builtin_function_or_method); }
    rt.isFunction = isFunction;
    function bound(func, self) { return { cls: T.method, func: func, self: self }; }
    rt.bound = bound;
    // Descriptor GET of a class attribute found for `obj` (an instance) or for
    // the class itself when `obj` is null.
    function descrGet(attr, obj, owner) {
        if (attr === null || typeof attr !== "object") return attr;
        const c = attr.cls;
        if (c === T.function || c === T.builtin_function_or_method) return obj === null ? attr : bound(attr, obj);
        if (c === T.classmethod) return bound(attr.func, owner);
        if (c === T.staticmethod) return attr.func;
        if (c === T.property) {
            if (obj === null) return attr;
            if (attr.fget === null) fail(E.AttributeError, "property has no getter");
            return rt.call(attr.fget, [obj], null);
        }
        // A user-defined descriptor: __get__ on its type (a class-valued
        // attribute is not itself looked up on `type`).
        const get = attr.isType ? undefined : lookupType(c, "__get__");
        if (get !== undefined) return rt.call(get, [attr, obj === null ? null : obj, owner], null);
        return attr;
    }
    function isDataDescriptor(attr) {
        if (attr === null || typeof attr !== "object") return false;
        if (attr.cls === T.property) return true;
        const c = attr.cls;
        return c !== undefined && c !== T.function && lookupType(c, "__set__") !== undefined;
    }
    function typeAttr(t, name) {
        // Attributes of a class object: its own MRO, then the metaclass (type).
        if (name === "__name__") return t.name;
        if (name === "__qualname__") return t.qualname;
        if (name === "__module__") return t.module;
        if (name === "__mro__") return tuple(t.mro.slice());
        if (name === "__bases__") return tuple(t.bases.slice());
        if (name === "__dict__") return rt.mappingProxy(t.dict);
        if (name === "__class__") return t.cls;
        if (name === "__doc__") { const d = t.dict.get("__doc__"); return d === undefined ? null : d; }
        const attr = lookupType(t, name);
        if (attr !== undefined) return descrGet(attr, null, t);
        const meta = lookupType(t.cls, name);
        if (meta !== undefined) return descrGet(meta, t, t.cls);
        fail(E.AttributeError, "type object '" + t.name + "' has no attribute '" + name + "'");
    }
    function getattr(obj, name) {
        let t;
        if (obj !== null && typeof obj === "object") {
            if (obj.isType) return typeAttr(obj, name);
            t = obj.cls || ObjectType;
            if (t === T.module) return rt.moduleAttr(obj, name);
            if (t === T.super) return rt.superAttr(obj, name);
            if (t === TypeType) return typeAttr(obj, name);
        } else {
            t = typeOf(obj);
        }
        if (name === "__class__") return t;
        const attr = lookupType(t, name);
        if (attr !== undefined && isDataDescriptor(attr)) return descrGet(attr, obj, t);
        if (obj !== null && typeof obj === "object" && obj.dict !== undefined) {
            const v = obj.dict.get(name);
            if (v !== undefined) return v;
            if (name === "__dict__") return rt.instanceDict(obj);
        }
        if (attr !== undefined) return descrGet(attr, obj, t);
        const special = rt.specialAttr(obj, t, name);
        if (special !== undefined) return special;
        const ga = lookupType(t, "__getattr__");
        if (ga !== undefined) return rt.call(ga, [obj, name], null);
        if (obj !== null && typeof obj === "object" && obj.dict !== undefined) {
            fail(E.AttributeError, "'" + t.name + "' object has no attribute '" + name + "'");
        }
        fail(E.AttributeError, "'" + t.name + "' object has no attribute '" + name + "'");
    }
    function setattr(obj, name, value) {
        if (obj !== null && typeof obj === "object" && obj.isType) {
            if (name === "__name__") { obj.name = str(value); return null; }
            obj.dict.set(name, value); typeEpoch++; return null;
        }
        const t = typeOf(obj);
        if (obj === null || typeof obj !== "object" || obj.dict === undefined) {
            if (obj !== null && typeof obj === "object" && obj.cls === T.module) { obj.globals.set(name, value); return null; }
            fail(E.AttributeError, "'" + t.name + "' object has no attribute '" + name + "'");
        }
        const attr = lookupType(t, name);
        if (attr !== undefined && attr !== null && typeof attr === "object") {
            if (attr.cls === T.property) {
                if (attr.fset === null) fail(E.AttributeError, "property '" + name + "' of '" + t.name + "' object has no setter");
                rt.call(attr.fset, [obj, value], null); return null;
            }
            const set = attr.isType ? undefined : lookupType(attr.cls, "__set__");
            if (set !== undefined && attr.cls !== T.function) { rt.call(set, [attr, obj, value], null); return null; }
        }
        const sa = lookupType(t, "__setattr__");
        if (sa !== undefined && sa !== ObjectType.dict.get("__setattr__")) { rt.call(sa, [obj, name, value], null); return null; }
        if (t.noDict && !t.slots.has(name)) fail(E.AttributeError, "'" + t.name + "' object has no attribute '" + name + "' and no __dict__ for setting new attributes");
        obj.dict.set(name, value);
        return null;
    }
    function delattr(obj, name) {
        if (obj !== null && typeof obj === "object" && obj.isType) {
            if (!obj.dict.delete(name)) fail(E.AttributeError, name);
            typeEpoch++;
            return null;
        }
        const t = typeOf(obj);
        if (obj === null || typeof obj !== "object" || obj.dict === undefined) fail(E.AttributeError, "'" + t.name + "' object has no attribute '" + name + "'");
        const attr = lookupType(t, name);
        if (attr !== undefined && attr !== null && typeof attr === "object" && attr.cls === T.property) {
            if (attr.fdel === null) fail(E.AttributeError, "property has no deleter");
            rt.call(attr.fdel, [obj], null); return null;
        }
        const da = lookupType(t, "__delattr__");
        if (da !== undefined && da !== ObjectType.dict.get("__delattr__")) { rt.call(da, [obj, name], null); return null; }
        if (!obj.dict.delete(name)) fail(E.AttributeError, "'" + t.name + "' object has no attribute '" + name + "'");
        return null;
    }
    R.getattr = getattr; R.setattr = setattr; R.delattr = delattr;
    rt.getattr = getattr; rt.setattr = setattr; rt.descrGet = descrGet;
    // Attribute lookup that skips the instance dict: for dunder dispatch.
    function typeMethod(obj, name) {
        return lookupType(typeOf(obj), name);
    }
    rt.typeMethod = typeMethod;
    function callMethod(obj, name, args) {
        const m = typeMethod(obj, name);
        if (m === undefined) return undefined;
        return rt.call(descrGet(m, obj, typeOf(obj)), args, null);
    }
    rt.callMethod = callMethod;

    // ---- functions, binding and calls ----------------------------------------------------------
    R.func = function (code, name, qualname, argnames, posonly, positional, varargs, varkw,
                       defaults, kwdefNames, kwdefValues, cells, globals, isgen, doc, module) {
        const f = { cls: T.function, code: code, name: name, qualname: qualname, argnames: argnames,
            posonly: posonly, positional: positional, varargs: varargs, varkw: varkw,
            defaults: defaults, kwdefaults: null, cells: cells, globals: globals, isgen: isgen,
            doc: doc, module: module, dict: new Map(), simple: false };
        if (kwdefNames.length) { f.kwdefaults = new Map(); for (let i = 0; i < kwdefNames.length; i++) f.kwdefaults.set(kwdefNames[i], kwdefValues[i]); }
        f.simple = !varargs && !varkw && argnames.length === positional && defaults.length === 0;
        if (isgen) {
            // Called as `f.code(args)` (a member call, `this` = f): creating
            // the generator goes through `this.real`, never `Function.call`,
            // which would re-enter the interpreter natively.
            f.real = code;
            f.code = function (args) { return rt.makeGenerator(this.real(args), f); };
        } else if (f.simple) {
            // The body starts with its own argument-count guard.
            f.fast = code;
        }
        return f;
    };
    R.arity = function (f, args) { arityError(f, args.length); };
    R.fannotate = function (f, names, values) { const m = new Map(); for (let i = 0; i < names.length; i++) m.set(names[i], values[i]); f.annotations = m; return null; };
    // A builtin: `code(args)` with `this` unused; `arity` -1 for variadic.
    function builtin(name, arity, code, minArity) {
        return { cls: T.builtin_function_or_method, name: name, qualname: name, arity: arity,
            minArity: minArity === undefined ? arity : minArity, code: code, module: "builtins", dict: null, fast: builtinFast };
    }
    rt.builtin = builtin;
    // Keyword records: a Map from name to value (null when absent).
    R.kwnew = function () { return new Map(); };
    R.kwset = function (kw, name, value) {
        if (kw.has(name)) fail(TypeError, "keyword argument repeated: " + name);
        kw.set(name, value); return kw;
    };
    R.kwmerge = function (kw, mapping) {
        const d = rt.asDict(mapping, "argument after ** must be a mapping");
        for (const [k, v] of rt.dictEntries(d)) {
            if (typeof k !== "string") fail(TypeError, "keywords must be strings");
            if (kw.has(k)) fail(TypeError, "got multiple values for keyword argument '" + k + "'");
            kw.set(k, v);
        }
        return kw;
    };
    R.extend = function (arr, iterable) {
        const it = iter(iterable);
        for (;;) { const v = fornext(it); if (v === STOP) break; arr.push(v); }
        return arr;
    };
    R.append = function (arr, v) { arr.push(v); return arr; };
    // Bind `args`/`kwargs` to a Python function's signature: the bound array.
    function bindArgs(f, args, kwargs) {
        if (f.simple && (kwargs === null || kwargs.size === 0)) {
            if (args.length !== f.positional) arityError(f, args.length);
            return args;
        }
        const names = f.argnames, npos = f.positional, nall = names.length;
        const out = new Array(nall);
        const n = Math.min(args.length, npos);
        for (let i = 0; i < n; i++) out[i] = args[i];
        let extra = null;
        if (args.length > npos) {
            if (!f.varargs) fail(TypeError, f.name + "() takes " + npos + " positional argument" + (npos === 1 ? "" : "s") + " but " + args.length + (kwargs && kwargs.size ? " positional argument" + (args.length === 1 ? "" : "s") + " (and " + kwargs.size + " keyword-only argument" + (kwargs.size === 1 ? "" : "s") + ")" : "") + (args.length === 1 && !(kwargs && kwargs.size) ? " was" : " were") + " given");
            extra = args.slice(npos);
        }
        let kwrest = null;
        if (kwargs !== null && kwargs.size) {
            for (const [k, v] of kwargs) {
                const idx = names.indexOf(k);
                if (idx >= 0 && idx >= f.posonly) {
                    if (out[idx] !== undefined) fail(TypeError, f.name + "() got multiple values for argument '" + k + "'");
                    out[idx] = v;
                } else {
                    if (!f.varkw) fail(TypeError, f.name + "() got an unexpected keyword argument '" + k + "'");
                    if (kwrest === null) kwrest = rt.dict();
                    rt.dictSet(kwrest, k, v);
                }
            }
        }
        const firstDefault = npos - f.defaults.length;
        const missing = [], missingKw = [];
        for (let i = 0; i < nall; i++) {
            if (out[i] !== undefined) continue;
            if (i < npos) {
                if (i >= firstDefault) out[i] = f.defaults[i - firstDefault];
                else missing.push(names[i]);
            } else {
                const d = f.kwdefaults === null ? undefined : f.kwdefaults.get(names[i]);
                if (d !== undefined) out[i] = d; else missingKw.push(names[i]);
            }
        }
        if (missing.length) fail(TypeError, f.name + "() missing " + missing.length + " required positional argument" + (missing.length === 1 ? "" : "s") + ": " + listNames(missing));
        if (missingKw.length) fail(TypeError, f.name + "() missing " + missingKw.length + " required keyword-only argument" + (missingKw.length === 1 ? "" : "s") + ": " + listNames(missingKw));
        if (f.varargs) out.push(tuple(extra || []));
        if (f.varkw) out.push(kwrest || rt.dict());
        return out;
    }
    function arityError(f, got) {
        if (got < f.positional) {
            const missing = f.argnames.slice(got, f.positional);
            fail(TypeError, f.name + "() missing " + missing.length + " required positional argument" + (missing.length === 1 ? "" : "s") + ": " + listNames(missing));
        }
        fail(TypeError, f.name + "() takes " + f.positional + " positional argument" + (f.positional === 1 ? "" : "s") + " but " + got + " " + (got === 1 ? "was" : "were") + " given");
    }
    const listNames = (m) => m.length === 1 ? "'" + m[0] + "'" : m.length === 2 ? "'" + m[0] + "' and '" + m[1] + "'" : m.slice(0, -1).map((x) => "'" + x + "'").join(", ") + ", and '" + m[m.length - 1] + "'";
    function builtinArgs(f, args, kwargs) {
        if (kwargs !== null && kwargs.size) {
            if (!f.kwnames) fail(TypeError, f.name + "() takes no keyword arguments");
            // A builtin that accepts keywords takes them as a trailing Map.
            args = args.slice(); args.push(kwargs);
            return args;
        }
        if (f.arity >= 0 && (args.length > f.arity || args.length < f.minArity)) {
            fail(TypeError, f.name + "() takes " + (f.minArity === f.arity ? "exactly " + (f.arity === 1 ? "one" : f.arity) : "from " + f.minArity + " to " + f.arity) + " argument" + (f.arity === 1 ? "" : "s") + " (" + args.length + " given)");
        }
        return args;
    }
    // The prepared-call record the emitter calls through. ONE scratch object:
    // the emitter (and `call` below) read `code`, `self` and `args` before
    // any other code can run, so no allocation per call is needed.
    const PREP = { code: null, self: null, args: null };
    function prepared(code, self, args) { PREP.code = code; PREP.self = self; PREP.args = args; return PREP; }
    function prepare(f, args, kwargs) {
        if (f !== null && typeof f === "object") {
            const c = f.cls;
            if (c === T.function) return prepared(f.code, f, bindArgs(f, args, kwargs));
            if (c === T.builtin_function_or_method) return prepared(f.code, f, builtinArgs(f, args, kwargs));
            if (c === T.method) {
                const withSelf = [f.self]; for (let i = 0; i < args.length; i++) withSelf.push(args[i]);
                return prepare(f.func, withSelf, kwargs);
            }
            if (f.isType) return prepared(constructCode, f, [f, args, kwargs]);
            const call = typeMethod(f, "__call__");
            if (call !== undefined) {
                const withSelf = [f]; for (let i = 0; i < args.length; i++) withSelf.push(args[i]);
                return prepare(call, withSelf, kwargs);
            }
        }
        fail(TypeError, "'" + typeOf(f).name + "' object is not callable");
    }
    R.bind = prepare;
    // `obj.name(args)` resolved and called in one step. The callee runs as
    // a member call (`f.code(args)`), a VM frame below this one; the emitter
    // keeps a `bindmethod` + direct-call form for callers that need the
    // callee's frame to sit directly on their own.
    R.callmethod = function (obj, name, args, kwargs) {
        let t;
        if (obj !== null && typeof obj === "object") {
            if (obj.isType || obj.cls === T.module || obj.cls === T.super) return call(getattr(obj, name), args, kwargs);
            t = obj.cls || ObjectType;
        } else {
            t = typeOf(obj);
        }
        const attr = lookupType(t, name);
        if (attr !== undefined && attr !== null && typeof attr === "object") {
            const ac = attr.cls;
            if (ac === T.function || ac === T.builtin_function_or_method) {
                const inst = obj !== null && typeof obj === "object" && obj.dict !== undefined ? obj.dict.get(name) : undefined;
                if (inst === undefined) {
                    const withSelf = [obj]; for (let i = 0; i < args.length; i++) withSelf.push(args[i]);
                    if (ac === T.function) {
                        if (attr.fast !== undefined && kwargs === null) return attr.fast(withSelf);
                        return attr.code(bindArgs(attr, withSelf, kwargs));
                    }
                    return attr.code(builtinArgs(attr, withSelf, kwargs));
                }
            }
        }
        return call(getattr(obj, name), args, kwargs);
    };
    R.callv = function (f, args, kwargs) { return call(f, args, kwargs); };
    R.bindmethod = function (obj, name, args, kwargs) {
        // obj.name(args): resolve like getattr but avoid allocating a bound
        // method when the attribute is a plain or builtin function on the type.
        let t;
        if (obj !== null && typeof obj === "object") {
            if (obj.isType || obj.cls === T.module || obj.cls === T.super) return prepare(getattr(obj, name), args, kwargs);
            t = obj.cls || ObjectType;
        } else {
            t = typeOf(obj);
        }
        const attr = lookupType(t, name);
        if (attr !== undefined && attr !== null && typeof attr === "object") {
            const ac = attr.cls;
            if (ac === T.function || ac === T.builtin_function_or_method) {
                const inst = obj !== null && typeof obj === "object" && obj.dict !== undefined ? obj.dict.get(name) : undefined;
                if (inst === undefined) {
                    const withSelf = [obj]; for (let i = 0; i < args.length; i++) withSelf.push(args[i]);
                    if (ac === T.function) {
                        // A plain-positional function validates the count itself.
                        if (attr.fast !== undefined && kwargs === null) return prepared(attr.fast, attr, withSelf);
                        return prepared(attr.code, attr, bindArgs(attr, withSelf, kwargs));
                    }
                    return prepared(attr.code, attr, builtinArgs(attr, withSelf, kwargs));
                }
            }
        }
        return prepare(getattr(obj, name), args, kwargs);
    };
    // Calling from JS (helpers, dunder dispatch). `prepare` guarantees that
    // `p.self.code === p.code`, so this is a member call the VM runs on its
    // own frame stack — never `Function.prototype.call`, which re-enters the
    // interpreter natively (and the wasm build allows only one such level).
    function call(f, args, kwargs) {
        const p = prepare(f, args, kwargs === undefined ? null : kwargs);
        return p.self.code(p.args);
    }
    rt.call = call;
    // `Class(args)`: __new__/__init__.
    function constructCode(payload) {
        const cls = payload[0], args = payload[1], kwargs = payload[2];
        return construct(cls, args, kwargs);
    }
    function construct(cls, args, kwargs) {
        // A metaclass with its own __call__ takes over instance creation.
        const meta = cls.cls;
        if (meta !== null && meta !== TypeType && meta !== undefined) {
            const mc = lookupType(meta, "__call__");
            if (mc !== undefined && mc !== TypeType.dict.get("__call__")) return call(descrGet(mc, cls, meta), args, kwargs);
        }
        return constructDefault(cls, args, kwargs);
    }
    rt.constructDefault = constructDefault;
    function abstractNames(cls) {
        const seen = new Set(), abstract = [];
        for (const c of cls.mro) {
            for (const [k, v] of c.dict) {
                if (seen.has(k)) continue;
                seen.add(k);
                const f = v !== null && typeof v === "object" ? (v.cls === T.staticmethod || v.cls === T.classmethod ? v.func : v.cls === T.property ? v.fget : v) : v;
                if (f !== null && typeof f === "object" && f.isabstract) abstract.push(k);
            }
        }
        return abstract.sort();
    }
    function constructDefault(cls, args, kwargs) {
        if (cls === TypeType) {
            if (args.length === 1 && (kwargs === null || kwargs.size === 0)) return typeOf(args[0]);
            if (args.length === 3) return rt.makeClass(TypeType, args[0], args[1], args[2], kwargs);
            fail(TypeError, "type() takes 1 or 3 arguments");
        }
        if (cls.isABC) {
            const names = abstractNames(cls);
            if (names.length) fail(TypeError, "Can't instantiate abstract class " + cls.name + " without an implementation for abstract method" + (names.length === 1 ? "" : "s") + " " + names.map((n) => "'" + n + "'").join(", "));
        }
        const ctor = rt.constructors.get(cls);
        if (ctor !== undefined) return ctor(args, kwargs, cls);
        const newf = lookupType(cls, "__new__");
        let obj;
        if (newf !== undefined && newf !== ObjectType.dict.get("__new__")) {
            const withCls = [cls]; for (let i = 0; i < args.length; i++) withCls.push(args[i]);
            obj = call(newf.cls === T.staticmethod ? newf.func : newf, withCls, kwargs);
            if (!isInstance(obj, cls)) return obj;
        } else {
            obj = rt.allocInstance(cls);
        }
        const init = lookupType(cls, "__init__");
        if (init !== undefined && init !== ObjectType.dict.get("__init__")) {
            const withSelf = [obj]; for (let i = 0; i < args.length; i++) withSelf.push(args[i]);
            const r = call(init, withSelf, kwargs);
            if (r !== null) fail(TypeError, "__init__() should return None, not '" + typeOf(r).name + "'");
        } else if (args.length || (kwargs !== null && kwargs.size)) {
            if (newf === undefined || newf === ObjectType.dict.get("__new__")) fail(TypeError, cls.name + "() takes no arguments");
        }
        return obj;
    }
    rt.construct = construct;
    rt.constructors = new Map();          // builtin type -> (args, kwargs, cls) => value
    rt.allocInstance = function (cls) {
        // A subclass of a builtin container inherits its storage.
        for (const c of cls.mro) {
            const alloc = rt.allocators.get(c);
            if (alloc !== undefined) { const o = alloc(cls); o.dict = new Map(); return o; }
        }
        return { cls: cls, dict: new Map() };
    };
    rt.allocators = new Map();

    // ---- names -------------------------------------------------------------------------------
    R.cell = function (v) { return { v: v }; };
    R.gload = function (g, name) {
        const v = g.get(name);
        if (v !== undefined) return v;
        const b = rt.builtins.get(name);
        if (b !== undefined) return b;
        if (name === "__builtins__") return R.import("builtins", g);
        fail(E.NameError, "name '" + name + "' is not defined");
    };
    R.gstore = function (g, name, v) { g.set(name, v); return null; };
    R.gdel = function (g, name) { if (!g.delete(name)) fail(E.NameError, "name '" + name + "' is not defined"); return null; };
    R.newns = function () { return new Map(); };
    R.nsinit = function (ns, name, qualname, module) { ns.set("__module__", module); ns.set("__qualname__", qualname); return null; };
    R.nsload = function (ns, g, name) {
        const v = ns.get(name);
        if (v !== undefined) return v;
        return R.gload(g, name);
    };
    R.nsstore = function (ns, name, v) { ns.set(name, v); return null; };
    R.annotate = function (container, name, value) {
        let ann = container.get("__annotations__");
        if (ann === undefined) { ann = rt.dict(); container.set("__annotations__", ann); }
        rt.dictSet(ann, name, value);
        return null;
    };
    R.nsdel = function (ns, name) { if (!ns.delete(name)) fail(E.NameError, name); return null; };

    // ---- classes -----------------------------------------------------------------------------------
    // `type.__new__(metaclass, name, bases, namespace, **kw)`: the class
    // object itself. `ns` is the body's Map or a dict; `kw` are the class
    // keywords, handed on to `__init_subclass__`.
    rt.makeClass = function (metaclass, name, bases, ns, kw) {
        if (typeof name !== "string") fail(TypeError, "type.__new__() argument 1 must be str, not " + typeOf(name).name);
        if (bases !== null && typeof bases === "object" && bases.items !== undefined) bases = bases.items;
        else if (!Array.isArray(bases)) fail(TypeError, "type.__new__() argument 2 must be tuple, not " + typeOf(bases).name);
        if (!(ns instanceof Map)) ns = rt.mapFromDict(rt.asDict(ns, "type.__new__() argument 3 must be dict, not " + typeOf(ns).name));
        for (const b of bases) if (!isType(b)) fail(TypeError, "bases must be types");
        const cls = newType(name, bases.slice(), ns, ns.get("__module__") || "main");
        cls.cls = metaclass;
        cls.qualname = ns.get("__qualname__") || name;
        typeEpoch++;
        // __slots__: instances of an all-slotted hierarchy accept only those names.
        const slots = ns.get("__slots__");
        if (slots !== undefined) {
            const names = typeof slots === "string" ? [slots] : rt.drain(slots).map((s) => { if (typeof s !== "string") fail(TypeError, "__slots__ items must be strings, not '" + typeOf(s).name + "'"); return s; });
            const all = new Set(names);
            let sealed = true;
            for (const b of bases) {
                if (b === ObjectType || rt.constructors.has(b)) continue;
                if (b.slots === undefined || !b.noDict) sealed = false;
                if (b.slots !== undefined) for (const s of b.slots) all.add(s);
            }
            cls.slots = all;
            cls.noDict = sealed && !all.has("__dict__");
        } else {
            for (const b of bases) if (b.slots !== undefined) cls.slots = new Set(b.slots);
            cls.noDict = false;
        }
        for (const b of bases) if (b.isABC) cls.isABC = true;
        // Functions named __new__ are implicitly static methods.
        const nw = ns.get("__new__");
        if (nw !== undefined && nw !== null && typeof nw === "object" && nw.cls === T.function) ns.set("__new__", { cls: T.staticmethod, func: nw });
        for (const implicit of ["__init_subclass__", "__class_getitem__"]) {
            const f = ns.get(implicit);
            if (f !== undefined && f !== null && typeof f === "object" && f.cls === T.function) ns.set(implicit, { cls: T.classmethod, func: f });
        }
        // Descriptors learn their attribute name.
        for (const [k, v] of ns) {
            if (v === null || typeof v !== "object" || v.cls === undefined || v.isType) continue;
            const sn = lookupType(typeOf(v), "__set_name__");
            if (sn !== undefined) call(descrGet(sn, v, typeOf(v)), [cls, k], null);
        }
        // The nearest base's __init_subclass__ (a classmethod) sees the class keywords.
        for (let i = 1; i < cls.mro.length; i++) {
            const hook = cls.mro[i].dict.get("__init_subclass__");
            if (hook === undefined) continue;
            if (hook !== ObjectType.dict.get("__init_subclass__")) call(descrGet(hook.cls === T.classmethod ? hook : { cls: T.classmethod, func: hook }, null, cls), [], kw);
            else if (kw !== null && kw.size) fail(TypeError, cls.name + ".__init_subclass__() takes no keyword arguments");
            break;
        }
        return cls;
    };
    R.buildclass = function (bodyFn, ns, name, bases, kwargs) {
        let cell = null;
        if (bodyFn !== null) {
            const p = prepare(bodyFn, [], null);
            // The class body runs with its namespace as the only argument.
            cell = p.self.code([ns]);
        }
        let metaclass = null, kw = null;
        if (kwargs !== null && kwargs.size) {
            for (const [k, v] of kwargs) {
                if (k === "metaclass") metaclass = v;
                else { if (kw === null) kw = new Map(); kw.set(k, v); }
            }
        }
        for (const b of bases) if (!isType(b)) fail(TypeError, "bases must be types");
        // The metaclass: explicit, else the most derived metaclass of the bases.
        if (metaclass === null) {
            metaclass = TypeType;
            for (const b of bases) {
                const m = b.cls || TypeType;
                if (isSubclass(m, metaclass)) metaclass = m;
                else if (!isSubclass(metaclass, m)) fail(TypeError, "metaclass conflict: the metaclass of a derived class must be a (non-strict) subclass of the metaclasses of all its bases");
            }
        }
        let cls;
        if (metaclass === TypeType) cls = rt.makeClass(TypeType, name, bases, ns, kw);
        else if (isType(metaclass)) cls = call(metaclass, [name, tuple(bases.slice()), rt.dictFromMap(ns)], kw);
        else cls = call(metaclass, [name, tuple(bases.slice()), rt.dictFromMap(ns)], kw);
        if (cell !== null && typeof cell === "object" && "v" in cell) cell.v = cls;
        return cls;
    };
    rt.buildclass = R.buildclass;
    // ---- with statements ----------------------------------------------------------------------------
    R.withenter = function (mgr) {
        const t = typeOf(mgr);
        const enter = lookupType(t, "__enter__"), exit = lookupType(t, "__exit__");
        if (enter === undefined) fail(TypeError, "'" + t.name + "' object does not support the context manager protocol");
        if (exit === undefined) fail(TypeError, "'" + t.name + "' object does not support the context manager protocol (missed __exit__ method)");
        return [descrGet(exit, mgr, t), call(descrGet(enter, mgr, t), [], null)];
    };
    R.withexit = function (exit, e) {
        const exc = normexc(e);
        const r = call(exit, [typeOf(exc), exc, null], null);
        if (!rt.truth(r)) throw e;
        return null;
    };
    R.withexitnormal = function (exit) { call(exit, [null, null, null], null); return null; };
    R.superof = function (cls, self) {
        // `super()` in a metaclass method: `self` is a class that is an
        // INSTANCE of `cls`, so the MRO walked is that of its metaclass.
        const objtype = isType(self) && isSubclass(self, cls) ? self : typeOf(self);
        return { cls: T.super, type: cls, obj: self, objtype: objtype };
    };
    rt.superAttr = function (sup, name) {
        const mro = sup.objtype.mro;
        const start = mro.indexOf(sup.type) + 1;
        for (let i = start; i < mro.length; i++) {
            const v = mro[i].dict.get(name);
            if (v !== undefined) return descrGet(v, isType(sup.obj) && sup.obj === sup.objtype ? null : sup.obj, sup.objtype);
        }
        fail(E.AttributeError, "'super' object has no attribute '" + name + "'");
    };

    // ---- iteration ----------------------------------------------------------------------------------
    // A class-level dunder (a classmethod such as Enum.__iter__), bound to the class.
    function classDunder(t, name) {
        const m = lookupType(t, name);
        if (m !== undefined && m !== null && typeof m === "object" && m.cls === T.classmethod) return bound(m.func, t);
        return undefined;
    }
    rt.classDunder = classDunder;
    // Structural iteration for the builtin containers and their subclasses.
    function baseIter(v) {
        if (typeof v === "string") return { cls: T.iterator, next: stringIter(v) };
        if (v.items !== undefined && (isInstance(v, T.list) || isInstance(v, T.tuple))) { let i = 0; return { cls: T.list_iterator, next: () => i < v.items.length ? v.items[i++] : STOP }; }
        if (v.map !== undefined && isInstance(v, T.dict)) return rt.dictKeyIter(v);
        if (v.map !== undefined) return rt.setIter(v);
        if (isInstance(v, T.range)) return rangeIter(v);
        if (isInstance(v, T.bytes)) { let i = 0; return { cls: T.iterator, next: () => i < v.items.length ? BigInt(v.items[i++]) : STOP }; }
        fail(TypeError, "'" + typeOf(v).name + "' object is not iterable");
    }
    rt.baseIter = baseIter;
    function iter(v) {
        if (typeof v === "string") return baseIter(v);
        if (v !== null && typeof v === "object") {
            const c = v.cls;
            if (c === T.list || c === T.tuple || c === T.dict || c === T.set || c === T.frozenset || c === T.range || c === T.bytes) return baseIter(v);
            if (v.isType) {
                const m = classDunder(v, "__iter__");
                if (m !== undefined) return iter(call(m, [], null));
            }
            if (c === T.generator || c === T.iterator || c === T.list_iterator) return v;
            if (c === T.dict_keys || c === T.dict_values || c === T.dict_items) return v.iter();
            if (c === T.enumerate || c === T.zip || c === T.map || c === T.filter || c === T.reversed) return v;
            const m = typeMethod(v, "__iter__");
            if (m !== undefined) {
                if (m.isBase) return baseIter(v);
                const it = call(descrGet(m, v, c), [], null);
                if (it !== null && typeof it === "object" && it.next !== undefined) return it;
                return { cls: T.iterator, next: () => { const n = callMethod(it, "__next__", []); return n; }, wrapped: it };
            }
            const gi = typeMethod(v, "__getitem__");
            if (gi !== undefined && !gi.isBase) {
                let i = 0n;
                return { cls: T.iterator, next: () => {
                    try { return call(descrGet(gi, v, c), [i++], null); }
                    catch (e) { if (e && e.cls === E.IndexError) return STOP; throw e; }
                } };
            }
        }
        fail(TypeError, "'" + typeOf(v).name + "' object is not iterable");
    }
    function stringIter(s) {
        let i = 0;
        return () => {
            if (i >= s.length) return STOP;
            const cp = s.codePointAt(i); const ch = String.fromCodePoint(cp); i += ch.length; return ch;
        };
    }
    function rangeIter(r) {
        let v = r.start;
        return { cls: T.iterator, next: () => {
            if (r.step > 0n ? v >= r.stop : v <= r.stop) return STOP;
            const out = v; v += r.step; return out;
        } };
    }
    // One step: the next value or the STOP sentinel. Python-level
    // StopIteration raised by a __next__ becomes STOP.
    function fornext(it) {
        if (it.next !== undefined) {
            if (it.wrapped === undefined && it.cls !== T.generator) return it.next();
            try { return it.next(); }
            catch (e) { if (e !== null && typeof e === "object" && e.cls === E.StopIteration) return STOP; throw e; }
        }
        fail(TypeError, "'" + typeOf(it).name + "' object is not an iterator");
    }
    R.iter = iter; R.fornext = fornext; rt.iter = iter; rt.fornext = fornext;
    R.unpack = function (v, count, star) {
        const it = iter(v), items = [];
        if (star < 0) {
            for (let i = 0; i <= count; i++) {
                const x = fornext(it);
                if (x === STOP) {
                    if (i !== count) fail(ValueError, "not enough values to unpack (expected " + count + ", got " + i + ")");
                    return items;
                }
                if (i === count) fail(ValueError, "too many values to unpack (expected " + count + ")");
                items.push(x);
            }
        }
        for (;;) { const x = fornext(it); if (x === STOP) break; items.push(x); }
        const after = count - star - 1;
        if (items.length < count - 1) fail(ValueError, "not enough values to unpack (expected at least " + (count - 1) + ", got " + items.length + ")");
        const out = items.slice(0, star);
        out.push(list(items.slice(star, items.length - after)));
        for (let i = items.length - after; i < items.length; i++) out.push(items[i]);
        return out;
    };
    // ---- match statement ---------------------------------------------------------------------
    // The emitter compiles patterns to tests over these helpers; each returns
    // the sub-values to match next, or null when the subject does not match.
    R.meq = function (subject, value) { return rt.eq(subject, value); };
    function sequenceItems(subject) {
        if (subject === null || typeof subject !== "object") return null;
        if (subject.items !== undefined && Array.isArray(subject.items) && !isInstance(subject, T.bytes)) return subject.items;
        if (isInstance(subject, T.range)) return rt.drain(subject);
        return null;
    }
    R.mseq = function (subject, n, star) {
        const items = sequenceItems(subject);
        if (items === null) return null;
        if (star < 0) return items.length === n ? items : null;
        if (items.length < n - 1) return null;
        const out = [];
        for (let i = 0; i < star; i++) out.push(items[i]);
        const tail = n - 1 - star;
        out.push(R.list(items.slice(star, items.length - tail)));
        for (let i = items.length - tail; i < items.length; i++) out.push(items[i]);
        return out;
    };
    function isMapping(subject) {
        if (subject === null || typeof subject !== "object") return false;
        if (subject.map !== undefined && isInstance(subject, T.dict)) return true;
        return !subject.isType && typeMethod(subject, "keys") !== undefined && typeMethod(subject, "__getitem__") !== undefined;
    }
    R.mmap = function (subject, keys) {
        if (!isMapping(subject)) return null;
        const isDict = subject.map !== undefined && isInstance(subject, T.dict);
        const out = [];
        for (const k of keys) {
            if (isDict) { const v = rt.dictGet(subject, k); if (v === undefined) return null; out.push(v); }
            else { if (!rt.contains(subject, k)) return null; out.push(rt.getitem(subject, k)); }
        }
        return out;
    };
    R.mrest = function (subject, keys) {
        const out = rt.dict();
        const skip = new Set(); for (const k of keys) skip.add(rt.keyOf(k));
        if (subject.map !== undefined && isInstance(subject, T.dict)) {
            for (const [k, v] of rt.dictEntries(subject)) if (!skip.has(rt.keyOf(k))) rt.dictSet(out, k, v);
        } else {
            for (const k of rt.drain(callMethod(subject, "keys", []))) if (!skip.has(rt.keyOf(k))) rt.dictSet(out, k, rt.getitem(subject, k));
        }
        return out;
    };
    R.mattr = function (subject, name) {
        try { return getattr(subject, name); }
        catch (e) { if (e !== null && typeof e === "object" && isInstance(e, E.AttributeError)) return UNBOUND; throw e; }
    };
    R.mcls = function (subject, cls, npos) {
        if (!isType(cls)) fail(TypeError, "called match pattern must be a class");
        if (!rt.isinstanceCheck(subject, cls)) return null;
        if (npos === 0) return [];
        let names = lookupType(cls, "__match_args__");
        if (names === undefined) names = lookupType(cls, "_fields");
        if (names === undefined) {
            // Builtin types (and their subclasses) match the whole subject.
            for (const t of [T.bool, T.bytes, T.dict, T.float, T.frozenset, T.int, T.list, T.set, T.str, T.tuple]) {
                if (isSubclass(cls, t)) {
                    if (npos > 1) fail(TypeError, cls.name + "() accepts 1 positional sub-pattern (" + npos + " given)");
                    return [subject];
                }
            }
            fail(TypeError, cls.name + "() accepts 0 positional sub-patterns (" + npos + " given)");
        }
        if (names === null || typeof names !== "object" || names.cls !== T.tuple) fail(TypeError, cls.name + ".__match_args__ must be a tuple (got " + typeOf(names).name + ")");
        const items = names.items;
        if (npos > items.length) fail(TypeError, cls.name + "() accepts " + items.length + " positional sub-pattern" + (items.length === 1 ? "" : "s") + " (" + npos + " given)");
        const out = [];
        for (let i = 0; i < npos; i++) {
            const name = items[i];
            if (typeof name !== "string") fail(TypeError, "__match_args__ elements must be strings (got " + typeOf(name).name + ")");
            const v = R.mattr(subject, name);
            if (v === UNBOUND) return null;
            out.push(v);
        }
        return out;
    };
    // The counted `for i in range(...)` loop: only for the builtin itself.
    R.rangecheck = function (f) { return f === T.range; };
    R.rangeargs = function (args) {
        const r = rt.construct(T.range, args, null);
        return [r.start, r.stop, r.step, r.step > 0n];
    };
    R.accumulate = function (acc, v) {
        if (acc.cls === T.list) acc.items.push(v); else rt.setAdd(acc, v);
        return null;
    };
    R.genreturned = function (it) { return it.returned === undefined ? null : it.returned; };

    // ---- generators ----------------------------------------------------------------------------------
    rt.makeGenerator = function (jsgen, f) {
        const g = { cls: T.generator, js: jsgen, done: false, returned: null, name: f.name, qualname: f.qualname, running: false };
        g.next = function () {
            if (g.done) return STOP;
            if (g.running) fail(ValueError, "generator already executing");
            g.running = true;
            let r;
            try { r = jsgen.next(undefined); } finally { g.running = false; }
            if (r.done) { g.done = true; g.returned = r.value === undefined ? null : r.value; return STOP; }
            return r.value;
        };
        g.send = function (v) {
            if (g.done) throw makeExc(E.StopIteration, []);
            g.running = true;
            let r;
            try { r = jsgen.next(v); } finally { g.running = false; }
            if (r.done) { g.done = true; g.returned = r.value === undefined ? null : r.value; throw makeExc(E.StopIteration, r.value === undefined || r.value === null ? [] : [r.value]); }
            return r.value;
        };
        g.throwIn = function (exc) {
            if (g.done) throw exc;
            g.running = true;
            let r;
            try { r = jsgen.throw(exc); } finally { g.running = false; }
            if (r.done) { g.done = true; throw makeExc(E.StopIteration, []); }
            return r.value;
        };
        g.close = function () {
            if (g.done) return null;
            try { jsgen.return(undefined); } catch (e) { /* a throw during close */ }
            g.done = true; return null;
        };
        return g;
    };

    // ---- modules ----------------------------------------------------------------------------------------
    const inits = new Map(), modules = new Map(), builtinModules = new Map();
    let entryName = "main";
    rt.builtinModules = builtinModules; rt.modules = modules;
    function newModule(name, file) {
        return { cls: T.module, name: name, file: file || null, globals: new Map(), dict: null };
    }
    rt.newModule = newModule;
    rt.moduleAttr = function (m, name) {
        const v = m.globals.get(name);
        if (v !== undefined) return v;
        if (name === "__name__") return m.name;
        if (name === "__file__") return m.file;
        if (name === "__dict__") return rt.dictFromMap(m.globals);
        if (m.submodules !== undefined && m.submodules.has(name)) return m.submodules.get(name);
        fail(E.AttributeError, "module '" + m.name + "' has no attribute '" + name + "'");
    };
    R.module = function (name, code, file) { inits.set(name, { code: code, file: file }); files.push(file); return null; };
    R.entry = function (name) { entryName = name; return null; };
    R.import = function (name, importerGlobals) {
        if (modules.has(name)) return modules.get(name);
        const dot = name.indexOf(".");
        if (dot >= 0) {
            const head = R.import(name.slice(0, dot), importerGlobals);
            const sub = rt.moduleAttr(head, name.slice(dot + 1));
            return sub;
        }
        const init = inits.get(name);
        if (init !== undefined) return runModule(name, init, name === entryName ? "__main__" : name);
        const b = builtinModules.get(name);
        if (b !== undefined) { const m = typeof b === "function" ? b() : b; modules.set(name, m); return m; }
        fail(E.ModuleNotFoundError, "No module named '" + name + "'");
    };
    function runModule(name, init, dunderName) {
        const m = newModule(name, init.file);
        m.globals.set("__name__", dunderName);
        m.globals.set("__file__", init.file);
        m.globals.set("__doc__", null);
        modules.set(name, m);
        const fn = R.func(init.code, "<module>", "<module>", [], 0, 0, false, false, [], [], [], [], m.globals, false, null, name);
        try {
            fn.code([]);
        } catch (e) {
            modules.delete(name);
            throw e;
        }
        return m;
    }
    R.runmain = function (name) {
        const init = inits.get(name);
        if (init === undefined) fail(E.ModuleNotFoundError, name);
        return runModule(name, init, "__main__");
    };
    R.importfrom = function (m, name) {
        try { return getattr(m, name); }
        catch (e) {
            if (e && e.cls === E.AttributeError) fail(E.ImportError, "cannot import name '" + name + "' from '" + m.name + "'");
            throw e;
        }
    };
    R.importstar = function (m, g) {
        const all = m.globals.get("__all__");
        if (all !== undefined && all !== null && all.items) {
            for (const n of all.items) g.set(n, getattr(m, n));
        } else {
            for (const [k, v] of m.globals) if (!k.startsWith("_")) g.set(k, v);
            if (m.exports) for (const [k, v] of m.exports) if (!k.startsWith("_")) g.set(k, v);
        }
        return null;
    };
    rt.entryModule = function () { return modules.get(entryName) || null; };

    // ---- forward declarations filled by the other runtime files ------------------------------------------
    let str = null, repr = null;
    rt.setStrRepr = function (s, r) { str = s; repr = r; };
    rt.builtins = new Map();
    return R;
})();
