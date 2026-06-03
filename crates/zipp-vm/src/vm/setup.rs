#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// The `.prototype` object of a function/class value — lazily created and
    /// cached so it has stable identity (`C.prototype === C.prototype`). A class's
    /// prototype carries its OWN methods plus a `constructor` back-reference; a
    /// plain function's prototype just has `constructor`. `None` for non-callables
    /// (a plain object / array / instance has no `.prototype`).
    pub(crate) fn prototype_of(&mut self, obj: Value) -> Option<Value> {
        if !obj.is_heap() {
            return None;
        }
        let idx = obj.heap_index();
        // A built-in constructor global (Map/Set/Date/…) keeps its .prototype as an
        // own property; return it so `x instanceof Map` (instanceof_via_proto) works.
        if let HeapObj::Object(m) = self.heap.get(idx) {
            if m.is_ctor {
                return m.get("prototype");
            }
        }
        if !matches!(
            self.heap.get(idx),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Class(_)
        ) {
            return None;
        }
        if let Some(&p) = self.prototypes.get(&idx) {
            return Some(Value::heap(p));
        }
        // Collect own methods + accessors first (ends the immutable heap borrow
        // before alloc).
        #[allow(clippy::type_complexity)]
        let (methods, getters, setters): (
            Vec<(String, Value)>,
            Vec<(String, Value)>,
            Vec<(String, Value)>,
        ) = match self.heap.get(idx) {
            HeapObj::Class(c) => (
                c.methods.iter().map(|(k, v)| (k.clone(), *v)).collect(),
                c.getters.iter().map(|(k, v)| (k.clone(), *v)).collect(),
                c.setters.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            ),
            _ => (Vec::new(), Vec::new(), Vec::new()),
        };
        // A derived class's prototype chains to its parent's prototype (so a
        // subclass instance is `instanceof` the parent — including built-in
        // parents like Error/Array — and inherits the parent's prototype methods
        // through the chain). Method/getter resolution itself still uses the
        // class `extends` chain; this only extends the prototype fallback.
        let parent: Option<u32> = match self.heap.get(idx) {
            HeapObj::Class(c) => c.parent,
            _ => None,
        };
        // Methods and the constructor back-reference are NON-enumerable
        // (writable + configurable), matching ES `class`/function semantics that
        // test262's verifyProperty checks.
        let nonenum =
            PropAttr { writable: true, enumerable: false, configurable: true, accessor: false, setter: Value::UNDEFINED };
        let mut map = ObjMap::new();
        for (k, v) in &methods {
            map.define(k, *v, nonenum);
        }
        // Accessors become real accessor properties (getter in `vals`, setter in
        // `attr.setter`) so getOwnPropertyDescriptor / getOwnPropertyNames /
        // enumeration reflect them; non-enumerable + configurable per spec. A
        // get+set pair on one key merges into a single accessor property.
        let acc_attr =
            PropAttr { writable: false, enumerable: false, configurable: true, accessor: true, setter: Value::UNDEFINED };
        for (k, g) in &getters {
            map.define(k, *g, acc_attr);
        }
        for (k, s) in &setters {
            if let Some(i) = map.pos(k) {
                map.attrs[i].accessor = true;
                map.attrs[i].setter = *s;
            } else {
                let mut a = acc_attr;
                a.setter = *s;
                map.define(k, Value::UNDEFINED, a);
            }
        }
        map.define("constructor", obj, nonenum);
        let p = self.heap.alloc(HeapObj::Object(map));
        self.prototypes.insert(idx, p);
        // Link the prototype chain to the parent's prototype (a parent class's
        // own prototype, or a built-in parent ctor's `.prototype`).
        if let Some(par) = parent {
            if let Some(pp) = self.prototype_of(Value::heap(par)) {
                if pp.is_heap() {
                    self.proto_of.insert(p, pp);
                }
            }
        }
        Some(Value::heap(p))
    }

    /// Build one of the %GeneratorFunction% / %AsyncFunction% /
    /// %AsyncGeneratorFunction% intrinsic constructors and its `.prototype`,
    /// returning `(ctor, prototype)` heap indices. These are NOT global — they are
    /// reached via `Object.getPrototypeOf(function*(){}).constructor` etc. Their
    /// chain (spec 27.3/27.4/27.7): ctor [[Prototype]] = %Function%, ctor.prototype
    /// = proto ({w:false,e:false,c:true}); proto [[Prototype]] = %Function.prototype%,
    /// proto.constructor = ctor, proto[@@toStringTag] = `tag`. Requires `fn_proto`
    /// and `function_ctor` already set.
    fn build_dynamic_fn_intrinsic(&mut self, tag: &str) -> (u32, u32) {
        // {writable:false, enumerable:false, configurable:true} — name/length and
        // the intrinsic .prototype and @@toStringTag descriptor.
        let nameish = PropAttr {
            writable: false,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let method_attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let proto = self.heap.alloc(HeapObj::Object(ObjMap::new()));
        self.proto_of.insert(proto, Value::heap(self.fn_proto));
        let mut cm = ObjMap::new();
        cm.define("prototype", Value::heap(proto), nameish);
        cm.is_ctor = true;
        let ctor = self.heap.alloc(HeapObj::Object(cm));
        self.proto_of.insert(ctor, Value::heap(self.function_ctor));
        let namev = self.alloc_str(tag.to_string());
        let tagv = self.alloc_str(tag.to_string());
        if let HeapObj::Object(m) = self.heap.get_mut(ctor) {
            m.define("name", namev, nameish);
            m.define("length", Value::num(1.0), nameish);
        }
        if let HeapObj::Object(m) = self.heap.get_mut(proto) {
            m.define("@@toStringTag", tagv, nameish);
            m.define("constructor", Value::heap(ctor), method_attr);
        }
        (ctor, proto)
    }

    /// Build the built-in global object graph (Object/Array/Function + their
    /// prototypes, with methods as native function VALUES) and inject it into the
    /// global slots the compiler reserved for those free identifiers. Makes
    /// `Array.isArray`, `Object.defineProperty`, `Function.prototype.call`, etc.
    /// usable as first-class values (what the test262 harness binds).
    pub(crate) fn setup_globals(&mut self) {
        use native::*;
        // A built-in method property: a native function, non-enumerable but
        // writable + configurable (matching built-in method descriptors).
        let method_attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let proto_attr = PropAttr {
            writable: false,
            enumerable: false,
            configurable: false,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let mut build = |vm: &mut Self, methods: &[(&str, u16)], protolink: Option<u32>| -> u32 {
            let mut m = ObjMap::new();
            for &(name, id) in methods {
                let nv = Value::heap(vm.heap.alloc(HeapObj::Native(id)));
                m.define(name, nv, method_attr);
            }
            if let Some(p) = protolink {
                m.define("prototype", Value::heap(p), proto_attr);
                // A global built WITH a .prototype is a constructor (Object/Array/Map/…);
                // a namespace (Reflect/Math/JSON, protolink None) is not.
                m.is_ctor = true;
            }
            vm.heap.alloc(HeapObj::Object(m))
        };
        // Prototypes.
        self.obj_proto = build(
            self,
            &[
                ("hasOwnProperty", PROTO_HAS_OWN),
                ("propertyIsEnumerable", PROTO_PROP_ENUM),
                ("isPrototypeOf", PROTO_IS_PROTO_OF),
                ("valueOf", PROTO_VALUE_OF),
                ("toString", PROTO_TO_STRING),
                ("toLocaleString", PROTO_TO_LOCALE_STRING),
                ("__defineGetter__", OBJPROTO_DEFINE_GETTER),
                ("__defineSetter__", OBJPROTO_DEFINE_SETTER),
                ("__lookupGetter__", OBJPROTO_LOOKUP_GETTER),
                ("__lookupSetter__", OBJPROTO_LOOKUP_SETTER),
            ],
            None,
        );
        // `Object.prototype.__proto__` is an accessor (get/set the prototype).
        {
            let pg = Value::heap(self.heap.alloc(HeapObj::Native(OBJPROTO_PROTO_GET)));
            let ps = Value::heap(self.heap.alloc(HeapObj::Native(OBJPROTO_PROTO_SET)));
            let acc = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: true,
                setter: ps,
            };
            if let HeapObj::Object(m) = self.heap.get_mut(self.obj_proto) {
                m.define("__proto__", pg, acc);
            }
        }
        self.fn_proto = build(
            self,
            &[
                ("call", FN_CALL),
                ("apply", FN_APPLY),
                ("bind", FN_BIND),
                ("toString", FN_TO_STRING),
            ],
            None,
        );
        // Build the Array.prototype / String.prototype method lists from the
        // PROTO_METHODS table (id = PROTO_METHOD_BASE + index), so methods are
        // first-class values (`Array.prototype.map.call(arr, fn)`).
        let mut arr_methods: Vec<(&str, u16)> = vec![("join", ARR_JOIN), ("push", ARR_PUSH)];
        let mut str_methods: Vec<(&str, u16)> = Vec::new();
        let mut num_methods: Vec<(&str, u16)> = Vec::new();
        let mut set_methods: Vec<(&str, u16)> = Vec::new();
        let mut map_methods: Vec<(&str, u16)> = Vec::new();
        let mut bool_methods: Vec<(&str, u16)> = Vec::new();
        let mut date_methods: Vec<(&str, u16)> = Vec::new();
        let mut promise_methods: Vec<(&str, u16)> = Vec::new();
        for (i, &(name, kind, _len)) in native::PROTO_METHODS.iter().enumerate() {
            let id = native::PROTO_METHOD_BASE + i as u16;
            match kind {
                0 => arr_methods.push((name, id)),
                1 => str_methods.push((name, id)),
                2 => num_methods.push((name, id)),
                3 => set_methods.push((name, id)),
                4 => map_methods.push((name, id)),
                5 => bool_methods.push((name, id)),
                6 => date_methods.push((name, id)),
                _ => promise_methods.push((name, id)), // kind 7
            }
        }
        self.arr_proto = build(self, &arr_methods, None);
        self.str_proto = build(self, &str_methods, None);
        let str_proto = self.str_proto;
        let num_proto = build(self, &num_methods, None);
        let set_proto = build(self, &set_methods, None);
        let map_proto = build(self, &map_methods, None);
        let bool_proto = build(self, &bool_methods, None);
        let date_proto = build(self, &date_methods, None);
        let promise_proto = build(self, &promise_methods, None);
        // Store the proto indices so Map/Set/Date/Promise instances can delegate
        // method-as-value access to them (get_prop), mirroring arr_proto/str_proto.
        self.set_proto = set_proto;
        self.map_proto = map_proto;
        self.date_proto = date_proto;
        self.promise_proto = promise_proto;
        self.num_proto = num_proto;
        self.bool_proto = bool_proto;
        // Constructors.
        let obj_proto = self.obj_proto;
        let arr_proto = self.arr_proto;
        let fn_proto = self.fn_proto;
        let object_ctor = build(
            self,
            &[
                ("defineProperty", OBJ_DEFINE_PROPERTY),
                ("defineProperties", OBJ_DEFINE_PROPERTIES),
                ("getOwnPropertyDescriptor", OBJ_GET_OWN_DESC),
                ("getOwnPropertyNames", OBJ_GET_OWN_NAMES),
                ("getPrototypeOf", OBJ_GET_PROTO),
                ("keys", OBJ_KEYS),
                ("values", OBJ_VALUES),
                ("entries", OBJ_ENTRIES),
                ("assign", OBJ_ASSIGN),
                ("create", OBJ_CREATE),
                ("is", OBJ_IS),
                ("hasOwn", OBJ_HAS_OWN),
                ("fromEntries", OBJ_FROM_ENTRIES),
                ("setPrototypeOf", OBJ_SET_PROTO_OF),
                ("getOwnPropertySymbols", OBJ_GET_OWN_SYMBOLS),
                ("getOwnPropertyDescriptors", OBJ_GET_OWN_DESCS),
                ("freeze", OBJ_FREEZE),
                ("isFrozen", OBJ_IS_FROZEN),
                ("seal", OBJ_SEAL),
                ("isSealed", OBJ_IS_SEALED),
                ("preventExtensions", OBJ_PREVENT_EXT),
                ("isExtensible", OBJ_IS_EXT),
                ("groupBy", OBJ_GROUP_BY),
            ],
            Some(obj_proto),
        );
        let array_ctor = build(self, &[("isArray", ARR_IS_ARRAY), ("from", ARR_FROM), ("of", ARR_OF)], Some(arr_proto));
        let function_ctor = build(self, &[], Some(fn_proto));
        self.function_ctor = function_ctor;
        let string_ctor = build(
            self,
            &[
                ("fromCharCode", STR_FROM_CHAR_CODE),
                ("fromCodePoint", STR_FROM_CODE_POINT),
                ("raw", STR_RAW),
            ],
            Some(str_proto),
        );
        // `Number`: the numeric constants (non-writable/enumerable/configurable per
        // spec) + Number.prototype. `Number(x)` / `Number.isInteger(x)` etc. are
        // call-site lowered (GlobalFn), so only the value-level shape is built here.
        let number_ctor = {
            let mut m = ObjMap::new();
            let consts: &[(&str, f64)] = &[
                ("MAX_SAFE_INTEGER", 9007199254740991.0),
                ("MIN_SAFE_INTEGER", -9007199254740991.0),
                ("MAX_VALUE", f64::MAX),
                ("MIN_VALUE", 5e-324),
                ("EPSILON", f64::EPSILON),
                ("POSITIVE_INFINITY", f64::INFINITY),
                ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
                ("NaN", f64::NAN),
            ];
            for &(n, v) in consts {
                m.define(n, Value::num(v), proto_attr);
            }
            // Static methods as first-class values (the call form is StaticFn/GlobalFn).
            for &(name, id) in &[
                ("isInteger", NUM_IS_INTEGER),
                ("isNaN", NUM_IS_NAN),
                ("isFinite", NUM_IS_FINITE),
                ("isSafeInteger", NUM_IS_SAFE_INTEGER),
                ("parseInt", GLOBAL_PARSE_INT),
                ("parseFloat", GLOBAL_PARSE_FLOAT),
            ] {
                let nv = Value::heap(self.heap.alloc(HeapObj::Native(id)));
                m.define(name, nv, method_attr);
            }
            m.define("prototype", Value::heap(num_proto), proto_attr);
            m.is_ctor = true; // Number is a constructor (typeof "function").
            self.heap.alloc(HeapObj::Object(m))
        };
        // Set / Map / Boolean / Date globals: their .prototype (construction is
        // compile-lowered to NewSet / NewMap / DateNew; value-level shape here).
        let set_ctor = build(self, &[], Some(set_proto));
        let map_ctor = build(self, &[("groupBy", MAP_GROUP_BY)], Some(map_proto));
        let boolean_ctor = build(self, &[], Some(bool_proto));
        let date_ctor = build(
            self,
            &[("now", DATE_NOW), ("parse", DATE_PARSE), ("UTC", DATE_UTC)],
            Some(date_proto),
        );
        // Promise global: static combinators + Promise.prototype. `new Promise`
        // is compile-lowered to NewPromise.
        let promise_ctor = build(
            self,
            &[
                ("resolve", PROMISE_RESOLVE),
                ("reject", PROMISE_REJECT),
                ("all", PROMISE_ALL),
                ("allSettled", PROMISE_ALLSETTLED),
                ("race", PROMISE_RACE),
                ("any", PROMISE_ANY),
                // withResolvers/try validate `this` is a constructor (via
                // is_constructor) so the ctx-non-ctor/non-object tests throw correctly.
                ("withResolvers", PROMISE_WITH_RESOLVERS),
                ("try", PROMISE_TRY),
            ],
            Some(promise_proto),
        );
        // `Reflect`: a namespace object (no .prototype) of static methods that
        // mostly delegate to the existing property machinery.
        let reflect_ctor = build(
            self,
            &[
                ("apply", REFLECT_APPLY),
                ("construct", REFLECT_CONSTRUCT),
                ("get", REFLECT_GET),
                ("set", REFLECT_SET),
                ("has", REFLECT_HAS),
                ("deleteProperty", REFLECT_DELETE),
                ("ownKeys", REFLECT_OWN_KEYS),
                ("getPrototypeOf", REFLECT_GET_PROTO),
                ("setPrototypeOf", REFLECT_SET_PROTO),
                ("defineProperty", REFLECT_DEFINE),
                ("getOwnPropertyDescriptor", REFLECT_GET_OWN_DESC),
                ("isExtensible", REFLECT_IS_EXT),
                ("preventExtensions", REFLECT_PREVENT_EXT),
            ],
            None,
        );
        // `WeakMap`/`WeakSet`: distinct prototypes (get/set/has/delete, add/has/delete
        // — deliberately NO size/keys/values/iteration). Construction is compile-lowered
        // to NewWeakMap/NewWeakSet.
        let weakmap_proto = build(
            self,
            &[("get", WM_GET), ("set", WM_SET), ("has", WM_HAS), ("delete", WM_DELETE)],
            None,
        );
        let weakset_proto = build(self, &[("add", WS_ADD), ("has", WS_HAS), ("delete", WS_DELETE)], None);
        let weakref_proto = build(self, &[("deref", WR_DEREF)], None);
        let finreg_proto = build(self, &[("register", FR_REGISTER), ("unregister", FR_UNREGISTER)], None);
        // %ArrayIteratorPrototype% (next + @@iterator). Array entries/keys/values
        // iterators delegate here.
        let array_iter_proto = build(self, &[("next", ITER_NEXT), ("@@iterator", ITER_SELF)], None);
        self.array_iter_proto = array_iter_proto;
        // Distinct %MapIteratorPrototype% / %SetIteratorPrototype% (same natives,
        // different identity so getPrototypeOf discriminates them).
        self.map_iter_proto = build(self, &[("next", ITER_NEXT), ("@@iterator", ITER_SELF)], None);
        self.set_iter_proto = build(self, &[("next", ITER_NEXT), ("@@iterator", ITER_SELF)], None);
        // %StringIteratorPrototype% + String.prototype[@@iterator] (code points).
        self.string_iter_proto = build(self, &[("next", ITER_NEXT), ("@@iterator", ITER_SELF)], None);
        {
            let it = Value::heap(self.heap.alloc(HeapObj::Native(STR_ITERATOR)));
            if let HeapObj::Object(m) = self.heap.get_mut(str_proto) {
                m.define("@@iterator", it, method_attr);
            }
        }
        // ── ES2025 Iterator Helpers ──
        // %Iterator.prototype% (the shared root holding the helper methods).
        let iter_root = build(
            self,
            &[
                ("map", ITER_MAP),
                ("filter", ITER_FILTER),
                ("take", ITER_TAKE),
                ("drop", ITER_DROP),
                ("flatMap", ITER_FLATMAP),
                ("reduce", ITER_REDUCE),
                ("toArray", ITER_TOARRAY),
                ("forEach", ITER_FOREACH),
                ("some", ITER_SOME),
                ("every", ITER_EVERY),
                ("find", ITER_FIND),
                ("@@iterator", ITER_SELF),
            ],
            None,
        );
        self.iterator_proto_root = iter_root;
        self.proto_of.insert(iter_root, Value::heap(obj_proto));
        // @@toStringTag + constructor are accessors on %Iterator.prototype%.
        let acc_attr = |setter: Value| PropAttr {
            writable: false,
            enumerable: false,
            configurable: true,
            accessor: true,
            setter,
        };
        let tag_get = Value::heap(self.heap.alloc(HeapObj::Native(ITER_TAG_GET)));
        let tag_set = Value::heap(self.heap.alloc(HeapObj::Native(ITER_TAG_SET)));
        let ctor_get = Value::heap(self.heap.alloc(HeapObj::Native(ITER_CTOR_GET)));
        let ctor_set = Value::heap(self.heap.alloc(HeapObj::Native(ITER_CTOR_SET)));
        if let HeapObj::Object(m) = self.heap.get_mut(iter_root) {
            m.define("@@toStringTag", tag_get, acc_attr(tag_set));
            m.define("constructor", ctor_get, acc_attr(ctor_set));
        }
        // %IteratorHelperPrototype% (next/return for lazy helpers) chains to root.
        let helper_proto =
            build(self, &[("next", ITER_HELPER_NEXT), ("return", ITER_HELPER_RETURN)], None);
        self.iterator_helper_proto = helper_proto;
        self.proto_of.insert(helper_proto, Value::heap(iter_root));
        let helper_tag = self.alloc_str("Iterator Helper".to_string());
        let tag_data = PropAttr {
            writable: false,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        if let HeapObj::Object(m) = self.heap.get_mut(helper_proto) {
            m.define("@@toStringTag", helper_tag, tag_data);
        }
        // %GeneratorPrototype% — generator instances delegate here. next/return/
        // throw + @@iterator (self) + @@toStringTag "Generator"; chains to
        // %Iterator.prototype% so a generator inherits the helper methods
        // (`g().map(...)`, `g().take(n)`, …).
        let gen_proto = build(
            self,
            &[
                ("next", native::GEN_NEXT),
                ("return", native::GEN_RETURN),
                ("throw", native::GEN_THROW),
                ("@@iterator", ITER_SELF),
            ],
            None,
        );
        self.proto_of.insert(gen_proto, Value::heap(iter_root));
        let gen_tag = self.alloc_str("Generator".to_string());
        if let HeapObj::Object(m) = self.heap.get_mut(gen_proto) {
            m.define("@@toStringTag", gen_tag, tag_data);
        }
        self.gen_proto = gen_proto;
        // %AsyncIteratorPrototype% (@@asyncIterator returns self) and
        // %AsyncGeneratorPrototype% (next/return/throw returning Promises +
        // @@toStringTag "AsyncGenerator"), chained appropriately. Async-generator
        // instances delegate here.
        let async_iter_root = build(self, &[("@@asyncIterator", ITER_SELF)], None);
        self.proto_of.insert(async_iter_root, Value::heap(obj_proto));
        let asyncgen_proto = build(
            self,
            &[
                ("next", native::ASYNCGEN_NEXT),
                ("return", native::ASYNCGEN_RETURN),
                ("throw", native::ASYNCGEN_THROW),
            ],
            None,
        );
        self.proto_of.insert(asyncgen_proto, Value::heap(async_iter_root));
        let asyncgen_tag = self.alloc_str("AsyncGenerator".to_string());
        if let HeapObj::Object(m) = self.heap.get_mut(asyncgen_proto) {
            m.define("@@toStringTag", asyncgen_tag, tag_data);
        }
        self.asyncgen_proto = asyncgen_proto;
        // The `Iterator` constructor (abstract): prototype = %Iterator.prototype%,
        // static `Iterator.from`. name "Iterator", length 0.
        let iter_ctor = build(self, &[("from", ITER_FROM)], Some(iter_root));
        self.iterator_ctor = iter_ctor;
        let iter_name = self.alloc_str("Iterator".to_string());
        if let HeapObj::Object(m) = self.heap.get_mut(iter_ctor) {
            m.define("name", iter_name, tag_data);
            m.define("length", Value::num(0.0), tag_data);
        }
        // Built-in iterator prototypes inherit the helpers from %Iterator.prototype%
        // (with their own @@toStringTag so getPrototypeOf/toString stay correct).
        for (p, tag) in [
            (self.array_iter_proto, "Array Iterator"),
            (self.map_iter_proto, "Map Iterator"),
            (self.set_iter_proto, "Set Iterator"),
            (self.string_iter_proto, "String Iterator"),
        ] {
            self.proto_of.insert(p, Value::heap(iter_root));
            let tv = self.alloc_str(tag.to_string());
            if let HeapObj::Object(m) = self.heap.get_mut(p) {
                m.define("@@toStringTag", tv, tag_data);
            }
        }
        // %GeneratorFunction% / %AsyncFunction% / %AsyncGeneratorFunction%: the
        // dynamic-function constructors reached via a generator/async function's
        // .constructor. Built after `function_ctor`/`fn_proto` exist.
        let (g_ctor, g_proto) = self.build_dynamic_fn_intrinsic("GeneratorFunction");
        self.gen_fn_ctor = g_ctor;
        self.gen_fn_proto = g_proto;
        let (a_ctor, a_proto) = self.build_dynamic_fn_intrinsic("AsyncFunction");
        self.async_fn_ctor = a_ctor;
        self.async_fn_proto = a_proto;
        let (ag_ctor, ag_proto) = self.build_dynamic_fn_intrinsic("AsyncGeneratorFunction");
        self.asyncgen_fn_ctor = ag_ctor;
        self.asyncgen_fn_proto = ag_proto;
        // Link %GeneratorFunction.prototype%.prototype === %GeneratorPrototype% and
        // %AsyncGeneratorFunction.prototype%.prototype === %AsyncGeneratorPrototype%
        // ({writable:false, enumerable:false, configurable:true}) — this is how the
        // GeneratorPrototype / AsyncGeneratorPrototype tests reach those intrinsics
        // (getPrototypeOf(generatorFn).prototype).
        let proto_nw = PropAttr {
            writable: false,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        for (fn_proto, inst_proto) in
            [(self.gen_fn_proto, self.gen_proto), (self.asyncgen_fn_proto, self.asyncgen_proto)]
        {
            if fn_proto != 0 && inst_proto != 0 {
                let pv = Value::heap(inst_proto);
                if let HeapObj::Object(m) = self.heap.get_mut(fn_proto) {
                    m.define("prototype", pv, proto_nw);
                }
            }
        }
        // Default @@iterator: Map → entries, Set → values (alias to the same fn).
        let map_entries = match self.heap.get(map_proto) {
            HeapObj::Object(m) => m.get("entries"),
            _ => None,
        };
        let set_values = match self.heap.get(set_proto) {
            HeapObj::Object(m) => m.get("values"),
            _ => None,
        };
        let iter_attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        if let Some(v) = map_entries {
            if let HeapObj::Object(m) = self.heap.get_mut(map_proto) {
                m.define("@@iterator", v, iter_attr);
            }
        }
        if let Some(v) = set_values {
            if let HeapObj::Object(m) = self.heap.get_mut(set_proto) {
                m.define("@@iterator", v, iter_attr);
            }
        }
        // `Array.prototype[Symbol.iterator]` IS `Array.prototype.values` (same fn).
        let values_fn = match self.heap.get(self.arr_proto) {
            HeapObj::Object(m) => m.get("values"),
            _ => None,
        };
        if let Some(vf) = values_fn {
            let attr = PropAttr {
                writable: true,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            if let HeapObj::Object(m) = self.heap.get_mut(self.arr_proto) {
                m.define("@@iterator", vf, attr);
            }
            // Remember the default so array destructuring can detect a replaced
            // Array.prototype[Symbol.iterator] and switch to the iterator protocol.
            self.default_array_iter = vf;
        }
        self.weakmap_proto = weakmap_proto;
        self.weakset_proto = weakset_proto;
        self.weakref_proto = weakref_proto;
        self.finreg_proto = finreg_proto;
        // `X.prototype[Symbol.toStringTag] = "X"` (non-writable/enumerable,
        // configurable) — so `Object.prototype.toString.call(new Map())` is
        // "[object Map]", and the property itself is reflectable.
        let tag_attr = PropAttr {
            writable: false,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        for (proto, name) in [
            (map_proto, "Map"),
            (set_proto, "Set"),
            (promise_proto, "Promise"),
            (weakmap_proto, "WeakMap"),
            (weakset_proto, "WeakSet"),
            (weakref_proto, "WeakRef"),
            (finreg_proto, "FinalizationRegistry"),
        ] {
            let tag = self.alloc_str(name.to_string());
            if let HeapObj::Object(p) = self.heap.get_mut(proto) {
                p.define("@@toStringTag", tag, tag_attr);
            }
        }
        let weakmap_ctor = build(self, &[], Some(weakmap_proto));
        let weakset_ctor = build(self, &[], Some(weakset_proto));
        let weakref_ctor = build(self, &[], Some(weakref_proto));
        let finreg_ctor = build(self, &[], Some(finreg_proto));
        // Error hierarchy: `Error` + the 7 native subtypes. Each is a constructor
        // VALUE (is_ctor object with a `.prototype`) whose prototype carries own
        // `name`/`message`/`constructor` (+ `Error.prototype.toString`). Every error
        // instance — `new TypeError(x)` AND internal VM throws — links here via
        // `proto_of`, so `e.constructor === TypeError`, `e.name`, `e.toString()`,
        // and `e instanceof <ctor value>` all resolve through the chain.
        {
            // Error.prototype (chains to Object.prototype) carries toString.
            let err_proto = build(self, &[("toString", ERROR_TO_STRING)], None);
            self.proto_of.insert(err_proto, Value::heap(obj_proto));
            self.error_protos[0] = err_proto;
            // Subtype prototypes chain to Error.prototype.
            for k in 1..8usize {
                let p = build(self, &[], None);
                self.proto_of.insert(p, Value::heap(err_proto));
                self.error_protos[k] = p;
            }
            // Constructor function values (is_ctor, with a non-writable `.prototype`).
            for k in 0..8usize {
                let proto = self.error_protos[k];
                self.error_ctors[k] = build(self, &[], Some(proto));
            }
            // `Object.getPrototypeOf(TypeError) === Error`; `Error` → Function.prototype.
            let err_ctor = self.error_ctors[0];
            self.proto_of.insert(err_ctor, Value::heap(fn_proto));
            for k in 1..8usize {
                let c = self.error_ctors[k];
                self.proto_of.insert(c, Value::heap(err_ctor));
            }
            // Each prototype's own name/message/constructor (writable, non-enum,
            // configurable — matching the spec's Error.prototype descriptors).
            for k in 0..8usize {
                let name_v = self.alloc_str(native::ERROR_NAMES[k].to_string());
                let empty_v = self.alloc_str(String::new());
                let ctor_v = Value::heap(self.error_ctors[k]);
                let proto = self.error_protos[k];
                if let HeapObj::Object(m) = self.heap.get_mut(proto) {
                    m.define("name", name_v, method_attr);
                    m.define("message", empty_v, method_attr);
                    m.define("constructor", ctor_v, method_attr);
                }
            }
        }
        // `Symbol`: a callable-but-NOT-constructable function object (typeof
        // "function" via the type_of special case; `new Symbol()` throws because
        // it's not is_ctor). The well-known symbols (iterator/toPrimitive/…) are
        // real Symbol VALUES whose property-key form is the engine's `@@`-prefixed
        // key, so symbol-keyed access and iteration use one unified mechanism.
        {
            let symbol_proto = build(
                self,
                &[("toString", SYMBOL_TO_STRING), ("valueOf", SYMBOL_VALUE_OF)],
                None,
            );
            self.proto_of.insert(symbol_proto, Value::heap(obj_proto));
            self.symbol_proto = symbol_proto;
            let fn_attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            let for_v = Value::heap(self.heap.alloc(HeapObj::Native(SYMBOL_FOR)));
            let keyfor_v = Value::heap(self.heap.alloc(HeapObj::Native(SYMBOL_KEY_FOR)));
            let name_v = self.alloc_str("Symbol".to_string());
            let mut m = ObjMap::new();
            m.define("prototype", Value::heap(symbol_proto), proto_attr);
            m.define("for", for_v, method_attr);
            m.define("keyFor", keyfor_v, method_attr);
            m.define("name", name_v, fn_attr);
            m.define("length", Value::num(0.0), fn_attr);
            let symbol_ctor = self.heap.alloc(HeapObj::Object(m));
            self.symbol_ctor = symbol_ctor;
            // Symbol.prototype.constructor === Symbol.
            if let HeapObj::Object(p) = self.heap.get_mut(symbol_proto) {
                p.define("constructor", Value::heap(symbol_ctor), method_attr);
            }
            // Symbol.prototype: @@toPrimitive (returns the symbol), @@toStringTag
            // ("Symbol"), and `description` as a real accessor (so descriptor
            // introspection sees it; value access still uses the fast path).
            let to_prim = Value::heap(self.heap.alloc(HeapObj::Native(SYMBOL_TO_PRIMITIVE)));
            let desc_get = Value::heap(self.heap.alloc(HeapObj::Native(SYMBOL_DESCRIPTION_GET)));
            let tag = self.alloc_str("Symbol".to_string());
            let acc_attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: true,
                setter: Value::UNDEFINED,
            };
            if let HeapObj::Object(p) = self.heap.get_mut(symbol_proto) {
                p.define("@@toPrimitive", to_prim, fn_attr);
                p.define("@@toStringTag", tag, fn_attr);
                p.define("description", desc_get, acc_attr);
            }
            // Well-known symbols: real symbols (non-writable/enum/configurable own
            // props of Symbol), each with its fixed `@@`-prefixed key + description.
            for &(jsname, prop_key) in native::WELL_KNOWN_SYMBOLS {
                let desc = self.alloc_str(format!("Symbol.{jsname}"));
                let sym = self.make_named_symbol(desc, prop_key);
                if let HeapObj::Object(mm) = self.heap.get_mut(symbol_ctor) {
                    mm.define(jsname, sym, proto_attr);
                }
            }
        }
        // `BigInt`: callable-but-NOT-constructable (typeof "function"; new BigInt()
        // throws). BigInt(x) converts (compile-lowered to BigIntFrom); asIntN/asUintN
        // are statics; toString/valueOf on BigInt.prototype.
        {
            let bigint_proto = build(
                self,
                &[("toString", BIGINT_TO_STRING), ("valueOf", BIGINT_VALUE_OF)],
                None,
            );
            self.proto_of.insert(bigint_proto, Value::heap(obj_proto));
            self.bigint_proto = bigint_proto;
            let fn_attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            let asintn = Value::heap(self.heap.alloc(HeapObj::Native(BIGINT_AS_INTN)));
            let asuintn = Value::heap(self.heap.alloc(HeapObj::Native(BIGINT_AS_UINTN)));
            let name_v = self.alloc_str("BigInt".to_string());
            let mut m = ObjMap::new();
            m.define("prototype", Value::heap(bigint_proto), proto_attr);
            m.define("asIntN", asintn, method_attr);
            m.define("asUintN", asuintn, method_attr);
            m.define("name", name_v, fn_attr);
            m.define("length", Value::num(1.0), fn_attr);
            let bigint_ctor = self.heap.alloc(HeapObj::Object(m));
            self.bigint_ctor = bigint_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(bigint_proto) {
                p.define("constructor", Value::heap(bigint_ctor), method_attr);
            }
        }
        // `RegExp` (constructable; `new RegExp`/`/x/` literals lower to NewRegExp).
        // Instance accessors (source/flags/lastIndex/…) are computed in get_prop;
        // the prototype carries test/exec/toString.
        {
            let regexp_proto = build(
                self,
                &[
                    ("test", REGEXP_TEST),
                    ("exec", REGEXP_EXEC),
                    ("toString", REGEXP_TO_STRING),
                    ("compile", REGEXP_COMPILE),
                ],
                None,
            );
            self.proto_of.insert(regexp_proto, Value::heap(obj_proto));
            self.regexp_proto = regexp_proto;
            // The flag/source/flags accessors live on the prototype as getters
            // (spec: a RegExp instance has no own properties for these).
            let accessors: [(&str, u16); 10] = [
                ("source", REGEXP_GET_SOURCE),
                ("flags", REGEXP_GET_FLAGS),
                ("global", REGEXP_GET_GLOBAL),
                ("ignoreCase", REGEXP_GET_IGNORECASE),
                ("multiline", REGEXP_GET_MULTILINE),
                ("dotAll", REGEXP_GET_DOTALL),
                ("unicode", REGEXP_GET_UNICODE),
                ("unicodeSets", REGEXP_GET_UNICODESETS),
                ("sticky", REGEXP_GET_STICKY),
                ("hasIndices", REGEXP_GET_HASINDICES),
            ];
            for (name, getid) in accessors {
                let gv = Value::heap(self.heap.alloc(HeapObj::Native(getid)));
                let acc = PropAttr {
                    writable: false,
                    enumerable: false,
                    configurable: true,
                    accessor: true,
                    setter: Value::UNDEFINED,
                };
                if let HeapObj::Object(p) = self.heap.get_mut(regexp_proto) {
                    p.define(name, gv, acc);
                }
            }
            // %RegExpStringIteratorPrototype% — inherits %Iterator.prototype%,
            // carries the "RegExp String Iterator" toStringTag.
            let rsi_proto =
                build(self, &[("next", ITER_NEXT), ("@@iterator", ITER_SELF)], None);
            let iter_root = self.iterator_proto_root;
            self.proto_of.insert(rsi_proto, Value::heap(iter_root));
            let tag = self.alloc_str("RegExp String Iterator".to_string());
            let tag_attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            if let HeapObj::Object(p) = self.heap.get_mut(rsi_proto) {
                p.define("@@toStringTag", tag, tag_attr);
            }
            self.regexp_string_iter_proto = rsi_proto;
            // RegExp.prototype Symbol methods.
            for (key, nid) in [
                ("@@matchAll", REGEXP_SYM_MATCHALL),
                ("@@search", REGEXP_SYM_SEARCH),
                ("@@match", REGEXP_SYM_MATCH),
                ("@@split", REGEXP_SYM_SPLIT),
                ("@@replace", REGEXP_SYM_REPLACE),
            ] {
                let mv = Value::heap(self.heap.alloc(HeapObj::Native(nid)));
                if let HeapObj::Object(p) = self.heap.get_mut(regexp_proto) {
                    p.define(key, mv, method_attr);
                }
            }
            let regexp_ctor = build(self, &[("escape", REGEXP_ESCAPE)], Some(regexp_proto));
            self.regexp_ctor = regexp_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(regexp_proto) {
                p.define("constructor", Value::heap(regexp_ctor), method_attr);
            }
        }
        // TypedArrays: the %TypedArray% abstract base (its prototype holds the shared
        // methods), the 11 concrete kinds inheriting from it, plus ArrayBuffer and
        // DataView. `Object.getPrototypeOf(Int8Array) === %TypedArray%` and
        // `Int8Array.prototype.__proto__ === %TypedArray%.prototype`.
        {
            let fn_attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            let ta_methods: Vec<(&str, u16)> = native::TA_PROTO_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::TA_METHOD_BASE + i as u16))
                .collect();
            let ta_base_proto = build(self, &ta_methods, None);
            self.proto_of.insert(ta_base_proto, Value::heap(obj_proto));
            self.ta_base_proto = ta_base_proto;
            let ta_base_ctor = build(self, &[], Some(ta_base_proto));
            self.ta_base_ctor = ta_base_ctor;
            self.proto_of.insert(ta_base_ctor, Value::heap(fn_proto));
            let tname = self.alloc_str("TypedArray".to_string());
            let ta_from = Value::heap(self.heap.alloc(HeapObj::Native(TA_FROM)));
            let ta_of = Value::heap(self.heap.alloc(HeapObj::Native(TA_OF)));
            if let HeapObj::Object(m) = self.heap.get_mut(ta_base_ctor) {
                m.define("name", tname, fn_attr);
                m.define("length", Value::num(0.0), fn_attr);
                // %TypedArray%.from / .of — inherited by every concrete kind ctor.
                m.define("from", ta_from, method_attr);
                m.define("of", ta_of, method_attr);
            }
            if let HeapObj::Object(m) = self.heap.get_mut(ta_base_proto) {
                m.define("constructor", Value::heap(ta_base_ctor), method_attr);
            }
            // %TypedArray%.prototype accessor getters (buffer/byteLength/byteOffset/
            // length + the @@toStringTag) — real prototype accessors (like the RegExp
            // flag getters); instances still resolve them via the get_prop fast path.
            for (name, getid) in [
                ("buffer", TA_GET_BUFFER),
                ("byteLength", TA_GET_BYTELENGTH),
                ("byteOffset", TA_GET_BYTEOFFSET),
                ("length", TA_GET_LENGTH),
                ("@@toStringTag", TA_GET_TOSTRINGTAG),
            ] {
                let gv = Value::heap(self.heap.alloc(HeapObj::Native(getid)));
                let acc = PropAttr {
                    writable: false,
                    enumerable: false,
                    configurable: true,
                    accessor: true,
                    setter: Value::UNDEFINED,
                };
                if let HeapObj::Object(m) = self.heap.get_mut(ta_base_proto) {
                    m.define(name, gv, acc);
                }
            }
            for k in 0..native::TA_KINDS.len() {
                let size = native::TA_KINDS[k].1;
                let proto = build(self, &[], None);
                self.proto_of.insert(proto, Value::heap(ta_base_proto));
                self.ta_protos[k] = proto;
                let ctor = build(self, &[], Some(proto));
                self.proto_of.insert(ctor, Value::heap(ta_base_ctor));
                self.ta_ctors[k] = ctor;
                if let HeapObj::Object(m) = self.heap.get_mut(proto) {
                    m.define("constructor", Value::heap(ctor), method_attr);
                    m.define("BYTES_PER_ELEMENT", Value::num(size as f64), proto_attr);
                }
                if let HeapObj::Object(m) = self.heap.get_mut(ctor) {
                    m.define("BYTES_PER_ELEMENT", Value::num(size as f64), proto_attr);
                }
            }
            let arraybuffer_proto = build(
                self,
                &[
                    ("slice", ARRAYBUFFER_SLICE),
                    ("resize", ARRAYBUFFER_RESIZE),
                    ("transferToImmutable", native::ARRAYBUFFER_TRANSFER_IMMUTABLE),
                    ("sliceToImmutable", native::ARRAYBUFFER_SLICE_IMMUTABLE),
                    ("transfer", native::ARRAYBUFFER_TRANSFER),
                    ("transferToFixedLength", native::ARRAYBUFFER_TRANSFER_FIXED),
                ],
                None,
            );
            self.proto_of.insert(arraybuffer_proto, Value::heap(obj_proto));
            self.arraybuffer_proto = arraybuffer_proto;
            let arraybuffer_ctor = build(self, &[("isView", ARRAYBUFFER_ISVIEW)], Some(arraybuffer_proto));
            self.arraybuffer_ctor = arraybuffer_ctor;
            if let HeapObj::Object(m) = self.heap.get_mut(arraybuffer_proto) {
                m.define("constructor", Value::heap(arraybuffer_ctor), method_attr);
            }
            // SharedArrayBuffer: a parallel to ArrayBuffer (shared buffers reuse the
            // ArrayBuffer representation, flagged in `shared_buffers`). Reuses
            // `slice`; adds `grow` (grow-only), growable/byteLength/maxByteLength
            // accessor getters, and @@toStringTag "SharedArrayBuffer".
            let sab_proto =
                build(self, &[("slice", ARRAYBUFFER_SLICE), ("grow", native::SAB_GROW)], None);
            self.proto_of.insert(sab_proto, Value::heap(obj_proto));
            self.sab_proto = sab_proto;
            let sab_ctor = build(self, &[], Some(sab_proto));
            self.sab_ctor = sab_ctor;
            let sab_acc = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: true,
                setter: Value::UNDEFINED,
            };
            let sab_data_nw = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            for (g, &name) in native::SAB_GETTERS.iter().enumerate() {
                let getter =
                    Value::heap(self.heap.alloc(HeapObj::Native(native::SAB_GETTER_BASE + g as u16)));
                if let HeapObj::Object(m) = self.heap.get_mut(sab_proto) {
                    m.define(name, getter, sab_acc);
                }
            }
            let sab_tag = self.alloc_str("SharedArrayBuffer".to_string());
            if let HeapObj::Object(m) = self.heap.get_mut(sab_proto) {
                m.define("constructor", Value::heap(sab_ctor), method_attr);
                m.define("@@toStringTag", sab_tag, sab_data_nw);
            }
            let dv_methods: Vec<(&str, u16)> = native::DV_PROTO_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::DV_METHOD_BASE + i as u16))
                .collect();
            let dataview_proto = build(self, &dv_methods, None);
            self.proto_of.insert(dataview_proto, Value::heap(obj_proto));
            self.dataview_proto = dataview_proto;
            // Register byteLength/maxByteLength/resizable/detached (ArrayBuffer),
            // byteLength/byteOffset/length (%TypedArray%.prototype), and byteLength/
            // byteOffset (DataView) as real accessor properties, so their
            // descriptors expose a brand-checked `get`.
            let acc = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: true,
                setter: Value::UNDEFINED,
            };
            for (g, &(name, kind)) in native::BUFFER_GETTERS.iter().enumerate() {
                let getter = Value::heap(
                    self.heap.alloc(HeapObj::Native(native::BUFFER_GETTER_BASE + g as u16)),
                );
                let target = match kind {
                    0 => arraybuffer_proto,
                    1 => ta_base_proto,
                    _ => dataview_proto,
                };
                if let HeapObj::Object(m) = self.heap.get_mut(target) {
                    m.define(name, getter, acc);
                }
            }
            // `Proxy`: a constructor with no `.prototype`; `Proxy.revocable` static.
            let revocable = Value::heap(self.heap.alloc(HeapObj::Native(PROXY_REVOCABLE)));
            let mut pm = ObjMap::new();
            pm.define("revocable", revocable, method_attr);
            pm.is_ctor = true;
            self.proxy_ctor = self.heap.alloc(HeapObj::Object(pm));
            // `Temporal` namespace + `Temporal.Duration`.
            let dur_methods: Vec<(&str, u16)> = native::TEMPORAL_DURATION_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::TEMPORAL_M_BASE + i as u16))
                .collect();
            let duration_proto = build(self, &dur_methods, None);
            self.proto_of.insert(duration_proto, Value::heap(obj_proto));
            self.duration_proto = duration_proto;
            let dfrom = Value::heap(self.heap.alloc(HeapObj::Native(TEMPORAL_DURATION_FROM)));
            let dcompare = Value::heap(self.heap.alloc(HeapObj::Native(TEMPORAL_DURATION_COMPARE)));
            let dname = self.alloc_str("Duration".to_string());
            let dtag = self.alloc_str("Temporal.Duration".to_string());
            let mut dm = ObjMap::new();
            dm.define("prototype", Value::heap(duration_proto), proto_attr);
            dm.define("from", dfrom, method_attr);
            dm.define("compare", dcompare, method_attr);
            dm.define("name", dname, fn_attr);
            dm.define("length", Value::num(0.0), fn_attr);
            dm.is_ctor = true;
            let duration_ctor = self.heap.alloc(HeapObj::Object(dm));
            self.duration_ctor = duration_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(duration_proto) {
                p.define("constructor", Value::heap(duration_ctor), method_attr);
                p.define("@@toStringTag", dtag, fn_attr);
            }
            // Temporal.PlainDate
            let pd_methods: Vec<(&str, u16)> = native::PLAINDATE_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::PD_M_BASE + i as u16))
                .collect();
            let plaindate_proto = build(self, &pd_methods, None);
            self.proto_of.insert(plaindate_proto, Value::heap(obj_proto));
            self.plaindate_proto = plaindate_proto;
            let pdfrom = Value::heap(self.heap.alloc(HeapObj::Native(PLAINDATE_FROM)));
            let pdcompare = Value::heap(self.heap.alloc(HeapObj::Native(PLAINDATE_COMPARE)));
            let pdname = self.alloc_str("PlainDate".to_string());
            let pdtag = self.alloc_str("Temporal.PlainDate".to_string());
            let mut pdm = ObjMap::new();
            pdm.define("prototype", Value::heap(plaindate_proto), proto_attr);
            pdm.define("from", pdfrom, method_attr);
            pdm.define("compare", pdcompare, method_attr);
            pdm.define("name", pdname, fn_attr);
            pdm.define("length", Value::num(3.0), fn_attr);
            pdm.is_ctor = true;
            let plaindate_ctor = self.heap.alloc(HeapObj::Object(pdm));
            self.plaindate_ctor = plaindate_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(plaindate_proto) {
                p.define("constructor", Value::heap(plaindate_ctor), method_attr);
                p.define("@@toStringTag", pdtag, fn_attr);
            }
            // Temporal.PlainTime
            let pt_methods: Vec<(&str, u16)> = native::PLAINTIME_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::PT_M_BASE + i as u16))
                .collect();
            let plaintime_proto = build(self, &pt_methods, None);
            self.proto_of.insert(plaintime_proto, Value::heap(obj_proto));
            self.plaintime_proto = plaintime_proto;
            let ptfrom = Value::heap(self.heap.alloc(HeapObj::Native(PLAINTIME_FROM)));
            let ptcompare = Value::heap(self.heap.alloc(HeapObj::Native(PLAINTIME_COMPARE)));
            let ptname = self.alloc_str("PlainTime".to_string());
            let pttag = self.alloc_str("Temporal.PlainTime".to_string());
            let mut ptm = ObjMap::new();
            ptm.define("prototype", Value::heap(plaintime_proto), proto_attr);
            ptm.define("from", ptfrom, method_attr);
            ptm.define("compare", ptcompare, method_attr);
            ptm.define("name", ptname, fn_attr);
            ptm.define("length", Value::num(0.0), fn_attr);
            ptm.is_ctor = true;
            let plaintime_ctor = self.heap.alloc(HeapObj::Object(ptm));
            self.plaintime_ctor = plaintime_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(plaintime_proto) {
                p.define("constructor", Value::heap(plaintime_ctor), method_attr);
                p.define("@@toStringTag", pttag, fn_attr);
            }
            // Temporal.PlainDateTime
            let pdt_methods: Vec<(&str, u16)> = native::PLAINDATETIME_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::PDT_M_BASE + i as u16))
                .collect();
            let plaindatetime_proto = build(self, &pdt_methods, None);
            self.proto_of.insert(plaindatetime_proto, Value::heap(obj_proto));
            self.plaindatetime_proto = plaindatetime_proto;
            let pdtfrom = Value::heap(self.heap.alloc(HeapObj::Native(PLAINDATETIME_FROM)));
            let pdtcompare = Value::heap(self.heap.alloc(HeapObj::Native(PLAINDATETIME_COMPARE)));
            let pdtname = self.alloc_str("PlainDateTime".to_string());
            let pdttag = self.alloc_str("Temporal.PlainDateTime".to_string());
            let mut pdtm = ObjMap::new();
            pdtm.define("prototype", Value::heap(plaindatetime_proto), proto_attr);
            pdtm.define("from", pdtfrom, method_attr);
            pdtm.define("compare", pdtcompare, method_attr);
            pdtm.define("name", pdtname, fn_attr);
            pdtm.define("length", Value::num(3.0), fn_attr);
            pdtm.is_ctor = true;
            let plaindatetime_ctor = self.heap.alloc(HeapObj::Object(pdtm));
            self.plaindatetime_ctor = plaindatetime_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(plaindatetime_proto) {
                p.define("constructor", Value::heap(plaindatetime_ctor), method_attr);
                p.define("@@toStringTag", pdttag, fn_attr);
            }
            // Temporal.Instant
            let inst_methods: Vec<(&str, u16)> = native::INSTANT_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::INST_M_BASE + i as u16))
                .collect();
            let instant_proto = build(self, &inst_methods, None);
            self.proto_of.insert(instant_proto, Value::heap(obj_proto));
            self.instant_proto = instant_proto;
            let iname = self.alloc_str("Instant".to_string());
            let itag = self.alloc_str("Temporal.Instant".to_string());
            let mut im = ObjMap::new();
            im.define("prototype", Value::heap(instant_proto), proto_attr);
            for (n, id) in [
                ("from", INST_FROM),
                ("fromEpochMilliseconds", INST_FROM_EPOCH_MS),
                ("fromEpochNanoseconds", INST_FROM_EPOCH_NS),
                ("fromEpochSeconds", INST_FROM_EPOCH_SEC),
                ("fromEpochMicroseconds", INST_FROM_EPOCH_US),
                ("compare", INST_COMPARE),
            ] {
                let v = Value::heap(self.heap.alloc(HeapObj::Native(id)));
                im.define(n, v, method_attr);
            }
            im.define("name", iname, fn_attr);
            im.define("length", Value::num(1.0), fn_attr);
            im.is_ctor = true;
            let instant_ctor = self.heap.alloc(HeapObj::Object(im));
            self.instant_ctor = instant_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(instant_proto) {
                p.define("constructor", Value::heap(instant_ctor), method_attr);
                p.define("@@toStringTag", itag, fn_attr);
            }
            // Temporal.PlainYearMonth
            let pym_methods: Vec<(&str, u16)> = native::PLAINYEARMONTH_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::PYM_M_BASE + i as u16))
                .collect();
            let plainyearmonth_proto = build(self, &pym_methods, None);
            self.proto_of.insert(plainyearmonth_proto, Value::heap(obj_proto));
            self.plainyearmonth_proto = plainyearmonth_proto;
            let pymfrom = Value::heap(self.heap.alloc(HeapObj::Native(PLAINYEARMONTH_FROM)));
            let pymcompare = Value::heap(self.heap.alloc(HeapObj::Native(PLAINYEARMONTH_COMPARE)));
            let pymname = self.alloc_str("PlainYearMonth".to_string());
            let pymtag = self.alloc_str("Temporal.PlainYearMonth".to_string());
            let mut pymm = ObjMap::new();
            pymm.define("prototype", Value::heap(plainyearmonth_proto), proto_attr);
            pymm.define("from", pymfrom, method_attr);
            pymm.define("compare", pymcompare, method_attr);
            pymm.define("name", pymname, fn_attr);
            pymm.define("length", Value::num(2.0), fn_attr);
            pymm.is_ctor = true;
            let plainyearmonth_ctor = self.heap.alloc(HeapObj::Object(pymm));
            self.plainyearmonth_ctor = plainyearmonth_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(plainyearmonth_proto) {
                p.define("constructor", Value::heap(plainyearmonth_ctor), method_attr);
                p.define("@@toStringTag", pymtag, fn_attr);
            }
            // Temporal.PlainMonthDay
            let pmd_methods: Vec<(&str, u16)> = native::PLAINMONTHDAY_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::PMD_M_BASE + i as u16))
                .collect();
            let plainmonthday_proto = build(self, &pmd_methods, None);
            self.proto_of.insert(plainmonthday_proto, Value::heap(obj_proto));
            self.plainmonthday_proto = plainmonthday_proto;
            let pmdfrom = Value::heap(self.heap.alloc(HeapObj::Native(PLAINMONTHDAY_FROM)));
            let pmdname = self.alloc_str("PlainMonthDay".to_string());
            let pmdtag = self.alloc_str("Temporal.PlainMonthDay".to_string());
            let mut pmdm = ObjMap::new();
            pmdm.define("prototype", Value::heap(plainmonthday_proto), proto_attr);
            pmdm.define("from", pmdfrom, method_attr);
            pmdm.define("name", pmdname, fn_attr);
            pmdm.define("length", Value::num(2.0), fn_attr);
            pmdm.is_ctor = true;
            let plainmonthday_ctor = self.heap.alloc(HeapObj::Object(pmdm));
            self.plainmonthday_ctor = plainmonthday_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(plainmonthday_proto) {
                p.define("constructor", Value::heap(plainmonthday_ctor), method_attr);
                p.define("@@toStringTag", pmdtag, fn_attr);
            }
            // Temporal.ZonedDateTime
            let zdt_methods: Vec<(&str, u16)> = native::ZONEDDATETIME_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::ZDT_M_BASE + i as u16))
                .collect();
            let zoneddatetime_proto = build(self, &zdt_methods, None);
            self.proto_of.insert(zoneddatetime_proto, Value::heap(obj_proto));
            self.zoneddatetime_proto = zoneddatetime_proto;
            let zdtfrom = Value::heap(self.heap.alloc(HeapObj::Native(native::ZDT_FROM)));
            let zdtcompare = Value::heap(self.heap.alloc(HeapObj::Native(native::ZDT_COMPARE)));
            let zdtname = self.alloc_str("ZonedDateTime".to_string());
            let zdttag = self.alloc_str("Temporal.ZonedDateTime".to_string());
            let mut zdtm = ObjMap::new();
            zdtm.define("prototype", Value::heap(zoneddatetime_proto), proto_attr);
            zdtm.define("from", zdtfrom, method_attr);
            zdtm.define("compare", zdtcompare, method_attr);
            zdtm.define("name", zdtname, fn_attr);
            zdtm.define("length", Value::num(2.0), fn_attr);
            zdtm.is_ctor = true;
            let zoneddatetime_ctor = self.heap.alloc(HeapObj::Object(zdtm));
            self.zoneddatetime_ctor = zoneddatetime_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(zoneddatetime_proto) {
                p.define("constructor", Value::heap(zoneddatetime_ctor), method_attr);
                p.define("@@toStringTag", zdttag, fn_attr);
            }
            // Temporal.Now (a namespace object, not a constructor).
            let nowtag = self.alloc_str("Temporal.Now".to_string());
            let mut nown = ObjMap::new();
            for (n, id) in [
                ("instant", NOW_INSTANT),
                ("plainDateTimeISO", NOW_PLAINDATETIME_ISO),
                ("plainDateISO", NOW_PLAINDATE_ISO),
                ("plainTimeISO", NOW_PLAINTIME_ISO),
                ("timeZoneId", NOW_TIMEZONE_ID),
                ("zonedDateTimeISO", NOW_ZONEDDATETIME_ISO),
            ] {
                let v = Value::heap(self.heap.alloc(HeapObj::Native(id)));
                nown.define(n, v, method_attr);
            }
            nown.define("@@toStringTag", nowtag, fn_attr);
            let now_ns = self.heap.alloc(HeapObj::Object(nown));
            let mut tn = ObjMap::new();
            tn.define("Duration", Value::heap(duration_ctor), method_attr);
            tn.define("PlainDate", Value::heap(plaindate_ctor), method_attr);
            tn.define("PlainTime", Value::heap(plaintime_ctor), method_attr);
            tn.define("PlainDateTime", Value::heap(plaindatetime_ctor), method_attr);
            tn.define("Instant", Value::heap(instant_ctor), method_attr);
            tn.define("PlainYearMonth", Value::heap(plainyearmonth_ctor), method_attr);
            tn.define("PlainMonthDay", Value::heap(plainmonthday_ctor), method_attr);
            tn.define("ZonedDateTime", Value::heap(zoneddatetime_ctor), method_attr);
            tn.define("Now", Value::heap(now_ns), method_attr);
            self.temporal_ns = self.heap.alloc(HeapObj::Object(tn));
            // Register each Temporal type's field getters as accessor properties on
            // its prototype (the value still resolves via get_member's fast path;
            // this gives `getOwnPropertyDescriptor(Type.prototype, field).get` a real
            // function and brand-checks when invoked on the wrong receiver).
            let temporal_getter_sets: [(u32, &[&str]); 8] = [
                (self.duration_proto, native::TEMP_G_DURATION),
                (self.plaindate_proto, native::TEMP_G_PLAINDATE),
                (self.plaintime_proto, native::TEMP_G_PLAINTIME),
                (self.plaindatetime_proto, native::TEMP_G_PLAINDATETIME),
                (self.instant_proto, native::TEMP_G_INSTANT),
                (self.plainyearmonth_proto, native::TEMP_G_PLAINYEARMONTH),
                (self.plainmonthday_proto, native::TEMP_G_PLAINMONTHDAY),
                (self.zoneddatetime_proto, native::TEMP_G_ZONEDDATETIME),
            ];
            let getter_attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: true,
                setter: Value::UNDEFINED,
            };
            for (proto, fields) in temporal_getter_sets {
                for &name in fields {
                    let idx = native::TEMPORAL_GETTER_FIELDS
                        .iter()
                        .position(|f| *f == name)
                        .expect("getter field in union");
                    let gid = native::TEMPORAL_GETTER_BASE + idx as u16;
                    let gv = Value::heap(self.heap.alloc(HeapObj::Native(gid)));
                    if let HeapObj::Object(p) = self.heap.get_mut(proto) {
                        p.define(name, gv, getter_attr);
                    }
                }
            }
            // Shared `toLocaleString` (routes to toString) on every Temporal proto.
            for proto in [
                self.duration_proto,
                self.plaindate_proto,
                self.plaintime_proto,
                self.plaindatetime_proto,
                self.instant_proto,
                self.plainyearmonth_proto,
                self.plainmonthday_proto,
                self.zoneddatetime_proto,
            ] {
                let v = Value::heap(self.heap.alloc(HeapObj::Native(TEMPORAL_TO_LOCALE_STRING)));
                if let HeapObj::Object(p) = self.heap.get_mut(proto) {
                    p.define("toLocaleString", v, method_attr);
                }
            }
            // ── Intl namespace + service constructors ──
            let intl_services: Vec<(u8, &str, f64, Vec<(&str, u16)>, bool)> = vec![
                (
                    native::INTL_NUMBERFORMAT,
                    "NumberFormat",
                    0.0,
                    // `format` is an accessor (added below), not a data method.
                    vec![
                        ("formatToParts", INTL_NF_FORMAT_TO_PARTS),
                        ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                    ],
                    true,
                ),
                (
                    native::INTL_DATETIMEFORMAT,
                    "DateTimeFormat",
                    0.0,
                    vec![
                        ("formatToParts", INTL_DTF_FORMAT_TO_PARTS),
                        ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                    ],
                    true,
                ),
                (
                    native::INTL_COLLATOR,
                    "Collator",
                    0.0,
                    vec![("resolvedOptions", INTL_RESOLVED_OPTIONS)],
                    true,
                ),
                (
                    native::INTL_PLURALRULES,
                    "PluralRules",
                    0.0,
                    vec![
                        ("select", INTL_PLURAL_SELECT),
                        ("selectRange", INTL_PLURAL_SELECT_RANGE),
                        ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                    ],
                    true,
                ),
                (
                    native::INTL_LISTFORMAT,
                    "ListFormat",
                    0.0,
                    vec![
                        ("format", INTL_LIST_FORMAT),
                        ("formatToParts", INTL_LIST_FORMAT_TO_PARTS),
                        ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                    ],
                    true,
                ),
                (
                    native::INTL_RELATIVETIMEFORMAT,
                    "RelativeTimeFormat",
                    0.0,
                    vec![
                        ("format", INTL_RTF_FORMAT),
                        ("formatToParts", INTL_RTF_FORMAT_TO_PARTS),
                        ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                    ],
                    true,
                ),
                (
                    native::INTL_SEGMENTER,
                    "Segmenter",
                    0.0,
                    vec![
                        ("segment", INTL_SEGMENTER_SEGMENT),
                        ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                    ],
                    true,
                ),
                (
                    native::INTL_LOCALE,
                    "Locale",
                    1.0,
                    vec![
                        ("toString", INTL_LOCALE_TOSTRING),
                        ("maximize", INTL_LOCALE_MAXIMIZE),
                        ("minimize", INTL_LOCALE_MINIMIZE),
                    ],
                    false,
                ),
                (
                    native::INTL_DISPLAYNAMES,
                    "DisplayNames",
                    2.0,
                    vec![
                        ("of", INTL_DISPLAYNAMES_OF),
                        ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                    ],
                    true,
                ),
                (
                    native::INTL_DURATIONFORMAT,
                    "DurationFormat",
                    0.0,
                    vec![
                        ("format", INTL_DURATION_FORMAT),
                        ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                    ],
                    true,
                ),
            ];
            let mut intl_ns_map = ObjMap::new();
            for (kind, name, len, methods, slo) in intl_services {
                let proto = build(self, &methods, None);
                self.proto_of.insert(proto, Value::heap(obj_proto));
                self.intl_protos[kind as usize] = proto;
                let statics: Vec<(&str, u16)> = if slo {
                    vec![("supportedLocalesOf", INTL_SUPPORTED_LOCALES_OF)]
                } else {
                    vec![]
                };
                let ctor = build(self, &statics, Some(proto));
                self.intl_ctors[kind as usize] = ctor;
                let nm = self.alloc_str(name.to_string());
                let tag = self.alloc_str(format!("Intl.{name}"));
                if let HeapObj::Object(m) = self.heap.get_mut(ctor) {
                    m.define("name", nm, fn_attr);
                    m.define("length", Value::num(len), fn_attr);
                }
                if let HeapObj::Object(p) = self.heap.get_mut(proto) {
                    p.define("constructor", Value::heap(ctor), method_attr);
                    p.define("@@toStringTag", tag, fn_attr);
                }
                intl_ns_map.define(name, Value::heap(ctor), method_attr);
            }
            // Intl.Locale.prototype subtag getters (accessors reading the instance).
            let accessor_attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: true,
                setter: Value::UNDEFINED,
            };
            let locale_proto = self.intl_protos[native::INTL_LOCALE as usize];
            for (i, gname) in native::LOCALE_ACCESSORS.iter().enumerate() {
                let getter =
                    Value::heap(self.heap.alloc(HeapObj::Native(INTL_LOCALE_GET_BASE + i as u16)));
                if let HeapObj::Object(p) = self.heap.get_mut(locale_proto) {
                    p.define(gname, getter, accessor_attr);
                }
            }
            // NumberFormat/DateTimeFormat `format` + Collator `compare`: spec says
            // these are accessors returning a function bound to the instance.
            for (k, name, gid) in [
                (native::INTL_NUMBERFORMAT, "format", INTL_NF_FORMAT_GET),
                (native::INTL_DATETIMEFORMAT, "format", INTL_DTF_FORMAT_GET),
                (native::INTL_COLLATOR, "compare", INTL_COLLATOR_COMPARE_GET),
            ] {
                let getter = Value::heap(self.heap.alloc(HeapObj::Native(gid)));
                let p = self.intl_protos[k as usize];
                if let HeapObj::Object(o) = self.heap.get_mut(p) {
                    o.define(name, getter, accessor_attr);
                }
            }
            let gcl = Value::heap(self.heap.alloc(HeapObj::Native(INTL_GET_CANONICAL_LOCALES)));
            intl_ns_map.define("getCanonicalLocales", gcl, method_attr);
            let svo = Value::heap(self.heap.alloc(HeapObj::Native(INTL_SUPPORTED_VALUES_OF)));
            intl_ns_map.define("supportedValuesOf", svo, method_attr);
            let intltag = self.alloc_str("Intl".to_string());
            intl_ns_map.define("@@toStringTag", intltag, fn_attr);
            self.intl_ns = self.heap.alloc(HeapObj::Object(intl_ns_map));
            let dataview_ctor = build(self, &[], Some(dataview_proto));
            self.dataview_ctor = dataview_ctor;
            if let HeapObj::Object(m) = self.heap.get_mut(dataview_proto) {
                m.define("constructor", Value::heap(dataview_ctor), method_attr);
            }
        }
        // Wire each built-in prototype's `constructor` back to its constructor
        // (`Array.prototype.constructor === Array`, `p.constructor === Promise`,
        // `(5).constructor === Number`, …) — a fundamental invariant assertions
        // rely on. Writable, non-enumerable, configurable (the spec descriptor).
        for (proto, ctor) in [
            (self.obj_proto, object_ctor),
            (self.arr_proto, array_ctor),
            (self.fn_proto, function_ctor),
            (self.str_proto, string_ctor),
            (self.num_proto, number_ctor),
            (self.bool_proto, boolean_ctor),
            (self.set_proto, set_ctor),
            (self.map_proto, map_ctor),
            (self.date_proto, date_ctor),
            (self.promise_proto, promise_ctor),
            (self.weakmap_proto, weakmap_ctor),
            (self.weakset_proto, weakset_ctor),
            (self.weakref_proto, weakref_ctor),
            (self.finreg_proto, finreg_ctor),
        ] {
            if proto != 0 {
                let cv = Value::heap(ctor);
                if let HeapObj::Object(m) = self.heap.get_mut(proto) {
                    m.define("constructor", cv, method_attr);
                }
            }
        }
        // `JSON`: a namespace object. The direct `JSON.parse(x)`/`stringify(x)` call
        // forms are compile-lowered to ops; these back the value form + reflection.
        let json_ctor = build(
            self,
            &[
                ("parse", JSON_PARSE),
                ("stringify", JSON_STRINGIFY),
                ("rawJSON", JSON_RAW_JSON),
                ("isRawJSON", JSON_IS_RAW_JSON),
            ],
            None,
        );
        {
            // JSON[Symbol.toStringTag] = "JSON" (so Object.prototype.toString is
            // "[object JSON]"); non-writable/enumerable, configurable.
            let jtag = self.alloc_str("JSON".to_string());
            let attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            if let HeapObj::Object(m) = self.heap.get_mut(json_ctor) {
                m.define("@@toStringTag", jtag, attr);
            }
        }
        // `Math`: a namespace object — the 8 constants (non-w/e/c) + the methods as
        // first-class values + `random`. Direct `Math.abs(x)` is compile-lowered to
        // MathOp; this backs the value form + reflection.
        let math_ctor = {
            let mut methods: Vec<(&str, u16)> = native::MATH_METHODS
                .iter()
                .enumerate()
                .map(|(i, &(name, _, _))| (name, native::MATH_METHOD_BASE + i as u16))
                .collect();
            methods.push(("random", MATH_RANDOM));
            let idx = build(self, &methods, None);
            let consts: &[(&str, f64)] = &[
                ("E", std::f64::consts::E),
                ("LN10", std::f64::consts::LN_10),
                ("LN2", std::f64::consts::LN_2),
                ("LOG10E", std::f64::consts::LOG10_E),
                ("LOG2E", std::f64::consts::LOG2_E),
                ("PI", std::f64::consts::PI),
                ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
                ("SQRT2", std::f64::consts::SQRT_2),
            ];
            if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                for &(n, v) in consts {
                    m.define(n, Value::num(v), proto_attr);
                }
            }
            idx
        };
        // `Atomics`: a namespace object (typeof "object", not a constructor) whose
        // methods perform atomic read-modify-write on integer TypedArrays. Single
        // -threaded, so the ops are plain RMW; wait/notify have no real waiters.
        let atomics_ns = {
            let methods: Vec<(&str, u16)> = native::ATOMICS_METHODS
                .iter()
                .enumerate()
                .map(|(i, &(name, _))| (name, native::ATOMICS_BASE + i as u16))
                .collect();
            let idx = build(self, &methods, None);
            self.proto_of.insert(idx, Value::heap(obj_proto));
            let tag = self.alloc_str("Atomics".to_string());
            let data_nw = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                m.define("@@toStringTag", tag, data_nw);
            }
            idx
        };
        // DisposableStack (ES2026 explicit resource management): use/adopt/defer/
        // dispose/move methods, a `disposed` accessor, [Symbol.dispose] (= dispose),
        // and @@toStringTag "DisposableStack".
        {
            let methods: &[(&str, u16)] = &[
                ("use", native::DISPOSABLE_USE),
                ("adopt", native::DISPOSABLE_ADOPT),
                ("defer", native::DISPOSABLE_DEFER),
                ("dispose", native::DISPOSABLE_DISPOSE),
                ("move", native::DISPOSABLE_MOVE),
            ];
            let p = build(self, methods, None);
            self.proto_of.insert(p, Value::heap(obj_proto));
            let getter =
                Value::heap(self.heap.alloc(HeapObj::Native(native::DISPOSABLE_DISPOSED_GET)));
            let dispose_fn =
                Value::heap(self.heap.alloc(HeapObj::Native(native::DISPOSABLE_DISPOSE)));
            let acc = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: true,
                setter: Value::UNDEFINED,
            };
            let data_nw = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            let tag = self.alloc_str("DisposableStack".to_string());
            if let HeapObj::Object(m) = self.heap.get_mut(p) {
                m.define("disposed", getter, acc);
                m.define("@@dispose", dispose_fn, method_attr);
                m.define("@@toStringTag", tag, data_nw);
            }
            self.disposablestack_proto = p;
            let ctor = build(self, &[], Some(p));
            self.disposablestack_ctor = ctor;
            if let HeapObj::Object(m) = self.heap.get_mut(p) {
                m.define("constructor", Value::heap(ctor), method_attr);
            }
        }
        // AsyncDisposableStack: like DisposableStack but `disposeAsync` (returns a
        // Promise) instead of `dispose`, and [Symbol.asyncDispose].
        {
            let methods: &[(&str, u16)] = &[
                ("use", native::DISPOSABLE_USE),
                ("adopt", native::DISPOSABLE_ADOPT),
                ("defer", native::DISPOSABLE_DEFER),
                ("disposeAsync", native::DISPOSABLE_DISPOSE_ASYNC),
                ("move", native::DISPOSABLE_MOVE),
            ];
            let p = build(self, methods, None);
            self.proto_of.insert(p, Value::heap(obj_proto));
            let getter =
                Value::heap(self.heap.alloc(HeapObj::Native(native::DISPOSABLE_DISPOSED_GET)));
            let dispose_fn =
                Value::heap(self.heap.alloc(HeapObj::Native(native::DISPOSABLE_DISPOSE_ASYNC)));
            let acc = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: true,
                setter: Value::UNDEFINED,
            };
            let data_nw = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            let tag = self.alloc_str("AsyncDisposableStack".to_string());
            if let HeapObj::Object(m) = self.heap.get_mut(p) {
                m.define("disposed", getter, acc);
                m.define("@@asyncDispose", dispose_fn, method_attr);
                m.define("@@toStringTag", tag, data_nw);
            }
            self.asyncdisposablestack_proto = p;
            let ctor = build(self, &[], Some(p));
            self.asyncdisposablestack_ctor = ctor;
            if let HeapObj::Object(m) = self.heap.get_mut(p) {
                m.define("constructor", Value::heap(ctor), method_attr);
            }
        }
        // SuppressedError (ES2026): an error carrying `error` + `suppressed`. Its
        // prototype chains to %Error.prototype% (so `instanceof Error` holds) and
        // the ctor's [[Prototype]] is %Error%.
        {
            let p = build(self, &[], None);
            if self.error_protos[0] != 0 {
                self.proto_of.insert(p, Value::heap(self.error_protos[0]));
            }
            let name_v = self.alloc_str("SuppressedError".to_string());
            let empty_v = self.alloc_str(String::new());
            if let HeapObj::Object(m) = self.heap.get_mut(p) {
                m.define("name", name_v, method_attr);
                m.define("message", empty_v, method_attr);
            }
            self.suppressederror_proto = p;
            let ctor = build(self, &[], Some(p));
            if self.error_ctors[0] != 0 {
                self.proto_of.insert(ctor, Value::heap(self.error_ctors[0]));
            }
            self.suppressederror_ctor = ctor;
            if let HeapObj::Object(m) = self.heap.get_mut(p) {
                m.define("constructor", Value::heap(ctor), method_attr);
            }
        }
        // ShadowRealm: evaluate + importValue + @@toStringTag "ShadowRealm".
        {
            let p = build(
                self,
                &[
                    ("evaluate", native::SHADOWREALM_EVALUATE),
                    ("importValue", native::SHADOWREALM_IMPORTVALUE),
                ],
                None,
            );
            self.proto_of.insert(p, Value::heap(obj_proto));
            let tag = self.alloc_str("ShadowRealm".to_string());
            let data_nw = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            if let HeapObj::Object(m) = self.heap.get_mut(p) {
                m.define("@@toStringTag", tag, data_nw);
            }
            self.shadowrealm_proto = p;
            let ctor = build(self, &[], Some(p));
            self.shadowrealm_ctor = ctor;
            if let HeapObj::Object(m) = self.heap.get_mut(p) {
                m.define("constructor", Value::heap(ctor), method_attr);
            }
        }
        // Bare global functions as first-class values (the call form is GlobalFn).
        let parse_int_fn = self.heap.alloc(HeapObj::Native(GLOBAL_PARSE_INT));
        let parse_float_fn = self.heap.alloc(HeapObj::Native(GLOBAL_PARSE_FLOAT));
        let is_nan_fn = self.heap.alloc(HeapObj::Native(GLOBAL_IS_NAN));
        let is_finite_fn = self.heap.alloc(HeapObj::Native(GLOBAL_IS_FINITE));
        let eval_fn = self.heap.alloc(HeapObj::Native(GLOBAL_EVAL));
        // `globalThis`: an empty Object whose property access is routed to the
        // global slots by name (see get_prop/set_prop/has_own_property).
        let global_this = self.heap.alloc(HeapObj::Object(ObjMap::new()));
        self.global_this = global_this;
        // The test262 `$262` host object: { global, detachArrayBuffer, gc }.
        let d262_detach = Value::heap(self.heap.alloc(HeapObj::Native(DOLLAR262_DETACH)));
        let d262_gc = Value::heap(self.heap.alloc(HeapObj::Native(DOLLAR262_GC)));
        let mut d262 = ObjMap::new();
        d262.define("global", Value::heap(global_this), method_attr);
        d262.define("detachArrayBuffer", d262_detach, method_attr);
        d262.define("gc", d262_gc, method_attr);
        self.dollar262 = self.heap.alloc(HeapObj::Object(d262));
        // Inject into the reserved global slots (collect first to end the program
        // borrow before mutating `self.globals`).
        // Every builtin global NAME → its heap value, built ONCE and recorded in
        // `builtin_globals` regardless of whether the running program referenced
        // it — so eval'd code can resolve a builtin the program never named.
        let mut all: Vec<(&str, u32)> = vec![
            ("Object", object_ctor),
            ("Array", array_ctor),
            ("Function", function_ctor),
            ("String", string_ctor),
            ("Number", number_ctor),
            ("Set", set_ctor),
            ("Map", map_ctor),
            ("Boolean", boolean_ctor),
            ("Date", date_ctor),
            ("Promise", promise_ctor),
            ("Reflect", reflect_ctor),
            ("JSON", json_ctor),
            ("Math", math_ctor),
            ("WeakMap", weakmap_ctor),
            ("WeakSet", weakset_ctor),
            ("WeakRef", weakref_ctor),
            ("FinalizationRegistry", finreg_ctor),
            ("Error", self.error_ctors[0]),
            ("TypeError", self.error_ctors[1]),
            ("RangeError", self.error_ctors[2]),
            ("SyntaxError", self.error_ctors[3]),
            ("ReferenceError", self.error_ctors[4]),
            ("EvalError", self.error_ctors[5]),
            ("URIError", self.error_ctors[6]),
            ("AggregateError", self.error_ctors[7]),
            ("Symbol", self.symbol_ctor),
            ("BigInt", self.bigint_ctor),
            ("RegExp", self.regexp_ctor),
            ("ArrayBuffer", self.arraybuffer_ctor),
            ("SharedArrayBuffer", self.sab_ctor),
            ("DataView", self.dataview_ctor),
            ("Proxy", self.proxy_ctor),
            ("Iterator", self.iterator_ctor),
            ("Temporal", self.temporal_ns),
            ("Intl", self.intl_ns),
            ("Atomics", atomics_ns),
            ("DisposableStack", self.disposablestack_ctor),
            ("AsyncDisposableStack", self.asyncdisposablestack_ctor),
            ("SuppressedError", self.suppressederror_ctor),
            ("ShadowRealm", self.shadowrealm_ctor),
            ("parseInt", parse_int_fn),
            ("parseFloat", parse_float_fn),
            ("isNaN", is_nan_fn),
            ("isFinite", is_finite_fn),
            ("eval", eval_fn),
            ("globalThis", global_this),
            ("$262", self.dollar262),
        ];
        // The 11 TypedArray constructors (Int8Array … BigUint64Array).
        for (k, t) in native::TA_KINDS.iter().enumerate() {
            all.push((t.0, self.ta_ctors[k]));
        }
        for &(name, v) in &all {
            // Constructor globals expose own `name`/`length` like any function
            // ({writable:false, enumerable:false, configurable:true}). Namespaces
            // (Reflect/Math/JSON, is_ctor==false) don't. Applied to EVERY builtin
            // (not just referenced ones) so eval sees correct `RangeError.name` etc.
            if matches!(self.heap.get(v), HeapObj::Object(m) if m.is_ctor) {
                let len = match name {
                    "Date" => 7.0,
                    "Map" | "Set" | "WeakMap" | "WeakSet" | "Iterator"
                    | "DisposableStack" | "AsyncDisposableStack" | "ShadowRealm" => 0.0,
                    "AggregateError" => 2.0,  // (errors, message?)
                    "SuppressedError" => 3.0, // (error, suppressed, message?)
                    "RegExp" => 2.0,          // (pattern, flags)
                    "Proxy" => 2.0,           // (target, handler)
                    // TypedArray ctors take (length | buffer, byteOffset, length).
                    n if native::TA_KINDS.iter().any(|t| t.0 == n) => 3.0,
                    _ => 1.0, // Object/Array/Function/String/Number/Boolean/Promise/Error+subtypes/ArrayBuffer/DataView
                };
                let nm = self.alloc_str(name.to_string());
                let fn_attr = PropAttr {
                    writable: false,
                    enumerable: false,
                    configurable: true,
                    accessor: false,
                    setter: Value::UNDEFINED,
                };
                if let HeapObj::Object(m) = self.heap.get_mut(v) {
                    m.define("length", Value::num(len), fn_attr);
                    m.define("name", nm, fn_attr);
                }
            }
            self.builtin_globals.insert(name.to_string(), v);
        }
        // Inject into the program's reserved global slots (collect first to end the
        // program borrow before mutating `self.globals`).
        let mut sets: Vec<(usize, u32)> = Vec::new();
        for (slot, name) in self.program.global_names.iter().enumerate() {
            if let Some(&v) = self.builtin_globals.get(name.as_str()) {
                sets.push((slot, v));
            }
        }
        for (slot, v) in sets {
            if slot < self.globals.len() {
                self.globals[slot] = Value::heap(v);
            }
        }
        // `get [Symbol.species]` (a shared getter returning `this`) on every
        // species-aware constructor — used by slice/map/etc. and required by the
        // Symbol.species descriptor tests. Globals are assigned above, so
        // global_by_name resolves the named ctors here.
        {
            let species_get = Value::heap(self.heap.alloc(HeapObj::Native(SPECIES_GET)));
            let sp_acc = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: true,
                setter: Value::UNDEFINED,
            };
            let mut ctors: Vec<u32> =
                vec![self.ta_base_ctor, self.regexp_ctor, self.arraybuffer_ctor];
            for n in ["Array", "Map", "Set", "Promise"] {
                if let Some(v) = self.global_by_name(n) {
                    if v.is_heap() {
                        ctors.push(v.heap_index());
                    }
                }
            }
            for c in ctors {
                if c != 0 {
                    if let HeapObj::Object(m) = self.heap.get_mut(c) {
                        m.define("@@species", species_get, sp_acc);
                    }
                }
            }
        }
    }

}
