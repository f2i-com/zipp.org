// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// Build the callable for a class constructor: a plain `Func`, or a `Closure`
    /// over the cells the ctor captured (at class-definition time) when it closes
    /// over an enclosing-function local.
    pub(crate) fn ctor_value(&mut self, fid: u32, ups: &[u32]) -> Value {
        if ups.is_empty() {
            Value::heap(self.heap.alloc(HeapObj::Func(fid)))
        } else {
            Value::heap(self.heap.alloc(HeapObj::Closure {
                func: fid,
                upvalues: ups.to_vec(),
                this_val: Value::UNDEFINED,
            }))
        }
    }

    /// OrdinaryHasInstance(C, O) — the algorithm behind both the default
    /// `Function.prototype[Symbol.hasInstance]` method and the `instanceof`
    /// operator's non-overridden path. A bound function resolves to its target; a
    /// throwing `prototype` getter / non-object prototype propagates per spec
    /// (unlike the operator's cached-prototype fast path).
    pub(crate) fn ordinary_has_instance(&mut self, mut c: Value, v: Value) -> Result<bool, Thrown> {
        // Bound functions are guest-chainable. OrdinaryHasInstance delegates to
        // [[BoundTargetFunction]], but doing that with a Rust self-call lets one
        // `instanceof` exhaust the native stack before any VM frame limit runs.
        loop {
            let target = match c.is_heap().then(|| self.heap.get(c.heap_index())) {
                Some(HeapObj::Bound { target, .. }) => Some(*target),
                _ => None,
            };
            match target {
                Some(target) => c = target,
                None => break,
            }
        }
        if !self.is_callable(c) {
            // Symbol/BigInt are callable globals (typeof "function") that
            // is_callable reports false for (no user-invocable [[Construct]]);
            // as `instanceof` right operands they still take the ordinary
            // prototype-chain path (`Object(Symbol()) instanceof Symbol`).
            let special = c.is_heap()
                && ((self.symbol_ctor != 0 && c.heap_index() == self.symbol_ctor)
                    || (self.bigint_ctor != 0 && c.heap_index() == self.bigint_ctor));
            if !special {
                return Ok(false);
            }
        }
        if !self.is_object_value(v) {
            return Ok(false);
        }
        // C.prototype: a non-object DATA reassignment (`f.prototype = undefined`)
        // is recorded in fn_props because the lazy prototype cache only holds
        // object prototypes (get_prop returns that cache), so read such an override
        // directly. An accessor (`defineProperty` get) or an object prototype still
        // goes through get_prop — so a throwing prototype getter propagates.
        let data_override = if c.is_heap() {
            self.fn_props.get(&c.heap_index()).and_then(|m| {
                m.pos("prototype")
                    .and_then(|i| (!m.attr_at(i).accessor).then(|| m.val_at(i)))
            })
        } else {
            None
        };
        let p = match data_override {
            Some(v) => v,
            None => self.get_prop(c, "prototype")?,
        };
        if !self.is_object_value(p) {
            return Err(Thrown(
                "TypeError: Function has non-object prototype in instanceof check".into(),
            ));
        }
        // Walk V's prototype chain via [[GetPrototypeOf]] (proxy-trap-aware): a
        // throwing getPrototypeOf trap propagates per OrdinaryHasInstance step 7.b.
        let mut cur = self.get_prototype_of_checked(v)?;
        for _ in 0..10_000 {
            if !cur.is_heap() {
                return Ok(false);
            }
            if cur == p {
                return Ok(true);
            }
            cur = self.get_prototype_of_checked(cur)?;
        }
        Ok(false)
    }

    /// The class VALUE the constructor frame on top of the stack belongs to.
    ///
    /// `class_values[id]` holds only the LATEST evaluation of a class
    /// expression, so a chain built by re-evaluating one expression —
    ///     let chain = base;
    ///     for (let i = 0; i < 100; i++)
    ///         chain = class extends chain { constructor() { super(); } };
    ///     new chain();                      // staging/sm/class/superPropChains.js
    /// — made every level's `super()` resolve GetSuperConstructor against the
    /// NEWEST class, whose parent is the level below it: an infinite recursion
    /// that overflowed the native stack and ABORTED the process (not even a
    /// catchable RangeError). 15.7.16 GetSuperConstructor reads the *running*
    /// function object's [[GetPrototypeOf]], so recover that specific evaluation
    /// here.
    ///
    /// The link is the private brand, which is already minted per class
    /// EVALUATION: `method_brand[classValue] = [ownBrand, ...enclosing]` and
    /// `brand_owner[ownBrand] = classValue`, and run_class_ctor/[[Construct]]
    /// copy `method_brand` from the class value onto the freshly allocated ctor
    /// callable it invokes — which is this frame's `callee`. The
    /// `ctor == frame.func` check proves the recovered class really owns the
    /// running frame (a recycled heap index cannot fake it), and `None` leaves
    /// every caller on the previous `class_values` path.
    pub(crate) fn running_class_value(&self) -> Option<Value> {
        let f = self.frames.last()?;
        let callee = f.callee;
        if !callee.is_heap() {
            return None;
        }
        let brand = *self.method_brand.get(&callee.heap_index())?.first()?;
        let owner = *self.brand_owner.get(&brand)?;
        match self.heap.get(owner) {
            HeapObj::Class(c) if c.ctor == Some(f.func) => Some(Value::heap(owner)),
            _ => None,
        }
    }

    /// The superclass value for a `super` reference inside a method of class
    /// `home_class_id`: that class's runtime `ClassData.parent` (linked by
    /// MakeClass from the evaluated `extends` expression), or None.
    pub(crate) fn super_parent(&self, home_class_id: u32) -> Option<Value> {
        let home = match self.running_class_value() {
            Some(v) => v,
            None => (*self.class_values.get(home_class_id as usize)?)?,
        };
        match self.heap.get(home.heap_index()) {
            HeapObj::Class(c) => c.parent.map(Value::heap),
            _ => None,
        }
    }

    /// The fetch half of GetSuperConstructor() WITHOUT the IsConstructor check —
    /// the spec evaluates the SuperCall's argument list BETWEEN the two, so the
    /// compiler emits this (SuperCtorFetch) before the args and the check lands
    /// in the SuperCtor op that runs after them. UNDEFINED when unresolvable
    /// (the check then throws "superclass is not a constructor").
    pub(crate) fn super_ctor_fetch(&mut self, home_class_id: u32) -> Value {
        // The RUNNING ctor's own class evaluation, not merely the latest one
        // bound to this class_id — see `running_class_value`.
        let home = self.running_class_value().or_else(|| {
            self.class_values
                .get(home_class_id as usize)
                .copied()
                .flatten()
        });
        if let Some(h) = home.filter(|h| h.is_heap()) {
            if let Some(&p) = self.proto_of.get(&h.heap_index()) {
                return p;
            }
        }
        self.super_parent(home_class_id).unwrap_or(Value::UNDEFINED)
    }

    /// The super BASE object for a `super.x` reference inside a method of class
    /// `home_class_id`: GetPrototypeOf(HomeObject), where HomeObject is the class's
    /// own `prototype`. For `class C extends B` this is `B.prototype`; for a BASE
    /// class it is `%Object.prototype%` (so `super.toString()` etc. resolve). Returns
    /// UNDEFINED if unresolvable. (Unlike `super_parent`, this does not require a
    /// parent class, so `super.x` works in base-class methods.)
    pub(crate) fn super_base(&mut self, home_class_id: u32, is_static: bool) -> Value {
        let home = match self
            .class_values
            .get(home_class_id as usize)
            .copied()
            .flatten()
        {
            Some(c) => c,
            None => return Value::UNDEFINED,
        };
        // A STATIC element's HomeObject is the class value itself, so its super base
        // is GetPrototypeOf(class) — the PARENT CLASS for `class C extends B` (or
        // %Function.prototype% for a base class) — letting `super.x` reach inherited
        // STATIC members. An instance element's HomeObject is the class prototype.
        if is_static {
            // LIVE GetPrototypeOf(class): Object.setPrototypeOf(C, X) after the
            // definition re-targets `super.x` in static members (a null proto
            // then throws via the caller's RequireObjectCoercible).
            return self.object_get_prototype_of(home);
        }
        let home_proto = match self.prototype_of(home) {
            Some(p) => p,
            None => return Value::UNDEFINED,
        };
        self.object_get_prototype_of(home_proto)
    }

    /// The super BASE for an OBJECT-method `super.x`: GetPrototypeOf([[HomeObject]]),
    /// where the home object is looked up by the executing closure (`callee`) in
    /// `closure_home`. `UNDEFINED` if there is no home (then the caller's
    /// RequireObjectCoercible throws — shouldn't happen for well-formed object super).
    pub(crate) fn obj_super_base(&mut self, callee: Value) -> Value {
        let home = if callee.is_heap() {
            self.closure_home
                .get(&callee.heap_index())
                .copied()
                .unwrap_or(Value::UNDEFINED)
        } else {
            Value::UNDEFINED
        };
        if home.is_heap() {
            self.object_get_prototype_of(home)
        } else {
            Value::UNDEFINED
        }
    }

    /// `super.key = v` with the base prototype already resolved (the class
    /// variant resolves it via `super_base`, the object-method variant via the
    /// [[HomeObject]]). An inherited setter on the chain is invoked with `this`
    /// = the receiver; otherwise the value is set on the receiver, with the
    /// REFERENCE SITE's strictness (a sloppy method's rejected write no-ops).
    pub(crate) fn super_set_obj(
        &mut self,
        proto: Value,
        key: &str,
        this: Value,
        v: Value,
        strict: bool,
    ) -> Result<(), Thrown> {
        // MakeSuperPropertyReference: RequireObjectCoercible(GetSuperBase()).
        self.require_object_coercible(proto)?;
        // OrdinarySetWithOwnDescriptor consults Receiver.[[GetOwnProperty]]
        // FIRST: a deferred-namespace receiver triggers evaluation, and a
        // namespace receiver's uninit export throws ReferenceError.
        self.defer_check(this, key)?;
        self.ns_tdz_check(this, key)?;
        // `super.x = v` IS `GetSuperBase().[[Set]](x, v, Receiver = this)`, so it
        // must run the full OrdinarySetWithOwnDescriptor. Approximating it with
        // `Set(this, …)` after a chain setter probe got three cases wrong, all of
        // them silently (staging/sm/class/superProp{NoOverwriting,StrictAssign}):
        //   * an own ACCESSOR on the receiver rejects the write — it must NOT run
        //     the receiver's own setter (`super.p = 1` where `this.p` is an
        //     accessor is a TypeError, not a setter call);
        //   * an own non-writable data prop on the receiver rejects it;
        //   * a NON-WRITABLE data property found on the SUPER chain rejects it
        //     (the old code only looked for a setter, so it fell through and
        //     created an own prop on the receiver instead).
        // A rejected [[Set]] is a TypeError at a strict reference site and a
        // silent no-op at a sloppy one (an object-literal method in sloppy code).
        let kv = self.key_to_value(key);
        if !self.ordinary_set_with_receiver(proto, kv, v, this)? && strict {
            return Err(Thrown(format!(
                "TypeError: Cannot assign to read only property '{key}' of object"
            )));
        }
        Ok(())
    }

    /// Run a class's constructor contribution on an existing instance `obj` —
    /// for `super(...)` and the implicit-super chain. An explicit ctor runs its
    /// own `super`; an implicit one runs the parent chain then its fields.
    /// Completion of a `super(...)` call (the SuperCtor/SuperCtorSpread ops),
    /// AFTER the parent ctor ran (spec evaluates the SuperCall fully; only
    /// BindThisValue then throws on re-initialization): enforce the
    /// once-per-activation rule, rebind reg 0 to the produced `this`
    /// (return-override), lift the this-TDZ, mark super-called, and run the
    /// home class's deferred instance-field initializers on the result.
    pub(crate) fn super_ctor_complete(
        &mut self,
        base: usize,
        this: Value,
        produced: Value,
        home_class_id: u32,
    ) -> Result<(), Thrown> {
        let in_arrow = self
            .frames
            .last()
            .map(|f| self.func(f.func as usize).lexical_this)
            .unwrap_or(false);
        let already = if in_arrow {
            // An arrow frame has no super state of its own: the lexical ctor
            // activation already initialized `this` iff it left the TDZ.
            this.is_heap() && !self.this_tdz.contains(&this.heap_index())
        } else {
            self.frames.last().map(|f| f.super_done).unwrap_or(false)
        };
        if already {
            return Err(Thrown(
                "ReferenceError: super constructor may only be called once".into(),
            ));
        }
        if produced.is_heap() {
            self.set(base, 0, produced);
            // A parent RETURN-OVERRIDE must become the construction result even
            // when the derived ctor later returns `undefined` (reg 0 alone
            // doesn't reach construct()) — bank it keyed by the original this.
            if produced != this && this.is_heap() {
                self.super_this.insert(this.heap_index(), produced);
            }
        }
        if let Some(f) = self.frames.last_mut() {
            f.super_done = true;
        }
        if this.is_heap() {
            self.this_tdz.remove(&this.heap_index());
            self.super_called.insert(this.heap_index());
        }
        // InitializeInstanceElements: this class's field initializers run NOW
        // (not at ctor entry), on the produced instance, with no new.target.
        // Same evaluation the `super()` above resolved against, so a chain built
        // by re-evaluating one class expression initializes each level's OWN
        // fields rather than the newest evaluation's.
        let cls_v = self.running_class_value().or_else(|| {
            self.class_values
                .get(home_class_id as usize)
                .copied()
                .flatten()
        });
        if let Some(cv) = cls_v {
            // Checked PrivateBrandAdd: a return-override that already carries
            // this class's private elements, or a non-extensible instance,
            // throws TypeError BEFORE the field initializers run.
            let inst = if produced.is_heap() { produced } else { this };
            let is_override = produced.is_heap() && produced != this;
            self.private_init_checked(inst, cv, is_override)?;
            self.run_field_thunk(inst, cv)?;
        }
        Ok(())
    }

    /// The instance-field half of InitializeInstanceElements for a class whose
    /// ctor is EXPLICIT: its initializers live in a separate thunk (see
    /// `defer_fields` in compile/funcs.rs) so they run at the spec's point —
    /// after super() for a derived class, before the ctor body for a base one —
    /// and in the class scope rather than the ctor's parameter scope. No-op for a
    /// class that declares no instance fields. `new.target` is deliberately left
    /// alone: it is undefined inside a field initializer.
    pub(crate) fn run_field_thunk(&mut self, inst: Value, cv: Value) -> Result<(), Thrown> {
        if !cv.is_heap() {
            return Ok(());
        }
        let (tfid, tups) = match self.heap.get(cv.heap_index()) {
            HeapObj::Class(c) => (c.field_thunk, c.field_thunk_upvalues.clone()),
            _ => (None, Vec::new()),
        };
        let Some(fid) = tfid else { return Ok(()) };
        let f = self.ctor_value(fid, &tups);
        // The thunk runs in the class body's private scope (same brand chain
        // handed to the ctor in `construct`).
        if let Some(brands) = self.method_brand.get(&cv.heap_index()).cloned() {
            if f.is_heap() {
                self.method_brand.insert(f.heap_index(), brands);
            }
        }
        self.call_value(f, inst, &[])?;
        Ok(())
    }

    pub(crate) fn run_class_ctor(
        &mut self,
        cval: Value,
        obj: Value,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, Thrown> {
        if !cval.is_heap() {
            return Ok(obj);
        }
        let (ctor, ctor_ups, has_explicit, parent, extends_null) =
            match self.heap.get(cval.heap_index()) {
                HeapObj::Class(c) => (
                    c.ctor,
                    c.ctor_upvalues.clone(),
                    c.has_explicit_ctor,
                    c.parent,
                    c.extends_null,
                ),
                // `super(...)` to a BUILT-IN parent (`class X extends Error`). We model
                // the Error family: set `message` on the instance from the argument
                // (AggregateError takes it as the 2nd arg). The instance's prototype
                // chain already reaches the error prototype (so name/toString/
                // instanceof resolve), so nothing else is needed here.
                _ => {
                    // `super(...)` to a BUILT-IN EXOTIC parent (`class X extends Set/…`):
                    // brand the plain-Object instance with the builtin's internal
                    // representation so its methods work and it is a real instanceof. The
                    // instance keeps its own (subclass) prototype, recorded in proto_of.
                    if self.brand_builtin_subclass(cval, obj, args)? {
                        return Ok(obj);
                    }
                    // `class X extends SuppressedError`: super(error, suppressed, msg)
                    // installs the three own props on the instance and brands the
                    // [[ErrorData]] slot (so Error.isError is true).
                    if cval.heap_index() == self.suppressederror_ctor
                        && self.suppressederror_ctor != 0
                    {
                        let error = args.first().copied().unwrap_or(Value::UNDEFINED);
                        let suppressed = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                        let message = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                        let msg_val = if message != Value::UNDEFINED {
                            Some(self.to_str_value(message)?)
                        } else {
                            None
                        };
                        let attr = PropAttr {
                            writable: true,
                            enumerable: false,
                            configurable: true,
                            accessor: false,
                            setter: Value::UNDEFINED,
                        };
                        if let HeapObj::Object(map) = self.heap.get_mut(obj.heap_index()) {
                            if let Some(mv) = msg_val {
                                map.define("message", mv, attr);
                            }
                            map.define("error", error, attr);
                            map.define("suppressed", suppressed, attr);
                        }
                        self.error_data.insert(obj.heap_index());
                        return Ok(obj);
                    }
                    if let Some(k) = self
                        .error_ctors
                        .iter()
                        .position(|&c| c == cval.heap_index())
                    {
                        // `class X extends Error` instance: it has the [[ErrorData]] slot.
                        self.error_data.insert(obj.heap_index());
                        let msg = if k == 7 {
                            args.get(1).copied()
                        } else {
                            args.first().copied()
                        };
                        if let Some(m) = msg.filter(|m| *m != Value::UNDEFINED) {
                            let message_value = self.to_str_value(m)?;
                            if let HeapObj::Object(map) = self.heap.get_mut(obj.heap_index()) {
                                // `message` is a non-enumerable own data property.
                                map.define(
                                    "message",
                                    message_value,
                                    PropAttr {
                                        writable: true,
                                        enumerable: false,
                                        configurable: true,
                                        accessor: false,
                                        setter: Value::UNDEFINED,
                                    },
                                );
                            }
                        }
                    }
                    // `class X extends someProxy`: SuperCall is
                    // `Construct(parent, args, newTarget)`, so the PROXY's
                    // [[Construct]] runs — its `construct` trap, or (trap-less) the
                    // forward to its target. The result IS the instance: with
                    // newTarget still the derived class, OrdinaryCreateFromConstructor
                    // gives it the derived prototype, and `super_ctor_complete`
                    // rebinds `this` to it. Without this arm the parent constructor
                    // never ran at all and `this` kept the pre-made empty object
                    // (staging/sm/class/superCall{BaseInvoked,ProperBase}.js).
                    if self.proxy_parts(cval.heap_index()).is_some() {
                        if !self.is_constructor(cval) {
                            return Err(Thrown(
                                "TypeError: the superclass is not a constructor".into(),
                            ));
                        }
                        return self.construct_with_newtarget(cval, args, new_target);
                    }
                    // `class X extends f` where f is a PLAIN user function: super(...)
                    // must actually INVOKE the parent with this = the instance and the
                    // subclass new.target (a non-constructor parent is a TypeError; an
                    // object return is the return-override instance).
                    if matches!(
                        self.heap.get(cval.heap_index()),
                        HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. }
                    ) {
                        if !self.is_constructor(cval) {
                            return Err(Thrown(
                                "TypeError: the superclass is not a constructor".into(),
                            ));
                        }
                        self.pending_new_target = new_target;
                        let r = self.call_value(cval, obj, args)?;
                        if self.is_object_value(r) {
                            return Ok(r);
                        }
                    }
                    return Ok(obj);
                }
            };
        if has_explicit {
            // An explicit ctor produces `this`: its object-return (return-override)
            // becomes the effective instance; a non-object/undefined return keeps obj.
            if let Some(fid) = ctor {
                let f = self.ctor_value(fid, &ctor_ups);
                if let Some(brands) = self.method_brand.get(&cval.heap_index()).cloned() {
                    if f.is_heap() {
                        self.method_brand.insert(f.heap_index(), brands);
                    }
                }
                // A BASE parent's InitializeInstanceElements runs at entry —
                // brand, then fields, both before the parent ctor's parameter
                // prologue.
                if parent.is_none() && !extends_null {
                    self.brand_instance(obj, cval);
                    self.run_field_thunk(obj, cval)?;
                }
                // A derived parent ctor begins with `this` back in TDZ (until
                // ITS OWN super() completes). No removal here: if the parent
                // throws pre-super, the caller's binding is still uninitialized
                // (a catching outer ctor may legitimately retry super()); the
                // outermost construct() clears the mark on every exit.
                if parent.is_some() || extends_null {
                    self.this_tdz.insert(obj.heap_index());
                }
                // `new.target` propagates unchanged through super() to the parent ctor.
                self.pending_new_target = new_target;
                let r = self.call_value(f, obj, args)?;
                // An undefined return yields super()'s produced this (a parent
                // return-override banked by super_ctor_complete), else obj. The
                // banked mapping is READ, not removed: it doubles as the escaped-
                // arrow `this` resolution (ThisCheck) for THIS activation, so it
                // must survive a repeated super() attempt (which runs the parent
                // ctor again before BindThisValue throws) — the GC prunes it once
                // the placeholder is unreachable.
                let result = if self.is_object_value(r) {
                    r
                } else if let Some(st) = self.super_this.get(&obj.heap_index()).copied() {
                    st
                } else {
                    obj
                };
                // A return-override result receives this class's private brand.
                self.brand_instance(result, cval);
                return Ok(result);
            }
            Ok(obj)
        } else {
            // Implicit ctor: run the parent chain (threading its produced `this`),
            // then this class's field initializers on it.
            //
            // A default derived constructor is `constructor(...args){ super(...args) }`,
            // and its `super` resolves through GetSuperConstructor() — the class
            // object's LIVE [[GetPrototypeOf]]. Using the `parent` recorded at
            // class-definition time made `Object.setPrototypeOf(D, Other)` a no-op
            // for a class with no explicit ctor, while the explicit-ctor bytecodes
            // already retargeted
            // (staging/sm/class/superCallProperBase.js).
            let mut eff = obj;
            if parent.is_some() {
                let live = self.object_get_prototype_of(cval);
                let target = if live.is_heap() {
                    live
                } else {
                    Value::heap(parent.unwrap())
                };
                eff = self.run_class_ctor(target, eff, args, new_target)?;
            }
            // PrivateBrandAdd before this class's field initializers run.
            self.brand_instance(eff, cval);
            if let Some(fid) = ctor {
                let f = self.ctor_value(fid, &ctor_ups);
                if let Some(brands) = self.method_brand.get(&cval.heap_index()).cloned() {
                    if f.is_heap() {
                        self.method_brand.insert(f.heap_index(), brands);
                    }
                }
                self.pending_new_target = new_target;
                let r = self.call_value(f, eff, &[])?;
                if self.is_object_value(r) {
                    eff = r;
                }
            }
            Ok(eff)
        }
    }
}
