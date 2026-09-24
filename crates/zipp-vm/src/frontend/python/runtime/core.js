/* ZIPP Python runtime — core object model. Apache-2.0.
 *
 * Trusted JavaScript compiled by ZIPP once per program. Guest Python is never
 * translated to JavaScript source; the Rust emitter lowers it to bytecode that
 * calls into the `__zipp_py` helper object defined here.
 *
 * Value model: int = BigInt, float = number, bool = boolean, str = string,
 * None = null, complex = `{cls, re, im}` (types.js). Everything else is a JS
 * object with a `cls` field pointing at its Python class object; instances
 * keep attributes in `dict` (a Map).
 *
 * Code ABI: every Python code object is called as a member of its Python
 * function object (`this`), with the bound values as its own parameters
 * (`invoke`; one array instead past `MAX_DIRECT`). Positional call sites call
 * a function's per-count entry (`f.c<n>`) directly and everything else binds
 * (`bindArgs`) first; calls stay on the VM's explicit frame stack. The
 * emitter's inline attribute, method and construction paths read per-class
 * cache tables kept here (`ga`, `sa`, `gm`, `gb`, `gp`, `sp`, `gx`, and the
 * classes' own `c<n>` construction entries), invalidated wherever the class
 * lookup cache is.
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
    // A builtin called with a count its arity admits gets a positional entry
    // for that count (`c<n>`, the trampoline below), so the emitter's
    // positional call sites reach it without this frame next time.
    function builtinFast(args) {
        const n = args.length, arity = this.arity;
        if (arity < 0 ? n >= this.minArity : n <= arity && n >= this.minArity) {
            if (n < BUILTIN_ENTRY.length && this["c" + n] === undefined) this["c" + n] = BUILTIN_ENTRY[n];
            return this.code(args);
        }
        return this.code(builtinArgs(this, args, null));
    }
    const BUILTIN_ENTRY = [
        function () { return this.code([]); },
        function (a) { return this.code([a]); },
        function (a, b) { return this.code([a, b]); },
        function (a, b, c) { return this.code([a, b, c]); },
        function (a, b, c, d) { return this.code([a, b, c, d]); },
        function (a, b, c, d, e) { return this.code([a, b, c, d, e]); },
        function (a, b, c, d, e, f) { return this.code([a, b, c, d, e, f]); },
    ];
    function makeType(name, bases, dict, module) {
        // `code` makes a class callable through the same member-call path as
        // functions (`prepare` returns self = the class, code = constructCode).
        const t = { cls: null, name: name, qualname: name, module: module || "builtins",
            bases: bases, mro: null, dict: dict || new Map(), id: nextId++, isType: true, code: constructCode, fast: typeFast,
            ga: Object.create(null), sa: Object.create(null), gm: Object.create(null), gb: Object.create(null),
            gp: Object.create(null), sp: Object.create(null), gx: Object.create(null),
            userClass: false, flagged: false, gs: Object.create(null), gv: Object.create(null) };
        t.mro = computeMro(t);
        return t;
    }
    // C3 linearisation, as CPython. Every class in it but `t` records `t` as
    // a subclass, so a change to one of them reaches `t`'s lookup cache (see
    // `typeChanged`).
    function computeMro(t) {
        const seqs = t.bases.map((b) => b.mro.slice());
        seqs.push(t.bases.slice());
        const result = [t];
        for (;;) {
            const live = seqs.filter((s) => s.length > 0);
            if (live.length === 0) { noteSubclass(t, result); return result; }
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
    // Each class keeps weak references to all its subclasses (direct or
    // not), so it never keeps a dynamically created class alive. Dead
    // entries are dropped when the list has doubled since the last sweep.
    const weakRef = typeof WeakRef === "function" ? (v) => new WeakRef(v) : (v) => ({ deref: () => v });
    function noteSubclass(t, mro) {
        for (let i = 1; i < mro.length; i++) {
            const c = mro[i];
            let subs = c.subclasses;
            if (subs === undefined) { subs = c.subclasses = []; c.subSweep = 16; }
            if (subs.length >= c.subSweep) { liveSubclasses(c, null); c.subSweep = 2 * subs.length + 16; }
            subs.push(weakRef(t));
        }
    }
    // Calls `visit` on every live subclass of `c` and compacts the list.
    function liveSubclasses(c, visit) {
        const subs = c.subclasses;
        if (subs === undefined) return;
        let live = 0;
        for (let i = 0; i < subs.length; i++) {
            const s = subs[i].deref();
            if (s === undefined) continue;
            subs[live++] = subs[i];
            if (visit !== null) visit(s);
        }
        subs.length = live;
    }
    const TypeType = makeType("type", [], new Map());
    TypeType.cls = TypeType;
    const ObjectType = makeType("object", [], new Map());
    ObjectType.cls = TypeType;
    TypeType.bases = [ObjectType]; TypeType.mro = [TypeType, ObjectType]; noteSubclass(TypeType, TypeType.mro);
    function newType(name, bases, dict, module) {
        const t = makeType(name, bases.length ? bases : [ObjectType], dict, module);
        t.cls = TypeType;
        return t;
    }
    rt.TypeType = TypeType; rt.ObjectType = ObjectType; rt.newType = newType; rt.computeMro = computeMro;
    const T = {};                        // builtin type objects by name
    rt.T = T;
    for (const name of ["NoneType", "bool", "int", "float", "complex", "str", "list", "tuple", "dict", "set",
        "frozenset", "range", "function", "builtin_function_or_method", "method", "module",
        "generator", "NotImplementedType", "ellipsis", "slice", "property", "staticmethod",
        "classmethod", "super", "cell", "bytes", "list_iterator", "dict_keys", "dict_values",
        "dict_items", "enumerate", "zip", "map", "filter", "reversed", "iterator", "code"]) {
        T[name] = newType(name, [], new Map());
    }
    T.bool.bases = [T.int]; T.bool.mro = [T.bool, T.int, ObjectType]; noteSubclass(T.bool, [T.bool, T.int]);
    ELLIPSIS.cls = T.ellipsis; NOTIMPL.cls = T.NotImplementedType;
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
    // Exception instances are ordinary objects carrying `args`; one leaving
    // the program becomes a host Error in entry.js (`hostError`).
    const E = {};
    rt.E = E;
    function defExc(name, base) { E[name] = newType(name, [base || E.Exception], new Map()); return E[name]; }
    E.BaseException = newType("BaseException", [ObjectType], new Map());
    defExc("Exception", E.BaseException);
    defExc("SystemExit", E.BaseException); defExc("KeyboardInterrupt", E.BaseException); defExc("GeneratorExit", E.BaseException);
    for (const n of ["ArithmeticError", "AssertionError", "AttributeError", "EOFError", "ImportError", "LookupError",
        "MemoryError", "NameError", "OSError", "RuntimeError", "StopIteration", "SyntaxError", "TypeError",
        "ValueError", "Warning", "BufferError", "ReferenceError"]) defExc(n);
    defExc("ModuleNotFoundError", E.ImportError); E.ImportError.dict.set("name", null); E.ImportError.dict.set("path", null);
    defExc("IndexError", E.LookupError); defExc("KeyError", E.LookupError);
    defExc("UnboundLocalError", E.NameError); defExc("NotImplementedError", E.RuntimeError); defExc("RecursionError", E.RuntimeError);
    defExc("OverflowError", E.ArithmeticError); defExc("ZeroDivisionError", E.ArithmeticError); defExc("FloatingPointError", E.ArithmeticError);
    defExc("FileNotFoundError", E.OSError); defExc("PermissionError", E.OSError); defExc("TimeoutError", E.OSError);
    defExc("FileExistsError", E.OSError); defExc("IsADirectoryError", E.OSError); defExc("NotADirectoryError", E.OSError);
    defExc("UnicodeError", E.ValueError); defExc("UnicodeDecodeError", E.UnicodeError); defExc("UnicodeEncodeError", E.UnicodeError);
    defExc("IndentationError", E.SyntaxError); defExc("DeprecationWarning", E.Warning); defExc("UserWarning", E.Warning);
    defExc("StopAsyncIteration"); defExc("JSONDecodeError", E.ValueError);
    defExc("BaseExceptionGroup", E.BaseException); defExc("ExceptionGroup", E.BaseExceptionGroup);
    E.ExceptionGroup.bases = [E.BaseExceptionGroup, E.Exception]; E.ExceptionGroup.mro = computeMro(E.ExceptionGroup);
    function locationText(enc) {
        if (!enc || enc < 0) return "";
        const file = files[Math.floor(enc / 1000000)] || "?";
        return " (" + file + ":" + (enc % 1000000) + ")";
    }
    // Runtime-raised exceptions take the exception being handled as their
    // __context__ (a `raise` statement sets it again at raise time). The
    // location is kept as the encoded line; its text, like the message, is
    // built only when the exception reaches the host (most are caught).
    // Python code keeps its current line in a register of its frame, not in
    // a global, so a new (or explicitly raised) exception's line is pending
    // (-1) until the frame it was raised in handles it (`R.caught`,
    // `R.withexit`) or lets it out (`R.addframe`): that frame's line register
    // still holds the raising statement's line then.
    function makeExc(cls, args) {
        return { cls: cls, dict: new Map(), args: sequence(T.tuple, args), cause: null,
            context: excStack.length ? excStack[excStack.length - 1] : null, tbline: -1, suppress: false };
    }
    rt.makeExc = makeExc;
    // The innermost recorded frame holds the line its own frame was running;
    // the global line is only re-stamped when a line changes, so after a call
    // returns it can still name the callee's last line (another module).
    rt.excLocation = function (e) {
        const fr = e.frames !== undefined && e.frames.length ? e.frames[0] : null;
        if (fr !== null && fr.file !== "?" && fr.line > 0) return " (" + fr.file + ":" + fr.line + ")";
        return locationText(e.tbline);
    };
    // Loop forms of map/filter/findIndex/every for callbacks that may run
    // guest code: a native array builtin would run each callback in a nested
    // interpreter loop, and the hardened wasm profile caps that nesting.
    function amap(arr, fn) { const out = new Array(arr.length); for (let i = 0; i < arr.length; i++) out[i] = fn(arr[i], i); return out; }
    function afilter(arr, fn) { const out = []; for (let i = 0; i < arr.length; i++) if (fn(arr[i], i)) out.push(arr[i]); return out; }
    function aindex(arr, fn) { for (let i = 0; i < arr.length; i++) if (fn(arr[i], i)) return i; return -1; }
    function aevery(arr, fn) { for (let i = 0; i < arr.length; i++) if (!fn(arr[i], i)) return false; return true; }
    function acount(arr, fn) { let n = 0; for (let i = 0; i < arr.length; i++) if (fn(arr[i], i)) n++; return n; }
    rt.amap = amap; rt.afilter = afilter; rt.aindex = aindex; rt.aevery = aevery; rt.acount = acount;
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
    // An exception entering a handler of the frame it was raised in (see
    // `makeExc`): its pending line is that frame's current line.
    R.caught = function (e, line) {
        // `normexc`'s own first test, inline: a Python exception is itself.
        const exc = e !== null && typeof e === "object" && e.tbline !== undefined && e.cls !== undefined && e.cls.mro !== undefined && e.cls.mro.indexOf(E.BaseException) >= 0 ? e : normexc(e);
        if (exc.tbline === -1) exc.tbline = line;
        return exc;
    };
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
        if (exc.tbline === -1) exc.tbline = enc;
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
    // The emitter pushes and pops this array itself (never replaced).
    R.EXCSTACK = excStack;
    // What the emitter's native `raise` / handler entry (`PyRaise`,
    // `PyCaught`) test exceptions against.
    R.EBASE = E.BaseException;
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
        e.tbline = -1;
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
    // MRO lookups are cached per type (`lcache`, which also holds the
    // construction plan). Setting or deleting a class attribute drops that
    // one name from the class's cache and its subclasses' (`typeChanged`),
    // so a class-attribute counter leaves every other lookup cached. The
    // global epoch drops every cache, for the runtime's own bulk edits of a
    // class dict (`rt.bumpEpoch`).
    let typeEpoch = 1;
    const MISSING_ATTR = { missing: true };
    const CTOR_PLAN = { ctorPlan: true };  // the `lcache` key of `ctorPlan`
    // The `lcache` key of the instance-attribute store plan: true when a
    // plain `obj.dict.set` is exactly what `setattr` ends up doing for this
    // class (no `__setattr__` override, an instance dict), false otherwise.
    const SET_PLAN = { setPlan: true };
    // Inline-cache tables the emitter's fast paths read with plain property
    // loads (`obj.cls.ga.x`), set by the slow helpers once they know a name's
    // answer for a class and cleared wherever `lcache` entries are:
    //   ga[name] === true  instances of this user class may answer `name`
    //                      from their own dict (no data descriptor on the
    //                      type), so a dict hit is `getattr`'s result;
    //   sa[name] === true  storing `name` on an instance is exactly
    //                      `obj.dict.set` (see the plain store in `setattr`);
    //   gm[name] = f       `obj.name(...)` finds the plain Python function f
    //                      on the type (the call still checks the instance
    //                      dict first).
    //   gp[name] = g       `name` is a property whose getter is the Python
    //                      function g, which takes the instance as its one
    //                      positional value (`g.c1`);
    //   sp[name] = s       likewise a property whose setter s takes
    //                      (instance, value) (`s.c2`);
    //   gx[name] === true  nothing on the type answers `name` (no class
    //                      attribute, no special attribute, no __getattr__):
    //                      an instance's dict alone does.
    // Only user classes (`makeClass`) get those: their instances always
    // carry a Map dict. The dict-less builtin containers and str (see
    // `R.mfind`) get one more:
    //   gb[name#n] = b     `obj.name(n values)` calls the builtin method b
    //                      with the receiver first; its arity admits n + 1.
    // A false/undefined entry means "ask the helper".
    const flagged = [];
    function noteFlagged(t) {
        if (t.flagged !== true) { t.flagged = true; flagged.push(weakRef(t)); }
    }
    function clearFlags(t) {
        t.ga = Object.create(null); t.sa = Object.create(null); t.gm = Object.create(null); t.gb = Object.create(null);
        t.gp = Object.create(null); t.sp = Object.create(null); t.gx = Object.create(null);
        t.gs = Object.create(null); t.gv = Object.create(null);
        t.flagged = false;
        if (t.ctorEntries !== undefined) clearCtorEntries(t);
    }
    rt.noteFlagged = noteFlagged;
    rt.builtinsTouched = false;
    rt.bumpEpoch = function () {
        typeEpoch++;
        for (let i = 0; i < flagged.length; i++) { const t = flagged[i].deref(); if (t !== undefined) clearFlags(t); }
        flagged.length = 0;
    };
    function forgetName(t, name) {
        if (t.flagged === true) {
            if (name === "__setattr__") clearFlags(t);
            else {
                t.ga[name] = undefined; t.sa[name] = undefined; t.gm[name] = undefined; t.gb = Object.create(null);
                t.gp[name] = undefined; t.sp[name] = undefined; t.gx[name] = undefined; t.gs[name] = undefined; t.gv[name] = undefined;
                if (name === "__getattr__") t.gx = Object.create(null);
            }
        }
        if ((name === "__init__" || name === "__new__") && t.ctorEntries !== undefined) clearCtorEntries(t);
        const cache = t.lcache;
        if (cache === undefined) return;
        cache.delete(name);
        if (name === "__init__" || name === "__new__") cache.delete(CTOR_PLAN);
        else if (name === "__setattr__") cache.delete(SET_PLAN);
    }
    function typeChanged(t, name) {
        // A builtin type changed (the runtime's own types are never changed
        // otherwise): the comparisons' shortcut for exact tuples and lists
        // (`R.richcmp`) stops assuming their dunders are the base ones.
        if (t.userClass !== true) rt.builtinsTouched = true;
        forgetName(t, name);
        liveSubclasses(t, (s) => forgetName(s, name));
    }
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
    // `lookupType`'s answer through a valid cache without the call: the
    // attribute, `undefined` for a cached miss, or MISSING_ATTR when the
    // cache has no entry (or is stale) and `lookupType` must run. The hot
    // attribute paths below start with this, and one call per lookup is
    // what an interpreted frame costs here.
    function cachedType(t, name) {
        const cache = t.lcache;
        if (cache === undefined || t.lepoch !== typeEpoch) return MISSING_ATTR;
        const hit = cache.get(name);
        if (hit === undefined) return MISSING_ATTR;
        return hit === MISSING_ATTR ? undefined : hit;
    }
    // Whether `setattr` on an instance of `t` is exactly `obj.dict.set`
    // once the name is known to be no data descriptor: no `__setattr__`
    // other than object's, and instances carry a dict. Cached in `lcache`
    // (`forgetName` drops it with `__setattr__`).
    function setPlan(t) {
        let cache = t.lcache;
        if (cache === undefined || t.lepoch !== typeEpoch) { cache = t.lcache = new Map(); t.lepoch = typeEpoch; }
        let plain = cache.get(SET_PLAN);
        if (plain === undefined) {
            const sa = lookupType(t, "__setattr__");
            plain = (sa === undefined || sa === ObjectType.dict.get("__setattr__")) && t.noDict !== true;
            cache.set(SET_PLAN, plain);
        }
        return plain;
    }
    function isFunction(v) { return v !== null && typeof v === "object" && (v.cls === T.function || v.cls === T.builtin_function_or_method); }
    rt.isFunction = isFunction;
    function bound(func, self) { return { cls: T.method, func: func, self: self }; }
    rt.bound = bound;
    // An instance of a subclass of int, float or str boxes the primitive:
    // `{cls, pyval, dict}`. The builtin methods of those types (flagged
    // `unboxSelf`) receive the primitive as self.
    function unbox(v) { return v !== null && typeof v === "object" && v.pyval !== undefined ? v.pyval : v; }
    rt.unbox = unbox;
    // Descriptor GET of a class attribute found for `obj` (an instance) or for
    // the class itself when `obj` is null.
    function descrGet(attr, obj, owner) {
        if (attr === null || typeof attr !== "object") return attr;
        const c = attr.cls;
        if (c === T.function) return obj === null ? attr : bound(attr, obj);
        if (c === T.builtin_function_or_method) return obj === null ? attr : bound(attr, attr.unboxSelf === true && typeof obj === "object" ? unbox(obj) : obj);
        if (c === T.classmethod) return bound(attr.func, owner);
        if (c === T.staticmethod) return attr.func;
        if (c === T.property) {
            if (obj === null) return attr;
            const fget = attr.fget;
            if (fget === null) fail(E.AttributeError, "property has no getter");
            // The positional entry the emitter's own calls use; `rt.call`
            // binds the same single argument for anything without one.
            if (fget !== undefined && typeof fget === "object") {
                const c1 = fget.c1;
                if (c1 !== undefined) return fget.c1(obj);
                if (fget.fast !== undefined) return fget.fast([obj]);
            }
            return rt.call(fget, [obj], null);
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
    function typeAttr(t, name, missing) {
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
        if (missing !== undefined) return missing;
        fail(E.AttributeError, "type object '" + t.name + "' has no attribute '" + name + "'");
    }
    // `missing` (hasattr, getattr with a default, imports): returned instead
    // of raising when the lookup itself finds nothing, so a miss allocates no
    // exception. An AttributeError raised by a descriptor or __getattr__
    // still propagates.
    function getattr(obj, name, missing) {
        let t, dict;
        if (obj !== null && typeof obj === "object") {
            if (obj.isType) return typeAttr(obj, name, missing);
            t = obj.cls || ObjectType;
            if (t === T.module) return rt.moduleAttr(obj, name, missing);
            if (t === T.super) return rt.superAttr(obj, name);
            if (t === TypeType) return typeAttr(obj, name, missing);
            dict = obj.dict;
        } else {
            t = typeOf(obj);
        }
        if (name === "__class__") return t;
        if (dict !== undefined && t.gx[name] === true) {
            const v = dict.get(name);
            if (v !== undefined) return v;
            if (missing !== undefined) return missing;
        }
        let attr = cachedType(t, name);
        if (attr === MISSING_ATTR) attr = lookupType(t, name);
        // `isDataDescriptor`, inline: a property, or a class-valued
        // attribute other than a function whose type defines `__set__`.
        if (attr !== undefined && attr !== null && typeof attr === "object") {
            const ac = attr.cls;
            if (ac === T.property || (ac !== undefined && ac !== T.function && lookupType(ac, "__set__") !== undefined)) {
                if (ac === T.property && t.userClass === true && dict !== undefined) {
                    const g = attr.fget;
                    if (g !== null && typeof g === "object" && g.cls === T.function && typeof g.c1 === "function") { t.gp[name] = g; if (t.flagged !== true) noteFlagged(t); }
                }
                return descrGet(attr, obj, t);
            }
        }
        if (dict !== undefined) {
            // No data descriptor: a dict hit is the answer from now on, until
            // the name changes on the class (`forgetName`).
            if (t.userClass === true && (attr === undefined || attr === null || typeof attr !== "object" || attr.cls === T.function) && name !== "__dict__") {
                t.ga[name] = true; if (t.flagged !== true) noteFlagged(t);
            }
            const v = dict.get(name);
            if (v !== undefined) return v;
            if (name === "__dict__" && t.noDict !== true) return rt.instanceDict(obj);
            // A plain class attribute (an int, float, str or bool): the
            // answer for an instance dict without the name, until the name
            // changes on the class (`gv`, read by the emitter's `PyClassAttr`).
            const ta = typeof attr;
            if (t.userClass === true && t.ga[name] === true && (ta === "bigint" || ta === "number" || ta === "string" || ta === "boolean")) {
                t.gv[name] = attr; if (t.flagged !== true) noteFlagged(t);
            }
        }
        if (attr !== undefined) return descrGet(attr, obj, t);
        const special = rt.specialAttr(obj, t, name);
        if (special !== undefined) return special;
        const ga = lookupType(t, "__getattr__");
        if (ga !== undefined) return rt.call(ga, [obj, name], null);
        if (t.userClass === true && dict !== undefined && name !== "__dict__") { t.gx[name] = true; if (t.flagged !== true) noteFlagged(t); }
        if (missing !== undefined) return missing;
        fail(E.AttributeError, "'" + t.name + "' object has no attribute '" + name + "'");
    }
    function setattr(obj, name, value) {
        if (obj !== null && typeof obj === "object") {
            if (obj.isType) {
                if (name === "__name__") { obj.name = str(value); return null; }
                // An enum class's members are fixed.
                if (obj.members !== undefined && obj.members.includes(obj.dict.get(name))) fail(E.AttributeError, "cannot reassign member '" + name + "'");
                obj.dict.set(name, value); typeChanged(obj, name); return null;
            }
            if (name === "__dict__" && obj.dict !== undefined && obj.dict !== null && obj.cls !== T.module && (obj.cls || ObjectType).noDict !== true) { rt.setInstanceDict(obj, value); return null; }
            // The common store: an instance whose class has no attribute of
            // that name, or one that is no data descriptor (a primitive, None
            // or a plain function), and no `__setattr__` of its own. The
            // general path below reaches the same `dict.set` after the same
            // two lookups; MISSING_ATTR (nothing cached yet) takes it.
            const dict = obj.dict;
            if (dict !== undefined && dict !== null) {
                const t = obj.cls || ObjectType;
                if (t !== T.module) {
                    const attr = cachedType(t, name);
                    if ((attr === undefined || attr === null || typeof attr !== "object" || attr.cls === T.function) && setPlan(t)) {
                        if (t.userClass === true) { t.sa[name] = true; if (t.flagged !== true) noteFlagged(t); }
                        dict.set(name, value);
                        return null;
                    }
                }
            }
        }
        const t = typeOf(obj);
        if (obj === null || typeof obj !== "object" || obj.dict === undefined || obj.dict === null) {
            if (obj !== null && typeof obj === "object" && obj.cls === T.module) { obj.globals.set(name, value); return null; }
            if (lookupType(t, name) !== undefined) fail(E.AttributeError, "'" + t.name + "' object attribute '" + name + "' is read-only");
            fail(E.AttributeError, "'" + t.name + "' object has no attribute '" + name + "' and no __dict__ for setting new attributes");
        }
        const attr = lookupType(t, name);
        if (attr !== undefined && attr !== null && typeof attr === "object") {
            if (attr.cls === T.property) {
                const fs = attr.fset;
                if (fs === null) fail(E.AttributeError, "property '" + name + "' of '" + t.name + "' object has no setter");
                if (t.userClass === true && typeof fs === "object" && fs.cls === T.function && typeof fs.c2 === "function") { t.sp[name] = fs; if (t.flagged !== true) noteFlagged(t); }
                rt.call(fs, [obj, value], null); return null;
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
            if (!obj.dict.delete(name)) fail(E.AttributeError, "type object '" + obj.name + "' has no attribute '" + name + "'");
            typeChanged(obj, name);
            return null;
        }
        const t = typeOf(obj);
        if (obj !== null && typeof obj === "object" && obj.cls === T.module) {
            if (!obj.globals.delete(name)) fail(E.AttributeError, "module '" + obj.name + "' has no attribute '" + name + "'");
            return null;
        }
        if (obj === null || typeof obj !== "object" || obj.dict === undefined || obj.dict === null) fail(E.AttributeError, "'" + t.name + "' object has no attribute '" + name + "'");
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
    rt.getattr = getattr; rt.setattr = setattr; rt.descrGet = descrGet; rt.MISSING_ATTR = MISSING_ATTR;
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
    // The code ABI. A code object takes its bound values as its own JS
    // parameters, in signature order (positionals and keyword-only names,
    // then the `*args` tuple, then the `**kwargs` dict), and is called as a
    // member of its function object (`this`). A code object with more than
    // MAX_DIRECT bound values takes them as one array instead (`f.arr`).
    // A function whose signature has only positional parameters also gets
    // one entry per positional count it accepts, `f.c0`..`f.cN`: the code
    // object itself, which loads the defaults of the parameters a shorter
    // call leaves undefined (no Python value is `undefined`). The emitter's
    // positional call sites probe `f.c<argc>` and call it directly, so the
    // arity check is the probe; anything else binds and then `invoke`s.
    const MAX_DIRECT = 12;
    rt.MAX_DIRECT = MAX_DIRECT;
    // `f.code(...bound)` without a spread call, which the VM would run on a
    // nested native interpreter (a recursion limit the wasm build keeps low).
    function invoke(f, a) {
        if (f.arr === true) return f.code(a);
        switch (a.length) {
            case 0: return f.code();
            case 1: return f.code(a[0]);
            case 2: return f.code(a[0], a[1]);
            case 3: return f.code(a[0], a[1], a[2]);
            case 4: return f.code(a[0], a[1], a[2], a[3]);
            case 5: return f.code(a[0], a[1], a[2], a[3], a[4]);
            case 6: return f.code(a[0], a[1], a[2], a[3], a[4], a[5]);
            case 7: return f.code(a[0], a[1], a[2], a[3], a[4], a[5], a[6]);
            case 8: return f.code(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]);
            case 9: return f.code(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8]);
            case 10: return f.code(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9]);
            case 11: return f.code(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10]);
            case 12: return f.code(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11]);
        }
        fail(E.RuntimeError, "internal: bad bound argument count");
    }
    rt.invoke = invoke;
    // Generator functions: `code` creates the generator (the body runs up
    // to its GenStart, binding the parameters, now) from `this.real`, the
    // generator code object, forwarding the bound values as they came.
    const GEN_ENTRY = [
        function () { return rt.makeGenerator(this.real(), this); },
        function (a) { return rt.makeGenerator(this.real(a), this); },
        function (a, b) { return rt.makeGenerator(this.real(a, b), this); },
        function (a, b, c) { return rt.makeGenerator(this.real(a, b, c), this); },
        function (a, b, c, d) { return rt.makeGenerator(this.real(a, b, c, d), this); },
        function (a, b, c, d, e) { return rt.makeGenerator(this.real(a, b, c, d, e), this); },
        function (a, b, c, d, e, g) { return rt.makeGenerator(this.real(a, b, c, d, e, g), this); },
        function (a, b, c, d, e, g, h) { return rt.makeGenerator(this.real(a, b, c, d, e, g, h), this); },
        function (a, b, c, d, e, g, h, i) { return rt.makeGenerator(this.real(a, b, c, d, e, g, h, i), this); },
        function (a, b, c, d, e, g, h, i, j) { return rt.makeGenerator(this.real(a, b, c, d, e, g, h, i, j), this); },
        function (a, b, c, d, e, g, h, i, j, k) { return rt.makeGenerator(this.real(a, b, c, d, e, g, h, i, j, k), this); },
        function (a, b, c, d, e, g, h, i, j, k, l) { return rt.makeGenerator(this.real(a, b, c, d, e, g, h, i, j, k, l), this); },
        function (a, b, c, d, e, g, h, i, j, k, l, m) { return rt.makeGenerator(this.real(a, b, c, d, e, g, h, i, j, k, l, m), this); },
    ];
    function genEntryArr(a) { return rt.makeGenerator(this.real(a), this); }
    // The positional-array entry every callable has (`f.fast(args)`).
    function funcFast(args) { return invoke(this, bindArgs(this, args, null)); }
    R.func = function (code, name, qualname, argnames, posonly, positional, varargs, varkw,
                       defaults, kwdefNames, kwdefValues, cells, globals, isgen, doc, module) {
        const f = { cls: T.function, code: code, name: name, qualname: qualname, argnames: argnames,
            posonly: posonly, positional: positional, varargs: varargs, varkw: varkw,
            defaults: defaults, kwdefaults: null, cells: cells, globals: globals, isgen: isgen,
            doc: doc, module: module, dict: new Map(), simple: false, arr: false, fast: funcFast };
        if (kwdefNames.length) { f.kwdefaults = new Map(); for (let i = 0; i < kwdefNames.length; i++) f.kwdefaults.set(kwdefNames[i], kwdefValues[i]); }
        f.simple = !varargs && !varkw && argnames.length === positional && defaults.length === 0;
        const nbound = argnames.length + (varargs ? 1 : 0) + (varkw ? 1 : 0);
        f.arr = nbound > MAX_DIRECT;
        if (isgen) {
            f.real = code;
            f.code = f.arr ? genEntryArr : GEN_ENTRY[nbound];
        }
        if (!f.arr && !varargs && !varkw && argnames.length === positional) {
            for (let n = positional - defaults.length; n <= positional; n++) f["c" + n] = f.code;
        }
        return f;
    };
    R.arity = function (f, args) { arityError(f, args.length); };
    R.fannotate = function (f, names, values) { const m = new Map(); for (let i = 0; i < names.length; i++) m.set(names[i], values[i]); f.annotations = m; return null; };
    // A builtin: `code(args)` with `this` unused; `arity` -1 for variadic.
    // `unboxSelf` is an own field on every builtin (set true later for the
    // int/float/str methods) so method calls read it without a shape miss.
    function builtin(name, arity, code, minArity) {
        return { cls: T.builtin_function_or_method, name: name, qualname: name, arity: arity,
            minArity: minArity === undefined ? arity : minArity, code: code, module: "builtins", dict: undefined, fast: builtinFast,
            unboxSelf: false };
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
    // Keyword name -> parameter index, built on the first keyword call.
    function argIndex(f) {
        let m = f.argIndex;
        if (m === undefined) { m = f.argIndex = new Map(); for (let i = 0; i < f.argnames.length; i++) m.set(f.argnames[i], i); }
        return m;
    }
    // The error paths below follow CPython's order: keyword problems, then
    // too many positionals, then missing arguments.
    function bindArgs(f, args, kwargs) {
        const nkw = kwargs === null ? 0 : kwargs.size;
        if (f.simple && nkw === 0) {
            if (args.length !== f.positional) arityError(f, args.length);
            return args;
        }
        // Only positional-or-keyword parameters, and keywords that all name
        // parameters the positionals left (none positional-only): each fills
        // its own slot and defaults fill the rest. Anything else (an unknown,
        // repeated or positional-only keyword, a missing argument) binds
        // below, which also raises.
        if (nkw !== 0 && !f.varargs && !f.varkw && f.argnames.length === f.positional) {
            const names = f.argnames, npos = f.positional, na = args.length;
            if (na <= npos && na >= f.posonly) {
                const defaults = f.defaults, first = npos - defaults.length, out = args.slice();
                let used = 0, i = na;
                for (; i < npos; i++) {
                    const v = kwargs.get(names[i]);
                    if (v !== undefined) { out.push(v); used++; }
                    else if (i >= first) out.push(defaults[i - first]);
                    else break;
                }
                if (i === npos && used === nkw) {
                    // Keywords naming the next parameters in order: the call is
                    // the positional one of all its values. Recorded for the
                    // emitter's keyword call sites (`k<n>:<names>`).
                    const entry = f.arr ? undefined : f["c" + (na + nkw)];
                    if (entry !== undefined) {
                        let j = na, inOrder = true;
                        for (const k of kwargs.keys()) { if (k !== names[j]) { inOrder = false; break; } j++; }
                        if (inOrder) f["k" + na + ":" + names.slice(na, na + nkw).join(",")] = entry;
                    }
                    return out;
                }
            }
        }
        const names = f.argnames, npos = f.positional, nall = names.length;
        const out = new Array(nall);
        const n = args.length < npos ? args.length : npos;
        for (let i = 0; i < n; i++) out[i] = args[i];
        let kwrest = null;
        if (nkw !== 0) {
            // The common case, looked up per parameter without iterating the
            // Map: every keyword names a parameter not already filled.
            let matched = 0;
            for (let i = f.posonly; i < nall && matched !== nkw; i++) {
                const v = kwargs.get(names[i]);
                if (v === undefined) continue;
                if (out[i] !== undefined) { matched = -1; break; }
                out[i] = v; matched++;
            }
            if (matched !== nkw) kwrest = bindKeywordsInOrder(f, kwargs, out, n);
        }
        let extra = null;
        if (args.length > npos) {
            if (!f.varargs) tooManyPositional(f, args.length, out);
            extra = args.slice(npos);
        }
        const firstDefault = npos - f.defaults.length;
        let missing = false;
        for (let i = 0; i < nall; i++) {
            if (out[i] !== undefined) continue;
            if (i < npos) {
                if (i >= firstDefault) out[i] = f.defaults[i - firstDefault];
                else missing = true;
            } else {
                const d = f.kwdefaults === null ? undefined : f.kwdefaults.get(names[i]);
                if (d !== undefined) out[i] = d; else missing = true;
            }
        }
        if (missing) missingArguments(f, out);
        if (f.varargs) out.push(tuple(extra || []));
        if (f.varkw) out.push(kwrest || rt.dict());
        return out;
    }
    // Keywords bound in call order (extras for **kwargs, or an error named
    // for the first offending keyword, as CPython reports it); `out` holds
    // the first `n` positionals.
    function bindKeywordsInOrder(f, kwargs, out, n) {
        for (let i = n; i < out.length; i++) out[i] = undefined;
        const index = argIndex(f);
        let kwrest = null;
        for (const [k, v] of kwargs) {
            const idx = index.get(k);
            if (idx !== undefined && idx >= f.posonly) {
                if (out[idx] !== undefined) fail(TypeError, f.qualname + "() got multiple values for argument '" + k + "'");
                out[idx] = v;
            } else {
                if (!f.varkw) {
                    if (idx !== undefined) posonlyAsKeyword(f, kwargs);
                    fail(TypeError, f.qualname + "() got an unexpected keyword argument '" + k + "'");
                }
                if (kwrest === null) kwrest = rt.dict();
                rt.dictSet(kwrest, k, v);
            }
        }
        return kwrest;
    }
    function posonlyAsKeyword(f, kwargs) {
        const bad = [];
        for (const k of kwargs.keys()) { const i = argIndex(f).get(k); if (i !== undefined && i < f.posonly) bad.push(k); }
        fail(TypeError, f.qualname + "() got some positional-only arguments passed as keyword arguments: '" + bad.join(", ") + "'");
    }
    function tooManyPositional(f, got, out) {
        const npos = f.positional, ndef = f.defaults.length;
        let kwonly = 0;
        for (let i = npos; i < f.argnames.length; i++) if (out[i] !== undefined) kwonly++;
        const takes = ndef ? "from " + (npos - ndef) + " to " + npos + " positional arguments" : npos + " positional argument" + (npos === 1 ? "" : "s");
        const given = kwonly ? got + " positional argument" + (got === 1 ? "" : "s") + " (and " + kwonly + " keyword-only argument" + (kwonly === 1 ? "" : "s") + ") were" : got + (got === 1 ? " was" : " were");
        fail(TypeError, f.qualname + "() takes " + takes + " but " + given + " given");
    }
    function missingArguments(f, out) {
        const npos = f.positional, names = f.argnames, firstDefault = npos - f.defaults.length;
        const missing = [], missingKw = [];
        for (let i = 0; i < names.length; i++) {
            if (out[i] !== undefined) continue;
            if (i < npos) { if (i < firstDefault) missing.push(names[i]); }
            else if (f.kwdefaults === null || f.kwdefaults.get(names[i]) === undefined) missingKw.push(names[i]);
        }
        if (missing.length) fail(TypeError, f.qualname + "() missing " + missing.length + " required positional argument" + (missing.length === 1 ? "" : "s") + ": " + listNames(missing));
        fail(TypeError, f.qualname + "() missing " + missingKw.length + " required keyword-only argument" + (missingKw.length === 1 ? "" : "s") + ": " + listNames(missingKw));
    }
    function arityError(f, got) {
        if (got < f.positional) {
            const missing = f.argnames.slice(got, f.positional);
            fail(TypeError, f.qualname + "() missing " + missing.length + " required positional argument" + (missing.length === 1 ? "" : "s") + ": " + listNames(missing));
        }
        fail(TypeError, f.qualname + "() takes " + f.positional + " positional argument" + (f.positional === 1 ? "" : "s") + " but " + got + " " + (got === 1 ? "was" : "were") + " given");
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
            // Builtins that unpack a tuple of arguments word it CPython's other way.
            if (f.unpackArgs === true) {
                const bound = f.minArity === f.arity ? "" : args.length < f.minArity ? "at least " : "at most ";
                const k = args.length < f.minArity ? f.minArity : f.arity;
                fail(TypeError, f.name + " expected " + bound + k + " argument" + (k === 1 ? "" : "s") + ", got " + args.length);
            }
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
    // `prepare`'s record, for the runtime's own callers (`call` below); a
    // function's `code` takes the bound values as parameters (`invoke`).
    R.bind = prepare;
    // `obj.name(args)` resolved and called in one step, for the runtime's
    // own callers. The callee runs as a member call, a VM frame below this
    // one; the emitter resolves methods itself (`R.mfind` and the inline
    // caches) and calls the callee's entry directly.
    R.callmethod = function (obj, name, args, kwargs) {
        let t;
        if (obj !== null && typeof obj === "object") {
            if (obj.cls === T.module) {
                // `math.sqrt(x)`, `F.relu(x)`: a module global, called through
                // its positional entry (functions, builtins and classes).
                let v = obj.globals.get(name);
                if (v === undefined) v = rt.moduleAttr(obj, name);
                if (kwargs === null && v !== null && typeof v === "object" && v.fast !== undefined) return v.fast(args);
                return call(v, args, kwargs);
            }
            if (obj.isType || obj.cls === T.super) return call(getattr(obj, name), args, kwargs);
            t = obj.cls || ObjectType;
        } else {
            t = typeOf(obj);
        }
        let attr = cachedType(t, name);
        if (attr === MISSING_ATTR) attr = lookupType(t, name);
        if (attr !== undefined && attr !== null && typeof attr === "object") {
            const ac = attr.cls;
            if (ac === T.function || ac === T.builtin_function_or_method) {
                const inst = obj !== null && typeof obj === "object" && obj.dict !== undefined ? obj.dict.get(name) : undefined;
                if (inst === undefined) {
                    if (ac === T.function) {
                        // The receiver and up to three values straight into
                        // the parameters when they are exactly the signature.
                        if (kwargs === null && attr.simple === true && attr.positional === args.length + 1) {
                            switch (args.length) {
                                case 0: return attr.code(obj);
                                case 1: return attr.code(obj, args[0]);
                                case 2: return attr.code(obj, args[0], args[1]);
                                case 3: return attr.code(obj, args[0], args[1], args[2]);
                            }
                        }
                        const withSelf = [obj]; for (let i = 0; i < args.length; i++) withSelf.push(args[i]);
                        return invoke(attr, bindArgs(attr, withSelf, kwargs));
                    }
                    const withSelf = [obj]; for (let i = 0; i < args.length; i++) withSelf.push(args[i]);
                    if (attr.unboxSelf === true && typeof obj === "object") withSelf[0] = unbox(obj);
                    // `builtinArgs` with no keywords and a count the arity
                    // admits returns the array itself.
                    if (kwargs === null) {
                        const arity = attr.arity;
                        if (arity < 0 || (withSelf.length <= arity && withSelf.length >= attr.minArity)) return attr.code(withSelf);
                    }
                    return attr.code(builtinArgs(attr, withSelf, kwargs));
                }
            }
        }
        return call(getattr(obj, name), args, kwargs);
    };
    R.callv = function (f, args, kwargs) { return call(f, args, kwargs); };
    // Calling from JS (helpers, dunder dispatch). `prepare` guarantees that
    // `p.self.code === p.code`, so this is a member call the VM runs on its
    // own frame stack — never `Function.prototype.call`, which re-enters the
    // interpreter natively (and the wasm build allows only one such level).
    function call(f, args, kwargs) {
        if (f !== null && typeof f === "object" && f.cls === T.function) return invoke(f, bindArgs(f, args, kwargs === undefined ? null : kwargs));
        const p = prepare(f, args, kwargs === undefined ? null : kwargs);
        const self = p.self;
        return self.cls === T.function ? invoke(self, p.args) : self.code(p.args);
    }
    rt.call = call;
    // `call(f, [x], null)` through `f`'s one-argument entry when it has one
    // (what a positional call site calls: the same binding, no array).
    function call1(f, x) {
        if (f !== null && typeof f === "object") { const c1 = f.c1; if (typeof c1 === "function") return f.c1(x); }
        return call(f, [x], null);
    }
    rt.call1 = call1;
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
        return abstract.sort(rt.compareStrings);
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
        const plan = ctorPlan(cls);
        if (plan.ctor !== undefined) return plan.ctor(args, kwargs, cls);
        const init = plan.init;
        // The plain case (a user class of `type` whose instance is a fresh
        // record handed to a Python __init__ taking exactly these values,
        // or to no __init__ at all) gets a positional entry for the count.
        if (cls.userClass === true && cls.cls === TypeType && !cls.isABC && (kwargs === null || kwargs.size === 0)
            && plan.builtinBase === undefined && plan.newf === undefined && plan.alloc === undefined) {
            const n = args.length;
            if (n < CTOR_ENTRY.length) {
                if (init === undefined && n === 0) setCtorEntry(cls, 0, null);
                else if (init !== undefined && init !== null && init.cls === T.function && typeof init["c" + (n + 1)] === "function") setCtorEntry(cls, n, init);
            }
        }
        // An exception class with the base allocation and __init__ (a
        // builtin one, or a user subclass adding neither): the positional
        // entry builds the instance the path below builds.
        if (plan.alloc === rt.excAlloc && plan.init === rt.excInit && plan.newf === undefined && plan.builtinBase === undefined
            && cls.cls === TypeType && !cls.isABC && (kwargs === null || kwargs.size === 0) && args.length < EXC_ENTRY.length) {
            setExcEntry(cls, args.length);
        }
        // A subclass of an immutable builtin (int, str, tuple, ...) takes its
        // value from that builtin's __new__; a user __init__ then runs.
        if (plan.builtinBase !== undefined) {
            const obj = rt.builtinNew(cls, plan.builtinBase, args, kwargs);
            if (init !== undefined && (init === null || !init.isBase)) call(descrGet(init, obj, cls), args, kwargs);
            return obj;
        }
        let obj;
        if (plan.newf !== undefined) {
            const newf = plan.newf, withCls = [cls]; for (let i = 0; i < args.length; i++) withCls.push(args[i]);
            obj = call(newf.cls === T.staticmethod ? newf.func : newf, withCls, kwargs);
            if (!isInstance(obj, cls)) return obj;
        } else if (plan.alloc !== undefined) {
            // A subclass of a builtin container inherits its storage.
            obj = plan.alloc(cls);
            if (obj.dict === undefined || obj.dict === null) obj.dict = new Map();
        } else {
            obj = { cls: cls, dict: new Map() };
        }
        if (init !== undefined) {
            const withSelf = [obj]; for (let i = 0; i < args.length; i++) withSelf.push(args[i]);
            // A Python __init__ is entered as `callmethod` does.
            const r = init === null || init.cls !== T.function ? call(init, withSelf, kwargs)
                : invoke(init, bindArgs(init, withSelf, kwargs));
            if (r !== null) fail(TypeError, "__init__() should return None, not '" + typeOf(r).name + "'");
        } else if (args.length || (kwargs !== null && kwargs.size)) {
            if (plan.newf === undefined) fail(TypeError, cls.name + "() takes no arguments");
        }
        return obj;
    }
    // Positional entries of classes (`cls.c<n>`, see `constructDefault`):
    // exactly the plain construction path, with the __init__ found then.
    function initReturned(r) { fail(TypeError, "__init__() should return None, not '" + typeOf(r).name + "'"); }
    const CTOR_ENTRY = [
        function () { const obj = { cls: this, dict: new Map() }; const init = this.ctorInit; if (init !== null) { const r = init.c1(obj); if (r !== null) initReturned(r); } return obj; },
        function (a) { const obj = { cls: this, dict: new Map() }; const r = this.ctorInit.c2(obj, a); if (r !== null) initReturned(r); return obj; },
        function (a, b) { const obj = { cls: this, dict: new Map() }; const r = this.ctorInit.c3(obj, a, b); if (r !== null) initReturned(r); return obj; },
        function (a, b, c) { const obj = { cls: this, dict: new Map() }; const r = this.ctorInit.c4(obj, a, b, c); if (r !== null) initReturned(r); return obj; },
        function (a, b, c, d) { const obj = { cls: this, dict: new Map() }; const r = this.ctorInit.c5(obj, a, b, c, d); if (r !== null) initReturned(r); return obj; },
        function (a, b, c, d, e) { const obj = { cls: this, dict: new Map() }; const r = this.ctorInit.c6(obj, a, b, c, d, e); if (r !== null) initReturned(r); return obj; },
        function (a, b, c, d, e, f) { const obj = { cls: this, dict: new Map() }; const r = this.ctorInit.c7(obj, a, b, c, d, e, f); if (r !== null) initReturned(r); return obj; },
    ];
    const EXC_ENTRY = [
        function () { const e = makeExc(this, []); e.context = null; return e; },
        function (a) { const e = makeExc(this, [a]); e.context = null; return e; },
        function (a, b) { const e = makeExc(this, [a, b]); e.context = null; return e; },
        function (a, b, c) { const e = makeExc(this, [a, b, c]); e.context = null; return e; },
    ];
    function setExcEntry(cls, n) {
        if (cls.ctorInit !== rt.excInit) { clearCtorEntries(cls); cls.ctorInit = rt.excInit; }
        if (cls.ctorEntries === undefined || cls.ctorEntries === null) cls.ctorEntries = [];
        cls["c" + n] = EXC_ENTRY[n];
        cls.ctorEntries.push(n);
        noteFlagged(cls);
    }
    function setCtorEntry(cls, n, init) {
        if (cls.ctorInit !== init) { clearCtorEntries(cls); cls.ctorInit = init; }
        if (cls.ctorEntries === undefined || cls.ctorEntries === null) cls.ctorEntries = [];
        cls["c" + n] = CTOR_ENTRY[n];
        cls.ctorEntries.push(n);
        noteFlagged(cls);
    }
    function clearCtorEntries(cls) {
        const set = cls.ctorEntries;
        if (set === undefined || set === null) return;
        for (let i = 0; i < set.length; i++) cls["c" + set[i]] = undefined;
        cls.ctorEntries = null;
    }
    // What `constructDefault` needs to know about a class, cached with its
    // attribute lookups (`lcache`); a change to `__init__` or `__new__`
    // anywhere in its MRO drops it (`forgetName`).
    function ctorPlan(cls) {
        let cache = cls.lcache;
        if (cache === undefined || cls.lepoch !== typeEpoch) { cache = cls.lcache = new Map(); cls.lepoch = typeEpoch; }
        let plan = cache.get(CTOR_PLAN);
        if (plan !== undefined) return plan;
        const newf = lookupType(cls, "__new__"), init = lookupType(cls, "__init__");
        let alloc;
        for (const c of cls.mro) { alloc = rt.allocators.get(c); if (alloc !== undefined) break; }
        plan = { ctor: rt.constructors.get(cls),
            builtinBase: newf !== undefined && newf !== null ? newf.builtinBase : undefined,
            newf: newf !== undefined && newf !== ObjectType.dict.get("__new__") ? newf : undefined,
            init: init !== undefined && init !== ObjectType.dict.get("__init__") ? init : undefined,
            alloc: alloc };
        cache.set(CTOR_PLAN, plan);
        return plan;
    }
    rt.construct = construct;
    // `base.__new__(cls, *args)` for an immutable builtin base: the builtin's
    // constructor, re-classed (containers) or boxed (int/float/str).
    rt.builtinNew = function (cls, base, args, kwargs) {
        if (!isType(cls)) fail(TypeError, base.name + ".__new__(X): X is not a type object (" + typeOf(cls).name + ")");
        if (!isSubclass(cls, base)) fail(TypeError, base.name + ".__new__(" + cls.name + "): " + cls.name + " is not a subtype of " + base.name);
        const v = rt.constructors.get(base)(args, kwargs, cls);
        if (cls === base) return v;
        if (v === null || typeof v !== "object") return { cls: cls, pyval: v, dict: new Map() };
        if (v.cls !== cls) v.cls = cls;
        if (v.dict === undefined || v.dict === null) v.dict = new Map();
        return v;
    };
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
        // Instances always carry a Map dict: the inline caches may serve them.
        // A metaclass's instances are classes, which never take that path.
        cls.userClass = !isSubclass(cls, TypeType);
        cls.qualname = ns.get("__qualname__") || name;
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
            cell = p.self.code(ns);
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
    R.withexit = function (exit, e, line) {
        const exc = normexc(e);
        if (exc.tbline === -1) exc.tbline = line;
        // __exit__ runs while `exc` is being handled: it is the context of
        // anything __exit__ raises.
        excStack.push(exc);
        let r;
        try { r = call(exit, [typeOf(exc), exc, null], null); } finally { excStack.pop(); }
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
    // A class-level dunder (a classmethod such as Enum.__iter__), bound to the
    // class: the nearest classmethod on the MRO. An instance method of the
    // same name nearer the class (Flag.__iter__) serves the instances and
    // does not hide it.
    function classDunder(t, name) {
        for (const c of t.mro) {
            const m = c.dict.get(name);
            if (m !== undefined && m !== null && typeof m === "object" && m.cls === T.classmethod) return bound(m.func, t);
        }
        return undefined;
    }
    rt.classDunder = classDunder;
    // Structural iteration for the builtin containers and their subclasses.
    function baseIter(v) {
        if (typeof v === "string") return { cls: T.iterator, next: stringIter(v) };
        // An exhausted list iterator stays exhausted when the list grows.
        if (v.items !== undefined && (isInstance(v, T.list) || isInstance(v, T.tuple))) { let i = 0; return { cls: T.list_iterator, next: () => { if (i < v.items.length) return v.items[i++]; i = Infinity; return STOP; } }; }
        if (v.map !== undefined && isInstance(v, T.dict)) return rt.dictKeyIter(v);
        if (v.map !== undefined) return rt.setIter(v);
        if (v.pyval !== undefined) return baseIter(v.pyval);
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
    // StopIteration raised by a __next__ becomes STOP. A generator never
    // lets one out (PEP 479, see makeGenerator), so only wrapped user
    // iterators pay for the handler.
    function fornext(it) {
        if (it.next !== undefined) {
            if (it.wrapped === undefined) return it.next();
            try { return it.next(); }
            catch (e) { if (e !== null && typeof e === "object" && e.cls === E.StopIteration) return STOP; throw e; }
        }
        fail(TypeError, "'" + typeOf(it).name + "' object is not an iterator");
    }
    R.iter = iter; R.fornext = fornext; rt.iter = iter; rt.fornext = fornext;
    // Whether iter() failing on `v` means it is not iterable at all (rather
    // than a TypeError raised by its own __iter__).
    function isIterable(v) {
        if (typeof v === "string") return true;
        if (v === null || typeof v !== "object") return false;
        if (v.next !== undefined || v.iter !== undefined) return true;
        return typeMethod(v, "__iter__") !== undefined || typeMethod(v, "__getitem__") !== undefined || (v.isType === true && classDunder(v, "__iter__") !== undefined);
    }
    rt.isIterable = isIterable;
    R.unpack = function (v, count, star) {
        // An exact list or tuple of the right length needs no iterator (the
        // emitter only reads the result). A list is copied: storing the first
        // target can run user code that mutates it before the next is read.
        if (star < 0 && v !== null && typeof v === "object" && v.items !== undefined && v.items.length === count) {
            if (v.cls === T.tuple) return v.items;
            if (v.cls === T.list) return v.items.slice();
        }
        let it;
        try { it = iter(v); }
        catch (e) {
            if (isExcOf(e, TypeError) && !isIterable(v)) fail(TypeError, "cannot unpack non-iterable " + typeOf(v).name + " object");
            throw e;
        }
        const items = [];
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
        try { return getattr(subject, name, UNBOUND); }
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
    // The protocol lives in four shared functions called as members of the
    // generator record (`g.next()`), so creating a generator allocates one
    // object. `returned` holds the return value only for the step that
    // finished the generator (a later next() has no value, as in CPython).
    function isExcOf(e, cls) { return e !== null && typeof e === "object" && e.cls !== undefined && e.cls.mro !== undefined && e.cls.mro.indexOf(cls) >= 0; }
    // PEP 479: a StopIteration escaping the body becomes a RuntimeError.
    function genEscape(g, e) {
        g.running = false; g.done = true; g.returned = null;
        if (!isExcOf(e, E.StopIteration)) return e;
        const err = makeExc(E.RuntimeError, ["generator raised StopIteration"]);
        err.cause = e; err.context = e; err.suppress = true;
        return err;
    }
    function genFinish(g, r) {
        g.running = false; g.done = true;
        g.returned = r.value === undefined ? null : r.value;
    }
    // One resumption. The exceptions a generator is handling are its own
    // frame's state: they go back on the current-exception stack (above the
    // caller's) while it runs, and whatever is still pushed when it yields
    // is taken off again and kept for the next resumption, so the caller's
    // sys.exc_info() and __context__ never see them. `throwing` raises `v`
    // at the paused yield, where it chains to the generator's own exception.
    function genStep(g, throwing, v) {
        const depth = excStack.length, saved = g.excs;
        if (saved !== null) { g.excs = null; for (let i = 0; i < saved.length; i++) excStack.push(saved[i]); }
        if (throwing && v !== null && typeof v === "object") { const cur = rt.currentExc(); if (cur !== null && cur !== v) v.context = cur; }
        let r;
        try { r = throwing ? g.js.throw(v) : g.js.next(v); }
        catch (e) { excStack.length = depth; throw genEscape(g, e); }
        if (!r.done && excStack.length > depth) g.excs = excStack.splice(depth);
        else excStack.length = depth;
        return r;
    }
    // `genStep(g, false, undefined)` written out: this is every `for` step.
    function genNext() {
        const g = this;
        if (g.done) { g.returned = null; return STOP; }
        if (g.running) fail(ValueError, "generator already executing");
        g.running = true; g.started = true;
        const depth = excStack.length, saved = g.excs;
        if (saved !== null) { g.excs = null; for (let i = 0; i < saved.length; i++) excStack.push(saved[i]); }
        let r;
        try { r = g.js.next(undefined); }
        catch (e) { excStack.length = depth; throw genEscape(g, e); }
        if (!r.done && excStack.length > depth) g.excs = excStack.splice(depth);
        else if (excStack.length !== depth) excStack.length = depth;
        if (r.done) { genFinish(g, r); return STOP; }
        g.running = false;
        return r.value;
    }
    function genSend(v) {
        const g = this;
        if (g.running) fail(ValueError, "generator already executing");
        if (g.done) { g.returned = null; throw makeExc(E.StopIteration, []); }
        if (!g.started && v !== null) fail(TypeError, "can't send non-None value to a just-started generator");
        g.running = true; g.started = true;
        const r = genStep(g, false, v);
        if (r.done) { genFinish(g, r); throw makeExc(E.StopIteration, g.returned === null ? [] : [g.returned]); }
        g.running = false;
        return r.value;
    }
    function genThrow(exc) {
        const g = this;
        if (g.running) fail(ValueError, "generator already executing");
        if (g.done) throw exc;
        g.running = true; g.started = true;
        const r = genStep(g, true, exc);
        if (r.done) { genFinish(g, r); throw makeExc(E.StopIteration, g.returned === null ? [] : [g.returned]); }
        g.running = false;
        return r.value;
    }
    // close(): GeneratorExit raised at the paused yield. Returning (or letting
    // GeneratorExit out) closes it; yielding again is an error; any other
    // exception propagates.
    function genClose() {
        const g = this;
        if (g.done) return null;
        if (g.running) fail(ValueError, "generator already executing");
        if (!g.started) { g.done = true; g.js.return(undefined); return null; }
        g.running = true;
        let r;
        try { r = genStep(g, true, makeExc(E.GeneratorExit, [])); }
        catch (e) {
            if (isExcOf(e, E.GeneratorExit)) return null;
            throw e;
        }
        g.running = false;
        if (!r.done) fail(E.RuntimeError, "generator ignored GeneratorExit");
        g.done = true; g.returned = null;
        return r.value === undefined ? null : r.value;
    }
    // In a Python program `next` is the engine's native form of genNext
    // (`vm::py_gen`, `__zipp_py_gen`): it resumes a plain suspended
    // generator itself and calls genNext (`grt[0]`) for every other state.
    // `grt` is what it needs of this runtime, shared by every record.
    let GEN_NEXT = genNext;
    try { if (typeof __zipp_py_gen === "function") GEN_NEXT = __zipp_py_gen; } catch (e) { GEN_NEXT = genNext; }
    const GEN_RT = [genNext, excStack, STOP, genEscape];
    rt.makeGenerator = function (jsgen, f) {
        return { cls: T.generator, js: jsgen, done: false, started: false, running: false, returned: null, excs: null,
            name: f.name, qualname: f.qualname, next: GEN_NEXT, send: genSend, throwIn: genThrow, close: genClose, grt: GEN_RT };
    };

    // ---- modules ----------------------------------------------------------------------------------------
    const inits = new Map(), modules = new Map(), builtinModules = new Map();
    let entryName = "main";
    rt.builtinModules = builtinModules; rt.modules = modules;
    function newModule(name, file) {
        return { cls: T.module, name: name, file: file || null, globals: new Map(), dict: null };
    }
    rt.newModule = newModule;
    rt.moduleAttr = function (m, name, missing) {
        const v = m.globals.get(name);
        if (v !== undefined) return v;
        if (name === "__name__") return m.name;
        if (name === "__file__") return m.file;
        if (name === "__dict__") return rt.namespaceView(m.globals);
        if (m.submodules !== undefined && m.submodules.has(name)) return m.submodules.get(name);
        if (missing !== undefined) return missing;
        fail(E.AttributeError, "module '" + m.name + "' has no attribute '" + name + "'");
    };
    R.module = function (name, code, file) { inits.set(name, { code: code, file: file }); files.push(file); return null; };
    rt.moduleInits = inits;
    R.entry = function (name) { entryName = name; return null; };
    // Dotted names are packages: `a.b.c` imports `a`, then `a.b`, then `a.b.c`
    // (each from the project or the bundled library), and each submodule
    // becomes an attribute of its parent. A folder without `__init__.py` is a
    // namespace package. Builtin modules with dotted names (`os.path`) are
    // attributes of their parent.
    function hasSubmodules(name) {
        const prefix = name + ".";
        for (const k of inits.keys()) if (k.startsWith(prefix)) return true;
        return false;
    }
    R.import = function (name, importerGlobals) {
        if (modules.has(name)) return modules.get(name);
        const dot = name.lastIndexOf(".");
        let parent = null, leaf = name;
        if (dot >= 0) {
            parent = R.import(name.slice(0, dot), importerGlobals);
            leaf = name.slice(dot + 1);
            if (modules.has(name)) return modules.get(name);
        }
        let m;
        const init = inits.get(name);
        if (init !== undefined) m = runModule(name, init, name === entryName ? "__main__" : name);
        else {
            const b = builtinModules.get(name);
            if (b !== undefined) { m = typeof b === "function" ? b() : b; modules.set(name, m); }
            else if (hasSubmodules(name)) { m = newModule(name, null); m.globals.set("__name__", name); m.globals.set("__path__", rt.list([name.replace(/\./g, "/")])); modules.set(name, m); }
            else if (parent !== null && parent.submodules !== undefined && parent.submodules.has(leaf)) { m = parent.submodules.get(leaf); modules.set(name, m); }
            else { const e = makeExc(E.ModuleNotFoundError, ["No module named '" + name + "'"]); e.dict.set("name", name); throw e; }
        }
        if (parent !== null && parent.globals.get(leaf) === undefined) parent.globals.set(leaf, m);
        return m;
    };
    // ---- the virtual filesystem ------------------------------------------------------------
    // Files the host gave the program (root-relative paths, bytes) plus
    // whatever it writes. Nothing here touches a real disk; the host reads
    // the changes back through `__zipp_py_vfs_changed`.
    const vfs = new Map(), vfsChanged = new Set();
    const vfsNorm = function (p) {
        p = String(p).replace(/\\/g, "/");
        const parts = [], segs = p.split("/");
        for (const s of segs) { if (s === "" || s === ".") continue; if (s === "..") { parts.pop(); continue; } parts.push(s); }
        return parts.join("/");
    };
    rt.vfs = {
        norm: vfsNorm,
        has: (p) => vfs.has(vfsNorm(p)),
        get: (p) => vfs.get(vfsNorm(p)),
        set: (p, bytes) => { const n = vfsNorm(p); vfs.set(n, bytes); vfsChanged.add(n); return n; },
        remove: (p) => { const n = vfsNorm(p); const had = vfs.delete(n); if (had) vfsChanged.add(n); return had; },
        isDir: (p) => { const n = vfsNorm(p); if (n === "") return true; const prefix = n + "/"; for (const k of vfs.keys()) if (k.startsWith(prefix)) return true; return dirs.has(n); },
        list: () => Array.from(vfs.keys()),
        listDir: (p) => {
            const n = vfsNorm(p), prefix = n === "" ? "" : n + "/", out = new Set();
            for (const k of vfs.keys()) if (k.startsWith(prefix)) { const rest = k.slice(prefix.length); const i = rest.indexOf("/"); out.add(i < 0 ? rest : rest.slice(0, i)); }
            for (const d of dirs) if (d.startsWith(prefix) && d !== n) { const rest = d.slice(prefix.length); const i = rest.indexOf("/"); out.add(i < 0 ? rest : rest.slice(0, i)); }
            return Array.from(out).sort(rt.compareStrings);
        },
        mkdir: (p) => { dirs.add(vfsNorm(p)); },
        // Removes an empty directory; otherwise says why not: "missing",
        // "file" or "full" (a directory implied by the files under it is never empty).
        rmdir: (p) => { const n = vfsNorm(p); if (vfs.has(n)) return "file"; if (!rt.vfs.isDir(n)) return "missing"; if (n === "" || rt.vfs.listDir(n).length) return "full"; dirs.delete(n); return null; },
        changed: () => { const out = Array.from(vfsChanged); vfsChanged.clear(); return out; },
    };
    const dirs = new Set();
    R.vfs = function (path, base64) {
        vfs.set(vfsNorm(path), Uint8Array.fromBase64(base64));
        return null;
    };
    rt.argv = [];
    rt.version = "0.0.17";
    R.argv = function (items) { rt.argv = items.slice(); return null; };
    function runModule(name, init, dunderName) {
        const m = newModule(name, init.file);
        m.globals.set("__name__", dunderName);
        m.globals.set("__file__", init.file);
        m.globals.set("__doc__", null);
        modules.set(name, m);
        const fn = R.func(init.code, "<module>", "<module>", [], 0, 0, false, false, [], [], [], [], m.globals, false, null, name);
        try {
            fn.code();
        } catch (e) {
            modules.delete(name);
            throw e;
        }
        return m;
    }
    // ---- host requests -----------------------------------------------------------------------
    // A program hands work to its embedder as plain data (`kind`, `payload`)
    // with a callback; the host drains the queue after a call returns and
    // delivers each answer later through `__zipp_py_deliver`. Without a host
    // (`hosted` false) modules settle requests themselves.
    rt.hosted = false;
    R.hosted = function (v) { rt.hosted = v === true; return null; };
    const hostRequests = [], hostCallbacks = new Map();
    let nextRequest = 1;
    const MAX_HOST_PENDING = 64;
    rt.postHost = function (kind, payload, callback) {
        if (hostCallbacks.size >= MAX_HOST_PENDING) fail(E.RuntimeError, "too many host requests are pending (" + MAX_HOST_PENDING + ")");
        const id = nextRequest++;
        hostCallbacks.set(id, callback);
        hostRequests.push({ id: id, kind: kind, payload: payload });
        return id;
    };
    rt.takeHostRequests = function () { const out = hostRequests.slice(); hostRequests.length = 0; return out; };
    rt.deliverHost = function (id, reply) {
        const callback = hostCallbacks.get(id);
        if (callback === undefined) return false;
        hostCallbacks.delete(id);
        call(callback, [reply], null);
        return true;
    };
    rt.pendingHostRequests = function () { return hostCallbacks.size; };
    R.runmain = function (name) {
        const init = inits.get(name);
        if (init === undefined) fail(E.ModuleNotFoundError, name);
        const m = runModule(name, init, "__main__");
        // A test file as the entry runs its tests, as `pytest file.py` would.
        const base = String(init.file || "").split("/").pop();
        if ((/^test_.*\.py$/.test(base) || /_test\.py$/.test(base)) && inits.has("pytest")) {
            const pytest = R.import("pytest", null);
            const run = pytest.globals.get("run_module");
            if (run !== undefined) {
                const status = call(run, [rt.dictFromMap(m.globals), base], null);
                if (rt.truth(status)) throw makeExc(E.SystemExit, [status]);
            }
        }
        return m;
    };
    R.importfrom = function (m, name) {
        const v = getattr(m, name, MISSING_ATTR);
        if (v !== MISSING_ATTR) return v;
        // `from pkg import sub`: a submodule that is not an attribute yet.
        if (m !== null && typeof m === "object" && m.globals !== undefined && typeof m.name === "string") {
            const full = m.name + "." + name;
            if (inits.has(full) || builtinModules.has(full) || hasSubmodules(full)) return R.import(full, null);
        }
        const e = makeExc(E.ImportError, ["cannot import name '" + name + "' from '" + m.name + "'"]);
        e.dict.set("name", m.name);
        throw e;
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
