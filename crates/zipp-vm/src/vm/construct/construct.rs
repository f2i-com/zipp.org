// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Upper bound on a learned `ctor_field_hint` (see `Vm::ctor_field_hint`).
/// Keeps one outlier instance from teaching a huge reservation permanently.
const CTOR_FIELD_HINT_MAX: u16 = 32;

impl<'p> Vm<'p> {
    /// `new cv(args)` with newTarget defaulting to the constructor itself (the
    /// common case for `new` / a plain `Reflect.construct(cv, args)`).
    pub(crate) fn construct(&mut self, cv: Value, args: &[Value]) -> Result<Value, Thrown> {
        self.construct_with_newtarget(cv, args, cv)
    }

    /// OrdinaryCreateFromConstructor's prototype selection: when `new_target`
    /// differs from the base constructor `cval` (a `Reflect.construct(c, args,
    /// newTarget)` or a derived-class `super()`), the instance's [[Prototype]] is
    /// `Get(new_target, "prototype")` when that is an object, else `default`. For
    /// the common `new C()` case (`new_target == cval`) the default — `cval`'s own
    /// prototype — is used unchanged (no extra Get on the hot path).
    pub(crate) fn newtarget_proto(
        &mut self,
        new_target: Value,
        cval: Value,
        default: Value,
    ) -> Result<Value, Thrown> {
        if new_target.is_heap() && new_target != cval {
            let p = self.get_prop(new_target, "prototype")?;
            if self.is_object_value(p) {
                return Ok(p);
            }
            // GetFunctionRealm(newTarget) throws on a REVOKED proxy — the
            // `prototype` Get's trap may have just revoked it.
            self.check_function_realm_reachable(new_target)?;
            // Non-object prototype: GetPrototypeFromConstructor falls back to
            // GetFunctionRealm(newTarget)'s intrinsic prototype — the realm's
            // image of `default` when it IS an intrinsic (Iterator), else the
            // realm's %Object.prototype% (an ordinary function's `default` is
            // its own `.prototype` object, which has no realm image).
            if default.is_heap() {
                if let Some(rp) = self.realm_proto_fallback(new_target, default.heap_index()) {
                    return Ok(Value::heap(rp));
                }
            }
            if let Some(rp) = self.realm_proto_fallback(new_target, self.obj_proto) {
                return Ok(Value::heap(rp));
            }
        }
        Ok(default)
    }

    /// GetFunctionRealm (10.2.5) reduced to its only OBSERVABLE effect: a REVOKED
    /// Proxy anywhere in the bound-target / proxy-target unwrap chain throws a
    /// TypeError (step 3.a). GetPrototypeFromConstructor reaches it exactly when
    /// `Get(newTarget, "prototype")` did NOT return an object — which is the case
    /// a `get` trap that revokes its own proxy and returns undefined produces, for
    /// every built-in constructor
    /// (staging/sm/Proxy/revoked-get-function-realm-typeerror.js).
    pub(crate) fn check_function_realm_reachable(&self, f: Value) -> Result<(), Thrown> {
        let mut cur = f;
        // Bounded: a proxy/bound chain is acyclic by construction, but a corrupt
        // one must not hang the engine.
        for _ in 0..1000 {
            if !cur.is_heap() {
                return Ok(());
            }
            let idx = cur.heap_index();
            if let Some((target, _, revoked)) = self.proxy_parts(idx) {
                if revoked {
                    return Err(Thrown(
                        "TypeError: Cannot get the function realm of a revoked proxy".into(),
                    ));
                }
                cur = target;
                continue;
            }
            match self.heap.get(idx) {
                HeapObj::Bound { target, .. } => cur = *target,
                _ => return Ok(()),
            }
        }
        Ok(())
    }

    /// For a cross-realm `new_target` whose `prototype` is not an object, the
    /// realm's copy of `main_proto` (the intrinsic default proto) — else None.
    pub(crate) fn realm_proto_fallback(&self, new_target: Value, main_proto: u32) -> Option<u32> {
        let r = self.get_function_realm(new_target) as usize;
        if r != 0 {
            return self.realms.get(r).and_then(|m| m.get(&main_proto).copied());
        }
        None
    }

    /// `Get(new_target, "prototype")` when it is an object and `new_target`
    /// differs from the base constructor — the [[Prototype]] override a built-in
    /// constructor must apply when built via `Reflect.construct(C, args, newTarget)`
    /// / a derived `super()` / a cross-realm newTarget. For a cross-realm newTarget
    /// with a non-object prototype, falls back to that realm's `%default_proto%`.
    /// `None` for the ordinary `new C()` case (use the built-in's default prototype).
    pub(crate) fn newtarget_proto_override(
        &mut self,
        new_target: Value,
        cv: Value,
        default_proto: u32,
    ) -> Result<Option<Value>, Thrown> {
        if new_target.is_heap() && new_target != cv {
            let p = self.get_prop(new_target, "prototype")?;
            if self.is_object_value(p) {
                return Ok(Some(p));
            }
            // GetPrototypeFromConstructor step 3.a: a non-object `prototype`
            // falls back to GetFunctionRealm(newTarget), which THROWS when the
            // constructor is a revoked Proxy.
            self.check_function_realm_reachable(new_target)?;
            if let Some(rp) = self.realm_proto_fallback(new_target, default_proto) {
                return Ok(Some(Value::heap(rp)));
            }
        }
        Ok(None)
    }

    /// Apply a newTarget [[Prototype]] override to a freshly-built built-in
    /// instance (an Array/Object/Map/Date/Boxed/… created by `Reflect.construct`
    /// with a foreign newTarget). A no-op when `over` is `None`.
    pub(crate) fn set_ctor_proto(&mut self, result: Value, over: Option<Value>) -> Value {
        if let Some(p) = over {
            if result.is_heap() {
                self.proto_of.insert(result.heap_index(), p);
            }
        }
        result
    }

    /// [[Construct]](argumentsList, newTarget). `new_target` is threaded to a Proxy
    /// `construct` trap (its 3rd argument), through a trap-less Proxy's forward to
    /// the target, and into the instance's [[Prototype]] via OrdinaryCreateFrom
    /// Constructor (see `newtarget_proto`) for the Func/Class paths.
    pub(crate) fn construct_with_newtarget(
        &mut self,
        cv: Value,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, Thrown> {
        if !cv.is_heap() {
            return Err(Thrown("TypeError: value is not a constructor".into()));
        }
        // A constructor from a `$262.createRealm()` realm. Only the realm's
        // FACADE constructors (plain Objects) take this route — a realm-TAGGED
        // Proxy / class / real function / native (stage-1 realms create those
        // too) must run its ordinary [[Construct]] below (a realm class's ctor
        // body, a Proxy's construct trap, a non-constructor's TypeError).
        let cr = self.get_function_realm(cv);
        if cr != 0
            && self.proxy_parts(cv.heap_index()).is_none()
            && matches!(self.heap.get(cv.heap_index()), HeapObj::Object(_))
        {
            // If we know the MAIN-realm constructor it mirrors, build a REAL instance
            // by delegating to it with `cv` as newTarget (so the instance's
            // [[Prototype]] is the realm's `X.prototype`), then tag it with the realm.
            // (`fn.prototype` is now assignable to a non-object, so a real
            // `new other.Function()` works as a settable-prototype newTarget.)
            if let Some(&main) = self.realm_ctor_main.get(&cv.heap_index()) {
                // `new other.Function(src)` (and the generator/async variants):
                // compile `src` with the CHILD realm ACTIVE, so the function's
                // globals bind in the child's own table (its `this`-global and
                // error identities follow from the realm tag below).
                let prev_realm = self.active_realm;
                if self.realm_main_ctor_is_fn_like(main) {
                    if let Some(g) = self.realm_global_obj(cr) {
                        self.active_realm = Some(g);
                    }
                }
                // Preserve an EXPLICIT foreign newTarget (`Reflect.construct(
                // other.Function, args, nt)` resolves nt's realm prototype);
                // the common `new other.X()` case keeps the facade (cv) so the
                // REALM's prototype applies.
                let nt = if new_target == cv { cv } else { new_target };
                // The delegated [[Construct]] runs ON BEHALF OF the child realm,
                // so any intrinsic object it allocates internally belongs to that
                // realm: `new g.AggregateError([e]).errors` must have
                // `g.Array.prototype` (staging/sm/Error/AggregateError.js line 85).
                // `native_callee_realm` is the existing "which realm's built-in is
                // running" context that `alloc_array_current_realm` consults.
                let prev_ncr = self.native_callee_realm;
                self.native_callee_realm = Some(cr);
                let res = self.construct_with_newtarget(Value::heap(main), args, nt);
                self.native_callee_realm = prev_ncr;
                self.active_realm = prev_realm;
                // Same rule as the call route: an INTERNAL throw from the realm's
                // own [[Construct]] carries that realm's error identity.
                if let Err(ref t) = res {
                    if self.pending_throw.is_none() {
                        let e = self.alloc_error_from_message(&t.0);
                        self.realm_adopt_error_to(e, cr);
                        self.pending_throw = Some(e);
                    }
                }
                let res = res?;
                if res.is_heap() {
                    self.obj_realm.insert(res.heap_index(), cr);
                }
                return Ok(res);
            }
            // Otherwise, for a realm CONSTRUCTOR object with no main mirror: a
            // fresh realm-tagged, function-like object (a valid foreign
            // newTarget / GetFunctionRealm subject). A realm-tagged NON-ctor
            // object falls through to the ordinary paths (TypeError).
            if matches!(self.heap.get(cv.heap_index()), HeapObj::Object(m) if m.is_ctor) {
                let proto_idx = self.heap.alloc(HeapObj::Object(Box::new(ObjMap::new())));
                let mut m = ObjMap::new();
                m.is_ctor = true;
                m.define("prototype", Value::heap(proto_idx), PropAttr::data());
                let idx = self.heap.alloc(HeapObj::Object(Box::new(m)));
                self.obj_realm.insert(idx, cr);
                self.obj_realm.insert(proto_idx, cr);
                return Ok(Value::heap(idx));
            }
        }
        // A built-in error constructor used as a VALUE (`var E = TypeError; new E()`,
        // `Reflect.construct(RangeError, [msg])`). Mirrors the compile-lowered
        // `new TypeError(msg)` path. AggregateError takes the message as arg[1].
        if let Some(k) = self.error_ctors.iter().position(|&c| c == cv.heap_index()) {
            let over = self.newtarget_proto_override(new_target, cv, self.error_protos[k])?;
            // AggregateError (k==7) takes its message as arg[1] and coerces it with a
            // real ToString (observable / abrupt) before iterating arg[0] into `errors`.
            let e = if k == 7 {
                let msg = match args.get(1).copied() {
                    Some(m) if m != Value::UNDEFINED => Some(self.to_str_value(m)?),
                    _ => None,
                };
                let e = self.make_error(7, msg);
                let errors_arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.install_agg_errors(e, errors_arg)?;
                e
            } else {
                // Coerce `message` with a real ToString FIRST (observable / abrupt):
                // a Symbol message throws TypeError, and a throwing toString /
                // @@toPrimitive propagates — before the error object is allocated.
                let msg = match args.first().copied() {
                    Some(m) if m != Value::UNDEFINED => Some(self.to_str_value(m)?),
                    _ => None,
                };
                self.make_error(k as u8, msg)
            };
            // InstallErrorCause: options (arg 1; arg 2 for AggregateError) with
            // a `cause` (HasProperty: proto chain + has trap, observable) adds
            // a non-enumerable own `cause` data property.
            let options = args.get(if k == 7 { 2 } else { 1 }).copied().unwrap_or(Value::UNDEFINED);
            if self.is_object_value(options) {
                let kc = self.alloc_str("cause".to_string());
                if self.has_property_dyn(options, kc)? {
                    let cause = self.get_prop(options, "cause")?;
                    if let HeapObj::Object(m) = self.heap.get_mut(e.heap_index()) {
                        m.define(
                            "cause",
                            cause,
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
            return Ok(self.set_ctor_proto(e, over));
        }
        // ArrayBuffer / DataView / TypedArray constructors used as values.
        let ci = cv.heap_index();
        if ci == self.function_ctor && ci != 0 {
            // CreateDynamicFunction PARSES (step 16) before
            // GetPrototypeFromConstructor(newTarget, …) (step 21): a SyntaxError
            // in the body must leave `newTarget.prototype` unread
            // (sm/*/create-function-parse-before-getprototype.js).
            let r = self.build_function(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.fn_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        // The dynamic-function intrinsics: GetPrototypeFromConstructor with the
        // matching HIDDEN intrinsic default (%GeneratorFunction.prototype% etc.)
        // — a foreign newTarget with a non-object `prototype` falls back to the
        // NEWTARGET REALM's image of that intrinsic (realms map entries built by
        // create_realm).
        if ci == self.gen_fn_ctor && ci != 0 {
            let r = self.build_function_kind(args, 1)?;
            let over = self.newtarget_proto_override(new_target, cv, self.gen_fn_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.async_fn_ctor && ci != 0 {
            let r = self.build_function_kind(args, 2)?;
            let over = self.newtarget_proto_override(new_target, cv, self.async_fn_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.asyncgen_fn_ctor && ci != 0 {
            let r = self.build_function_kind(args, 3)?;
            let over = self.newtarget_proto_override(new_target, cv, self.asyncgen_fn_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.arraybuffer_ctor && ci != 0 {
            // The observable argument coercions run BEFORE newTarget.prototype
            // is read (OrdinaryCreateFromConstructor), and allocation after both.
            let (n, max) = self.validate_array_buffer_args(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.arraybuffer_proto)?;
            if n > super::typedarray::MAX_ARRAY_BUFFER_LEN as usize {
                return Err(Thrown("RangeError: ArrayBuffer length exceeds the maximum".into()));
            }
            let buf = self.alloc_array_buffer(n);
            if let Some(m) = max {
                self.ab_max.insert(buf, m);
            }
            return Ok(self.set_ctor_proto(Value::heap(buf), over));
        }
        if ci == self.sab_ctor && ci != 0 {
            let (n, max) = self.validate_array_buffer_args(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.sab_proto)?;
            if n > super::typedarray::MAX_ARRAY_BUFFER_LEN as usize {
                return Err(Thrown("RangeError: ArrayBuffer length exceeds the maximum".into()));
            }
            // Truly-shared storage (marks shared_buffers + links sab_proto).
            let buf = self.alloc_shared_array_buffer(n, max);
            if let Some(m) = max {
                self.ab_max.insert(buf, m);
            }
            return Ok(self.set_ctor_proto(Value::heap(buf), over));
        }
        if ci == self.disposablestack_ctor && ci != 0 {
            let r = Value::heap(self.alloc_disposable_stack(false));
            let over = self.newtarget_proto_override(new_target, cv, self.disposablestack_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.asyncdisposablestack_ctor && ci != 0 {
            let r = Value::heap(self.alloc_disposable_stack(true));
            let over =
                self.newtarget_proto_override(new_target, cv, self.asyncdisposablestack_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.suppressederror_ctor && ci != 0 {
            let r = self.build_suppressed_error(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.suppressederror_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.weakmap_ctor && ci != 0 {
            let over = self.newtarget_proto_override(new_target, cv, self.weakmap_proto)?;
            let wm = Value::heap(
                self.heap.alloc(HeapObj::WeakMap { keys: Vec::new(), vals: Vec::new() }),
            );
            let wm = self.set_ctor_proto(wm, over);
            let it = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !it.is_nullish() {
                self.add_entries_via_adder(wm, it, true)?;
            }
            return Ok(wm);
        }
        if ci == self.weakset_ctor && ci != 0 {
            let over = self.newtarget_proto_override(new_target, cv, self.weakset_proto)?;
            let ws = Value::heap(self.heap.alloc(HeapObj::WeakSet(Vec::new())));
            let ws = self.set_ctor_proto(ws, over);
            let it = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !it.is_nullish() {
                self.add_entries_via_adder(ws, it, false)?;
            }
            return Ok(ws);
        }
        if ci == self.weakref_ctor && ci != 0 {
            let t = args.first().copied().unwrap_or(Value::UNDEFINED);
            // CanBeHeldWeakly: any object, or a non-registered Symbol.
            if !self.can_be_held_weakly(t) {
                return Err(Thrown("TypeError: WeakRef: target cannot be held weakly".into()));
            }
            let over = self.newtarget_proto_override(new_target, cv, self.weakref_proto)?;
            let wr = Value::heap(self.heap.alloc(HeapObj::WeakRef(t)));
            return Ok(self.set_ctor_proto(wr, over));
        }
        if ci == self.finreg_ctor && ci != 0 {
            let cb = args.first().copied().unwrap_or(Value::UNDEFINED);
            if self.type_of(cb) != "function" {
                return Err(Thrown(
                    "TypeError: FinalizationRegistry: cleanup callback must be callable".into(),
                ));
            }
            let over = self.newtarget_proto_override(new_target, cv, self.finreg_proto)?;
            let fr = Value::heap(
                self.heap.alloc(HeapObj::FinalizationRegistry { cleanup: cb, tokens: Vec::new() }),
            );
            return Ok(self.set_ctor_proto(fr, over));
        }
        if ci == self.abstractmodulesource_ctor && ci != 0 {
            // 28.1.1.1 %AbstractModuleSource% ( ): step 1 — throw a TypeError
            // (abstract; never directly constructable, even via a subclass).
            return Err(Thrown(
                "TypeError: AbstractModuleSource is not constructable".into(),
            ));
        }
        if ci == self.shadowrealm_ctor && ci != 0 {
            let over = self.newtarget_proto_override(new_target, cv, self.shadowrealm_proto)?;
            let idx = self.heap.alloc(HeapObj::Object(Box::new(ObjMap::new())));
            if self.shadowrealm_proto != 0 {
                self.proto_of.insert(idx, Value::heap(self.shadowrealm_proto));
            }
            self.shadow_realms.insert(idx);
            return Ok(self.set_ctor_proto(Value::heap(idx), over));
        }
        if ci == self.dataview_ctor && ci != 0 {
            let r = self.build_data_view(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.dataview_proto)?;
            // OrdinaryCreateFromConstructor read newTarget.prototype (a user
            // getter may have detached or shrunk the buffer): re-validate the
            // view per GetViewByteLength before exposing it.
            if let HeapObj::DataView { buffer, byte_offset, byte_length } =
                *self.heap.get(r.heap_index())
            {
                if matches!(self.heap.get(buffer), HeapObj::ArrayBuffer { detached: true, .. }) {
                    return Err(Thrown(
                        "TypeError: Cannot construct a DataView on a detached ArrayBuffer".into(),
                    ));
                }
                let bl = self.array_buffer_len(buffer);
                if byte_offset > bl {
                    return Err(Thrown("RangeError: invalid DataView offset".into()));
                }
                let explicit = matches!(args.get(2), Some(&v) if v != Value::UNDEFINED);
                if explicit && byte_offset + byte_length > bl {
                    return Err(Thrown("RangeError: invalid DataView offset/length".into()));
                }
            }
            return Ok(self.set_ctor_proto(r, over));
        }
        if let Some(k) = self.ta_ctors.iter().position(|&c| c == ci && ci != 0) {
            // 23.2.5.1 steps 5 and 6.b.i: with NO arguments, or an OBJECT first
            // argument, AllocateTypedArray runs FIRST — and its
            // OrdinaryCreateFromConstructor does Get(NewTarget, "prototype"), so a
            // throwing `prototype` accessor must win over the byteOffset/length
            // ToIndex coercions and the detached-buffer check that
            // InitializeTypedArrayFromArrayBuffer performs
            // (staging/sm/TypedArray/constructor-buffer-sequence.js line 71).
            // Step 6.c is the other way round for a PRIMITIVE first argument
            // (ToIndex(firstArgument), THEN AllocateTypedArray), so that order is
            // kept below.
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            let alloc_first = args.is_empty() || self.is_object_value(a0);
            let early = if alloc_first {
                Some(self.newtarget_proto_override(new_target, cv, self.ta_protos[k])?)
            } else {
                None
            };
            let r = self.build_typed_array(k as u8, args)?;
            // OrdinaryCreateFromConstructor: a foreign/derived newTarget sets the
            // instance's [[Prototype]] (cross-realm intrinsic fallback when its
            // .prototype is not an object).
            let over = match early {
                Some(o) => o,
                None => self.newtarget_proto_override(new_target, cv, self.ta_protos[k])?,
            };
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.ta_base_ctor && ci != 0 {
            return Err(Thrown("TypeError: Abstract class TypedArray not directly constructable".into()));
        }
        if ci == self.iterator_ctor && ci != 0 {
            // %Iterator% is abstract ONLY against itself: `new Iterator()` (or
            // NewTarget === %Iterator%) throws; a subclass/foreign NewTarget
            // (super(), Reflect.construct) makes an ordinary object whose
            // [[Prototype]] comes from NewTarget (realm-intrinsic fallback).
            if !new_target.is_heap()
                || new_target == cv
                || new_target.heap_index() == self.iterator_ctor
            {
                return Err(Thrown(
                    "TypeError: Abstract class Iterator not directly constructable".into(),
                ));
            }
            let proto =
                self.newtarget_proto(new_target, cv, Value::heap(self.iterator_proto_root))?;
            let oidx = self.heap.alloc(HeapObj::Object(Box::new(ObjMap::new())));
            if proto.is_heap() {
                self.proto_of.insert(oidx, proto);
            }
            return Ok(Value::heap(oidx));
        }
        if ci == self.proxy_ctor && ci != 0 {
            return self.make_proxy(
                args.first().copied().unwrap_or(Value::UNDEFINED),
                args.get(1).copied().unwrap_or(Value::UNDEFINED),
            );
        }
        if ci == self.duration_ctor && ci != 0 {
            let r = self.build_duration(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.duration_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.plaindate_ctor && ci != 0 {
            let y = self.temporal_ctor_int(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            let m = self.temporal_ctor_int(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
            let d = self.temporal_ctor_int(args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
            let cal = self.validate_calendar_identifier(args.get(3).copied().unwrap_or(Value::UNDEFINED))?;
            let r = self.make_plain_date(y, m, d)?;
            let r = self.tag_cal(r, cal);
            let over = self.newtarget_proto_override(new_target, cv, self.plaindate_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.plaintime_ctor && ci != 0 {
            let mut f = [0i64; 6];
            for (i, slot) in f.iter_mut().enumerate() {
                let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
                if v != Value::UNDEFINED {
                    *slot = self.temporal_ctor_int(v)?;
                }
            }
            let r = self.make_plain_time(f)?;
            let over = self.newtarget_proto_override(new_target, cv, self.plaintime_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.plaindatetime_ctor && ci != 0 {
            // year/month/day are required: an undefined coerces to NaN → RangeError.
            // The time fields (i >= 3) default to 0 when undefined.
            let mut f = [0i64; 9];
            for (i, slot) in f.iter_mut().enumerate() {
                let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
                if i < 3 || v != Value::UNDEFINED {
                    *slot = self.temporal_ctor_int(v)?;
                }
            }
            let cal = self.validate_calendar_identifier(args.get(9).copied().unwrap_or(Value::UNDEFINED))?;
            let r = self.make_plain_date_time(f)?;
            let r = self.tag_cal(r, cal);
            let over = self.newtarget_proto_override(new_target, cv, self.plaindatetime_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.instant_ctor && ci != 0 {
            // Beyond-i128 saturates (sign preserved) — certainly outside the
            // Instant range, which make_instant validates.
            let ns =
                self.to_bigint(args.first().copied().unwrap_or(Value::UNDEFINED))?.to_i128_sat();
            let r = self.make_instant(ns)?;
            let over = self.newtarget_proto_override(new_target, cv, self.instant_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.plainyearmonth_ctor && ci != 0 {
            // (year, month, calendar?, referenceISODay=1)
            let y = self.temporal_ctor_int(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            let m = self.temporal_ctor_int(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
            let cal = self.validate_calendar_identifier(args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
            let rd = match args.get(3).copied() {
                Some(v) if v != Value::UNDEFINED => self.temporal_ctor_int(v)?,
                _ => 1,
            };
            let r = self.make_plain_year_month(y, m, rd)?;
            let r = self.tag_cal(r, cal);
            let over = self.newtarget_proto_override(new_target, cv, self.plainyearmonth_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.plainmonthday_ctor && ci != 0 {
            // (month, day, calendar?, referenceISOYear=1972)
            let m = self.temporal_ctor_int(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            let d = self.temporal_ctor_int(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
            let cal = self.validate_calendar_identifier(args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
            let ry = match args.get(3).copied() {
                Some(v) if v != Value::UNDEFINED => self.temporal_ctor_int(v)?,
                _ => 1972,
            };
            let r = self.make_plain_month_day(m, d, ry)?;
            let r = self.tag_cal(r, cal);
            let over = self.newtarget_proto_override(new_target, cv, self.plainmonthday_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.zoneddatetime_ctor && ci != 0 {
            let cal = self.validate_calendar_identifier(args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
            let r = self.make_zoned_date_time(args)?;
            let r = self.tag_cal(r, cal);
            let over = self.newtarget_proto_override(new_target, cv, self.zoneddatetime_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        // Intl.<service> constructors.
        if self.intl_ctors[0] != 0 {
            if let Some(kind) = self.intl_ctors.iter().position(|&c| c == ci) {
                let locales = args.first().copied().unwrap_or(Value::UNDEFINED);
                let options = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                // OrdinaryCreateFromConstructor(newTarget, "%Intl.<svc>.prototype%"):
                // a subclass / Reflect.construct newTarget supplies the instance's
                // [[Prototype]] (`class M extends Intl.Collator {}` must produce an
                // M, not a bare Collator). It is step 2 of every Intl constructor,
                // BEFORE Initialize<Service> — so a newTarget whose "prototype"
                // getter throws wins over any options/locale error.
                let default_proto = self.intl_protos[kind];
                let over = self.newtarget_proto_override(new_target, cv, default_proto)?;
                let r = self.make_intl(kind as u8, locales, options)?;
                return Ok(self.set_ctor_proto(r, over));
            }
        }
        // Constructing through a Proxy: `construct` trap (or construct the target).
        if let Some((target, handler, revoked)) = self.proxy_parts(ci) {
            if revoked {
                return Err(Thrown("TypeError: Cannot perform 'construct' on a revoked proxy".into()));
            }
            return match self.proxy_trap(handler, "construct")? {
                Some(trap) => {
                    let arr = Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec())));
                    // The trap's 3rd arg is the REAL newTarget, not the proxy itself.
                    let res = self.call_value(trap, handler, &[target, arr, new_target])?;
                    // ProxyConstruct: the trap result must be an Object.
                    if !self.is_object_value(res) {
                        return Err(Thrown(
                            "TypeError: proxy [[Construct]] must return an object".into(),
                        ));
                    }
                    Ok(res)
                }
                // No trap: forward to the target's [[Construct]], preserving newTarget.
                None => self.construct_with_newtarget(target, args, new_target),
            };
        }
        // Symbol/BigInt report IsConstructor true (is_ctor — they serve as a
        // Reflect.construct newTarget) but their [[Construct]] unconditionally
        // throws: `new Symbol()` / `new BigInt()` are TypeErrors.
        if ci != 0 && (ci == self.symbol_ctor || ci == self.bigint_ctor) {
            let n = if ci == self.symbol_ctor { "Symbol" } else { "BigInt" };
            return Err(Thrown(format!("TypeError: {n} is not a constructor")));
        }
        // A core built-in constructor used as a VALUE (`new C()` where C is the
        // Array/Object/Map/… constructor reached via a variable, `.constructor`,
        // or a species lookup — not the compile-lowered `new Array()` literal).
        // Identify it by its own `prototype` (the canonical proto object), so it
        // works however the constructor was obtained.
        let builtin_proto = match self.heap.get(ci) {
            HeapObj::Object(m) if m.is_ctor => {
                m.get("prototype").filter(|p| p.is_heap()).map(|p| p.heap_index())
            }
            _ => None,
        };
        if let Some(p) = builtin_proto {
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            // Promise(executor): IsCallable(executor) (step 2) is checked BEFORE
            // OrdinaryCreateFromConstructor reads newTarget.prototype (step 3) — so a
            // non-callable executor throws even when newTarget.prototype would throw.
            if p == self.promise_proto && self.promise_proto != 0 && !self.is_callable(a0) {
                return Err(Thrown(format!(
                    "TypeError: Promise resolver {} is not a function",
                    self.display(a0)
                )));
            }
            // RegExp(pattern, flags) snapshots the pattern's [[OriginalSource]] /
            // [[OriginalFlags]] in steps 4-6, BEFORE RegExpAlloc (step 7) reads
            // newTarget.prototype. Building it after the shared override below
            // let a `prototype` getter that calls `pattern.compile(…)` change the
            // source the new RegExp is built from
            // (staging/sm/RegExp/constructor-ordering.js: source became "b").
            if p == self.regexp_proto && self.regexp_proto != 0 {
                let pre = self.regexp_pattern_snapshot(a0)?;
                let over = self.newtarget_proto_override(new_target, cv, p)?;
                let f = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let r = self.build_regexp_snapshot(a0, f, pre)?;
                return Ok(self.set_ctor_proto(r, over));
            }
            // OrdinaryCreateFromConstructor: a foreign newTarget (Reflect.construct /
            // cross-realm / derived super) sets the instance's [[Prototype]] to
            // newTarget.prototype rather than the built-in's default `p`.
            let over = self.newtarget_proto_override(new_target, cv, p)?;
            if p == self.arr_proto && self.arr_proto != 0 {
                let mut virtual_len: Option<u32> = None;
                let arr = if args.len() == 1 && a0.is_number() {
                    let n = a0.as_f64();
                    if n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    if n as usize > super::MAX_DENSE_ARRAY_LEN {
                        // Past the eager-materialization cap: a SPARSE array — no
                        // elements, just a virtual length in the side table.
                        virtual_len = Some(n as u32);
                        Vec::new()
                    } else {
                        // `new Array(n)` / `Array(n)` creates n HOLES (absent
                        // elements), not n present `undefined`s.
                        vec![Value::HOLE; n as usize]
                    }
                } else {
                    args.to_vec()
                };
                let r = Value::heap(self.heap.alloc(HeapObj::Array(arr)));
                if let Some(n) = virtual_len {
                    self.array_js_len.insert(r.heap_index(), n);
                }
                return Ok(self.set_ctor_proto(r, over));
            }
            if p == self.obj_proto && self.obj_proto != 0 {
                // Object(value): when NewTarget is present and NOT the Object
                // function itself (a subclass `new O(x)` / an explicit
                // Reflect.construct newTarget), OrdinaryCreateFromConstructor
                // builds a FRESH instance and the value argument is IGNORED
                // (spec 20.1.1.1 step 1). Otherwise ToObject(value), with a
                // nullish value yielding a fresh ordinary object.
                if over.is_some() {
                    let r = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(ObjMap::new()))));
                    return Ok(self.set_ctor_proto(r, over));
                }
                return self.to_object(a0);
            }
            if p == self.num_proto && self.num_proto != 0 {
                // ToNumber(value) — observable (a user valueOf/toString runs) and
                // abrupt; `to_number` alone returns NaN for a plain object.
                let n = if args.is_empty() { 0.0 } else { self.to_number_coerce(a0)? };
                let r = Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 1, value: Value::num(n) }));
                return Ok(self.set_ctor_proto(r, over));
            }
            if p == self.bool_proto && self.bool_proto != 0 {
                let b = !args.is_empty() && self.truthy(a0);
                let r = Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 2, value: Value::bool(b) }));
                return Ok(self.set_ctor_proto(r, over));
            }
            if p == self.str_proto && self.str_proto != 0 {
                // Identity for a string argument: `new String('\uD800')` boxes
                // the exact lone-surrogate string.
                let sv = if args.is_empty() {
                    self.alloc_str(String::new())
                } else {
                    self.to_str_value(a0)?
                };
                let r = Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 0, value: sv }));
                return Ok(self.set_ctor_proto(r, over));
            }
            if p == self.map_proto && self.map_proto != 0 {
                // Per spec the entries are added via the `set` adder resolved off the
                // new map — so an overridden `set` (or a subclass's) is honoured.
                // AddEntriesFromIterable (shared with WeakMap) steps the iterator
                // LAZILY, requires every entry to be an Object, and closes the
                // iterator on any abrupt completion. The eager `iterate_to_vec`
                // drain this replaced did none of the three: a non-object entry was
                // silently indexed instead of throwing TypeError, and a throwing
                // `[0]`/`[1]` getter or adder left the iterator open
                // (staging/sm/Map/constructor-iterator-close.js).
                let map_v = Value::heap(self.heap.alloc(HeapObj::Map { keys: Vec::new(), vals: Vec::new() }));
                if !a0.is_nullish() {
                    self.add_entries_via_adder(map_v, a0, true)?;
                }
                return Ok(self.set_ctor_proto(map_v, over));
            }
            if p == self.set_proto && self.set_proto != 0 {
                let set_v = Value::heap(self.heap.alloc(HeapObj::Set(Vec::new())));
                if !a0.is_nullish() {
                    self.add_entries_via_adder(set_v, a0, false)?;
                }
                return Ok(self.set_ctor_proto(set_v, over));
            }
            if p == self.date_proto && self.date_proto != 0 {
                let ms = self.date_new_ms(args)?;
                let r = Value::heap(self.heap.alloc(HeapObj::Date(ms)));
                return Ok(self.set_ctor_proto(r, over));
            }
            if p == self.promise_proto && self.promise_proto != 0 {
                if !self.is_callable(a0) {
                    return Err(Thrown(format!(
                        "TypeError: Promise resolver {} is not a function",
                        self.display(a0)
                    )));
                }
                let prom = self.alloc_promise();
                let pair = self.new_resolver_pair();
                let res = Value::heap(
                    self.heap.alloc(HeapObj::BoundResolver { promise: prom, is_reject: false, pair }),
                );
                let rej = Value::heap(
                    self.heap.alloc(HeapObj::BoundResolver { promise: prom, is_reject: true, pair }),
                );
                if self.call_value(a0, Value::UNDEFINED, &[res, rej]).is_err() {
                    let reason = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                    // Step 10.a Call([[Reject]]): no-op once the pair fired
                    // (resolve-then-throw keeps the resolution).
                    if self.resolver_pair_fire(pair) {
                        self.reject(prom, reason);
                    }
                }
                return Ok(self.set_ctor_proto(Value::heap(prom), over));
            }
        }
        // A user function with no [[Construct]] (generator, async, arrow, or a
        // concise method) — `new` on it is a TypeError. Gated to Func/Closure so
        // built-in Native ctors, classes, and bound functions are untouched.
        if matches!(self.heap.get(cv.heap_index()), HeapObj::Func(_) | HeapObj::Closure { .. })
            && !self.is_constructor(cv)
        {
            return Err(Thrown("TypeError: function is not a constructor".into()));
        }
        // Constructor FUNCTION (`new F()`, the pre-class OOP idiom): make an object
        // whose [[Prototype]] is `F.prototype` (so its methods + `constructor`
        // resolve), run `F` with `this` = that object, and use F's return value if
        // it returns an object (else the new object).
        if matches!(
            self.heap.get(cv.heap_index()),
            HeapObj::Func(_) | HeapObj::Closure { .. }
        ) {
            // The instance's [[Prototype]] is newTarget.prototype (OrdinaryCreate
            // FromConstructor); for the common `new F()` case this is F.prototype.
            let default = self.prototype_of(cv).unwrap_or(Value::UNDEFINED);
            let mut proto = self.newtarget_proto(new_target, cv, default)?;
            // GetPrototypeFromConstructor: a non-object prototype falls back
            // to %Object.prototype% — from the CONSTRUCTOR's realm when it is
            // realm-tagged (a real function made by new other.Function()).
            if !self.is_object_value(proto) {
                if let Some(rp) = self.realm_proto_fallback(cv, self.obj_proto) {
                    proto = Value::heap(rp);
                }
            }
            // Pre-size the instance from what THIS constructor's previous
            // instances ended up holding (see `ctor_field_hint`). Without it the
            // map starts empty and the first `this.x = v` allocates all three
            // parallel vectors, the second regrows them, and so on — a two-field
            // constructor cost 71ns/field against 28ns/field for the same fields
            // written as a literal, which gets its size from `NewObject { hint }`.
            let fid = match self.heap.get(cv.heap_index()) {
                HeapObj::Func(f) => Some(*f),
                HeapObj::Closure { func, .. } => Some(*func),
                _ => None,
            };
            let hint = fid
                .and_then(|f| self.ctor_field_hint.get(f as usize).copied())
                .unwrap_or(0);
            let obj = Value::heap(
                self.heap.alloc(HeapObj::Object(Box::new(ObjMap::with_capacity(hint as usize)))),
            );
            if proto.is_heap() {
                self.proto_of.insert(obj.heap_index(), proto);
            }
            // `new.target` for the constructor body (the next frame entered).
            self.pending_new_target = new_target;
            let ret = self.call_value(cv, obj, args)?;
            // Learn this constructor's instance size for the next `new`. Taken as
            // a high-water mark so a constructor with a conditional field settles
            // on the larger shape rather than oscillating; capacity is cheap and
            // regrowth is what we are paying to avoid.
            if let Some(f) = fid {
                let n = match self.heap.get(obj.heap_index()) {
                    HeapObj::Object(m) => m.keys.len().min(u16::MAX as usize) as u16,
                    _ => 0,
                };
                // CAPPED. A high-water mark is unbounded by construction: one
                // unusual instance that happens to take 65,535 properties would
                // teach the mark permanently, and every later instance of that
                // constructor would then reserve ~3 MiB across the three vectors
                // — for the whole process lifetime, which is especially bad in a
                // persistent embedded VM (`embed::ScriptState`). Pre-sizing only
                // ever saves regrowth, and regrowth past a few dozen properties
                // is amortised anyway, so the cap costs nothing measurable.
                let n = n.min(CTOR_FIELD_HINT_MAX);
                if n > 0 {
                    if self.ctor_field_hint.len() <= f as usize {
                        self.ctor_field_hint.resize(f as usize + 1, 0);
                    }
                    if n > self.ctor_field_hint[f as usize] {
                        self.ctor_field_hint[f as usize] = n;
                    }
                }
            }
            // A constructor that returns ANY object (TypedArray/Map/Date/… too, not
            // just a plain object/array) replaces the new instance with it.
            if self.is_object_value(ret) {
                return Ok(ret);
            }
            return Ok(obj);
        }
        // `new (boundFn)(...)`: [[Construct]] forwards to the bound target with the
        // bound arguments prepended (the bound `this` is ignored for construction).
        let bound_parts = match self.heap.get(cv.heap_index()) {
            HeapObj::Bound { target, args: bargs, .. } => Some((*target, bargs.clone())),
            _ => None,
        };
        if let Some((target, bargs)) = bound_parts {
            let combined: Vec<Value> = bargs.into_iter().chain(args.iter().copied()).collect();
            // Bound [[Construct]]: substitute the target for newTarget only when
            // newTarget is the bound function itself; otherwise keep the caller's
            // newTarget so OrdinaryCreateFromConstructor uses its prototype.
            let nt = if new_target == cv { target } else { new_target };
            return self.construct_with_newtarget(target, &combined, nt);
        }
        let (ctor, ctor_ups, has_explicit, parent, extends_null) = match self.heap.get(cv.heap_index()) {
            HeapObj::Class(c) => {
                (c.ctor, c.ctor_upvalues.clone(), c.has_explicit_ctor, c.parent, c.extends_null)
            }
            _ => return Err(Thrown("TypeError: value is not a constructor".into())),
        };
        // The instance links to its class for method lookup + instanceof; its own
        // keys hold only the fields (so enumeration / JSON stay method-free).
        let mut map = ObjMap::new();
        map.class = Some(cv.heap_index());
        let obj = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(map))));
        // OrdinaryCreateFromConstructor: a `Reflect.construct(Class, args, NT)` (or
        // any newTarget other than the class) gives the instance NT.prototype as its
        // [[Prototype]], overriding the class-derived default (proto_of is consulted
        // first by object_get_prototype_of / instanceof). `new Class()` is unchanged.
        if new_target.is_heap() && new_target != cv {
            let p = self.get_prop(new_target, "prototype")?;
            if self.is_object_value(p) {
                self.proto_of.insert(obj.heap_index(), p);
            } else if let Some(rp) = self.realm_proto_fallback(new_target, self.obj_proto) {
                // GetPrototypeFromConstructor: a createRealm-child newTarget
                // with a non-object `prototype` falls back to ITS realm's
                // %Object.prototype% (None for a main-realm newTarget).
                self.proto_of.insert(obj.heap_index(), Value::heap(rp));
            }
        }
        if has_explicit {
            // The explicit constructor runs its own `super(...)`; a ctor that
            // returns an object/array replaces the instance.
            if let Some(fid) = ctor {
                // A BASE class's InitializeInstanceElements runs at
                // [[Construct]] entry (a DERIVED class's at super() completion) —
                // brand first, then the field initializers, and BOTH before the
                // ctor's own FunctionDeclarationInstantiation, so a parameter
                // default sees the fields already installed.
                if parent.is_none() && !extends_null {
                    self.brand_instance(obj, cv);
                    self.run_field_thunk(obj, cv)?;
                }
                // The ctor function value is materialized AFTER the field thunk,
                // not before: the thunk runs user code (field initializers), so a
                // collection can land between the two — and a Value held only in
                // a Rust local is not a GC root. Built first, `class A { x = {};
                // constructor(){} }` under ZIPP_GC_STRESS=1 swept this very
                // function and called whatever object reused its slot
                // ("[object Object] is not a function").
                let f = self.ctor_value(fid, &ctor_ups);
                // The ctor (incl. field initializers) runs in the class body's private
                // scope: give its function value the class's lexical brand chain so
                // `this.#x` + classes defined in field initializers resolve.
                if let Some(brands) = self.method_brand.get(&cv.heap_index()).cloned() {
                    if f.is_heap() {
                        self.method_brand.insert(f.heap_index(), brands);
                    }
                }
                // A DERIVED ctor's `this` is in TDZ until its `super(...)`
                // completes (the SuperCtor ops remove the mark).
                if parent.is_some() || extends_null {
                    self.this_tdz.insert(obj.heap_index());
                }
                // `new.target` for the class constructor body (the next frame entered).
                self.pending_new_target = new_target;
                let result = self.call_value(f, obj, args);
                // Capture + clear the super() signal BEFORE propagating any throw,
                // so a constructor that threw never leaves a stale entry (the heap
                // index could later be reused by another instance).
                let super_called = self.super_called.remove(&obj.heap_index());
                self.this_tdz.remove(&obj.heap_index());
                let super_this = self.super_this.remove(&obj.heap_index());
                let ret = result?;
                // Any object return replaces the new instance.
                if self.is_object_value(ret) {
                    // A return-override result receives this class's private brand.
                    self.brand_instance(ret, cv);
                    return Ok(ret);
                }
                if parent.is_some() || extends_null {
                    // A DERIVED class constructor may only return an object or
                    // undefined — any other value throws (a base class silently
                    // ignores a primitive return and yields `this`).
                    if ret != Value::UNDEFINED {
                        return Err(Thrown(
                            "TypeError: Derived constructors may only return object or undefined".into(),
                        ));
                    }
                    // …and `this` must have been initialised by `super(...)`.
                    if !super_called {
                        return Err(Thrown(
                            "ReferenceError: Must call super constructor in derived class before returning from derived constructor".into(),
                        ));
                    }
                    // `super()` produced a return-override instance and the ctor
                    // returned undefined: that instance IS the result.
                    if let Some(st) = super_this {
                        self.brand_instance(st, cv);
                        return Ok(st);
                    }
                }
            }
        } else {
            // No own constructor: run the parent's ctor (implicit `super(...args)`),
            // threading its PRODUCED `this` (a base ctor's object-return becomes the
            // instance), then this class's field initializers on it.
            let mut inst = obj;
            // `class C extends null {}` with no own ctor: the implicit
            // super(...args) calls a null parent — TypeError per spec.
            if extends_null {
                return Err(Thrown(
                    "TypeError: Super constructor null of anonymous class is not a constructor".into(),
                ));
            }
            if let Some(pidx) = parent {
                // A default derived constructor is
                // `constructor(...args){ super(...args) }`, whose `super` resolves
                // through GetSuperConstructor() — the class object's LIVE
                // [[GetPrototypeOf]]. Using the `parent` recorded at class-
                // definition time made `Object.setPrototypeOf(D, Other)` a no-op
                // for a ctor-less class, while the explicit-ctor form (which goes
                // through `super_ctor_func`) already retargeted
                // (staging/sm/class/superCallProperBase.js).
                let live = self.object_get_prototype_of(cv);
                let sup = if live.is_heap() { live } else { Value::heap(pidx) };
                let r = self.run_class_ctor(sup, inst, args, new_target);
                // An explicit DERIVED parent in the chain may have left a this-TDZ
                // mark (it threw pre-super) or a banked return-override (it
                // object-returned past it) on the threaded instance — clear both.
                self.this_tdz.remove(&obj.heap_index());
                self.super_this.remove(&obj.heap_index());
                inst = r?;
            }
            // PrivateBrandAdd + the double-init / non-extensible checks run
            // BEFORE the field initializers (spec InitializeInstanceElements
            // order — an initializer may call this class's own privates).
            let r = self.private_init_checked(inst, cv, inst != obj);
            // Clear any super() mark a nested parent ctor left on this instance
            // (even when the checked init throws).
            self.super_called.remove(&inst.heap_index());
            r?;
            if let Some(fid) = ctor {
                let f = self.ctor_value(fid, &ctor_ups);
                if let Some(brands) = self.method_brand.get(&cv.heap_index()).cloned() {
                    if f.is_heap() {
                        self.method_brand.insert(f.heap_index(), brands);
                    }
                }
                self.call_value(f, inst, &[])?;
            }
            return Ok(inst);
        }
        Ok(obj)
    }

    /// `super(...)` to a built-in EXOTIC parent (`class X extends Set/Map/…`): re-brand
    /// the plain-Object instance `obj` with the builtin's internal representation so its
    /// prototype methods operate and `instanceof` the builtin holds. The instance's own
    /// (subclass) prototype is captured first and re-recorded in `proto_of` (so subclass
    /// methods/fields still resolve — exotic get_member walks proto_of when present), and
    /// later field initializers write named props into the exotic `arr_props` side table.
    /// Returns `true` when `cval` was a recognised builtin exotic ctor (and obj branded).
    pub(crate) fn brand_builtin_subclass(
        &mut self,
        cval: Value,
        obj: Value,
        args: &[Value],
    ) -> Result<bool, Thrown> {
        let oidx = obj.heap_index();
        // A `$262.createRealm()` FACADE constructor mirrors a main-realm built-in,
        // so `class B extends g.ArrayBuffer` must brand exactly like `class B
        // extends ArrayBuffer`. Every test below compares against a MAIN ctor /
        // MAIN `.prototype` heap index, which a facade never matches — the
        // instance stayed a plain Object and its `byteLength` getter then threw
        // "incompatible receiver" (staging/sm/ArrayBuffer/slice-species.js line
        // 131). Only the branding decision is re-homed: the instance keeps the
        // subclass prototype captured below, so the realm identity survives.
        let cval = match cval
            .is_heap()
            .then(|| self.realm_ctor_main.get(&cval.heap_index()).copied())
            .flatten()
        {
            Some(main) => Value::heap(main),
            None => cval,
        };
        // `class S extends Symbol/BigInt`: super() must throw — neither is a
        // constructor ([[Construct]] is absent). Checked FIRST: these ctors have
        // no .prototype mapping, so the pidx lookup below would bail before it.
        if cval.is_heap() {
            let ci = cval.heap_index();
            if ci != 0 && ci == self.symbol_ctor {
                return Err(Thrown("TypeError: Symbol is not a constructor".into()));
            }
            if ci != 0 && ci == self.bigint_ctor {
                return Err(Thrown("TypeError: BigInt is not a constructor".into()));
            }
        }
        // Only re-brand a class instance that is still a plain Object (not already a
        // builtin variant from a deeper super() in the chain).
        if !matches!(self.heap.get(oidx), HeapObj::Object(_)) {
            return Ok(false);
        }
        let pidx = match self.prototype_of(cval) {
            Some(p) if p.is_heap() => p.heap_index(),
            _ => return Ok(false),
        };
        // Capture the subclass prototype before re-branding loses the map.class link.
        let sub_proto = self.object_get_prototype_of(obj);
        // `class T extends Uint8Array` (or any TypedArray kind): build a real typed
        // array through the builtin ctor (handling every arg form — length, array,
        // (buffer, byteOffset, length) on a fixed/resizable buffer) and move it into
        // the instance. The TA references a freshly-constructed ArrayBuffer, which is
        // correct to share. Detected by the parent being a TypedArray constructor.
        if cval.is_heap() && self.ta_ctors.iter().any(|&c| c != 0 && c == cval.heap_index()) {
            let tv = self.construct(cval, args)?;
            let tvi = tv.heap_index();
            let cloned = self.heap.get(tvi).clone();
            *self.heap.get_mut(oidx) = cloned;
            // Carry over the length-tracking flag (a `new T(rab[, offset])` view with
            // no explicit length follows the resizable buffer): it lives in a side
            // set keyed by heap index, which the clone above does NOT move.
            if self.ta_tracking.contains(&tvi) {
                self.ta_tracking.insert(oidx);
            }
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        // `class B extends ArrayBuffer`: materialize a REAL ArrayBuffer into the
        // instance slot (so byteLength/slice's brand checks pass); the resizable
        // max lives in the ab_max side table, keyed by the instance's heap index.
        if cval.is_heap() && cval.heap_index() == self.arraybuffer_ctor && self.arraybuffer_ctor != 0
        {
            let (n, max) = self.validate_array_buffer_args(args)?;
            if n > super::typedarray::MAX_ARRAY_BUFFER_LEN as usize {
                return Err(Thrown("RangeError: ArrayBuffer length exceeds the maximum".into()));
            }
            *self.heap.get_mut(oidx) =
                HeapObj::ArrayBuffer { data: vec![0u8; n].into(), detached: false };
            if let Some(m) = max {
                self.ab_max.insert(oidx, m);
            }
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        // `class D extends DataView`: build a real DataView through the builtin
        // ctor and move it into the instance (the buffer heap index is shared
        // correctly by the clone; the dv_tracking side-set flag is carried like
        // the TypedArray arm carries ta_tracking).
        if cval.is_heap() && cval.heap_index() == self.dataview_ctor && self.dataview_ctor != 0 {
            let tv = self.build_data_view(args)?;
            let tvi = tv.heap_index();
            let cloned = self.heap.get(tvi).clone();
            *self.heap.get_mut(oidx) = cloned;
            if self.dv_tracking.contains(&tvi) {
                self.dv_tracking.insert(oidx);
            }
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        // `class M extends Intl.<service>`: the service instance is an exotic
        // `HeapObj::Intl`, and every prototype method brand-checks for it, so
        // build the real one and move it into the subclass instance (`new
        // MyNF("en").format(1)` otherwise threw "incompatible receiver" — the
        // instance was still a plain Object). `Reflect.construct` already worked;
        // only the derived-`super()` path lands here.
        if cval.is_heap() && self.intl_ctors[0] != 0 {
            if let Some(kind) =
                self.intl_ctors.iter().position(|&c| c != 0 && c == cval.heap_index())
            {
                let locales = args.first().copied().unwrap_or(Value::UNDEFINED);
                let options = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let tv = self.make_intl(kind as u8, locales, options)?;
                let cloned = self.heap.get(tv.heap_index()).clone();
                *self.heap.get_mut(oidx) = cloned;
                if sub_proto.is_heap() {
                    self.proto_of.insert(oidx, sub_proto);
                }
                return Ok(true);
            }
        }
        // `class B extends Boolean/Number/String/Date/RegExp`: construct() already
        // implements each builtin's argument semantics (truthy boxing, ToNumber,
        // ToString, the Date overloads, pattern/flags); clone the built heap
        // object into the instance — Boxed/Date/RegExp carry no heap-index-keyed
        // side state that matters here.
        if pidx != 0
            && [self.bool_proto, self.num_proto, self.str_proto, self.date_proto, self.regexp_proto]
                .contains(&pidx)
        {
            let tv = self.construct(cval, args)?;
            let cloned = self.heap.get(tv.heap_index()).clone();
            *self.heap.get_mut(oidx) = cloned;
            // Carry any named own props the build recorded (e.g. a RegExp's
            // side-table entries) from the temp object to the instance.
            if let Some(m) = self.arr_props.remove(&tv.heap_index()) {
                self.arr_props.insert(oidx, m);
            }
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        // `class F extends Function/GeneratorFunction/Async(Generator)Function`:
        // build the dynamic function through the builtin ctor (construct routes
        // each ctor to its build_function_kind) and move it into the instance,
        // carrying the function-keyed side tables so name/length/prototype and
        // callability follow the instance's heap index.
        if cval.is_heap()
            && [self.function_ctor, self.gen_fn_ctor, self.async_fn_ctor, self.asyncgen_fn_ctor]
                .contains(&cval.heap_index())
            && cval.heap_index() != 0
        {
            let tv = self.construct(cval, args)?;
            let tvi = tv.heap_index();
            let cloned = self.heap.get(tvi).clone();
            *self.heap.get_mut(oidx) = cloned;
            if let Some(m) = self.fn_props.remove(&tvi) {
                self.fn_props.insert(oidx, m);
            }
            if let Some(p) = self.prototypes.remove(&tvi) {
                self.prototypes.insert(oidx, p);
            }
            if let Some(v) = self.fn_proto_override.remove(&tvi) {
                self.fn_proto_override.insert(oidx, v);
            }
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        // `class D extends DisposableStack/AsyncDisposableStack`: the brand is
        // purely the dispose_stacks (+ async_stacks) side-table entry — the
        // instance stays a plain Object and the subclass prototype is already
        // in place, so just install the empty, not-yet-disposed stack.
        if cval.is_heap()
            && cval.heap_index() != 0
            && (cval.heap_index() == self.disposablestack_ctor
                || cval.heap_index() == self.asyncdisposablestack_ctor)
        {
            self.dispose_stacks.insert(oidx, (Vec::new(), false));
            if cval.heap_index() == self.asyncdisposablestack_ctor {
                self.async_stacks.insert(oidx);
            }
            return Ok(true);
        }
        // `class W extends WeakMap/WeakSet`: brand first (so the adder operates
        // on the real variant), then add iterable entries via the instance's
        // adder (honouring a subclass override) — modeled on the Map/Set arms.
        if pidx == self.weakmap_proto && self.weakmap_proto != 0 {
            *self.heap.get_mut(oidx) = HeapObj::WeakMap { keys: Vec::new(), vals: Vec::new() };
            // Re-branded in place: no stale collection index may key this slot.
            self.coll_index_invalidate(oidx);
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !a0.is_nullish() {
                self.add_entries_via_adder(obj, a0, true)?;
            }
            return Ok(true);
        }
        if pidx == self.weakset_proto && self.weakset_proto != 0 {
            *self.heap.get_mut(oidx) = HeapObj::WeakSet(Vec::new());
            // Re-branded in place: no stale collection index may key this slot.
            self.coll_index_invalidate(oidx);
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !a0.is_nullish() {
                self.add_entries_via_adder(obj, a0, false)?;
            }
            return Ok(true);
        }
        if pidx == self.set_proto && self.set_proto != 0 {
            // Brand to an EMPTY Set first, then add through the adder resolved off
            // the instance. Building the element list directly (what this used to
            // do) skipped `add` entirely, so `class S extends Set { add() { … } }`
            // never saw its own override run — and a throwing override could not
            // close the iterator either
            // (staging/sm/Map/constructor-iterator-close.js). The builtin `add`
            // still does the SameValueZero dedup.
            *self.heap.get_mut(oidx) = HeapObj::Set(Vec::new());
            // The instance slot is re-branded in place: make sure no stale
            // collection index can be keyed by it.
            self.coll_index_invalidate(oidx);
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !a0.is_nullish() {
                self.add_entries_via_adder(obj, a0, false)?;
            }
            return Ok(true);
        }
        if pidx == self.arr_proto && self.arr_proto != 0 {
            // `class A extends Array`: build a fresh array via the existing ctor dispatch
            // (the p==arr_proto path, incl. the single-number `new Array(n)` length form)
            // and clone it into the instance. Array is exotic (length) but a plain
            // Vec<Value> with no back-references, so the clone is safe.
            let tv = self.construct(cval, args)?;
            let cloned = self.heap.get(tv.heap_index()).clone();
            *self.heap.get_mut(oidx) = cloned;
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        if pidx == self.map_proto && self.map_proto != 0 {
            // Brand first so the `set` adder operates on a real Map, then add entries
            // via the adder resolved off the instance (honouring a subclass override).
            *self.heap.get_mut(oidx) = HeapObj::Map { keys: Vec::new(), vals: Vec::new() };
            // Re-branded in place: no stale collection index may key this slot.
            self.coll_index_invalidate(oidx);
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !a0.is_nullish() {
                self.add_entries_via_adder(obj, a0, true)?;
            }
            return Ok(true);
        }
        if pidx == self.promise_proto && self.promise_proto != 0 {
            // `class P extends Promise`: brand the instance AS the promise (its heap
            // index IS the promise), bind resolve/reject to it, and run the executor —
            // so NewPromiseCapability(P) (construct -> super(executor)) yields a branded P.
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !self.is_callable(a0) {
                return Err(Thrown(format!(
                    "TypeError: Promise resolver {} is not a function",
                    self.display(a0)
                )));
            }
            *self.heap.get_mut(oidx) = HeapObj::Promise {
                state: PromiseState::Pending,
                result: Value::UNDEFINED,
                fulfill: Vec::new(),
                reject: Vec::new(),
                handled: false,
            };
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            let pair = self.new_resolver_pair();
            let res = Value::heap(
                self.heap.alloc(HeapObj::BoundResolver { promise: oidx, is_reject: false, pair }),
            );
            let rej = Value::heap(
                self.heap.alloc(HeapObj::BoundResolver { promise: oidx, is_reject: true, pair }),
            );
            if self.call_value(a0, Value::UNDEFINED, &[res, rej]).is_err() {
                let reason = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                // Step 10.a Call([[Reject]]): no-op once the pair fired.
                if self.resolver_pair_fire(pair) {
                    self.reject(oidx, reason);
                }
            }
            return Ok(true);
        }
        // Temporal value types (immutable kind+fields, no back-references): the parent
        // ctor is one of the 8 Temporal constructors. Build a fresh instance through the
        // existing ctor dispatch, then clone its representation into the instance —
        // `construct` reuses all the per-kind arg parsing/validation. A ZonedDateTime
        // also carries its time-zone in the `zdt_tz` side table.
        let ci = cval.heap_index();
        let is_temporal_ctor = ci != 0
            && (ci == self.duration_ctor
                || ci == self.plaindate_ctor
                || ci == self.plaintime_ctor
                || ci == self.plaindatetime_ctor
                || ci == self.instant_ctor
                || ci == self.plainyearmonth_ctor
                || ci == self.plainmonthday_ctor
                || ci == self.zoneddatetime_ctor);
        if is_temporal_ctor {
            let tv = self.construct(cval, args)?;
            let tvi = tv.heap_index();
            let cloned = self.heap.get(tvi).clone();
            *self.heap.get_mut(oidx) = cloned;
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            if let Some(tz) = self.zdt_tz.get(&tvi).copied() {
                self.zdt_tz.insert(oidx, tz);
            }
            return Ok(true);
        }
        Ok(false)
    }

}
