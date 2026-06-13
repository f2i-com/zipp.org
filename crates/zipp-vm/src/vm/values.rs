#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// `key in obj` — does `obj` have the property `key`? Own object keys, a
    /// class instance's inherited methods/getters, array indices / `length`,
    /// Map/Set `size`, and class static members. `in` on a primitive throws
    /// in JS; here it's `false` (rare).
    /// `[[HasProperty]]` for a fixed string key — walks the prototype chain
    /// (unlike has_own_property). Allocates a transient key string; used by
    /// ToPropertyDescriptor, where an inherited/accessor field counts as present.
    pub(crate) fn has_property_str(&mut self, obj: Value, key: &str) -> bool {
        let k = self.alloc_str(key.to_string());
        self.has_property(obj, k)
    }

    pub(crate) fn has_property(&self, obj: Value, key: Value) -> bool {
        if !obj.is_heap() {
            return false;
        }
        let idx = obj.heap_index();
        match self.heap.get(idx) {
            HeapObj::Object(map) => {
                let k = self.key_of(key);
                if map.get(&k).is_some() {
                    return true;
                }
                // %Array.prototype% is an Array exotic: its `length` is a real
                // own property (virtual, in arr_proto_len), so
                // `"length" in Object.create(Array.prototype)` is true.
                if k == "length" && idx == self.arr_proto && self.arr_proto != 0 {
                    return true;
                }
                // Inherited method/getter/setter through the class chain.
                let class = map.class;
                let mut cur = class;
                while let Some(cidx) = cur {
                    match self.heap.get(cidx) {
                        HeapObj::Class(c) => {
                            if c.methods.iter().any(|(n, _)| *n == k)
                                || c.getters.iter().any(|(n, _)| *n == k)
                                || c.setters.iter().any(|(n, _)| *n == k)
                            {
                                return true;
                            }
                            cur = c.parent;
                        }
                        _ => break,
                    }
                }
                // [[HasProperty]] continues up the prototype chain: an explicit
                // `Object.create` proto, then the base Object.prototype (which
                // carries toString/hasOwnProperty/valueOf/…). Mirrors get_member's
                // proto resolution, minus class-instance C.prototype (its methods
                // are already covered by the class-chain walk above).
                let proto = if let Some(&p) = self.proto_of.get(&idx) {
                    p.is_heap().then_some(p)
                } else if self.obj_proto != 0 && idx != self.obj_proto {
                    Some(Value::heap(self.obj_proto))
                } else {
                    None
                };
                match proto {
                    Some(p) => self.has_property(p, key),
                    None => false,
                }
            }
            HeapObj::Array(items) => {
                let len = items.len();
                // A canonical integer index: a numeric Value, or a canonical numeric
                // string ("0", not "01"/"-1").
                let int_index = array_index(key).or_else(|| {
                    let k = self.key_of(key);
                    match k.parse::<u32>() {
                        Ok(n) if n != u32::MAX && n.to_string() == k => Some(n as usize),
                        _ => None,
                    }
                });
                if let Some(i) = int_index {
                    // An in-range slot is present iff it is not a hole.
                    if i < len && !items[i].is_hole() {
                        return true;
                    }
                    // A hole OR an out-of-range index is not an own element, but it may
                    // be overridden (a defineProperty'd index in arr_props) or inherited
                    // from the prototype chain — [[HasProperty]] must keep walking (an
                    // out-of-range `i` was previously reported absent without this check).
                    // Only materialize the index-key string when there IS an arr_props
                    // overlay to probe: a packed array with delete-punched holes has none,
                    // so a hole/OOB `i in arr` skips the per-probe key alloc entirely (the
                    // hot hole-aware-iteration path) and goes straight to the prototype.
                    if let Some(m) = self.arr_props.get(&idx) {
                        let k = self.key_of(key);
                        if m.pos(&k).is_some() {
                            return true;
                        }
                    }
                    // Walk the ACTUAL [[Prototype]] (a setPrototypeOf custom proto
                    // carries inherited indices), else %Array.prototype%.
                    let proto = match self.proto_of.get(&idx) {
                        Some(&p) => p,
                        None if self.arr_proto != 0 => Value::heap(self.arr_proto),
                        None => Value::NULL,
                    };
                    return proto.is_heap() && self.has_property(proto, key);
                }
                let k = self.key_of(key);
                if k == "length" {
                    return true;
                }
                if self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some()) {
                    return true;
                }
                // Inherited: the actual [[Prototype]] (custom via setPrototypeOf),
                // else Array.prototype (push/map/…) then Object.prototype.
                let proto = match self.proto_of.get(&idx) {
                    Some(&p) => p,
                    None if self.arr_proto != 0 => Value::heap(self.arr_proto),
                    None => Value::NULL,
                };
                proto.is_heap() && self.has_property(proto, key)
            }
            HeapObj::Str(s) => match array_index(key) {
                Some(i) => i < s.units(),
                None => self.display(key) == "length",
            },
            HeapObj::Cons { len, .. } => match array_index(key) {
                Some(i) => i < *len,
                None => self.display(key) == "length",
            },
            // A TypedArray's integer-indexed exotic own properties (`0 in ta`),
            // then any named own prop (`ta.constructor` override in arr_props),
            // then the %TypedArray%.prototype chain (`"subarray" in ta`).
            HeapObj::TypedArray { .. } => {
                let k = self.key_of(key);
                // A CanonicalNumericIndexString is absorbed by the integer-indexed
                // exotic [[HasProperty]]: present iff it's a VALID integer index,
                // and never inherited from the prototype (so `TA.prototype[5]`
                // does not make `5 in ta` true on a shorter array).
                if self.is_canonical_numeric_index(&k) {
                    return self.ta_valid_index(idx, &k).is_some();
                }
                if self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some()) {
                    return true;
                }
                match self.proto_of.get(&idx).copied().filter(|p| p.is_heap()) {
                    Some(p) => self.has_property(p, key),
                    None => false,
                }
            }
            HeapObj::Map { .. } | HeapObj::Set(_) => self.display(key) == "size",
            // Static members (data + `static get`/`set` accessors) are own
            // properties of the class value and are inherited up the chain.
            HeapObj::Class(_) => {
                let k = self.key_of(key);
                // A class always owns `prototype`.
                if k == "prototype" && self.callable_has_prototype(obj) {
                    return true;
                }
                let mut cur = Some(idx);
                while let Some(cidx) = cur {
                    match self.heap.get(cidx) {
                        HeapObj::Class(c) => {
                            if c.statics.get(&k).is_some()
                                || c.static_getters.iter().any(|(n, _)| *n == k)
                                || c.static_setters.iter().any(|(n, _)| *n == k)
                            {
                                return true;
                            }
                            cur = c.parent;
                        }
                        _ => break,
                    }
                }
                false
            }
            _ => {
                let k = self.key_of(key);
                // Exotic objects (boxed primitives, Date, Promise, RegExp, Weak*,
                // …) keep their named own props in the arr_props side table.
                if self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some()) {
                    return true;
                }
                // A RegExp instance's `lastIndex` is an own data property held
                // in the heap object itself (not arr_props) —
                // `"lastIndex" in re` is true.
                if k == "lastIndex" && matches!(self.heap.get(idx), HeapObj::RegExp { .. }) {
                    return true;
                }
                // A callable's assigned own properties (`fn.x = …`) live in the
                // SEPARATE fn_props side table — `"x" in fn` must see them too
                // (mirrors get_own_property; read_descriptor relies on this for a
                // Function-object descriptor).
                if matches!(
                    self.heap.get(idx),
                    HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_)
                ) && self.fn_props.get(&idx).map_or(false, |m| m.pos(&k).is_some())
                {
                    return true;
                }
                // A function's synthesized `prototype` own property (ordinary
                // functions + generators; not arrows/methods/async/bound/native).
                if k == "prototype" && self.callable_has_prototype(obj) {
                    return true;
                }
                // A boxed String wrapper exposes the wrapped string's chars +
                // `length` as integer-indexed own properties.
                if let HeapObj::Boxed { kind: 0, value } = self.heap.get(idx) {
                    let v = *value;
                    let clen = match self.heap.get(v.heap_index()) {
                        HeapObj::Str(s) => Some(s.units()),
                        HeapObj::Cons { len, .. } => Some(*len),
                        _ => None,
                    };
                    if let Some(n) = clen {
                        if let Some(i) = array_index(key) {
                            if i < n {
                                return true;
                            }
                        }
                        if k == "length" {
                            return true;
                        }
                    }
                }
                // The prototype chain: an explicit `proto_of`, else the intrinsic
                // prototype for this object's kind — so inherited methods/accessors
                // are visible to `in` (`"toFixed" in Object(2.5)`, `"call" in fn`),
                // mirroring get_member. Without this fallback `in` saw only own
                // props on a boxed primitive / callable / Date / Promise / …
                let proto = if let Some(&p) = self.proto_of.get(&idx) {
                    p.is_heap().then_some(p)
                } else {
                    let bp = match self.heap.get(idx) {
                        HeapObj::Func(_)
                        | HeapObj::Closure { .. }
                        | HeapObj::Bound { .. }
                        | HeapObj::Native(_) => self.fn_proto,
                        HeapObj::Boxed { kind: 0, .. } => self.str_proto,
                        HeapObj::Boxed { kind: 1, .. } => self.num_proto,
                        HeapObj::Boxed { kind: 2, .. } => self.bool_proto,
                        HeapObj::Boxed { kind: 3, .. } => self.symbol_proto,
                        HeapObj::Boxed { kind: 4, .. } => self.bigint_proto,
                        HeapObj::Date(_) => self.date_proto,
                        HeapObj::Promise { .. } => self.promise_proto,
                        HeapObj::RegExp { .. } => self.regexp_proto,
                        HeapObj::WeakMap { .. } => self.weakmap_proto,
                        HeapObj::WeakSet(_) => self.weakset_proto,
                        HeapObj::WeakRef(_) => self.weakref_proto,
                        HeapObj::FinalizationRegistry { .. } => self.finreg_proto,
                        _ => 0,
                    };
                    (bp != 0).then_some(Value::heap(bp))
                };
                match proto {
                    Some(p) => self.has_property(p, key),
                    None => false,
                }
            }
        }
    }

    /// The ORDERED lexical private-brand chain of the class body currently
    /// executing — resolved from the running frame's callee (a method/getter/setter
    /// VALUE, or the class value for the ctor/field-init/static block; both recorded
    /// in `method_brand` at MakeClass). `None` outside a class body.
    pub(crate) fn current_private_brands(&self) -> Option<&Vec<u64>> {
        let callee = self.frames.last()?.callee;
        if !callee.is_heap() {
            return None;
        }
        self.method_brand.get(&callee.heap_index())
    }

    /// Whether `receiver` was constructed by a class evaluation carrying `brand`
    /// (or one of its ancestors) — i.e. the private brand is installed on it.
    /// Walks the instance's class chain collecting each ClassData.private_brand.
    pub(crate) fn instance_has_brand(&self, receiver: Value, brand: u64) -> bool {
        if !receiver.is_heap() {
            return false;
        }
        // A static private member's receiver IS the class value: walk its own
        // chain (the lenient textual paths accept ancestors; the precise
        // static check is private_receiver_ok's receiver==owner).
        if matches!(self.heap.get(receiver.heap_index()), HeapObj::Class(_)) {
            let mut cur = Some(receiver.heap_index());
            while let Some(cidx) = cur {
                match self.heap.get(cidx) {
                    HeapObj::Class(c) => {
                        if c.private_brand == brand {
                            return true;
                        }
                        cur = c.parent;
                    }
                    _ => break,
                }
            }
        }
        // An INSTANCE carries exactly the brands explicitly installed on it —
        // each class in the construction chain installs its own at ITS
        // InitializeInstanceElements point (base: ctor entry; derived: its
        // super() completion). Before that point the class's privates are NOT
        // yet on the instance (prod-private-*-before-super-return), even
        // though map.class already links to the class.
        self.instance_brand.get(&receiver.heap_index()).is_some_and(|bs| bs.contains(&brand))
    }

    /// PrivateBrandAdd: install a class's own private brand on an instance at
    /// its InitializeInstanceElements point. Only classes that DECLARE private
    /// members brand (keeps the side map empty on the hot path).
    pub(crate) fn brand_instance(&mut self, inst: Value, classval: Value) {
        if !inst.is_heap() || !classval.is_heap() {
            return;
        }
        let own = self.method_brand.get(&classval.heap_index()).and_then(|c| c.first()).copied();
        if let Some(own) = own {
            if !self.brand_private_names.contains_key(&own) {
                return;
            }
            let e = self.instance_brand.entry(inst.heap_index()).or_default();
            if !e.contains(&own) {
                e.push(own);
            }
        }
    }

    /// Infallible extensibility of a plain heap target (no proxy traps): an
    /// Object's flag; string/symbol/bigint primitives are never extensible
    /// targets; everything else (Array/Class/Date/…) keeps its flag in the
    /// arr_props side table (where preventExtensions stores it).
    pub(crate) fn target_is_extensible(&self, v: Value) -> bool {
        if !v.is_heap() {
            return true;
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Object(m) => m.extensible,
            HeapObj::Str(_)
            | HeapObj::Cons { .. }
            | HeapObj::Symbol { .. }
            | HeapObj::BigInt(_)
            | HeapObj::BigIntBig(_) => false,
            _ => self.arr_props.get(&v.heap_index()).map_or(true, |m| m.extensible),
        }
    }

    /// Checked InitializeInstanceElements / PrivateBrandAdd / PrivateFieldAdd
    /// entry: adding `classval`'s private elements to `inst` (when the class
    /// declares any) throws TypeError if `inst` ALREADY carries this class's
    /// own brand (double initialization via return-override — spec
    /// PrivateMethodOrAccessorAdd/PrivateFieldAdd step "entry is not empty"),
    /// or if `inst` is non-extensible (nonextensible-applies-to-private).
    /// `is_override` gates the duplicate check: a NORMAL instance's map.class
    /// chain carries the constructing class's brand by construction (that is
    /// the pending initialization, not a completed one).
    pub(crate) fn private_init_checked(
        &mut self,
        inst: Value,
        classval: Value,
        is_override: bool,
    ) -> Result<(), Thrown> {
        if !inst.is_heap() || !classval.is_heap() {
            return Ok(());
        }
        let own = match self.method_brand.get(&classval.heap_index()).and_then(|c| c.first()) {
            Some(&b) => b,
            None => return Ok(()),
        };
        let has_instance_privates = self
            .brand_private_names
            .get(&own)
            .is_some_and(|ns| ns.iter().any(|(_, k)| k & 8 == 0));
        if has_instance_privates {
            if is_override && self.instance_has_brand(inst, own) {
                return Err(Thrown(
                    "TypeError: cannot initialize the same private elements twice on an object"
                        .into(),
                ));
            }
            if let HeapObj::Object(m) = self.heap.get(inst.heap_index()) {
                if !m.extensible {
                    return Err(Thrown(
                        "TypeError: cannot define private elements on a non-extensible object"
                            .into(),
                    ));
                }
            }
        }
        self.brand_instance(inst, classval);
        Ok(())
    }

    /// The activation's EvalScope, CREATING it on the frame when the running
    /// function contains a sloppy function-context direct eval: closures made
    /// BEFORE the eval call must share the scope the eval later populates.
    pub(crate) fn ensure_frame_eval_scope(&mut self, frame_idx: usize) -> Option<u32> {
        if let Some(sc) = self.frame_eval_scope(frame_idx) {
            return Some(sc);
        }
        if self.func_has_sloppy_eval(self.frames[frame_idx].func as usize) {
            let s = self
                .heap
                .alloc(HeapObj::EvalScope(std::collections::HashMap::new()));
            self.frames[frame_idx].eval_scope = s;
            return Some(s);
        }
        None
    }

    /// Memoised: does this function's code contain a DirectEval whose var
    /// environment is a FUNCTION activation (sloppy, non-global var env)?
    fn func_has_sloppy_eval(&mut self, func_id: usize) -> bool {
        if func_id >= self.sloppy_eval_memo.len() {
            self.sloppy_eval_memo.resize(func_id + 1, 0);
        }
        match self.sloppy_eval_memo[func_id] {
            1 => false,
            2 => true,
            _ => {
                let has = self.func(func_id).code.iter().any(|i| {
                    matches!(
                        i,
                        crate::bytecode::Instr::DirectEval {
                            var_env_is_global: false,
                            strict_caller: false,
                            ..
                        }
                    )
                });
                self.sloppy_eval_memo[func_id] = if has { 2 } else { 1 };
                has
            }
        }
    }

    /// The running activation's dynamic EvalScope: the frame's own, or the
    /// one stamped on its callee (closures created under an eval scope).
    pub(crate) fn frame_eval_scope(&self, frame_idx: usize) -> Option<u32> {
        let f = &self.frames[frame_idx];
        if f.eval_scope != u32::MAX {
            return Some(f.eval_scope);
        }
        if f.callee.is_heap() {
            return self.closure_eval_scope.get(&f.callee.heap_index()).copied();
        }
        None
    }

    /// Dynamic-first lookup for the LoadGlobalDyn family: the current
    /// activation's EvalScope binding for the slot's NAME, if any.
    pub(crate) fn eval_scope_lookup(&self, idx: u32) -> Option<Value> {
        let fi = self.frames.len().checked_sub(1)?;
        let sc = self.frame_eval_scope(fi)?;
        let name = self.global_slot_name(idx)?;
        match self.heap.get(sc) {
            HeapObj::EvalScope(m) => m.get(&name).copied(),
            _ => None,
        }
    }

    /// Dynamic-first store: write an EXISTING EvalScope binding; false = fall
    /// back to the ordinary global store.
    pub(crate) fn eval_scope_store(&mut self, idx: u32, v: Value) -> bool {
        let Some(fi) = self.frames.len().checked_sub(1) else {
            return false;
        };
        let Some(sc) = self.frame_eval_scope(fi) else {
            return false;
        };
        let Some(name) = self.global_slot_name(idx) else {
            return false;
        };
        if let HeapObj::EvalScope(m) = self.heap.get_mut(sc) {
            if let Some(slot) = m.get_mut(&name) {
                *slot = v;
                return true;
            }
        }
        false
    }

    /// PrivateFieldFind: the field's value for the EXACT (instance, brand,
    /// name), or None (spec: empty -> TypeError at the access site).
    pub(crate) fn private_field_get(&self, inst: Value, brand: u64, key: &str) -> Option<Value> {
        if !inst.is_heap() {
            return None;
        }
        self.private_fields.get(&inst.heap_index())?.get(&(brand, key.to_string())).copied()
    }

    /// PrivateFieldAdd (`add=true`: upsert — field-init stores) or
    /// PrivateFieldSet (`add=false`: only an EXISTING entry). Returns whether
    /// the write happened.
    pub(crate) fn private_field_set(
        &mut self,
        inst: Value,
        brand: u64,
        key: &str,
        v: Value,
        add: bool,
    ) -> bool {
        if !inst.is_heap() {
            return false;
        }
        let m = self.private_fields.entry(inst.heap_index()).or_default();
        let k = (brand, key.to_string());
        if !add && !m.contains_key(&k) {
            return false;
        }
        m.insert(k, v);
        true
    }

    /// Lenient any-brand lookup for the textual-fallback paths (the accessing
    /// chain was unresolvable — e.g. a class defined in a static initializer).
    pub(crate) fn private_field_scan(&self, inst: Value, key: &str) -> Option<Value> {
        if !inst.is_heap() {
            return None;
        }
        let m = self.private_fields.get(&inst.heap_index())?;
        m.iter().find(|((_, n), _)| n == key).map(|(_, &v)| v)
    }

    pub(crate) fn private_field_scan_has(&self, inst: Value, key: &str) -> bool {
        self.private_field_scan(inst, key).is_some()
    }

    /// Any-brand write-if-present for the textual-fallback paths.
    pub(crate) fn private_field_scan_set(&mut self, inst: Value, key: &str, v: Value) -> bool {
        if !inst.is_heap() {
            return false;
        }
        let Some(m) = self.private_fields.get_mut(&inst.heap_index()) else {
            return false;
        };
        let Some(k) = m.keys().find(|(_, n)| n == key).cloned() else {
            return false;
        };
        m.insert(k, v);
        true
    }

    /// Declaring-class private resolution (kind-aware): walk the ACCESSING
    /// code's lexical brand chain innermost-first; for the first brand that
    /// DECLARES `key`, return (brand, kind, declaring-class heap idx). `None`
    /// when the chain is unresolvable, the name is foreign to the chain, or
    /// the declaring class value is unknown — the caller keeps its textual
    /// path. kind bits: 1 = method, 2 = getter, 4 = setter; 0 = field.
    pub(crate) fn resolve_private(&self, key: &str) -> Option<(u64, u8, u32)> {
        let chain = self.current_private_brands()?;
        for &b in chain.iter() {
            if let Some(names) = self.brand_private_names.get(&b) {
                if let Some(kind) = names.iter().find(|(n, _)| n == key).map(|&(_, k)| k) {
                    let owner = *self.brand_owner.get(&b)?;
                    return Some((b, kind, owner));
                }
            }
        }
        None
    }

    /// A private member's VALUE from its DECLARING class. `want`: 1 = method,
    /// 2 = getter, 4 = setter (each searches the instance + static lists; a
    /// class cannot declare the same private name twice, so order is moot).
    pub(crate) fn private_member_from_owner(&self, owner: u32, key: &str, want: u8) -> Option<Value> {
        let HeapObj::Class(c) = self.heap.get(owner) else {
            return None;
        };
        let from = |list: &[(String, Value)]| {
            list.iter().find(|(n, _)| n == key).map(|&(_, v)| v)
        };
        let is_static = want & 8 != 0;
        match want & 7 {
            1 => {
                if is_static {
                    c.statics.get(key)
                } else {
                    from(&c.methods)
                }
            }
            2 => from(if is_static { &c.static_getters } else { &c.getters }),
            4 => from(if is_static { &c.static_setters } else { &c.setters }),
            _ => None,
        }
    }

    /// Spec-side brand check for a RESOLVED private member: a STATIC member's
    /// brand lives on the declaring class VALUE itself (PrivateBrandAdd(F, F) —
    /// not on subclasses, not on instances); an INSTANCE member's brand lives
    /// on constructed instances (map.class chain or return-override extras) —
    /// never on the class value.
    pub(crate) fn private_receiver_ok(&self, recv: Value, b: u64, kind: u8, owner: u32) -> bool {
        if !recv.is_heap() {
            return false;
        }
        if kind & 8 != 0 {
            return recv.heap_index() == owner;
        }
        if matches!(self.heap.get(recv.heap_index()), HeapObj::Class(_)) {
            // A class value reads an INSTANCE private only if it was itself
            // branded as a return-override instance.
            return self
                .instance_brand
                .get(&recv.heap_index())
                .is_some_and(|bs| bs.contains(&b));
        }
        self.instance_has_brand(recv, b)
    }

    /// Brand-aware private presence for accessing private name `key`:
    /// `Some(true/false)` when the accessing class body's brand chain is
    /// resolvable, `None` when not (the caller keeps its textual check).
    ///
    /// Resolution is name-precise: walk the lexical chain INNERMOST-first and, for
    /// the first brand whose class actually DECLARES `key`, require the receiver to
    /// carry THAT specific brand (so a `#x` declared in an enclosing/shadowing
    /// class resolves to the right class, not merely "some class in scope"). If no
    /// chain brand is known to declare `key` (e.g. the name set is incomplete),
    /// fall back to the lenient any-brand check — never tighter than the chain, so
    /// this can only ADD precision, never reject a previously-accepted access.
    pub(crate) fn private_brand_ok(&self, receiver: Value, key: &str) -> Option<bool> {
        let chain = self.current_private_brands()?;
        for &b in chain.iter() {
            if self
                .brand_private_names
                .get(&b)
                .is_some_and(|names| names.iter().any(|(n, _)| n == key))
        {
                return Some(self.instance_has_brand(receiver, b));
            }
        }
        // The name is FOREIGN to the entire lexical chain: the access sits in a
        // context whose chain is incomplete (e.g. a class defined inside a
        // STATIC FIELD INITIALIZER runs in the defining frame, so its methods'
        // chains miss the outer class's brand). Defer to the caller's textual
        // fallback rather than rejecting — names DECLARED by a chain brand keep
        // the precise per-evaluation check above.
        if !chain.iter().any(|&b| {
            self.brand_private_names.get(&b).is_some_and(|names| !names.is_empty())
        }) || chain.iter().all(|&b| {
            self.brand_private_names.get(&b).map_or(true, |names| !names.iter().any(|(n, _)| n == key))
        }) {
            return None;
        }
        Some(chain.iter().any(|&b| self.instance_has_brand(receiver, b)))
    }

    /// Proxy-aware [[HasProperty]] (`in` / Reflect.has). Mirrors `has_property`
    /// but is `&mut` so it can dispatch a `has` trap — both when `obj` itself is a
    /// Proxy AND when a Proxy sits in the prototype chain (e.g.
    /// `Object.create(proxy)`), which the immutable `has_property` cannot do.
    pub(crate) fn has_property_dyn(&mut self, obj: Value, key: Value) -> Result<bool, Thrown> {
        if !obj.is_heap() {
            return Ok(false);
        }
        let idx = obj.heap_index();
        if !self.deferred_ns_state.is_empty() && self.deferred_ns_state.contains_key(&idx) {
            let ks = self.key_of(key);
            if Self::defer_key_triggers(&ks) {
                self.defer_ns_trigger(idx)?;
            }
        }
        // A Proxy: dispatch the `has` trap (with the post-trap invariant), or
        // forward to the target's [[HasProperty]] when there is no trap.
        if let Some((target, handler, revoked)) = self.proxy_parts(idx) {
            if revoked {
                return Err(Thrown("TypeError: Cannot perform 'has' on a revoked proxy".into()));
            }
            return match self.proxy_trap(handler, "has")? {
                Some(trap) => {
                    let ks = self.key_of(key);
                    let kv = self.key_to_value(&ks);
                    let res = self.call_value(trap, handler, &[target, kv])?;
                    let present = self.truthy(res);
                    // A `false` result is illegal when the target has the own
                    // property non-configurable, or the target is non-extensible.
                    if !present {
                        let desc = self.object_get_own_property_descriptor(target, &ks);
                        if desc != Value::UNDEFINED {
                            let cfg = self.get_prop(desc, "configurable")?;
                            if !self.truthy(cfg) || !self.is_extensible(target)? {
                                return Err(Thrown(
                                    "TypeError: proxy 'has' returned false for a non-configurable / non-extensible-target own property".into(),
                                ));
                            }
                        }
                    }
                    Ok(present)
                }
                None => self.has_property_dyn(target, key),
            };
        }
        // A plain object: own data property or an inherited method/getter/setter
        // on the class chain; else walk the [[Prototype]] (which may be a Proxy)
        // via has_property_dyn, using has_property's proto resolution. Each
        // `self.heap.get` borrow is scoped so the recursive &mut call is free.
        if matches!(self.heap.get(idx), HeapObj::Object(_)) {
            let k = self.key_of(key);
            if matches!(self.heap.get(idx), HeapObj::Object(m) if m.get(&k).is_some()) {
                return Ok(true);
            }
            // %Array.prototype%'s exotic `length` (virtual own prop) — mirrors
            // has_property's Object arm.
            if k == "length" && idx == self.arr_proto && self.arr_proto != 0 {
                return Ok(true);
            }
            let mut cur = match self.heap.get(idx) {
                HeapObj::Object(m) => m.class,
                _ => None,
            };
            // PRIVATE members are not observable via the ordinary
            // [[HasProperty]] (`in` / Reflect.has): skip the class-member
            // extension for "#..." keys. (has_property_str keeps it — the
            // legacy textual private brand checks rely on it.)
            let skip_members = k.starts_with('#');
            while let Some(cidx) = cur {
                let step = match self.heap.get(cidx) {
                    HeapObj::Class(c) => Some((
                        !skip_members
                            && (c.methods.iter().any(|(n, _)| *n == k)
                                || c.getters.iter().any(|(n, _)| *n == k)
                                || c.setters.iter().any(|(n, _)| *n == k)),
                        c.parent,
                    )),
                    _ => None,
                };
                match step {
                    Some((true, _)) => return Ok(true),
                    Some((false, parent)) => cur = parent,
                    None => break,
                }
            }
            let proto = if let Some(&p) = self.proto_of.get(&idx) {
                p.is_heap().then_some(p)
            } else if self.obj_proto != 0 && idx != self.obj_proto {
                Some(Value::heap(self.obj_proto))
            } else {
                None
            };
            return match proto {
                Some(p) => self.has_property_dyn(p, key),
                None => Ok(false),
            };
        }
        // An ARRAY with a custom prototype chain can have a Proxy in it whose
        // 'has' trap must fire with the right (target, key) arguments
        // (has/call-in-prototype-index) — the immutable walk treats it as
        // inert. Check the own properties (dense index / length / arr_props),
        // then recurse dynamically up the ACTUAL chain.
        if matches!(self.heap.get(idx), HeapObj::Array(_)) {
            // NUMERIC-key fast path: resolve the index straight off the Value —
            // `key_of` would allocate a fresh key String per probe, which
            // dominated the hot hole-aware `i in arr` iteration. The canonical
            // string is built only for the (rare) side-table check, and the
            // answers are identical to the general path below (key_of of a
            // canonical numeric Value IS i.to_string()).
            if let Some(i) = array_index(key) {
                if matches!(self.heap.get(idx), HeapObj::Array(items) if i < items.len() && !items[i].is_hole())
                {
                    return Ok(true);
                }
                if let Some(m) = self.arr_props.get(&idx) {
                    if m.pos(&i.to_string()).is_some() {
                        return Ok(true);
                    }
                }
                let proto = match self.proto_of.get(&idx) {
                    Some(&p) => p,
                    None if self.arr_proto != 0 => Value::heap(self.arr_proto),
                    None => Value::NULL,
                };
                // Default-chain MISS shortcut: the receiver chains to the PLAIN
                // %Array.prototype% → %Object.prototype% (neither re-prototyped),
                // no integer index was ever defined on either (every such write
                // sets `array_proto_has_index` — note_array_proto_index), and
                // %Array.prototype%'s dense store is empty (a direct
                // `Array.prototype[i] = x` lands there) — the index is
                // definitively absent: skip the recursive walk and its
                // per-probe key-string allocations (the hot hole-aware
                // `i in arr` loop spends its time there).
                if !self.array_proto_has_index
                    && proto.is_heap()
                    && proto.heap_index() == self.arr_proto
                    && !self.proto_of.contains_key(&self.arr_proto)
                    && !self.proto_of.contains_key(&self.obj_proto)
                    && matches!(self.heap.get(self.arr_proto), HeapObj::Array(items) if items.is_empty())
                {
                    return Ok(false);
                }
                return match proto.is_heap() {
                    true => self.has_property_dyn(proto, key),
                    false => Ok(false),
                };
            }
            let k = self.key_of(key);
            let int_index = match k.parse::<u32>() {
                Ok(n) if n != u32::MAX && n.to_string() == k => Some(n as usize),
                _ => None,
            };
            let own = if let Some(i) = int_index {
                matches!(self.heap.get(idx), HeapObj::Array(items) if i < items.len() && !items[i].is_hole())
                    || self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some())
            } else {
                k == "length" || self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some())
            };
            if own {
                return Ok(true);
            }
            let proto = match self.proto_of.get(&idx) {
                Some(&p) => p,
                None if self.arr_proto != 0 => Value::heap(self.arr_proto),
                None => Value::NULL,
            };
            return match proto.is_heap() {
                true => self.has_property_dyn(proto, key),
                false => Ok(false),
            };
        }
        // A TypedArray with a USER prototype chain can have a Proxy in it whose
        // 'has' trap must fire (the immutable walk below treats it as inert):
        // absorb canonical numeric indices, check arr_props own props, then
        // recurse dynamically up the chain.
        if matches!(self.heap.get(idx), HeapObj::TypedArray { .. }) {
            let k = self.key_of(key);
            if self.is_canonical_numeric_index(&k) {
                return Ok(self.ta_valid_index(idx, &k).is_some());
            }
            if self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some()) {
                return Ok(true);
            }
            return match self.proto_of.get(&idx).copied().filter(|p| p.is_heap()) {
                Some(p) => self.has_property_dyn(p, key),
                None => Ok(false),
            };
        }
        // Other heap kinds (Array / Str / …) carry no Proxy in their
        // built-in prototype chain, so the exact immutable walk suffices.
        Ok(self.has_property(obj, key))
    }

    /// READ-ONLY, infallible `in` for the JIT region helper (`jit_has_property`).
    /// Returns `Some(present)` when the answer is computable WITHOUT running user
    /// code (no Proxy `has` trap, no object-key ToString coercion) and WITHOUT a
    /// TypeError, so it matches the interpreter's `in` (HasProp, brand=false) arm
    /// bit-for-bit; returns `None` (deopt) for every case the interpreter would
    /// handle by throwing or by re-entering user code, so the region bails and the
    /// interpreter re-executes the op with full semantics.
    ///
    /// Deopt cases (the interpreter path that the helper declines to replicate):
    ///   - RHS not an Object — `in` throws a TypeError.
    ///   - key is a heap value that is NOT a flat string (an object/Symbol key:
    ///     `coerce_index_key` runs ToString / ToPrimitive = user code, or maps a
    ///     Symbol to its key form). A Number or String/Cons key needs none.
    ///   - the receiver, or ANY node on its prototype chain, is a Proxy (the `has`
    ///     trap is user code) or carries deferred-namespace state (a getter-like
    ///     trigger). Proxies are vanishingly rare in hot `i in arr` loops.
    /// Everything else is a pure read-only chain walk, so it delegates to the
    /// infallible `has_property` — identical to what `has_property_dyn` computes
    /// for a non-Proxy chain. `has_property` is `&self`: it performs no VM-heap
    /// allocation (only a transient Rust `key_of` String for the rare arr_props
    /// overlay probe), so no GC safe point and no pinned-pointer concern.
    pub(crate) fn has_property_jit(&self, obj: Value, key: Value) -> Option<bool> {
        // RHS must be an Object (else `in` throws).
        if !self.is_object_value(obj) {
            return None;
        }
        // A heap key that is not a flat string would need ToPropertyKey coercion
        // (user code) or Symbol handling — interpreter only. A Number or a
        // String/Cons key is taken verbatim (matches coerce_index_key's no-op).
        if key.is_heap()
            && !matches!(self.heap.get(key.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. })
        {
            return None;
        }
        // A private-name string key ("#m") must NOT leak via plain `in`: the
        // interpreter's `has_property_dyn` skips `#`-prefixed class members, but
        // `has_property` (which this helper delegates to) does NOT (its class-chain
        // walk matches them for legacy brand checks). DEOPT so the interpreter
        // computes the correct (false) answer. ONLY a heap STRING key can be a
        // private name, so a numeric key (the hot `i in arr` path) skips this
        // entirely — checking the key's FIRST BYTE in place (Cow::Borrowed for a
        // flat string) avoids the per-probe `key_of` allocation that this check
        // originally introduced into the hot loop.
        if key.is_heap() {
            let is_private = self
                .heap
                .str_wtf8_cow(key.heap_index())
                .is_some_and(|b| b.first() == Some(&b'#'));
            if is_private {
                return None;
            }
        }
        // A Proxy anywhere in the chain (or a deferred namespace) dispatches user
        // code / a trigger — let the interpreter run it. The walk mirrors
        // has_property/has_property_dyn's proto resolution (explicit proto_of, then
        // the intrinsic prototype for the receiver's kind), capped defensively.
        let mut cur = obj.heap_index();
        for _ in 0..64 {
            if matches!(self.heap.get(cur), HeapObj::Proxy { .. })
                || (!self.deferred_ns_state.is_empty()
                    && self.deferred_ns_state.contains_key(&cur))
            {
                return None;
            }
            let proto = if let Some(&p) = self.proto_of.get(&cur) {
                if p.is_heap() { p.heap_index() } else { 0 }
            } else {
                match self.heap.get(cur) {
                    HeapObj::Object(_) => {
                        if self.obj_proto != 0 && cur != self.obj_proto {
                            self.obj_proto
                        } else {
                            0
                        }
                    }
                    HeapObj::Array(_) | HeapObj::Cons { .. } => self.arr_proto,
                    HeapObj::TypedArray { .. } => 0, // no intrinsic in proto_of → end
                    HeapObj::Func(_)
                    | HeapObj::Closure { .. }
                    | HeapObj::Bound { .. }
                    | HeapObj::Native(_) => self.fn_proto,
                    HeapObj::Boxed { kind: 0, .. } => self.str_proto,
                    HeapObj::Boxed { kind: 1, .. } => self.num_proto,
                    HeapObj::Boxed { kind: 2, .. } => self.bool_proto,
                    HeapObj::Date(_) => self.date_proto,
                    HeapObj::Promise { .. } => self.promise_proto,
                    HeapObj::RegExp { .. } => self.regexp_proto,
                    _ => 0,
                }
            };
            if proto == 0 || proto == cur {
                break;
            }
            cur = proto;
        }
        Some(self.has_property(obj, key))
    }

    /// `val instanceof <built-in ctor>`. With no user prototype chain the result
    /// is structural: by heap kind for Array/Object/Function, and by the `name`
    /// field for the Error family (any error subtype satisfies `instanceof
    /// Error`). Primitives are never an instance of anything.
    pub(crate) fn eval_instanceof(&mut self, val: Value, ctor: InstanceCtor) -> bool {
        use InstanceCtor as C;
        if !val.is_heap() {
            return false;
        }
        let idx = val.heap_index();
        match ctor {
            C::Array => matches!(self.heap.get(idx), HeapObj::Array(_)),
            // Spec instanceof: is %Function.prototype% in `val`'s prototype chain?
            // Catches plain functions/closures AND bound functions, natives, and
            // the builtin constructor objects (Array/Object/Map/…) — all of which
            // chain to %Function.prototype% — not just literal Func/Closure values.
            C::Function => {
                self.fn_proto != 0 && self.is_prototype_of(Value::heap(self.fn_proto), val)
            }
            // OrdinaryHasInstance: %Object.prototype% must be IN the value's
            // prototype chain — a null-proto object / module namespace is NOT
            // an instanceof Object.
            C::Object => {
                self.obj_proto != 0 && self.is_prototype_of(Value::heap(self.obj_proto), val)
            }
            // An error ctor: a canonical-named error instance (internal throw /
            // `new TypeError`) OR — for `class X extends TypeError` / `Object.
            // create(TypeError.prototype)` — the matching error prototype is in
            // `val`'s prototype chain.
            C::Error => self.error_name(idx).is_some() || self.error_proto_in_chain(val, "Error"),
            C::TypeError => {
                self.error_name(idx).as_deref() == Some("TypeError")
                    || self.error_proto_in_chain(val, "TypeError")
            }
            C::RangeError => {
                self.error_name(idx).as_deref() == Some("RangeError")
                    || self.error_proto_in_chain(val, "RangeError")
            }
            C::SyntaxError => {
                self.error_name(idx).as_deref() == Some("SyntaxError")
                    || self.error_proto_in_chain(val, "SyntaxError")
            }
            C::ReferenceError => {
                self.error_name(idx).as_deref() == Some("ReferenceError")
                    || self.error_proto_in_chain(val, "ReferenceError")
            }
            C::EvalError => {
                self.error_name(idx).as_deref() == Some("EvalError")
                    || self.error_proto_in_chain(val, "EvalError")
            }
            C::UriError => {
                self.error_name(idx).as_deref() == Some("URIError")
                    || self.error_proto_in_chain(val, "URIError")
            }
            C::AggregateError => {
                self.error_name(idx).as_deref() == Some("AggregateError")
                    || self.error_proto_in_chain(val, "AggregateError")
            }
        }
    }

    /// Whether the error prototype named `name` (e.g. "TypeError") is in `val`'s
    /// prototype chain — the proto-based half of `instanceof <ErrorCtor>`, which
    /// catches subclasses and `Object.create(XError.prototype)`.
    fn error_proto_in_chain(&mut self, val: Value, name: &str) -> bool {
        match native::ERROR_NAMES.iter().position(|&n| n == name) {
            Some(k) if self.error_protos[k] != 0 => {
                self.is_prototype_of(Value::heap(self.error_protos[k]), val)
            }
            _ => false,
        }
    }

    /// Build an Error object from an internal throw message. A message like
    /// `"TypeError: cannot read …"` splits into `name="TypeError"` and
    /// `message="cannot read …"`; anything else becomes a generic `Error` whose
    /// message is the whole text. Mirrors the `{name, message}` shape the
    /// compiler emits for `new TypeError(…)`, so both catch paths are uniform.
    pub(crate) fn alloc_error_from_message(&mut self, raw: &str) -> Value {
        // Internal errors are formatted "Name: message"; recover the kind so the
        // synthesised object links to the right prototype (and `e instanceof X`,
        // `e.constructor` work). Anything unrecognised is a base `Error`.
        let (kind, message) = match raw.split_once(": ") {
            Some((pre, rest)) => match native::ERROR_NAMES.iter().position(|&n| n == pre) {
                Some(i) => (i as u8, rest.to_string()),
                None => (0, raw.to_string()),
            },
            None => (0, raw.to_string()),
        };
        let msg_v = self.alloc_str(message);
        self.make_error(kind, Some(msg_v))
    }

    /// Allocate a proto-linked error instance of the given kind (0=Error … 7=
    /// AggregateError). `name` is set own (so the structural `instanceof`/`error_name`
    /// path keeps working); `message` is set own only when supplied and not
    /// `undefined` (else inherited as "" from the prototype). The prototype link
    /// gives `.constructor`, `.toString`, and value-`instanceof` resolution.
    /// Map a Thrown message ("SyntaxError: …") to a REAL error object of the
    /// matching constructor, preserving the message text (used where a Thrown
    /// must surface as a rejection value, e.g. import()).
    pub(crate) fn error_from_thrown(&mut self, msg: &str) -> Value {
        let (kind, text) = match msg.split_once(": ") {
            Some((n, rest)) => {
                let k: u8 = match n {
                    "TypeError" => 1,
                    "RangeError" => 2,
                    "SyntaxError" => 3,
                    "ReferenceError" => 4,
                    "EvalError" => 5,
                    "URIError" => 6,
                    _ => 0,
                };
                (k, rest.to_string())
            }
            None => (0u8, msg.to_string()),
        };
        let m = self.alloc_str(text);
        self.make_error(kind, Some(m))
    }

    pub(crate) fn make_error(&mut self, kind: u8, msg: Option<Value>) -> Value {
        let k = (kind as usize).min(7);
        let name_v = self.alloc_str(native::ERROR_NAMES[k].to_string());
        let msg_idx = match msg {
            Some(m) if m != Value::UNDEFINED => Some(self.to_str_idx(m)),
            _ => None,
        };
        // `message` is a non-enumerable own data property (ES: CreateNonEnumerable-
        // DataPropertyOrThrow). `name` is normally inherited from the prototype, but
        // zipp keeps it own for the structural error_name/instanceof path — also
        // non-enumerable, so `Object.keys(err)` is `[]` as the spec requires.
        let attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let mut map = ObjMap::new();
        map.define("name", name_v, attr);
        if let Some(mi) = msg_idx {
            map.define("message", Value::heap(mi), attr);
        }
        let obj = self.heap.alloc(HeapObj::Object(map));
        let p = self.error_protos[k];
        if p != 0 {
            self.proto_of.insert(obj, Value::heap(p));
        }
        self.error_data.insert(obj); // [[ErrorData]] internal slot
        Value::heap(obj)
    }

    /// AggregateError(errors, …): install the `errors` data property — a
    /// non-enumerable, writable, configurable own array built from
    /// IterableToList(errorsArg) (spec 20.5.7.1 steps 4-5). `iterate_to_vec` runs the
    /// argument's iterator (user code) under a GC lock, so `err` stays live across it.
    pub(crate) fn install_agg_errors(&mut self, err: Value, errors_arg: Value) -> Result<(), Thrown> {
        let list = self.iterate_to_vec(errors_arg)?;
        let arr = Value::heap(self.heap.alloc(HeapObj::Array(list)));
        if let HeapObj::Object(m) = self.heap.get_mut(err.heap_index()) {
            m.define(
                "errors",
                arr,
                PropAttr {
                    writable: true,
                    enumerable: false,
                    configurable: true,
                    accessor: false,
                    setter: Value::UNDEFINED,
                },
            );
        }
        Ok(())
    }

    /// Build the `arguments` object for a (non-arrow) function activation. The
    /// element store is still a dense Array (so `arguments[i]`/`.length` stay fast),
    /// but it is given the spec-mandated ordinary shape:
    ///   - [[Prototype]] = %Object.prototype% (arguments is NOT an Array — it must
    ///     not inherit Array.prototype methods);
    ///   - own @@iterator = %Array.prototype.values% { w:t, e:f, c:t };
    ///   - own `callee`: a sloppy function gets a data property = the function
    ///     { w:t, e:f, c:t }; a strict function gets the %ThrowTypeError% poison-pill
    ///     accessor { e:f, c:f } (the unmapped-arguments callee).
    /// (The exotic-vs-ordinary `length` distinction is left as-is for now.)
    /// `mapinfo` = `Some((frame_idx, base, param_count))` for a MAPPED
    /// arguments object (sloppy callee, simple parameter list): the upcoming
    /// frame's position/window, recorded so `arguments[i]` aliases the formal
    /// registers while that frame is live. `None` = unmapped (snapshot).
    pub(crate) fn build_arguments_object(
        &mut self,
        args: Vec<Value>,
        callee: Value,
        // Strictness no longer changes the shape directly — every UNMAPPED
        // arguments object (strict, or sloppy with non-simple params) gets the
        // %ThrowTypeError% callee; kept for call-site readability.
        _is_strict: bool,
        mapinfo: Option<(usize, usize, usize)>,
    ) -> Value {
        let obj_proto = self.obj_proto;
        let array_values = self.default_array_iter;
        // The canonical %ThrowTypeError% (set up at init); a createRealm-child
        // function's strict arguments object gets the CHILD's per-realm
        // singleton (stable within the realm, distinct across realms). Fall
        // back to a fresh thrower only in the unlikely event a strict
        // arguments object is built pre-setup. EVERY unmapped arguments object
        // gets the poison pill — strict functions AND sloppy ones with a
        // non-simple parameter list (CreateUnmappedArgumentsObject step 8).
        let unmapped = mapinfo.is_none();
        let thrower = if unmapped {
            let r =
                if self.realm_global_objs.is_empty() { 0 } else { self.get_function_realm(callee) };
            if r != 0 {
                self.realm_throw_type_error(r)
            } else if self.throw_type_error != Value::UNDEFINED {
                self.throw_type_error
            } else {
                Value::heap(self.heap.alloc(HeapObj::Native(native::FN_THROW_TYPE_ERROR)))
            }
        } else {
            Value::UNDEFINED
        };
        let argc = args.len();
        let idx = self.heap.alloc(HeapObj::Array(args));
        let map = mapinfo.map(|(frame_idx, base, param_count)| crate::vm::ArgsMap {
            frame_idx,
            base,
            mapped_count: param_count.min(argc),
            unmapped: 0,
        });
        self.arguments_objs.insert(idx, map);
        if obj_proto != 0 {
            self.proto_of.insert(idx, Value::heap(obj_proto));
        }
        let m = self.arr_props.entry(idx).or_insert_with(ObjMap::new);
        // `length` is an ORDINARY data property of an arguments object
        // (writable, non-enumerable, configurable — it can hold any value and
        // be deleted), not the Array exotic length. Every Array-length special
        // case is guarded with !arguments_objs.contains.
        m.define(
            "length",
            Value::num(argc as f64),
            PropAttr {
                writable: true,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            },
        );
        m.define(
            "@@iterator",
            array_values,
            PropAttr {
                writable: true,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            },
        );
        if unmapped {
            m.define(
                "callee",
                thrower,
                PropAttr {
                    writable: false,
                    enumerable: false,
                    configurable: false,
                    accessor: true,
                    setter: thrower,
                },
            );
        } else {
            m.define(
                "callee",
                callee,
                PropAttr {
                    writable: true,
                    enumerable: false,
                    configurable: true,
                    accessor: false,
                    setter: Value::UNDEFINED,
                },
            );
        }
        Value::heap(idx)
    }

    /// Re-link a resumed generator/async activation's MAPPED `arguments`
    /// object (recorded in `gen_args_obj` at allocation) to the frame just
    /// pushed at `frame_idx`/`base`, so its [[ParameterMap]] aliases the live
    /// register window again for this resumption. While suspended, the
    /// liveness check in `args_mapped_get`/`_set` fails and the dense store
    /// (the escape snapshot) answers instead.
    pub(crate) fn relink_mapped_args(&mut self, state_idx: u32, frame_idx: usize, base: usize) {
        let Some(&aidx) = self.gen_args_obj.get(&state_idx) else { return };
        if let Some(Some(m)) = self.arguments_objs.get_mut(&aidx) {
            m.frame_idx = frame_idx;
            m.base = base;
            if let Some(f) = self.frames.get_mut(frame_idx) {
                f.args_obj = aidx;
            }
        }
    }

    /// Mapped-arguments [[Get]]: if `idx` is a LIVE mapped arguments object
    /// and `i` a still-mapped formal index, the formal's register (through
    /// its cell when captured) IS the current value. `None` = not mapped /
    /// severed / frame dead → ordinary (dense-store) semantics apply.
    pub(crate) fn args_mapped_get(&self, idx: u32, i: usize) -> Option<Value> {
        let m = self.arguments_objs.get(&idx)?.as_ref()?;
        if i >= m.mapped_count || i >= 64 || (m.unmapped >> i) & 1 == 1 {
            return None;
        }
        let f = self.frames.get(m.frame_idx)?;
        if f.args_obj != idx || f.base != m.base {
            return None;
        }
        let v = self.regs[m.base + 1 + i];
        if v.is_heap() {
            if let HeapObj::Cell(inner) = self.heap.get(v.heap_index()) {
                return Some(*inner);
            }
        }
        Some(v)
    }

    /// Mapped-arguments [[Set]] companion: write the formal's register
    /// (through its cell) when `idx`/`i` is live-mapped. The caller still
    /// performs the ordinary store into the dense slot / side table, keeping
    /// the escape store in sync. Returns whether the alias write happened.
    pub(crate) fn args_mapped_set(&mut self, idx: u32, i: usize, val: Value) -> bool {
        let Some(Some(m)) = self.arguments_objs.get(&idx) else { return false };
        if i >= m.mapped_count || i >= 64 || (m.unmapped >> i) & 1 == 1 {
            return false;
        }
        let (frame_idx, base) = (m.frame_idx, m.base);
        let live = self
            .frames
            .get(frame_idx)
            .map_or(false, |f| f.args_obj == idx && f.base == base);
        if !live {
            return false;
        }
        let slot = base + 1 + i;
        let cur = self.regs[slot];
        if cur.is_heap() {
            if let HeapObj::Cell(inner) = self.heap.get_mut(cur.heap_index()) {
                *inner = val;
                return true;
            }
        }
        self.regs[slot] = val;
        true
    }

    /// Sever formal `i` from `idx`'s [[ParameterMap]] (map.Delete: a deleted
    /// index, an accessor redefine, or a writable:false redefine) — reads and
    /// writes permanently revert to the ordinary property. When `flush`, the
    /// live formal value is copied into the dense slot first so the ordinary
    /// property keeps the latest aliased value.
    pub(crate) fn args_unmap(&mut self, idx: u32, i: usize, flush: bool) {
        let cur = if flush { self.args_mapped_get(idx, i) } else { None };
        if let Some(Some(m)) = self.arguments_objs.get_mut(&idx) {
            if i < 64 {
                m.unmapped |= 1 << i;
            }
        }
        if let Some(v) = cur {
            if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                if i < items.len() {
                    items[i] = v;
                }
            }
        }
    }

    /// Refresh a LIVE-mapped arguments object's dense slots from the formal
    /// registers (still-mapped indices only). Called before BULK dense reads
    /// (spread / IterToArray fast paths) that bypass the per-index mapped
    /// [[Get]] hooks; a no-op for everything else.
    pub(crate) fn args_sync_dense(&mut self, idx: u32) {
        let count = match self.arguments_objs.get(&idx) {
            Some(Some(m)) => m.mapped_count.min(64),
            _ => return,
        };
        for i in 0..count {
            if let Some(v) = self.args_mapped_get(idx, i) {
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    if i < items.len() {
                        items[i] = v;
                    }
                }
            }
        }
    }

    /// Allocate a fresh unique `Symbol` with description `desc` (a string Value or
    /// UNDEFINED) and a unique internal prop_key (`@@sym:N`). Recorded in
    /// `symbol_keys` so the symbol can be reflected from an own property key.
    pub(crate) fn make_symbol(&mut self, desc: Value) -> Value {
        self.symbol_counter += 1;
        let prop_key = format!("@@sym:{}", self.symbol_counter);
        let v = Value::heap(self.heap.alloc(HeapObj::Symbol { desc, prop_key: prop_key.clone() }));
        self.symbol_keys.insert(prop_key, v);
        v
    }

    /// Allocate a symbol with a FIXED prop_key (well-known `@@iterator` etc., or a
    /// `Symbol.for` registry key) — so distinct call sites share the same key.
    pub(crate) fn make_named_symbol(&mut self, desc: Value, prop_key: &str) -> Value {
        let v = Value::heap(
            self.heap.alloc(HeapObj::Symbol { desc, prop_key: prop_key.to_string() }),
        );
        self.symbol_keys.insert(prop_key.to_string(), v);
        v
    }

    /// Coerce a Value used as a PROPERTY KEY to its string form: a Symbol → its
    /// internal `prop_key` (`@@iterator` / `@@sym:N`), anything else → `display`.
    pub(crate) fn key_of(&self, key: Value) -> String {
        if key.is_heap() {
            if let HeapObj::Symbol { prop_key, .. } = self.heap.get(key.heap_index()) {
                return prop_key.clone();
            }
        }
        self.display(key)
    }

    /// `ToPropertyKey(key)` (7.1.19): a Symbol maps to its registry key; anything
    /// else is `ToString`-coerced (invoking `toString`/`valueOf` on an object and
    /// throwing TypeError for a Symbol-returning conversion). Unlike [`key_of`]
    /// this runs user coercion, so it is `&mut self` and fallible — use it for a
    /// caller-supplied property-name argument (e.g. `Object.defineProperty`).
    pub(crate) fn to_property_key(&mut self, key: Value) -> Result<String, Thrown> {
        // ToPropertyKey: ToPrimitive(key, hint String) FIRST — a Symbol result
        // (from a plain Symbol, or an object whose @@toPrimitive/toString returns a
        // Symbol) is the property key (its "@@…" form); any other primitive is
        // ToString'd. (Without the ToPrimitive step, an object key resolving to a
        // Symbol would wrongly throw "Cannot convert a Symbol value to a string".)
        let prim = self.to_primitive_string(key)?;
        if prim.is_heap() {
            if let HeapObj::Symbol { prop_key, .. } = self.heap.get(prim.heap_index()) {
                return Ok(prop_key.clone());
            }
        }
        self.to_js_string(prim)
    }

    /// Allocate a BigInt value.
    pub(crate) fn make_bigint(&mut self, v: i128) -> Value {
        Value::heap(self.heap.alloc(HeapObj::BigInt(v)))
    }

    /// `ToBigInt(v)` (used by `BigInt(x)`, asIntN/asUintN, and `==`). A non-integer
    /// number → RangeError; symbol/null/undefined/object → TypeError; a bad numeric
    /// string → SyntaxError.
    pub(crate) fn to_bigint(&mut self, v: Value) -> Result<BigVal, Thrown> {
        if let Some(n) = self.bigint_val(v) {
            return Ok(n);
        }
        if v.is_bool() {
            return Ok(BigVal::Small(if v.as_bool() { 1 } else { 0 }));
        }
        // ToBigInt of a Number is a TypeError — a Number is only accepted by the
        // BigInt() constructor's NumberToBigInt step (see `bigint_from`). This covers
        // a boxed Number and an object whose ToPrimitive yields a Number (via the
        // object branch below), not just a bare numeric literal.
        if v.is_number() {
            return Err(Thrown("TypeError: Cannot convert a Number to a BigInt".into()));
        }
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
            let t = s.trim();
            if t.is_empty() {
                return Ok(BigVal::Small(0));
            }
            return parse_bigint_str(t)
                .ok_or_else(|| Thrown(format!("SyntaxError: Cannot convert {t} to a BigInt")));
        }
        // ToBigInt step 1: an object is first taken through ToPrimitive(number)
        // (honouring Symbol.toPrimitive / valueOf / toString), then re-dispatched.
        // to_primitive_number always yields a primitive or throws, so this recurses
        // at most once into a non-object branch above.
        if self.is_object_value(v) {
            let prim = self.to_primitive_number(v)?;
            return self.to_bigint(prim);
        }
        Err(Thrown("TypeError: Cannot convert this value to a BigInt".into()))
    }

    /// The `BigInt(value)` constructor coercion (NOT the abstract ToBigInt): the
    /// value is taken through ToPrimitive(number); an integral Number is accepted
    /// via NumberToBigInt (a non-integral Number is a RangeError), and any other
    /// primitive falls through to the strict ToBigInt (Boolean/String/BigInt).
    pub(crate) fn bigint_from(&mut self, v: Value) -> Result<BigVal, Thrown> {
        let prim = if self.is_object_value(v) {
            self.to_primitive_number(v)?
        } else {
            v
        };
        if prim.is_number() {
            let d = prim.as_f64();
            if !d.is_finite() || d.fract() != 0.0 {
                return Err(Thrown(
                    "RangeError: The number is not a safe integer and cannot be converted to a BigInt"
                        .into(),
                ));
            }
            // Safely within i128 → fast tier; near/beyond the boundary → exact
            // num-bigint conversion (an f64 integer is always exact), then
            // canonicalize (`BigInt(1e300)` is a real beyond-i128 BigInt now).
            if d.abs() < (1i128 << 126) as f64 {
                return Ok(BigVal::Small(d as i128));
            }
            use num_traits::FromPrimitive;
            let b = num_bigint::BigInt::from_f64(d)
                .ok_or_else(|| Thrown("RangeError: Cannot convert to a BigInt".into()))?;
            return Ok(BigVal::from_num(b));
        }
        self.to_bigint(prim)
    }

    /// Build a RegExp from a pattern value + flags value (`/x/g`, `new RegExp(p,f)`).
    /// A RegExp pattern contributes its source (+ its flags when none are given);
    /// else ToString. Validates flags + compiles via `regress` (bad → SyntaxError).
    pub(crate) fn build_regexp(&mut self, p: Value, f: Value) -> Result<Value, Thrown> {
        // Step 1: IsRegExp(pattern) — an OBSERVABLE Get(pattern, @@match) which
        // may MUTATE the pattern (e.g. a getter calling pattern.compile(..)), so
        // it must run BEFORE the [[OriginalSource]]/[[OriginalFlags]] snapshot.
        // (Non-objects return false with no Get.)
        let pattern_is_regexp = self.is_regexp(p)?;
        // A real RegExp exotic contributes its [[OriginalSource]] (+ flags when
        // none are given) via its slots, regardless of a falsy @@match override.
        let real_regexp = match p.is_heap().then(|| self.heap.get(p.heap_index())) {
            Some(HeapObj::RegExp { source, flags, .. }) => Some((source.clone(), flags.clone())),
            _ => None,
        };
        // EXACT pattern bytes, captured when the pattern VALUE is a string
        // holding lone surrogates: `new RegExp('\uD800')` must compile the
        // surrogate itself (ToString below is LOSSY — it would compile U+FFFD).
        // `regress::Regex::from_unicode` accepts the pattern characters directly.
        // A RegExp pattern with an exact-source side entry contributes its
        // exact bytes the same way (`new RegExp(re)` round-trips them).
        let exact_bytes: Option<Vec<u8>> = if p.is_heap() && self.heap.is_str_like(p.heap_index()) {
            self.heap.str_exact_if_not_wellformed(p.heap_index())
        } else if real_regexp.is_some() {
            self.regexp_exact_source.get(&p.heap_index()).cloned()
        } else {
            None
        };
        let (source, inherited) = if let Some((src, fl)) = real_regexp {
            (src, Some(fl))
        } else if p.is_undefined() {
            (String::new(), None)
        } else if pattern_is_regexp {
            // A RegExp-LIKE object (truthy `@@match`, but not a real RegExp exotic):
            // read `source`/`flags` via Get (observable, may throw) per the RegExp
            // constructor, instead of ToString(pattern).
            let src_v = self.get_prop(p, "source")?;
            let src = if src_v.is_undefined() { "(?:)".to_string() } else { self.to_js_string(src_v)? };
            let inh = if f.is_undefined() {
                let fl_v = self.get_prop(p, "flags")?;
                Some(if fl_v.is_undefined() { String::new() } else { self.to_js_string(fl_v)? })
            } else {
                None
            };
            (src, inh)
        } else {
            (self.to_js_string(p)?, None)
        };
        let flags = if f.is_undefined() {
            inherited.unwrap_or_default()
        } else {
            self.to_js_string(f)?
        };
        // Validate: only g/i/m/s/u/y/d/v, each at most once.
        let mut seen = std::collections::HashSet::new();
        for c in flags.chars() {
            if !"gimsuyvd".contains(c) || !seen.insert(c) {
                return Err(Thrown(format!(
                    "SyntaxError: Invalid flags supplied to RegExp constructor '{flags}'"
                )));
            }
        }
        // `u` (Unicode) and `v` (UnicodeSets) select mutually-exclusive grammars;
        // enabling both is a SyntaxError (ParsePattern). (Literal `/x/uv` is caught
        // earlier by the parser; this guards the `new RegExp(p, "uv")` path.)
        if seen.contains(&'u') && seen.contains(&'v') {
            return Err(Thrown(format!(
                "SyntaxError: Invalid flags supplied to RegExp constructor '{flags}'"
            )));
        }
        // The matching flags `regress` understands (g/y/d are JS-level state).
        // `u` enables Unicode mode and `v` the distinct UnicodeSets grammar (set
        // operations, nested classes, `\q{…}`, properties-of-strings) — pass each
        // through verbatim rather than collapsing `v` into `u`.
        let mut rflags = String::new();
        for c in flags.chars() {
            match c {
                'i' | 'm' | 's' | 'u' | 'v' => rflags.push(c),
                _ => {}
            }
        }
        // The PATTERN CHARACTERS fed to the regress parser. In `u`/`v` mode the
        // pattern is a sequence of code points; otherwise the spec grammar reads
        // UTF-16 CODE UNITS — an astral literal is its two surrogate halves,
        // each its own pattern character, so `/𠮷/` (non-u) is the 2-unit
        // sequence `𠮷` and matches over UCS-2 subject units (group NAMES
        // recombine pairs — see `nonunicode_pattern_chars`). The exact-bytes
        // form (lone-surrogate pattern string) feeds the surrogates verbatim;
        // `.source` stays lossy (display only).
        let unicode_mode = seen.contains(&'u') || seen.contains(&'v');
        let compile_cps: Vec<u32> = match (&exact_bytes, unicode_mode) {
            (Some(b), true) => crate::heap::wtf8_code_points(b).collect(),
            (Some(b), false) => super::proxy_regexp::nonunicode_pattern_chars(
                &crate::heap::wtf8_units_iter(b).collect::<Vec<u16>>(),
            ),
            (None, true) => source.chars().map(u32::from).collect(),
            (None, false) => super::proxy_regexp::nonunicode_pattern_chars(
                &source.encode_utf16().collect::<Vec<u16>>(),
            ),
        };
        // Compile, through the (source, flags) cache: @@matchAll/@@split
        // construct a species clone per call, so hot loops re-build the same
        // pattern — share the immutable compiled program instead.
        // Lone-surrogate patterns (exact bytes ≠ lossy source) bypass it.
        let cache_key = exact_bytes.is_none().then(|| (source.clone(), rflags.clone(), false));
        let regex: std::sync::Arc<regress::Regex> =
            match cache_key.as_ref().and_then(|k| self.regex_compile_cache.get(k)) {
                Some(rc) => rc.clone(),
                None => {
                    let r = regress::Regex::from_unicode(compile_cps.iter().copied(), rflags.as_str())
                        .map_err(|e| {
                            Thrown(format!("SyntaxError: Invalid regular expression: /{source}/: {e}"))
                        })?;
                    let rc = std::sync::Arc::new(r);
                    if let Some(k) = cache_key {
                        if self.regex_compile_cache.len() >= 512 {
                            self.regex_compile_cache.clear();
                        }
                        self.regex_compile_cache.insert(k, rc.clone());
                    }
                    rc
                }
            };
        let idx = self
            .heap
            .alloc(HeapObj::RegExp { regex, source, flags, last_index: Value::int(0), ascii_twin: None });
        // Lone-surrogate pattern: record the EXACT [[OriginalSource]] bytes in
        // the side table (the struct's `source: String` is lossy) so the
        // `source` getter / `toString` round-trip the surrogates. The heap
        // slot is fresh — no stale entry to clear on the None path.
        if let Some(b) = exact_bytes {
            self.regexp_exact_source.insert(idx, b);
        }
        if self.regexp_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.regexp_proto));
        }
        Ok(Value::heap(idx))
    }

    // ── Temporal.Duration ──

}
