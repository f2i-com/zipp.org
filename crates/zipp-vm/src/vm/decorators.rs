//! proposal-decorators: the class-definition-time decoration runtime.
//!
//! ## What the compiler hands us
//!
//! A decorated class carries a [`crate::bytecode::DecPlan`] on its `ClassDef`:
//! the number of class decorators plus one [`DecElemDef`] per decorated element,
//! in DOCUMENT order. `MakeClass` allocates the matching per-evaluation
//! [`DecState`] on the `ClassData`, and five ops drive the rest:
//!
//! * `DecKey`   — record a computed element's evaluated ClassElementName.
//! * `DecElem`  — apply one element's decorators (reverse list order).
//! * `DecClass` — apply the class's own decorators; install `[Symbol.metadata]`.
//! * `DecInits` — run one `addInitializer` list (instance-methods / static-methods
//!   / class / one element's own).
//! * `DecField` — pipe a decorated field's value through its initializer chain.
//!
//! The ordering contract every one of them exists to enforce is pinned in
//! `crates/zipp-vm/tests/decorators.rs`: test262 has only SYNTAX tests for
//! decorators, so nothing else in the tree notices if this order changes.
//!
//! ## Ordering
//!
//! node/V8 implements no decorators at any flag, so none of this can be
//! differential-tested against it. It is instead read off ClassDefinitionEvaluation
//! and ApplyDecoratorsToElementDefinition in tc39/ecma262#2417 (the merged
//! decorators PR) and cross-checked against the two implementations written to
//! that text — Babel's `2023-11` transform and TypeScript 5.9's `__esDecorate`.
//!
//! 1. Class decorator EXPRESSIONS, left to right — before the heritage, because
//!    `ClassDeclaration : DecoratorList class BindingIdentifier ClassTail`
//!    evaluates the list before ClassTail (and the heritage lives in ClassTail).
//! 2. Each element's decorator expressions, then its ClassElementName, in
//!    document order.
//! 3. Element decorators APPLIED, in FOUR GROUPS — static non-fields, instance
//!    non-fields, static fields, instance fields — each group in document order.
//!    (ClassDefinitionEvaluation runs four separate loops over the element lists;
//!    a flat document-order pass is observably different as soon as a class mixes
//!    a decorated field with a decorated method.) Each element's own decorator
//!    list applies in REVERSE source order: `DecoratorListEvaluation` PREPENDS,
//!    so `@a @b m(){}` calls `b` first and hands its result to `a`.
//! 4. Class decorators applied, in reverse. The result REPLACES the class —
//!    which is why this precedes the static field initializers: `@dec class C {
//!    static x = 1 }` must install `x` on the class `dec` returned, not on the
//!    one it was handed. The class's own INNER binding is (re)bound to the
//!    replacement here too.
//! 5. `staticMethodExtraInitializers` (`this` = the class).
//! 6. Static field initializers and `static {}` blocks, in document order; each
//!    decorated static field runs its own initializer chain, is defined, and then
//!    runs its OWN `addInitializer` callbacks.
//! 7. `classExtraInitializers` (`this` = the class) — the class is now fully
//!    defined, which is the property the README gives them.
//!
//! `instanceMethodExtraInitializers` run at the head of instance element
//! initialization; then each instance field/accessor in document order runs its
//! initializer chain, is defined, and runs its own `addInitializer` callbacks
//! (InitializeFieldOrAccessor). A field/accessor decorator's callbacks are
//! per-element and never join the shared method list — that is the difference
//! between "before any field" (methods) and "right after this field" (fields).

#![allow(unused_imports)]
use super::*;
use crate::bytecode::DecElemDef;
use crate::heap::{ClassData, DecState, Heap, HeapObj, ObjMap, PropAttr};
use crate::value::Value;

/// Element kinds, matching `DecElemDef::kind`.
pub(crate) const DK_METHOD: u8 = 0;
pub(crate) const DK_GETTER: u8 = 1;
pub(crate) const DK_SETTER: u8 = 2;
pub(crate) const DK_FIELD: u8 = 3;
pub(crate) const DK_ACCESSOR: u8 = 4;

/// `{ value, writable: true, enumerable: true, configurable: true }` — the
/// attributes CreateDataPropertyOrThrow gives every property of a context object.
fn data_attr() -> PropAttr {
    PropAttr {
        writable: true,
        enumerable: true,
        configurable: true,
        accessor: false,
        setter: Value::UNDEFINED,
    }
}

impl<'p> Vm<'p> {
    // ── the state-carrying builtins the context object exposes ──────────────

    /// [[Call]] for a [`HeapObj::NativeClosure`]. `state` is the captured
    /// `Value` list, already cloned out of the heap by the caller (the body
    /// re-enters the VM, so it must not hold a heap borrow).
    pub(crate) fn call_native_closure(
        &mut self,
        id: u16,
        state: &[Value],
        _this: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        match id {
            native::DEC_ADD_INITIALIZER => {
                let class = state[0];
                let which = state[1].as_f64() as u8;
                let elem = state[2].as_f64() as usize;
                let my_gen = state[3].as_f64() as u32;
                // `decorationState.[[Finished]]` is checked BEFORE IsCallable —
                // CreateAddInitializerFunction tests it first, so a stale context
                // reports staleness even when handed a non-function.
                let stale = self.dec_of(class).map(|d| d.gen != my_gen).unwrap_or(true);
                if stale {
                    return Err(Thrown(
                        "TypeError: addInitializer called after decoration completed".into(),
                    ));
                }
                if !self.is_callable(a0) {
                    return Err(Thrown(
                        "TypeError: addInitializer expects a function".into(),
                    ));
                }
                if let Some(d) = self.dec_of_mut(class) {
                    match which {
                        0 => d.instance_extra.push(a0),
                        1 => d.static_extra.push(a0),
                        2 => d.class_extra.push(a0),
                        // A field/accessor element owns its own list.
                        _ if elem < d.elem_extra.len() => d.elem_extra[elem].push(a0),
                        _ => {}
                    }
                }
                Ok(Value::UNDEFINED)
            }
            native::DEC_ACCESS_GET | native::DEC_ACCESS_SET | native::DEC_ACCESS_HAS => {
                self.dec_access(id, state, a0, a1)
            }
            // Annex B legacy RegExp static GETTER: state[0] is the slot index
            // into `regexp_last` ([input, lastMatch, lastParen, leftContext,
            // rightContext, $1..$9]). GetLegacyRegExpStaticProperty: the receiver
            // must be the %RegExp% constructor itself; empty string before the
            // first match.
            native::REGEXP_LEGACY_GET => {
                if !(_this.is_heap() && _this.heap_index() == self.regexp_ctor) {
                    return Err(Thrown(
                        "TypeError: RegExp legacy static getter called on a non-%RegExp% receiver"
                            .into(),
                    ));
                }
                let slot = state[0].as_f64() as usize;
                // Slots 1..=13 (lastMatch/lastParen/leftContext/rightContext/
                // $1..$9) may be deferred ranges after an ASCII match — build them
                // now. Only slot 0 (`input`) is always already materialised; slot 1
                // joined the deferred set in B71.
                if slot >= 1 {
                    self.regexp_last_materialise();
                }
                match self.regexp_last.get(slot).copied() {
                    Some(v) => Ok(v),
                    None => Ok(self.alloc_str(String::new())),
                }
            }
            _ => Err(Thrown("TypeError: unknown native closure".into())),
        }
    }

    /// `context.access.{get,set,has}`. `state` is
    /// `[class, key, flags, storageKeyOrUndefined]`, where `flags` packs
    /// 1 = private, 2 = static and the low bits 4..7 the element kind.
    fn dec_access(
        &mut self,
        id: u16,
        state: &[Value],
        target: Value,
        value: Value,
    ) -> Result<Value, Thrown> {
        let class = state[0];
        let key = state[1];
        let flags = state[2].as_f64() as u32;
        let is_private = flags & 1 != 0;
        let is_static = flags & 2 != 0;
        let kind = (flags >> 2) as u8;
        // Every one of the three closures opens with "If obj is not an Object,
        // throw a TypeError" — before the key is even consulted, so
        // `access.get("str")` throws rather than reading String.prototype.
        if !self.is_object_value(target) {
            return Err(Thrown(
                "TypeError: decorator access expects an object".into(),
            ));
        }
        if !is_private {
            // A PUBLIC element's access object is spec'd in terms of the
            // ordinary Get/Set/HasProperty on the receiver — so it goes through
            // a Proxy trap, an inherited accessor and an auto-accessor's own
            // get/set pair, all of which a direct slot poke would skip.
            return match id {
                native::DEC_ACCESS_GET => self.get_index(target, key),
                native::DEC_ACCESS_SET => {
                    self.set_index(target, key, value, true)?;
                    Ok(Value::UNDEFINED)
                }
                _ => Ok(Value::bool(self.has_property_dyn(target, key)?)),
            };
        }
        // ── private ──
        let name = self.key_of(key);
        let brand = match self.heap.get(class.heap_index()) {
            HeapObj::Class(c) => c.private_brand,
            _ => 0,
        };
        // The BACKING SLOT of a private auto-accessor is a distinct private name
        // from the accessor's own `#x`; state[3] carries it.
        let storage = if state[3].is_heap() {
            Some(self.key_of(state[3]))
        } else {
            None
        };
        match id {
            native::DEC_ACCESS_HAS => {
                let present = match kind {
                    DK_FIELD => self.private_field_get(target, brand, &name).is_some(),
                    DK_ACCESSOR => self
                        .private_field_get(target, brand, storage.as_deref().unwrap_or(&name))
                        .is_some(),
                    // A private method/accessor's presence IS the brand check
                    // (PrivateElementFind over [[PrivateMethods]]): a static one
                    // is branded onto the class value itself, an instance one
                    // onto every instance.
                    _ if is_static => Ok::<bool, Thrown>(
                        target.is_heap() && target.heap_index() == class.heap_index(),
                    )?,
                    _ => self.instance_has_brand(target, brand),
                };
                Ok(Value::bool(present))
            }
            native::DEC_ACCESS_GET => match kind {
                DK_FIELD => self.private_field_get(target, brand, &name).ok_or_else(|| {
                    Thrown(format!(
                        "TypeError: cannot read private member {name} from an object whose class did not declare it"
                    ))
                }),
                // A private auto-accessor or getter reads through the GETTER on
                // the class, which a decorator may have replaced.
                DK_ACCESSOR | DK_GETTER => {
                    let g = self.class_member_value(class, &name, kind, is_static, true);
                    match g {
                        Some(g) => self.call_value(g, target, &[]),
                        None => Err(Thrown(format!(
                            "TypeError: private member {name} has no getter"
                        ))),
                    }
                }
                _ => self
                    .class_member_value(class, &name, kind, is_static, true)
                    .ok_or_else(|| Thrown(format!("TypeError: no private member {name}"))),
            },
            _ => match kind {
                DK_FIELD => {
                    if self.private_field_set(target, brand, &name, value, false) {
                        Ok(Value::UNDEFINED)
                    } else {
                        Err(Thrown(format!(
                            "TypeError: cannot write private member {name} on an object whose class did not declare it"
                        )))
                    }
                }
                DK_ACCESSOR | DK_SETTER => {
                    let s = self.class_member_value(class, &name, kind, is_static, false);
                    match s {
                        Some(s) => {
                            self.call_value(s, target, &[value])?;
                            Ok(Value::UNDEFINED)
                        }
                        None => Err(Thrown(format!(
                            "TypeError: private member {name} has no setter"
                        ))),
                    }
                }
                _ => Err(Thrown(format!(
                    "TypeError: private method {name} is not writable"
                ))),
            },
        }
    }

    // ── reading / writing a decorated element on the class value ────────────

    /// The element's live function value on the class. `want_get` picks the
    /// getter half of an accessor pair (an auto-accessor has both).
    fn class_member_value(
        &self,
        class: Value,
        key: &str,
        kind: u8,
        is_static: bool,
        want_get: bool,
    ) -> Option<Value> {
        let HeapObj::Class(c) = self.heap.get(class.heap_index()) else {
            return None;
        };
        let find = |l: &[(String, Value)]| l.iter().find(|(n, _)| n == key).map(|&(_, v)| v);
        match kind {
            DK_METHOD => {
                if is_static {
                    c.statics.get(key)
                } else {
                    find(&c.methods)
                }
            }
            DK_GETTER => find(if is_static {
                &c.static_getters
            } else {
                &c.getters
            }),
            DK_SETTER => find(if is_static {
                &c.static_setters
            } else {
                &c.setters
            }),
            DK_ACCESSOR if want_get => find(if is_static {
                &c.static_getters
            } else {
                &c.getters
            }),
            DK_ACCESSOR => find(if is_static {
                &c.static_setters
            } else {
                &c.setters
            }),
            _ => None,
        }
    }

    /// Install `v` as the element's function value, replacing whatever is there.
    /// A decorator that returns a replacement is the only caller.
    fn class_member_set(
        &mut self,
        class: Value,
        key: &str,
        kind: u8,
        is_static: bool,
        set_get: bool,
        v: Value,
    ) {
        let method_attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let HeapObj::Class(c) = self.heap.get_mut(class.heap_index()) else {
            return;
        };
        let put = |l: &mut Vec<(String, Value)>| match l.iter_mut().find(|(n, _)| n == key) {
            Some(slot) => slot.1 = v,
            // A decorator cannot introduce an element that was not declared, so
            // reaching here means the key was renamed under us; append rather
            // than drop the replacement on the floor.
            None => l.push((key.to_string(), v)),
        };
        match kind {
            DK_METHOD => {
                if is_static {
                    c.statics.define(key, v, method_attr);
                } else {
                    put(&mut c.methods);
                }
            }
            DK_GETTER => put(if is_static {
                &mut c.static_getters
            } else {
                &mut c.getters
            }),
            DK_SETTER => put(if is_static {
                &mut c.static_setters
            } else {
                &mut c.setters
            }),
            DK_ACCESSOR if set_get => put(if is_static {
                &mut c.static_getters
            } else {
                &mut c.getters
            }),
            DK_ACCESSOR => put(if is_static {
                &mut c.static_setters
            } else {
                &mut c.setters
            }),
            _ => {}
        }
    }

    // ── DecState access ─────────────────────────────────────────────────────

    fn dec_of(&self, class: Value) -> Option<&DecState> {
        if !class.is_heap() {
            return None;
        }
        match self.heap.get(class.heap_index()) {
            HeapObj::Class(c) => c.dec.as_deref(),
            _ => None,
        }
    }

    fn dec_of_mut(&mut self, class: Value) -> Option<&mut DecState> {
        if !class.is_heap() {
            return None;
        }
        match self.heap.get_mut(class.heap_index()) {
            HeapObj::Class(c) => c.dec.as_deref_mut(),
            _ => None,
        }
    }

    /// The runtime class value a compile-time class id denotes FOR THE RUNNING
    /// CODE.
    ///
    /// `class_values[class_id]` alone is not enough: a `class` inside a function
    /// called twice leaves only the LATEST evaluation there, so an instance of
    /// the first — constructed after the second — would run the second
    /// evaluation's decorator initializers (a real wrong answer, since each
    /// evaluation calls its decorators afresh). The running constructor / field
    /// thunk carries its own class evaluation's lexical private-brand chain, so
    /// the first brand whose owner was materialized from THIS `class_id` is the
    /// right evaluation. The `class_values` fallback covers class-definition-time
    /// callers (static / class initializers), whose frame belongs to the
    /// ENCLOSING code and therefore has no brand for this class — there
    /// `class_values` was written moments ago and cannot be stale.
    fn dec_class_of_id(&self, class_id: u32) -> Option<Value> {
        if let Some(chain) = self.current_private_brands() {
            for &b in chain.iter() {
                if let Some(&owner) = self.brand_owner.get(&b) {
                    if let HeapObj::Class(c) = self.heap.get(owner) {
                        // The class_id check also guards against a brand whose
                        // owner slot the GC has since recycled.
                        if c.class_id == class_id && c.dec.is_some() {
                            return Some(Value::heap(owner));
                        }
                    }
                }
            }
        }
        self.class_values.get(class_id as usize).copied().flatten()
    }

    // ── MakeClass hook ──────────────────────────────────────────────────────

    /// Allocate the class's `DecState` and its `[Symbol.metadata]` object.
    /// The metadata object's [[Prototype]] is the SUPERCLASS's metadata (the
    /// decorator-metadata proposal), so a subclass's decorators read through to
    /// what the base class's decorators wrote.
    pub(crate) fn dec_init_state(&mut self, class: Value, n_elems: usize, parent: Option<u32>) {
        let parent_meta = match parent {
            Some(p) => {
                let pv = Value::heap(p);
                self.get_prop(pv, "@@metadata").unwrap_or(Value::UNDEFINED)
            }
            None => Value::UNDEFINED,
        };
        let meta_idx = self.heap.alloc(HeapObj::Object(Box::new(ObjMap::new())));
        // OrdinaryObjectCreate(parentMetadata): a null prototype when the parent
        // has none, so `Object.getPrototypeOf(C[Symbol.metadata])` is null rather
        // than %Object.prototype% and an inherited-key probe cannot see through.
        self.proto_of.insert(
            meta_idx,
            if parent_meta.is_heap() {
                parent_meta
            } else {
                Value::NULL
            },
        );
        let mut st = DecState::new(n_elems);
        st.metadata = Value::heap(meta_idx);
        if let HeapObj::Class(c) = self.heap.get_mut(class.heap_index()) {
            c.dec = Some(Box::new(st));
        }
    }

    /// `DecKey`: remember the evaluated ClassElementName of a computed element.
    ///
    /// `context.name` is "a property key or a Private Name" — a String or a
    /// Symbol, never a Number. The engine's index-key coercion deliberately
    /// leaves a numeric key as an f64 (that is what makes `a[0]` fast), so
    /// `class C { @d [1+1] = 1 }` would otherwise hand the decorator the number
    /// `2` where every other engine hands it the string `"2"`.
    pub(crate) fn dec_record_key(&mut self, class: Value, elem: usize, key: Value) {
        let key = if key.is_heap() {
            key
        } else {
            let s = self.key_of(key);
            self.alloc_str(s)
        };
        if let Some(d) = self.dec_of_mut(class) {
            if elem < d.keys.len() {
                d.keys[elem] = key;
            }
        }
    }

    // ── the context object ──────────────────────────────────────────────────

    fn native_closure(
        &mut self,
        id: u16,
        state: Vec<Value>,
        name: &'static str,
        length: u8,
    ) -> Value {
        Value::heap(self.heap.alloc(HeapObj::NativeClosure {
            id,
            state,
            name,
            length,
        }))
    }

    /// CreateDecoratorAccessObject: `{ has, get?, set? }`. `get` is present for a
    /// field/method/accessor/getter, `set` for a field/setter/accessor; `has`
    /// always. `key` is the element's resolved ClassElementName.
    fn dec_access_object(&mut self, class: Value, e: &DecElemDef, key: Value) -> Value {
        let flags = (e.is_private as u32) | ((e.is_static as u32) << 1) | ((e.kind as u32) << 2);
        let storage = if e.storage.is_empty() {
            Value::UNDEFINED
        } else {
            self.alloc_str(e.storage.clone())
        };
        let st = vec![class, key, Value::num(flags as f64), storage];
        let attr = data_attr();
        // CreateDecoratorAccessObject names `has` but leaves the getter and
        // setter closures ANONYMOUS (CreateBuiltinFunction(…, 1, "")), so
        // `context.access.get.name` is "" — the one place the three differ.
        let has = self.native_closure(native::DEC_ACCESS_HAS, st.clone(), "has", 1);
        let get = matches!(e.kind, DK_FIELD | DK_METHOD | DK_ACCESSOR | DK_GETTER)
            .then(|| self.native_closure(native::DEC_ACCESS_GET, st.clone(), "", 1));
        let set = matches!(e.kind, DK_FIELD | DK_SETTER | DK_ACCESSOR)
            .then(|| self.native_closure(native::DEC_ACCESS_SET, st, "", 2));
        let mut m = ObjMap::new();
        m.define("has", has, attr);
        if let Some(g) = get {
            m.define("get", g, attr);
        }
        if let Some(s) = set {
            m.define("set", s, attr);
        }
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))))
    }

    /// CreateDecoratorContextObject for a class ELEMENT. `elem` is the element's
    /// index in the plan, needed because a field/accessor's `addInitializer`
    /// callbacks live in that element's OWN list rather than the shared one.
    fn dec_context(&mut self, class: Value, e: &DecElemDef, elem: usize, key: Value) -> Value {
        let kind = self.alloc_str(
            match e.kind {
                DK_METHOD => "method",
                DK_GETTER => "getter",
                DK_SETTER => "setter",
                DK_FIELD => "field",
                _ => "accessor",
            }
            .to_string(),
        );
        let access = self.dec_access_object(class, e, key);
        // ClassDefinitionEvaluation passes `e.[[ExtraInitializers]]` for a field
        // OR an accessor and the shared static/instance method list for the rest.
        let which_extra: f64 = match e.kind {
            DK_FIELD | DK_ACCESSOR => 3.0,
            _ if e.is_static => 1.0,
            _ => 0.0,
        };
        let gen = self.dec_of(class).map(|d| d.gen).unwrap_or(0);
        let add_init = self.native_closure(
            native::DEC_ADD_INITIALIZER,
            vec![
                class,
                Value::num(which_extra),
                Value::num(elem as f64),
                Value::num(gen as f64),
            ],
            "addInitializer",
            1,
        );
        let metadata = self
            .dec_of(class)
            .map(|d| d.metadata)
            .unwrap_or(Value::UNDEFINED);
        let attr = data_attr();
        let mut m = ObjMap::new();
        m.define("kind", kind, attr);
        m.define("name", key, attr);
        m.define("static", Value::bool(e.is_static), attr);
        m.define("private", Value::bool(e.is_private), attr);
        m.define("access", access, attr);
        m.define("addInitializer", add_init, attr);
        m.define("metadata", metadata, attr);
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))))
    }

    /// CreateDecoratorContextObject for the CLASS itself: no `static`, `private`
    /// or `access` — there is no element to reach.
    fn dec_class_context(&mut self, class: Value) -> Value {
        let kind = self.alloc_str("class".to_string());
        let name = match self.heap.get(class.heap_index()) {
            HeapObj::Class(c) => c.name.clone(),
            _ => String::new(),
        };
        // An anonymous class's `context.name` is undefined, not "".
        let name = if name.is_empty() || name == "<class>" {
            Value::UNDEFINED
        } else {
            self.alloc_str(name)
        };
        let gen = self.dec_of(class).map(|d| d.gen).unwrap_or(0);
        let add_init = self.native_closure(
            native::DEC_ADD_INITIALIZER,
            vec![
                class,
                Value::num(2.0),
                Value::num(0.0),
                Value::num(gen as f64),
            ],
            "addInitializer",
            1,
        );
        let metadata = self
            .dec_of(class)
            .map(|d| d.metadata)
            .unwrap_or(Value::UNDEFINED);
        let attr = data_attr();
        let mut m = ObjMap::new();
        m.define("kind", kind, attr);
        m.define("name", name, attr);
        m.define("addInitializer", add_init, attr);
        m.define("metadata", metadata, attr);
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))))
    }

    // ── applying decorators ─────────────────────────────────────────────────

    /// `DecElem`: DecorateElement for one class element.
    pub(crate) fn dec_apply_element(
        &mut self,
        class: Value,
        class_id: u32,
        elem: usize,
        decorators: &[(Value, Value)],
    ) -> Result<(), Thrown> {
        let e: DecElemDef = {
            let def = self.class_def(class_id as usize);
            let Some(plan) = def.dec_plan.as_ref() else {
                return Ok(());
            };
            plan.elements[elem].clone()
        };
        // The key: a static name is compile-time, a computed one was recorded by
        // the `DecKey` op when its expression was evaluated.
        let key = if e.computed {
            self.dec_of(class)
                .map(|d| d.keys[elem])
                .unwrap_or(Value::UNDEFINED)
        } else if let Some(sym) = e
            .sym_key
            .then(|| self.well_known_symbol_value(&e.name))
            .flatten()
        {
            // `[Symbol.iterator]` and friends CONSTANT-FOLD to the engine's
            // "@@iterator" key string at compile time, so the element takes the
            // static-name path and never gets a `DecKey`. `context.name` is still
            // required to be the Symbol itself, not that internal spelling.
            sym
        } else {
            self.alloc_str(e.name.clone())
        };
        let key_str = self.key_of(key);
        // GC is suspended for the whole element: `value`, `key` and `class` are
        // Rust locals across calls into user decorators, and only `class` is
        // otherwise rooted. Bounded work — one class element's decorator list.
        let _gc = self.gc_lock_guard();

        // The value handed to the decorator: the method/accessor function, an
        // undefined for a field, or the `{ get, set }` pair of an auto-accessor.
        let mut value = match e.kind {
            DK_FIELD => Value::UNDEFINED,
            DK_ACCESSOR => {
                let g = self
                    .class_member_value(class, &key_str, DK_ACCESSOR, e.is_static, true)
                    .unwrap_or(Value::UNDEFINED);
                let s = self
                    .class_member_value(class, &key_str, DK_ACCESSOR, e.is_static, false)
                    .unwrap_or(Value::UNDEFINED);
                let attr = data_attr();
                let mut m = ObjMap::new();
                m.define("get", g, attr);
                m.define("set", s, attr);
                Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))))
            }
            k => self
                .class_member_value(class, &key_str, k, e.is_static, true)
                .unwrap_or(Value::UNDEFINED),
        };

        // Decorators apply INNERMOST FIRST: `@a @b m(){}` calls `b(m)` and then
        // `a(<b's result>)`, so a decorator sees the composition below it.
        for &(dec, recv) in decorators.iter().rev() {
            let ctx = self.dec_context(class, &e, elem, key);
            let out = self.call_value(dec, recv, &[value, ctx])?;
            // `Set decorationState.[[Finished]] to true` — immediately after the
            // Call and BEFORE the result is validated, so a context this decorator
            // stashed is already closed for the next one.
            if let Some(d) = self.dec_of_mut(class) {
                d.gen = d.gen.wrapping_add(1);
            }
            match e.kind {
                // A field decorator returns an INITIALIZER `(v) => v`, chained
                // onto the field's initial value at construction time.
                DK_FIELD => {
                    if self.is_callable(out) {
                        if let Some(d) = self.dec_of_mut(class) {
                            // "PREPEND newValue to [[Initializers]]": decorators
                            // apply innermost-first but their initializers RUN
                            // outermost-first, so `@a @b x = V` gives `b(a(V))`.
                            d.field_inits[elem].insert(0, out);
                        }
                    } else if !out.is_undefined() {
                        return Err(Thrown(
                            "TypeError: a field decorator must return a function or undefined"
                                .into(),
                        ));
                    }
                }
                // An auto-accessor decorator returns `{ get, set, init }`; any
                // subset may be present, and `init` joins the backing field's
                // initializer chain.
                DK_ACCESSOR => {
                    if out.is_undefined() {
                        continue;
                    }
                    if !self.is_object_value(out) {
                        return Err(Thrown(
                            "TypeError: an accessor decorator must return an object or undefined"
                                .into(),
                        ));
                    }
                    let g = self.get_prop(out, "get")?;
                    let s = self.get_prop(out, "set")?;
                    let init = self.get_prop(out, "init")?;
                    // Each of the three is "IsCallable → take it; not undefined →
                    // TypeError". Testing `is_heap()` instead let `{ init: 5 }`
                    // through silently, which is the same class of bug as a
                    // decorator that quietly does nothing.
                    for (v, what) in [(g, "get"), (s, "set"), (init, "init")] {
                        if !self.is_callable(v) && !v.is_undefined() {
                            return Err(Thrown(format!(
                                "TypeError: an accessor decorator's '{what}' must be a function or undefined"
                            )));
                        }
                    }
                    if self.is_callable(init) {
                        if let Some(d) = self.dec_of_mut(class) {
                            d.field_inits[elem].insert(0, init);
                        }
                    }
                    // The NEXT (outer) decorator must see the pair this one
                    // produced, so the running `value` is rebuilt from it.
                    let attr = data_attr();
                    let mut m = ObjMap::new();
                    let cur_g = if g.is_undefined() {
                        self.get_prop(value, "get")?
                    } else {
                        g
                    };
                    let cur_s = if s.is_undefined() {
                        self.get_prop(value, "set")?
                    } else {
                        s
                    };
                    m.define("get", cur_g, attr);
                    m.define("set", cur_s, attr);
                    value = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))));
                }
                // A method/getter/setter decorator returns a replacement (or
                // undefined to keep the original).
                _ => {
                    if out.is_undefined() {
                        continue;
                    }
                    if !self.is_callable(out) {
                        return Err(Thrown(
                            "TypeError: a method decorator must return a function or undefined"
                                .into(),
                        ));
                    }
                    value = out;
                }
            }
        }

        // Install whatever the chain produced.
        match e.kind {
            DK_FIELD => {}
            DK_ACCESSOR => {
                let g = self.get_prop(value, "get")?;
                let s = self.get_prop(value, "set")?;
                if self.is_callable(g) {
                    self.class_member_set(class, &key_str, DK_ACCESSOR, e.is_static, true, g);
                }
                if self.is_callable(s) {
                    self.class_member_set(class, &key_str, DK_ACCESSOR, e.is_static, false, s);
                }
            }
            k => {
                if self.is_callable(value) {
                    self.class_member_set(class, &key_str, k, e.is_static, true, value);
                }
            }
        }
        Ok(())
    }

    /// `DecClass`: ApplyDecoratorsToClassDefinition, plus the class's
    /// `[Symbol.metadata]` property. Returns the (possibly replaced) class.
    pub(crate) fn dec_apply_class(
        &mut self,
        class: Value,
        decorators: &[(Value, Value)],
    ) -> Result<Value, Thrown> {
        let mut cur = class;
        {
            let _gc = self.gc_lock_guard();
            for &(dec, recv) in decorators.iter().rev() {
                let ctx = self.dec_class_context(class);
                let out = self.call_value(dec, recv, &[cur, ctx])?;
                if let Some(d) = self.dec_of_mut(class) {
                    d.gen = d.gen.wrapping_add(1);
                }
                if out.is_undefined() {
                    continue;
                }
                if !self.is_callable(out) {
                    return Err(Thrown(
                        "TypeError: a class decorator must return a constructor or undefined"
                            .into(),
                    ));
                }
                cur = out;
            }
        }
        // `classEnv.InitializeBinding(classBinding, F)` runs on the value the
        // class decorators produced, so the class's own INNER name binding must
        // resolve to the replacement from here on. `class_values` keeps pointing
        // at the original (that is how the runtime finds this `DecState` and the
        // computed field keys); `LoadClassValue` is the one reader that hops.
        if cur != class {
            if let Some(d) = self.dec_of_mut(class) {
                d.replacement = cur;
            }
        }
        // `C[Symbol.metadata]` — non-enumerable, writable, configurable. Defined
        // on the value the decorators produced, which is the class user code sees.
        let meta = self
            .dec_of(class)
            .map(|d| d.metadata)
            .unwrap_or(Value::UNDEFINED);
        if meta.is_heap() {
            let mut desc = ObjMap::new();
            desc.set("value", meta);
            desc.set("writable", Value::TRUE);
            desc.set("enumerable", Value::FALSE);
            desc.set("configurable", Value::TRUE);
            let d = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(desc))));
            self.object_define_property(cur, "@@metadata", d)?;
        }
        Ok(cur)
    }

    /// `DecInits`: run one `addInitializer` list with `this` = `recv`.
    /// `which`: 0 = instance methods, 1 = static methods, 2 = class,
    /// 3 = the per-element list of field/accessor `elem`.
    pub(crate) fn dec_run_inits(
        &mut self,
        class_id: u32,
        which: u8,
        elem: usize,
        recv: Value,
    ) -> Result<(), Thrown> {
        let Some(class) = self.dec_class_of_id(class_id) else {
            return Ok(());
        };
        let list: Vec<Value> = match self.dec_of(class) {
            Some(d) => match which {
                0 => d.instance_extra.clone(),
                1 => d.static_extra.clone(),
                2 => d.class_extra.clone(),
                _ if elem < d.elem_extra.len() => d.elem_extra[elem].clone(),
                _ => return Ok(()),
            },
            None => return Ok(()),
        };
        for f in list {
            self.call_value(f, recv, &[])?;
        }
        Ok(())
    }

    /// The well-known Symbol VALUE behind one of the engine's reserved `"@@name"`
    /// key strings, or `None` for an ordinary key. The compiler folds
    /// `[Symbol.iterator]` straight to `"@@iterator"`, so this is the only way
    /// back to the Symbol a decorator's `context.name` must be handed.
    fn well_known_symbol_value(&mut self, key: &str) -> Option<Value> {
        let js = key.strip_prefix("@@")?;
        if self.symbol_ctor == 0 || !native::WELL_KNOWN_SYMBOLS.iter().any(|&(n, _)| n == js) {
            return None;
        }
        let sym = self.get_prop(Value::heap(self.symbol_ctor), js).ok()?;
        sym.is_heap().then_some(sym)
    }

    /// `DecField`, step 1: the initializer chain a decorated field's decorators
    /// returned, or an empty list (the common case — the element carried
    /// decorators but none of them returned an initializer).
    ///
    /// Split from the calls themselves so the dispatch arm can park the running
    /// value back in its REGISTER after every step: this path runs once per
    /// decorated field per constructed object, and a `gc_lock_guard` around it
    /// would suspend collection on the allocation-heavy `new` path. The chain's
    /// own entries need no such care — they stay rooted in the class's
    /// `DecState` for as long as the class lives.
    pub(crate) fn dec_field_inits(&self, class_id: u32, elem: usize) -> Vec<Value> {
        let Some(class) = self.dec_class_of_id(class_id) else {
            return Vec::new();
        };
        match self.dec_of(class) {
            Some(d) if elem < d.field_inits.len() => d.field_inits[elem].clone(),
            _ => Vec::new(),
        }
    }
}
