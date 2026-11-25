/* ZIPP Python runtime — builtin functions, type constructors, methods of the
 * builtin types, and string formatting. Apache-2.0. */
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T, E = rt.E, STOP = rt.STOP, NOTIMPL = rt.NOTIMPL, ObjectType = rt.ObjectType, TypeType = rt.TypeType;
    const fail = rt.fail, typeOf = rt.typeOf, isType = rt.isType, isInstance = rt.isInstance, isSubclass = rt.isSubclass;
    const list = rt.list, tuple = rt.tuple, sequence = rt.sequence, call = rt.call, iter = rt.iter, fornext = rt.fornext;
    const str = rt.str, repr = rt.repr, eq = rt.eq, cmp = rt.cmp, truth = rt.truth, len = rt.len, getitem = rt.getitem;
    const isInt = rt.isInt, asInt = rt.asInt, isNum = rt.isNum, toFloat = rt.toFloat, codepoints = rt.codepoints;
    const dict = rt.dict, dictGet = rt.dictGet, dictSet = rt.dictSet, dictDel = rt.dictDel, dictEntries = rt.dictEntries, dictEntryList = rt.dictEntryList;
    const set = rt.set, setAdd = rt.setAdd, setHas = rt.setHas, setDel = rt.setDel, setValues = rt.setValues, setList = rt.setList, setFrom = rt.setFrom;
    const builtin = rt.builtin, typeMethod = rt.typeMethod, descrGet = rt.descrGet, callMethod = rt.callMethod, keyOf = rt.keyOf;
    const B = rt.builtins;
    function def(name, arity, code, minArity) { B.set(name, builtin(name, arity, code, minArity)); }
    function defkw(name, code) { const f = builtin(name, -1, code); f.kwnames = true; B.set(name, f); }
    function method(type, name, arity, code, minArity) { type.dict.set(name, builtin(name, arity, code, minArity)); }
    function methodkw(type, name, code) { const f = builtin(name, -1, code); f.kwnames = true; type.dict.set(name, f); }
    function drain(iterable) {
        // Exact builtin containers/iterators skip generic iter()+fornext() dispatch.
        if (iterable !== null && typeof iterable === "object") {
            const c = iterable.cls;
            if (c === T.list || c === T.tuple) return iterable.items.slice();
            if (c === T.set || c === T.frozenset) return setList(iterable);
            if (c === T.dict) return rt.dictKeyList(iterable);
            if (c === T.dict_keys) return rt.dictKeyList(iterable.of);
            if (c === T.list_iterator) {
                const items = iterable.seq.items, i = iterable.i;
                iterable.i = -1;
                if (i < 0 || i >= items.length) return [];
                return i === 0 ? items.slice() : items.slice(i);
            }
            if (c === T.generator) {
                const out = [];
                if (rt.NATIVE_GEN_NEXT !== undefined && iterable.next === rt.NATIVE_GEN_NEXT && iterable.next(out) === out) return out;
                for (;;) { const v = iterable.next(); if (v === STOP) break; out.push(v); }
                return out;
            }
            if (rt.iterStep !== null && rt.iterStep !== undefined && iterable.next === rt.iterStep && iterable.wrapped === undefined) {
                const out = [];
                if (iterable.next(out) === out) return out;
            }
        }
        const out = []; const it = iter(iterable); for (;;) { const v = fornext(it); if (v === STOP) break; out.push(v); } return out;
    }
    rt.drain = drain;
    function kwOf(args, allowed) {
        // Builtins accepting keywords receive them as a trailing Map.
        const last = args[args.length - 1];
        if (last instanceof Map) {
            args.pop();
            if (allowed !== null) for (const k of last.keys()) if (allowed.indexOf(k) < 0) fail(E.TypeError, "'" + k + "' is an invalid keyword argument");
            return last;
        }
        return null;
    }
    function kwget(kw, name, dflt) { if (kw === null) return dflt; const v = kw.get(name); return v === undefined ? dflt : v; }
    function needInt(v, what) { if (isInt(v)) return asInt(v); if (v !== null && typeof v === "object" && typeof v.pyval === "bigint") return v.pyval; fail(E.TypeError, "'" + typeOf(v).name + "' object cannot be interpreted as an integer" + (what ? " (" + what + ")" : "")); }
    function needStr(v, what) { if (typeof v === "string") return v; if (v !== null && typeof v === "object" && typeof v.pyval === "string") return v.pyval; fail(E.TypeError, (what || "argument") + " must be str, not " + typeOf(v).name); }
    function toIndex(v) { return Number(rt.indexOf(v)); }
    rt.needInt = needInt; rt.needStr = needStr; rt.kwOf = kwOf; rt.kwget = kwget;

    // ---- object ------------------------------------------------------------------------------------------------
    method(ObjectType, "__init__", -1, function (args) { return null; });
    ObjectType.dict.set("__new__", { cls: T.staticmethod, func: builtin("__new__", -1, function (args) {
        const cls = args[0]; if (!isType(cls)) fail(E.TypeError, "object.__new__(X): X is not a type object");
        return rt.allocInstance(cls);
    }) });
    method(ObjectType, "__eq__", 2, function (args) { return args[0] === args[1] ? true : NOTIMPL; });
    method(ObjectType, "__ne__", 2, function (args) { const r = rt.callMethod(args[0], "__eq__", [args[1]]); return r === NOTIMPL ? NOTIMPL : !truth(r); });
    method(ObjectType, "__hash__", 1, function (args) { return BigInt(rt.ident(args[0])); });
    method(ObjectType, "__repr__", 1, function (args) { const v = args[0]; const c = typeOf(v); return "<" + (c.module === "builtins" ? "" : c.module + ".") + c.qualname + " object at 0x" + rt.ident(v).toString(16).padStart(8, "0") + ">"; });
    method(ObjectType, "__str__", 1, function (args) { return repr(args[0]); });
    // A class (a metaclass's `super().__setattr__`) goes through setattr so
    // the lookup caches see the change.
    method(ObjectType, "__setattr__", 3, function (args) { const o = args[0]; if (o !== null && typeof o === "object" && o.isType === true) return rt.setattr(o, args[1], args[2]); if (o === null || typeof o !== "object" || o.dict === undefined) fail(E.AttributeError, "can't set attribute"); if (o.dict === null) { if (o.cls === T.function) o.dict = new Map(); else fail(E.AttributeError, "can't set attribute"); } o.dict.set(args[1], args[2]); return null; });
    method(ObjectType, "__getattribute__", 2, function (args) { return rt.objectGetattr(args[0], args[1]); });
    method(ObjectType, "__delattr__", 2, function (args) { const o = args[0]; if (o !== null && typeof o === "object" && o.isType === true) return R.delattr(o, args[1]); if (o === null || typeof o !== "object" || o.dict === undefined || o.dict === null || !o.dict.delete(args[1])) fail(E.AttributeError, "'" + typeOf(o).name + "' object has no attribute '" + args[1] + "'"); return null; });
    method(ObjectType, "__format__", 2, function (args) { if (args[1] !== "") fail(E.TypeError, "unsupported format string passed to " + typeOf(args[0]).name + ".__format__"); return str(args[0]); });
    method(ObjectType, "__init_subclass__", -1, function () { return null; });
    method(ObjectType, "__dir__", 1, function (args) { return list(dirOf(args[0])); });
    method(ObjectType, "__reduce__", 1, function () { fail(E.TypeError, "cannot pickle"); });
    // `obj.__class__ = Other`: for instances whose storage layout matches.
    const layoutOf = (cls) => { for (const c of cls.mro) if (rt.allocators.has(c)) return isSubclass(c, E.BaseException) ? E.BaseException : c; return ObjectType; };
    const setClass = builtin("__class__", 2, (a) => {
        const o = a[0], cls = a[1];
        if (!isType(cls)) fail(E.TypeError, "__class__ must be set to a class, not '" + typeOf(cls).name + "' object");
        if (o === null || typeof o !== "object" || o.isType || o.dict === undefined || o.dict === null || typeOf(o).mro.indexOf(ObjectType) < 0 || rt.constructors.has(o.cls)) fail(E.TypeError, "__class__ assignment only supported for mutable types or ModuleType subclasses");
        if (layoutOf(o.cls) !== layoutOf(cls) || (o.cls.noDict === true) !== (cls.noDict === true)) fail(E.TypeError, "__class__ assignment: '" + cls.name + "' object layout differs from '" + o.cls.name + "'");
        o.cls = cls;
        return null;
    });
    ObjectType.dict.set("__class__", { cls: T.property, fget: builtin("__class__", 1, (a) => typeOf(a[0])), fset: setClass, fdel: null, doc: null });
    method(ObjectType, "__sizeof__", 1, function () { return 64n; });
    function dirOf(v) {
        const names = new Set();
        const t = typeOf(v);
        for (const c of t.mro) for (const k of c.dict.keys()) names.add(k);
        if (v !== null && typeof v === "object" && v.dict) for (const k of v.dict.keys()) names.add(k);
        if (v !== null && typeof v === "object" && v.isType) for (const c of v.mro) for (const k of c.dict.keys()) names.add(k);
        if (v !== null && typeof v === "object" && v.cls === T.module) for (const k of v.globals.keys()) names.add(k);
        return Array.from(names).sort(rt.compareStrings);
    }
    // Exceptions: BaseException.__init__ stores args.
    method(E.BaseException, "__init__", -1, function (args) {
        const self = args[0]; self.args = tuple(args.slice(1)); return null;
    });
    method(E.BaseException, "__str__", 1, function (args) { const a = args[0].args.items; return a.length === 0 ? "" : a.length === 1 ? str(a[0]) : repr(args[0].args); });
    method(E.KeyError, "__str__", 1, function (args) { const a = args[0].args.items; return a.length === 1 ? repr(a[0]) : a.length === 0 ? "" : repr(args[0].args); });
    method(E.BaseException, "__repr__", 1, function (args) { return typeOf(args[0]).name + "(" + args[0].args.items.map(repr).join(", ") + ")"; });
    // OSError(errno, strerror[, filename[, winerror[, filename2]]]): the
    // errno attributes, "[Errno N] message" text, and (constructed as OSError
    // itself) the subclass for that errno.
    const ERRNO_CLASS = new Map([[1n, E.PermissionError], [2n, E.FileNotFoundError], [13n, E.PermissionError], [17n, E.FileExistsError], [20n, E.NotADirectoryError], [21n, E.IsADirectoryError], [110n, E.TimeoutError]]);
    method(E.OSError, "__init__", -1, function (args) {
        const self = args[0], a = args.slice(1);
        const d = self.dict;
        for (const k of ["errno", "strerror", "filename", "filename2"]) d.set(k, null);
        if (a.length >= 2 && a.length <= 5) {
            d.set("errno", a[0]); d.set("strerror", a[1]);
            if (a.length >= 3) d.set("filename", a[2]);
            if (a.length === 5) d.set("filename2", a[4]);
            if (self.cls === E.OSError && isInt(a[0])) { const sub = ERRNO_CLASS.get(asInt(a[0])); if (sub !== undefined) self.cls = sub; }
            self.args = tuple(a.slice(0, 2));
        } else self.args = tuple(a);
        return null;
    });
    method(E.OSError, "__str__", 1, function (args) {
        const self = args[0], d = self.dict;
        const filename = d.get("filename"), filename2 = d.get("filename2");
        if (filename !== undefined && filename !== null) return "[Errno " + str(d.get("errno")) + "] " + str(d.get("strerror")) + ": " + repr(filename) + (filename2 !== undefined && filename2 !== null ? " -> " + repr(filename2) : "");
        if (d.get("errno") !== undefined && d.get("errno") !== null && self.args.items.length === 2) return "[Errno " + str(d.get("errno")) + "] " + str(d.get("strerror"));
        const it = self.args.items; return it.length === 0 ? "" : it.length === 1 ? str(it[0]) : repr(self.args);
    });
    method(E.BaseException, "with_traceback", 2, function (args) { return args[0]; });
    method(E.BaseException, "add_note", 2, function (args) {
        const self = args[0], note = args[1];
        if (typeof note !== "string") fail(E.TypeError, "note must be a str, not '" + typeOf(note).name + "'");
        let notes = self.dict.get("__notes__");
        if (notes === undefined) { notes = list([]); self.dict.set("__notes__", notes); }
        else if (notes === null || typeof notes !== "object" || notes.cls !== T.list) {
            rt.call(rt.getattr(notes, "append"), [note], null);
            return null;
        }
        notes.items.push(note);
        return null;
    });
    // A constructed exception has no context until it is raised. (One
    // allocator for all, so construction can recognize it: `excEntry`.)
    const excAlloc = (cls) => { const e = rt.makeExc(cls, []); e.context = null; return e; };
    rt.excAlloc = excAlloc; rt.excInit = E.BaseException.dict.get("__init__");
    for (const [name, exc] of Object.entries(E)) {
        rt.allocators.set(exc, excAlloc);
        B.set(name, exc);
    }
    B.set("EnvironmentError", E.OSError); B.set("IOError", E.OSError);
    // Constructing an exception directly: Exception("msg") -> instance with args.
    rt.constructors.set(E.BaseException, null);
    rt.constructors.delete(E.BaseException);

    // ---- type / super / property / staticmethod / classmethod -------------------------------------------------------
    B.set("object", ObjectType); B.set("type", TypeType);
    for (const name of ["int", "float", "str", "bool", "list", "tuple", "dict", "set", "frozenset", "range", "bytes", "slice", "property", "staticmethod", "classmethod", "super", "enumerate", "zip", "map", "filter", "reversed"]) B.set(name, T[name]);
    B.set("NotImplemented", NOTIMPL); B.set("Ellipsis", rt.ELLIPSIS); B.set("None", null); B.set("True", true); B.set("False", false);
    methodkw(TypeType, "__call__", function (args) { const kw = args[args.length - 1] instanceof Map ? args.pop() : null; return rt.constructDefault(args[0], args.slice(1), kw); });
    methodkw(TypeType, "__new__", function (args) {
        const kw = args[args.length - 1] instanceof Map ? args.pop() : null;
        if (args.length === 2 && (kw === null || kw.size === 0)) return rt.typeOf(args[1]);
        if (args.length !== 4) fail(E.TypeError, "type.__new__() takes exactly 3 arguments (" + (args.length - 1) + " given)");
        return rt.makeClass(args[0], args[1], args[2], args[3], kw);
    });
    methodkw(TypeType, "__init__", function () { return null; });
    // `int | None`: a union type (PEP 604) usable in isinstance(), plus
    // types.GenericAlias and PEP 695/696 typing runtime types.
    const UnionType = rt.newType("UnionType", [ObjectType], new Map(), "types");
    const GenericAlias = rt.newType("GenericAlias", [ObjectType], new Map(), "types");
    const NoDefaultType = rt.newType("_NoDefaultType", [ObjectType], new Map(), "typing");
    NoDefaultType.dict.set("__repr__", builtin("__repr__", 1, () => "typing.NoDefault"));
    const NoDefault = { cls: NoDefaultType, dict: new Map() };
    const TypeVar = rt.newType("TypeVar", [ObjectType], new Map(), "typing");
    const ParamSpec = rt.newType("ParamSpec", [ObjectType], new Map(), "typing");
    const ParamSpecArgs = rt.newType("ParamSpecArgs", [ObjectType], new Map(), "typing");
    const ParamSpecKwargs = rt.newType("ParamSpecKwargs", [ObjectType], new Map(), "typing");
    const TypeVarTuple = rt.newType("TypeVarTuple", [ObjectType], new Map(), "typing");
    const TypeAliasType = rt.newType("TypeAliasType", [ObjectType], new Map(), "typing");
    const Generic = rt.newType("Generic", [ObjectType], new Map(), "typing");
    const UnpackType = rt.newType("_UnpackGenericAlias", [ObjectType], new Map(), "typing");
    const UnpackSpecial = rt.newType("_UnpackForm", [ObjectType], new Map(), "typing");
    const Unpack = { cls: UnpackSpecial, dict: new Map([["name", "Unpack"]]) };

    const isTypeParam = (v) => v !== null && typeof v === "object" && (v.cls === TypeVar || v.cls === ParamSpec || v.cls === TypeVarTuple);
    const isUnionOperand = (x) => {
        if (x === null || isType(x)) return true;
        if (x !== null && typeof x === "object") {
            const c = x.cls;
            if (c === UnionType || c === GenericAlias || c === TypeVar || c === ParamSpec || c === TypeAliasType) return true;
            if (c !== undefined && (c.module === "typing" || rt.typeOf(x).name === "_Alias")) return true;
        }
        return false;
    };
    const unionArgs = (v) => v !== null && typeof v === "object" && v.cls === UnionType ? v.dict.get("__args__").items : [v];
    const isTypingOnlyUnionArg = (x) => x !== null && typeof x === "object" && (x.cls === TypeVar || x.cls === ParamSpec || x.cls === TypeAliasType || (x.cls !== undefined && (x.cls.module === "typing" || rt.typeOf(x).name === "_Alias")));
    const makeUnion = (a, b) => {
        if (!isUnionOperand(a) || !isUnionOperand(b)) return NOTIMPL;
        const args = []; for (const x of unionArgs(a).concat(unionArgs(b))) if (!args.includes(x)) args.push(x);
        const typingUnion = args.some(isTypingOnlyUnionArg);
        return { cls: UnionType, dict: new Map([["__args__", tuple(args)]]), typingUnion: typingUnion };
    };
    rt.makeUnion = makeUnion;
    const gaName = (x) => {
        if (x === rt.ELLIPSIS) return "...";
        if (x === null) return "None";
        if (isType(x)) return x.module === "builtins" ? x.qualname : x.module + "." + x.qualname;
        return repr(x);
    };
    const collectParams = (items, out) => {
        for (let i = 0; i < items.length; i++) {
            const x = items[i];
            if (isTypeParam(x)) { if (!out.includes(x)) out.push(x); }
            else if (x !== null && typeof x === "object" && x.dict !== undefined && x.dict !== null) {
                const p = x.dict.get("__parameters__");
                if (p !== undefined && p !== null && p.cls === T.tuple) {
                    for (const t of p.items) if (!out.includes(t)) out.push(t);
                }
            }
        }
    };
    const makeGenericAlias = (origin, rawArgs) => {
        const isSingle = !(rawArgs !== null && typeof rawArgs === "object" && rawArgs.cls === T.tuple);
        if (isSingle && origin !== null && typeof origin === "object" && origin.isType === true && origin.cls === TypeType) {
            const cache = origin._subCache;
            if (cache !== undefined) {
                const hit = cache.get(rawArgs);
                if (hit !== undefined) return hit;
            }
        }
        const items = isSingle ? [rawArgs] : rawArgs.items.slice();
        const params = [];
        collectParams(items, params);
        const res = {
            cls: GenericAlias,
            dict: new Map([
                ["__origin__", origin],
                ["__args__", tuple(items)],
                ["__parameters__", tuple(params)],
            ]),
        };
        if (isSingle && origin !== null && typeof origin === "object" && origin.isType === true && origin.cls === TypeType) {
            let cache = origin._subCache;
            if (cache === undefined) cache = origin._subCache = new Map();
            if (cache.size < 256) cache.set(rawArgs, res);
            rt.noteFlagged(origin);
        }
        return res;
    };
    const makeUnpack = (target) => {
        const params = [];
        collectParams([target], params);
        return {
            cls: UnpackType,
            dict: new Map([
                ["__origin__", Unpack],
                ["__args__", tuple([target])],
                ["__parameters__", tuple(params)],
            ]),
        };
    };
    UnpackSpecial.dict.set("__getitem__", builtin("__getitem__", 2, (a) => makeUnpack(a[1])));
    UnpackSpecial.dict.set("__repr__", builtin("__repr__", 1, () => "typing.Unpack"));
    UnpackType.dict.set("__repr__", builtin("__repr__", 1, (a) => "typing.Unpack[" + gaName(a[0].dict.get("__args__").items[0]) + "]"));
    UnpackType.dict.set("__eq__", builtin("__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && a[1].cls === UnpackType && rt.eq(a[0].dict.get("__args__"), a[1].dict.get("__args__"))));
    UnpackType.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(tuple([Unpack, a[0].dict.get("__args__")]))));

    const substArg = (arg, map) => {
        if (map.has(arg)) return map.get(arg);
        if (arg !== null && typeof arg === "object" && arg.cls === GenericAlias) {
            const sub = arg.dict.get("__args__").items.map((x) => substArg(x, map));
            return makeGenericAlias(arg.dict.get("__origin__"), tuple(sub));
        }
        if (arg !== null && typeof arg === "object" && arg.cls === UnionType) {
            const sub = arg.dict.get("__args__").items.map((x) => substArg(x, map));
            let acc = sub[0];
            for (let i = 1; i < sub.length; i++) acc = makeUnion(acc, sub[i]);
            return acc;
        }
        return arg;
    };
    rt.constructors.set(GenericAlias, (args) => {
        if (args.length !== 2) fail(E.TypeError, "GenericAlias expected 2 arguments, got " + args.length);
        return makeGenericAlias(args[0], args[1]);
    });
    GenericAlias.dict.set("__repr__", builtin("__repr__", 1, (a) => {
        const o = a[0].dict.get("__origin__"), xs = a[0].dict.get("__args__").items;
        return gaName(o) + "[" + xs.map(gaName).join(", ") + "]";
    }));
    GenericAlias.dict.set("__eq__", builtin("__eq__", 2, (a) => {
        const y = a[1];
        if (y === null || typeof y !== "object" || y.cls !== GenericAlias) return NOTIMPL;
        return rt.eq(a[0].dict.get("__origin__"), y.dict.get("__origin__")) && rt.eq(a[0].dict.get("__args__"), y.dict.get("__args__"));
    }));
    GenericAlias.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(tuple([a[0].dict.get("__origin__"), a[0].dict.get("__args__")]))));
    GenericAlias.dict.set("__or__", builtin("__or__", 2, (a) => makeUnion(a[0], a[1])));
    GenericAlias.dict.set("__ror__", builtin("__ror__", 2, (a) => makeUnion(a[1], a[0])));
    GenericAlias.dict.set("__call__", (() => {
        const f = builtin("__call__", -1, (a) => {
            const kw = a[a.length - 1] instanceof Map ? a.pop() : null;
            return rt.call(a[0].dict.get("__origin__"), a.slice(1), kw);
        });
        f.kwnames = true;
        return f;
    })());
    GenericAlias.dict.set("__mro_entries__", builtin("__mro_entries__", 2, (a) => {
        const self = a[0], origin = self.dict.get("__origin__"), bases = a[1];
        if (origin === Generic) {
            if (bases !== null && typeof bases === "object" && bases.cls === T.tuple) {
                for (const b of bases.items) {
                    if (b === self) continue;
                    const t = isType(b) ? b : (b !== null && typeof b === "object" && b.cls === GenericAlias && isType(b.dict.get("__origin__")) ? b.dict.get("__origin__") : null);
                    if (t !== null && isSubclass(t, Generic)) return tuple([]);
                }
            }
            return tuple([Generic]);
        }
        return tuple([origin]);
    }));
    GenericAlias.dict.set("__getitem__", builtin("__getitem__", 2, (a) => {
        const self = a[0], params = self.dict.get("__parameters__").items;
        const vals = a[1] !== null && typeof a[1] === "object" && a[1].cls === T.tuple ? a[1].items : [a[1]];
        if (!params.length) fail(E.TypeError, repr(self) + " is not a generic class");
        if (vals.length !== params.length) fail(E.TypeError, "Too " + (vals.length < params.length ? "few" : "many") + " arguments for " + repr(self) + "; actual " + vals.length + ", expected " + params.length);
        const map = new Map();
        for (let i = 0; i < params.length; i++) map.set(params[i], vals[i]);
        const nextArgs = self.dict.get("__args__").items.map((x) => substArg(x, map));
        return makeGenericAlias(self.dict.get("__origin__"), tuple(nextArgs));
    }));
    GenericAlias.dict.set("__getattr__", builtin("__getattr__", 2, (a) => {
        const name = a[1];
        if (typeof name === "string" && (name === "__origin__" || name === "__args__" || name === "__parameters__")) fail(E.AttributeError, name);
        return rt.getattr(a[0].dict.get("__origin__"), name);
    }));
    Generic.dict.set("__class_getitem__", { cls: T.classmethod, func: builtin("__class_getitem__", 2, (a) => makeGenericAlias(a[0], a[1])) });

    // TypeVar / ParamSpec / TypeVarTuple / TypeAliasType (PEP 695 & 696).
    const evalLazy = (obj, key, fnKey, fallback) => {
        if (obj.dict.has(key)) return obj.dict.get(key);
        const fn = obj[fnKey];
        if (fn === null || fn === undefined) return fallback;
        const v = rt.call(fn, [], null);
        obj.dict.set(key, v);
        return v;
    };
    const makeTypeVar = (name, boundFn, isConstraints, defaultFn, cov, contra, infer, eagerBound, eagerConstraints, eagerDefault, hasDefault) => {
        const obj = {
            cls: TypeVar,
            dict: new Map([
                ["__name__", name],
                ["__covariant__", !!cov],
                ["__contravariant__", !!contra],
                ["__infer_variance__", !!infer],
            ]),
            _boundFn: !isConstraints ? boundFn : null,
            _constraintsFn: isConstraints ? boundFn : null,
            _defaultFn: defaultFn,
            _hasDefault: !!hasDefault,
        };
        if (eagerBound !== undefined) obj.dict.set("_bound", eagerBound);
        if (eagerConstraints !== undefined) obj.dict.set("_constraints", eagerConstraints);
        if (eagerDefault !== undefined) obj.dict.set("_default", eagerDefault);
        return obj;
    };
    rt.constructors.set(TypeVar, (args, kw) => {
        if (args.length < 1) fail(E.TypeError, "TypeVar() missing 1 required positional argument: 'name'");
        const name = needStr(args[0]);
        const constraints = args.slice(1);
        const bound = kw && kw.has("bound") ? kw.get("bound") : null;
        const cov = kw && kw.has("covariant") ? truth(kw.get("covariant")) : false;
        const contra = kw && kw.has("contravariant") ? truth(kw.get("contravariant")) : false;
        const infer = kw && kw.has("infer_variance") ? truth(kw.get("infer_variance")) : false;
        const hasDef = kw && kw.has("default") && kw.get("default") !== NoDefault;
        const defVal = hasDef ? kw.get("default") : NoDefault;
        return makeTypeVar(name, null, false, null, cov, contra, infer, bound, tuple(constraints), defVal, hasDef);
    });
    TypeVar.dict.set("__bound__", { cls: T.property, fget: builtin("__bound__", 1, (a) => evalLazy(a[0], "_bound", "_boundFn", null)), fset: null, fdel: null, doc: null });
    TypeVar.dict.set("__constraints__", { cls: T.property, fget: builtin("__constraints__", 1, (a) => {
        const v = evalLazy(a[0], "_constraints", "_constraintsFn", tuple([]));
        return v !== null && typeof v === "object" && v.cls === T.tuple ? v : tuple([]);
    }), fset: null, fdel: null, doc: null });
    TypeVar.dict.set("__default__", { cls: T.property, fget: builtin("__default__", 1, (a) => evalLazy(a[0], "_default", "_defaultFn", NoDefault)), fset: null, fdel: null, doc: null });
    TypeVar.dict.set("has_default", builtin("has_default", 1, (a) => a[0]._hasDefault === true));
    TypeVar.dict.set("__repr__", builtin("__repr__", 1, (a) => {
        const d = a[0].dict;
        const p = d.get("__infer_variance__") ? "" : d.get("__covariant__") ? "+" : d.get("__contravariant__") ? "-" : "~";
        return p + d.get("__name__");
    }));
    TypeVar.dict.set("__or__", builtin("__or__", 2, (a) => makeUnion(a[0], a[1])));
    TypeVar.dict.set("__ror__", builtin("__ror__", 2, (a) => makeUnion(a[1], a[0])));

    ParamSpecArgs.dict.set("__repr__", builtin("__repr__", 1, (a) => a[0].dict.get("__origin__").dict.get("__name__") + ".args"));
    ParamSpecKwargs.dict.set("__repr__", builtin("__repr__", 1, (a) => a[0].dict.get("__origin__").dict.get("__name__") + ".kwargs"));
    const makeParamSpec = (name, defaultFn, infer, eagerDefault, hasDefault) => {
        const obj = {
            cls: ParamSpec,
            dict: new Map([
                ["__name__", name],
                ["__covariant__", false],
                ["__contravariant__", false],
                ["__infer_variance__", !!infer],
            ]),
            _defaultFn: defaultFn,
            _hasDefault: !!hasDefault,
        };
        obj.dict.set("args", { cls: ParamSpecArgs, dict: new Map([["__origin__", obj]]) });
        obj.dict.set("kwargs", { cls: ParamSpecKwargs, dict: new Map([["__origin__", obj]]) });
        if (eagerDefault !== undefined) obj.dict.set("_default", eagerDefault);
        return obj;
    };
    rt.constructors.set(ParamSpec, (args, kw) => {
        if (args.length < 1) fail(E.TypeError, "ParamSpec() missing 1 required positional argument: 'name'");
        const name = needStr(args[0]);
        const hasDef = kw && kw.has("default") && kw.get("default") !== NoDefault;
        const defVal = hasDef ? kw.get("default") : NoDefault;
        return makeParamSpec(name, null, false, defVal, hasDef);
    });
    ParamSpec.dict.set("__default__", { cls: T.property, fget: builtin("__default__", 1, (a) => evalLazy(a[0], "_default", "_defaultFn", NoDefault)), fset: null, fdel: null, doc: null });
    ParamSpec.dict.set("has_default", builtin("has_default", 1, (a) => a[0]._hasDefault === true));
    ParamSpec.dict.set("__repr__", builtin("__repr__", 1, (a) => (a[0].dict.get("__infer_variance__") ? "" : "~") + a[0].dict.get("__name__")));
    ParamSpec.dict.set("__or__", builtin("__or__", 2, (a) => makeUnion(a[0], a[1])));
    ParamSpec.dict.set("__ror__", builtin("__ror__", 2, (a) => makeUnion(a[1], a[0])));

    const makeTypeVarTuple = (name, defaultFn, eagerDefault, hasDefault) => {
        const obj = {
            cls: TypeVarTuple,
            dict: new Map([["__name__", name]]),
            _defaultFn: defaultFn,
            _hasDefault: !!hasDefault,
        };
        if (eagerDefault !== undefined) obj.dict.set("_default", eagerDefault);
        return obj;
    };
    rt.constructors.set(TypeVarTuple, (args, kw) => {
        if (args.length < 1) fail(E.TypeError, "TypeVarTuple() missing 1 required positional argument: 'name'");
        const name = needStr(args[0]);
        const hasDef = kw && kw.has("default") && kw.get("default") !== NoDefault;
        const defVal = hasDef ? kw.get("default") : NoDefault;
        return makeTypeVarTuple(name, null, defVal, hasDef);
    });
    TypeVarTuple.dict.set("__default__", { cls: T.property, fget: builtin("__default__", 1, (a) => evalLazy(a[0], "_default", "_defaultFn", NoDefault)), fset: null, fdel: null, doc: null });
    TypeVarTuple.dict.set("has_default", builtin("has_default", 1, (a) => a[0]._hasDefault === true));
    TypeVarTuple.dict.set("__iter__", builtin("__iter__", 1, (a) => { let done = false; return { cls: T.iterator, next: () => { if (done) return STOP; done = true; return makeUnpack(a[0]); } }; }));
    TypeVarTuple.dict.set("__repr__", builtin("__repr__", 1, (a) => a[0].dict.get("__name__")));

    const makeTypeAlias = (name, valueFn, typeParams, modName, eagerValue) => {
        const tp = typeParams !== null && typeParams !== undefined && typeParams.cls === T.tuple ? typeParams : tuple([]);
        const obj = {
            cls: TypeAliasType,
            dict: new Map([
                ["__name__", name],
                ["__type_params__", tp],
                ["__parameters__", tp],
                ["__module__", modName || "__main__"],
            ]),
            _valueFn: valueFn,
        };
        if (eagerValue !== undefined) obj.dict.set("_value", eagerValue);
        return obj;
    };
    rt.constructors.set(TypeAliasType, (args, kw) => {
        if (args.length < 2) fail(E.TypeError, "TypeAliasType() takes at least 2 arguments");
        const tp = kw && kw.has("type_params") ? tuple(drain(kw.get("type_params"))) : tuple([]);
        return makeTypeAlias(needStr(args[0]), null, tp, "__main__", args[1]);
    });
    TypeAliasType.dict.set("__value__", { cls: T.property, fget: builtin("__value__", 1, (a) => evalLazy(a[0], "_value", "_valueFn", null)), fset: null, fdel: null, doc: null });
    TypeAliasType.dict.set("__getitem__", builtin("__getitem__", 2, (a) => {
        const tp = a[0].dict.get("__type_params__").items;
        if (!tp.length) fail(E.TypeError, "Only generic type aliases are subscriptable");
        return makeGenericAlias(a[0], a[1]);
    }));
    TypeAliasType.dict.set("__or__", builtin("__or__", 2, (a) => makeUnion(a[0], a[1])));
    TypeAliasType.dict.set("__ror__", builtin("__ror__", 2, (a) => makeUnion(a[1], a[0])));
    TypeAliasType.dict.set("__repr__", builtin("__repr__", 1, (a) => a[0].dict.get("__name__")));

    // Builtin generic types support `list[int]`, `dict[str, int]`, etc.
    const cgBuiltin = { cls: T.classmethod, func: builtin("__class_getitem__", 2, (a) => makeGenericAlias(a[0], a[1])) };
    for (const gt of [T.list, T.tuple, T.dict, T.set, T.frozenset, TypeType, T.dict_keys, T.dict_values, T.dict_items]) {
        gt.dict.set("__class_getitem__", cgBuiltin);
    }

    // PEP 695/696 emitter helpers (available without importing typing).
    R.typevar = function (name, boundFn, isConstraints, defaultFn) {
        return makeTypeVar(name, boundFn, isConstraints, defaultFn, false, false, true, undefined, undefined, undefined, defaultFn !== null);
    };
    R.paramspec = function (name, defaultFn) {
        return makeParamSpec(name, defaultFn, true, undefined, defaultFn !== null);
    };
    R.typevartuple = function (name, defaultFn) {
        return makeTypeVarTuple(name, defaultFn, undefined, defaultFn !== null);
    };
    R.typealias = function (name, valueFn, typeParams, modName) {
        return makeTypeAlias(name, valueFn, typeParams, modName, undefined);
    };
    R.generic_base = function (bases, tps) {
        for (let i = 0; i < bases.length; i++) {
            const b = bases[i];
            if (b === Generic) return bases;
            if (b !== null && typeof b === "object" && b.cls === GenericAlias && b.dict.get("__origin__") === Generic) return bases;
        }
        bases.push(makeGenericAlias(Generic, tps));
        return bases;
    };
    R.set_type_params = function (obj, tps) {
        if (obj !== null && typeof obj === "object") {
            if (obj.isType) obj.dict.set("__type_params__", tps);
            else obj.type_params = tps;
        }
        return obj;
    };
    R.unpack_type = function (v) { return makeUnpack(v); };

    method(TypeType, "__or__", 2, (a) => makeUnion(a[0], a[1]));
    method(TypeType, "__ror__", 2, (a) => makeUnion(a[1], a[0]));
    UnionType.dict.set("__or__", builtin("__or__", 2, (a) => makeUnion(a[0], a[1])));
    UnionType.dict.set("__ror__", builtin("__ror__", 2, (a) => makeUnion(a[1], a[0])));
    UnionType.dict.set("__repr__", builtin("__repr__", 1, (a) => {
        const items = a[0].dict.get("__args__").items;
        if (a[0].typingUnion) return "typing.Union[" + items.map(gaName).join(", ") + "]";
        return items.map((x) => x === null ? "None" : isType(x) ? x.name : repr(x)).join(" | ");
    }));
    UnionType.dict.set("__eq__", builtin("__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && a[1].cls === UnionType && rt.eq(a[0].dict.get("__args__"), a[1].dict.get("__args__"))));
    UnionType.dict.set("__hash__", builtin("__hash__", 1, (a) => rt.hashInt(a[0].dict.get("__args__"))));
    rt.UnionType = UnionType;
    rt.GenericAlias = GenericAlias;
    rt.makeGenericAlias = makeGenericAlias;
    rt.NoDefault = NoDefault;
    rt.TypeVar = TypeVar;
    rt.ParamSpec = ParamSpec;
    rt.TypeVarTuple = TypeVarTuple;
    rt.TypeAliasType = TypeAliasType;
    rt.Generic = Generic;
    rt.Unpack = Unpack;
    method(TypeType, "__prepare__", -1, function () { return dict(); });
    method(TypeType, "__repr__", 1, function (args) { return repr(args[0]); });
    method(TypeType, "mro", 1, function (args) { return list(args[0].mro.slice()); });
    method(TypeType, "__subclasses__", 1, function () { return list([]); });
    method(TypeType, "__instancecheck__", 2, function (args) { return isInstance(args[1], args[0]); });
    rt.constructors.set(T.property, (args, kw) => {
        const k = kw; const get = (n, i) => { if (k && k.has(n)) return k.get(n); return args[i] === undefined ? null : args[i]; };
        return { cls: T.property, fget: get("fget", 0), fset: get("fset", 1), fdel: get("fdel", 2), doc: get("doc", 3) };
    });
    method(T.property, "getter", 2, (a) => ({ cls: T.property, fget: a[1], fset: a[0].fset, fdel: a[0].fdel, doc: a[0].doc }));
    method(T.property, "setter", 2, (a) => ({ cls: T.property, fget: a[0].fget, fset: a[1], fdel: a[0].fdel, doc: a[0].doc }));
    method(T.property, "deleter", 2, (a) => ({ cls: T.property, fget: a[0].fget, fset: a[0].fset, fdel: a[1], doc: a[0].doc }));
    // The descriptor protocol of the builtin descriptors, callable directly
    // (`C.__dict__["m"].__get__(obj, C)`); attribute lookup itself goes
    // through descrGet.
    const descrGetMethod = (a) => {
        const owner = a[2] === undefined || a[2] === null ? (a[1] === null ? null : typeOf(a[1])) : a[2];
        if (a[1] === null && owner === null) fail(E.TypeError, "__get__(None, None) is invalid");
        const d = a[0];
        if (d.cls === T.classmethod) return rt.bound(d.func, owner);
        return descrGet(d, a[1], owner);
    };
    for (const t of [T.function, T.classmethod, T.staticmethod, T.property]) method(t, "__get__", 3, descrGetMethod, 2);
    rt.constructors.set(T.staticmethod, (args) => ({ cls: T.staticmethod, func: args[0] }));
    rt.constructors.set(T.classmethod, (args) => ({ cls: T.classmethod, func: args[0] }));
    rt.constructors.set(T.super, (args) => {
        if (args.length !== 2) fail(E.RuntimeError, "super(): use super() or super(type, obj)");
        return R.superof(args[0], args[1]);
    });
    method(T.super, "__repr__", 1, (a) => "<super: <class '" + a[0].type.name + "'>, <" + typeOf(a[0].obj).name + " object>>");

    // ---- numbers ---------------------------------------------------------------------------------------------------
    // int(str, base) as CPython parses it: an optional sign, an optional
    // base prefix (when base is 0 or matches), single underscores only
    // between digits or right after the prefix, and no leading zeros in a
    // base-0 decimal other than zero itself.
    function parseIntLiteral(s, base) {
        const bad = () => fail(E.ValueError, "invalid literal for int() with base " + base + ": " + repr(s));
        let t = s.trim();
        let neg = false;
        if (t[0] === "+" || t[0] === "-") { neg = t[0] === "-"; t = t.slice(1); }
        const p = t.slice(0, 2).toLowerCase();
        let radix = base, prefixed = false;
        if (base === 0) {
            radix = p === "0x" ? 16 : p === "0o" ? 8 : p === "0b" ? 2 : 10;
            prefixed = radix !== 10;
        } else prefixed = (base === 16 && p === "0x") || (base === 8 && p === "0o") || (base === 2 && p === "0b");
        if (prefixed) { t = t.slice(2); if (t[0] === "_") t = t.slice(1); }
        if (!/^[0-9a-z]+(?:_[0-9a-z]+)*$/i.test(t)) bad();
        t = t.replace(/_/g, "").toLowerCase();
        if (base === 0 && radix === 10 && t.length > 1 && t[0] === "0" && /[^0]/.test(t)) bad();
        let v;
        if (radix === 10 || radix === 16 || radix === 8 || radix === 2) {
            const digits = radix === 10 ? /^[0-9]+$/ : radix === 16 ? /^[0-9a-f]+$/ : radix === 8 ? /^[0-7]+$/ : /^[01]+$/;
            if (!digits.test(t)) bad();
            v = BigInt(radix === 10 ? t : (radix === 16 ? "0x" : radix === 8 ? "0o" : "0b") + t);
        } else {
            v = 0n; const big = BigInt(radix);
            for (const ch of t) {
                const d = parseInt(ch, 36);
                if (d >= radix) bad();
                v = v * big + BigInt(d);
            }
        }
        return neg ? -v : v;
    }
    rt.constructors.set(T.int, (args, kw) => {
        const k = kw; let base = 10;
        if (k && k.has("base")) base = Number(needInt(k.get("base")));
        if (args.length === 0) return 0n;
        if (args.length === 2) base = Number(needInt(args[1]));
        if (base !== 0 && (base < 2 || base > 36)) fail(E.ValueError, "int() base must be >= 2 and <= 36, or 0");
        const v = typeof args[0] === "object" && args[0] !== null && typeof args[0].pyval === "string" ? args[0].pyval : args[0];
        if (typeof v === "string") return parseIntLiteral(v, base);
        if (args.length === 2 || (k && k.has("base"))) fail(E.TypeError, "int() can't convert non-string with explicit base");
        if (isInt(v)) return asInt(v);
        if (typeof v === "number") return rt.floatToInt(v);
        if (v !== null && typeof v === "object" && v.cls === T.bytes) return parseIntLiteral(String.fromCharCode(...v.items), base);
        const m = typeMethod(v, "__int__") || typeMethod(v, "__index__") || typeMethod(v, "__trunc__");
        if (m !== undefined) return call(descrGet(m, v, typeOf(v)), [], null);
        fail(E.TypeError, "int() argument must be a string, a bytes-like object or a real number, not '" + typeOf(v).name + "'");
    });
    function parseFloatLiteral(s) {
        const t = s.trim().toLowerCase();
        if (t === "inf" || t === "+inf" || t === "infinity" || t === "+infinity") return Infinity;
        if (t === "-inf" || t === "-infinity") return -Infinity;
        if (t === "nan" || t === "+nan" || t === "-nan") return NaN;
        // Underscores only between digits.
        if (!/^[+-]?(?:\d(?:_?\d)*(?:\.(?:\d(?:_?\d)*)?)?|\.\d(?:_?\d)*)(?:e[+-]?\d(?:_?\d)*)?$/.test(t)) fail(E.ValueError, "could not convert string to float: " + repr(s));
        return Number(t.replace(/_/g, ""));
    }
    rt.constructors.set(T.float, (args) => {
        if (args.length === 0) return 0.0;
        const v = typeof args[0] === "object" && args[0] !== null && typeof args[0].pyval === "string" ? args[0].pyval : args[0];
        if (typeof v === "number") return v;
        if (isInt(v)) return toFloat(v);
        if (typeof v === "string") return parseFloatLiteral(v);
        const m = typeMethod(v, "__float__");
        if (m !== undefined) return call(descrGet(m, v, typeOf(v)), [], null);
        fail(E.TypeError, "float() argument must be a string or a real number, not '" + typeOf(v).name + "'");
    });
    rt.constructors.set(T.bool, (args) => args.length === 0 ? false : truth(args[0]));
    rt.constructors.set(T.str, (args, kw) => {
        if (args.length === 0) return "";
        if (args[0] !== null && typeof args[0] === "object" && args[0].cls === T.bytes && (args.length > 1 || (kw && kw.size))) return rt.decodeBytes(args[0], args[1] === undefined ? "utf-8" : args[1]);
        return str(args[0]);
    });
    for (const t of [T.int, T.float, T.str, T.bool]) rt.allocators.set(t, null), rt.allocators.delete(t);
    method(T.int, "__index__", 1, (a) => asInt(a[0]));
    method(T.int, "__int__", 1, (a) => asInt(a[0]));
    method(T.int, "__float__", 1, (a) => toFloat(a[0]));
    // Called unbound (`int.__add__(x, y)`) self may still be boxed, and the
    // other operand may be a boxed subclass instance.
    const unbox = rt.unbox;
    for (const [t, base] of [[T.int, true], [T.float, false]]) {
        for (const [name, op] of [["add", "add"], ["sub", "sub"], ["mul", "mul"], ["truediv", "truediv"], ["floordiv", "floordiv"], ["mod", "mod"], ["pow", "pow"], ["and", "and"], ["or", "or"], ["xor", "xor"], ["lshift", "lshift"], ["rshift", "rshift"]]) {
            if (!base && ["and", "or", "xor", "lshift", "rshift"].includes(op)) continue;
            method(t, "__" + name + "__", 2, (a) => { const y = unbox(a[1]); return isNum(y) ? R.binop(op, unbox(a[0]), y) : NOTIMPL; });
            method(t, "__r" + name + "__", 2, (a) => { const y = unbox(a[1]); return isNum(y) ? R.binop(op, y, unbox(a[0])) : NOTIMPL; });
        }
        method(t, "__neg__", 1, (a) => R.unop("neg", unbox(a[0])));
        method(t, "__pos__", 1, (a) => unbox(a[0]));
        method(t, "__abs__", 1, (a) => { const v = unbox(a[0]); return isInt(v) ? (asInt(v) < 0n ? -asInt(v) : asInt(v)) : Math.abs(v); });
        method(t, "__bool__", 1, (a) => rt.truth(unbox(a[0])));
        for (const [name, op] of [["__eq__", "eq"], ["__ne__", "ne"], ["__lt__", "lt"], ["__le__", "le"], ["__gt__", "gt"], ["__ge__", "ge"]]) method(t, name, 2, (a) => { const y = unbox(a[1]); return isNum(y) ? rt.cmp(op, unbox(a[0]), y) : NOTIMPL; });
    }
    method(T.int, "__invert__", 1, (a) => R.unop("invert", a[0]));
    method(T.int, "bit_count", 1, (a) => { let n = asInt(a[0]); if (n < 0n) n = -n; let c = 0n; while (n) { c += n & 1n; n >>= 1n; } return c; });
    method(T.int, "as_integer_ratio", 1, (a) => tuple([asInt(a[0]), 1n]));
    method(T.int, "is_integer", 1, () => true);
    method(T.int, "bit_length", 1, (a) => { const v = asInt(a[0]); return BigInt((v < 0n ? -v : v).toString(2).replace("0", v === 0n ? "" : "0").length); });
    T.int.dict.set("bit_length", builtin("bit_length", 1, (a) => { const v = asInt(a[0]); return v === 0n ? 0n : BigInt((v < 0n ? -v : v).toString(2).length); }));
    method(T.int, "conjugate", 1, (a) => asInt(a[0]));
    method(T.int, "__repr__", 1, (a) => asInt(a[0]).toString());
    method(T.int, "__hash__", 1, (a) => rt.hashInt(a[0]));
    method(T.int, "to_bytes", -1, (a) => {
        const kw = kwOf(a, ["length", "byteorder", "signed"]);
        let v = asInt(a[0]); const length = Number(a[1] !== undefined ? needInt(a[1]) : kwget(kw, "length", 1n));
        const order = a[2] !== undefined ? a[2] : kwget(kw, "byteorder", "big");
        if (v < 0n) v += 1n << BigInt(8 * length);
        const out = []; for (let i = 0; i < length; i++) { out.push(Number(v & 0xFFn)); v >>= 8n; }
        if (order === "big") out.reverse();
        return rt.bytes(out);
    });
    T.int.dict.set("from_bytes", { cls: T.classmethod, func: builtin("from_bytes", -1, (a) => {
        const kw = kwOf(a, ["byteorder", "signed"]);
        const b = a[1].items.slice(); const order = a[2] !== undefined ? a[2] : kwget(kw, "byteorder", "big");
        if (order === "little") b.reverse();
        let v = 0n; for (const x of b) v = (v << 8n) | BigInt(x);
        if (truth(kwget(kw, "signed", false)) && b.length && (b[0] & 0x80)) v -= 1n << BigInt(8 * b.length);
        return v;
    }) });
    method(T.float, "is_integer", 1, (a) => Number.isInteger(a[0]));
    method(T.float, "__int__", 1, (a) => BigInt(Math.trunc(a[0])));
    method(T.float, "__float__", 1, (a) => a[0]);
    method(T.float, "__repr__", 1, (a) => rt.floatRepr(a[0]));
    method(T.float, "__round__", -1, (a) => R.bind ? roundValue(a[0], a[1]) : null);
    method(T.float, "hex", 1, (a) => rt.floatHex(a[0]));
    T.float.dict.set("fromhex", { cls: T.classmethod, func: builtin("fromhex", 2, (a) => rt.floatFromHex(needStr(a[1]))) });
    method(T.float, "conjugate", 1, (a) => a[0]);
    method(T.float, "as_integer_ratio", 1, (a) => {
        let x = a[0]; if (!Number.isFinite(x)) fail(E.ValueError, "cannot convert to integer ratio");
        let den = 1n; while (!Number.isInteger(x)) { x *= 2; den *= 2n; }
        return tuple([BigInt(x), den]);
    });
    method(T.bool, "__repr__", 1, (a) => a[0] ? "True" : "False");
    function roundValue(x, n) {
        if (isInt(x)) {
            const v = asInt(x);
            if (n === undefined || n === null) return v;
            const d = needInt(n); if (d >= 0n) return v;
            const p = 10n ** (-d); const q = v / p, r = v % p; const half = p / 2n;
            let out = q * p;
            const ar = r < 0n ? -r : r;
            if (ar > half || (ar === half && ((q % 2n) !== 0n))) out += r < 0n ? -p : p;
            return out;
        }
        if (typeof x !== "number") { const m = typeMethod(x, "__round__"); if (m !== undefined) return call(descrGet(m, x, typeOf(x)), n === undefined ? [] : [n], null); fail(E.TypeError, "type " + typeOf(x).name + " doesn't define __round__ method"); }
        if (n === undefined || n === null) {
            if (!Number.isFinite(x)) rt.floatToInt(x);
            const f = Math.floor(x), diff = x - f;
            let r = diff < 0.5 ? f : diff > 0.5 ? f + 1 : (f % 2 === 0 ? f : f + 1);
            return BigInt(r);
        }
        const d = Number(needInt(n));
        if (!Number.isFinite(x)) return x;
        // `fixedHalfEven` formats through a decimal string, which has no
        // negative zero: round(-0.0, 2) and round(-0.001, 1) are -0.0.
        if (d > 0) { const r = Number(fixedHalfEven(x, Math.min(d, 100))); return r === 0 && (x < 0 || Object.is(x, -0)) ? -0 : r; }
        const factor = Math.pow(10, -d);
        const scaled = x / factor;
        let r = Math.floor(scaled);
        const diff = scaled - r;
        if (diff > 0.5 || (diff === 0.5 && r % 2 !== 0)) r += 1;
        // A zero result keeps the sign of x (round(-0.3) is -0.0).
        const out = r * factor;
        return out === 0 && (x < 0 || Object.is(x, -0)) ? -0 : out;
    }
    rt.roundValue = roundValue;
    // `x` to `n` decimals, correctly rounded with ties to even on the EXACT
    // binary value (JS's toFixed rounds exact ties away from zero).
    function fixedHalfEven(x, n) {
        if (!Number.isFinite(x)) return String(x);
        // toFixed switches to exponent form from 1e21; such doubles are
        // integers, so their exact digits come from BigInt.
        if (Math.abs(x) >= 1e21) return (x < 0 ? "-" : "") + BigInt(Math.abs(x)).toString() + (n > 0 ? "." + "0".repeat(n) : "");
        const exact = Math.abs(x).toFixed(Math.min(n + 30, 100));
        const tail = exact.slice(exact.indexOf(".") + 1 + n);
        let s = Math.abs(x).toFixed(n);
        if (/^50*$/.test(tail)) {
            // An exact tie: keep the truncated digits and round to even.
            const kept = exact.slice(0, exact.indexOf(".") + 1 + n).replace(/\.$/, "");
            const digits = kept.replace(".", "");
            const lastDigit = Number(digits[digits.length - 1]);
            if (lastDigit % 2 === 0) s = n > 0 ? kept : kept;
            else {
                // Bump the kept digits by one unit in the last place.
                let arr = digits.split("").map(Number); let i = arr.length - 1;
                while (i >= 0) { if (arr[i] === 9) { arr[i] = 0; i--; } else { arr[i]++; break; } }
                let str2 = (i < 0 ? "1" : "") + arr.join("");
                if (n > 0) str2 = str2.slice(0, str2.length - n) + "." + str2.slice(str2.length - n);
                s = str2;
            }
        }
        return (x < 0 || Object.is(x, -0)) && Number(s) !== 0 ? "-" + s : (x < 0 && Number(s) === 0 ? "-" + s : s);
    }
    rt.fixedHalfEven = fixedHalfEven;

    // ---- str ------------------------------------------------------------------------------------------------------
    const S = T.str;
    function strSelf(a) { const v = a[0]; return typeof v === "string" ? v : needStr(v, "descriptor requires a 'str' object"); }
    method(S, "__len__", 1, (a) => rt.baseLen(a[0]));
    method(S, "__hash__", 1, (a) => rt.hashInt(a[0]));
    method(S, "__repr__", 1, (a) => rt.quoteStr(a[0]));
    method(S, "__str__", 1, (a) => a[0]);
    method(S, "__getitem__", 2, (a) => rt.baseGetitem(a[0], a[1]));
    method(S, "__contains__", 2, (a) => rt.baseContains(a[0], a[1]));
    method(S, "__add__", 2, (a) => { const y = rt.unbox(a[1]); return typeof y === "string" ? strSelf(a) + y : NOTIMPL; });
    method(S, "__mul__", 2, (a) => R.binop("mul", a[0], a[1]));
    method(S, "__mod__", 2, (a) => rt.percentFormat(a[0], a[1]));
    method(S, "__eq__", 2, (a) => { const y = rt.unbox(a[1]); return typeof y === "string" ? strSelf(a) === y : NOTIMPL; });
    method(S, "__lt__", 2, (a) => { const y = rt.unbox(a[1]); return typeof y === "string" ? rt.compareStrings(strSelf(a), y) < 0 : NOTIMPL; });
    method(S, "__iter__", 1, (a) => rt.baseIter(a[0]));
    method(S, "upper", 1, (a) => strSelf(a).toUpperCase());
    method(S, "lower", 1, (a) => strSelf(a).toLowerCase());
    method(S, "casefold", 1, (a) => strSelf(a).toLowerCase().replace(/ß/g, "ss").replace(/ſ/g, "s").replace(/ﬁ/g, "fi").replace(/ﬂ/g, "fl"));
    method(S, "swapcase", 1, (a) => Array.from(strSelf(a), (c) => c === c.toUpperCase() ? c.toLowerCase() : c.toUpperCase()).join(""));
    // Titlecase mappings that differ from uppercase (Unicode's Lt digraphs,
    // SpecialCasing titlecase forms, and Georgian, whose letters titlecase to
    // themselves); everything else titlecases like toUpperCase.
    const TITLE = new Map([["Ǆ", "ǅ"], ["ǅ", "ǅ"], ["ǆ", "ǅ"], ["Ǉ", "ǈ"], ["ǈ", "ǈ"], ["ǉ", "ǈ"], ["Ǌ", "ǋ"], ["ǋ", "ǋ"], ["ǌ", "ǋ"], ["Ǳ", "ǲ"], ["ǲ", "ǲ"], ["ǳ", "ǲ"],
        ["ß", "Ss"], ["ﬀ", "Ff"], ["ﬁ", "Fi"], ["ﬂ", "Fl"], ["ﬃ", "Ffi"], ["ﬄ", "Ffl"], ["ﬅ", "St"], ["ﬆ", "St"], ["և", "Եւ"], ["ﬓ", "Մն"], ["ﬔ", "Մե"], ["ﬕ", "Մի"], ["ﬖ", "Վն"], ["ﬗ", "Մխ"]]);
    for (let cp = 0x1f80; cp < 0x1fb0; cp++) TITLE.set(String.fromCodePoint(cp), String.fromCodePoint(cp | 8));
    for (const [from, to] of [[0x1fb3, 0x1fbc], [0x1fbc, 0x1fbc], [0x1fc3, 0x1fcc], [0x1fcc, 0x1fcc], [0x1ff3, 0x1ffc], [0x1ffc, 0x1ffc]]) TITLE.set(String.fromCodePoint(from), String.fromCodePoint(to));
    function titleChar(ch) {
        const t = TITLE.get(ch);
        if (t !== undefined) return t;
        const cp = ch.codePointAt(0);
        return (cp >= 0x10d0 && cp <= 0x10fa) || (cp >= 0x10fd && cp <= 0x10ff) ? ch : ch.toUpperCase();
    }
    const CASED_CHAR = /^[\p{Lowercase}\p{Uppercase}\p{Lt}]$/u;
    method(S, "capitalize", 1, (a) => { const s = strSelf(a); if (!s.length) return s; const cps = codepoints(s); return titleChar(cps[0]) + cps.slice(1).join("").toLowerCase(); });
    method(S, "title", 1, (a) => {
        const s = strSelf(a);
        if (/^[\x00-\x7f]*$/.test(s)) return s.replace(/[A-Za-z]+/g, (w) => w[0].toUpperCase() + w.slice(1).toLowerCase());
        let out = "", prevCased = false;
        for (const ch of s) { out += prevCased ? ch.toLowerCase() : titleChar(ch); prevCased = CASED_CHAR.test(ch); }
        return out;
    });
    // Python's whitespace (`rt.PY_WS`): one character of it, a run of it, a
    // str of nothing else, and the first and the last word with the run
    // beside it.
    const WS_CHAR = new RegExp("[" + rt.PY_WS + "]"), WS_RUN = rt.WS_RUN, WS_ONLY = new RegExp("^[" + rt.PY_WS + "]+$");
    const WS_FIRST = new RegExp("^([^" + rt.PY_WS + "]+)[" + rt.PY_WS + "]+"), WS_LAST = new RegExp("[" + rt.PY_WS + "]+([^" + rt.PY_WS + "]+)$");
    // `s` without the whitespace at its left end (mode < 0), its right end
    // (mode > 0) or both (0); `s` itself when there is none.
    function stripWs(s, mode) {
        let i = 0, j = s.length;
        if (mode <= 0) while (i < j && WS_CHAR.test(s[i])) i++;
        if (mode >= 0) while (j > i && WS_CHAR.test(s[j - 1])) j--;
        return i === 0 && j === s.length ? s : s.slice(i, j);
    }
    function stripChars(s, chars, mode) {
        if (chars === undefined || chars === null) return stripWs(s, mode);
        const set = new Set(Array.from(needStr(chars)));
        let cps = Array.from(s), i = 0, j = cps.length;
        if (mode <= 0) while (i < j && set.has(cps[i])) i++;
        if (mode >= 0) while (j > i && set.has(cps[j - 1])) j--;
        return cps.slice(i, j).join("");
    }
    method(S, "strip", 2, (a) => stripChars(strSelf(a), a[1], 0), 1);
    method(S, "lstrip", 2, (a) => stripChars(strSelf(a), a[1], -1), 1);
    method(S, "rstrip", 2, (a) => stripChars(strSelf(a), a[1], 1), 1);
    // sep and maxsplit may also come as keywords.
    methodkw(S, "split", (a) => {
        const kw = kwOf(a, ["sep", "maxsplit"]);
        const s = strSelf(a); const sep = a[1] !== undefined ? a[1] : kwget(kw, "sep", null);
        let maxsplit = Number(needInt(a[2] !== undefined ? a[2] : kwget(kw, "maxsplit", -1n)));
        if (sep === null) {
            const parts = s.split(WS_RUN).filter((x) => x.length);
            if (maxsplit < 0 || parts.length <= maxsplit) return list(parts);
            // More words than splits: the first `maxsplit` of them, then the
            // rest of the text from its next word on, verbatim (CPython keeps
            // the whitespace that ends it).
            const out = []; let rest = stripWs(s, -1);
            for (let i = 0; i < maxsplit; i++) { const m = WS_FIRST.exec(rest); if (!m) break; out.push(m[1]); rest = rest.slice(m[0].length); }
            if (rest.length) out.push(rest);
            return list(out);
        }
        if (needStr(sep) === "") fail(E.ValueError, "empty separator");
        const parts = s.split(sep);
        if (maxsplit >= 0 && parts.length > maxsplit + 1) { const head = parts.slice(0, maxsplit); head.push(parts.slice(maxsplit).join(sep)); return list(head); }
        return list(parts);
    });
    methodkw(S, "rsplit", (a) => {
        const kw = kwOf(a, ["sep", "maxsplit"]);
        const s = strSelf(a); const sep = a[1] !== undefined ? a[1] : kwget(kw, "sep", null);
        const maxsplit = Number(needInt(a[2] !== undefined ? a[2] : kwget(kw, "maxsplit", -1n)));
        if (sep === null) {
            const parts = s.split(WS_RUN).filter((x) => x.length);
            if (maxsplit < 0 || parts.length <= maxsplit) return list(parts);
            // As `split`, from the right: the text before the last
            // `maxsplit` words is kept with the whitespace that starts it.
            const out = []; let rest = stripWs(s, 1);
            for (let i = 0; i < maxsplit; i++) { const m = WS_LAST.exec(rest); if (!m) break; out.unshift(m[1]); rest = rest.slice(0, rest.length - m[0].length); }
            if (rest.length) out.unshift(rest);
            return list(out);
        }
        const parts = s.split(needStr(sep));
        if (maxsplit >= 0 && parts.length > maxsplit + 1) { const tail = parts.slice(parts.length - maxsplit); tail.unshift(parts.slice(0, parts.length - maxsplit).join(sep)); return list(tail); }
        return list(parts);
    });
    // Every line boundary str.splitlines recognizes.
    method(S, "splitlines", 2, (a) => { const keep = a[1] !== undefined && truth(a[1]); const s = strSelf(a); const out = []; const re = /([^\n\r\x0b\x0c\x1c-\x1e\x85\u2028\u2029]*)(\r\n|[\n\r\x0b\x0c\x1c-\x1e\x85\u2028\u2029]|$)/g; let m; while ((m = re.exec(s)) !== null && (m[1].length || m[2].length)) { out.push(keep ? m[1] + m[2] : m[1]); if (m[2] === "") break; } return list(out); }, 1);
    method(S, "join", 2, (a) => { const parts = drain(a[1]); for (let i = 0; i < parts.length; i++) if (typeof parts[i] !== "string") { const u = rt.unbox(parts[i]); if (typeof u !== "string") fail(E.TypeError, "sequence item " + i + ": expected str instance, " + typeOf(parts[i]).name + " found"); parts[i] = u; } return rt.checkedText(parts.join(strSelf(a))); });
    method(S, "replace", 4, (a) => {
        const s = strSelf(a), from = needStr(a[1]), to = needStr(a[2]); const count = a[3] === undefined ? -1 : Number(needInt(a[3]));
        if (from === "") {
            // An empty pattern matches before every code point and at the end.
            const cps = codepoints(s); const n = count < 0 ? cps.length + 1 : Math.min(count, cps.length + 1);
            if (n === 0) return s;
            if (cps.length === 0) return to;
            if (n > cps.length) return rt.checkedText(to + cps.join(to) + to);
            let out = ""; for (let i = 0; i < n; i++) out += to + cps[i];
            return rt.checkedText(out + cps.slice(n).join(""));
        }
        if (count < 0) return rt.checkedText(s.split(from).join(to));
        let out = "", rest = s, n = 0; while (n < count) { const i = rest.indexOf(from); if (i < 0) break; out += rest.slice(0, i) + to; rest = rest.slice(i + from.length); n++; } return rt.checkedText(out + rest);
    }, 3);
    // Parse optional [start, end] code-point bounds onto UTF-16 [u0, u1],
    // without splitting `s` into a codepoint array. Returns null when start > end.
    function strBounds(a, s) {
        const hasBounds = (a[2] !== undefined && a[2] !== null) || (a[3] !== undefined && a[3] !== null);
        let n, pairs = null;
        if (rt.hasSurrogate(s)) { rt.strInfo(s); n = rt.infoLen; pairs = rt.infoPairs; }
        else n = s.length;
        if (!hasBounds) return { lo: 0, hi: n, u0: 0, u1: s.length, pairs: pairs };
        let lo = a[2] === undefined || a[2] === null ? 0 : typeof a[2] === "bigint" ? Number(a[2]) : Number(rt.indexOf(a[2]));
        let hi = a[3] === undefined || a[3] === null ? n : typeof a[3] === "bigint" ? Number(a[3]) : Number(rt.indexOf(a[3]));
        if (lo < 0) lo = Math.max(0, lo + n);
        if (hi < 0) hi = Math.max(0, hi + n);
        if (hi > n) hi = n;
        if (lo > hi) return null;
        const u0 = pairs !== null ? rt.utf16FromCp(pairs, lo) : lo;
        const u1 = hi === n ? s.length : (pairs !== null ? rt.utf16FromCp(pairs, hi) : hi);
        return { lo: lo, hi: hi, u0: u0, u1: u1, pairs: pairs };
    }
    function findImpl(a, rev, raise) {
        const s = strSelf(a), sub = needStr(a[1]);
        const b = strBounds(a, s);
        if (b === null) { if (raise) fail(E.ValueError, "substring not found"); return -1n; }
        if (sub.length === 0) return BigInt(rev ? b.hi : b.lo);
        if (b.u0 + sub.length > b.u1) { if (raise) fail(E.ValueError, "substring not found"); return -1n; }
        let i;
        if (b.pairs !== null && rt.hasSurrogate(sub)) {
            const hay = b.u0 === 0 && b.u1 === s.length ? s : s.slice(b.u0, b.u1);
            const rel = rev ? hay.lastIndexOf(sub) : hay.indexOf(sub);
            i = rel < 0 ? -1 : b.u0 + rel;
        } else if (rev) {
            i = s.lastIndexOf(sub, b.u1 - sub.length);
            if (i < b.u0) i = -1;
        } else {
            i = s.indexOf(sub, b.u0);
            if (i < 0 || i + sub.length > b.u1) i = -1;
        }
        if (i < 0) { if (raise) fail(E.ValueError, "substring not found"); return -1n; }
        return BigInt(b.pairs !== null ? rt.cpFromUtf16(b.pairs, i) : i);
    }
    method(S, "find", 4, (a) => findImpl(a, false, false), 2);
    method(S, "rfind", 4, (a) => findImpl(a, true, false), 2);
    method(S, "index", 4, (a) => findImpl(a, false, true), 2);
    method(S, "rindex", 4, (a) => findImpl(a, true, true), 2);
    method(S, "count", 4, (a) => {
        const s = strSelf(a), sub = needStr(a[1]);
        const b = strBounds(a, s);
        if (b === null) return 0n;
        if (sub.length === 0) return BigInt(b.hi - b.lo + 1);
        let c = 0, p = b.u0;
        const lim = b.u1 - sub.length;
        while (p <= lim) {
            const k = s.indexOf(sub, p);
            if (k < 0 || k > lim) break;
            c++; p = k + sub.length;
        }
        return BigInt(c);
    }, 2);
    function affix(a, end) {
        const s = strSelf(a), p = a[1];
        const b = strBounds(a, s);
        const test = (x) => {
            const q = needStr(x);
            if (b === null) return false;
            if (b.pairs !== null && rt.hasSurrogate(q)) {
                const sub = b.u0 === 0 && b.u1 === s.length ? s : s.slice(b.u0, b.u1);
                if (q.length > sub.length) return false;
                if (end) {
                    if (!sub.endsWith(q)) return false;
                    const at = sub.length - q.length;
                    if (at > 0 && sub.charCodeAt(at - 1) >= 0xd800 && sub.charCodeAt(at - 1) <= 0xdbff && sub.charCodeAt(at) >= 0xdc00 && sub.charCodeAt(at) <= 0xdfff) return false;
                    return true;
                }
                if (!sub.startsWith(q)) return false;
                const after = q.length;
                if (after > 0 && after < sub.length && sub.charCodeAt(after - 1) >= 0xd800 && sub.charCodeAt(after - 1) <= 0xdbff && sub.charCodeAt(after) >= 0xdc00 && sub.charCodeAt(after) <= 0xdfff) return false;
                return true;
            }
            if (b.u0 + q.length > b.u1) return false;
            return end ? s.endsWith(q, b.u1) : s.startsWith(q, b.u0);
        };
        if (p !== null && typeof p === "object" && p.cls === T.tuple) return p.items.some(test);
        return test(p);
    }
    method(S, "startswith", 4, (a) => affix(a, false), 2);
    method(S, "endswith", 4, (a) => affix(a, true), 2);
    // Direct positional entries (`c<n>`, see `builtinFast`) for the str
    // methods programs call most: a str receiver with str arguments gets
    // exactly the method's result without its argument array and checks;
    // anything else runs the method itself (`this` is the builtin).
    {
        const direct = (name, n, fn) => { S.dict.get(name)["c" + n] = fn; };
        direct("lower", 1, function (s) { return typeof s === "string" ? s.toLowerCase() : this.code([s]); });
        direct("upper", 1, function (s) { return typeof s === "string" ? s.toUpperCase() : this.code([s]); });
        direct("strip", 1, function (s) { return typeof s === "string" ? stripWs(s, 0) : this.code([s]); });
        // With a str of characters (and no surrogate in either str, so each
        // code unit is a code point): the ends cut while they are in it.
        const stripIn = (s, chars, mode) => {
            let i = 0, j = s.length;
            if (mode <= 0) while (i < j && chars.indexOf(s[i]) >= 0) i++;
            if (mode >= 0) while (j > i && chars.indexOf(s[j - 1]) >= 0) j--;
            return i === 0 && j === s.length ? s : s.slice(i, j);
        };
        const plainPair = (s, chars) => typeof s === "string" && typeof chars === "string" && !rt.hasSurrogate(s) && !rt.hasSurrogate(chars);
        direct("strip", 2, function (s, chars) { return plainPair(s, chars) ? stripIn(s, chars, 0) : this.code([s, chars]); });
        direct("lstrip", 2, function (s, chars) { return plainPair(s, chars) ? stripIn(s, chars, -1) : this.code([s, chars]); });
        direct("rstrip", 2, function (s, chars) { return plainPair(s, chars) ? stripIn(s, chars, 1) : this.code([s, chars]); });
        direct("startswith", 2, function (s, p) { return typeof s === "string" && typeof p === "string" ? s.startsWith(p) : this.code([s, p]); });
        direct("endswith", 2, function (s, p) { return typeof s === "string" && typeof p === "string" ? s.endsWith(p) : this.code([s, p]); });
        direct("find", 2, function (s, sub) {
            if (typeof s !== "string" || typeof sub !== "string") return this.code([s, sub]);
            const i = s.indexOf(sub);
            if (i < 0) return -1n;
            return BigInt(rt.hasSurrogate(s) ? rt.strLen(s.slice(0, i)) : i);
        });
        direct("replace", 3, function (s, from, to) {
            return typeof s === "string" && typeof from === "string" && typeof to === "string" && from !== "" ? rt.checkedText(s.split(from).join(to)) : this.code([s, from, to]);
        });
        direct("split", 2, function (s, sep) {
            return typeof s === "string" && typeof sep === "string" && sep !== "" ? list(s.split(sep)) : this.code([s, sep]);
        });
        direct("split", 1, function (s) {
            if (typeof s !== "string") return this.code([s]);
            // The split at whitespace runs without its empty ends (a loop:
            // `filter` would run its callback on a nested interpreter).
            const raw = s.split(WS_RUN), parts = [];
            for (let i = 0; i < raw.length; i++) if (raw[i].length) parts.push(raw[i]);
            return list(parts);
        });
        direct("join", 2, function (sep, it) {
            if (typeof sep !== "string") return this.code([sep, it]);
            const parts = drain(it);
            for (let i = 0; i < parts.length; i++) {
                if (typeof parts[i] !== "string") {
                    const u = rt.unbox(parts[i]);
                    if (typeof u !== "string") fail(E.TypeError, "sequence item " + i + ": expected str instance, " + typeOf(parts[i]).name + " found");
                    parts[i] = u;
                }
            }
            return rt.checkedText(parts.join(sep));
        });
    }
    // The engine's native forms of some of those entries (`vm::py_str`,
    // bound only in a Python program): each keeps the entry it replaces as
    // `j<n>`, answers what that answers for plain strs, and calls it for
    // anything else.
    {
        let STRM = null;
        try { STRM = typeof __zipp_py_strm === "function" ? __zipp_py_strm : null; } catch (e) { STRM = null; }
        rt.nativeEntry = null;
        if (STRM !== null) {
            const LT = list([]);
            const install = (b, n, op) => {
                b.strop = op; b.ltmpl = LT; b.ttype = T.tuple;
                // A positional builtin with no entry yet: the one
                // `builtinFast` would give it.
                b["j" + n] = b["c" + n] !== undefined ? b["c" + n] : n === 1 ? function (a) { return this.code([a]); } : n === 2 ? function (a, c) { return this.code([a, c]); } : n === 3 ? function (a, c, d) { return this.code([a, c, d]); } : function (a, c, d, e) { return this.code([a, c, d, e]); };
                b["c" + n] = STRM;
            };
            const native = (name, n, op) => install(S.dict.get(name), n, op);
            rt.nativeEntry = install;
            native("strip", 2, 1); native("lstrip", 2, 2); native("rstrip", 2, 3);
            native("strip", 1, 1); native("lstrip", 1, 2); native("rstrip", 1, 3);
            native("split", 1, 4); native("split", 2, 4); native("split", 3, 4);
            native("rsplit", 1, 26); native("rsplit", 2, 26); native("rsplit", 3, 26);
            native("splitlines", 1, 27); native("splitlines", 2, 27);
            native("replace", 3, 5); native("replace", 4, 5);
            native("find", 2, 6); native("find", 3, 6); native("find", 4, 6);
            native("index", 2, 25); native("index", 3, 25); native("index", 4, 25);
            native("rfind", 2, 22); native("rfind", 3, 22); native("rfind", 4, 22);
            native("rindex", 2, 24); native("rindex", 3, 24); native("rindex", 4, 24);
            native("count", 2, 23); native("count", 3, 23); native("count", 4, 23);
            native("join", 2, 7); native("lower", 1, 8); native("upper", 1, 9);
            native("startswith", 2, 18); native("startswith", 3, 18); native("startswith", 4, 18);
            native("endswith", 2, 19); native("endswith", 3, 19); native("endswith", 4, 19);
            native("capitalize", 1, 21);
        }
        // An f-string's parts (strs, and ints left as ints: `"" + 5n` is
        // "5", as `str(5)`) are joined by `R.FJOIN.c<n>(...parts)`, a call
        // the emitter makes with the record as `this`; the native (op 20)
        // joins them in one allocation and calls `j<n>` for anything else.
        const FJ = { strop: 20, ltmpl: null, ttype: null };
        const fjoin = function (...parts) { let out = ""; for (let i = 0; i < parts.length; i++) { const p = parts[i]; out += typeof p === "string" ? p : typeof p === "bigint" ? "" + p : R.fmt(p, "", 0); } return rt.checkedText(out); };
        for (let n = 1; n <= 12; n++) { FJ["j" + n] = fjoin; FJ["c" + n] = STRM !== null ? STRM : fjoin; }
        R.FJOIN = FJ;
        // Direct `sep.join([e1, ..., ek])`: passes elements directly without
        // constructing an intermediate list.
        R.SJOIN = S.dict.get("join");
        const SJ = { strop: 36, ltmpl: null, ttype: null };
        const sjoin = function (sep, ...parts) { return R.SJOIN.c2(sep, list(parts)); };
        for (let n = 2; n <= 9; n++) { SJ["j" + n] = sjoin; SJ["c" + n] = STRM !== null ? STRM : sjoin; }
        R.SJOIN_DIRECT = SJ;
        // Direct `o[lo:hi]` / `o[lo:hi:step]` without allocating a slice record.
        const SLG = { strop: 37, ltmpl: list([]), ttype: T.tuple };
        const sliceget = function (o, lo, hi, step) { return R.getitem(o, R.slice(lo, hi, step === undefined ? null : step)); };
        SLG.j3 = sliceget; SLG.c3 = STRM !== null ? STRM : sliceget;
        SLG.j4 = sliceget; SLG.c4 = STRM !== null ? STRM : sliceget;
        R.SLICEGET = SLG;
    }
    // CPython's digit classes follow Numeric_Type: isdecimal is Decimal (Nd),
    // isdigit adds Digit (superscripts, circled and parenthesized digits,
    // all in No), isnumeric adds Numeric (the rest of Nl/No plus the CJK
    // numeral ideographs, which are Lo). Tables from Unicode 14 (CPython 3.11).
    method(S, "isdigit", 1, (a) => /^[\p{Nd}²³¹፩-፱᧚⁰⁴-⁹₀-₉①-⑨⑴-⑼⒈-⒐⓪⓵-⓽⓿❶-❾➀-➈➊-➒\u{10a40}-\u{10a43}\u{10e60}-\u{10e68}\u{11052}-\u{1105a}\u{1f100}-\u{1f10a}]+$/u.test(strSelf(a)));
    method(S, "isdecimal", 1, (a) => /^\p{Nd}+$/u.test(strSelf(a)));
    method(S, "isnumeric", 1, (a) => /^[\p{Nd}\p{Nl}\p{No}㐅㒃㠪㭍一七万三九二五亖亿-什仟仨伍佰億兆兩八六十千-卅卌叁-叄四壱壹幺廾-廿弌-弎弐拾捌柒漆玖百肆萬貮貳贰阡陆陌陸零參拾兩零六陸什\u{20001}\u{20064}\u{200e2}\u{20121}\u{2092a}\u{20983}\u{2098c}\u{2099c}\u{20aea}\u{20afd}\u{20b19}\u{22390}\u{22998}\u{23b1b}\u{2626d}\u{2f890}]+$/u.test(strSelf(a)));
    method(S, "isalpha", 1, (a) => /^[\p{L}]+$/u.test(strSelf(a)));
    method(S, "isalnum", 1, (a) => /^[\p{L}\p{N}]+$/u.test(strSelf(a)));
    method(S, "isspace", 1, (a) => WS_ONLY.test(strSelf(a)));
    // Cased characters are those with the Lowercase or Uppercase property or
    // the titlecase category, as in CPython's unicode ctype table.
    method(S, "isupper", 1, (a) => { const s = strSelf(a); return /\p{Uppercase}/u.test(s) && !/[\p{Lowercase}\p{Lt}]/u.test(s); });
    method(S, "islower", 1, (a) => { const s = strSelf(a); return /\p{Lowercase}/u.test(s) && !/[\p{Uppercase}\p{Lt}]/u.test(s); });
    if (rt.nativeEntry !== null) {
        rt.nativeEntry(S.dict.get("isspace"), 1, 28);
        rt.nativeEntry(S.dict.get("isdigit"), 1, 29);
        rt.nativeEntry(S.dict.get("isalpha"), 1, 30);
        rt.nativeEntry(S.dict.get("isalnum"), 1, 31);
        rt.nativeEntry(S.dict.get("islower"), 1, 32);
        rt.nativeEntry(S.dict.get("isupper"), 1, 33);
    }
    method(S, "istitle", 1, (a) => {
        let cased = false, prevCased = false;
        for (const ch of strSelf(a)) {
            if (/^[\p{Uppercase}\p{Lt}]$/u.test(ch)) { if (prevCased) return false; prevCased = cased = true; }
            else if (/^\p{Lowercase}$/u.test(ch)) { if (!prevCased) return false; prevCased = cased = true; }
            else prevCased = false;
        }
        return cased;
    });
    method(S, "isidentifier", 1, (a) => /^[\p{L}\p{Nl}_][\p{L}\p{Nl}\p{Mn}\p{Mc}\p{Nd}\p{Pc}_]*$/u.test(strSelf(a)));
    method(S, "isascii", 1, (a) => { const s = strSelf(a); for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) > 0x7f) return false; return true; });
    method(S, "isprintable", 1, (a) => !rt.NONPRINTABLE.test(strSelf(a)));
    function pad(a, mode) {
        const s = strSelf(a); const width = Number(needInt(a[1])); const fill = a[2] === undefined ? " " : needStr(a[2]);
        if (codepoints(fill).length !== 1) fail(E.TypeError, "The fill character must be exactly one character long");
        const n = rt.strLen(s); if (width <= n) return s;
        const total = width - n;
        if (mode < 0) return s + fill.repeat(total);
        if (mode > 0) return fill.repeat(total) + s;
        const left = Math.floor(total / 2) + ((total & 1) && (width & 1) ? 1 : 0);
        return fill.repeat(left) + s + fill.repeat(total - left);
    }
    method(S, "ljust", 3, (a) => pad(a, -1), 2);
    method(S, "rjust", 3, (a) => pad(a, 1), 2);
    method(S, "center", 3, (a) => pad(a, 0), 2);
    method(S, "zfill", 2, (a) => { const s = strSelf(a); const width = Number(needInt(a[1])); const n = rt.strLen(s); if (n >= width) return s; const sign = s[0] === "-" || s[0] === "+" ? s[0] : ""; return sign + "0".repeat(width - n) + s.slice(sign.length); });
    method(S, "partition", 2, (a) => { const s = strSelf(a), sep = needStr(a[1]); const i = s.indexOf(sep); return i < 0 ? tuple([s, "", ""]) : tuple([s.slice(0, i), sep, s.slice(i + sep.length)]); });
    method(S, "rpartition", 2, (a) => { const s = strSelf(a), sep = needStr(a[1]); const i = s.lastIndexOf(sep); return i < 0 ? tuple(["", "", s]) : tuple([s.slice(0, i), sep, s.slice(i + sep.length)]); });
    method(S, "encode", -1, (a) => { const kw = kwOf(a, ["encoding", "errors"]); return rt.encodeStr(strSelf(a), a[1] === undefined ? kwget(kw, "encoding", "utf-8") : a[1], a[2] === undefined ? kwget(kw, "errors", "strict") : a[2]); });
    method(S, "format", -1, (a) => { const kw = kwOf(a, null) === null ? null : null; return rt.strFormat(strSelf(a), a.slice(1), a.kwmap || null); });
    S.dict.get("format").kwnames = true;
    S.dict.set("format", builtin("format", -1, (a) => { const last = a[a.length - 1]; let kw = null; if (last instanceof Map) { kw = last; a.pop(); } return rt.strFormat(strSelf(a), a.slice(1), kw); }));
    S.dict.get("format").kwnames = true;
    method(S, "format_map", 2, (a) => rt.strFormat(strSelf(a), [], rt.mapFromDict(rt.asDict(a[1]))));
    method(S, "expandtabs", 2, (a) => { const size = a[1] === undefined ? 8 : Number(needInt(a[1])); let out = "", col = 0; for (const ch of strSelf(a)) { if (ch === "\t") { const n = size > 0 ? size - (col % size) : 0; out += " ".repeat(n); col += n; } else { out += ch; col = ch === "\n" || ch === "\r" ? 0 : col + 1; } } return out; }, 1);
    method(S, "removeprefix", 2, (a) => { const s = strSelf(a), p = needStr(a[1]); return s.startsWith(p) ? s.slice(p.length) : s; });
    method(S, "removesuffix", 2, (a) => { const s = strSelf(a), p = needStr(a[1]); return p.length && s.endsWith(p) ? s.slice(0, s.length - p.length) : s; });
    S.dict.set("maketrans", { cls: T.staticmethod, func: builtin("maketrans", 3, (a) => {
        const d = dict();
        if (a.length === 1) { for (const [k, v] of dictEntries(rt.asDict(a[0]))) dictSet(d, typeof k === "string" ? BigInt(k.codePointAt(0)) : k, v); return d; }
        const x = codepoints(needStr(a[0])), y = codepoints(needStr(a[1]));
        if (x.length !== y.length) fail(E.ValueError, "the first two maketrans arguments must have equal length");
        for (let i = 0; i < x.length; i++) dictSet(d, BigInt(x[i].codePointAt(0)), BigInt(y[i].codePointAt(0)));
        if (a[2] !== undefined) for (const ch of codepoints(needStr(a[2]))) dictSet(d, BigInt(ch.codePointAt(0)), null);
        return d;
    }, 1) });
    method(S, "translate", 2, (a) => { const table = a[1]; let out = ""; for (const ch of codepoints(strSelf(a))) { const cp = BigInt(ch.codePointAt(0)); let v; try { v = getitem(table, cp); } catch (e) { if (e && (e.cls === E.LookupError || isInstance(e, E.LookupError))) { out += ch; continue; } throw e; } if (v === null) continue; out += isInt(v) ? String.fromCodePoint(Number(asInt(v))) : str(v); } return out; });
    // float.hex(): sign, 0x, 13 hex digits of mantissa, p, decimal exponent.
    rt.floatHex = function (x) {
        if (Number.isNaN(x)) return "nan";
        if (x === Infinity) return "inf";
        if (x === -Infinity) return "-inf";
        const sign = x < 0 || Object.is(x, -0) ? "-" : "";
        x = Math.abs(x);
        if (x === 0) return sign + "0x0.0p+0";
        let e = Math.floor(Math.log2(x));
        let m = x / Math.pow(2, e);
        if (m >= 2) { m /= 2; e++; } else if (m < 1) { m *= 2; e--; }
        // Subnormals keep the minimum exponent with a leading 0 digit.
        let lead = "1";
        if (e < -1022) { m = x / Math.pow(2, -1022); e = -1022; lead = "0"; }
        let frac = m - Number(lead);
        let digits = "";
        for (let i = 0; i < 13; i++) { frac *= 16; const d = Math.floor(frac); digits += d.toString(16); frac -= d; }
        return sign + "0x" + lead + "." + digits + "p" + (e < 0 ? "-" : "+") + Math.abs(e);
    };
    rt.floatFromHex = function (s) {
        const t = s.trim().toLowerCase();
        if (t === "inf" || t === "+inf" || t === "infinity") return Infinity;
        if (t === "-inf" || t === "-infinity") return -Infinity;
        if (t === "nan" || t === "+nan" || t === "-nan") return NaN;
        const m = /^([+-])?(?:0x)?([0-9a-f]*)(?:\.([0-9a-f]*))?(?:p([+-]?\d+))?$/.exec(t);
        if (m === null || (m[2] === "" && (m[3] === undefined || m[3] === ""))) fail(E.ValueError, "invalid hexadecimal floating-point string");
        let v = 0;
        for (const c of m[2]) v = v * 16 + parseInt(c, 16);
        let scale = 1 / 16;
        for (const c of m[3] || "") { v += parseInt(c, 16) * scale; scale /= 16; }
        const r = v * Math.pow(2, m[4] === undefined ? 0 : parseInt(m[4], 10));
        if (!Number.isFinite(r)) fail(E.OverflowError, "hexadecimal value too large to represent as a float");
        return m[1] === "-" ? -r : r;
    };
    rt.encodeStr = function (s, encoding, errors) {
        const enc = String(encoding).toLowerCase().replace("-", "");
        const out = [];
        if (enc === "ascii" || enc === "latin1" || enc === "latin_1" || enc === "iso88591") {
            const limit = enc === "ascii" ? 127 : 255;
            let pos = 0;
            for (const ch of s) {
                const cp = ch.codePointAt(0);
                if (cp > limit) {
                    switch (errors === undefined ? "strict" : errors) {
                        case "ignore": break;
                        case "replace": out.push(63); break;
                        case "backslashreplace": for (const c of (cp > 0xffff ? "\\U" + cp.toString(16).padStart(8, "0") : cp > 0xff ? "\\u" + cp.toString(16).padStart(4, "0") : "\\x" + cp.toString(16).padStart(2, "0"))) out.push(c.charCodeAt(0)); break;
                        case "xmlcharrefreplace": for (const c of "&#" + cp + ";") out.push(c.charCodeAt(0)); break;
                        default: fail(E.UnicodeEncodeError, "'" + encoding + "' codec can't encode character " + rt.quoteStr(ch) + " in position " + pos + ": ordinal not in range(" + (limit + 1) + ")");
                    }
                }
                else out.push(cp);
                pos++;
            }
            return rt.bytes(out);
        }
        for (const ch of s) { let cp = ch.codePointAt(0); if (cp < 0x80) out.push(cp); else if (cp < 0x800) out.push(0xC0 | (cp >> 6), 0x80 | (cp & 63)); else if (cp < 0x10000) out.push(0xE0 | (cp >> 12), 0x80 | ((cp >> 6) & 63), 0x80 | (cp & 63)); else out.push(0xF0 | (cp >> 18), 0x80 | ((cp >> 12) & 63), 0x80 | ((cp >> 6) & 63), 0x80 | (cp & 63)); }
        return rt.bytes(out);
    };
    let DECODE_NATIVE = null;
    try { DECODE_NATIVE = typeof __zipp_py_decode === "function" ? __zipp_py_decode : null; } catch (e) { DECODE_NATIVE = null; }
    rt.decodeBytes = function (b, encoding, errors) {
        const items = b.items;
        if (DECODE_NATIVE !== null) {
            const fast = DECODE_NATIVE(items, String(encoding), errors === undefined ? "strict" : String(errors));
            if (fast !== undefined) return fast;
        }
        const enc = String(encoding).toLowerCase().replace("-", "");
        const mode = errors === undefined ? "strict" : String(errors);
        if (enc === "ascii") { let out = ""; for (let i = 0; i < items.length; i++) { const c = items[i]; if (c < 128) out += String.fromCharCode(c); else if (mode === "replace") out += "�"; else if (mode !== "ignore") fail(E.UnicodeDecodeError, "'ascii' codec can't decode byte 0x" + c.toString(16) + " in position " + i + ": ordinal not in range(128)"); } return out; }
        if (enc === "latin1" || enc === "latin_1" || enc === "iso88591") { let out = ""; for (let i = 0; i < items.length; i++) out += String.fromCharCode(items[i]); return out; }
        let out = "", i = 0;
        const cont = (k) => i + k < items.length && (items[i + k] & 0xC0) === 0x80;
        while (i < items.length) {
            const c = items[i];
            if (c < 0x80) { out += String.fromCharCode(c); i++; }
            else if (c >= 0xC2 && c < 0xE0 && cont(1)) { out += String.fromCodePoint(((c & 31) << 6) | (items[i + 1] & 63)); i += 2; }
            else if (c >= 0xE0 && c < 0xF0 && cont(1) && cont(2)) { out += String.fromCodePoint(((c & 15) << 12) | ((items[i + 1] & 63) << 6) | (items[i + 2] & 63)); i += 3; }
            else if (c >= 0xF0 && c < 0xF5 && cont(1) && cont(2) && cont(3)) { out += String.fromCodePoint(((c & 7) << 18) | ((items[i + 1] & 63) << 12) | ((items[i + 2] & 63) << 6) | (items[i + 3] & 63)); i += 4; }
            else if (mode === "replace") { out += "�"; i++; }
            else if (mode === "ignore") { i++; }
            else fail(E.UnicodeDecodeError, "'utf-8' codec can't decode byte 0x" + c.toString(16) + " in position " + i + ": invalid start byte");
        }
        return out;
    };
    // Double quotes when the bytes hold ' but no ", like str's repr;
    // bytearray's repr (`array`) still escapes ' inside them, as CPython's does.
    rt.bytesRepr = function (b, array) {
        const q = b.items.indexOf(39) >= 0 && b.items.indexOf(34) < 0 ? "\"" : "'";
        let out = "b" + q;
        for (const c of b.items) {
            if (c === 39 && (q === "'" || array)) out += "\\'"; else if (c === 92) out += "\\\\"; else if (c === 10) out += "\\n"; else if (c === 13) out += "\\r"; else if (c === 9) out += "\\t";
            else if (c < 32 || c >= 127) out += "\\x" + c.toString(16).padStart(2, "0"); else out += String.fromCharCode(c);
        }
        return out + q;
    };
    // Large byte buffers are built element by element: past the engine's
    // dense-array limit (2^20 natively, more under safe-sandbox)
    // `new Array(n).fill()` leaves a sparse array and `Array.from(typedArray)`
    // throws, where a push loop stays dense.
    const DENSE_NATIVE = 1 << 20;
    rt.zeroBytes = function (n) {
        if (n < 0) fail(E.ValueError, "negative count");
        if (n > rt.MAX_ITEMS) fail(E.MemoryError, "sequence limit exceeded");
        if (n <= DENSE_NATIVE) return new Array(n).fill(0);
        const out = []; for (let i = 0; i < n; i++) out.push(0); return out;
    };
    // A push loop is also the fast copy: the engine's Array.from over a
    // typed array is several times slower.
    rt.bytesFromU8 = function (u8) {
        const out = []; for (let i = 0; i < u8.length; i++) out.push(u8[i]); return out;
    };
    rt.constructors.set(T.bytes, (args) => {
        if (args.length === 0) return rt.bytes([]);
        const v = args[0];
        if (typeof v === "string") return rt.encodeStr(v, args[1] === undefined ? "utf-8" : args[1]);
        if (isInt(v)) return rt.bytes(rt.zeroBytes(Number(asInt(v))));
        if (v !== null && typeof v === "object" && v.cls === T.bytes) return rt.bytes(v.items.slice());
        return rt.bytes(drain(v).map((x) => { const n = Number(needInt(x)); if (n < 0 || n > 255) fail(E.ValueError, "bytes must be in range(0, 256)"); return n; }));
    });
    methodkw(T.bytes, "decode", (a) => { const kw = kwOf(a, ["encoding", "errors"]); return rt.decodeBytes(a[0], a[1] === undefined ? kwget(kw, "encoding", "utf-8") : a[1], a[2] === undefined ? kwget(kw, "errors", "strict") : a[2]); });
    method(T.bytes, "__len__", 1, (a) => BigInt(a[0].items.length));
    method(T.bytes, "__contains__", 2, (a) => rt.baseContains(a[0], a[1]));
    method(T.bytes, "hex", 1, (a) => a[0].items.map((c) => c.toString(16).padStart(2, "0")).join(""));
    // Byte strings borrow the str algorithms through a Latin-1 view.
    const b2s = (b) => { let s = ""; for (const c of b.items) s += String.fromCharCode(c); return s; };
    const s2b = (s) => { const out = []; for (let i = 0; i < s.length; i++) out.push(s.charCodeAt(i) & 255); return rt.bytes(out); };
    const bArg = (v, what) => { if (v !== null && typeof v === "object" && v.cls === T.bytes) return b2s(v); if (isInt(v)) return String.fromCharCode(Number(asInt(v))); fail(E.TypeError, (what || "argument") + " should be integer or bytes-like object, not '" + typeOf(v).name + "'"); };
    const bIdx = (v) => v === undefined || v === null ? undefined : Number(needInt(v));
    // Searches run on the byte arrays themselves (the engine's native
    // indexOf/lastIndexOf finds each candidate first byte), with start/end
    // clamped like slice bounds.
    const bNeedle = (v) => { if (v !== null && typeof v === "object" && v.items !== undefined && isInstance(v, T.bytes)) return v.items; if (isInt(v)) { const n = Number(asInt(v)); if (n < 0 || n > 255) fail(E.ValueError, "byte must be in range(0, 256)"); return [n]; } fail(E.TypeError, "argument should be integer or bytes-like object, not '" + typeOf(v).name + "'"); };
    const bBounds = (len, s, e) => {
        let start = s === undefined || s === null ? 0 : Number(rt.indexOf(s)), end = e === undefined || e === null ? len : Number(rt.indexOf(e));
        if (start < 0) { start += len; if (start < 0) start = 0; }
        if (end < 0) { end += len; if (end < 0) end = 0; } else if (end > len) end = len;
        return [start, end];
    };
    const bMatchAt = (hay, needle, i) => { for (let k = 1; k < needle.length; k++) if (hay[i + k] !== needle[k]) return false; return true; };
    function bFind(hay, needle, start, end) {
        const n = needle.length;
        if (start > hay.length || end - start < n) return -1;
        if (n === 0) return start;
        const first = needle[0], last = end - n;
        for (let i = hay.indexOf(first, start); i >= 0 && i <= last; i = hay.indexOf(first, i + 1)) if (bMatchAt(hay, needle, i)) return i;
        return -1;
    }
    function bRfind(hay, needle, start, end) {
        const n = needle.length;
        if (start > hay.length || end - start < n) return -1;
        if (n === 0) return end;
        const first = needle[0];
        for (let i = hay.lastIndexOf(first, end - n); i >= start; i = i > 0 ? hay.lastIndexOf(first, i - 1) : -1) if (bMatchAt(hay, needle, i)) return i;
        return -1;
    }
    rt.bytesFind = bFind; rt.bytesRfind = bRfind;
    const bSearch = (a, fnc) => { const hay = a[0].items, [start, end] = bBounds(hay.length, a[2], a[3]); return fnc(hay, bNeedle(a[1]), start, end); };
    method(T.bytes, "find", 4, (a) => BigInt(bSearch(a, bFind)), 2);
    method(T.bytes, "rfind", 4, (a) => BigInt(bSearch(a, bRfind)), 2);
    method(T.bytes, "index", 4, (a) => { const i = bSearch(a, bFind); if (i < 0) fail(E.ValueError, "subsection not found"); return BigInt(i); }, 2);
    method(T.bytes, "rindex", 4, (a) => { const i = bSearch(a, bRfind); if (i < 0) fail(E.ValueError, "subsection not found"); return BigInt(i); }, 2);
    method(T.bytes, "count", 4, (a) => {
        const hay = a[0].items, needle = bNeedle(a[1]), [start, end] = bBounds(hay.length, a[2], a[3]);
        if (start > hay.length || end < start) return 0n;
        if (needle.length === 0) return BigInt(end - start + 1);
        let n = 0;
        for (let i = bFind(hay, needle, start, end); i >= 0; i = bFind(hay, needle, i + needle.length, end)) n++;
        return BigInt(n);
    }, 2);
    method(T.bytes, "startswith", 4, (a) => { const s = b2s(a[0]).slice(bIdx(a[2]) || 0, bIdx(a[3])); const p = a[1]; if (p !== null && typeof p === "object" && p.cls === T.tuple) return p.items.some((x) => s.startsWith(bArg(x))); return s.startsWith(bArg(p)); }, 2);
    method(T.bytes, "endswith", 4, (a) => { const s = b2s(a[0]).slice(bIdx(a[2]) || 0, bIdx(a[3])); const p = a[1]; if (p !== null && typeof p === "object" && p.cls === T.tuple) return p.items.some((x) => s.endsWith(bArg(x))); return s.endsWith(bArg(p)); }, 2);
    method(T.bytes, "split", 3, (a) => { const s = b2s(a[0]); const max = a[2] === undefined ? -1 : Number(needInt(a[2])); let parts; if (a[1] === undefined || a[1] === null) { parts = s.split(/[ \t\n\r\x0b\x0c]+/).filter((x) => x !== ""); if (max >= 0 && parts.length > max + 1) { const head = parts.slice(0, max); const rest = s.replace(/^[ \t\n\r\x0b\x0c]+/, ""); let pos = 0; for (let i = 0; i < max; i++) { pos = rest.indexOf(head[i], pos) + head[i].length; } parts = head.concat([rest.slice(pos).replace(/^[ \t\n\r\x0b\x0c]+/, "")]); } } else { const sep = bArg(a[1]); if (sep === "") fail(E.ValueError, "empty separator"); parts = s.split(sep); if (max >= 0 && parts.length > max + 1) parts = parts.slice(0, max).concat([parts.slice(max).join(sep)]); } return list(parts.map(s2b)); }, 1);
    // Concatenates the parts' byte arrays with the engine's native concat,
    // up to 1024 arrays per call, so a join of many parts stays linear.
    const concatArrays = (arrs) => {
        if (arrs.length < 2) return arrs.length ? arrs[0].slice() : [];
        while (arrs.length > 1) {
            const next = [];
            for (let i = 0; i < arrs.length; i += 1024) next.push(Array.prototype.concat.apply([], arrs.slice(i, i + 1024)));
            arrs = next;
        }
        return arrs[0];
    };
    rt.concatByteArrays = concatArrays;
    method(T.bytes, "join", 2, (a) => {
        const src = a[1], sep = a[0].items, arrs = [];
        const parts = src !== null && typeof src === "object" && (src.cls === T.list || src.cls === T.tuple) ? src.items : drain(src);
        const bytesType = T.bytes;
        for (let i = 0; i < parts.length; i++) {
            const x = parts[i];
            if (x === null || typeof x !== "object" || (x.cls !== bytesType && (x.items === undefined || !isInstance(x, bytesType)))) fail(E.TypeError, "sequence item " + i + ": expected a bytes-like object, " + typeOf(x).name + " found");
            if (i > 0 && sep.length) arrs.push(sep);
            arrs.push(x.items);
        }
        const out = concatArrays(arrs);
        if (out.length > rt.MAX_ITEMS) fail(E.MemoryError, "sequence limit exceeded");
        return rt.bytes(out);
    });
    const bStrip = (name, fnc) => method(T.bytes, name, 2, (a) => { const s = b2s(a[0]); const chars = a[1] === undefined || a[1] === null ? " \t\n\r\x0b\x0c" : bArg(a[1]); return s2b(fnc(s, chars)); }, 1);
    const lstrip = (s, cs) => { let i = 0; while (i < s.length && cs.indexOf(s[i]) >= 0) i++; return s.slice(i); };
    const rstrip = (s, cs) => { let j = s.length; while (j > 0 && cs.indexOf(s[j - 1]) >= 0) j--; return s.slice(0, j); };
    bStrip("strip", (s, cs) => rstrip(lstrip(s, cs), cs)); bStrip("lstrip", lstrip); bStrip("rstrip", rstrip);
    method(T.bytes, "replace", 4, (a) => { const s = b2s(a[0]), old = bArg(a[1]), nw = bArg(a[2]); const max = a[3] === undefined ? -1 : Number(needInt(a[3])); if (max < 0) return s2b(s.split(old).join(nw)); let out = "", rest = s, n = 0; while (n < max) { const i = rest.indexOf(old); if (i < 0) break; out += rest.slice(0, i) + nw; rest = rest.slice(i + old.length); n++; } return s2b(out + rest); }, 3);
    method(T.bytes, "lower", 1, (a) => s2b(b2s(a[0]).replace(/[A-Z]/g, (c) => c.toLowerCase())));
    method(T.bytes, "upper", 1, (a) => s2b(b2s(a[0]).replace(/[a-z]/g, (c) => c.toUpperCase())));
    method(T.bytes, "isdigit", 1, (a) => a[0].items.length > 0 && a[0].items.every((c) => c >= 48 && c <= 57));
    method(T.bytes, "isalpha", 1, (a) => a[0].items.length > 0 && a[0].items.every((c) => (c >= 65 && c <= 90) || (c >= 97 && c <= 122)));
    method(T.bytes, "isalnum", 1, (a) => a[0].items.length > 0 && a[0].items.every((c) => (c >= 48 && c <= 57) || (c >= 65 && c <= 90) || (c >= 97 && c <= 122)));
    method(T.bytes, "isspace", 1, (a) => a[0].items.length > 0 && a[0].items.every((c) => c === 32 || (c >= 9 && c <= 13)));
    method(T.bytes, "isupper", 1, (a) => /[A-Z]/.test(b2s(a[0])) && !/[a-z]/.test(b2s(a[0])));
    method(T.bytes, "islower", 1, (a) => /[a-z]/.test(b2s(a[0])) && !/[A-Z]/.test(b2s(a[0])));
    method(T.bytes, "zfill", 2, (a) => s2b(b2s(a[0]).padStart(Number(needInt(a[1])), "0")));
    method(T.bytes, "partition", 2, (a) => { const s = b2s(a[0]), sep = bArg(a[1]); const i = s.indexOf(sep); return tuple(i < 0 ? [a[0], rt.bytes([]), rt.bytes([])] : [s2b(s.slice(0, i)), s2b(sep), s2b(s.slice(i + sep.length))]); });
    method(T.bytes, "splitlines", 2, (a) => list(b2s(a[0]).split(/\r\n|\r|\n/).filter((x, i, arr) => !(i === arr.length - 1 && x === "")).map(s2b)), 1);
    rt.bytesToLatin1 = b2s; rt.latin1ToBytes = s2b;
    method(T.bytes, "__repr__", 1, (a) => rt.bytesRepr(a[0]));
    T.bytes.dict.set("fromhex", { cls: T.classmethod, func: builtin("fromhex", 2, (a) => { const s = needStr(a[1]).replace(/\s+/g, ""); if (s.length % 2 || /[^0-9a-fA-F]/.test(s)) fail(E.ValueError, "non-hexadecimal number found in fromhex() arg"); const out = []; for (let i = 0; i < s.length; i += 2) out.push(parseInt(s.slice(i, i + 2), 16)); return rt.bytes(out); }) });

    // ---- list ---------------------------------------------------------------------------------------------------
    const L = T.list;
    function listSelf(a) { const v = a[0]; if (v !== null && typeof v === "object" && v.items !== undefined && (v.cls === T.list || isInstance(v, T.list))) return v; fail(E.TypeError, "descriptor requires a 'list' object but received '" + typeOf(v).name + "'"); }
    rt.constructors.set(T.list, (args, kw, cls) => { const out = cls === T.list ? list([]) : rt.allocInstance(cls); if (args.length) out.items = drain(args[0]); return out; });
    rt.allocators.set(T.list, (cls) => ({ cls: cls, items: [] }));
    rt.allocators.set(T.tuple, (cls) => ({ cls: cls, items: [] }));
    rt.allocators.set(T.dict, (cls) => rt.dict(cls));
    rt.allocators.set(T.set, (cls) => rt.set(cls));
    {
        const listInit = builtin("__init__", -1, (a) => { const self = a[0]; self.items = a.length > 1 ? drain(a[1]) : []; return null; });
        listInit.c1 = (self) => { self.items = []; return null; };
        listInit.c2 = (self, x) => { self.items = (x !== null && typeof x === "object" && (x.cls === T.list || x.cls === T.tuple)) ? x.items.slice() : drain(x); return null; };
        L.dict.set("__init__", listInit);
    }
    method(L, "append", 2, (a) => { const l = listSelf(a); if (l.items.length >= rt.MAX_ITEMS) fail(E.MemoryError, "list limit exceeded"); l.items.push(a[1]); return null; });
    method(L, "extend", 2, (a) => { const l = listSelf(a); const items = drain(a[1]); for (const x of items) l.items.push(x); return null; });
    method(L, "insert", 3, (a) => { const l = listSelf(a); let i = Number(needInt(a[1])); if (i < 0) i = Math.max(0, i + l.items.length); l.items.splice(Math.min(i, l.items.length), 0, a[2]); return null; });
    method(L, "pop", 2, (a) => { const l = listSelf(a); if (l.items.length === 0) fail(E.IndexError, "pop from empty list"); if (a[1] === undefined) return l.items.pop(); let i = Number(needInt(a[1])); if (i < 0) i += l.items.length; if (i < 0 || i >= l.items.length) fail(E.IndexError, "pop index out of range"); return l.items.splice(i, 1)[0]; }, 1);
    method(L, "remove", 2, (a) => { const l = listSelf(a); const i = rt.aindex(l.items, (x) => eq(x, a[1])); if (i < 0) fail(E.ValueError, "list.remove(x): x not in list"); l.items.splice(i, 1); return null; });
    method(L, "index", 4, (a) => { const l = listSelf(a); const start = a[2] === undefined ? 0 : Number(needInt(a[2])); const end = a[3] === undefined ? l.items.length : Number(needInt(a[3])); for (let i = Math.max(0, start < 0 ? start + l.items.length : start); i < Math.min(end < 0 ? end + l.items.length : end, l.items.length); i++) if (eq(l.items[i], a[1])) return BigInt(i); fail(E.ValueError, repr(a[1]) + " is not in list"); }, 2);
    method(L, "count", 2, (a) => BigInt(rt.acount(listSelf(a).items, (x) => eq(x, a[1]))));
    method(L, "clear", 1, (a) => { listSelf(a).items.length = 0; return null; });
    method(L, "copy", 1, (a) => list(listSelf(a).items.slice()));
    method(L, "reverse", 1, (a) => { listSelf(a).items.reverse(); return null; });
    method(L, "__len__", 1, (a) => rt.baseLen(a[0]));
    method(L, "__getitem__", 2, (a) => rt.baseGetitem(a[0], a[1]));
    method(L, "__setitem__", 3, (a) => rt.baseSetitem(a[0], a[1], a[2]));
    method(L, "__delitem__", 2, (a) => rt.baseDelitem(a[0], a[1]));
    method(L, "__contains__", 2, (a) => rt.baseContains(a[0], a[1]));
    method(L, "__iter__", 1, (a) => rt.baseIter(a[0]));
    method(L, "__reversed__", 1, (a) => { const items = a[0].items; let i = items.length; return { cls: T.iterator, next: () => i > 0 ? items[--i] : STOP }; });
    method(L, "__add__", 2, (a) => rt.baseBinop("add", a[0], a[1], false));
    method(L, "__iadd__", 2, (a) => rt.baseBinop("add", a[0], a[1], true));
    method(L, "__mul__", 2, (a) => rt.baseBinop("mul", a[0], a[1], false));
    // Item-wise equality for the sequence types and their subclasses (never
    // back through `eq` on the containers, which would recurse).
    function seqEq(a, b) {
        if (a.items.length !== b.items.length) return false;
        for (let i = 0; i < a.items.length; i++) if (!eq(a.items[i], b.items[i])) return false;
        return true;
    }
    method(L, "__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.list) ? seqEq(a[0], a[1]) : NOTIMPL);
    method(L, "__ne__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.list) ? !seqEq(a[0], a[1]) : NOTIMPL);
    method(L, "__lt__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.list) ? cmp("lt", list(a[0].items), list(a[1].items)) : NOTIMPL);
    method(L, "__le__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.list) ? cmp("le", list(a[0].items), list(a[1].items)) : NOTIMPL);
    method(L, "__gt__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.list) ? cmp("gt", list(a[0].items), list(a[1].items)) : NOTIMPL);
    method(L, "__ge__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.list) ? cmp("ge", list(a[0].items), list(a[1].items)) : NOTIMPL);
    method(L, "__repr__", 1, (a) => rt.baseRepr(a[0]));
    L.dict.set("__hash__", null);
    function sortItems(items, key, reverse) {
        // Native all-in-one sort (op 4): evaluates plain/leaf/tuple-key lambdas
        // and sorts + permutes in Rust without intermediate JS arrays or loops.
        if (rt.PYORD !== null && items.length > 1) {
            const res = rt.PYORD(4, items, key, reverse ? 1 : 0, T.tuple);
            if (res !== undefined) return res;
        }
        // As CPython: reverse=True reverses, sorts stably and reverses back,
        // so equal items keep their order and `<` sees the same operands.
        // Each key through the key function's one-argument entry when it
        // has one (as `rt.call1`, with no frame of its own per item).
        let keys;
        if (key === null) keys = items.slice();
        else {
            keys = [];
            const keyObj = key !== null && typeof key === "object";
            for (let i = 0; i < items.length; i++) {
                const c1 = keyObj ? key.c1 : undefined;
                keys.push(typeof c1 === "function" ? key.c1(items[i]) : rt.call(key, [items[i]], null));
            }
        }
        const values = key === null ? null : items.slice();
        if (reverse) { keys.reverse(); if (values !== null) values.reverse(); }
        const lt = rt.fastLt;
        // Keys that are all plain values (or tuples of them): the engine sorts
        // them stably, which for such keys is exactly the order (and the tie
        // order) any stable sort by `<` gives.
        if (rt.PYORD !== null && keys.length > 1) {
            // (Op 2: tuple records read natively, nested at most 8 deep.)
            const perm = rt.PYORD(2, keys, T.tuple);
            if (perm !== undefined) {
                const ks = keys.slice(), vs = values === null ? null : values.slice();
                for (let i = 0; i < perm.length; i++) { const j = perm[i]; keys[i] = ks[j]; if (vs !== null) values[i] = vs[j]; }
                const out = values === null ? keys : values;
                if (reverse) out.reverse();
                return out;
            }
        }
        if (keys.length < 64) smallSort(keys, values, lt);
        else {
            const keyed = rt.amap(keys, (k, i) => [k, values === null ? k : values[i]]);
            const sorted = mergeSort(keyed, (a, b) => lt(a[0], b[0]));
            for (let i = 0; i < sorted.length; i++) { keys[i] = sorted[i][0]; if (values !== null) values[i] = sorted[i][1]; }
        }
        const out = values === null ? keys : values;
        if (reverse) out.reverse();
        return out;
    }
    // CPython 3.13's sort of a short list, comparison for comparison: the
    // leading run (count_run, a descending run reversed in place), then
    // binary insertion of the rest.
    function smallSort(keys, values, lt) {
        const n = keys.length;
        if (n < 2) return;
        const rev = (lo, hi) => { for (let i = lo, j = hi - 1; i < j; i++, j--) { [keys[i], keys[j]] = [keys[j], keys[i]]; if (values !== null) [values[i], values[j]] = [values[j], values[i]]; } };
        let run = 1;
        while (run < n && !lt(keys[run], keys[run - 1])) run++;
        if (run < n) {
            // A strictly ascending run ends here; a run of equal items turns
            // into the start of a descending one.
            let ascending = false;
            if (run > 1) { if (lt(keys[0], keys[run - 1])) ascending = true; else rev(0, run); }
            if (!ascending) {
                run++;
                let neq = 0;
                for (; run < n; run++) {
                    if (lt(keys[run], keys[run - 1])) { if (neq) { rev(run - neq - 1, run); neq = 0; } }
                    else if (lt(keys[run - 1], keys[run])) break;
                    else neq++;
                }
                if (neq) rev(run - neq - 1, run);
                rev(0, run);
                while (run < n && !lt(keys[run], keys[run - 1])) run++;
            }
        }
        for (let i = run; i < n; i++) {
            const pivot = keys[i]; let l = 0, r = i;
            while (l < r) { const m = (l + r) >> 1; if (lt(pivot, keys[m])) r = m; else l = m + 1; }
            if (l < i) {
                keys.copyWithin(l + 1, l, i); keys[l] = pivot;
                if (values !== null) { const pv = values[i]; values.copyWithin(l + 1, l, i); values[l] = pv; }
            }
        }
    }
    function mergeSort(arr, lt) {
        if (arr.length <= 1) return arr;
        const mid = arr.length >> 1;
        const left = mergeSort(arr.slice(0, mid), lt), right = mergeSort(arr.slice(mid), lt);
        const out = []; let i = 0, j = 0;
        while (i < left.length && j < right.length) { if (lt(right[j], left[i])) out.push(right[j++]); else out.push(left[i++]); }
        while (i < left.length) out.push(left[i++]); while (j < right.length) out.push(right[j++]);
        return out;
    }
    rt.sortItems = sortItems;
    // As CPython, the list is empty while it sorts (keys and comparisons see
    // `[]`); a failed sort puts the items back, and any change made during
    // the sort is discarded with a ValueError.
    methodkw(L, "sort", (a) => {
        const kw = kwOf(a, ["key", "reverse"]); const l = listSelf(a);
        if (a.length > 1) fail(E.TypeError, "sort() takes no positional arguments");
        const key = kwget(kw, "key", null), reverse = truth(kwget(kw, "reverse", false));
        const saved = l.items;
        l.items = [];
        let sorted;
        try { sorted = sortItems(saved, key, reverse); }
        catch (e) { l.items = saved; throw e; }
        const modified = l.items.length !== 0;
        l.items = sorted;
        if (modified) fail(E.ValueError, "list modified during sort");
        return null;
    });

    // ---- tuple ------------------------------------------------------------------------------------------------------
    rt.constructors.set(T.tuple, (args, kw, cls) => { const out = cls === T.tuple ? tuple([]) : rt.allocInstance(cls); if (args.length) out.items = drain(args[0]); return out; });
    method(T.tuple, "__len__", 1, (a) => rt.baseLen(a[0]));
    method(T.tuple, "__getitem__", 2, (a) => rt.baseGetitem(a[0], a[1]));
    method(T.tuple, "__contains__", 2, (a) => rt.baseContains(a[0], a[1]));
    method(T.tuple, "__iter__", 1, (a) => rt.baseIter(a[0]));
    method(T.tuple, "__hash__", 1, (a) => rt.baseHash(a[0]));
    method(T.tuple, "__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.tuple) ? seqEq(a[0], a[1]) : NOTIMPL);
    method(T.tuple, "__ne__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.tuple) ? !seqEq(a[0], a[1]) : NOTIMPL);
    method(T.tuple, "__lt__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.tuple) ? cmp("lt", tuple(a[0].items), tuple(a[1].items)) : NOTIMPL);
    method(T.tuple, "__le__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.tuple) ? cmp("le", tuple(a[0].items), tuple(a[1].items)) : NOTIMPL);
    method(T.tuple, "__gt__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.tuple) ? cmp("gt", tuple(a[0].items), tuple(a[1].items)) : NOTIMPL);
    method(T.tuple, "__ge__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.tuple) ? cmp("ge", tuple(a[0].items), tuple(a[1].items)) : NOTIMPL);
    method(T.tuple, "__add__", 2, (a) => rt.baseBinop("add", a[0], a[1], false));
    method(T.tuple, "__mul__", 2, (a) => rt.baseBinop("mul", a[0], a[1], false));
    method(T.tuple, "__repr__", 1, (a) => rt.baseRepr(a[0]));
    method(T.tuple, "index", 2, (a) => { const i = rt.aindex(a[0].items, (x) => eq(x, a[1])); if (i < 0) fail(E.ValueError, "tuple.index(x): x not in tuple"); return BigInt(i); });
    method(T.tuple, "count", 2, (a) => BigInt(rt.acount(a[0].items, (x) => eq(x, a[1]))));

    // ---- dict ---------------------------------------------------------------------------------------------------------
    const D = T.dict;
    function dictSelf(a) { const v = a[0]; if (v !== null && typeof v === "object" && v.map !== undefined && isInstance(v, T.dict)) return v; fail(E.TypeError, "descriptor requires a 'dict' object but received '" + typeOf(v).name + "'"); }
    function fillDict(d, src, kw) {
        if (src !== undefined && src !== null) {
            if (rt.isPlainDictStorage(src)) { rt.dictUpdate(d, src); }
            else if (typeof src === "object" && (src.cls === T.list || src.cls === T.tuple)) {
                const items = src.items;
                for (let i = 0; i < items.length; i++) {
                    const pair = items[i];
                    if (pair !== null && typeof pair === "object" && (pair.cls === T.tuple || pair.cls === T.list)) {
                        const pi = pair.items;
                        if (pi.length !== 2) fail(E.ValueError, "dictionary update sequence element has length " + pi.length + "; 2 is required");
                        dictSet(d, pi[0], pi[1]);
                    } else {
                        const kv = drain(pair);
                        if (kv.length !== 2) fail(E.ValueError, "dictionary update sequence element has length " + kv.length + "; 2 is required");
                        dictSet(d, kv[0], kv[1]);
                    }
                }
            }
            else if (typeof src === "object" && typeMethod(src, "keys") !== undefined) { for (const [k, v] of dictEntries(rt.asDict(src))) dictSet(d, k, v); }
            else {
                for (const pair of drain(src)) {
                    if (pair !== null && typeof pair === "object" && (pair.cls === T.tuple || pair.cls === T.list)) {
                        const pi = pair.items;
                        if (pi.length !== 2) fail(E.ValueError, "dictionary update sequence element has length " + pi.length + "; 2 is required");
                        dictSet(d, pi[0], pi[1]);
                    } else {
                        const kv = drain(pair);
                        if (kv.length !== 2) fail(E.ValueError, "dictionary update sequence element has length " + kv.length + "; 2 is required");
                        dictSet(d, kv[0], kv[1]);
                    }
                }
            }
        }
        if (kw) for (const [k, v] of kw) dictSet(d, k, v);
    }
    rt.constructors.set(T.dict, (args, kw, cls) => { const d = cls === T.dict ? dict() : rt.allocInstance(cls); fillDict(d, args[0], kw); return d; });
    methodkw(D, "__init__", (a) => { const kw = kwOf(a, null); fillDict(a[0], a[1], kw); return null; });
    method(D, "__len__", 1, (a) => rt.baseLen(a[0]));
    method(D, "__getitem__", 2, (a) => rt.baseGetitem(a[0], a[1]));
    method(D, "__setitem__", 3, (a) => rt.baseSetitem(a[0], a[1], a[2]));
    method(D, "__delitem__", 2, (a) => rt.baseDelitem(a[0], a[1]));
    method(D, "__contains__", 2, (a) => rt.dictHas(a[0], a[1]));
    method(D, "__iter__", 1, (a) => rt.baseIter(a[0]));
    method(D, "__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && isInstance(a[1], T.dict) ? rt.dictEq(a[0], a[1]) : NOTIMPL);
    method(D, "__repr__", 1, (a) => rt.baseRepr(a[0]));
    method(D, "__or__", 2, (a) => rt.baseBinop("or", a[0], a[1], false));
    D.dict.set("__hash__", null);
    method(D, "get", 3, (a) => { const v = dictGet(dictSelf(a), a[1]); return v === undefined ? (a[2] === undefined ? null : a[2]) : v; }, 2);
    method(D, "setdefault", 3, (a) => { const d = dictSelf(a); const v = dictGet(d, a[1]); if (v !== undefined) return v; const dflt = a[2] === undefined ? null : a[2]; dictSet(d, a[1], dflt); return dflt; }, 2);
    method(D, "pop", 3, (a) => { const v = rt.dictPop(dictSelf(a), a[1]); if (v === undefined) { if (a[2] === undefined) throw rt.makeExc(E.KeyError, [a[1]]); return a[2]; } return v; }, 2);
    method(D, "popitem", 1, (a) => { const e = rt.dictPopItem(dictSelf(a), false); if (e === undefined) fail(E.KeyError, "popitem(): dictionary is empty"); return tuple([e[0], e[1]]); });
    methodkw(D, "update", (a) => { const kw = kwOf(a, null); fillDict(dictSelf(a), a[1], kw); return null; });
    method(D, "clear", 1, (a) => { rt.dictClear(dictSelf(a)); return null; });
    method(D, "copy", 1, (a) => rt.dictCopy(dictSelf(a)));
    // A view's iterator keeps its state in its own fields (`a` the entries
    // snapshot, `i` the position, `b` the dict, `size` its size then, `pick`
    // 0/1/2 for keys/values/items), so its step is one shared function
    // (`viewNext`, as `jnext`) or the engine's native form of it
    // (`vm::py_str`, `__zipp_py_iter`, bound only in a Python program).
    function viewNext() {
        if (this.b.size !== this.size) fail(E.RuntimeError, "dictionary changed size during iteration");
        const entries = this.a;
        if (this.i < entries.length) { const e = entries[this.i++]; const p = this.pick; return p === 0 ? e[0] : p === 1 ? e[1] : tuple([e[0], e[1]]); }
        return STOP;
    }
    let ITER_STEP = null;
    try { ITER_STEP = typeof __zipp_py_iter === "function" ? __zipp_py_iter : null; } catch (e) { ITER_STEP = null; }
    const ITER_TMPL = list([]);
    function view(type, d, pick) {
        return { cls: type, of: d, iter: () => ({ cls: T.iterator, next: ITER_STEP !== null ? ITER_STEP : viewNext, jnext: viewNext,
            kind: 1, a: dictEntryList(d), i: 0, b: d, size: d.size, pick: pick, tmpl: ITER_TMPL, ttype: T.tuple }) };
    }
    rt.iterStep = ITER_STEP; rt.iterTmpl = ITER_TMPL;
    method(D, "keys", 1, (a) => view(T.dict_keys, dictSelf(a), 0));
    method(D, "values", 1, (a) => view(T.dict_values, dictSelf(a), 1));
    method(D, "items", 1, (a) => view(T.dict_items, dictSelf(a), 2));
    for (const v of [T.dict_keys, T.dict_values, T.dict_items]) {
        method(v, "__len__", 1, (a) => BigInt(a[0].of.size));
        method(v, "__iter__", 1, (a) => a[0].iter());
        method(v, "__repr__", 1, (a) => repr(a[0]));
        method(v, "__reversed__", 1, (a) => { const xs = drain(a[0]); let i = xs.length; return { cls: T.reversed, next: () => i > 0 ? xs[--i] : STOP }; });
    }
    method(T.dict_keys, "__contains__", 2, (a) => rt.dictHas(a[0].of, a[1]));
    method(T.dict_items, "__contains__", 2, (a) => { const p = a[1]; if (p === null || typeof p !== "object" || p.cls !== T.tuple || p.items.length !== 2) return false; const v = rt.dictGet(a[0].of, p.items[0]); return v !== undefined && rt.eq(v, p.items[1]); });
    // Keys and items views behave as sets in the set operators.
    for (const v of [T.dict_keys, T.dict_items]) {
        for (const [name, op] of [["__sub__", "sub"], ["__and__", "and"], ["__or__", "or"], ["__xor__", "xor"]]) {
            method(v, name, 2, (a) => rt.setBinop(op, setFrom(a[0], T.set), setFrom(a[1], T.set), false));
            method(v, "__r" + name.slice(2), 2, (a) => rt.setBinop(op, setFrom(a[1], T.set), setFrom(a[0], T.set), false));
        }
        method(v, "isdisjoint", 2, (a) => rt.setBinop("and", setFrom(a[0], T.set), setFrom(a[1], T.set), false).size === 0);
        // Set-like equality with sets, frozensets and the other keys/items views.
        method(v, "__eq__", 2, (a) => {
            const self = a[0], o = a[1];
            if (o === null || typeof o !== "object" || !(o.cls === T.dict_keys || o.cls === T.dict_items || isInstance(o, T.set) || isInstance(o, T.frozenset))) return NOTIMPL;
            if (len(self) !== len(o)) return false;
            const it = self.iter();
            for (;;) { const x = it.next(); if (x === STOP) return true; if (!rt.contains(o, x)) return false; }
        });
    }
    D.dict.set("fromkeys", { cls: T.classmethod, func: builtin("fromkeys", 3, (a) => { const d = rt.construct(a[0], [], null); const v = a[2] === undefined ? null : a[2]; for (const k of drain(a[1])) dictSet(d, k, v); return d; }, 2) });

    // ---- set / frozenset -------------------------------------------------------------------------------------------------
    for (const [type, frozen] of [[T.set, false], [T.frozenset, true]]) {
        rt.constructors.set(type, (args, kw, cls) => {
            if (cls === type) {
                if (!args.length) return set(type);
                return frozen && args[0] !== null && typeof args[0] === "object" && args[0].cls === T.frozenset ? args[0] : setFrom(args[0], type);
            }
            const s = rt.allocInstance(cls);
            if (args.length) for (const x of drain(args[0])) setAdd(s, x);
            return s;
        });
        method(type, "__len__", 1, (a) => rt.baseLen(a[0]));
        method(type, "__contains__", 2, (a) => setHas(a[0], a[1]));
        method(type, "__iter__", 1, (a) => rt.baseIter(a[0]));
        method(type, "__repr__", 1, (a) => rt.baseRepr(a[0]));
        method(type, "__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && (a[1].cls === T.set || a[1].cls === T.frozenset) ? rt.setEq(a[0], a[1]) : NOTIMPL);
        // intersection() with a non-set argument keeps the argument's elements.
        const intersectIterable = (s, other) => {
            if (other !== null && typeof other === "object" && other.map !== undefined && !isInstance(other, T.dict)) return rt.setBinop("and", s, other, false);
            const out = set(s.cls); for (const x of drain(other)) if (setHas(s, x)) setAdd(out, x); return out;
        };
        for (const [name, op] of [["union", "or"], ["intersection", "and"], ["difference", "sub"], ["symmetric_difference", "xor"]]) {
            const step = op === "and" ? intersectIterable : (out, other) => rt.setBinop(op, out, setFrom(other, type), false);
            method(type, name, -1, (a) => { let out = setFrom(a[0], type); for (let i = 1; i < a.length; i++) out = step(out, a[i]); return out; });
            method(type, "__" + op + "__", 2, (a) => rt.baseBinop(op, a[0], a[1], false));
        }
        method(type, "issubset", 2, (a) => rt.setCompare("le", a[0], setFrom(a[1], type)));
        method(type, "issuperset", 2, (a) => rt.setCompare("ge", a[0], setFrom(a[1], type)));
        method(type, "isdisjoint", 2, (a) => { for (const x of drain(a[1])) if (setHas(a[0], x)) return false; return true; });
        method(type, "copy", 1, (a) => setFrom(a[0], type));
        if (frozen) { method(type, "__hash__", 1, (a) => rt.baseHash(a[0])); continue; }
        type.dict.set("__hash__", null);
        method(type, "__init__", -1, (a) => { const s = a[0]; rt.setClear(s); if (a.length > 1) for (const x of drain(a[1])) setAdd(s, x); return null; });
        method(type, "add", 2, (a) => { setAdd(a[0], a[1]); return null; });
        method(type, "remove", 2, (a) => { if (!setDel(a[0], a[1])) throw rt.makeExc(E.KeyError, [a[1]]); return null; });
        method(type, "discard", 2, (a) => { setDel(a[0], a[1]); return null; });
        method(type, "pop", 1, (a) => { const v = rt.setPop(a[0]); if (v === undefined) fail(E.KeyError, "pop from an empty set"); return v; });
        method(type, "clear", 1, (a) => { rt.setClear(a[0]); return null; });
        method(type, "update", -1, (a) => { for (let i = 1; i < a.length; i++) for (const x of drain(a[i])) setAdd(a[0], x); return null; });
        method(type, "intersection_update", -1, (a) => { for (let i = 1; i < a.length; i++) { rt.setReplace(a[0], intersectIterable(a[0], a[i])); } return null; });
        method(type, "difference_update", -1, (a) => { for (let i = 1; i < a.length; i++) rt.setBinop("sub", a[0], setFrom(a[i], type), true); return null; });
        method(type, "symmetric_difference_update", 2, (a) => { rt.setBinop("xor", a[0], setFrom(a[1], type), true); return null; });
    }

    // ---- range / slice / iterator types --------------------------------------------------------------------------------------------
    rt.constructors.set(T.range, (args) => {
        if (args.length < 1 || args.length > 3) fail(E.TypeError, "range expected at most 3 arguments, got " + args.length);
        const start = args.length === 1 ? 0n : rt.indexOf(args[0], "range");
        const stop = rt.indexOf(args[args.length === 1 ? 0 : 1], "range");
        const step = args.length === 3 ? rt.indexOf(args[2], "range") : 1n;
        if (step === 0n) fail(E.ValueError, "range() arg 3 must not be zero");
        return rt.range(start, stop, step);
    });
    method(T.range, "__len__", 1, (a) => rt.rangeLength(a[0]));
    method(T.range, "__iter__", 1, (a) => rt.baseIter(a[0]));
    method(T.range, "__contains__", 2, (a) => rt.baseContains(a[0], a[1]));
    method(T.range, "__getitem__", 2, (a) => rt.baseGetitem(a[0], a[1]));
    method(T.range, "__repr__", 1, (a) => repr(a[0]));
    method(T.range, "__reversed__", 1, (a) => { const r = a[0]; const n = rt.rangeLength(r); let i = n; return { cls: T.iterator, next: () => i > 0n ? r.start + (--i) * r.step : STOP }; });
    method(T.range, "index", 2, (a) => { const r = a[0]; if (!rt.contains(r, a[1])) fail(E.ValueError, repr(a[1]) + " is not in range"); return (asInt(a[1]) - r.start) / r.step; });
    method(T.range, "count", 2, (a) => rt.contains(a[0], a[1]) ? 1n : 0n);
    rt.constructors.set(T.slice, (args) => { if (args.length === 1) return R.slice(null, args[0], null); return R.slice(args[0], args[1], args[2] === undefined ? null : args[2]); });
    method(T.slice, "indices", 2, (a) => { const [s, e, st] = rt.sliceIndices(a[0], Number(needInt(a[1]))); return tuple([BigInt(s), BigInt(e), BigInt(st)]); });
    for (const t of [T.iterator, T.list_iterator, T.generator, T.enumerate, T.zip, T.map, T.filter, T.reversed]) {
        method(t, "__iter__", 1, (a) => a[0]);
        method(t, "__next__", 1, (a) => { const v = a[0].next(); if (v === STOP) throw rt.makeExc(E.StopIteration, []); return v; });
    }
    let BUILTIN_STRM = null;
    try { BUILTIN_STRM = typeof __zipp_py_strm === "function" ? __zipp_py_strm : null; } catch (e) { BUILTIN_STRM = null; }
    {
        const genSendMethod = builtin("send", 2, (a) => a[0].send(a[1]));
        const genSendJ2 = (g, v) => g.send(v);
        if (BUILTIN_STRM !== null) {
            genSendMethod.strop = 54;
            genSendMethod.j2 = genSendJ2;
            genSendMethod.c2 = BUILTIN_STRM;
            genSendMethod.sendDone = (g) => { throw rt.makeExc(E.StopIteration, g.returned === null ? [] : [g.returned]); };
        } else {
            genSendMethod.c2 = genSendJ2;
        }
        T.generator.dict.set("send", genSendMethod);
    }
    method(T.generator, "close", 1, (a) => a[0].close());
    method(T.generator, "throw", -1, (a) => { let e = a[1]; if (isType(e)) e = rt.construct(e, a[2] === undefined ? [] : [a[2]], null); return a[0].throwIn(e); });
    method(T.coroutine, "send", 2, (a) => a[0].send(a[1]));
    method(T.coroutine, "close", 1, (a) => a[0].close());
    method(T.coroutine, "throw", -1, (a) => { let e = a[1]; if (isType(e)) e = rt.construct(e, a[2] === undefined ? [] : [a[2]], null); return a[0].throwIn(e); });
    method(T.coroutine, "__await__", 1, (a) => rt.makeCoroutineWrapper(a[0]));
    for (const t of [T.coroutine_wrapper, T.async_generator_asend]) {
        method(t, "__iter__", 1, (a) => a[0]);
        method(t, "__next__", 1, (a) => { const v = a[0].next(); if (v === STOP) throw rt.makeExc(E.StopIteration, a[0].returned === null || a[0].returned === undefined ? [] : [a[0].returned]); return v; });
        method(t, "send", 2, (a) => a[0].send(a[1]));
        method(t, "close", 1, (a) => a[0].close());
        method(t, "throw", -1, (a) => { let e = a[1]; if (isType(e)) e = rt.construct(e, a[2] === undefined ? [] : [a[2]], null); return a[0].throwIn(e); });
    }
    method(T.async_generator_asend, "__await__", 1, (a) => a[0]);
    method(T.async_generator, "__aiter__", 1, (a) => a[0]);
    method(T.async_generator, "__anext__", 1, (a) => rt.makeAgStep(a[0], 0, null));
    method(T.async_generator, "asend", 2, (a) => rt.makeAgStep(a[0], 0, a[1]));
    method(T.async_generator, "athrow", -1, (a) => { let e = a[1]; if (isType(e)) e = rt.construct(e, a[2] === undefined ? [] : [a[2]], null); return rt.makeAgStep(a[0], 1, e); });
    method(T.async_generator, "aclose", 1, (a) => rt.makeAgStep(a[0], 2, null));
    // `enumerate` over an exact list or tuple keeps its state in its own
    // fields (`a` the sequence, `i` the position, `b` the count), as the
    // list iterator it would wrap reads the live items (see `view`).
    function enumNext() {
        const items = this.a.items;
        if (this.i < items.length) { const v = items[this.i++]; const c = this.b; this.b = c + 1n; return tuple([c, v]); }
        this.i = Infinity; return STOP;
    }
    rt.constructors.set(T.enumerate, (args, kw) => {
        let start = kw && kw.has("start") ? needInt(kw.get("start")) : args[1] === undefined ? 0n : needInt(args[1]);
        const src = args[0];
        if (src !== null && typeof src === "object" && (src.cls === T.list || src.cls === T.tuple) && src.items !== undefined) {
            return { cls: T.enumerate, next: rt.iterStep !== null ? rt.iterStep : enumNext, jnext: enumNext,
                kind: 2, a: src, i: 0, b: start, size: 0, pick: 0, tmpl: rt.iterTmpl, ttype: T.tuple };
        }
        const it = iter(args[0]); let i = start;
        return { cls: T.enumerate, next: () => { const v = fornext(it); if (v === STOP) return STOP; return tuple([i++, v]); } };
    });
    // `zip` over exact lists and tuples keeps its state in its own fields
    // (`a` the sequences, `i` the common position; see `view`): the list
    // iterators it would wrap read the live items, and once one ends every
    // later step ends too.
    function zipNext() {
        const srcs = this.a, i = this.i, out = new Array(srcs.length);
        for (let j = 0; j < srcs.length; j++) { const items = srcs[j].items; if (!(i < items.length)) { this.i = Infinity; return STOP; } out[j] = items[i]; }
        this.i = i + 1; return tuple(out);
    }
    rt.constructors.set(T.zip, (args, kw) => {
        if ((kw === null || kw === undefined || kw.size === 0) && args.length > 0
            && rt.aevery(args, (s) => s !== null && typeof s === "object" && (s.cls === T.list || s.cls === T.tuple) && s.items !== undefined)) {
            return { cls: T.zip, next: rt.iterStep !== null ? rt.iterStep : zipNext, jnext: zipNext,
                kind: 4, a: args.slice(), i: 0, b: null, size: 0, pick: 0, tmpl: rt.iterTmpl, ttype: T.tuple };
        }
        const strict = kw && kw.has("strict") ? truth(kw.get("strict")) : false;
        const its = args.map(iter);
        return { cls: T.zip, next: () => {
            if (its.length === 0) return STOP;
            const out = [];
            for (let i = 0; i < its.length; i++) { const v = fornext(its[i]); if (v === STOP) { if (strict && i > 0) fail(E.ValueError, "zip() argument " + (i + 1) + " is shorter than argument 1"); if (strict && i === 0) for (let j = 1; j < its.length; j++) if (fornext(its[j]) !== STOP) fail(E.ValueError, "zip() argument " + (j + 1) + " is longer than argument 1"); return STOP; } out.push(v); }
            return tuple(out);
        } };
    });
    rt.constructors.set(T.map, (args) => {
        const f = args[0]; const its = args.slice(1).map(iter);
        return { cls: T.map, next: () => { const vals = []; for (const it of its) { const v = fornext(it); if (v === STOP) return STOP; vals.push(v); } return vals.length === 1 ? rt.call1(f, vals[0]) : call(f, vals, null); } };
    });
    rt.constructors.set(T.filter, (args) => {
        const f = args[0]; const it = iter(args[1]);
        return { cls: T.filter, next: () => { for (;;) { const v = fornext(it); if (v === STOP) return STOP; if (f === null ? truth(v) : truth(rt.call1(f, v))) return v; } } };
    });
    rt.constructors.set(T.reversed, (args) => {
        const v = args[0];
        if (v !== null && typeof v === "object" && (v.cls === T.list || v.cls === T.tuple)) { let i = v.items.length; return { cls: T.reversed, next: () => i > 0 ? v.items[--i] : STOP }; }
        if (typeof v === "string") { const cps = codepoints(v); let i = cps.length; return { cls: T.reversed, next: () => i > 0 ? cps[--i] : STOP }; }
        if (v !== null && typeof v === "object" && v.map !== undefined && isInstance(v, T.dict)) { const ks = rt.dictEntryList(v).map((e) => e[0]); let i = ks.length; return { cls: T.reversed, next: () => i > 0 ? ks[--i] : STOP }; }
        if (v !== null && typeof v === "object" && (v.cls === T.dict_keys || v.cls === T.dict_values || v.cls === T.dict_items)) { const xs = drain(v); let i = xs.length; return { cls: T.reversed, next: () => i > 0 ? xs[--i] : STOP }; }
        const m = typeMethod(v, "__reversed__");
        if (m !== undefined) return call(descrGet(m, v, typeOf(v)), [], null);
        if (typeMethod(v, "__len__") !== undefined && typeMethod(v, "__getitem__") !== undefined) { let i = len(v); return { cls: T.reversed, next: () => i > 0n ? getitem(v, --i) : STOP }; }
        fail(E.TypeError, "'" + typeOf(v).name + "' object is not reversible");
    });

    for (const t of [T.list, T.tuple, T.dict, T.set, T.frozenset, T.str, T.range, T.bytes, T.int, T.float, T.bool, ObjectType]) {
        for (const v of t.dict.values()) if (v !== null && typeof v === "object" && v.cls === T.builtin_function_or_method) v.isBase = true;
    }
    for (const t of [T.str, T.int, T.float, T.bool]) {
        for (const v of t.dict.values()) if (v !== null && typeof v === "object" && v.cls === T.builtin_function_or_method) v.unboxSelf = true;
    }
    // Direct positional entries (`c0`..`c3`) on builtin types so `str(x)`,
    // `set()`, `set(x)`, `list(x)`, `tuple(x)`, `dict()`, `int(x)`, `bool(x)`,
    // `range(...)` hit `PyCall` directly without `constructDefault`/`ctorPlan`.
    {
        const intCtor = rt.constructors.get(T.int);
        const floatCtor = rt.constructors.get(T.float);
        const enumCtor = rt.constructors.get(T.enumerate);
        const revCtor = rt.constructors.get(T.reversed);
        T.str.c0 = () => "";
        T.str.c1 = (x) => typeof x === "string" ? x : str(x);
        T.bool.c0 = () => false;
        T.bool.c1 = (x) => truth(x);
        T.int.c0 = () => 0n;
        T.int.c1 = (x) => isInt(x) ? asInt(x) : typeof x === "number" ? rt.floatToInt(x) : typeof x === "string" ? parseIntLiteral(x, 10) : intCtor([x], null);
        T.int.c2 = (x, b) => intCtor([x, b], null);
        T.float.c0 = () => 0.0;
        T.float.c1 = (x) => typeof x === "number" ? x : isInt(x) ? toFloat(x) : typeof x === "string" ? parseFloatLiteral(x) : floatCtor([x], null);
        const listJ0 = () => list([]);
        const listJ1 = (x) => (x !== null && typeof x === "object" && (x.cls === T.list || x.cls === T.tuple)) ? list(x.items.slice()) : list(drain(x));
        if (BUILTIN_STRM !== null) {
            T.list.strop = 52;
            T.list.j0 = listJ0;
            T.list.j1 = listJ1;
            T.list.c0 = BUILTIN_STRM;
            T.list.c1 = BUILTIN_STRM;
        } else {
            T.list.c0 = listJ0;
            T.list.c1 = listJ1;
        }
        const tupleJ0 = () => tuple([]);
        const tupleJ1 = (x) => (x !== null && typeof x === "object" && x.cls === T.tuple) ? x : (x !== null && typeof x === "object" && x.cls === T.list) ? tuple(x.items.slice()) : tuple(drain(x));
        if (BUILTIN_STRM !== null) {
            T.tuple.strop = 55;
            T.tuple.j0 = tupleJ0;
            T.tuple.j1 = tupleJ1;
            T.tuple.c0 = BUILTIN_STRM;
            T.tuple.c1 = BUILTIN_STRM;
        } else {
            T.tuple.c0 = tupleJ0;
            T.tuple.c1 = tupleJ1;
        }
        T.dict.c0 = () => dict();
        T.dict.c1 = (x) => { const d = dict(); fillDict(d, x, null); return d; };
        T.set.c0 = () => set(T.set);
        T.set.c1 = (x) => setFrom(x, T.set);
        T.frozenset.c0 = () => set(T.frozenset);
        T.frozenset.c1 = (x) => (x !== null && typeof x === "object" && x.cls === T.frozenset) ? x : setFrom(x, T.frozenset);
        T.range.c1 = (stop) => rt.range(0n, rt.indexOf(stop, "range"), 1n);
        T.range.c2 = (start, stop) => rt.range(rt.indexOf(start, "range"), rt.indexOf(stop, "range"), 1n);
        T.range.c3 = (start, stop, step) => { const st = rt.indexOf(step, "range"); if (st === 0n) fail(E.ValueError, "range() arg 3 must not be zero"); return rt.range(rt.indexOf(start, "range"), rt.indexOf(stop, "range"), st); };
        T.slice.c1 = (stop) => R.slice(null, stop, null);
        T.slice.c2 = (start, stop) => R.slice(start, stop, null);
        T.slice.c3 = (start, stop, step) => R.slice(start, stop, step);
        T.enumerate.c1 = (it) => enumCtor([it], null);
        T.reversed.c1 = (it) => revCtor([it], null);
    }
    // ---- complex ---------------------------------------------------------------------------------------------------
    // The type's methods and constructor (CPython 3.13's complexobject.c);
    // the value helpers are in types.js. Methods read `re`/`im` from self,
    // which may be a subclass instance.
    const CX = T.complex, cx = rt.cx, isCx = rt.isCx;
    for (const op of ["add", "sub", "mul", "truediv"]) {
        method(CX, "__" + op + "__", 2, (a) => rt.cxBinop(op, a[0], a[1]));
        method(CX, "__r" + op + "__", 2, (a) => rt.cxBinop(op, a[1], a[0]));
    }
    // A third argument other than None is refused once both operands convert.
    const cxPowMethod = (x, y, z) => {
        if (z !== undefined && z !== null) { if (rt.cxParts(x) === null || rt.cxParts(y) === null) return NOTIMPL; fail(E.ValueError, "complex modulo"); }
        return rt.cxBinop("pow", x, y);
    };
    method(CX, "__pow__", 3, (a) => cxPowMethod(a[0], a[1], a[2]), 2);
    method(CX, "__rpow__", 3, (a) => cxPowMethod(a[1], a[0], a[2]), 2);
    method(CX, "__neg__", 1, (a) => cx(-a[0].re, -a[0].im));
    method(CX, "__pos__", 1, (a) => a[0].cls === CX ? a[0] : cx(a[0].re, a[0].im));
    method(CX, "__abs__", 1, (a) => {
        const r = rt.cxAbs(a[0].re, a[0].im);
        if ((r === Infinity) && Number.isFinite(a[0].re) && Number.isFinite(a[0].im)) fail(E.OverflowError, "absolute value too large");
        return r;
    });
    method(CX, "__bool__", 1, (a) => a[0].re !== 0 || a[0].im !== 0);
    method(CX, "__eq__", 2, (a) => rt.cxEq(a[0], a[1]));
    method(CX, "__ne__", 2, (a) => { const r = rt.cxEq(a[0], a[1]); return r === NOTIMPL ? r : !r; });
    for (const name of ["__lt__", "__le__", "__gt__", "__ge__"]) method(CX, name, 2, () => NOTIMPL);
    method(CX, "__hash__", 1, (a) => rt.cxHash(a[0].re, a[0].im));
    rt.setCxHash(CX.dict.get("__hash__"));
    method(CX, "__repr__", 1, (a) => rt.cxRepr(a[0].re, a[0].im));
    method(CX, "__format__", 2, (a) => rt.formatComplex(a[0], needStr(a[1], "complex.__format__() argument")));
    method(CX, "__complex__", 1, (a) => a[0].cls === CX ? a[0] : cx(a[0].re, a[0].im));
    method(CX, "__getnewargs__", 1, (a) => tuple([a[0].re, a[0].im]));
    method(CX, "conjugate", 1, (a) => cx(a[0].re, -a[0].im));
    CX.dict.set("real", { cls: T.property, fget: builtin("real", 1, (a) => a[0].re), fset: null, fdel: null, doc: "the real part of a complex number" });
    CX.dict.set("imag", { cls: T.property, fget: builtin("imag", 1, (a) => a[0].im), fset: null, fdel: null, doc: "the imaginary part of a complex number" });
    const strValue = (v) => typeof v === "string" ? v : v !== null && typeof v === "object" && typeof v.pyval === "string" ? v.pyval : undefined;
    const isIntLike = (v) => isInt(v) || (v !== null && typeof v === "object" && typeof v.pyval === "bigint");
    // PyNumber_Float, for a value known to have __float__ or __index__.
    function numberFloat(v) {
        if (typeof v === "number") return v;
        if (isInt(v)) return toFloat(v);
        const m = typeMethod(v, "__float__");
        if (m !== undefined) {
            const r = call(descrGet(m, v, typeOf(v)), [], null);
            if (typeof r === "number") return r;
            if (r !== null && typeof r === "object" && typeof r.pyval === "number") return r.pyval;
            fail(E.TypeError, typeOf(v).name + ".__float__ returned non-float (type " + typeOf(r).name + ")");
        }
        const r = call(descrGet(typeMethod(v, "__index__"), v, typeOf(v)), [], null);
        if (!isIntLike(r)) fail(E.TypeError, "__index__ returned non-int (type " + typeOf(r).name + ")");
        return toFloat(unbox(r));
    }
    // The `nb_float || nb_index || complex` test complex() makes of its arguments.
    const numberLike = (v) => isNum(v) || isCx(v) || typeMethod(v, "__float__") !== undefined || typeMethod(v, "__index__") !== undefined;
    rt.numberFloat = numberFloat;
    // complex_from_string_inner over the ASCII form of the text.
    const PY_SPACE = (c) => c === " " || c === "\t" || c === "\n" || c === "\v" || c === "\f" || c === "\r";
    const FLOAT_PREFIX = /^[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?/, INF_NAN_PREFIX = /^([+-]?)(inf(?:inity)?|nan)/i;
    // PyOS_string_to_double's longest float prefix at `p`: [value, end].
    function floatPrefix(s, p) {
        const rest = s.slice(p);
        let m = FLOAT_PREFIX.exec(rest);
        if (m !== null) return [Number(m[0]), p + m[0].length];
        m = INF_NAN_PREFIX.exec(rest);
        if (m !== null) return [m[2][0] === "n" || m[2][0] === "N" ? NaN : m[1] === "-" ? -Infinity : Infinity, p + m[0].length];
        return [-1, p];
    }
    function parseComplex(s) {
        const n = s.length, isJ = (c) => c === "j" || c === "J";
        let p = 0, x = 0, y = 0, bracket = false;
        while (p < n && PY_SPACE(s[p])) p++;
        if (s[p] === "(") { bracket = true; p++; while (p < n && PY_SPACE(s[p])) p++; }
        const [z, end] = floatPrefix(s, p);
        if (end !== p) {
            p = end;
            if (s[p] === "+" || s[p] === "-") {
                x = z;
                const [w, end2] = floatPrefix(s, p);
                if (end2 !== p) { y = w; p = end2; } else { y = s[p] === "+" ? 1 : -1; p++; }
                if (!isJ(s[p])) return null;
                p++;
            } else if (isJ(s[p])) { p++; y = z; }
            else x = z;
        } else {
            if (s[p] === "+" || s[p] === "-") { y = s[p] === "+" ? 1 : -1; p++; }
            else y = 1;
            if (!isJ(s[p])) return null;
            p++;
        }
        while (p < n && PY_SPACE(s[p])) p++;
        if (bracket) {
            if (s[p] !== ")") return null;
            p++;
            while (p < n && PY_SPACE(s[p])) p++;
        }
        return p === n ? [x, y] : null;
    }
    // A Unicode decimal digit's value: its offset in its run of digits
    // (every run of category Nd is whole decades starting at a zero).
    const ND = /^\p{Nd}$/u;
    function digitValue(cp) {
        let start = cp;
        while (start > 0 && ND.test(String.fromCodePoint(start - 1))) start--;
        return (cp - start) % 10;
    }
    const UNICODE_SPACE = /^[\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]$/;
    function complexFromString(text, orig) {
        // _PyUnicode_TransformDecimalAndSpaceToASCII: other whitespace is a
        // space, other decimal digits their ASCII digit, anything else '?'.
        let s = "";
        for (const ch of text) {
            const c = ch.codePointAt(0);
            if (c < 128) s += ch;
            else if (UNICODE_SPACE.test(ch)) s += " ";
            else if (ND.test(ch)) s += String(digitValue(c));
            else s += "?";
        }
        // _Py_string_to_number_with_underscores: single underscores only
        // between digits.
        if (s.includes("_")) {
            const bad = () => fail(E.ValueError, "could not convert string to complex: " + repr(orig));
            let out = "", prev = "";
            for (const ch of s) {
                if (ch === "\0") bad();
                const digit = ch >= "0" && ch <= "9";
                if (ch === "_") { if (!(prev >= "0" && prev <= "9")) bad(); }
                else { out += ch; if (prev === "_" && !digit) bad(); }
                prev = ch;
            }
            if (prev === "_") bad();
            s = out;
        }
        const parts = parseComplex(s);
        if (parts === null) fail(E.ValueError, "complex() arg is a malformed string");
        return cx(parts[0], parts[1]);
    }
    rt.constructors.set(CX, (args, kw, cls) => {
        const given = args.length + (kw !== null && kw !== undefined ? kw.size : 0);
        if (given > 2) fail(E.TypeError, "complex() takes at most 2 arguments (" + given + " given)");
        let r = args[0], i = args[1];
        if (kw !== null && kw !== undefined) {
            for (const [k, v] of kw) {
                if (k === "real") { if (args.length >= 1) fail(E.TypeError, "argument for complex() given by name ('real') and position (1)"); r = v; }
                else if (k === "imag") { if (args.length >= 2) fail(E.TypeError, "argument for complex() given by name ('imag') and position (2)"); i = v; }
                else fail(E.TypeError, "complex() got an unexpected keyword argument '" + k + "'");
            }
        }
        if (r === undefined) r = 0n;
        if (i === undefined && cls === CX && r !== null && typeof r === "object" && r.cls === CX) return r;
        const rs = strValue(r);
        if (rs !== undefined) {
            if (i !== undefined) fail(E.TypeError, "complex() can't take second arg if first is a string");
            return complexFromString(rs, r);
        }
        if (i !== undefined && strValue(i) !== undefined) fail(E.TypeError, "complex() second arg can't be a string");
        // try_complex_special_method (exact complex answers itself).
        if (!(r !== null && typeof r === "object" && r.cls === CX)) {
            const m = typeMethod(r, "__complex__");
            if (m !== undefined) {
                const res = call(descrGet(m, r, typeOf(r)), [], null);
                if (!isCx(res)) fail(E.TypeError, "__complex__ returned non-complex (type " + typeOf(res).name + ")");
                r = res;
            }
        }
        if (!numberLike(r)) fail(E.TypeError, "complex() first argument must be a string or a number, not '" + typeOf(r).name + "'");
        if (i !== undefined && !numberLike(i)) fail(E.TypeError, "complex() second argument must be a number, not '" + typeOf(i).name + "'");
        let crr, cri = 0, rComplex = false;
        if (isCx(r)) { crr = r.re; cri = r.im; rComplex = true; } else crr = numberFloat(r);
        let cir;
        if (i === undefined) cir = cri;
        else if (isCx(i)) { cir = i.re; crr -= i.im; }
        else cir = numberFloat(i);
        if (rComplex && i !== undefined) cir += cri;
        return cx(crr, cir);
    });
    B.set("complex", CX);

    // `__new__` of the immutable builtins, for subclasses (`super().__new__(cls, v)`).
    for (const base of [T.int, T.float, T.complex, T.str, T.tuple, T.frozenset, T.bytes]) {
        const f = builtin("__new__", -1, (a) => {
            const kw = a.length && a[a.length - 1] instanceof Map ? a.pop() : null;
            if (a.length === 0) fail(E.TypeError, base.name + ".__new__(): not enough arguments");
            return rt.builtinNew(a[0], base, a.slice(1), kw);
        });
        f.kwnames = true;
        if (base === T.str) {
            f.c2 = function (cls, v) {
                if (cls !== null && typeof cls === "object" && cls.isType === true && cls !== T.str && typeof v === "string" && rt.isSubclass(cls, T.str)) {
                    return { cls: cls, pyval: v, dict: rt.newAttrs() };
                }
                return rt.builtinNew(cls, T.str, [v], null);
            };
        }
        base.dict.set("__new__", { cls: T.staticmethod, func: f, builtinBase: base });
    }

    // ---- builtin functions ---------------------------------------------------------------------------------------------------
    let PY_WRITE_NATIVE = null;
    try { PY_WRITE_NATIVE = typeof __zipp_py_write === "function" ? __zipp_py_write : null; } catch (e) { PY_WRITE_NATIVE = null; }
    const emitOutLine = (toErr, line) => {
        if (PY_WRITE_NATIVE !== null) { PY_WRITE_NATIVE(1, toErr, line); return; }
        if (toErr) console.error(line); else console.log(line);
    };
    defkw("print", (a) => {
        const kw = kwOf(a, ["sep", "end", "file", "flush"]);
        const sep = kwget(kw, "sep", null), end = kwget(kw, "end", null), file = kwget(kw, "file", null), flush = kwget(kw, "flush", false);
        let target = file;
        if (target === null) {
            const sm = rt.modules.get("sys");
            if (sm !== undefined) {
                const so = sm.globals.get("stdout");
                if (so !== undefined) {
                    if (so === null) return null;
                    target = so;
                }
            }
        }
        const text = a.map(str).join(sep === null ? " " : needStr(sep, "sep")) + (end === null ? "\n" : needStr(end, "end"));
        rt.writeOut(text, target);
        if (truth(flush)) {
            if (target === null || target === undefined || ((target.isStdout === true || target.isStderr === true) && !(target.dict && target.dict.has("flush")))) {
                rt.flushStream(target !== null && target !== undefined && target.isStderr === true);
            } else {
                call(rt.getattr(target, "flush"), [], null);
            }
        }
        return null;
    });
    // Native CLI hosts install a raw output sink so stdout/stderr preserve exact
    // bytes with no appended newline and no 64 KiB inspect_line truncation.
    // Non-streaming hosts (WASM, embedder tests) remain line-buffered while still
    // bypassing inspect_line's 64 KiB truncation via PY_WRITE op 1.
    let outBuf = "", errBuf = "";
    rt.writeOut = function (text, file) {
        const hasCustomWrite = file !== null && file !== undefined && file.dict && file.dict.has("write");
        const toErr = !hasCustomWrite && file !== null && file !== undefined && file.isStderr === true;
        if (!toErr && file !== null && file !== undefined && (file.isStdout !== true || hasCustomWrite)) {
            call(rt.getattr(file, "write"), [text], null);
            return;
        }
        if (PY_WRITE_NATIVE !== null && PY_WRITE_NATIVE(0, toErr, text)) return;
        if (toErr) {
            errBuf += text;
            const i = errBuf.lastIndexOf("\n");
            if (i >= 0) { emitOutLine(true, errBuf.slice(0, i)); errBuf = errBuf.slice(i + 1); }
            return;
        }
        outBuf += text;
        const i = outBuf.lastIndexOf("\n");
        if (i >= 0) { emitOutLine(false, outBuf.slice(0, i)); outBuf = outBuf.slice(i + 1); }
    };
    rt.writeBytesOut = function (bytesObj, isErr) {
        const items = bytesObj.items;
        if (PY_WRITE_NATIVE !== null && PY_WRITE_NATIVE(2, isErr, items)) return;
        rt.writeOut(rt.decodeBytes(bytesObj, "utf-8", "replace"), isErr ? { isStderr: true } : null);
    };
    rt.flushStream = function (isErr) {
        if (PY_WRITE_NATIVE !== null) PY_WRITE_NATIVE(3, isErr);
    };
    rt.flushOut = function () {
        if (PY_WRITE_NATIVE !== null) { PY_WRITE_NATIVE(3, false); PY_WRITE_NATIVE(3, true); }
        if (outBuf.length) { emitOutLine(false, outBuf); outBuf = ""; }
        if (errBuf.length) { emitOutLine(true, errBuf); errBuf = ""; }
    };
    def("len", 1, (a) => len(a[0]));
    def("repr", 1, (a) => repr(a[0]));
    // One escape per code point: an astral character is \U0001xxxx, not a surrogate pair.
    rt.ascii = (v) => repr(v).replace(/[^\x00-\x7f]/gu, (c) => rt.hexEscape(c.codePointAt(0)));
    def("ascii", 1, (a) => rt.ascii(a[0]));
    def("abs", 1, (a) => { const v = a[0]; if (isInt(v)) { const i = asInt(v); return i < 0n ? -i : i; } if (typeof v === "number") return Math.abs(v); const r = callMethod(v, "__abs__", []); if (r !== undefined) return r; fail(E.TypeError, "bad operand type for abs(): '" + typeOf(v).name + "'"); });
    defkw("min", (a) => minmax(a, "lt", "min"));
    defkw("max", (a) => minmax(a, "gt", "max"));
    function minmax(a, op, name) {
        const kw = kwOf(a, ["key", "default"]);
        const key = kwget(kw, "key", null);
        let items;
        if (a.length === 1) { items = drain(a[0]); if (!items.length) { if (kw && kw.has("default")) return kw.get("default"); fail(E.ValueError, name + "() iterable argument is empty"); } }
        else { if (a.length === 0) fail(E.TypeError, name + " expected at least 1 argument, got 0"); items = a; }
        let best = items[0], bestKey = key === null ? best : rt.call1(key, best);
        for (let i = 1; i < items.length; i++) { const k = key === null ? items[i] : rt.call1(key, items[i]); if (cmp(op, k, bestKey)) { best = items[i]; bestKey = k; } }
        return best;
    }
    defkw("sum", (a) => {
        const kw = kwOf(a, ["start"]); let acc = a[1] !== undefined ? a[1] : kwget(kw, "start", 0n);
        if (typeof acc === "string") fail(E.TypeError, "sum() can't sum strings [use ''.join(seq) instead]");
        const it = iter(a[0]);
        // Floats add with Neumaier compensation, as CPython's sum() does.
        let fsum = 0, comp = 0, floating = false;
        for (;;) {
            const v = fornext(it); if (v === STOP) break;
            if (typeof v === "number" && (floating || isInt(acc) || typeof acc === "number")) {
                if (!floating) { fsum = toFloat(acc); comp = 0; floating = true; }
                const t = fsum + v;
                comp += Math.abs(fsum) >= Math.abs(v) ? (fsum - t) + v : (v - t) + fsum;
                fsum = t;
                continue;
            }
            if (floating) {
                if (isInt(v)) { const x = toFloat(v), t = fsum + x; comp += Math.abs(fsum) >= Math.abs(t - fsum) ? (fsum - t) + x : (x - t) + fsum; fsum = t; continue; }
                acc = fsum + comp; floating = false;
            }
            acc = R.add(acc, v);
        }
        return floating ? fsum + comp : acc;
    });
    defkw("sorted", (a) => { const kw = kwOf(a, ["key", "reverse"]); return list(rt.sortItems(drain(a[0]), kwget(kw, "key", null), truth(kwget(kw, "reverse", false)))); });
    def("any", 1, (a) => { const it = iter(a[0]); for (;;) { const v = fornext(it); if (v === STOP) return false; if (truth(v)) return true; } });
    B.get("any").c1 = function (x) {
        if (x !== null && typeof x === "object") {
            const c = x.cls;
            if (c === T.generator) {
                for (;;) { const v = x.next(); if (v === STOP) return false; if (truth(v)) return true; }
            }
            if (c === T.list || c === T.tuple) {
                const items = x.items;
                for (let i = 0; i < items.length; i++) if (truth(items[i])) return true;
                return false;
            }
        }
        const it = iter(x); for (;;) { const v = fornext(it); if (v === STOP) return false; if (truth(v)) return true; }
    };
    def("all", 1, (a) => { const it = iter(a[0]); for (;;) { const v = fornext(it); if (v === STOP) return true; if (!truth(v)) return false; } });
    B.get("all").c1 = function (x) {
        if (x !== null && typeof x === "object") {
            const c = x.cls;
            if (c === T.generator) {
                for (;;) { const v = x.next(); if (v === STOP) return true; if (!truth(v)) return false; }
            }
            if (c === T.list || c === T.tuple) {
                const items = x.items;
                for (let i = 0; i < items.length; i++) if (!truth(items[i])) return false;
                return true;
            }
        }
        const it = iter(x); for (;;) { const v = fornext(it); if (v === STOP) return true; if (!truth(v)) return false; }
    };
    def("iter", 2, (a) => { if (a.length === 2) { const f = a[0], sentinel = a[1]; return { cls: T.iterator, next: () => { const v = call(f, [], null); return eq(v, sentinel) ? STOP : v; } }; } return iter(a[0]); }, 1);
    B.get("iter").c1 = function (x) { return iter(x); };
    def("next", 2, (a) => { const it = a[0]; if (it === null || typeof it !== "object" || (it.next === undefined && typeMethod(it, "__next__") === undefined)) fail(E.TypeError, "'" + typeOf(it).name + "' object is not an iterator"); const v = it.next !== undefined ? fornext(it) : (() => { try { return callMethod(it, "__next__", []); } catch (e) { if (e && e.cls === E.StopIteration) return STOP; throw e; } })(); if (v === STOP) { if (a.length === 2) return a[1]; throw rt.makeExc(E.StopIteration, it.cls === T.generator && it.returned !== null ? [it.returned] : []); } return v; }, 1);
    const nextFn = B.get("next");
    const nextJ1 = function (it) {
        if (it !== null && typeof it === "object" && it.next !== undefined) {
            const v = it.wrapped === undefined ? it.next() : fornext(it);
            if (v !== STOP) return v;
            throw rt.makeExc(E.StopIteration, it.cls === T.generator && it.returned !== null ? [it.returned] : []);
        }
        return nextFn.code([it]);
    };
    const nextJ2 = function (it, dflt) {
        if (it !== null && typeof it === "object" && it.next !== undefined) {
            const v = it.wrapped === undefined ? it.next() : fornext(it);
            return v === STOP ? dflt : v;
        }
        return nextFn.code([it, dflt]);
    };
    if (BUILTIN_STRM !== null) {
        nextFn.strop = 53;
        nextFn.j1 = nextJ1;
        nextFn.j2 = nextJ2;
        nextFn.c1 = BUILTIN_STRM;
        nextFn.c2 = BUILTIN_STRM;
        nextFn.stopExc = (it) => { throw rt.makeExc(E.StopIteration, it.cls === T.generator && it.returned !== null ? [it.returned] : []); };
    } else {
        nextFn.c1 = nextJ1;
        nextFn.c2 = nextJ2;
    }
    def("aiter", 1, (a) => R.aiter(a[0]));
    def("anext", 2, (a) => {
        const aw = R.anext_awaitable(a[0]);
        if (a.length === 1) return aw;
        const dflt = a[1];
        let sub = null, done = false;
        const wrap = {
            cls: T.async_generator_asend,
            returned: null,
            _step: function (mode, v) {
                if (done) fail(E.RuntimeError, "cannot reuse already awaited anext");
                if (sub === null) sub = R.awaititer(aw);
                try {
                    const r = R.yfstep(sub, mode, v);
                    if (r === STOP) { done = true; this.returned = R.yfret; return STOP; }
                    return r;
                } catch (e) {
                    if (R.is_stop_async(e)) { done = true; this.returned = dflt; return STOP; }
                    done = true; throw e;
                }
            },
            next: function () { return this._step(0, null); },
            send: function (v) { const r = this._step(0, v); if (r === STOP) throw rt.makeExc(E.StopIteration, this.returned === null ? [] : [this.returned]); return r; },
            throwIn: function (e) { const r = this._step(1, e); if (r === STOP) throw rt.makeExc(E.StopIteration, this.returned === null ? [] : [this.returned]); return r; },
            close: function () { done = true; if (sub !== null) R.yfclose(sub); return null; },
        };
        return wrap;
    }, 1);
    // A tuple of types may nest further tuples (and unions), as in CPython.
    const instanceOfAny = (v, t) => { if (t !== null && typeof t === "object" && t.cls === T.tuple) return t.items.some((x) => instanceOfAny(v, x)); if (t !== null && typeof t === "object" && t.cls === rt.UnionType) return t.dict.get("__args__").items.some((x) => x === null ? v === null : rt.isinstanceCheck(v, x)); return rt.isinstanceCheck(v, t); };
    def("isinstance", 2, (a) => instanceOfAny(a[0], a[1]));
    rt.isinstanceCheck = function (v, t) {
        if (!isType(t)) { const m = typeMethod(t, "__instancecheck__"); if (m !== undefined) return truth(call(descrGet(m, t, typeOf(t)), [v], null)); fail(E.TypeError, "isinstance() arg 2 must be a type, a tuple of types, or a union"); }
        if (t.cls !== undefined && t.cls !== TypeType) {
            const ic = rt.lookupType(t.cls, "__instancecheck__");
            if (ic !== undefined && ic !== TypeType.dict.get("__instancecheck__")) return truth(call(descrGet(ic, t, t.cls), [v], null));
        }
        if (t === T.int && typeof v === "boolean") return true;
        return isInstance(v, t) && (t !== T.bytes || !isInstance(v, T.bytearray));
    };
    // bytearray reuses bytes' storage and methods by subclassing it here;
    // Python-visible checks keep them unrelated, as in CPython.
    const subclassCheck = (a, b) => {
        if (b.cls !== undefined && b.cls !== TypeType) {
            const sc = rt.lookupType(b.cls, "__subclasscheck__");
            if (sc !== undefined && sc !== TypeType.dict.get("__subclasscheck__")) return truth(call(descrGet(sc, b, b.cls), [a], null));
        }
        return isSubclass(a, b) && (b !== T.bytes || !isSubclass(a, T.bytearray));
    };
    const subclassOfAny = (c, t) => {
        if (t !== null && typeof t === "object" && t.cls === T.tuple) return t.items.some((x) => subclassOfAny(c, x));
        if (t !== null && typeof t === "object" && t.cls === rt.UnionType) return t.dict.get("__args__").items.some((x) => x === null ? c === T.NoneType : subclassOfAny(c, x));
        if (!isType(t)) fail(E.TypeError, "issubclass() arg 2 must be a class, a tuple of classes, or a union");
        return subclassCheck(c, t);
    };
    def("issubclass", 2, (a) => { if (!isType(a[0])) fail(E.TypeError, "issubclass() arg 1 must be a class"); return subclassOfAny(a[0], a[1]); });
    const attrName = (v) => { if (typeof v === "string") return v; fail(E.TypeError, "attribute name must be string, not '" + typeOf(v).name + "'"); };
    // A plain miss comes back as MISSING_ATTR (no exception is built); an
    // AttributeError from a property or __getattr__ is still caught.
    const MISSING_ATTR = rt.MISSING_ATTR;
    def("hasattr", 2, (a) => { const name = attrName(a[1]); try { return rt.getattr(a[0], name, MISSING_ATTR) !== MISSING_ATTR; } catch (e) { if (e && e.cls && isSubclass(e.cls, E.AttributeError)) return false; throw e; } });
    B.get("hasattr").c2 = function (obj, name) {
        if (typeof name !== "string") name = attrName(name);
        if (obj !== null && typeof obj === "object" && !obj.isType && name !== "__class__") {
            const t = obj.cls, dict = obj.dict;
            if (t !== undefined && t.userClass === true && dict !== undefined && dict !== null) {
                if (t.gx[name] === true) return dict.get(name) !== undefined;
                if (t.gm[name] !== undefined || t.gv[name] !== undefined) return true;
            }
        }
        try { return rt.getattr(obj, name, MISSING_ATTR) !== MISSING_ATTR; }
        catch (e) { if (e && e.cls && isSubclass(e.cls, E.AttributeError)) return false; throw e; }
    };
    def("getattr", 3, (a) => {
        const name = attrName(a[1]);
        if (a.length < 3) return rt.getattr(a[0], name);
        try { const v = rt.getattr(a[0], name, MISSING_ATTR); return v === MISSING_ATTR ? a[2] : v; }
        catch (e) { if (e && e.cls && isSubclass(e.cls, E.AttributeError)) return a[2]; throw e; }
    }, 2);
    const getattrFn = B.get("getattr");
    getattrFn.c2 = function (obj, name) {
        if (typeof name !== "string") name = attrName(name);
        if (obj !== null && typeof obj === "object" && !obj.isType && name !== "__class__") {
            const t = obj.cls, dict = obj.dict;
            if (t !== undefined && t.userClass === true && dict !== undefined && dict !== null) {
                const fn = t.gm[name];
                if (fn !== undefined) {
                    const v = dict.get(name);
                    return v !== undefined ? v : rt.bound(fn, obj);
                }
                if (t.ga[name] === true) {
                    const v = dict.get(name);
                    if (v !== undefined) return v;
                }
            }
        }
        return rt.getattr(obj, name);
    };
    getattrFn.c3 = function (obj, name, dflt) {
        if (typeof name !== "string") name = attrName(name);
        if (obj !== null && typeof obj === "object" && !obj.isType && name !== "__class__") {
            const t = obj.cls, dict = obj.dict;
            if (t !== undefined && t.userClass === true && dict !== undefined && dict !== null) {
                if (t.gx[name] === true) {
                    const v = dict.get(name);
                    return v !== undefined ? v : dflt;
                }
                const fn = t.gm[name];
                if (fn !== undefined) {
                    const v = dict.get(name);
                    return v !== undefined ? v : rt.bound(fn, obj);
                }
                if (t.ga[name] === true) {
                    const v = dict.get(name);
                    if (v !== undefined) return v;
                }
            }
        }
        try { const v = rt.getattr(obj, name, MISSING_ATTR); return v === MISSING_ATTR ? dflt : v; }
        catch (e) { if (e && e.cls && isSubclass(e.cls, E.AttributeError)) return dflt; throw e; }
    };
    def("setattr", 3, (a) => rt.setattr(a[0], attrName(a[1]), a[2]));
    B.get("setattr").c3 = function (obj, name, val) { return rt.setattr(obj, typeof name === "string" ? name : attrName(name), val); };
    def("delattr", 2, (a) => R.delattr(a[0], attrName(a[1])));
    B.get("delattr").c2 = function (obj, name) { return R.delattr(obj, typeof name === "string" ? name : attrName(name)); };
    def("id", 1, (a) => BigInt(rt.ident(a[0]) || (typeof a[0] === "string" ? rt.hashInt(a[0]) : 0n)));
    def("hash", 1, (a) => rt.hashInt(a[0]));
    // A str's hash natively (`vm::py_str`, op 10).
    if (rt.nativeEntry !== null) rt.nativeEntry(B.get("hash"), 1, 10);
    const isCallable = (v) => v !== null && typeof v === "object" && (v.cls === T.function || v.cls === T.builtin_function_or_method || v.cls === T.method || v.isType === true || typeMethod(v, "__call__") !== undefined);
    rt.isCallable = isCallable;
    def("callable", 1, (a) => isCallable(a[0]));
    def("chr", 1, (a) => { const n = Number(needInt(a[0])); if (n < 0 || n > 0x10FFFF) fail(E.ValueError, "chr() arg not in range(0x110000)"); return String.fromCodePoint(n); });
    def("ord", 1, (a) => { const s = needStr(a[0], "ord() expected string of length 1, but"); const cps = codepoints(s); if (cps.length !== 1) fail(E.TypeError, "ord() expected a character, but string of length " + cps.length + " found"); return BigInt(cps[0].codePointAt(0)); });
    if (rt.nativeEntry !== null) {
        rt.nativeEntry(B.get("ord"), 1, 34);
        rt.nativeEntry(B.get("chr"), 1, 35);
    }
    def("round", 2, (a) => roundValue(a[0], a[1]), 1);
    def("divmod", 2, (a) => {
        const x = a[0], y = a[1];
        if (isNum(x) && isNum(y)) {
            if (typeof x === "number" || typeof y === "number") return tuple(rt.floatDivmod(toFloat(x), toFloat(y)));
            return tuple([R.binop("floordiv", x, y), R.binop("mod", x, y)]);
        }
        const m = typeMethod(x, "__divmod__");
        if (m !== undefined && !m.isBase) { const r = call(descrGet(m, x, typeOf(x)), [y], null); if (r !== NOTIMPL) return r; }
        const rm = typeMethod(y, "__rdivmod__");
        if (rm !== undefined && !rm.isBase) { const r = call(descrGet(rm, y, typeOf(y)), [x], null); if (r !== NOTIMPL) return r; }
        // complex has neither // nor %, so no divmod.
        if (isCx(x) || isCx(y)) fail(E.TypeError, "unsupported operand type(s) for divmod(): '" + typeOf(x).name + "' and '" + typeOf(y).name + "'");
        return tuple([R.binop("floordiv", x, y), R.binop("mod", x, y)]);
    });
    def("pow", 3, (a) => {
        if (a.length === 3 && a[2] !== null) {
            if (!isInt(a[0]) || !isInt(a[1]) || !isInt(a[2])) {
                // pow(x, y, z) on an instance calls its __pow__ with the modulus.
                const m = a[0] !== null && typeof a[0] === "object" ? typeMethod(a[0], "__pow__") : undefined;
                if (m !== undefined && !m.isBase) { const r = call(descrGet(m, a[0], typeOf(a[0])), [a[1], a[2]], null); if (r !== NOTIMPL) return r; }
                // complex_pow refuses a modulus once both operands convert.
                if ((isCx(a[0]) || isCx(a[1]) || isCx(a[2])) && rt.cxParts(a[0]) !== null && rt.cxParts(a[1]) !== null) fail(E.ValueError, "complex modulo");
                if (isNum(a[0]) && isNum(a[1]) && isNum(a[2])) fail(E.TypeError, "pow() 3rd argument not allowed unless all arguments are integers");
                fail(E.TypeError, "unsupported operand type(s) for ** or pow(): '" + typeOf(a[0]).name + "', '" + typeOf(a[1]).name + "', '" + typeOf(a[2]).name + "'");
            }
            let b = asInt(a[0]), e = asInt(a[1]); const m = asInt(a[2]);
            if (m === 0n) fail(E.ValueError, "pow() 3rd argument cannot be 0");
            const mm = m < 0n ? -m : m;
            b = ((b % mm) + mm) % mm;
            if (e < 0n) {
                // A negative exponent uses the modular inverse (extended Euclid).
                let r0 = mm, r1 = b, t0 = 0n, t1 = 1n;
                while (r1 !== 0n) { const q = r0 / r1; [r0, r1] = [r1, r0 - q * r1]; [t0, t1] = [t1, t0 - q * t1]; }
                if (r0 !== 1n) fail(E.ValueError, "base is not invertible for the given modulus");
                b = ((t0 % mm) + mm) % mm; e = -e;
            }
            let r = 1n % mm;
            while (e > 0n) { if (e & 1n) r = (r * b) % mm; e >>= 1n; if (e > 0n) b = (b * b) % mm; }
            return m < 0n && r !== 0n ? r - mm : r;
        }
        return R.binop("pow", a[0], a[1]);
    }, 2);
    def("bin", 1, (a) => { const v = rt.indexOf(a[0]); return (v < 0n ? "-0b" : "0b") + (v < 0n ? -v : v).toString(2); });
    def("oct", 1, (a) => { const v = rt.indexOf(a[0]); return (v < 0n ? "-0o" : "0o") + (v < 0n ? -v : v).toString(8); });
    def("hex", 1, (a) => { const v = rt.indexOf(a[0]); return (v < 0n ? "-0x" : "0x") + (v < 0n ? -v : v).toString(16); });
    def("format", 2, (a) => rt.formatValue(a[0], a[1] === undefined ? "" : needStr(a[1], "format() argument 2")), 1);
    def("input", 1, (a) => { if (a.length) rt.writeOut(str(a[0]), null); fail(E.EOFError, "input() is not available: no interactive console in this environment"); }, 0);
    // ---- open(): files of the program's virtual filesystem -----------------------------------------
    // Text and binary modes, read/write/append/exclusive, with the usual
    // read/readline/readlines/write/iteration and context-manager protocol.
    // Writes stay in the sandbox; the host collects them afterwards.
    const FileType = rt.newType("TextIOWrapper", [rt.ObjectType], new Map(), "io");
    const BinType = rt.newType("BufferedReader", [rt.ObjectType], new Map(), "io");
    const fstate = (self) => { const s = self.dict.get("_f"); if (s.closed) fail(E.ValueError, "I/O operation on closed file."); return s; };
    const flush = (s) => { if (s.dirty) { rt.vfs.set(s.path, new Uint8Array(s.buf)); s.dirty = false; } };
    // Writable handles still open. CPython flushes a file when its last
    // reference goes and at interpreter exit; here every open handle is
    // flushed before its path is read and before the host collects the
    // program's changes (`rt.flushOpenFiles()` from the entry and
    // `vfsChangedText`).
    const openStates = new Set();
    rt.flushOpenFiles = function (path) {
        for (const s of openStates) if (s.dirty && (path === undefined || s.path === path)) flush(s);
    };
    {
        const vfsGet = rt.vfs.get;
        rt.vfs.get = (p) => { if (openStates.size !== 0) rt.flushOpenFiles(rt.vfs.norm(p)); return vfsGet(p); };
    }
    // The hosts refuse to load a VFS file over 8 MiB; refuse to write one.
    const MAX_FILE = 8 * 1024 * 1024;
    const fileTooLarge = () => fail(E.OSError, "[Errno 27] File too large");
    function openFile(args) {
        const kw = kwOf(args, ["mode", "buffering", "encoding", "errors", "newline"]);
        let path = args[0];
        if (path !== null && typeof path === "object" && path.cls !== T.bytes && rt.typeMethod(path, "__fspath__") !== undefined) path = rt.callMethod(path, "__fspath__", []);
        if (typeof path !== "string") fail(E.TypeError, "expected str, bytes or os.PathLike object, not " + typeOf(path).name);
        const mode = args[1] === undefined ? needStr(kwget(kw, "mode", "r")) : needStr(args[1]);
        const binary = mode.includes("b"), textFlag = mode.includes("t"), plus = mode.includes("+");
        if (binary && textFlag) fail(E.ValueError, "can't have text and binary mode at once");
        const kind = mode.replace(/[bt+]/g, "");
        if (!["r", "w", "a", "x"].includes(kind)) fail(E.ValueError, "invalid mode: '" + mode + "'");
        const encArg = args[3] !== undefined ? args[3] : kwget(kw, "encoding", null);
        const errArg = args[4] !== undefined ? args[4] : kwget(kw, "errors", null);
        const nlArg = args[5] !== undefined ? args[5] : kwget(kw, "newline", null);
        if (binary) {
            if (encArg !== null) fail(E.ValueError, "binary mode doesn't take an encoding argument");
            if (errArg !== null) fail(E.ValueError, "binary mode doesn't take an errors argument");
            if (nlArg !== null) fail(E.ValueError, "binary mode doesn't take a newline argument");
        } else if (nlArg !== null && nlArg !== "" && nlArg !== "\n" && nlArg !== "\r" && nlArg !== "\r\n") {
            fail(E.ValueError, "illegal newline value: " + repr(nlArg));
        }
        const norm = rt.vfs.norm(path);
        if (!rt.vfs.has(norm) && rt.vfs.isDir(norm)) fail(E.IsADirectoryError, "[Errno 21] Is a directory: '" + path + "'");
        let existing = rt.vfs.get(norm);
        if (kind === "r" && existing === undefined) fail(E.FileNotFoundError, "[Errno 2] No such file or directory: '" + path + "'");
        if (kind === "x" && existing !== undefined) fail(E.FileExistsError, "[Errno 17] File exists: '" + path + "'");
        const writable = kind !== "r" || plus;
        const buf = !writable ? null : (kind === "w" || kind === "x" || existing === undefined ? [] : rt.bytesFromU8(existing));
        const u8 = !writable ? existing : null;
        const s = {
            path: norm, buf: buf, u8: u8, pos: kind === "a" ? buf.length : 0,
            binary: binary, readable: kind === "r" || plus, writable: writable,
            closed: false, dirty: kind === "w" || kind === "x" || kind === "a" && existing === undefined,
            mode: mode, encoding: encArg === null ? "utf-8" : needStr(encArg),
            errors: errArg === null ? "strict" : needStr(errArg),
            newline: binary ? null : nlArg, rawText: null, rawBasePos: 0, tpos: 0, seenNl: 0,
        };
        if (s.dirty) flush(s);
        if (s.writable) openStates.add(s);
        return { cls: binary ? BinType : FileType, dict: new Map([["_f", s], ["name", path], ["mode", mode]]) };
    }
    const ensureBuf = (s) => { if (s.buf === null) s.buf = s.u8 === null ? [] : rt.bytesFromU8(s.u8); return s.buf; };
    const byteLen = (s) => s.buf !== null ? s.buf.length : (s.u8 !== null ? s.u8.length : 0);
    const bytesOfText = (s, text) => rt.encodeStr(text, s.encoding, s.errors).items;
    const readAllBin = (s) => {
        if (s.buf === null) { const out = rt.bytesFromU8(s.pos === 0 ? s.u8 : s.u8.subarray(s.pos)); s.pos = s.u8.length; return out; }
        const out = s.buf.slice(s.pos); s.pos = s.buf.length; return out;
    };
    const readNBin = (s, n) => {
        if (n === 0) return [];
        if (s.buf === null) { const end = Math.min(s.u8.length, s.pos + n); const out = rt.bytesFromU8(s.u8.subarray(s.pos, end)); s.pos = end; return out; }
        const out = s.buf.slice(s.pos, s.pos + n); s.pos += out.length; return out;
    };
    const readLineBin = (s, limit) => {
        if (limit === 0) return [];
        const src = s.buf !== null ? s.buf : s.u8;
        const maxEnd = limit < 0 ? src.length : Math.min(src.length, s.pos + limit);
        let end = src.indexOf(10, s.pos);
        end = end < 0 || end >= maxEnd ? maxEnd : end + 1;
        const out = s.buf !== null ? s.buf.slice(s.pos, end) : rt.bytesFromU8(s.u8.subarray(s.pos, end));
        s.pos = end; return out;
    };
    const noteNl = (s, nl) => { s.seenNl |= nl === "\n" ? 1 : nl === "\r" ? 2 : 4; };
    const ensureText = (s) => {
        if (s.rawText === null) {
            const src = s.buf !== null
                ? (s.pos === 0 ? s.buf : s.buf.slice(s.pos))
                : (s.pos === 0 ? s.u8 : s.u8.subarray(s.pos));
            s.rawText = rt.decodeBytes({ items: src }, s.encoding, s.errors);
            s.rawBasePos = s.pos;
            s.tpos = 0;
        }
        return s.rawText;
    };
    const syncTextBytePos = (s) => {
        if (s.rawText !== null && s.tpos > 0) {
            s.pos = s.tpos >= s.rawText.length ? byteLen(s) : s.rawBasePos + bytesOfText(s, s.rawText.slice(0, s.tpos)).length;
        }
    };
    const advanceCp = (str, start, count) => {
        let p = start, len = str.length, k = 0;
        while (k < count && p < len) {
            const c = str.charCodeAt(p);
            p += (c >= 0xd800 && c < 0xdc00 && p + 1 < len && (str.charCodeAt(p + 1) & 0xfc00) === 0xdc00) ? 2 : 1;
            k++;
        }
        return p;
    };
    const readTextAll = (s) => {
        const raw = ensureText(s);
        const tail = s.tpos === 0 ? raw : raw.slice(s.tpos);
        s.tpos = raw.length;
        s.pos = byteLen(s);
        if (s.newline === null) {
            if (tail.indexOf("\r") !== -1) {
                if (tail.indexOf("\r\n") !== -1) noteNl(s, "\r\n");
                if (/\r(?!\n)/.test(tail)) noteNl(s, "\r");
                if (/(^|[^\r])\n/.test(tail)) noteNl(s, "\n");
                return tail.replace(/\r\n|\r/g, "\n");
            }
            if (tail.indexOf("\n") !== -1) noteNl(s, "\n");
        }
        return tail;
    };
    const readTextN = (s, n) => {
        if (n === 0) return "";
        const raw = ensureText(s);
        let p = s.tpos, len = raw.length;
        if (p >= len) return "";
        if (s.newline === null) {
            if (raw.indexOf("\r", p) === -1 && !rt.hasSurrogate(raw)) {
                const end = Math.min(len, p + n);
                const out = raw.slice(p, end);
                if (out.indexOf("\n") !== -1) noteNl(s, "\n");
                s.tpos = end;
                return out;
            }
            let out = "", k = 0;
            while (k < n && p < len) {
                const c = raw.charCodeAt(p);
                if (c === 13) {
                    if (p + 1 < len && raw.charCodeAt(p + 1) === 10) { noteNl(s, "\r\n"); p += 2; }
                    else { noteNl(s, "\r"); p += 1; }
                    out += "\n"; k++;
                } else if (c >= 0xd800 && c < 0xdc00 && p + 1 < len && (raw.charCodeAt(p + 1) & 0xfc00) === 0xdc00) {
                    out += raw.slice(p, p + 2); p += 2; k++;
                } else {
                    if (c === 10) noteNl(s, "\n");
                    out += raw[p++]; k++;
                }
            }
            s.tpos = p;
            return out;
        }
        const end = rt.hasSurrogate(raw) ? advanceCp(raw, p, n) : Math.min(len, p + n);
        s.tpos = end;
        return raw.slice(p, end);
    };
    const readTextLine = (s, limit) => {
        if (limit === 0) return "";
        const raw = ensureText(s);
        const p = s.tpos, len = raw.length;
        if (p >= len) return "";
        if (s.newline === null) {
            if (limit < 0) {
                const cr = raw.indexOf("\r", p), lf = raw.indexOf("\n", p);
                if (cr < 0 && lf < 0) { s.tpos = len; return raw.slice(p); }
                if (cr < 0 || (lf >= 0 && lf < cr)) { noteNl(s, "\n"); s.tpos = lf + 1; return raw.slice(p, lf + 1); }
                if (cr + 1 < len && raw.charCodeAt(cr + 1) === 10) { noteNl(s, "\r\n"); s.tpos = cr + 2; }
                else { noteNl(s, "\r"); s.tpos = cr + 1; }
                return raw.slice(p, cr) + "\n";
            }
            let cur = p, out = "", k = 0;
            while (k < limit && cur < len) {
                const c = raw.charCodeAt(cur);
                if (c === 13) {
                    if (cur + 1 < len && raw.charCodeAt(cur + 1) === 10) { noteNl(s, "\r\n"); cur += 2; }
                    else { noteNl(s, "\r"); cur += 1; }
                    out += "\n"; break;
                }
                if (c === 10) { noteNl(s, "\n"); out += "\n"; cur++; break; }
                if (c >= 0xd800 && c < 0xdc00 && cur + 1 < len && (raw.charCodeAt(cur + 1) & 0xfc00) === 0xdc00) {
                    out += raw.slice(cur, cur + 2); cur += 2; k++;
                } else { out += raw[cur++]; k++; }
            }
            s.tpos = cur;
            return out;
        }
        let end;
        if (s.newline === "") {
            const cr = raw.indexOf("\r", p), lf = raw.indexOf("\n", p);
            if (cr >= 0 && (lf < 0 || cr < lf)) {
                if (cr + 1 < len && raw.charCodeAt(cr + 1) === 10) { noteNl(s, "\r\n"); end = cr + 2; }
                else { noteNl(s, "\r"); end = cr + 1; }
            } else if (lf >= 0) { noteNl(s, "\n"); end = lf + 1; }
            else end = len;
        } else {
            const idx = raw.indexOf(s.newline, p);
            end = idx < 0 ? len : idx + s.newline.length;
        }
        if (limit >= 0) {
            const capped = rt.hasSurrogate(raw) ? advanceCp(raw, p, limit) : Math.min(len, p + limit);
            if (capped < end) end = capped;
        }
        s.tpos = end;
        return raw.slice(p, end);
    };
    for (const [type, binary] of [[FileType, false], [BinType, true]]) {
        type.dict.set("read", builtin("read", 2, (a) => {
            const s = fstate(a[0]); if (!s.readable) fail(E.OSError, "not readable");
            const n = a[1] === undefined || a[1] === null ? -1 : Number(needInt(a[1]));
            if (binary) return rt.bytes(n < 0 ? readAllBin(s) : readNBin(s, n));
            return n < 0 ? readTextAll(s) : readTextN(s, n);
        }, 1));
        type.dict.set("readline", builtin("readline", 2, (a) => {
            const s = fstate(a[0]); if (!s.readable) fail(E.OSError, "not readable");
            const limit = a[1] === undefined || a[1] === null ? -1 : Number(needInt(a[1]));
            return binary ? rt.bytes(readLineBin(s, limit)) : readTextLine(s, limit);
        }, 1));
        type.dict.set("readlines", builtin("readlines", 2, (a) => {
            const s = fstate(a[0]); if (!s.readable) fail(E.OSError, "not readable");
            const out = [];
            if (binary) { while (s.pos < byteLen(s)) out.push(rt.bytes(readLineBin(s, -1))); }
            else { const raw = ensureText(s); while (s.tpos < raw.length) out.push(readTextLine(s, -1)); }
            return list(out);
        }, 1));
        type.dict.set("__iter__", builtin("__iter__", 1, (a) => {
            const s = fstate(a[0]);
            if (binary) return { cls: T.iterator, next: () => s.pos < byteLen(s) ? rt.bytes(readLineBin(s, -1)) : STOP };
            return { cls: T.iterator, next: () => { const raw = ensureText(s); return s.tpos < raw.length ? readTextLine(s, -1) : STOP; } };
        }));
        type.dict.set("write", builtin("write", 2, (a) => {
            const s = fstate(a[0]); if (!s.writable) fail(E.OSError, "not writable");
            let bytes, writtenLen;
            if (binary) {
                if (a[1] === null || typeof a[1] !== "object" || a[1].cls !== T.bytes) fail(E.TypeError, "a bytes-like object is required, not '" + typeOf(a[1]).name + "'");
                bytes = a[1].items; writtenLen = bytes.length;
            } else {
                let text = needStr(a[1]);
                writtenLen = rt.strLen(text);
                syncTextBytePos(s);
                if (s.newline === "\r" && text.indexOf("\n") !== -1) text = text.replace(/\n/g, "\r");
                else if (s.newline === "\r\n" && text.indexOf("\n") !== -1) text = text.replace(/\n/g, "\r\n");
                bytes = bytesOfText(s, text);
            }
            const buf = ensureBuf(s);
            if (s.pos + bytes.length > MAX_FILE) fileTooLarge();
            // Past the end a gap (after a seek) fills with zeros, then appends;
            // a large append at the end is one native concat.
            while (buf.length < s.pos) buf.push(0);
            if (s.pos === buf.length && bytes.length >= 4096 && bytes.length * 4 >= buf.length) s.buf = buf.concat(bytes);
            else for (let i = 0; i < bytes.length; i++) { if (s.pos + i < buf.length) buf[s.pos + i] = bytes[i]; else buf.push(bytes[i]); }
            s.pos += bytes.length; s.dirty = true; s.rawText = null; s.tpos = 0;
            return BigInt(writtenLen);
        }));
        type.dict.set("writelines", builtin("writelines", 2, (a) => { for (const line of drain(a[1])) rt.callMethod(a[0], "write", [line]); return null; }));
        type.dict.set("flush", builtin("flush", 1, (a) => { flush(fstate(a[0])); return null; }));
        type.dict.set("close", builtin("close", 1, (a) => { const s = a[0].dict.get("_f"); if (!s.closed) { flush(s); s.closed = true; openStates.delete(s); } return null; }));
        type.dict.set("seek", builtin("seek", 3, (a) => {
            const s = fstate(a[0]); const off = Number(needInt(a[1])); const whence = a[2] === undefined ? 0 : Number(needInt(a[2]));
            if (!binary) syncTextBytePos(s);
            s.pos = Math.max(0, whence === 0 ? off : whence === 1 ? s.pos + off : byteLen(s) + off);
            if (!binary) {
                if (s.rawText !== null && s.pos === s.rawBasePos) s.tpos = 0;
                else { s.rawText = null; s.tpos = 0; }
            }
            return BigInt(s.pos);
        }, 2));
        type.dict.set("tell", builtin("tell", 1, (a) => {
            const s = fstate(a[0]);
            if (!binary) syncTextBytePos(s);
            return BigInt(s.pos);
        }));
        type.dict.set("truncate", builtin("truncate", 2, (a) => { const s = fstate(a[0]); if (!binary) syncTextBytePos(s); const buf = ensureBuf(s); const n = a[1] === undefined || a[1] === null ? s.pos : Number(needInt(a[1])); if (n < 0) fail(E.OSError, "[Errno 22] Invalid argument"); if (n > MAX_FILE) fileTooLarge(); if (n < buf.length) buf.length = n; else while (buf.length < n) buf.push(0); s.dirty = true; s.rawText = null; s.tpos = 0; return BigInt(n); }, 1));
        type.dict.set("readable", builtin("readable", 1, (a) => fstate(a[0]).readable));
        type.dict.set("writable", builtin("writable", 1, (a) => fstate(a[0]).writable));
        type.dict.set("__enter__", builtin("__enter__", 1, (a) => a[0]));
        type.dict.set("__exit__", builtin("__exit__", 4, (a) => { rt.callMethod(a[0], "close", []); return false; }));
        type.dict.set("closed", { cls: T.property, fget: builtin("closed", 1, (a) => a[0].dict.get("_f").closed), fset: null, fdel: null, doc: null });
        if (!binary) {
            type.dict.set("encoding", { cls: T.property, fget: builtin("encoding", 1, (a) => a[0].dict.get("_f").encoding), fset: null, fdel: null, doc: null });
            type.dict.set("errors", { cls: T.property, fget: builtin("errors", 1, (a) => a[0].dict.get("_f").errors), fset: null, fdel: null, doc: null });
            type.dict.set("newlines", { cls: T.property, fget: builtin("newlines", 1, (a) => {
                const m = a[0].dict.get("_f").seenNl;
                if (m === 0) return null;
                if (m === 1) return "\n";
                if (m === 2) return "\r";
                if (m === 4) return "\r\n";
                const arr = [];
                if (m & 1) arr.push("\n");
                if (m & 2) arr.push("\r");
                if (m & 4) arr.push("\r\n");
                return tuple(arr);
            }), fset: null, fdel: null, doc: null });
        }
        type.dict.set("__repr__", builtin("__repr__", 1, (a) => "<" + (binary ? "_io.BufferedReader" : "_io.TextIOWrapper") + " name=" + repr(a[0].dict.get("name")) + " mode=" + repr(a[0].dict.get("mode")) + ">"));
    }
    rt.openFile = openFile;
    defkw("open", (a) => openFile(a));
    def("exit", 1, (a) => { throw rt.makeExc(E.SystemExit, a.length ? [a[0]] : []); }, 0);
    B.set("quit", B.get("exit"));
    def("vars", 1, (a) => { if (a.length === 0) fail(E.TypeError, "vars() without arguments is not supported"); const v = a[0]; if (v !== null && typeof v === "object" && (v.dict || v.cls === T.function) && !(v.cls !== undefined && v.cls.noDict === true)) { if (v.dict === null) v.dict = new Map(); return rt.instanceDict(v); } if (v !== null && typeof v === "object" && v.cls === T.module) return rt.dictFromMap(v.globals); fail(E.TypeError, "vars() argument must have __dict__ attribute"); }, 0);
    {
        // Writes through obj.__dict__ land on the instance itself.
        const ID = rt.newType("dict", [], new Map(), "builtins");
        const m = (name, arity, code, min) => ID.dict.set(name, builtin(name, arity, code, min));
        const target = (a) => a[0].target.dict;
        // Slot values share the instance Map; the view of a slotted class hides them.
        const shown = (a) => { const v = a[0], m = v.target.dict; if (v.hide === undefined) return m; const out = new Map(); for (const [k, x] of m) if (!v.hide.has(k)) out.set(k, x); return out; };
        const keyStr = (k) => { if (typeof k !== "string") throw rt.makeExc(E.KeyError, [k]); return k; };
        m("__getitem__", 2, (a) => { const v = shown(a).get(keyStr(a[1])); if (v === undefined) throw rt.makeExc(E.KeyError, [a[1]]); return v; });
        m("__setitem__", 3, (a) => { if (typeof a[1] !== "string") fail(E.TypeError, "attribute name must be string"); target(a).set(a[1], a[2]); return null; });
        m("__delitem__", 2, (a) => { if (!target(a).delete(keyStr(a[1]))) throw rt.makeExc(E.KeyError, [a[1]]); return null; });
        m("__contains__", 2, (a) => typeof a[1] === "string" && shown(a).has(a[1]));
        m("__len__", 1, (a) => BigInt(shown(a).size));
        m("__iter__", 1, (a) => iter(list(Array.from(shown(a).keys()))));
        m("keys", 1, (a) => list(Array.from(shown(a).keys())));
        m("values", 1, (a) => list(Array.from(shown(a).values())));
        m("items", 1, (a) => list(Array.from(shown(a), ([k, v]) => tuple([k, v]))));
        m("get", 3, (a) => { const v = typeof a[1] === "string" ? shown(a).get(a[1]) : undefined; return v === undefined ? (a[2] === undefined ? null : a[2]) : v; }, 2);
        m("pop", 3, (a) => { const v = typeof a[1] === "string" ? target(a).get(a[1]) : undefined; if (v === undefined) { if (a[2] === undefined) throw rt.makeExc(E.KeyError, [a[1]]); return a[2]; } target(a).delete(a[1]); return v; }, 2);
        m("setdefault", 3, (a) => { const v = target(a).get(keyStr(a[1])); if (v !== undefined) return v; const d = a[2] === undefined ? null : a[2]; target(a).set(a[1], d); return d; }, 2);
        m("update", 2, (a) => { for (const [k, v] of rt.dictEntries(rt.asDict(a[1]))) target(a).set(needStr(k), v); return null; });
        m("clear", 1, (a) => { target(a).clear(); return null; });
        m("copy", 1, (a) => rt.dictFromMap(shown(a)));
        m("__repr__", 1, (a) => repr(rt.dictFromMap(shown(a))));
        m("__eq__", 2, (a) => rt.eq(rt.dictFromMap(shown(a)), a[1]));
        m("__or__", 2, (a) => R.binop("or", rt.dictFromMap(shown(a)), a[1]));
        // Without slots to hide, `obj.__dict__` is a real dict whose storage
        // is the instance's own attribute storage (`vm::py_table::layout`; a
        // Map in an object made otherwise, which refuses a non-str key), so
        // isinstance, json, pickle, copy and every dict method see it as
        // CPython's do, and writes land on the object.
        const refuseKey = () => fail(E.TypeError, "attribute name must be string");
        const liveDict = (target) => ({
            cls: T.dict, liveTarget: target,
            get map() { return target.dict; }, set map(v) { refuseKey(); },
            get size() { return target.dict.size; }, set size(v) { },
        });
        rt.instanceDict = (obj) => {
            if (obj.dict === null && obj.cls === T.function) obj.dict = new Map();
            const hide = obj.cls !== undefined && obj.cls !== null ? obj.cls.slots : undefined;
            if (hide !== undefined && hide !== null && hide.size !== 0) return { cls: ID, target: obj, hide: hide };
            // One view per object, as `obj.__dict__ is obj.__dict__` holds in CPython.
            let view = obj.dictView;
            if (view === undefined || view.liveTarget !== obj) { view = liveDict(obj); obj.dictView = view; }
            return view;
        };
        // `obj.__dict__ = d`: the dict's own table becomes the object's
        // attribute storage (so `self.__dict__ = self` in a dict subclass
        // makes keys and attributes one namespace, as in CPython); its size
        // then follows the shared table.
        rt.setInstanceDict = (obj, d) => {
            if (d === null || typeof d !== "object" || d.map === undefined || !isInstance(d, T.dict)) fail(E.TypeError, "__dict__ must be set to a dictionary, not a '" + typeOf(d).name + "'");
            if (d.liveTarget !== undefined) { obj.dict = d.liveTarget.dict; return; }
            const shared = d.map;
            Object.defineProperty(d, "map", { get: () => shared, set: (v) => { refuseKey(); }, configurable: true });
            Object.defineProperty(d, "size", { get: () => shared.size, set: (v) => { }, configurable: true });
            obj.dict = shared;
            if (d.cls === T.dict) obj.dictView = Object.defineProperty(d, "liveTarget", { value: obj, configurable: true, writable: true });
        };
        // globals() and module.__dict__: the same live dict over a module's namespace Map.
        rt.namespaceView = (map) => liveDict({ dict: map });
        rt.T.instancedict = ID;
    }
    for (const name of ["isinstance", "issubclass", "hasattr", "getattr", "setattr", "delattr", "divmod", "iter", "next", "format"]) B.get(name).unpackArgs = true;
    def("dir", 1, (a) => list(a.length ? dirOf(a[0]) : []), 0);
    def("globals", 0, () => { const m = rt.entryModule(); return m ? rt.namespaceView(m.globals) : dict(); });
    def("locals", 0, () => dict());
    def("type", 3, (a) => a.length === 1 ? typeOf(a[0]) : rt.buildclass(null, rt.mapFromDict(rt.asDict(a[2])), needStr(a[0]), drain(a[1]), null), 1);
    B.set("type", TypeType);
    def("__import__", -1, (a) => R.import(needStr(a[0]), null));
    def("__build_class__", -1, () => fail(E.RuntimeError, "__build_class__ is internal"));
    def("breakpoint", -1, () => null);
    def("help", -1, () => { rt.writeOut("help() is not available in this environment\n", null); return null; });
    def("memoryview", 1, (a) => a[0]);
    // bytearray: a mutable bytes (a subtype here, so every bytes method and
    // structural operation applies; CPython keeps them distinct).
    {
        const BA = rt.newType("bytearray", [T.bytes], new Map(), "builtins");
        T.bytearray = BA;
        rt.constructors.set(BA, (args, kw, cls) => { const b = rt.constructors.get(T.bytes)(args, kw, T.bytes); return { cls: cls, items: b.items.slice(), dict: new Map() }; });
        const item = (v) => { const n = Number(needInt(v)); if (n < 0 || n > 255) fail(E.ValueError, "byte must be in range(0, 256)"); return n; };
        const items = (v) => { if (v !== null && typeof v === "object" && v.items !== undefined && isInstance(v, T.bytes)) return v.items; if (typeof v === "string") fail(E.TypeError, "a bytes-like object is required, not 'str'"); return drain(v).map(item); };
        BA.dict.set("append", builtin("append", 2, (a) => { a[0].items.push(item(a[1])); return null; }));
        BA.dict.set("extend", builtin("extend", 2, (a) => { for (const x of items(a[1])) a[0].items.push(x); return null; }));
        BA.dict.set("insert", builtin("insert", 3, (a) => { let i = Number(needInt(a[1])); const n = a[0].items.length; if (i < 0) i = Math.max(0, i + n); a[0].items.splice(Math.min(i, n), 0, item(a[2])); return null; }));
        BA.dict.set("pop", builtin("pop", 2, (a) => { const arr = a[0].items; if (!arr.length) fail(E.IndexError, "pop from empty bytearray"); let i = a[1] === undefined ? arr.length - 1 : Number(needInt(a[1])); if (i < 0) i += arr.length; if (i < 0 || i >= arr.length) fail(E.IndexError, "pop index out of range"); return BigInt(arr.splice(i, 1)[0]); }, 1));
        BA.dict.set("remove", builtin("remove", 2, (a) => { const i = a[0].items.indexOf(item(a[1])); if (i < 0) fail(E.ValueError, "value not found in bytearray"); a[0].items.splice(i, 1); return null; }));
        BA.dict.set("clear", builtin("clear", 1, (a) => { a[0].items.length = 0; return null; }));
        BA.dict.set("reverse", builtin("reverse", 1, (a) => { a[0].items.reverse(); return null; }));
        BA.dict.set("copy", builtin("copy", 1, (a) => ({ cls: BA, items: a[0].items.slice(), dict: new Map() })));
        BA.dict.set("__setitem__", builtin("__setitem__", 3, (a) => {
            const arr = a[0].items;
            if (a[1] !== null && typeof a[1] === "object" && a[1].cls === T.slice) { const [start, stop, step, count] = rt.sliceIndices(a[1], arr.length); const values = items(a[2]); if (step === 1) { arr.splice(start, Math.max(0, stop - start), ...values); return null; } if (values.length !== count) fail(E.ValueError, "attempt to assign bytes of size " + values.length + " to extended slice of size " + count); for (let i = 0, j = start; i < count; i++, j += step) arr[j] = values[i]; return null; }
            let i = Number(needInt(a[1])); if (i < 0) i += arr.length; if (i < 0 || i >= arr.length) fail(E.IndexError, "bytearray index out of range"); arr[i] = item(a[2]); return null;
        }));
        BA.dict.set("__delitem__", builtin("__delitem__", 2, (a) => { const arr = a[0].items; if (a[1] !== null && typeof a[1] === "object" && a[1].cls === T.slice) { const [start, stop, step, count] = rt.sliceIndices(a[1], arr.length); if (step === 1) { arr.splice(start, Math.max(0, stop - start)); return null; } const drop = new Set(); for (let i = 0, j = start; i < count; i++, j += step) drop.add(j); a[0].items = arr.filter((_, i) => !drop.has(i)); return null; } let i = Number(needInt(a[1])); if (i < 0) i += arr.length; if (i < 0 || i >= arr.length) fail(E.IndexError, "bytearray index out of range"); arr.splice(i, 1); return null; }));
        BA.dict.set("__iadd__", builtin("__iadd__", 2, (a) => { for (const x of items(a[1])) a[0].items.push(x); return a[0]; }));
        BA.dict.set("__add__", builtin("__add__", 2, (a) => ({ cls: BA, items: a[0].items.concat(items(a[1])), dict: new Map() })));
        // Repetition keeps the bytearray type, from either side.
        const repeated = (ba, n) => rt.baseBinop("mul", { cls: T.bytes, items: ba.items }, n, false).items;
        const times = (a) => isInt(a[1]) ? { cls: BA, items: repeated(a[0], a[1]), dict: new Map() } : NOTIMPL;
        BA.dict.set("__mul__", builtin("__mul__", 2, times));
        BA.dict.set("__rmul__", builtin("__rmul__", 2, times));
        BA.dict.set("__imul__", builtin("__imul__", 2, (a) => { if (!isInt(a[1])) return NOTIMPL; a[0].items = repeated(a[0], a[1]); return a[0]; }));
        BA.dict.set("__repr__", builtin("__repr__", 1, (a) => "bytearray(" + rt.bytesRepr(a[0], true) + ")"));
        BA.dict.set("__hash__", null);
        BA.dict.set("__iter__", builtin("__iter__", 1, (a) => iter({ cls: T.bytes, items: a[0].items })));
        // A slice of a bytearray is a bytearray.
        BA.dict.set("__getitem__", builtin("__getitem__", 2, (a) => { const r = getitem({ cls: T.bytes, items: a[0].items }, a[1]); return r !== null && typeof r === "object" && r.cls === T.bytes ? { cls: BA, items: r.items, dict: new Map() } : r; }));
        BA.dict.set("__eq__", builtin("__eq__", 2, (a) => a[1] !== null && typeof a[1] === "object" && a[1].items !== undefined && isInstance(a[1], T.bytes) && a[0].items.length === a[1].items.length && a[0].items.every((x, i) => x === a[1].items[i])));
        B.set("bytearray", BA);
    }
    B.set("__name__", "builtins");
    B.set("__debug__", true);
    // BaseExceptionGroup / ExceptionGroup (PEP 654): auto-narrowing __new__,
    // recursive .subgroup() / .split() / .derive(), and except* helpers.
    {
        const isSeqArg = (v) => {
            if (v === null || typeof v !== "object") return false;
            if (v.cls === T.list || v.cls === T.tuple) return true;
            if (v.cls === T.dict || v.cls === T.set || v.cls === T.frozenset) return false;
            return rt.lookupType(typeOf(v), "__getitem__") !== undefined && rt.lookupType(typeOf(v), "__len__") !== undefined;
        };
        const makeEg = (cls, msg, excsArg) => {
            if (typeof msg !== "string") fail(E.TypeError, "argument 1 must be str, not " + typeOf(msg).name);
            if (!isSeqArg(excsArg)) fail(E.TypeError, "second argument (exceptions) must be a sequence");
            const excs = drain(excsArg);
            if (!excs.length) fail(E.ValueError, "second argument (exceptions) must be a non-empty sequence");
            let hasNonExc = false;
            for (let i = 0; i < excs.length; i++) {
                const x = excs[i];
                if (!isInstance(x, E.BaseException)) fail(E.ValueError, "Item " + i + " of second argument (exceptions) is not an exception");
                if (!isInstance(x, E.Exception)) hasNonExc = true;
            }
            let target = cls;
            if (cls === E.BaseExceptionGroup) {
                if (!hasNonExc) target = E.ExceptionGroup;
            } else if (isSubclass(cls, E.Exception)) {
                if (hasNonExc) fail(E.TypeError, "Cannot nest BaseExceptions in an ExceptionGroup");
            }
            const self = rt.makeExc(target, [msg, excsArg]);
            self.context = null;
            self.dict.set("message", msg);
            self.dict.set("exceptions", tuple(excs));
            return self;
        };
        const copyEgMeta = (src, dst) => {
            if (dst === null || typeof dst !== "object") return dst;
            dst.cause = src.cause;
            dst.context = src.context;
            dst.suppress = src.suppress;
            const srcFrames = rt.normFrames(src);
            if (srcFrames !== undefined) dst.frames = srcFrames.slice();
            dst.tbline = src.tbline;
            const notes = src.dict.get("__notes__");
            if (notes !== undefined) {
                dst.dict.set("__notes__", notes !== null && typeof notes === "object" && notes.cls === T.list ? list(notes.items.slice()) : notes);
            }
            return dst;
        };
        const egCondFn = (cond) => {
            if (isType(cond)) {
                if (!isSubclass(cond, E.BaseException)) fail(E.TypeError, "expected a function, exception type or tuple of exception types");
                return (e) => isInstance(e, cond);
            }
            if (cond !== null && typeof cond === "object" && cond.cls === T.tuple) {
                for (const item of cond.items) {
                    if (!isType(item) || !isSubclass(item, E.BaseException)) fail(E.TypeError, "expected a function, exception type or tuple of exception types");
                }
                return (e) => {
                    for (const item of cond.items) if (isInstance(e, item)) return true;
                    return false;
                };
            }
            if (rt.isCallable(cond)) return (e) => truth(rt.call(cond, [e], null));
            fail(E.TypeError, "expected a function, exception type or tuple of exception types");
        };
        const egSplitRec = (eg, matchFn) => {
            if (matchFn(eg)) return [eg, null];
            const excs = eg.dict.get("exceptions").items;
            const matched = [], unmatched = [];
            for (let i = 0; i < excs.length; i++) {
                const e = excs[i];
                if (isInstance(e, E.BaseExceptionGroup)) {
                    const pair = egSplitRec(e, matchFn);
                    if (pair[0] !== null) matched.push(pair[0]);
                    if (pair[1] !== null) unmatched.push(pair[1]);
                } else if (matchFn(e)) {
                    matched.push(e);
                } else {
                    unmatched.push(e);
                }
            }
            const mk = (xs) => {
                if (!xs.length) return null;
                const d = rt.call(rt.getattr(eg, "derive"), [list(xs)], null);
                if (!isInstance(d, E.BaseExceptionGroup)) fail(E.TypeError, "derive must return an instance of BaseExceptionGroup");
                return copyEgMeta(eg, d);
            };
            return [mk(matched), mk(unmatched)];
        };
        E.BaseExceptionGroup.dict.set("__new__", { cls: T.staticmethod, func: builtin("__new__", -1, (a) => {
            if (a.length < 3) fail(E.TypeError, "function takes exactly 2 arguments (" + (a.length - 1) + " given)");
            return makeEg(a[0], a[1], a[2]);
        }) });
        E.BaseExceptionGroup.dict.set("__init__", builtin("__init__", -1, () => null));
        E.BaseExceptionGroup.dict.set("derive", builtin("derive", 2, (a) => makeEg(E.BaseExceptionGroup, a[0].dict.get("message"), a[1])));
        E.BaseExceptionGroup.dict.set("__str__", builtin("__str__", 1, (a) => { const n = a[0].dict.get("exceptions").items.length; return a[0].dict.get("message") + " (" + n + " sub-exception" + (n === 1 ? "" : "s") + ")"; }));
        E.BaseExceptionGroup.dict.set("__repr__", builtin("__repr__", 1, (a) => {
            const items = a[0].args.items;
            const excsRepr = items.length >= 2 ? repr(items[1]) : repr(list(a[0].dict.get("exceptions").items.slice()));
            return typeOf(a[0]).name + "(" + repr(a[0].dict.get("message")) + ", " + excsRepr + ")";
        }));
        E.BaseExceptionGroup.dict.set("subgroup", builtin("subgroup", 2, (a) => egSplitRec(a[0], egCondFn(a[1]))[0]));
        E.BaseExceptionGroup.dict.set("split", builtin("split", 2, (a) => tuple(egSplitRec(a[0], egCondFn(a[1])))));
        // except* helpers (PEP 654).
        const checkStarSpec = (spec) => {
            if (isType(spec)) {
                if (!isSubclass(spec, E.BaseException)) fail(E.TypeError, "catching classes that do not inherit from BaseException is not allowed");
                if (isSubclass(spec, E.BaseExceptionGroup)) fail(E.TypeError, "catching ExceptionGroup with except* is not allowed. Use except instead.");
                return;
            }
            if (spec !== null && typeof spec === "object" && spec.cls === T.tuple) {
                for (const item of spec.items) checkStarSpec(item);
                return;
            }
            fail(E.TypeError, "catching classes that do not inherit from BaseException is not allowed");
        };
        const collectLeaves = (e, out) => {
            if (isInstance(e, E.BaseExceptionGroup)) {
                const xs = e.dict.get("exceptions").items;
                for (let i = 0; i < xs.length; i++) collectLeaves(xs[i], out);
            } else {
                out.add(e);
            }
        };
        R.eg_split = function (unmatched, spec) {
            checkStarSpec(spec);
            if (unmatched === null) return [null, null];
            if (isInstance(unmatched, E.BaseExceptionGroup)) {
                return egSplitRec(unmatched, (e) => R.excmatch(e, spec));
            }
            if (R.excmatch(unmatched, spec)) {
                const wrapped = makeEg(E.BaseExceptionGroup, "", tuple([unmatched]));
                copyEgMeta(unmatched, wrapped);
                return [wrapped, null];
            }
            return [null, unmatched];
        };
        R.eg_record = function (reraised, raised, matched, thrown) {
            const e = rt.normexc(thrown);
            if (e === matched) reraised.push(e);
            else raised.push(e);
            return null;
        };
        R.eg_finish = function (orig, unmatched, reraised, raised) {
            let kept = null;
            if (reraised.length === 0) {
                kept = unmatched;
            } else if (unmatched === null && reraised.length === 1) {
                kept = reraised[0];
            } else if (isInstance(orig, E.BaseExceptionGroup)) {
                const want = new Set();
                if (unmatched !== null) collectLeaves(unmatched, want);
                for (let i = 0; i < reraised.length; i++) collectLeaves(reraised[i], want);
                const origLeaves = new Set();
                collectLeaves(orig, origLeaves);
                let allOrig = true;
                for (const x of origLeaves) if (!want.has(x)) { allOrig = false; break; }
                kept = allOrig ? orig : egSplitRec(orig, (e) => want.has(e))[0];
            } else {
                kept = reraised[0];
            }
            if (raised.length === 0) {
                if (kept === null) return null;
                throw kept;
            }
            const all = raised.slice();
            if (kept !== null) all.push(kept);
            if (all.length === 1) throw all[0];
            const combined = makeEg(E.BaseExceptionGroup, "", list(all));
            combined.context = orig;
            throw combined;
        };
    }
    for (const n of ["BaseExceptionGroup", "ExceptionGroup"]) B.set(n, E[n]);
    // `object` and friends double as constructors through `construct`.
    rt.constructors.set(ObjectType, (args, kw, cls) => { if (cls === ObjectType) { if (args.length || (kw && kw.size)) fail(E.TypeError, "object() takes no arguments"); return { cls: ObjectType, dict: new Map() }; } return null; });
    rt.constructors.delete(ObjectType);
})(__zipp_py);

/* ---- Emitter lowering helpers ---------------------------------------------------------------------
 * Entry points the Rust emitter's lowerings call directly (bytes literals,
 * spec-order method calls, `yield from` delegation, guarded builtin calls),
 * kept as one block apart from the object model above. */
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T, E = rt.E, STOP = rt.STOP;
    const isInstance = rt.isInstance, getattr = rt.getattr, call = rt.call;
    // A bytes literal: its bytes as a Latin-1 string constant, built the
    // way the emitter's `bytes` helper builds one from its byte values.
    R.bytesconst = function (latin1) {
        const items = new Array(latin1.length);
        for (let i = 0; i < latin1.length; i++) items[i] = latin1.charCodeAt(i) & 255;
        return R.bytes(items);
    };

    // `obj.name(args)` in Python's order: the attribute is resolved before
    // the arguments are evaluated. A plain or builtin function found on the
    // type, not shadowed by an instance attribute, comes back unbound with
    // `R.mself` true, and the emitter calls it with the receiver prepended
    // (no bound method is allocated); anything else is the attribute's value,
    // with `R.mself` false. The emitter reads `mself` straight after the call.
    R.mself = false;
    R.mlookup = function (obj, name) {
        let t;
        if (obj !== null && typeof obj === "object") {
            if (obj.isType || obj.cls === T.module || obj.cls === T.super) { const v = getattr(obj, name); R.mself = false; return v; }
            t = obj.cls || rt.ObjectType;
        } else {
            t = rt.typeOf(obj);
        }
        if (t.userClass === true) {
            const gv = t.gv[name];
            if (gv !== undefined && t.ga[name] === true && (obj.dict === undefined || obj.dict === null || obj.dict.get(name) === undefined)) {
                R.mself = false;
                return gv;
            }
        }
        if (!rt.getPlan(t)) { const v = getattr(obj, name); R.mself = false; return v; }
        const attr = rt.lookupType(t, name);
        if (attr !== undefined && attr !== null && typeof attr === "object" && (attr.cls === T.function || attr.cls === T.builtin_function_or_method)) {
            const own = obj !== null && typeof obj === "object" && obj.dict !== undefined && obj.dict !== null ? obj.dict.get(name) : undefined;
            if (own === undefined) { R.mself = true; return attr; }
        }
        // `getattr` may run guest code (which may look methods up itself).
        const v = getattr(obj, name);
        R.mself = false;
        return v;
    };

    // `obj.name` for a call with `argc` positional values, as `mlookup`
    // resolves it (`R.mself` says whether the receiver goes first), and the
    // inline method caches the emitter probes next time: `gm` for a plain
    // Python function on a user class, `gb` for a builtin method of a
    // dict-less builtin container or str. A builtin that unboxes its
    // receiver is bound by `getattr` instead when the receiver is boxed.
    R.mfind = function (obj, name, argc) {
        let t;
        if (obj !== null && typeof obj === "object") {
            if (obj.isType) {
                const gt = obj.gt[name];
                if (gt !== undefined) {
                    R.mself = false;
                    return gt;
                }
                const v = getattr(obj, name);
                if (obj.cls === rt.TypeType && !obj.isABC && name !== "__name__" && name !== "__qualname__" && name !== "__module__" && name !== "__mro__" && name !== "__bases__" && name !== "__dict__" && name !== "__class__" && name !== "__type_params__" && name !== "__doc__") {
                    const attr = rt.lookupType(obj, name);
                    if (attr !== undefined && attr !== null && typeof attr === "object") {
                        const ac = attr.cls;
                        if (ac === T.staticmethod || ac === T.classmethod || ac === T.function || ac === T.builtin_function_or_method || attr.isType === true) {
                            obj.gt[name] = v; rt.noteFlagged(obj);
                        }
                    }
                }
                R.mself = false;
                return v;
            }
            if (obj.cls === T.module || obj.cls === T.super) { const v = getattr(obj, name); R.mself = false; return v; }
            t = obj.cls || rt.ObjectType;
        } else {
            t = rt.typeOf(obj);
        }
        if (t.userClass === true) {
            const gv = t.gv[name];
            if (gv !== undefined && t.ga[name] === true && (obj.dict === undefined || obj.dict === null || obj.dict.get(name) === undefined)) {
                R.mself = false;
                return gv;
            }
            const gc = t.gc[name];
            if (gc !== undefined && (obj.dict === undefined || obj.dict === null || obj.dict.get(name) === undefined)) {
                R.mself = false;
                return gc;
            }
        }
        if (!rt.getPlan(t)) { const v = getattr(obj, name); R.mself = false; return v; }
        const attr = rt.lookupType(t, name);
        if (attr !== undefined && attr !== null && typeof attr === "object") {
            const ac = attr.cls;
            if (ac === T.staticmethod) {
                const own = obj !== null && typeof obj === "object" && obj.dict !== undefined && obj.dict !== null ? obj.dict.get(name) : undefined;
                if (own === undefined) {
                    if (t.userClass === true && name !== "__dict__") { t.ga[name] = true; t.gv[name] = attr.func; rt.noteFlagged(t); }
                    R.mself = false;
                    return attr.func;
                }
            }
            if (ac === T.classmethod) {
                const own = obj !== null && typeof obj === "object" && obj.dict !== undefined && obj.dict !== null ? obj.dict.get(name) : undefined;
                if (own === undefined) {
                    const b = rt.bound(attr.func, t);
                    if (t.userClass === true && t.cls === rt.TypeType && name !== "__dict__") { t.gc[name] = b; rt.noteFlagged(t); }
                    R.mself = false;
                    return b;
                }
            }
            if (ac === T.function || ac === T.builtin_function_or_method) {
                const own = obj !== null && typeof obj === "object" && obj.dict !== undefined && obj.dict !== null ? obj.dict.get(name) : undefined;
                if (own === undefined) {
                    if (ac === T.function) {
                        if (t.userClass === true || (t.cls === rt.TypeType && !t.isABC)) { t.gm[name] = attr; rt.noteFlagged(t); }
                        R.mself = true;
                        return attr;
                    }
                    if (!(attr.unboxSelf === true && obj !== null && typeof obj === "object" && obj.pyval !== undefined)) {
                        const n = argc + 1, arity = attr.arity;
                        const noDict = obj === null || typeof obj !== "object" || obj.dict === undefined;
                        if (argc >= 0 && (noDict || (attr.unboxSelf !== true && t.cls === rt.TypeType && !t.isABC)) && (arity < 0 ? n >= attr.minArity : n <= arity && n >= attr.minArity)) {
                            t.gb[name + "#" + argc] = attr; rt.noteFlagged(t);
                        }
                        R.mself = true;
                        return attr;
                    }
                }
            }
        }
        const v = getattr(obj, name);
        R.mself = false;
        return v;
    };
    // `super().name` for a call, as `superof` then `superAttr` resolve it,
    // without building the super object: a plain Python function comes
    // back unbound with `R.mself` set (the call passes `self` first); any
    // other attribute comes back bound, as `superAttr` gives it.
    // An instance's class keeps the plain functions found this way
    // (`gs[name]`: the class searched past and the function), dropped
    // wherever its other lookup tables are.
    // What the emitter calls first: the engine's form of the cached hit
    // below (`vm::py_attr`), `undefined` when this function must answer
    // (always, without the native).
    let SMFIND = null;
    try { SMFIND = typeof __zipp_py_smfind === "function" ? __zipp_py_smfind : null; } catch (e) { SMFIND = null; }
    R.SMFNATIVE = SMFIND !== null ? SMFIND : function () { return undefined; };
    R.smfind = function (cls, self, name) {
        if (self !== null && typeof self === "object" && self.isType !== true) {
            const t = self.cls;
            if (t !== undefined && t.gs !== undefined) {
                const hit = t.gs[name];
                if (hit !== undefined && hit.cls === cls) { R.mself = true; return hit.f; }
            }
        }
        if (!rt.isType(cls)) rt.fail(E.TypeError, "super() argument 1 must be type, not " + rt.typeOf(cls).name);
        const objtype = rt.isType(self) && rt.isSubclass(self, cls) ? self : rt.typeOf(self);
        const mro = objtype.mro;
        const start = mro.indexOf(cls) + 1;
        if (start === 0) rt.fail(E.TypeError, "super(type, obj): obj must be an instance or subtype of type");
        const onClass = rt.isType(self) && self === objtype;
        for (let i = start; i < mro.length; i++) {
            const v = mro[i].dict.get(name);
            if (v === undefined) continue;
            if (!onClass && v !== null && typeof v === "object" && (v.cls === T.function || (v.cls === T.builtin_function_or_method && v.unboxSelf !== true))) {
                if (start > 0 && objtype.gs !== undefined && self !== null && typeof self === "object" && self.cls === objtype) {
                    objtype.gs[name] = { cls: cls, f: v }; rt.noteFlagged(objtype);
                }
                R.mself = true; return v;
            }
            const r = rt.descrGet(v, onClass ? null : self, objtype);
            R.mself = false;
            return r;
        }
        rt.fail(E.AttributeError, "'super' object has no attribute '" + name + "'");
    };
    R.TSTR = T.str; R.TMODULE = T.module;
    // The builtins the emitter's method intrinsics stand in for.
    R.LAPPEND = T.list.dict.get("append"); R.DGET = T.dict.dict.get("get"); R.MAX_ITEMS = rt.MAX_ITEMS;

    // One step of `yield from` (PEP 380). Mode 0 sends `v` into the
    // subiterator (`next()` when `v` is None), mode 1 throws `v` into it.
    // Returns the next value to yield, or STOP once the subiterator has
    // finished, with its return value in `R.yfret` (read straight after).
    R.yfret = null;
    const isStop = (e) => e !== null && typeof e === "object" && e.cls !== undefined && isInstance(e, E.StopIteration);
    const stopValue = (e) => e.args && e.args.items.length ? e.args.items[0] : null;
    // The Python object behind an iterator record `iter()` made.
    const target = (it) => it.wrapped !== undefined ? it.wrapped : it;
    function finished(value) { R.yfret = value; return STOP; }
    const yfstepFallback = function (it, mode, v) {
        const gen = it.cls === T.generator || it.cls === T.coroutine || it.cls === T.coroutine_wrapper || it.cls === T.async_generator_asend;
        if (mode === 0) {
            if (v === null || v === undefined) {
                if (gen) {
                    if (it.cls === T.generator && it.done) return finished(null);
                    const r = it.next();
                    return r === STOP ? finished(it.returned) : r;
                }
                try {
                    const r = it.next();
                    return r === STOP ? finished(null) : r;
                } catch (e) {
                    if (isStop(e)) return finished(stopValue(e));
                    throw e;
                }
            }
            const send = gen ? null : getattr(target(it), "send");
            try {
                return gen ? it.send(v) : call(send, [v], null);
            } catch (e) {
                if (isStop(e)) return finished(stopValue(e));
                throw e;
            }
        }
        const exc = rt.normexc(v);
        if (isInstance(exc, E.GeneratorExit)) {
            closeSub(it);
            throw v;
        }
        let thrower = null;
        if (!gen) {
            try { thrower = getattr(target(it), "throw"); }
            catch (e) { if (e !== null && typeof e === "object" && e.cls !== undefined && isInstance(e, E.AttributeError)) throw v; throw e; }
        }
        try {
            return gen ? it.throwIn(exc) : call(thrower, [exc], null);
        } catch (e) {
            // The exception itself coming back is not the subiterator finishing.
            if (isStop(e) && e !== exc) return finished(e.args.items.length ? stopValue(e) : gen ? it.returned : null);
            throw e;
        }
    };
    let YFSTEP_NATIVE = null;
    try { YFSTEP_NATIVE = typeof __zipp_py_yfstep === "function" ? __zipp_py_yfstep : null; } catch (e) { YFSTEP_NATIVE = null; }
    R.yfstepFallback = yfstepFallback;
    R.yfstep = YFSTEP_NATIVE !== null ? YFSTEP_NATIVE : yfstepFallback;
    function closeSub(it) {
        if (it.cls === T.generator || it.cls === T.coroutine || it.cls === T.coroutine_wrapper || it.cls === T.async_generator_asend) { it.close(); return; }
        let close;
        try { close = getattr(target(it), "close"); }
        catch (e) { if (e !== null && typeof e === "object" && e.cls !== undefined && isInstance(e, E.AttributeError)) return; throw e; }
        call(close, [], null);
    }
    // The delegating generator was closed while suspended in `yield from`.
    R.yfclose = function (it) { closeSub(it); return null; };
})(__zipp_py);

/* ---- Emitter fast-path support ---------------------------------------------------------------------
 * The class objects the emitter's inline loops test for, and the one step
 * that decides how a loop iterates. */
(function (R) {
    "use strict";
    const rt = R.__rt, T = rt.T;
    R.TLIST = T.list; R.TTUPLE = T.tuple; R.TRANGE = T.range; R.TDICT = T.dict; R.TSET = T.set; R.TFROZENSET = T.frozenset; R.TSLICE = T.slice;
    // The two classes whose instances' `__class__` is not their `cls` as
    // `getattr` answers it (a module's globals, a super proxy's MRO).
    R.TMODULE = T.module; R.TSUPER = T.super; R.TMETHOD = T.method; R.TLISTITER = T.list_iterator; R.TITER = T.iterator; R.TGEN = T.generator; R.TTYPE = rt.TypeType;
    // The emitter's native list and tuple displays (`PySeq`) copy this
    // record, which `sequence` made and nothing else ever sees.
    R.SEQTMPL = rt.list([]);
    R.SLICETMPL = R.slice(null, null, null);
    // The `Number` intrinsic, for the emitter's guarded int-to-index conversions.
    R.Number = Number;
    // The text limit an inline str concatenation is checked against.
    R.MAX_TEXT = rt.MAX_TEXT;
    // A loop's iterable: an exact list, tuple or range as itself (the loop
    // indexes or counts it in place), anything else its iterator.
    R.seqiter = function (v) {
        if (v !== null && typeof v === "object") {
            const c = v.cls;
            if (c === T.list || c === T.tuple || c === T.range || c === T.generator || c === T.iterator || c === T.list_iterator) return v;
        }
        return rt.iter(v);
    };
    // Guarded builtin intrinsics: once a call site's callee is found to be
    // the builtin itself (`R.B*`), the emitter calls these directly.
    // The builtins dictionary itself: the emitter's inline global read
    // probes it after the module's own dictionary, ahead of `gload`.
    R.BUILTINS = rt.builtins;
    R.BLEN = rt.builtins.get("len"); R.len1 = rt.len;
    R.BISINSTANCE = rt.builtins.get("isinstance");
    // The exact classes of the primitive values, and bytes, for the
    // emitter's native `isinstance` (`PyIsInstance`); never changed.
    R.ISTYPES = [T.NoneType, T.bool, T.int, T.float, T.str, T.bytes];
    R.BHASATTR = rt.builtins.get("hasattr");
    const isinstanceCode = R.BISINSTANCE.code;
    R.isinst = function (v, t) {
        if (t !== null && typeof t === "object" && t.isType === true && t.cls === rt.TypeType) {
            // `v`'s exact type is `t`: true without the MRO walk.
            const tv = typeof v;
            const c = tv === "object" ? (v === null ? T.NoneType : v.cls) : tv === "bigint" ? T.int : tv === "string" ? T.str : tv === "number" ? T.float : tv === "boolean" ? T.bool : undefined;
            if (c === t) return true;
            // A primitive's type is a builtin one (no bytes/bytearray case,
            // bool's MRO holds int): its MRO answers.
            if (c !== undefined && (tv !== "object" || v === null)) return c.mro.indexOf(t) >= 0;
            return rt.isinstanceCheck(v, t);
        }
        return isinstanceCode([v, t]);
    };
})(__zipp_py);
