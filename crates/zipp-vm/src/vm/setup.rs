#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
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
        // own property; return it so OrdinaryHasInstance (`x instanceof Map`) works.
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
        // An explicit `fn.prototype = value` wins. A non-object value means there is
        // no usable prototype OBJECT (OrdinaryCreateFromConstructor then falls back to
        // the intrinsic default), so report None.
        // Empty in nearly every program: skip its SipHash (`instanceof`
        // asks this at every class level it walks).
        if !self.fn_proto_override.is_empty() {
            if let Some(&v) = self.fn_proto_override.get(&idx) {
                return if v.is_heap() && self.is_object_value(v) {
                    Some(v)
                } else {
                    None
                };
            }
        }
        if let Some(&p) = self.prototypes.get(&idx) {
            return Some(Value::heap(p));
        }
        // Collect own methods + accessors first (ends the immutable heap borrow
        // before alloc).
        #[allow(clippy::type_complexity)]
        let (methods, getters, setters, proto_order): (
            Vec<(String, Value)>,
            Vec<(String, Value)>,
            Vec<(String, Value)>,
            Vec<String>,
        ) = match self.heap.get(idx) {
            HeapObj::Class(c) => (
                c.methods.iter().map(|(k, v)| (k.clone(), *v)).collect(),
                c.getters.iter().map(|(k, v)| (k.clone(), *v)).collect(),
                c.setters.iter().map(|(k, v)| (k.clone(), *v)).collect(),
                c.proto_order.clone(),
            ),
            _ => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        };
        // A derived class's prototype chains to its parent's prototype (so a
        // subclass instance is `instanceof` the parent — including built-in
        // parents like Error/Array — and inherits the parent's prototype methods
        // through the chain). Member resolution uses the class `extends` chain's
        // tables only while they still describe these objects
        // (`ClassData::proto_dirty`), and this chain otherwise.
        let parent: Option<u32> = match self.heap.get(idx) {
            HeapObj::Class(c) => c.parent,
            _ => None,
        };
        // Methods and the constructor back-reference are NON-enumerable
        // (writable + configurable), matching ES `class`/function semantics that
        // test262's verifyProperty checks.
        let nonenum = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        // A generator / async-generator function's prototype object owns NO
        // `constructor` property (MakeMethod-less ordinary object per spec).
        let is_gen_fn = match self.heap.get(idx) {
            HeapObj::Func(id) => self.func(*id as usize).is_generator,
            HeapObj::Closure { func, .. } => self.func(*func as usize).is_generator,
            _ => false,
        };
        let mut map = ObjMap::new();
        // `constructor` is installed at prototype creation (before user methods), so
        // it is the FIRST string key — getOwnPropertyNames lists it after any integer
        // keys but before the declared methods (e.g. ['1','2','constructor','a','c']).
        if !is_gen_fn {
            map.define("constructor", obj, nonenum);
        }
        // Accessors become real accessor properties (getter in `vals`, setter in
        // `attr.setter`) so getOwnPropertyDescriptor / getOwnPropertyNames /
        // enumeration reflect them; non-enumerable + configurable per spec. A
        // get+set pair on one key merges into a single accessor property.
        let acc_attr = PropAttr {
            writable: false,
            enumerable: false,
            configurable: true,
            accessor: true,
            setter: Value::UNDEFINED,
        };
        // Walk the class body's SOURCE order (`proto_order`), not method-list
        // then accessor-list: the three lists are grouped by kind, so replaying
        // them in turn reported `class C { get g(){} m(){} }` as
        // ["constructor","m","g"] where OrdinaryOwnPropertyKeys requires
        // ["constructor","g","m"]. `proto_order` carries one entry per public
        // instance key, at its FIRST definition's position; the compiler
        // (`drop_member`) has already made a key belong to exactly one of
        // methods / (getters ∪ setters), so the classification below is
        // unambiguous.
        // `done` (not `map.pos`) is what stops the safety net below re-running a
        // key: `class C { ['constructor'](){} }` legally REDEFINES the
        // constructor back-reference installed above, so "already in the map"
        // must not mean "skip"
        // (language/computed-property-names/class/method/constructor.js).
        let mut done: Vec<&str> = Vec::new();
        let define_key = |map: &mut ObjMap, k: &str| {
            if k.starts_with('#') {
                return;
            }
            if let Some((_, v)) = methods.iter().find(|(n, _)| n == k) {
                map.define(k, *v, nonenum);
                return;
            }
            let g = getters.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
            let s = setters.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
            if g.is_none() && s.is_none() {
                return; // an unresolved computed-key placeholder
            }
            let mut a = acc_attr;
            a.setter = s.unwrap_or(Value::UNDEFINED);
            map.define(k, g.unwrap_or(Value::UNDEFINED), a);
        };
        for k in &proto_order {
            done.push(k.as_str());
            define_key(&mut map, k);
        }
        // Safety net: anything the order list missed (a class value built by a
        // path that never filled `proto_order`) still becomes a property, in the
        // old kind-grouped order. Keys the loop above already handled are
        // skipped, so this cannot reorder or duplicate them.
        for (k, _) in methods.iter().chain(getters.iter()).chain(setters.iter()) {
            if done.iter().any(|d| *d == k.as_str()) {
                continue;
            }
            done.push(k.as_str());
            define_key(&mut map, k);
        }
        let is_class = matches!(self.heap.get(idx), HeapObj::Class(_));
        if is_class {
            // The class's member tables describe this object only until it is
            // mutated; flag it so every mutation reaches a path that can say so.
            map.mark_class_proto();
        }
        let p = self.setup_object(map);
        self.prototypes.insert(idx, p);
        self.prototypes_fresh.push(p);
        if is_class {
            self.class_proto_owner.insert(p, idx);
        }
        // A function born in a $262.createRealm child gets a `.prototype` whose
        // [[Prototype]] is the CHILD's %Object.prototype% (OrdinaryFunctionCreate
        // runs in the function's realm) — `new fn() instanceof other.Object`.
        // The extends-null / generator links below take precedence.
        if !self.realm_global_objs.is_empty() {
            let r = self.get_function_realm(obj);
            if r != 0 {
                if let Some(&rp) = self
                    .realms
                    .get(r as usize)
                    .and_then(|m| m.get(&self.obj_proto))
                {
                    self.proto_of.insert(p, Value::heap(rp));
                    self.obj_realm.insert(p, r);
                }
            }
        }
        // `class C extends null {}`: the prototype object's [[Prototype]] is
        // null (chain walks stop on the non-heap entry).
        if matches!(self.heap.get(idx), HeapObj::Class(c) if c.extends_null) {
            self.proto_of.insert(p, Value::NULL);
        }
        // A generator / async-generator FUNCTION's `.prototype` object chains to
        // %GeneratorPrototype% / %AsyncGeneratorPrototype% (whose own [[Prototype]]
        // is %IteratorPrototype% / %AsyncIteratorPrototype%), so
        // `getPrototypeOf(getPrototypeOf(g.prototype))` reaches the iterator root —
        // matching the spec and unblocking the AsyncIteratorPrototype tests.
        let fn_id = match self.heap.get(idx) {
            HeapObj::Func(id) => Some(*id as usize),
            HeapObj::Closure { func, .. } => Some(*func as usize),
            _ => None,
        };
        if let Some(fid) = fn_id {
            let fp = self.func(fid);
            let (is_gen, is_async) = (fp.is_generator, fp.is_async);
            if is_gen {
                let mut gp = if is_async {
                    self.asyncgen_proto
                } else {
                    self.gen_proto
                };
                // A REALM-BORN generator function's `prototype` chains to the
                // realm's image of %GeneratorPrototype% / %AsyncGeneratorPrototype%
                // (create_realm's hidden intrinsics), so its instances reach the
                // realm's %Object.prototype% (`gen instanceof other.Object`).
                if gp != 0 && !self.realm_global_objs.is_empty() {
                    let r = self.get_function_realm(obj);
                    if r != 0 {
                        if let Some(&rp) = self.realms.get(r as usize).and_then(|m| m.get(&gp)) {
                            gp = rp;
                        }
                    }
                }
                if gp != 0 {
                    self.proto_of.insert(p, Value::heap(gp));
                }
            }
        }
        // Link the prototype chain to the parent's prototype (a parent class's
        // own prototype, or a built-in parent ctor's `.prototype`).
        if let Some(par) = parent {
            let pp = match self.prototype_of(Value::heap(par)) {
                Some(pp) => Some(pp),
                // `class C extends someProxy`: ClassDefinitionEvaluation's
                // protoParent is `Get(superclass, "prototype")`, which for a Proxy
                // means its `get` trap (or the trap-less forward to the target).
                // `prototype_of` only understands Func/Closure/Class/is_ctor
                // objects, so the link was silently dropped and C.prototype chained
                // to %Object.prototype% — instances inherited nothing from the
                // proxied base, and `super.x` in a method resolved against the
                // wrong object (staging/sm/class/superPropProxies.js line 23).
                None if self.proxy_parts(par).is_some() => {
                    self.get_prop(Value::heap(par), "prototype").ok()
                }
                None => None,
            };
            if let Some(pp) = pp {
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
    /// = proto ({w:false,e:false,c:false}); proto [[Prototype]] = %Function.prototype%,
    /// proto.constructor = ctor ({w:false,e:false,c:true}), proto[@@toStringTag] =
    /// `tag`. Requires `fn_proto` and `function_ctor` already set.
    fn build_dynamic_fn_intrinsic(&mut self, tag: &str) -> (u32, u32) {
        // {writable:false, enumerable:false, configurable:true} — name/length, the
        // prototype's `constructor` back-reference and @@toStringTag descriptor.
        let nameish = PropAttr {
            writable: false,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        // The ctor's own `.prototype` is additionally NON-configurable.
        let proto_attr = PropAttr {
            writable: false,
            enumerable: false,
            configurable: false,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let proto = self.setup_object(ObjMap::new());
        self.proto_of.insert(proto, Value::heap(self.fn_proto));
        let mut cm = ObjMap::new();
        cm.define("prototype", Value::heap(proto), proto_attr);
        cm.is_ctor = true;
        let ctor = self.setup_object(cm);
        self.proto_of.insert(ctor, Value::heap(self.function_ctor));
        let namev = self.alloc_str(tag.to_string());
        let tagv = self.alloc_str(tag.to_string());
        self.setup_define(ctor, "name", namev, nameish);
        self.setup_define(ctor, "length", Value::num(1.0), nameish);
        self.setup_define(proto, "@@toStringTag", tagv, nameish);
        self.setup_define(proto, "constructor", Value::heap(ctor), nameish);
        (ctor, proto)
    }

    /// Build the built-in global object graph (Object/Array/Function + their
    /// prototypes, with methods as native function VALUES) and inject it into the
    /// global slots the compiler reserved for those free identifiers. Makes
    /// `Array.isArray`, `Object.defineProperty`, `Function.prototype.call`, etc.
    /// usable as first-class values (what the test262 harness binds).
    /// A native function object for `setup_globals`, out of line: the setup
    /// makes about a hundred, and each made in place cost an enum built on
    /// the stack and the allocator call around it.
    #[inline(never)]
    fn setup_native(&mut self, id: u16) -> Value {
        Value::heap(self.heap.alloc(HeapObj::Native(id)))
    }

    /// A heap string with fixed text for `setup_globals`, out of line, so
    /// that the `to_string` allocation is not expanded at each of its forty
    /// uses.
    #[inline(never)]
    fn setup_str(&mut self, text: &str) -> Value {
        self.alloc_str(text.to_string())
    }

    /// `m.define(key, value, attr)` on heap object `obj` when it is an
    /// ordinary object (and nothing otherwise), out of line: in place, each of
    /// the setup's many defines carried its own bounds and tag check.
    #[inline(never)]
    fn setup_define(&mut self, obj: u32, key: &str, value: Value, attr: PropAttr) {
        if let HeapObj::Object(m) = self.heap.get_mut(obj) {
            m.define(key, value, attr);
        }
    }

    /// An ordinary object with these properties, on the heap, for
    /// `setup_globals`, out of line (each in place expanded the box's
    /// allocation and its failure path).
    #[inline(never)]
    fn setup_object(&mut self, map: ObjMap) -> u32 {
        self.heap.alloc(HeapObj::Object(Box::new(map)))
    }

    const SETUP_METHOD_ATTR: PropAttr = PropAttr {
        writable: true,
        enumerable: false,
        configurable: true,
        accessor: false,
        setter: Value::UNDEFINED,
    };

    const SETUP_PROTO_ATTR: PropAttr = PropAttr {
        writable: false,
        enumerable: false,
        configurable: false,
        accessor: false,
        setter: Value::UNDEFINED,
    };

    const SETUP_RO_ATTR: PropAttr = PropAttr {
        writable: false,
        enumerable: false,
        configurable: true,
        accessor: false,
        setter: Value::UNDEFINED,
    };

    const SETUP_GETTER_ATTR: PropAttr = PropAttr {
        writable: false,
        enumerable: false,
        configurable: true,
        accessor: true,
        setter: Value::UNDEFINED,
    };

    #[inline(never)]
    fn setup_build(&mut self, methods: &[(&str, u16)], protolink: Option<u32>) -> u32 {
        let mut m = ObjMap::new();
        for &(name, id) in methods {
            let nv = Value::heap(self.heap.alloc(HeapObj::Native(id)));
            m.define(name, nv, Self::SETUP_METHOD_ATTR);
        }
        if let Some(p) = protolink {
            m.define("prototype", Value::heap(p), Self::SETUP_PROTO_ATTR);
            // A global built WITH a .prototype is a constructor (Object/Array/Map/…);
            // a namespace (Reflect/Math/JSON, protolink None) is not.
            m.is_ctor = true;
        }
        self.heap.alloc(HeapObj::Object(Box::new(m)))
    }

    #[inline(never)]
    fn setup_build_indexed(&mut self, names: &[&str], base_id: u16) -> u32 {
        let mut m = ObjMap::new();
        for (i, &name) in names.iter().enumerate() {
            let nv = Value::heap(self.heap.alloc(HeapObj::Native(base_id + i as u16)));
            m.define(name, nv, Self::SETUP_METHOD_ATTR);
        }
        self.heap.alloc(HeapObj::Object(Box::new(m)))
    }

    #[inline(never)]
    fn setup_link_proto(&mut self, obj: u32, proto: u32) {
        self.proto_of.insert(obj, Value::heap(proto));
    }

    #[inline(never)]
    fn setup_define_native(&mut self, obj: u32, key: &str, id: u16, attr: PropAttr) {
        let v = self.setup_native(id);
        self.setup_define(obj, key, v, attr);
    }

    #[inline(never)]
    fn setup_define_methods(&mut self, obj: u32, pairs: &[(&str, u16)]) {
        for &(key, id) in pairs {
            self.setup_define_native(obj, key, id, Self::SETUP_METHOD_ATTR);
        }
    }

    #[inline(never)]
    fn setup_define_getter(&mut self, obj: u32, key: &str, id: u16) {
        self.setup_define_native(obj, key, id, Self::SETUP_GETTER_ATTR);
    }

    #[inline(never)]
    fn setup_define_acc(&mut self, obj: u32, key: &str, get_id: u16, set_id: u16) {
        let g = self.setup_native(get_id);
        let s = self.setup_native(set_id);
        let mut acc = Self::SETUP_GETTER_ATTR;
        acc.setter = s;
        self.setup_define(obj, key, g, acc);
    }

    #[inline(never)]
    fn setup_define_tag(&mut self, obj: u32, tag: &str) {
        let tv = self.setup_str(tag);
        self.setup_define(obj, "@@toStringTag", tv, Self::SETUP_RO_ATTR);
    }

    #[inline(never)]
    fn setup_prop_val(&self, obj: u32, key: &str) -> Option<Value> {
        match self.heap.get(obj) {
            HeapObj::Object(p) => p.get(key),
            _ => None,
        }
    }

    #[inline(never)]
    fn setup_alias_in_place(&mut self, obj: u32, alias: &str, canon: &str) -> Option<Value> {
        let cf = self.setup_prop_val(obj, canon)?;
        if let HeapObj::Object(p) = self.heap.get_mut(obj) {
            if let Some(i) = p.pos(alias) {
                p.set_val_at(i, cf);
            }
        }
        Some(cf)
    }

    #[cfg(not(feature = "wasm-lite"))]
    #[inline(never)]
    fn setup_temporal_type(
        &mut self,
        proto_methods: &[&str],
        method_base: u16,
        statics: &[(&str, u16)],
        name: &str,
        tag: &str,
        len: f64,
    ) -> (u32, u32) {
        let proto = self.setup_build_indexed(proto_methods, method_base);
        self.setup_link_proto(proto, self.obj_proto);
        let mut cm = ObjMap::new();
        cm.define("prototype", Value::heap(proto), Self::SETUP_PROTO_ATTR);
        for &(k, id) in statics {
            let v = self.setup_native(id);
            cm.define(k, v, Self::SETUP_METHOD_ATTR);
        }
        let nm = self.setup_str(name);
        cm.define("name", nm, Self::SETUP_RO_ATTR);
        cm.define("length", Value::num(len), Self::SETUP_RO_ATTR);
        cm.is_ctor = true;
        let ctor = self.setup_object(cm);
        self.setup_define(proto, "constructor", Value::heap(ctor), Self::SETUP_METHOD_ATTR);
        self.setup_define_tag(proto, tag);
        (proto, ctor)
    }

    #[inline(never)]
    fn setup_disposable_stack(
        &mut self,
        methods: &[(&str, u16)],
        alias_key: &str,
        canon_key: &str,
        tag: &str,
    ) -> (u32, u32) {
        let p = self.setup_build(methods, None);
        self.setup_link_proto(p, self.obj_proto);
        self.setup_define_getter(p, "disposed", native::DISPOSABLE_DISPOSED_GET);
        let dispose_fn = self.setup_prop_val(p, canon_key).unwrap_or(Value::UNDEFINED);
        self.setup_define(p, alias_key, dispose_fn, Self::SETUP_METHOD_ATTR);
        self.setup_define_tag(p, tag);
        let ctor = self.setup_build(&[], Some(p));
        self.setup_define(p, "constructor", Value::heap(ctor), Self::SETUP_METHOD_ATTR);
        (p, ctor)
    }

    #[cfg(any(feature = "php", feature = "rust", feature = "python"))]
    #[inline(never)]
    fn setup_bind_lang_natives(&mut self, pairs: &[(&str, u16)]) {
        for &(name, id) in pairs {
            let slot = self
                .program
                .global_names
                .iter()
                .position(|n| n == name);
            if let Some(slot) = slot.filter(|&slot| slot < self.globals.len()) {
                let h = self.heap.alloc(HeapObj::Native(id));
                self.globals[slot] = Value::heap(h);
                self.bump_global_gen(slot as u32);
            }
        }
    }

    pub(crate) fn setup_globals(&mut self) {
        use native::*;
        // Intern the eight `typeof` results once. This runs before
        // `set_gc_floor`, so the slots land below the floor and are pinned for
        // the VM's lifetime — no rooting, no per-GC work. See
        // `Vm::typeof_value`: the unfused `TypeOf` op previously allocated a
        // fresh string per evaluation (65ns) and made the `t === "number"` that
        // follows a content compare.
        //
        // Frozen: `resolve_const` answers a LITERAL that spells one of these
        // with the same handle, and a literal can open a string accumulator.
        // `var s = "number"; for (...) s += i` appended in place to the one
        // "number" the engine has, and `typeof 1` printed `number012` from
        // then on, in every tier and every release since the handles were
        // shared.
        for (i, name) in crate::bytecode::TYPEOF_NAMES.iter().enumerate() {
            let interned = self.setup_str(name);
            self.heap.freeze_shared_str(interned.heap_index());
            self.typeof_strs[i] = interned;
        }
        let method_attr = Self::SETUP_METHOD_ATTR;
        let proto_attr = Self::SETUP_PROTO_ATTR;
        let ro_attr = Self::SETUP_RO_ATTR;
        // Prototypes.
        self.obj_proto = self.setup_build(
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
        self.setup_define_acc(self.obj_proto, "__proto__", OBJPROTO_PROTO_GET, OBJPROTO_PROTO_SET);
        self.fn_proto = self.setup_build(
            &[
                ("call", FN_CALL),
                ("apply", FN_APPLY),
                ("bind", FN_BIND),
                ("toString", FN_TO_STRING),
            ],
            None,
        );
        // %Function.prototype% has own `name` ("") and `length` (0) data properties,
        // both { writable:false, enumerable:false, configurable:true } — it is itself
        // a function (one that returns undefined). An ordinary function's OWN
        // synthesized name/length shadow these (callable_intrinsic_value resolves
        // first in get_member), so this only surfaces for `Function.prototype.name`
        // itself or a function whose own `name` was `delete`d (which then reads "").
        {
            let empty = self.setup_str("");
            self.setup_define(self.fn_proto, "length", Value::int(0), ro_attr);
            self.setup_define(self.fn_proto, "name", empty, ro_attr);
        }
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
            // Preserve positional native IDs; omit only the Temporal extension.
            #[cfg(feature = "wasm-lite")]
            if name == "toTemporalInstant" {
                continue;
            }
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
        self.arr_proto = self.setup_build(&arr_methods, None);
        let arr_proto = self.arr_proto;
        self.heap.pin_mirror_dict(arr_proto);
        self.str_proto = self.setup_build(&str_methods, None);
        let str_proto = self.str_proto;
        if let HeapObj::Object(p) = self.heap.get(str_proto) {
            for (name, op) in [
                ("matchAll", crate::bytecode::RegExpMethod::MatchAll),
                ("replace", crate::bytecode::RegExpMethod::Replace),
                ("match", crate::bytecode::RegExpMethod::Match),
            ] {
                if let Some(v) = p.get(name).filter(|v| v.is_heap()) {
                    self.regexp_method_intrinsics[op.index()] = v.heap_index();
                }
            }
        }
        // `String.prototype.trimLeft`/`trimRight` ARE `trimStart`/`trimEnd` — the
        // SAME function objects (Annex B legacy aliases), so trimLeft===trimStart
        // and trimLeft.name is "trimStart". `setup_build` installed distinct natives;
        // alias the slots to the trimStart/trimEnd values (mirrors the Set
        // keys→values and Date toGMTString→toUTCString aliasing).
        self.setup_alias_in_place(str_proto, "trimLeft", "trimStart");
        self.setup_alias_in_place(str_proto, "trimRight", "trimEnd");
        // %String.prototype% is itself a String exotic object whose [[StringData]]
        // is the empty String, so it owns `length: 0` (non-writable, non-enumerable,
        // non-configurable) — `typeof String.prototype.length === "number"`.
        self.setup_define(str_proto, "length", Value::int(0), proto_attr);
        let num_proto = self.setup_build(&num_methods, None);
        let set_proto = self.setup_build(&set_methods, None);
        // `Set.prototype.keys` IS `%Set.prototype.values%` — the SAME function object
        // (24.2.3.x), so `Set.prototype.keys === Set.prototype.values`.
        self.setup_alias_in_place(set_proto, "keys", "values");
        let map_proto = self.setup_build(&map_methods, None);
        let bool_proto = self.setup_build(&bool_methods, None);
        let date_proto = self.setup_build(&date_methods, None);
        // `Date.prototype.toGMTString` IS `Date.prototype.toUTCString` — the SAME
        // function object (the Annex B legacy alias).
        self.setup_alias_in_place(date_proto, "toGMTString", "toUTCString");
        let promise_proto = self.setup_build(&promise_methods, None);
        // Store the proto indices so Map/Set/Date/Promise instances can delegate
        // method-as-value access to them (get_prop), mirroring arr_proto/str_proto.
        self.set_proto = set_proto;
        self.map_proto = map_proto;
        // `get Set.prototype.size` / `get Map.prototype.size` as real accessor
        // properties (the value still resolves via the fast get_member path). The
        // getter is brand-checked; { enumerable:false, configurable:true }.
        self.setup_define_getter(set_proto, "size", SET_SIZE_GET);
        self.setup_define_getter(map_proto, "size", MAP_SIZE_GET);
        self.date_proto = date_proto;
        // Date.prototype[Symbol.toPrimitive]: { writable:false, enumerable:false,
        // configurable:true } per spec; toJSON is GENERIC (overrides brand-checked).
        self.setup_define_native(date_proto, "@@toPrimitive", DATE_TO_PRIMITIVE, ro_attr);
        self.setup_define_native(date_proto, "toJSON", DATE_TO_JSON, method_attr);
        self.promise_proto = promise_proto;
        self.num_proto = num_proto;
        self.bool_proto = bool_proto;
        // Constructors.
        let obj_proto = self.obj_proto;
        let arr_proto = self.arr_proto;
        let fn_proto = self.fn_proto;
        // Function.prototype[Symbol.hasInstance]: OrdinaryHasInstance as a method,
        // with the spec-mandated { writable:false, enumerable:false,
        // configurable:false } — the only non-configurable @@-method on a builtin.
        self.setup_define_native(fn_proto, "@@hasInstance", FN_HAS_INSTANCE, proto_attr);
        // AddRestrictedFunctionProperties: `caller` and `arguments` are accessor
        // properties whose get AND set are both the single %ThrowTypeError%
        // intrinsic, { enumerable:false, configurable:true }. Ordinary functions
        // have no own caller/arguments and inherit these throwing accessors.
        let thrower = self.setup_native(FN_THROW_TYPE_ERROR);
        // Cache the single canonical %ThrowTypeError% so a strict arguments object's
        // callee poison-pill reuses THIS instance (intrinsic identity is observable).
        self.throw_type_error = thrower;
        // %ThrowTypeError% is born FROZEN: non-extensible with non-configurable
        // name/length (spec 10.2.4.1). The integrity flags live in the arr_props
        // side table (a Native has no inline map); the descriptor/delete paths
        // read them to report name/length non-configurable.
        {
            let m = self
                .arr_props
                .entry(thrower.heap_index())
                .or_insert_with(ObjMap::new);
            m.extensible = false;
            m.sealed = true;
            m.frozen = true;
        }
        let mut restricted_attr = Self::SETUP_GETTER_ATTR;
        restricted_attr.setter = thrower;
        self.setup_define(fn_proto, "caller", thrower, restricted_attr);
        self.setup_define(fn_proto, "arguments", thrower, restricted_attr);
        let object_ctor = self.setup_build(
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
        let array_ctor = self.setup_build(
            &[
                ("isArray", ARR_IS_ARRAY),
                ("from", ARR_FROM),
                ("fromAsync", ARR_FROM_ASYNC),
                ("of", ARR_OF),
            ],
            Some(arr_proto),
        );
        self.array_ctor = array_ctor;
        let function_ctor = self.setup_build(&[], Some(fn_proto));
        self.function_ctor = function_ctor;
        let string_ctor = self.setup_build(
            &[
                ("fromCharCode", STR_FROM_CHAR_CODE),
                ("fromCodePoint", STR_FROM_CODE_POINT),
                ("raw", STR_RAW),
            ],
            Some(str_proto),
        );
        // The global parseInt/parseFloat function objects, allocated up front so that
        // `Number.parseInt === parseInt` and `Number.parseFloat === parseFloat` (the
        // spec requires the SAME object, not just same behaviour). Reused for the bare
        // globals below.
        let parse_int_fn = self.heap.alloc(HeapObj::Native(GLOBAL_PARSE_INT));
        let parse_float_fn = self.heap.alloc(HeapObj::Native(GLOBAL_PARSE_FLOAT));
        // `Number`: the numeric constants (non-writable/enumerable/configurable per
        // spec) + Number.prototype. `Number(x)` / `Number.isInteger(x)` etc. are
        // call-site lowered (GlobalFn), so only the value-level shape is built here.
        let number_ctor = {
            let mut m = ObjMap::new();
            const CONSTS: &[(&str, f64)] = &[
                ("MAX_SAFE_INTEGER", 9007199254740991.0),
                ("MIN_SAFE_INTEGER", -9007199254740991.0),
                ("MAX_VALUE", f64::MAX),
                ("MIN_VALUE", 5e-324),
                ("EPSILON", f64::EPSILON),
                ("POSITIVE_INFINITY", f64::INFINITY),
                ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
                ("NaN", f64::NAN),
            ];
            for &(n, v) in CONSTS {
                m.define(n, Value::num(v), proto_attr);
            }
            for &(name, id) in &[
                ("isInteger", NUM_IS_INTEGER),
                ("isNaN", NUM_IS_NAN),
                ("isFinite", NUM_IS_FINITE),
                ("isSafeInteger", NUM_IS_SAFE_INTEGER),
            ] {
                let nv = self.setup_native(id);
                m.define(name, nv, method_attr);
            }
            m.define("parseInt", Value::heap(parse_int_fn), method_attr);
            m.define("parseFloat", Value::heap(parse_float_fn), method_attr);
            m.define("prototype", Value::heap(num_proto), proto_attr);
            m.is_ctor = true;
            self.setup_object(m)
        };
        let set_ctor = self.setup_build(&[], Some(set_proto));
        let map_ctor = self.setup_build(&[("groupBy", MAP_GROUP_BY)], Some(map_proto));
        self.set_ctor = set_ctor;
        self.map_ctor = map_ctor;
        let boolean_ctor = self.setup_build(&[], Some(bool_proto));
        let date_ctor = self.setup_build(
            &[("now", DATE_NOW), ("parse", DATE_PARSE), ("UTC", DATE_UTC)],
            Some(date_proto),
        );
        let promise_ctor = self.setup_build(
            &[
                ("resolve", PROMISE_RESOLVE),
                ("reject", PROMISE_REJECT),
                ("all", PROMISE_ALL),
                ("allSettled", PROMISE_ALLSETTLED),
                ("allKeyed", PROMISE_ALLKEYED),
                ("allSettledKeyed", PROMISE_ALLSETTLEDKEYED),
                ("race", PROMISE_RACE),
                ("any", PROMISE_ANY),
                ("withResolvers", PROMISE_WITH_RESOLVERS),
                ("try", PROMISE_TRY),
            ],
            Some(promise_proto),
        );
        self.promise_ctor_intrinsic = promise_ctor;
        if let Some(v) = self.setup_prop_val(promise_proto, "then").filter(|v| v.is_heap()) {
            self.promise_then_intrinsic = v.heap_index();
        }
        let reflect_ctor = self.setup_build(
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
        self.setup_define_tag(reflect_ctor, "Reflect");
        let weakmap_proto = self.setup_build(
            &[
                ("get", WM_GET),
                ("set", WM_SET),
                ("has", WM_HAS),
                ("delete", WM_DELETE),
                ("getOrInsert", WM_GET_OR_INSERT),
                ("getOrInsertComputed", WM_GET_OR_INSERT_COMPUTED),
            ],
            None,
        );
        let weakset_proto = self.setup_build(
            &[("add", WS_ADD), ("has", WS_HAS), ("delete", WS_DELETE)],
            None,
        );
        let weakref_proto = self.setup_build(&[("deref", WR_DEREF)], None);
        let finreg_proto = self.setup_build(
            &[("register", FR_REGISTER), ("unregister", FR_UNREGISTER)],
            None,
        );
        let array_iter_proto = self.setup_build(&[("next", ITER_NEXT)], None);
        self.array_iter_proto = array_iter_proto;
        self.default_array_iter_next = self
            .setup_prop_val(array_iter_proto, "next")
            .unwrap_or(Value::UNDEFINED);
        self.map_iter_proto = self.setup_build(&[("next", ITER_NEXT)], None);
        self.set_iter_proto = self.setup_build(&[("next", ITER_NEXT)], None);
        self.string_iter_proto = self.setup_build(&[("next", ITER_NEXT)], None);
        {
            let it = self.setup_native(STR_ITERATOR);
            self.setup_define(str_proto, "@@iterator", it, method_attr);
            self.default_string_iter = it;
        }
        // ── ES2025 Iterator Helpers ──
        let iter_root = self.setup_build(
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
                ("join", ITER_JOIN),
                ("chunks", ITER_CHUNKS),
                ("windows", ITER_WINDOWS),
                ("includes", ITER_INCLUDES),
                ("@@iterator", ITER_SELF),
                ("@@dispose", ITER_DISPOSE),
            ],
            None,
        );
        self.iterator_proto_root = iter_root;
        self.setup_link_proto(iter_root, obj_proto);
        self.setup_define_acc(iter_root, "@@toStringTag", ITER_TAG_GET, ITER_TAG_SET);
        self.setup_define_acc(iter_root, "constructor", ITER_CTOR_GET, ITER_CTOR_SET);
        let helper_proto = self.setup_build(
            &[("next", ITER_HELPER_NEXT), ("return", ITER_HELPER_RETURN)],
            None,
        );
        self.iterator_helper_proto = helper_proto;
        self.setup_link_proto(helper_proto, iter_root);
        self.setup_define_tag(helper_proto, "Iterator Helper");
        let gen_proto = self.setup_build(
            &[
                ("next", native::GEN_NEXT),
                ("return", native::GEN_RETURN),
                ("throw", native::GEN_THROW),
            ],
            None,
        );
        self.setup_link_proto(gen_proto, iter_root);
        self.setup_define_tag(gen_proto, "Generator");
        self.gen_proto = gen_proto;
        let async_iter_root = self.setup_build(
            &[
                ("@@asyncIterator", ITER_SELF),
                ("@@asyncDispose", native::ASYNC_ITER_DISPOSE),
            ],
            None,
        );
        self.setup_link_proto(async_iter_root, obj_proto);
        if let Some(f) = self
            .setup_prop_val(async_iter_root, "@@asyncIterator")
            .filter(|f| f.is_heap())
        {
            let nv = self.setup_str("[Symbol.asyncIterator]");
            self.fn_props_gen = self.fn_props_gen.wrapping_add(1);
            self.fn_props
                .entry(f.heap_index())
                .or_default()
                .define("name", nv, ro_attr);
        }
        let asyncgen_proto = self.setup_build(
            &[
                ("next", native::ASYNCGEN_NEXT),
                ("return", native::ASYNCGEN_RETURN),
                ("throw", native::ASYNCGEN_THROW),
            ],
            None,
        );
        self.setup_link_proto(asyncgen_proto, async_iter_root);
        self.setup_define_tag(asyncgen_proto, "AsyncGenerator");
        self.asyncgen_proto = asyncgen_proto;
        let iter_ctor = self.setup_build(
            &[
                ("from", ITER_FROM),
                ("concat", ITER_CONCAT),
                ("zip", ITER_ZIP),
                ("zipKeyed", ITER_ZIPKEYED),
            ],
            Some(iter_root),
        );
        self.iterator_ctor = iter_ctor;
        let iter_name = self.setup_str("Iterator");
        self.setup_define(iter_ctor, "name", iter_name, ro_attr);
        self.setup_define(iter_ctor, "length", Value::num(0.0), ro_attr);
        for (p, tag) in [
            (self.array_iter_proto, "Array Iterator"),
            (self.map_iter_proto, "Map Iterator"),
            (self.set_iter_proto, "Set Iterator"),
            (self.string_iter_proto, "String Iterator"),
        ] {
            self.setup_link_proto(p, iter_root);
            self.setup_define_tag(p, tag);
        }
        let (g_ctor, g_proto) = self.build_dynamic_fn_intrinsic("GeneratorFunction");
        self.gen_fn_ctor = g_ctor;
        self.gen_fn_proto = g_proto;
        let (a_ctor, a_proto) = self.build_dynamic_fn_intrinsic("AsyncFunction");
        self.async_fn_ctor = a_ctor;
        self.async_fn_proto = a_proto;
        let (ag_ctor, ag_proto) = self.build_dynamic_fn_intrinsic("AsyncGeneratorFunction");
        self.asyncgen_fn_ctor = ag_ctor;
        self.asyncgen_fn_proto = ag_proto;
        for (fn_proto, inst_proto) in [
            (self.gen_fn_proto, self.gen_proto),
            (self.asyncgen_fn_proto, self.asyncgen_proto),
        ] {
            if fn_proto != 0 && inst_proto != 0 {
                self.setup_define(fn_proto, "prototype", Value::heap(inst_proto), ro_attr);
                self.setup_define(inst_proto, "constructor", Value::heap(fn_proto), ro_attr);
            }
        }
        // Default @@iterator: Map → entries, Set → values, Array → values.
        if let Some(v) = self.setup_prop_val(map_proto, "entries") {
            self.setup_define(map_proto, "@@iterator", v, method_attr);
            self.default_map_iter = v;
        }
        if let Some(v) = self.setup_prop_val(set_proto, "values") {
            self.setup_define(set_proto, "@@iterator", v, method_attr);
            self.default_set_iter = v;
        }
        if let Some(vf) = self.setup_prop_val(self.arr_proto, "values") {
            self.setup_define(self.arr_proto, "@@iterator", vf, method_attr);
            self.default_array_iter = vf;
        }
        let mut unsc = ObjMap::new();
        let unsc_data_attr = PropAttr::data();
        for name in [
            "at",
            "copyWithin",
            "entries",
            "fill",
            "find",
            "findIndex",
            "findLast",
            "findLastIndex",
            "flat",
            "flatMap",
            "includes",
            "keys",
            "toReversed",
            "toSorted",
            "toSpliced",
            "values",
        ] {
            unsc.define(name, Value::TRUE, unsc_data_attr);
        }
        let unsc_idx = self.setup_object(unsc);
        self.proto_of.insert(unsc_idx, Value::NULL);
        self.setup_define(self.arr_proto, "@@unscopables", Value::heap(unsc_idx), ro_attr);
        self.weakmap_proto = weakmap_proto;
        self.weakset_proto = weakset_proto;
        self.weakref_proto = weakref_proto;
        self.finreg_proto = finreg_proto;
        for (proto, name) in [
            (map_proto, "Map"),
            (set_proto, "Set"),
            (promise_proto, "Promise"),
            (weakmap_proto, "WeakMap"),
            (weakset_proto, "WeakSet"),
            (weakref_proto, "WeakRef"),
            (finreg_proto, "FinalizationRegistry"),
        ] {
            self.setup_define_tag(proto, name);
        }
        let weakmap_ctor = self.setup_build(&[], Some(weakmap_proto));
        let weakset_ctor = self.setup_build(&[], Some(weakset_proto));
        self.weakmap_ctor = weakmap_ctor;
        self.weakset_ctor = weakset_ctor;
        let weakref_ctor = self.setup_build(&[], Some(weakref_proto));
        let finreg_ctor = self.setup_build(&[], Some(finreg_proto));
        self.weakref_ctor = weakref_ctor;
        self.finreg_ctor = finreg_ctor;
        let te_proto = self.setup_build(
            &[("encode", TE_ENCODE), ("encodeInto", TE_ENCODE_INTO)],
            None,
        );
        let td_proto = self.setup_build(&[("decode", TD_DECODE)], None);
        for (proto, key, id) in [
            (te_proto, "encoding", TE_ENCODING_GET),
            (td_proto, "encoding", TD_ENCODING_GET),
            (td_proto, "fatal", TD_FATAL_GET),
            (td_proto, "ignoreBOM", TD_IGNORE_BOM_GET),
        ] {
            self.setup_define_getter(proto, key, id);
        }
        self.setup_define_tag(te_proto, "TextEncoder");
        self.setup_define_tag(td_proto, "TextDecoder");
        self.text_encoder_proto = te_proto;
        self.text_decoder_proto = td_proto;
        let text_encoder_ctor = self.setup_build(&[], Some(te_proto));
        let text_decoder_ctor = self.setup_build(&[], Some(td_proto));
        self.text_encoder_ctor = text_encoder_ctor;
        self.text_decoder_ctor = text_decoder_ctor;
        // Error hierarchy: `Error` + the 7 native subtypes.
        {
            let err_proto = self.setup_build(&[("toString", ERROR_TO_STRING)], None);
            self.setup_link_proto(err_proto, obj_proto);
            self.error_protos[0] = err_proto;
            self.setup_define_acc(err_proto, "stack", ERROR_STACK_GET, ERROR_STACK_SET);
            for k in 1..8usize {
                let p = self.setup_build(&[], None);
                self.setup_link_proto(p, err_proto);
                self.error_protos[k] = p;
            }
            for k in 0..8usize {
                let proto = self.error_protos[k];
                self.error_ctors[k] = self.setup_build(&[], Some(proto));
            }
            let err_ctor = self.error_ctors[0];
            self.setup_link_proto(err_ctor, fn_proto);
            for k in 1..8usize {
                let c = self.error_ctors[k];
                self.setup_link_proto(c, err_ctor);
            }
            for k in 0..8usize {
                let name_v = self.setup_str(native::ERROR_NAMES[k]);
                let empty_v = self.setup_str("");
                let ctor_v = Value::heap(self.error_ctors[k]);
                let proto = self.error_protos[k];
                self.setup_define(proto, "name", name_v, method_attr);
                self.setup_define(proto, "message", empty_v, method_attr);
                self.setup_define(proto, "constructor", ctor_v, method_attr);
            }
            self.setup_define_native(self.error_ctors[0], "isError", ERROR_IS_ERROR, method_attr);
        }
        // `Symbol`
        {
            let symbol_proto = self.setup_build(
                &[("toString", SYMBOL_TO_STRING), ("valueOf", SYMBOL_VALUE_OF)],
                None,
            );
            self.setup_link_proto(symbol_proto, obj_proto);
            self.symbol_proto = symbol_proto;
            let for_v = self.setup_native(SYMBOL_FOR);
            let keyfor_v = self.setup_native(SYMBOL_KEY_FOR);
            let name_v = self.setup_str("Symbol");
            let mut m = ObjMap::new();
            m.define("prototype", Value::heap(symbol_proto), proto_attr);
            m.define("for", for_v, method_attr);
            m.define("keyFor", keyfor_v, method_attr);
            m.define("name", name_v, ro_attr);
            m.define("length", Value::num(0.0), ro_attr);
            m.is_ctor = true;
            let symbol_ctor = self.setup_object(m);
            self.symbol_ctor = symbol_ctor;
            self.setup_define(symbol_proto, "constructor", Value::heap(symbol_ctor), method_attr);
            self.setup_define_native(symbol_proto, "@@toPrimitive", SYMBOL_TO_PRIMITIVE, ro_attr);
            self.setup_define_tag(symbol_proto, "Symbol");
            self.setup_define_getter(symbol_proto, "description", SYMBOL_DESCRIPTION_GET);
            for &(jsname, prop_key) in native::WELL_KNOWN_SYMBOLS {
                let desc = self.alloc_str(format!("Symbol.{jsname}"));
                let sym = self.make_named_symbol(desc, prop_key);
                self.setup_define(symbol_ctor, jsname, sym, proto_attr);
            }
        }
        // `BigInt`
        {
            let bigint_proto = self.setup_build(
                &[
                    ("toString", BIGINT_TO_STRING),
                    ("toLocaleString", BIGINT_TO_LOCALE_STRING),
                    ("valueOf", BIGINT_VALUE_OF),
                ],
                None,
            );
            self.setup_link_proto(bigint_proto, obj_proto);
            self.bigint_proto = bigint_proto;
            let asintn = self.setup_native(BIGINT_AS_INTN);
            let asuintn = self.setup_native(BIGINT_AS_UINTN);
            let name_v = self.setup_str("BigInt");
            let mut m = ObjMap::new();
            m.define("prototype", Value::heap(bigint_proto), proto_attr);
            m.define("asIntN", asintn, method_attr);
            m.define("asUintN", asuintn, method_attr);
            m.define("name", name_v, ro_attr);
            m.define("length", Value::num(1.0), ro_attr);
            m.is_ctor = true;
            let bigint_ctor = self.setup_object(m);
            self.bigint_ctor = bigint_ctor;
            self.setup_define(bigint_proto, "constructor", Value::heap(bigint_ctor), method_attr);
            self.setup_define_tag(bigint_proto, "BigInt");
        }
        // `RegExp`
        {
            let regexp_proto = self.setup_build(
                &[
                    ("test", REGEXP_TEST),
                    ("exec", REGEXP_EXEC),
                    ("toString", REGEXP_TO_STRING),
                    ("compile", REGEXP_COMPILE),
                ],
                None,
            );
            self.setup_link_proto(regexp_proto, obj_proto);
            self.regexp_proto = regexp_proto;
            if let HeapObj::Object(p) = self.heap.get(regexp_proto) {
                for (name, op) in [
                    ("test", crate::bytecode::RegExpMethod::Test),
                    ("exec", crate::bytecode::RegExpMethod::Exec),
                ] {
                    if let Some(v) = p.get(name).filter(|v| v.is_heap()) {
                        self.regexp_method_intrinsics[op.index()] = v.heap_index();
                    }
                }
            }
            for &(name, getid) in &[
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
            ] {
                self.setup_define_getter(regexp_proto, name, getid);
            }
            let rsi_proto = self.setup_build(&[("next", ITER_NEXT)], None);
            let iter_root = self.iterator_proto_root;
            self.setup_link_proto(rsi_proto, iter_root);
            self.setup_define_tag(rsi_proto, "RegExp String Iterator");
            self.regexp_string_iter_proto = rsi_proto;
            self.setup_define_methods(
                regexp_proto,
                &[
                    ("@@matchAll", REGEXP_SYM_MATCHALL),
                    ("@@search", REGEXP_SYM_SEARCH),
                    ("@@match", REGEXP_SYM_MATCH),
                    ("@@split", REGEXP_SYM_SPLIT),
                    ("@@replace", REGEXP_SYM_REPLACE),
                ],
            );
            let regexp_ctor = self.setup_build(&[("escape", REGEXP_ESCAPE)], Some(regexp_proto));
            self.regexp_ctor = regexp_ctor;
            self.setup_define(regexp_proto, "constructor", Value::heap(regexp_ctor), method_attr);
            {
                let s = self.setup_native(REGEXP_LEGACY_SET);
                const PROPS: &[(&str, usize)] = &[
                    ("input", 0),
                    ("$_", 0),
                    ("lastMatch", 1),
                    ("$&", 1),
                    ("lastParen", 2),
                    ("$+", 2),
                    ("leftContext", 3),
                    ("$`", 3),
                    ("rightContext", 4),
                    ("$'", 4),
                    ("$1", 5),
                    ("$2", 6),
                    ("$3", 7),
                    ("$4", 8),
                    ("$5", 9),
                    ("$6", 10),
                    ("$7", 11),
                    ("$8", 12),
                    ("$9", 13),
                ];
                for &(key, slot) in PROPS {
                    let g = Value::heap(self.heap.alloc(HeapObj::NativeClosure {
                        id: REGEXP_LEGACY_GET,
                        state: vec![Value::num(slot as f64)],
                        name: "get",
                        length: 0,
                    }));
                    let mut attr = Self::SETUP_GETTER_ATTR;
                    if slot == 0 {
                        attr.setter = s;
                    }
                    self.setup_define(regexp_ctor, key, g, attr);
                }
            }
        }
        // TypedArrays, ArrayBuffer, SharedArrayBuffer, DataView, Proxy, Temporal, Intl.
        {
            let ta_base_proto =
                self.setup_build_indexed(native::TA_PROTO_METHODS, native::TA_METHOD_BASE);
            self.setup_link_proto(ta_base_proto, obj_proto);
            if let Some(vf) = self.setup_alias_in_place(ta_base_proto, "@@iterator", "values") {
                self.default_ta_iter = vf;
            }
            if let Some(ts) = self.setup_prop_val(self.arr_proto, "toString") {
                if let HeapObj::Object(p) = self.heap.get_mut(ta_base_proto) {
                    if let Some(i) = p.pos("toString") {
                        p.set_val_at(i, ts);
                    }
                }
            }
            self.ta_base_proto = ta_base_proto;
            let ta_base_ctor = self.setup_build(&[], Some(ta_base_proto));
            self.ta_base_ctor = ta_base_ctor;
            self.setup_link_proto(ta_base_ctor, fn_proto);
            let tname = self.setup_str("TypedArray");
            self.setup_define(ta_base_ctor, "name", tname, ro_attr);
            self.setup_define(ta_base_ctor, "length", Value::num(0.0), ro_attr);
            self.setup_define_methods(ta_base_ctor, &[("from", TA_FROM), ("of", TA_OF)]);
            self.setup_define(ta_base_proto, "constructor", Value::heap(ta_base_ctor), method_attr);
            for &(name, getid) in &[
                ("buffer", TA_GET_BUFFER),
                ("byteLength", TA_GET_BYTELENGTH),
                ("byteOffset", TA_GET_BYTEOFFSET),
                ("length", TA_GET_LENGTH),
                ("@@toStringTag", TA_GET_TOSTRINGTAG),
            ] {
                self.setup_define_getter(ta_base_proto, name, getid);
            }
            for k in 0..native::TA_KINDS.len() {
                let size = native::TA_KINDS[k].1;
                let proto = self.setup_build(&[], None);
                self.setup_link_proto(proto, ta_base_proto);
                self.ta_protos[k] = proto;
                let ctor = self.setup_build(&[], Some(proto));
                self.setup_link_proto(ctor, ta_base_ctor);
                self.ta_ctors[k] = ctor;
                self.setup_define(proto, "constructor", Value::heap(ctor), method_attr);
                self.setup_define(proto, "BYTES_PER_ELEMENT", Value::num(size as f64), proto_attr);
                self.setup_define(ctor, "BYTES_PER_ELEMENT", Value::num(size as f64), proto_attr);
            }
            self.setup_define_methods(
                self.ta_protos[1],
                &[
                    ("toHex", U8_TO_HEX),
                    ("setFromHex", U8_SET_FROM_HEX),
                    ("toBase64", U8_TO_BASE64),
                    ("setFromBase64", U8_SET_FROM_BASE64),
                ],
            );
            self.setup_define_methods(
                self.ta_ctors[1],
                &[("fromHex", U8_FROM_HEX), ("fromBase64", U8_FROM_BASE64)],
            );
            let arraybuffer_proto = self.setup_build(
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
            self.setup_link_proto(arraybuffer_proto, obj_proto);
            self.arraybuffer_proto = arraybuffer_proto;
            let arraybuffer_ctor =
                self.setup_build(&[("isView", ARRAYBUFFER_ISVIEW)], Some(arraybuffer_proto));
            self.arraybuffer_ctor = arraybuffer_ctor;
            self.setup_define(
                arraybuffer_proto,
                "constructor",
                Value::heap(arraybuffer_ctor),
                method_attr,
            );
            self.setup_define_tag(arraybuffer_proto, "ArrayBuffer");
            let sab_proto = self.setup_build(
                &[("slice", native::SAB_SLICE), ("grow", native::SAB_GROW)],
                None,
            );
            self.setup_link_proto(sab_proto, obj_proto);
            self.sab_proto = sab_proto;
            let sab_ctor = self.setup_build(&[], Some(sab_proto));
            self.sab_ctor = sab_ctor;
            for (g, &name) in native::SAB_GETTERS.iter().enumerate() {
                self.setup_define_getter(sab_proto, name, native::SAB_GETTER_BASE + g as u16);
            }
            self.setup_define(sab_proto, "constructor", Value::heap(sab_ctor), method_attr);
            self.setup_define_tag(sab_proto, "SharedArrayBuffer");
            let dataview_proto =
                self.setup_build_indexed(native::DV_PROTO_METHODS, native::DV_METHOD_BASE);
            self.setup_link_proto(dataview_proto, obj_proto);
            self.setup_define_tag(dataview_proto, "DataView");
            self.dataview_proto = dataview_proto;
            for (g, &(name, kind)) in native::BUFFER_GETTERS.iter().enumerate() {
                let target = match kind {
                    0 => arraybuffer_proto,
                    1 => ta_base_proto,
                    _ => dataview_proto,
                };
                self.setup_define_getter(target, name, native::BUFFER_GETTER_BASE + g as u16);
            }
            // `Proxy`: a constructor with no `.prototype`; `Proxy.revocable` static.
            let revocable = self.setup_native(PROXY_REVOCABLE);
            let mut pm = ObjMap::new();
            pm.define("revocable", revocable, method_attr);
            pm.is_ctor = true;
            self.proxy_ctor = self.setup_object(pm);
            #[cfg(not(feature = "wasm-lite"))]
            {
                // `Temporal` namespace + the 8 Temporal constructors.
                let (dp, dc) = self.setup_temporal_type(
                    native::TEMPORAL_DURATION_METHODS,
                    native::TEMPORAL_M_BASE,
                    &[
                        ("from", TEMPORAL_DURATION_FROM),
                        ("compare", TEMPORAL_DURATION_COMPARE),
                    ],
                    "Duration",
                    "Temporal.Duration",
                    0.0,
                );
                self.duration_proto = dp;
                self.duration_ctor = dc;
                let (pdp, pdc) = self.setup_temporal_type(
                    native::PLAINDATE_METHODS,
                    native::PD_M_BASE,
                    &[("from", PLAINDATE_FROM), ("compare", PLAINDATE_COMPARE)],
                    "PlainDate",
                    "Temporal.PlainDate",
                    3.0,
                );
                self.plaindate_proto = pdp;
                self.plaindate_ctor = pdc;
                let (ptp, ptc) = self.setup_temporal_type(
                    native::PLAINTIME_METHODS,
                    native::PT_M_BASE,
                    &[("from", PLAINTIME_FROM), ("compare", PLAINTIME_COMPARE)],
                    "PlainTime",
                    "Temporal.PlainTime",
                    0.0,
                );
                self.plaintime_proto = ptp;
                self.plaintime_ctor = ptc;
                let (pdtp, pdtc) = self.setup_temporal_type(
                    native::PLAINDATETIME_METHODS,
                    native::PDT_M_BASE,
                    &[
                        ("from", PLAINDATETIME_FROM),
                        ("compare", PLAINDATETIME_COMPARE),
                    ],
                    "PlainDateTime",
                    "Temporal.PlainDateTime",
                    3.0,
                );
                self.plaindatetime_proto = pdtp;
                self.plaindatetime_ctor = pdtc;
                let (ip, ic) = self.setup_temporal_type(
                    native::INSTANT_METHODS,
                    native::INST_M_BASE,
                    &[
                        ("from", INST_FROM),
                        ("fromEpochMilliseconds", INST_FROM_EPOCH_MS),
                        ("fromEpochNanoseconds", INST_FROM_EPOCH_NS),
                        ("compare", INST_COMPARE),
                    ],
                    "Instant",
                    "Temporal.Instant",
                    1.0,
                );
                self.instant_proto = ip;
                self.instant_ctor = ic;
                let (pymp, pymc) = self.setup_temporal_type(
                    native::PLAINYEARMONTH_METHODS,
                    native::PYM_M_BASE,
                    &[
                        ("from", PLAINYEARMONTH_FROM),
                        ("compare", PLAINYEARMONTH_COMPARE),
                    ],
                    "PlainYearMonth",
                    "Temporal.PlainYearMonth",
                    2.0,
                );
                self.plainyearmonth_proto = pymp;
                self.plainyearmonth_ctor = pymc;
                let (pmdp, pmdc) = self.setup_temporal_type(
                    native::PLAINMONTHDAY_METHODS,
                    native::PMD_M_BASE,
                    &[("from", PLAINMONTHDAY_FROM)],
                    "PlainMonthDay",
                    "Temporal.PlainMonthDay",
                    2.0,
                );
                self.plainmonthday_proto = pmdp;
                self.plainmonthday_ctor = pmdc;
                let (zdtp, zdtc) = self.setup_temporal_type(
                    native::ZONEDDATETIME_METHODS,
                    native::ZDT_M_BASE,
                    &[("from", native::ZDT_FROM), ("compare", native::ZDT_COMPARE)],
                    "ZonedDateTime",
                    "Temporal.ZonedDateTime",
                    2.0,
                );
                self.zoneddatetime_proto = zdtp;
                self.zoneddatetime_ctor = zdtc;
                // Temporal.Now (a namespace object, not a constructor).
                let now_ns = self.setup_build(
                    &[
                        ("instant", NOW_INSTANT),
                        ("plainDateTimeISO", NOW_PLAINDATETIME_ISO),
                        ("plainDateISO", NOW_PLAINDATE_ISO),
                        ("plainTimeISO", NOW_PLAINTIME_ISO),
                        ("timeZoneId", NOW_TIMEZONE_ID),
                        ("zonedDateTimeISO", NOW_ZONEDDATETIME_ISO),
                    ],
                    None,
                );
                self.setup_define_tag(now_ns, "Temporal.Now");
                let mut tn = ObjMap::new();
                for (name, c) in [
                    ("Duration", dc),
                    ("PlainDate", pdc),
                    ("PlainTime", ptc),
                    ("PlainDateTime", pdtc),
                    ("Instant", ic),
                    ("PlainYearMonth", pymc),
                    ("PlainMonthDay", pmdc),
                    ("ZonedDateTime", zdtc),
                    ("Now", now_ns),
                ] {
                    tn.define(name, Value::heap(c), method_attr);
                }
                let temporal_tag = self.setup_str("Temporal");
                tn.define("@@toStringTag", temporal_tag, ro_attr);
                self.temporal_ns = self.setup_object(tn);
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
                for (proto, fields) in temporal_getter_sets {
                    for &name in fields {
                        let idx = native::TEMPORAL_GETTER_FIELDS
                            .iter()
                            .position(|f| *f == name)
                            .expect("getter field in union");
                        self.setup_define_getter(
                            proto,
                            name,
                            native::TEMPORAL_GETTER_BASE + idx as u16,
                        );
                    }
                    self.setup_define_native(
                        proto,
                        "toLocaleString",
                        TEMPORAL_TO_LOCALE_STRING,
                        method_attr,
                    );
                }
                self.setup_define_native(
                    self.zoneddatetime_proto,
                    "getTimeZoneTransition",
                    ZDT_GET_TZ_TRANSITION,
                    method_attr,
                );
                self.setup_define_native(
                    self.plaindatetime_proto,
                    "withPlainTime",
                    PDT_WITH_PLAIN_TIME,
                    method_attr,
                );
                // ── Intl namespace + service constructors ──
                const INTL_SERVICES: &[(u8, &str, f64, &[(&str, u16)], bool)] = &[
                    (
                        native::INTL_NUMBERFORMAT,
                        "NumberFormat",
                        0.0,
                        &[
                            ("formatToParts", INTL_NF_FORMAT_TO_PARTS),
                            ("formatRange", INTL_NF_FORMAT_RANGE),
                            ("formatRangeToParts", INTL_NF_FORMAT_RANGE_TO_PARTS),
                            ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                        ],
                        true,
                    ),
                    (
                        native::INTL_DATETIMEFORMAT,
                        "DateTimeFormat",
                        0.0,
                        &[
                            ("formatToParts", INTL_DTF_FORMAT_TO_PARTS),
                            ("formatRange", INTL_DTF_FORMAT_RANGE),
                            ("formatRangeToParts", INTL_DTF_FORMAT_RANGE_TO_PARTS),
                            ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                        ],
                        true,
                    ),
                    (
                        native::INTL_COLLATOR,
                        "Collator",
                        0.0,
                        &[("resolvedOptions", INTL_RESOLVED_OPTIONS)],
                        true,
                    ),
                    (
                        native::INTL_PLURALRULES,
                        "PluralRules",
                        0.0,
                        &[
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
                        &[
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
                        &[
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
                        &[
                            ("segment", INTL_SEGMENTER_SEGMENT),
                            ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                        ],
                        true,
                    ),
                    (
                        native::INTL_LOCALE,
                        "Locale",
                        1.0,
                        &[
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
                        &[
                            ("of", INTL_DISPLAYNAMES_OF),
                            ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                        ],
                        true,
                    ),
                    (
                        native::INTL_DURATIONFORMAT,
                        "DurationFormat",
                        0.0,
                        &[
                            ("format", INTL_DURATION_FORMAT),
                            ("formatToParts", INTL_DURATION_FORMAT_TO_PARTS),
                            ("resolvedOptions", INTL_RESOLVED_OPTIONS),
                        ],
                        true,
                    ),
                ];
                let mut intl_ns_map = ObjMap::new();
                for &(kind, name, len, methods, slo) in INTL_SERVICES {
                    let mut pm = ObjMap::new();
                    for &(m, id) in methods {
                        let resolved_id = if id == INTL_RESOLVED_OPTIONS {
                            native::INTL_RESOLVED_OPTIONS_BASE + kind as u16
                        } else {
                            id
                        };
                        let nv = self.setup_native(resolved_id);
                        pm.define(m, nv, method_attr);
                    }
                    let proto = self.setup_object(pm);
                    self.setup_link_proto(proto, obj_proto);
                    self.intl_protos[kind as usize] = proto;
                    let statics: &[(&str, u16)] = if slo {
                        if kind == native::INTL_DATETIMEFORMAT {
                            &[("supportedLocalesOf", INTL_DTF_SUPPORTED_LOCALES_OF)]
                        } else {
                            &[("supportedLocalesOf", INTL_SUPPORTED_LOCALES_OF)]
                        }
                    } else {
                        &[]
                    };
                    let ctor = self.setup_build(statics, Some(proto));
                    self.intl_ctors[kind as usize] = ctor;
                    let nm = self.setup_str(name);
                    let tag = self.alloc_str(format!("Intl.{name}"));
                    self.setup_define(ctor, "name", nm, ro_attr);
                    self.setup_define(ctor, "length", Value::num(len), ro_attr);
                    self.setup_define(proto, "constructor", Value::heap(ctor), method_attr);
                    self.setup_define(proto, "@@toStringTag", tag, ro_attr);
                    intl_ns_map.define(name, Value::heap(ctor), method_attr);
                }
                let locale_proto = self.intl_protos[native::INTL_LOCALE as usize];
                for (i, gname) in native::LOCALE_ACCESSORS.iter().enumerate() {
                    self.setup_define_getter(locale_proto, gname, INTL_LOCALE_GET_BASE + i as u16);
                }
                for &(gname, gid) in &[
                    ("variants", INTL_LOCALE_GET_VARIANTS),
                    ("firstDayOfWeek", INTL_LOCALE_GET_FIRSTDAY),
                ] {
                    self.setup_define_getter(locale_proto, gname, gid);
                }
                for (i, mname) in native::LOCALE_INFO_METHODS.iter().enumerate() {
                    self.setup_define_native(
                        locale_proto,
                        mname,
                        INTL_LOCALE_INFO_BASE + i as u16,
                        method_attr,
                    );
                }
                for &(k, name, gid) in &[
                    (native::INTL_NUMBERFORMAT, "format", INTL_NF_FORMAT_GET),
                    (native::INTL_DATETIMEFORMAT, "format", INTL_DTF_FORMAT_GET),
                    (native::INTL_COLLATOR, "compare", INTL_COLLATOR_COMPARE_GET),
                ] {
                    let p = self.intl_protos[k as usize];
                    self.setup_define_getter(p, name, gid);
                }
                let segments_proto = self.setup_build(
                    &[
                        ("containing", INTL_SEGMENTS_CONTAINING),
                        ("@@iterator", INTL_SEGMENTS_ITERATOR),
                    ],
                    None,
                );
                self.setup_link_proto(segments_proto, obj_proto);
                self.intl_protos[native::INTL_SEGMENTS as usize] = segments_proto;
                let seg_iter_proto =
                    self.setup_build(&[("next", INTL_SEGMENT_ITER_NEXT)], None);
                self.setup_link_proto(seg_iter_proto, self.iterator_proto_root);
                self.intl_protos[native::INTL_SEGMENT_ITERATOR as usize] = seg_iter_proto;
                self.setup_define_tag(seg_iter_proto, "Segmenter String Iterator");
                let gcl = self.setup_native(INTL_GET_CANONICAL_LOCALES);
                intl_ns_map.define("getCanonicalLocales", gcl, method_attr);
                let svo = self.setup_native(INTL_SUPPORTED_VALUES_OF);
                intl_ns_map.define("supportedValuesOf", svo, method_attr);
                let intltag = self.setup_str("Intl");
                intl_ns_map.define("@@toStringTag", intltag, ro_attr);
                self.intl_ns = self.setup_object(intl_ns_map);
            }
            let dataview_ctor = self.setup_build(&[], Some(dataview_proto));
            self.dataview_ctor = dataview_ctor;
            self.setup_define(dataview_proto, "constructor", Value::heap(dataview_ctor), method_attr);
            // Capture only after the prototype is complete. The first eight
            // entries are the append-only non-BigInt numeric getters. Hidden
            // shape equality later proves their keys and descriptors, while
            // exact Value bits prove the live callable identity.
            if let HeapObj::Object(m) = self.heap.get(dataview_proto) {
                let shape = m.shape();
                let mut bits = [Value::UNDEFINED.bits(); 8];
                let mut valid = m.shape_guardable()
                    && m.slot_len() >= bits.len()
                    && native::DV_PROTO_METHODS.len() >= bits.len();
                if valid {
                    for method in 0..bits.len() {
                        let value = m.val_at(method);
                        let intrinsic = value.is_heap()
                            && matches!(
                                self.heap.get(value.heap_index()),
                                HeapObj::Native(id)
                                    if *id == native::DV_METHOD_BASE + method as u16
                            );
                        if !intrinsic
                            || m.key_at(method) != native::DV_PROTO_METHODS[method]
                            || m.attr_at(method).accessor
                        {
                            valid = false;
                            break;
                        }
                        bits[method] = value.bits();
                    }
                }
                // The numeric setters, entries 10..18, the same way.
                let mut set_bits = [Value::UNDEFINED.bits(); 8];
                if valid && m.slot_len() >= 18 && native::DV_PROTO_METHODS.len() >= 18 {
                    for (j, slot) in (10..18).enumerate() {
                        let value = m.val_at(slot);
                        let intrinsic = value.is_heap()
                            && matches!(
                                self.heap.get(value.heap_index()),
                                HeapObj::Native(id)
                                    if *id == native::DV_METHOD_BASE + slot as u16
                            );
                        if !intrinsic
                            || m.key_at(slot) != native::DV_PROTO_METHODS[slot]
                            || m.attr_at(slot).accessor
                        {
                            valid = false;
                            break;
                        }
                        set_bits[j] = value.bits();
                    }
                } else {
                    valid = false;
                }
                // The BigInt accessors, entries 8, 9, 18 and 19, the same way.
                let mut big_bits = [Value::UNDEFINED.bits(); 4];
                if valid && m.slot_len() >= 20 && native::DV_PROTO_METHODS.len() >= 20 {
                    for (j, slot) in [8usize, 9, 18, 19].into_iter().enumerate() {
                        let value = m.val_at(slot);
                        let intrinsic = value.is_heap()
                            && matches!(
                                self.heap.get(value.heap_index()),
                                HeapObj::Native(id)
                                    if *id == native::DV_METHOD_BASE + slot as u16
                            );
                        if !intrinsic
                            || m.key_at(slot) != native::DV_PROTO_METHODS[slot]
                            || m.attr_at(slot).accessor
                        {
                            valid = false;
                            break;
                        }
                        big_bits[j] = value.bits();
                    }
                } else {
                    valid = false;
                }
                debug_assert!(valid, "DataView prototype boot layout drifted");
                if valid {
                    self.dataview_proto_shape = shape;
                    self.dataview_numeric_getter_bits = bits;
                    self.dataview_numeric_setter_bits = set_bits;
                    self.dataview_bigint_bits = big_bits;
                }
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
            (self.text_encoder_proto, text_encoder_ctor),
            (self.text_decoder_proto, text_decoder_ctor),
        ] {
            if proto != 0 {
                let cv = Value::heap(ctor);
                self.setup_define(proto, "constructor", cv, method_attr);
            }
        }
        let json_ctor = self.setup_build(
            &[
                ("parse", JSON_PARSE),
                ("stringify", JSON_STRINGIFY),
                ("rawJSON", JSON_RAW_JSON),
                ("isRawJSON", JSON_IS_RAW_JSON),
            ],
            None,
        );
        self.setup_define_tag(json_ctor, "JSON");
        let math_ctor = {
            let mut m = ObjMap::new();
            for (i, &(name, _, _)) in native::MATH_METHODS.iter().enumerate() {
                let nv = self.setup_native(native::MATH_METHOD_BASE + i as u16);
                m.define(name, nv, method_attr);
            }
            for &(name, id) in &[
                ("random", MATH_RANDOM),
                ("sumPrecise", MATH_SUM_PRECISE),
                ("f16round", MATH_F16ROUND),
            ] {
                let nv = self.setup_native(id);
                m.define(name, nv, method_attr);
            }
            const CONSTS: &[(&str, f64)] = &[
                ("E", std::f64::consts::E),
                ("LN10", std::f64::consts::LN_10),
                ("LN2", std::f64::consts::LN_2),
                ("LOG10E", std::f64::consts::LOG10_E),
                ("LOG2E", std::f64::consts::LOG2_E),
                ("PI", std::f64::consts::PI),
                ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
                ("SQRT2", std::f64::consts::SQRT_2),
            ];
            for &(n, v) in CONSTS {
                m.define(n, Value::num(v), proto_attr);
            }
            let mtag = self.setup_str("Math");
            m.define("@@toStringTag", mtag, ro_attr);
            self.setup_object(m)
        };
        let atomics_ns = {
            let mut m = ObjMap::new();
            for (i, &(name, _)) in native::ATOMICS_METHODS.iter().enumerate() {
                let nv = self.setup_native(native::ATOMICS_BASE + i as u16);
                m.define(name, nv, method_attr);
            }
            let idx = self.setup_object(m);
            self.setup_link_proto(idx, obj_proto);
            self.setup_define_tag(idx, "Atomics");
            idx
        };
        #[cfg(feature = "webassembly")]
        let wasm_ns = {
            let names: Vec<Value> = crate::vm::webassembly::MEMBERS
                .iter()
                .map(|name| self.setup_str(name))
                .collect();
            let idx = self.wasm_namespace(obj_proto, &names);
            self.setup_define_tag(idx, "WebAssembly");
            idx
        };
        {
            let (p, ctor) = self.setup_disposable_stack(
                &[
                    ("use", native::DISPOSABLE_USE),
                    ("adopt", native::DISPOSABLE_ADOPT),
                    ("defer", native::DISPOSABLE_DEFER),
                    ("dispose", native::DISPOSABLE_DISPOSE),
                    ("move", native::DISPOSABLE_MOVE),
                ],
                "@@dispose",
                "dispose",
                "DisposableStack",
            );
            self.disposablestack_proto = p;
            self.disposablestack_ctor = ctor;
        }
        {
            let (p, ctor) = self.setup_disposable_stack(
                &[
                    ("use", native::ASYNC_DISPOSABLE_USE),
                    ("adopt", native::ASYNC_DISPOSABLE_ADOPT),
                    ("defer", native::ASYNC_DISPOSABLE_DEFER),
                    ("disposeAsync", native::DISPOSABLE_DISPOSE_ASYNC),
                    ("move", native::ASYNC_DISPOSABLE_MOVE),
                ],
                "@@asyncDispose",
                "disposeAsync",
                "AsyncDisposableStack",
            );
            self.asyncdisposablestack_proto = p;
            self.asyncdisposablestack_ctor = ctor;
        }
        {
            let p = self.setup_build(&[], None);
            if self.error_protos[0] != 0 {
                self.setup_link_proto(p, self.error_protos[0]);
            }
            let name_v = self.setup_str("SuppressedError");
            let empty_v = self.setup_str("");
            self.setup_define(p, "name", name_v, method_attr);
            self.setup_define(p, "message", empty_v, method_attr);
            self.suppressederror_proto = p;
            let ctor = self.setup_build(&[], Some(p));
            if self.error_ctors[0] != 0 {
                self.setup_link_proto(ctor, self.error_ctors[0]);
            }
            self.suppressederror_ctor = ctor;
            self.setup_define(p, "constructor", Value::heap(ctor), method_attr);
        }
        {
            let p = self.setup_build(
                &[
                    ("evaluate", native::SHADOWREALM_EVALUATE),
                    ("importValue", native::SHADOWREALM_IMPORTVALUE),
                ],
                None,
            );
            self.setup_link_proto(p, obj_proto);
            self.setup_define_tag(p, "ShadowRealm");
            self.shadowrealm_proto = p;
            let ctor = self.setup_build(&[], Some(p));
            self.shadowrealm_ctor = ctor;
            self.setup_define(p, "constructor", Value::heap(ctor), method_attr);
        }
        {
            let p = self.setup_build(&[], None);
            self.setup_link_proto(p, obj_proto);
            self.setup_define_getter(p, "@@toStringTag", native::ABSTRACT_MODULE_SOURCE_TAG_GET);
            self.abstractmodulesource_proto = p;
            let ctor = self.setup_build(&[], Some(p));
            self.setup_link_proto(ctor, self.fn_proto);
            let name_v = self.setup_str("AbstractModuleSource");
            self.setup_define(ctor, "name", name_v, ro_attr);
            self.setup_define(ctor, "length", Value::num(0.0), ro_attr);
            self.abstractmodulesource_ctor = ctor;
            self.setup_define(p, "constructor", Value::heap(ctor), method_attr);
            let host = self.setup_build(&[], None);
            self.setup_link_proto(host, p);
            self.module_source_proto = host;
        }
        let is_nan_fn = self.heap.alloc(HeapObj::Native(GLOBAL_IS_NAN));
        let is_finite_fn = self.heap.alloc(HeapObj::Native(GLOBAL_IS_FINITE));
        self.global_fn_intrinsics = [
            number_ctor,
            string_ctor,
            boolean_ctor,
            parse_int_fn,
            parse_float_fn,
            is_nan_fn,
            is_finite_fn,
        ];
        let eval_fn = self.heap.alloc(HeapObj::Native(GLOBAL_EVAL));
        self.eval_fn_idx = eval_fn;
        let encode_uri_fn = self.heap.alloc(HeapObj::Native(GLOBAL_ENCODE_URI));
        let encode_uri_component_fn = self
            .heap
            .alloc(HeapObj::Native(GLOBAL_ENCODE_URI_COMPONENT));
        let decode_uri_fn = self.heap.alloc(HeapObj::Native(GLOBAL_DECODE_URI));
        let decode_uri_component_fn = self
            .heap
            .alloc(HeapObj::Native(GLOBAL_DECODE_URI_COMPONENT));
        let escape_fn = self.heap.alloc(HeapObj::Native(GLOBAL_ESCAPE));
        let unescape_fn = self.heap.alloc(HeapObj::Native(GLOBAL_UNESCAPE));
        let print_fn = self.heap.alloc(HeapObj::Native(native::GLOBAL_PRINT));
        let console_obj = self.setup_build(
            &[
                ("log", CONSOLE_LOG),
                ("info", CONSOLE_INFO),
                ("warn", CONSOLE_WARN),
                ("error", CONSOLE_ERROR),
                ("debug", CONSOLE_DEBUG),
            ],
            None,
        );
        let performance_obj = self.setup_build(&[("now", native::PERFORMANCE_NOW)], None);
        let time_origin = crate::vm::clock::now_epoch_ns() as f64 / 1e6
            - (crate::vm::clock::now_mono_ms() - self.start_mono_ms);
        self.setup_define(performance_obj, "timeOrigin", Value::num(time_origin), ro_attr);
        // The Annex-B call-assignment-target ReferenceError thrower (lib.rs
        // parse-retry rewrite). Resolvable as a global under its reserved name
        // only; hidden from globalThis own-keys reflection (props.rs).
        let annexb_ref_error_fn = self.heap.alloc(HeapObj::Native(native::ANNEXB_REF_ERROR));
        // The embedder trampoline (`crate::embed`). Like the Annex-B helper it is
        // a RESERVED internal name, not a spec global: resolvable when named, but
        // hidden from globalThis own-keys reflection so `Object.keys(globalThis)`
        // and the test262 global-property tests are unaffected. With no host
        // installed the native itself throws, so a stock `zipp js` run cannot
        // reach anything through it.
        let host_call_fn = self.heap.alloc(HeapObj::Native(native::HOST_CALL));
        // `globalThis`: an empty Object whose property access is routed to the
        // global slots by name (see get_prop/set_prop/has_own_property).
        let global_this = self.setup_object(ObjMap::new());
        self.global_this = global_this;
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        {
            self.global_this_bits = Value::heap(global_this).bits();
        }
        self.heap.pin_mirror_dict(global_this);
        // The test262 `$262` host object: { global, detachArrayBuffer, gc }.
        let d262_detach = self.setup_native(DOLLAR262_DETACH);
        let d262_gc = self.setup_native(DOLLAR262_GC);
        let d262_create_realm =
            self.setup_native(DOLLAR262_CREATE_REALM);
        // `$262.IsHTMLDDA` — the `document.all` [[IsHTMLDDA]] emulation: an ordinary
        // object tagged in `is_htmldda` (typeof "undefined", == null/undefined,
        // falsy, callable→undefined).
        let d262_htmldda = self.setup_object(ObjMap::new());
        let inserted = self.is_htmldda.insert(d262_htmldda);
        debug_assert!(inserted);
        self.htmldda_idx = if self.is_htmldda.len() == 1 {
            d262_htmldda
        } else {
            u32::MAX
        };
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        {
            self.typeof_special_max = if self.htmldda_scalar_enabled && self.is_htmldda.len() <= 1 {
                let mut m = self.fn_proto.max(self.symbol_ctor).max(self.bigint_ctor);
                if self.htmldda_idx != u32::MAX {
                    m = m.max(self.htmldda_idx);
                }
                m
            } else {
                u32::MAX
            };
            self.heap.refresh_mirror(self.fn_proto);
            if let HeapObj::Object(m) = self.heap.get(self.fn_proto) {
                let sh = m.shape();
                if sh != crate::shape::DICT {
                    if let Some(slot) = m.pos("call") {
                        if !m.attr_at(slot).accessor {
                            self.fn_proto_shape = sh;
                            self.fn_proto_call_slot = slot as u32;
                            self.fn_call_native_bits = m.val_at(slot).bits();
                        }
                    }
                    if let Some(slot) = m.pos("apply") {
                        if !m.attr_at(slot).accessor {
                            self.fn_proto_shape = sh;
                            self.fn_proto_apply_slot = slot as u32;
                            self.fn_apply_native_bits = m.val_at(slot).bits();
                        }
                    }
                }
            }
            if self.array_ctor != 0 {
                if let HeapObj::Object(m) = self.heap.get(self.array_ctor) {
                    if let Some(v) = m.get("isArray") {
                        self.array_ctor_bits = Value::heap(self.array_ctor).bits();
                        self.array_is_array_bits = v.bits();
                    }
                }
            }
            if self.obj_proto != 0 {
                // Recorded as a guarded chain member's (`Heap::guard_version`),
                // which `%Object.prototype%` becomes with the first cached
                // prototype chain: the mark changes the version word, and
                // this record is never taken again. Recorded unmarked, it
                // stopped matching at that first chain, and from then on no
                // read of a missing property was answered from the shape
                // memo (`jit_fill_absent_ic`, `emit_mega_get_prop_stub`):
                // each one called the miss helper, 66 ns for five such reads
                // of a pattern object where they now take 23.
                self.obj_proto_setup_ver = self.heap.guard_version(self.obj_proto);
            }
            if self.symbol_ctor != 0 {
                if let HeapObj::Object(m) = self.heap.get(self.symbol_ctor) {
                    if let Some(v) = m.get("iterator") {
                        self.sym_iterator_bits = v.bits();
                    }
                }
            }
            if self.arr_proto != 0 && self.default_array_iter.is_heap() {
                if let HeapObj::Object(m) = self.heap.get(self.arr_proto) {
                    if let Some(slot) = m.pos("@@iterator") {
                        if !m.attr_at(slot).accessor
                            && m.val_at(slot).bits() == self.default_array_iter.bits()
                        {
                            self.arr_proto_iter_slot_ptr =
                                m.vals_ptr_raw().wrapping_add(slot) as u64;
                            // As `obj_proto_setup_ver`: the marked word.
                            self.arr_proto_iter_ver = self.heap.guard_version(self.arr_proto);
                        }
                    }
                }
            }
        }
        // `$262.agent`: the full multi-agent surface (vm/agents.rs).
        let agent_obj_idx = self.setup_build(
            &[
                ("monotonicNow", AGENT_MONOTONIC_NOW),
                ("setTimeout", AGENT_SET_TIMEOUT),
                ("getReport", AGENT_GET_REPORT),
                ("sleep", AGENT_SLEEP),
                ("start", AGENT_START),
                ("broadcast", AGENT_BROADCAST),
                ("receiveBroadcast", AGENT_RECEIVE_BROADCAST),
                ("report", AGENT_REPORT),
                ("leaving", AGENT_LEAVING),
            ],
            None,
        );
        let ag_st = self
            .setup_prop_val(agent_obj_idx, "setTimeout")
            .unwrap_or(Value::UNDEFINED);
        let agent_obj = Value::heap(agent_obj_idx);
        let d262_eval_script = self.setup_native(DOLLAR262_EVAL_SCRIPT);
        let mut d262 = ObjMap::new();
        d262.define("agent", agent_obj, method_attr);
        d262.define("evalScript", d262_eval_script, method_attr);
        d262.define("global", Value::heap(global_this), method_attr);
        d262.define("detachArrayBuffer", d262_detach, method_attr);
        d262.define("gc", d262_gc, method_attr);
        d262.define("createRealm", d262_create_realm, method_attr);
        d262.define("IsHTMLDDA", Value::heap(d262_htmldda), method_attr);
        d262.define(
            "AbstractModuleSource",
            Value::heap(self.abstractmodulesource_ctor),
            method_attr,
        );
        self.dollar262 = self.setup_object(d262);
        // Inject into the reserved global slots (collect first to end the program
        // borrow before mutating `self.globals`).
        // Every builtin global NAME → its heap value, built ONCE and recorded in
        // `builtin_globals` regardless of whether the running program referenced
        // it — so eval'd code can resolve a builtin the program never named.
        let mut all: Vec<(&str, u32)> = vec![
            // Host timer: the same native as `$262.agent.setTimeout`. Without
            // a global binding, atomicsHelper.js installs a Promise-chain
            // busy-spin fallback that starves the timer/waiter event-loop
            // phases (waitAsync 'timed-out' never fires).
            ("setTimeout", ag_st.heap_index()),
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
            ("TextEncoder", text_encoder_ctor),
            ("TextDecoder", text_decoder_ctor),
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
            #[cfg(not(feature = "wasm-lite"))]
            ("Temporal", self.temporal_ns),
            #[cfg(not(feature = "wasm-lite"))]
            ("Intl", self.intl_ns),
            ("Atomics", atomics_ns),
            #[cfg(feature = "webassembly")]
            ("WebAssembly", wasm_ns),
            ("DisposableStack", self.disposablestack_ctor),
            ("AsyncDisposableStack", self.asyncdisposablestack_ctor),
            ("SuppressedError", self.suppressederror_ctor),
            ("ShadowRealm", self.shadowrealm_ctor),
            ("parseInt", parse_int_fn),
            ("parseFloat", parse_float_fn),
            ("isNaN", is_nan_fn),
            ("isFinite", is_finite_fn),
            ("encodeURI", encode_uri_fn),
            ("encodeURIComponent", encode_uri_component_fn),
            ("decodeURI", decode_uri_fn),
            ("decodeURIComponent", decode_uri_component_fn),
            ("escape", escape_fn),
            ("unescape", unescape_fn),
            ("print", print_fn),
            ("console", console_obj),
            ("performance", performance_obj),
            ("eval", eval_fn),
            ("globalThis", global_this),
            (native::ANNEXB_REF_ERROR_NAME, annexb_ref_error_fn),
            (native::HOST_CALL_NAME, host_call_fn),
        ];
        // The WebAssembly engines' guest surface has no shared-memory globals
        // (the host boundary contract lists them with process, require, fetch
        // and WebAssembly). The native safe-sandbox profile keeps them, backed
        // by agent-local bytes (#61).
        #[cfg(feature = "wasm-single-agent")]
        all.retain(|(name, _)| !matches!(*name, "SharedArrayBuffer" | "Atomics"));
        // The test262 harness object, only when the host asked for it. Omitted
        // rather than zeroed: the loop below installs whatever index it is
        // handed, so a sentinel would bind `$262` to heap slot 0.
        if self.host_262 {
            all.push(("$262", self.dollar262));
        }
        // The 12 TypedArray constructors (Int8Array … Float16Array).
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
                    "Map"
                    | "Set"
                    | "WeakMap"
                    | "WeakSet"
                    | "Iterator"
                    | "Symbol"
                    | "DisposableStack"
                    | "AsyncDisposableStack"
                    | "ShadowRealm"
                    | "TextEncoder"
                    | "TextDecoder" => 0.0,
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
                self.setup_define(v, "length", Value::num(len), fn_attr);
                self.setup_define(v, "name", nm, fn_attr);
            }
            self.builtin_globals.insert(name.to_string(), v);
            if let Some(i) = crate::vm::helpers_misc::builtin_ns_index(name) {
                self.builtin_ns_slots[i] = v;
            }
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
                self.bump_global_gen(slot as u32);
            }
        }
        // The PHP / Rust / Python runtime private natives: bound by slot ONLY
        // when the matching `Program::*_natives` flag is set.
        #[cfg(feature = "php")]
        if self.program.php_natives {
            self.setup_bind_lang_natives(&[
                (native::PHP_OUTPUT_NAME, native::PHP_OUTPUT),
                (native::PHP_INCLUDE_NAME, native::PHP_INCLUDE),
                (native::PHP_FS_NAME, native::PHP_FS),
                (native::PHP_CRYPTO_NAME, native::PHP_CRYPTO),
                (native::PHP_ENV_NAME, native::PHP_ENV),
                (native::PHP_SAPI_NAME, native::PHP_SAPI),
                (native::PHP_MAP_KV_NAME, native::PHP_MAP_KV),
                (native::PHP_SQL_NAME, native::PHP_SQL),
            ]);
        }
        #[cfg(feature = "rust")]
        if self.program.rust_natives {
            self.setup_bind_lang_natives(&[
                (native::RS_IO_NAME, native::RS_IO),
                (native::RS_FS_NAME, native::RS_FS),
                (native::RS_ENV_NAME, native::RS_ENV),
                (native::RS_TIME_NAME, native::RS_TIME),
                (native::RS_NET_NAME, native::RS_NET),
                (native::RS_CRYPTO_NAME, native::RS_CRYPTO),
                (native::RS_PROC_NAME, native::RS_PROC),
                (native::RS_SQLITE_NAME, native::RS_SQLITE),
            ]);
        }
        #[cfg(feature = "python")]
        if self.program.python_natives {
            self.setup_bind_lang_natives(&[
                (native::PY_TENSOR_NAME, native::PY_TENSOR),
                (native::PY_COMPILE_NAME, native::PY_COMPILE),
                (native::PY_VFS_READ_NAME, native::PY_VFS_READ),
                (native::PY_PKG_CALL_NAME, native::PY_PKG_CALL),
                (native::PY_BIND_NAME, native::PY_BIND),
                (native::PY_STR_NAME, native::PY_STR),
                (native::PY_ORD_NAME, native::PY_ORD),
                (native::PY_GEN_NAME, native::PY_GEN),
                (native::PY_JSON_NAME, native::PY_JSON),
                (native::PY_ZLIB_NAME, native::PY_ZLIB),
                (native::PY_BYTES_DECODE_NAME, native::PY_BYTES_DECODE),
                (native::PY_TEXTWRAP_NAME, native::PY_TEXTWRAP),
                (native::PY_YFSTEP_NAME, native::PY_YFSTEP),
                (native::PY_RE_FINDALL_NAME, native::PY_RE_FINDALL),
                (native::PY_WRITE_NAME, native::PY_WRITE),
                (native::PY_PCT_NAME, native::PY_PCT),
                (native::PY_STRM_NAME, native::PY_STRM),
                (native::PY_ITER_NAME, native::PY_ITER),
                (native::PY_EXC_NAME, native::PY_EXC),
                (native::PY_IN_NAME, native::PY_IN),
                (native::PY_SMFIND_NAME, native::PY_SMFIND),
                (native::PY_TABLE_NAME, native::PY_TABLE),
            ]);
        }
        {
            let species_get = self.setup_native(SPECIES_GET);
            for c in [self.ta_base_ctor, self.regexp_ctor, self.arraybuffer_ctor] {
                if c != 0 {
                    self.setup_define(c, "@@species", species_get, Self::SETUP_GETTER_ATTR);
                }
            }
            for n in ["Array", "Map", "Set", "Promise", "SharedArrayBuffer"] {
                if let Some(v) = self.global_by_name(n).filter(|v| v.is_heap()) {
                    let c = v.heap_index();
                    if c != 0 {
                        self.setup_define(c, "@@species", species_get, Self::SETUP_GETTER_ATTR);
                    }
                }
            }
        }
        // Snapshot exact main-realm RegExp protocol identities only after all
        // prototype Symbol methods, accessors, and the constructor species
        // getter have been installed. Realm copies reuse Native ids, so every
        // optimization that elides a dynamic call must compare these Values.
        self.capture_regexp_protocol_intrinsics();
        // B191: snapshot the primitive prototypes' method slots LAST, after
        // every install above — the name-dispatched fast paths prove against
        // exactly this state before serving an intrinsic.
        self.capture_proto_baselines();
    }
}

#[cfg(all(test, feature = "jit", target_arch = "x86_64", not(feature = "safe-sandbox")))]
mod setup_version_tests {
    use crate::vm::Vm;

    fn vm(source: &str) -> Vm<'static> {
        let ast = crate::front::parse_script(source).expect("source parses");
        let program = Box::leak(Box::new(
            crate::compile::compile_main_program(&ast, source).expect("source compiles"),
        ));
        let mut vm = Vm::new(program);
        vm.run().expect("program runs");
        vm
    }

    /// The versions recorded at setup still match after a program that
    /// caches prototype chains through `%Object.prototype%` and
    /// `%Array.prototype%` and changes neither. A cached chain marks its
    /// members (`Heap::guard_version`), which changes their version words:
    /// recorded unmarked, both records went stale at the first such chain,
    /// and what they guard (the shape memo's answer for a missing property,
    /// the pristine array iterator) was never used again.
    #[test]
    fn setup_versions_survive_cached_prototype_chains() {
        let vm = vm(
            "function read(o) { return o.missing === undefined ? o.toString() : ''; }
             function walk(a) { let n = 0; for (const v of a) n += v; return n + a.indexOf(2) + a.map((x) => x).length; }
             var out = 0;
             for (var i = 0; i < 4000; i++) {
               var o = i % 3 === 0 ? { a: i } : i % 3 === 1 ? { b: i, c: 1 } : { d: i, e: 2, f: 3 };
               out += read(o).length + walk([1, 2, 3]);
             }",
        );
        assert_ne!(vm.obj_proto, 0);
        assert_eq!(
            vm.heap.version_of(vm.obj_proto),
            vm.obj_proto_setup_ver,
            "%Object.prototype% is unchanged, so its setup record must still match"
        );
        assert_ne!(vm.arr_proto_iter_ver, u32::MAX, "setup recorded the array iterator");
        assert_eq!(
            vm.heap.version_of(vm.arr_proto),
            vm.arr_proto_iter_ver,
            "%Array.prototype% is unchanged, so its iterator record must still match"
        );
    }

    /// A change to either prototype still ends the match.
    #[test]
    fn setup_versions_end_when_a_prototype_changes() {
        let vm = vm("Object.prototype.extra = 1; Array.prototype.extra = 2;");
        assert_ne!(vm.heap.version_of(vm.obj_proto), vm.obj_proto_setup_ver);
        assert_ne!(vm.heap.version_of(vm.arr_proto), vm.arr_proto_iter_ver);
    }
}
