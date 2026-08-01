#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap, PropAttr,
    PromiseState, ReactionPair, Reactions,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// SetterThatIgnoresPrototypeProperties(this, home, key, v): non-object
    /// receiver → TypeError; receiver IS the home object → TypeError; no own
    /// `key` → CreateDataPropertyOrThrow (bypasses the proto chain — an
    /// ordinary Set would re-enter this same inherited setter, an infinite
    /// native recursion); else ordinary Set with Throw = true.
    pub(crate) fn setter_ignoring_proto_props(
        &mut self,
        this: Value,
        home: u32,
        key: &str,
        v: Value,
    ) -> Result<(), Thrown> {
        if !self.is_object_value(this) {
            return Err(Thrown(format!(
                "TypeError: setter for '{key}' called on a non-object receiver"
            )));
        }
        if this.is_heap() && this.heap_index() == home {
            return Err(Thrown(format!(
                "TypeError: Cannot assign to read only property '{key}'"
            )));
        }
        // [[GetOwnProperty]]: trap-aware for a Proxy receiver (the gOPD trap
        // is observable), else the ordinary descriptor lookup.
        let own = match self.proxy_gopd(this, key)? {
            Some(d) => d,
            None => self.object_get_own_property_descriptor(this, key),
        };
        if own == Value::UNDEFINED {
            let attr = PropAttr {
                writable: true,
                enumerable: true,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            let mut dm = ObjMap::new();
            dm.define("value", v, attr);
            dm.define("writable", Value::bool(true), attr);
            dm.define("enumerable", Value::bool(true), attr);
            dm.define("configurable", Value::bool(true), attr);
            let desc = self.heap.alloc(HeapObj::Object(Box::new(dm)));
            if self.obj_proto != 0 {
                self.proto_of.insert(desc, Value::heap(self.obj_proto));
            }
            self.object_define_property(this, key, Value::heap(desc))?;
        } else {
            self.set_prop(this, key, v, true)?;
        }
        Ok(())
    }

    /// GetIteratorDirect receiver check: `this` must be an object (not a string/
    /// symbol/bigint primitive).
    fn iter_receiver_ok(&self, this: Value) -> bool {
        this.is_heap()
            && !matches!(
                self.heap.get(this.heap_index()),
                HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
            )
    }

    /// Walk a prototype chain (`proto_of` links) from `start`, returning `key`'s
    /// value and invoking an accessor getter with `receiver`. Used for iterators
    /// (which inherit the helpers from the intermediate %Iterator.prototype%).
    pub(crate) fn proto_chain_get(
        &mut self,
        start: u32,
        key: &str,
        receiver: Value,
    ) -> Result<Value, Thrown> {
        let mut cur = start;
        for _ in 0..64 {
            if cur == 0 {
                break;
            }
            // A PROXY link in the chain carries a `get` trap that `own_member`
            // (ordinary storage only) cannot see — delegate the rest of the walk
            // to the ordinary member path so the trap runs with `receiver`.
            if matches!(self.heap.get(cur), HeapObj::Proxy { .. }) {
                return self.get_member(Value::heap(cur), key, receiver);
            }
            if let Some((attr, raw)) = self.own_member(cur, key) {
                if attr.accessor {
                    return if raw == Value::UNDEFINED {
                        Ok(Value::UNDEFINED)
                    } else {
                        self.call_value(raw, receiver, &[])
                    };
                }
                return Ok(raw);
            }
            cur = match self.proto_of.get(&cur).copied() {
                Some(p) if p.is_heap() => p.heap_index(),
                _ => 0,
            };
        }
        Ok(Value::UNDEFINED)
    }

    /// Pull one value from an iterator (calling its `.next()`), `Ok(None)` at end.
    pub(crate) fn iterator_step(&mut self, iter: Value) -> Result<Option<Value>, Thrown> {
        if iter.is_heap() && matches!(self.heap.get(iter.heap_index()), HeapObj::Generator { .. }) {
            let res = self
                .generator_method(iter.heap_index(), "next", &[])?
                .unwrap_or(Value::UNDEFINED);
            let done = self.get_prop(res, "done")?;
            if self.truthy(done) {
                return Ok(None);
            }
            let val = self.get_prop(res, "value")?;
            return Ok(Some(val));
        }
        let next = self.get_prop(iter, "next")?;
        self.iterator_step_with(iter, next)
    }

    /// IteratorStep using a PRE-FETCHED `next` method (GetIteratorDirect cached it), so
    /// `iter.next` is not re-read each step. `Ok(None)` at end.
    pub(crate) fn iterator_step_with(
        &mut self,
        iter: Value,
        next: Value,
    ) -> Result<Option<Value>, Thrown> {
        let res = self.call_value(next, iter, &[])?;
        if !self.is_object_value(res) {
            return Err(Thrown("TypeError: iterator.next() returned a non-object".into()));
        }
        let done = self.get_prop(res, "done")?;
        if self.truthy(done) {
            return Ok(None);
        }
        let val = self.get_prop(res, "value")?;
        Ok(Some(val))
    }

    /// One step of SYNC `yield*` delegation (spec 14.4.14 step 5). Drives `iter`
    /// per the outer generator's resume `mode` (0 = next, 1 = throw, 2 = return)
    /// with argument `sent`, applying the missing-method rules. Returns
    /// `(value, done, ret)`: `done` ⇒ the `yield*` expression completes with
    /// `value`; `ret` ⇒ the generator must RETURN `value`; both false ⇒ `value`
    /// is the inner iterator's RAW result object, to be yielded VERBATIM (spec
    /// GeneratorYield(innerResult) — IteratorValue is read ONLY once done, so a
    /// `value` getter is untouched while delegation is ongoing and the inner
    /// result's identity/shape reaches the outer caller unchanged).
    pub(crate) fn iter_delegate_step(
        &mut self,
        iter: Value,
        mode: i32,
        sent: Value,
    ) -> Result<(Value, bool, bool), Thrown> {
        // IteratorComplete: validate `result` is an object, then read only `done`.
        let complete = |vm: &mut Self, result: Value| -> Result<bool, Thrown> {
            if !vm.is_object_value(result) {
                return Err(Thrown(
                    "TypeError: iterator result is not an object".into(),
                ));
            }
            let d = vm.get_prop(result, "done")?;
            Ok(vm.truthy(d))
        };
        match mode {
            // next: result = iter.next(sent) → done completes the yield* expression
            // with IteratorValue(result); not done yields the result verbatim.
            0 => {
                let next = self.get_prop(iter, "next")?;
                let result = self.call_value(next, iter, &[sent])?;
                let done = complete(self, result)?;
                if done {
                    let value = self.get_prop(result, "value")?;
                    return Ok((value, true, false));
                }
                Ok((result, false, false))
            }
            // throw: forward to iter.throw(sent); a missing `throw` closes the
            // iterator and is a TypeError (the inner can't handle the throw).
            1 => {
                let throw_m = self.get_prop(iter, "throw")?;
                if throw_m.is_nullish() {
                    let _ = self.iterator_close(iter);
                    return Err(Thrown(
                        "TypeError: The iterator does not provide a 'throw' method".into(),
                    ));
                }
                if !self.is_callable(throw_m) {
                    return Err(Thrown("TypeError: iterator 'throw' is not a function".into()));
                }
                let result = self.call_value(throw_m, iter, &[sent])?;
                let done = complete(self, result)?;
                if done {
                    let value = self.get_prop(result, "value")?;
                    return Ok((value, true, false));
                }
                Ok((result, false, false))
            }
            // return: forward to iter.return(sent); a missing `return` ends the
            // generator with `sent`; otherwise a done result ends it with the value.
            _ => {
                let ret_m = self.get_prop(iter, "return")?;
                if ret_m.is_nullish() {
                    return Ok((sent, false, true));
                }
                if !self.is_callable(ret_m) {
                    return Err(Thrown("TypeError: iterator 'return' is not a function".into()));
                }
                let result = self.call_value(ret_m, iter, &[sent])?;
                let done = complete(self, result)?;
                if done {
                    let value = self.get_prop(result, "value")?;
                    return Ok((value, false, true));
                }
                Ok((result, false, false))
            }
        }
    }

    /// GetIteratorFlattenable(obj, primitiveHandling): obtain a steppable iterator
    /// from an iterable (via @@iterator) or an object that is itself an iterator
    /// (no @@iterator → the object IS the iterator record). `reject_primitives`
    /// selects the spec mode: reject-primitives (flatMap/zip elements) throws for
    /// ANY non-object; iterate-string-primitives (Iterator.from / zip padding)
    /// additionally allows a String (which is then iterated).
    pub(crate) fn get_iterator_flattenable(
        &mut self,
        v: Value,
        reject_primitives: bool,
    ) -> Result<Value, Thrown> {
        if !self.is_object_value(v) {
            let is_str = v.is_heap()
                && matches!(self.heap.get(v.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. });
            if reject_primitives || !is_str {
                return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))));
            }
            // iterate-string-primitives: a String is iterable — fall through.
        }
        // GetMethod(@@iterator): undefined/null ⇒ the object IS the iterator record;
        // present-but-non-callable ⇒ TypeError; a returned iterator must be an Object.
        let m = self.get_prop(v, "@@iterator")?;
        if m.is_nullish() {
            return Ok(v);
        }
        if !self.is_callable(m) {
            return Err(Thrown("TypeError: [Symbol.iterator] is not a function".into()));
        }
        let it = self.call_value(m, v, &[])?;
        if !self.is_object_value(it) {
            return Err(Thrown("TypeError: [Symbol.iterator]() returned a non-object".into()));
        }
        Ok(it)
    }

    /// GetIteratorFlattenable + GetIteratorDirect's observable `Get(iter, "next")`:
    /// returns a 2-element heap Array `[iterator, nextMethod]` — the zip "Iterator
    /// Record". The Get is observable (proxy logs see it) and the cached method
    /// drives every later step (the spec's [[NextMethod]] is read ONCE).
    fn get_iterator_record_flattenable(
        &mut self,
        v: Value,
        reject_primitives: bool,
    ) -> Result<Value, Thrown> {
        let it = self.get_iterator_flattenable(v, reject_primitives)?;
        let next = self.get_prop(it, "next")?;
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(vec![it, next]))))
    }

    /// The iterator of a zip slot: a `[iterator, next]` record Array yields its
    /// first element; a bare iterator (or the NULL closed marker) passes through.
    fn iz_rec_iter(&self, rec: Value) -> Value {
        if rec.is_heap() {
            if let HeapObj::Array(a) = self.heap.get(rec.heap_index()) {
                return a.first().copied().unwrap_or(Value::NULL);
            }
        }
        rec
    }

    /// The `[iterator, next]` pair of a zip record slot (a bare iterator gets a
    /// freshly-read `next`; NULL yields None).
    fn iz_rec_parts(&self, rec: Value) -> Option<(Value, Value)> {
        if rec.is_heap() {
            if let HeapObj::Array(a) = self.heap.get(rec.heap_index()) {
                if a.len() >= 2 {
                    return Some((a[0], a[1]));
                }
            }
        }
        None
    }

    /// IteratorStepValue on a zip record slot: drive the iterator with its
    /// CACHED `next` ([[NextMethod]] — never re-read). A legacy bare-iterator
    /// slot falls back to the re-reading step.
    fn iz_step(&mut self, rec: Value) -> Result<Option<Value>, Thrown> {
        match self.iz_rec_parts(rec) {
            Some((it, next)) => self.iterator_step_with(it, next),
            None => self.iterator_step(rec),
        }
    }

    /// `Iterator.concat(...items)` (ES2025). Each item must be an Object with a
    /// callable `@@iterator`; the method is read ONCE here (eagerly, in argument
    /// order) and paired with its iterable. The returned Iterator Helper opens
    /// each iterable lazily — only when iteration reaches it — and yields all of
    /// its values before moving to the next.
    pub(crate) fn iterator_concat(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let mut pairs: Vec<Value> = Vec::with_capacity(args.len());
        for &item in args {
            if !self.is_object_value(item) {
                return Err(Thrown(
                    "TypeError: Iterator.concat argument is not an object".into(),
                ));
            }
            // GetMethod(item, @@iterator): undefined/null/non-callable all reject.
            let method = self.get_prop(item, "@@iterator")?;
            if !self.is_callable(method) {
                return Err(Thrown(
                    "TypeError: Iterator.concat argument is not iterable".into(),
                ));
            }
            pairs.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![item, method]))));
        }
        let src = Value::heap(self.heap.alloc(HeapObj::Array(pairs)));
        self.make_iter_helper(src, 6, Value::UNDEFINED, 0)
    }

    /// `Iterator.zip(iterables, options)` (keyed=false) / `Iterator.zipKeyed`
    /// (keyed=true). Opens every input iterator eagerly here (closing any already
    /// opened on an abrupt completion), reads the options (`mode`:
    /// shortest/longest/strict, `padding` for longest), then returns an Iterator
    /// Helper (kind 7) that lazily steps all iterators in lockstep. The multi-
    /// iterator state is encoded in the helper's existing fields: `source` = an
    /// Array of the open iterators (a `null` slot is a closed one), `arg` = the
    /// per-iterator padding Array, `inner` = the key Array (zipKeyed) or undefined.
    pub(crate) fn iterator_zip(
        &mut self,
        iterables: Value,
        options: Value,
        keyed: bool,
    ) -> Result<Value, Thrown> {
        let _gc = self.gc_lock_guard();
        let what = if keyed { "zipKeyed" } else { "zip" };
        if !self.is_object_value(iterables) {
            return Err(Thrown(format!("TypeError: Iterator.{what} called with a non-object")));
        }
        if options != Value::UNDEFINED && !self.is_object_value(options) {
            return Err(Thrown(format!("TypeError: Iterator.{what} options is not an object")));
        }
        // mode: shortest (0, default) / longest (1) / strict (2).
        let mode: u8 = if options != Value::UNDEFINED {
            let m = self.get_prop(options, "mode")?;
            if m == Value::UNDEFINED {
                0
            } else {
                // The spec does NO coercion: `mode` must be one of the three string
                // PRIMITIVES (a String wrapper / Symbol / number / {toString} is a
                // TypeError, with no toString call).
                let is_str_prim = m.is_heap()
                    && matches!(
                        self.heap.get(m.heap_index()),
                        HeapObj::Str(_) | HeapObj::Cons { .. }
                    );
                let s = if is_str_prim { self.to_js_string(m)? } else { String::new() };
                match s.as_str() {
                    "shortest" => 0,
                    "longest" => 1,
                    "strict" => 2,
                    _ => return Err(Thrown(format!("TypeError: Iterator.{what} invalid mode"))),
                }
            }
        } else {
            0
        };
        let padding_option = if mode == 1 && options != Value::UNDEFINED {
            let p = self.get_prop(options, "padding")?;
            if p != Value::UNDEFINED && !self.is_object_value(p) {
                return Err(Thrown(format!("TypeError: Iterator.{what} padding is not an object")));
            }
            p
        } else {
            Value::UNDEFINED
        };
        // Open each input iterator (closing the already-open ones on failure).
        let mut iters: Vec<Value> = Vec::new();
        let mut keys: Vec<Value> = Vec::new();
        if keyed {
            // zipKeyed walks the FULL own-key list ([[OwnPropertyKeys]] — integer,
            // string, THEN symbol keys), and per key interleaves [[GetOwnProperty]]
            // (skip absent / non-enumerable) → [[Get]] (skip undefined value) →
            // GetIteratorFlattenable(reject-primitives). Any abrupt completion closes
            // the already-opened iterators (reverse) first.
            let key_arr = self.object_own_keys(iterables)?;
            let key_list = match self.heap.get(key_arr.heap_index()) {
                HeapObj::Array(a) => a.clone(),
                _ => Vec::new(),
            };
            macro_rules! close_and_throw {
                ($e:expr) => {{
                    self.iz_close_others_abrupt(&iters, usize::MAX);
                    return Err($e);
                }};
            }
            for k in key_list {
                let ks = self.key_of(k);
                // [[GetOwnProperty]] (the gopd trap may throw for a Proxy).
                let desc = match self.proxy_gopd(iterables, &ks) {
                    Ok(Some(d)) => d,
                    Ok(None) => self.object_get_own_property_descriptor(iterables, &ks),
                    Err(e) => close_and_throw!(e),
                };
                if desc.is_undefined() {
                    continue; // property absent (e.g. deleted by an earlier [[Get]]).
                }
                let en = match self.get_prop(desc, "enumerable") {
                    Ok(v) => v,
                    Err(e) => close_and_throw!(e),
                };
                if !self.truthy(en) {
                    continue; // non-enumerable own keys are skipped (no [[Get]]).
                }
                // [[Get]] the value; a key whose value is undefined is skipped.
                let value = match self.get_member(iterables, &ks, iterables) {
                    Ok(v) => v,
                    Err(e) => close_and_throw!(e),
                };
                if value == Value::UNDEFINED {
                    continue;
                }
                match self.get_iterator_record_flattenable(value, true) {
                    Ok(rec) => {
                        iters.push(rec);
                        keys.push(k);
                    }
                    Err(e) => close_and_throw!(e),
                }
            }
        } else {
            // A real (steppable) iterator over the input — `get_iterator` returns a
            // plain array unchanged (no `.next()`), so use the @@iterator call form.
            // GetIteratorDirect: the input's `next` is Get ONCE (observable) and
            // cached for every step (spec [[NextMethod]]).
            let input_iter = self.get_iterator_direct(iterables)?;
            let input_next = self.get_prop(input_iter, "next")?;
            loop {
                match self.iterator_step_with(input_iter, input_next) {
                    Ok(None) => break,
                    Ok(Some(value)) => match self.get_iterator_record_flattenable(value, true) {
                        Ok(rec) => iters.push(rec),
                        Err(e) => {
                            // IfAbruptCloseIterators over «inputIter» ⧺ iters closes
                            // in REVERSE list order: the opened inner iterators
                            // (highest first), then the input iterator LAST — keeping
                            // the original abrupt value (close throws are discarded).
                            let saved = self.pending_throw;
                            let _ = self.iz_close_except(&iters, usize::MAX);
                            let _ = self.iterator_close(input_iter);
                            self.pending_throw = saved;
                            return Err(e);
                        }
                    },
                    Err(e) => {
                        // The input iterator's step threw; close the opened inners.
                        self.iz_close_others_abrupt(&iters, usize::MAX);
                        return Err(e);
                    }
                }
            }
        }
        let count = iters.len();
        // Longest-mode padding. For zip the padding option is an ITERABLE (read
        // `count` values, short → undefined fill); for zipKeyed it is an OBJECT
        // whose per-key property supplies that key's padding.
        let mut padding: Vec<Value> = vec![Value::UNDEFINED; count];
        if mode == 1 && padding_option != Value::UNDEFINED {
            // Any abrupt completion while reading padding must first IteratorCloseAll
            // the already-opened input iterators (reverse), keeping the abrupt value.
            macro_rules! pad_throw {
                ($e:expr) => {{
                    self.iz_close_others_abrupt(&iters, usize::MAX);
                    return Err($e);
                }};
            }
            if keyed {
                for (i, slot) in padding.iter_mut().enumerate() {
                    let ks = self.key_of(keys[i]);
                    match self.get_member(padding_option, &ks, padding_option) {
                        Ok(v) => *slot = v,
                        Err(e) => pad_throw!(e),
                    }
                }
            } else {
                // The padding option is iterated via real GetIterator (a non-iterable
                // such as `{}` is a TypeError — unlike GetIteratorFlattenable, which
                // would treat it as its own iterator). GetIteratorDirect Gets `next`
                // ONCE (observable) and that cached method drives every step.
                let pad_iter = match self.get_iterator_direct(padding_option) {
                    Ok(it) => it,
                    Err(e) => pad_throw!(e),
                };
                let pad_next = match self.get_prop(pad_iter, "next") {
                    Ok(n) => n,
                    Err(e) => pad_throw!(e),
                };
                let mut exhausted = false;
                for slot in padding.iter_mut() {
                    match self.iterator_step_with(pad_iter, pad_next) {
                        Ok(Some(v)) => *slot = v,
                        Ok(None) => {
                            exhausted = true;
                            break;
                        }
                        Err(e) => pad_throw!(e),
                    }
                }
                // IteratorClose(padding) — but ONLY when the iterator was not run
                // to exhaustion ([[Done]] suppresses the close; no `return` read).
                // A throwing return() propagates (closing the inputs first).
                if !exhausted {
                    if let Err(e) = self.iterator_close(pad_iter) {
                        pad_throw!(e);
                    }
                }
            }
        }
        let source = Value::heap(self.heap.alloc(HeapObj::Array(iters)));
        let arg = Value::heap(self.heap.alloc(HeapObj::Array(padding)));
        let inner = if keyed {
            Value::heap(self.heap.alloc(HeapObj::Array(keys)))
        } else {
            Value::UNDEFINED
        };
        let h = self.make_iter_helper(source, 7, arg, mode as i64)?;
        self.ih_set_inner(h.heap_index(), inner);
        Ok(h)
    }

    /// Set the i-th open iterator of a zip helper to `null` (closed/exhausted).
    fn iz_close_slot(&mut self, helper_idx: u32, i: usize) {
        let source = match self.heap.get(helper_idx) {
            HeapObj::IterHelper { source, .. } => *source,
            _ => return,
        };
        if let HeapObj::Array(items) = self.heap.get_mut(source.heap_index()) {
            if i < items.len() {
                items[i] = Value::NULL;
            }
        }
    }

    /// Close every still-open iterator in `iters` EXCEPT index `except`, in
    /// reverse order (spec IfAbruptCloseIterators closes highest-index first).
    /// Returns the first close error (if any). A `null` slot is already closed.
    ///
    /// IteratorClose threads ONE completion: once a throw is recorded, later close
    /// throws are DISCARDED. The Thrown error STRING already keeps the first, but the
    /// real thrown VALUE rides on `self.pending_throw`, which each close overwrites —
    /// so snapshot the first close's value and restore it after every later throw.
    fn iz_close_except(&mut self, iters: &[Value], except: usize) -> Option<Thrown> {
        let mut err = None;
        let mut first_pt: Option<Value> = None;
        for j in (0..iters.len()).rev() {
            if j != except && iters[j] != Value::NULL {
                let it = self.iz_rec_iter(iters[j]);
                if let Err(e) = self.iterator_close(it) {
                    if err.is_none() {
                        err = Some(e);
                        first_pt = self.pending_throw;
                    } else {
                        self.pending_throw = first_pt;
                    }
                }
            }
        }
        err
    }

    /// Close the other iterators after an ABRUPT completion at index `except`,
    /// keeping `self.pending_throw` (the original abrupt value) intact — every close
    /// throw is discarded so the original completion wins (value AND string).
    fn iz_close_others_abrupt(&mut self, iters: &[Value], except: usize) {
        let saved = self.pending_throw;
        let _ = self.iz_close_except(iters, except);
        self.pending_throw = saved;
    }

    /// Close `iters[lo..hi]` in REVERSE, discarding their close throws but keeping
    /// the current `self.pending_throw` (an already-set completion value) intact.
    fn iz_close_range_keep(&mut self, iters: &[Value], lo: usize, hi: usize) {
        let saved = self.pending_throw;
        let hi = hi.min(iters.len());
        for j in (lo..hi).rev() {
            if iters[j] != Value::NULL {
                let it = self.iz_rec_iter(iters[j]);
                let _ = self.iterator_close(it);
            }
        }
        self.pending_throw = saved;
    }

    /// Close every open iterator EXCEPT `except`, in reverse, keeping pending_throw.
    fn iz_close_all_except_keep(&mut self, iters: &[Value], except: usize) {
        let saved = self.pending_throw;
        for j in (0..iters.len()).rev() {
            if j != except && iters[j] != Value::NULL {
                let it = self.iz_rec_iter(iters[j]);
                let _ = self.iterator_close(it);
            }
        }
        self.pending_throw = saved;
    }

    /// Build the strict-mode "iterators have different lengths" TypeError, recording
    /// its VALUE on `pending_throw` so a subsequent close (which the caller runs with
    /// `*_keep`) cannot leak its own error value past it.
    fn iz_strict_type_error(&mut self) -> Thrown {
        let msg =
            self.alloc_str("Iterator.zip strict: iterators have different lengths".to_string());
        let te = self.make_error(1, Some(msg));
        self.pending_throw = Some(te);
        Thrown("TypeError: Iterator.zip strict: iterators have different lengths".into())
    }

    /// Close every still-open iterator of a zip helper (for `.return()`), in REVERSE
    /// order, propagating the FIRST close error (later throws discarded, value+string).
    pub(crate) fn iz_close_all(&mut self, helper_idx: u32) -> Option<Thrown> {
        let source = match self.heap.get(helper_idx) {
            HeapObj::IterHelper { source, .. } => *source,
            _ => return None,
        };
        let iters = match self.heap.get(source.heap_index()) {
            HeapObj::Array(a) => a.clone(),
            _ => Vec::new(),
        };
        self.iz_close_except(&iters, usize::MAX)
    }

    /// Lazy `.next()` for a zip helper, with the same re-entrancy guard as the
    /// single-source helpers (GeneratorValidate): a user iterator's `next()` that
    /// re-enters this helper while a step is executing is a TypeError.
    pub(crate) fn iter_zip_next(&mut self, idx: u32) -> Result<Value, Thrown> {
        match self.heap.get(idx) {
            HeapObj::IterHelper { running: true, .. } => {
                return Err(Thrown("TypeError: Iterator is already running".into()));
            }
            HeapObj::IterHelper { .. } => {}
            _ => {
                return Err(Thrown(
                    "TypeError: Iterator Helper next on incompatible receiver".into(),
                ))
            }
        }
        self.ih_set_running(idx, true);
        let r = self.iter_zip_next_inner(idx);
        self.ih_set_running(idx, false);
        r
    }

    /// One step of a zip Iterator Helper (kind 7): step every open iterator in
    /// lockstep and assemble one tuple (an Array for zip, a keyed object for
    /// zipKeyed) per the mode.
    fn iter_zip_next_inner(&mut self, idx: u32) -> Result<Value, Thrown> {
        let _gc = self.gc_lock_guard();
        let (source, arg, inner, mode, done) = match self.heap.get(idx) {
            HeapObj::IterHelper { source, arg, inner, n, done, .. } => {
                (*source, *arg, *inner, *n as u8, *done)
            }
            _ => return Err(Thrown("TypeError: Iterator Helper next on incompatible receiver".into())),
        };
        if done {
            return Ok(self.iter_result(Value::UNDEFINED, true));
        }
        let mut iters: Vec<Value> = match self.heap.get(source.heap_index()) {
            HeapObj::Array(a) => a.clone(),
            _ => Vec::new(),
        };
        let padding: Vec<Value> = match self.heap.get(arg.heap_index()) {
            HeapObj::Array(a) => a.clone(),
            _ => Vec::new(),
        };
        let count = iters.len();
        // Zero iterables → the zip iterator is immediately done.
        if count == 0 {
            self.ih_set_done(idx);
            return Ok(self.iter_result(Value::UNDEFINED, true));
        }
        let pad = |i: usize| padding.get(i).copied().unwrap_or(Value::UNDEFINED);
        let mut results: Vec<Value> = vec![Value::UNDEFINED; count];
        match mode {
            0 => {
                // shortest: any exhausted iterator finishes the zip; close the rest.
                for i in 0..count {
                    match self.iz_step(iters[i]) {
                        Ok(None) => {
                            // Normal completion: close the others — a close error
                            // surfaces (the completion was not abrupt).
                            self.ih_set_done(idx);
                            if let Some(e) = self.iz_close_except(&iters, i) {
                                return Err(e);
                            }
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        Ok(Some(v)) => results[i] = v,
                        Err(e) => {
                            // Abrupt: close the others (their errors are swallowed —
                            // the original abrupt completion wins).
                            self.ih_set_done(idx);
                            self.iz_close_others_abrupt(&iters, i);
                            return Err(e);
                        }
                    }
                }
            }
            1 => {
                // longest: continue until all are exhausted, padding the finished.
                let mut all_done = true;
                for i in 0..count {
                    if iters[i] == Value::NULL {
                        results[i] = pad(i);
                        continue;
                    }
                    match self.iz_step(iters[i]) {
                        Ok(None) => {
                            // Mark exhausted in BOTH the heap (persists to the next
                            // step) and the local copy (so an abrupt close below skips
                            // it — a done iterator must not have return() called).
                            self.iz_close_slot(idx, i);
                            iters[i] = Value::NULL;
                            results[i] = pad(i);
                        }
                        Ok(Some(v)) => {
                            results[i] = v;
                            all_done = false;
                        }
                        Err(e) => {
                            self.ih_set_done(idx);
                            self.iz_close_others_abrupt(&iters, i);
                            return Err(e);
                        }
                    }
                }
                if all_done {
                    self.ih_set_done(idx);
                    return Ok(self.iter_result(Value::UNDEFINED, true));
                }
            }
            _ => {
                // strict: every iterator must end on the same step.
                for i in 0..count {
                    match self.iz_step(iters[i]) {
                        Ok(None) => {
                            self.ih_set_done(idx);
                            if i == 0 {
                                // The first ended; the rest must ALSO be done now.
                                // Iterators that return done need no close; only an
                                // iterator that YIELDS (mismatch, close j..) or whose
                                // step THROWS (close j+1.., it's the abrupt source)
                                // triggers a close — both in reverse, keeping the
                                // surviving completion value.
                                for j in 1..count {
                                    match self.iz_step(iters[j]) {
                                        Ok(None) => {}
                                        Ok(Some(_)) => {
                                            let thr = self.iz_strict_type_error();
                                            self.iz_close_range_keep(&iters, j, count);
                                            return Err(thr);
                                        }
                                        Err(e) => {
                                            self.iz_close_range_keep(&iters, j + 1, count);
                                            return Err(e);
                                        }
                                    }
                                }
                                return Ok(self.iter_result(Value::UNDEFINED, true));
                            }
                            // An earlier iterator yielded a value but this one ended:
                            // length mismatch. Close every OTHER open iterator (the
                            // earlier yielders + the not-yet-stepped tail), reverse.
                            let thr = self.iz_strict_type_error();
                            self.iz_close_all_except_keep(&iters, i);
                            return Err(thr);
                        }
                        Ok(Some(v)) => results[i] = v,
                        Err(e) => {
                            // Abrupt step: close the others, the original wins.
                            self.ih_set_done(idx);
                            self.iz_close_others_abrupt(&iters, i);
                            return Err(e);
                        }
                    }
                }
            }
        }
        // Assemble the tuple: an Array (zip) or a keyed object (zipKeyed).
        let out = if inner != Value::UNDEFINED {
            let keys: Vec<Value> = match self.heap.get(inner.heap_index()) {
                HeapObj::Array(a) => a.clone(),
                _ => Vec::new(),
            };
            let mut m = ObjMap::new();
            for i in 0..count {
                // key_of keeps a symbol key's internal `@@`-prop key, so the result
                // object carries the original Symbol property (not "Symbol(...)").
                let ks = self.key_of(keys[i]);
                m.set(&ks, results[i]);
            }
            // The zipKeyed result is a NULL-prototype ordinary object (its keyed
            // properties keep the default data attributes from `set`).
            let o = self.heap.alloc(HeapObj::Object(Box::new(m)));
            self.proto_of.insert(o, Value::NULL);
            Value::heap(o)
        } else {
            Value::heap(self.heap.alloc(HeapObj::Array(results)))
        };
        // Mark the helper "started" (suspended at a yield): `.return()` then resumes
        // it in the "executing" state, vs a suspended-START return which completes
        // without the executing brand. `idx` is otherwise unused for kind 7.
        self.ih_inc_idx(idx);
        Ok(self.iter_result(out, false))
    }

    fn make_iter_helper(&mut self, source: Value, kind: u8, arg: Value, n: i64) -> Result<Value, Thrown> {
        // GetIteratorDirect(source): read `next` ONCE now (a getter fires once, and a
        // throwing `next` getter propagates here at creation). Single-source helpers
        // (kinds 0..=5) then step via this cached method; a generator uses the internal
        // step path, and zip/concat (6/7) hold an Array of sub-iterators.
        let next = if kind <= 5
            && source.is_heap()
            && !matches!(self.heap.get(source.heap_index()), HeapObj::Generator { .. })
        {
            self.get_prop(source, "next")?
        } else {
            Value::UNDEFINED
        };
        self.alloc_iter_helper(source, kind, arg, n, next)
    }

    /// `make_iter_helper` for a caller that has ALREADY performed GetIteratorDirect's
    /// `Get(source, "next")` — Iterator.from must do that read before its %Iterator%
    /// brand check, so the read cannot be deferred to helper creation.
    fn alloc_iter_helper(
        &mut self,
        source: Value,
        kind: u8,
        arg: Value,
        n: i64,
        next: Value,
    ) -> Result<Value, Thrown> {
        let idx = self.heap.alloc(HeapObj::IterHelper {
            source,
            kind,
            arg,
            n,
            idx: 0,
            done: false,
            inner: Value::UNDEFINED,
            next,
            running: false,
        });
        if self.iterator_helper_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.iterator_helper_proto));
        }
        Ok(Value::heap(idx))
    }

    // Field mutators for an IterHelper (kept tiny to dodge borrow conflicts).
    fn ih_set_done(&mut self, idx: u32) {
        if let HeapObj::IterHelper { done, .. } = self.heap.get_mut(idx) {
            *done = true;
        }
    }
    fn ih_inc_idx(&mut self, idx: u32) {
        if let HeapObj::IterHelper { idx: i, .. } = self.heap.get_mut(idx) {
            *i += 1;
        }
    }
    fn ih_set_n(&mut self, idx: u32, v: i64) {
        if let HeapObj::IterHelper { n, .. } = self.heap.get_mut(idx) {
            *n = v;
        }
    }
    fn ih_set_inner(&mut self, idx: u32, v: Value) {
        if let HeapObj::IterHelper { inner, .. } = self.heap.get_mut(idx) {
            *inner = v;
        }
    }
    pub(crate) fn ih_set_running(&mut self, idx: u32, v: bool) {
        if let HeapObj::IterHelper { running, .. } = self.heap.get_mut(idx) {
            *running = v;
        }
    }

    /// take()/drop() count (spec 27.1.4.3/.7): numLimit = ToNumber(limit); a NaN
    /// numLimit is a RangeError; integerLimit = ToIntegerOrInfinity(numLimit) (so a
    /// fraction truncates toward zero — `take(-0.5)` → 0, not negative); a negative
    /// integerLimit (incl. -∞) is a RangeError; +∞ means "all".
    fn iter_limit_arg(&mut self, v: Value) -> Result<i64, Thrown> {
        // ToNumber so an object limit's valueOf/@@toPrimitive runs (a throwing one
        // propagates); a Symbol/BigInt is a TypeError. `to_number_coerce` shares the
        // deliberately BigInt-lenient `to_number` (needed for `1n < 2`), so
        // `take(1n)` silently became 1 — the strict variant is the real ToNumber.
        let n = self.to_number_strict(v)?;
        if n.is_nan() {
            return Err(Thrown("RangeError: take/drop limit must not be NaN".into()));
        }
        if n.is_infinite() {
            return if n < 0.0 {
                Err(Thrown("RangeError: take/drop limit must be non-negative".into()))
            } else {
                Ok(i64::MAX)
            };
        }
        // ToIntegerOrInfinity truncates toward zero BEFORE the sign check, so
        // `-0.5` → -0 (allowed) but `-1` → -1 (RangeError).
        let int_limit = n.trunc();
        if int_limit < 0.0 {
            return Err(Thrown("RangeError: take/drop limit must be non-negative".into()));
        }
        Ok(int_limit as i64)
    }

    /// `iter_limit_arg` but IteratorClose(`src`) on an abrupt completion (the take/drop
    /// limit is validated AFTER GetIteratorDirect, so a bad limit closes the source).
    fn iter_limit_or_close(&mut self, v: Value, src: Value) -> Result<i64, Thrown> {
        match self.iter_limit_arg(v) {
            Ok(n) => Ok(n),
            Err(e) => {
                let _ = self.iterator_close(src);
                Err(e)
            }
        }
    }

    /// GetIteratorDirect's `next` read for a consuming helper (toArray/forEach/some/…):
    /// read `iter.next` ONCE (propagating a throwing getter) so the loop steps via the
    /// cached method. A generator uses the internal step path (UNDEFINED).
    fn iter_direct_next(&mut self, iter: Value) -> Result<Value, Thrown> {
        if iter.is_heap() && !matches!(self.heap.get(iter.heap_index()), HeapObj::Generator { .. }) {
            self.get_prop(iter, "next")
        } else {
            Ok(Value::UNDEFINED)
        }
    }

    /// Step a single-source helper: use the cached `next` (GetIteratorDirect) when set,
    /// else the generic step path (a generator source).
    fn ih_step(&mut self, source: Value, next: Value) -> Result<Option<Value>, Thrown> {
        if next != Value::UNDEFINED {
            self.iterator_step_with(source, next)
        } else {
            self.iterator_step(source)
        }
    }

    /// Call a helper callback (`this` = undefined); on an abrupt completion,
    /// IteratorClose the source iterator `src` before propagating (the callback's error
    /// wins over any close error), per the helpers' IfAbruptCloseIterator.
    fn iter_call_close(&mut self, cb: Value, src: Value, args: &[Value]) -> Result<Value, Thrown> {
        match self.call_value(cb, Value::UNDEFINED, args) {
            Ok(v) => Ok(v),
            Err(e) => {
                // IfAbruptCloseIterator returns the ORIGINAL completion: the callback's
                // thrown value wins even if the source's return() also throws — which is
                // exactly what `iterator_close_quiet` guarantees (it preserves the
                // `pending_throw` VALUE the close would otherwise overwrite).
                self.iterator_close_quiet(src);
                Err(e)
            }
        }
    }

    /// Dispatch an `Iterator.prototype` helper. `this` is the source iterator.
    pub(crate) fn iter_helper_method(
        &mut self,
        id: u16,
        this: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        use native::*;
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        if !self.iter_receiver_ok(this) {
            return Err(Thrown("TypeError: Iterator helper called on a non-object".into()));
        }
        let needs_fn = matches!(
            id,
            ITER_MAP | ITER_FILTER | ITER_FLATMAP | ITER_FOREACH | ITER_SOME | ITER_EVERY | ITER_FIND
        );
        // GetIteratorDirect(O) precedes the argument checks, so a callback that is not
        // callable (or, for take/drop, a limit that fails ToNumber / is out of range)
        // must IteratorClose the underlying iterator before throwing.
        if needs_fn && !self.is_callable(a0) {
            let _ = self.iterator_close(this);
            return Err(Thrown("TypeError: the callback argument is not a function".into()));
        }
        match id {
            ITER_MAP => self.make_iter_helper(this, 0, a0, 0),
            ITER_FILTER => self.make_iter_helper(this, 1, a0, 0),
            ITER_FLATMAP => self.make_iter_helper(this, 4, a0, 0),
            ITER_TAKE => {
                let n = self.iter_limit_or_close(a0, this)?;
                self.make_iter_helper(this, 2, Value::UNDEFINED, n)
            }
            ITER_DROP => {
                let n = self.iter_limit_or_close(a0, this)?;
                self.make_iter_helper(this, 3, Value::UNDEFINED, n)
            }
            ITER_TOARRAY => {
                let next = self.iter_direct_next(this)?;
                let mut out = Vec::new();
                while let Some(v) = self.ih_step(this, next)? {
                    out.push(v);
                }
                Ok(self.alloc_array_current_realm(out))
            }
            ITER_JOIN => {
                let sep = if a0 == Value::UNDEFINED {
                    ",".to_string()
                } else {
                    match self.to_js_string(a0) {
                        Ok(s) => s,
                        Err(e) => {
                            self.iterator_close_quiet(this);
                            return Err(e);
                        }
                    }
                };
                let next = self.iter_direct_next(this)?;
                let mut out = String::new();
                let mut first = true;
                while let Some(v) = self.ih_step(this, next)? {
                    if !first {
                        out.push_str(&sep);
                    }
                    first = false;
                    if !v.is_nullish() {
                        match self.to_js_string(v) {
                            Ok(s) => out.push_str(&s),
                            Err(e) => {
                                self.iterator_close_quiet(this);
                                return Err(e);
                            }
                        }
                    }
                }
                Ok(self.alloc_str(out))
            }
            ITER_FOREACH => {
                let next = self.iter_direct_next(this)?;
                let mut i = 0i64;
                while let Some(v) = self.ih_step(this, next)? {
                    // A throwing callback IteratorCloses the source (its error wins).
                    self.iter_call_close(a0, this, &[v, Value::num(i as f64)])?;
                    i += 1;
                }
                Ok(Value::UNDEFINED)
            }
            ITER_SOME => {
                let next = self.iter_direct_next(this)?;
                let mut i = 0i64;
                while let Some(v) = self.ih_step(this, next)? {
                    let r = self.iter_call_close(a0, this, &[v, Value::num(i as f64)])?;
                    if self.truthy(r) {
                        // Early return ALSO closes the iterator (IteratorClose).
                        self.iterator_close(this)?;
                        return Ok(Value::bool(true));
                    }
                    i += 1;
                }
                Ok(Value::bool(false))
            }
            ITER_EVERY => {
                let next = self.iter_direct_next(this)?;
                let mut i = 0i64;
                while let Some(v) = self.ih_step(this, next)? {
                    let r = self.iter_call_close(a0, this, &[v, Value::num(i as f64)])?;
                    if !self.truthy(r) {
                        self.iterator_close(this)?;
                        return Ok(Value::bool(false));
                    }
                    i += 1;
                }
                Ok(Value::bool(true))
            }
            ITER_FIND => {
                let next = self.iter_direct_next(this)?;
                let mut i = 0i64;
                while let Some(v) = self.ih_step(this, next)? {
                    let r = self.iter_call_close(a0, this, &[v, Value::num(i as f64)])?;
                    if self.truthy(r) {
                        self.iterator_close(this)?;
                        return Ok(v);
                    }
                    i += 1;
                }
                Ok(Value::UNDEFINED)
            }
            ITER_REDUCE => {
                if !self.is_callable(a0) {
                    let _ = self.iterator_close(this);
                    return Err(Thrown("TypeError: reduce reducer is not a function".into()));
                }
                let next = self.iter_direct_next(this)?;
                let has_init = args.len() >= 2;
                let mut acc = if has_init { args[1] } else { Value::UNDEFINED };
                let mut i = 0i64;
                if !has_init {
                    match self.ih_step(this, next)? {
                        Some(v) => {
                            acc = v;
                            i = 1;
                        }
                        None => {
                            return Err(Thrown(
                                "TypeError: reduce of empty iterator with no initial value".into(),
                            ))
                        }
                    }
                }
                while let Some(v) = self.ih_step(this, next)? {
                    acc = self.iter_call_close(a0, this, &[acc, v, Value::num(i as f64)])?;
                    i += 1;
                }
                Ok(acc)
            }
            _ => Err(Thrown("TypeError: unknown iterator helper".into())),
        }
    }

    /// Lazy `.next()` for an Iterator Helper (the `%IteratorHelperPrototype%.next`).
    /// Guards re-entrancy: a callback that calls `.next()` on the *same* helper while
    /// a step is in flight gets a TypeError (GeneratorValidate "executing"), not a
    /// stack overflow / silent wrong answer. The `running` brand is always cleared.
    pub(crate) fn iter_helper_next(&mut self, idx: u32) -> Result<Value, Thrown> {
        match self.heap.get(idx) {
            HeapObj::IterHelper { running: true, .. } => {
                return Err(Thrown("TypeError: Iterator is already running".into()));
            }
            HeapObj::IterHelper { .. } => {}
            _ => {
                return Err(Thrown(
                    "TypeError: Iterator Helper next called on incompatible receiver".into(),
                ))
            }
        }
        self.ih_set_running(idx, true);
        let r = self.iter_helper_next_inner(idx);
        self.ih_set_running(idx, false);
        r
    }

    fn iter_helper_next_inner(&mut self, idx: u32) -> Result<Value, Thrown> {
        loop {
            let (source, kind, arg, n, cidx, done, inner, next) = match self.heap.get(idx) {
                HeapObj::IterHelper { source, kind, arg, n, idx, done, inner, next, .. } => {
                    (*source, *kind, *arg, *n, *idx, *done, *inner, *next)
                }
                _ => {
                    return Err(Thrown(
                        "TypeError: Iterator Helper next called on incompatible receiver".into(),
                    ))
                }
            };
            if done {
                return Ok(self.iter_result(Value::UNDEFINED, true));
            }
            match kind {
                0 => {
                    // map
                    match self.ih_step(source, next)? {
                        None => {
                            self.ih_set_done(idx);
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        Some(v) => {
                            // A throwing mapper IteratorCloses the source.
                            let mapped =
                                self.iter_call_close(arg, source, &[v, Value::num(cidx as f64)])?;
                            self.ih_inc_idx(idx);
                            return Ok(self.iter_result(mapped, false));
                        }
                    }
                }
                1 => {
                    // filter
                    match self.ih_step(source, next)? {
                        None => {
                            self.ih_set_done(idx);
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        Some(v) => {
                            let keep =
                                self.iter_call_close(arg, source, &[v, Value::num(cidx as f64)])?;
                            self.ih_inc_idx(idx);
                            if self.truthy(keep) {
                                return Ok(self.iter_result(v, false));
                            }
                            continue;
                        }
                    }
                }
                2 => {
                    // take
                    if n <= 0 {
                        // Reaching the limit closes the source (IteratorClose).
                        self.ih_set_done(idx);
                        self.iterator_close(source)?;
                        return Ok(self.iter_result(Value::UNDEFINED, true));
                    }
                    self.ih_set_n(idx, n - 1);
                    match self.ih_step(source, next)? {
                        None => {
                            self.ih_set_done(idx);
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        Some(v) => return Ok(self.iter_result(v, false)),
                    }
                }
                3 => {
                    // drop
                    let mut nn = n;
                    while nn > 0 {
                        match self.ih_step(source, next)? {
                            None => {
                                self.ih_set_done(idx);
                                return Ok(self.iter_result(Value::UNDEFINED, true));
                            }
                            Some(_) => nn -= 1,
                        }
                    }
                    self.ih_set_n(idx, 0);
                    match self.ih_step(source, next)? {
                        None => {
                            self.ih_set_done(idx);
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        Some(v) => return Ok(self.iter_result(v, false)),
                    }
                }
                4 => {
                    // flatMap
                    if inner != Value::UNDEFINED {
                        // IfAbruptCloseIterator(innerValue, iterated) (27.1.4.9 step
                        // 6.b.viii.2): a throw from the INNER iterator — its next(), or
                        // the `done`/`value` getters on the result it returns — must
                        // close the OUTER iterator before propagating. Only the inner's
                        // completion survives, so the close is the quiet form.
                        match self.iterator_step(inner) {
                            Ok(Some(v)) => return Ok(self.iter_result(v, false)),
                            Ok(None) => {
                                self.ih_set_inner(idx, Value::UNDEFINED);
                                continue;
                            }
                            Err(e) => {
                                self.iterator_close_quiet(source);
                                return Err(e);
                            }
                        }
                    }
                    match self.ih_step(source, next)? {
                        None => {
                            self.ih_set_done(idx);
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        Some(v) => {
                            let mapped =
                                self.iter_call_close(arg, source, &[v, Value::num(cidx as f64)])?;
                            self.ih_inc_idx(idx);
                            // IfAbruptCloseIterator(innerIterator, iterated) (step
                            // 6.b.vi): a mapper result that is not flattenable (a
                            // primitive, or an @@iterator returning a non-object) also
                            // closes the OUTER iterator.
                            let it = match self.get_iterator_flattenable(mapped, true) {
                                Ok(it) => it,
                                Err(e) => {
                                    self.iterator_close_quiet(source);
                                    return Err(e);
                                }
                            };
                            self.ih_set_inner(idx, it);
                            continue;
                        }
                    }
                }
                6 => {
                    // concat: `source` is an Array of [iterable, method] pairs;
                    // `cidx` is the next pair to open; `inner` is the currently-open
                    // iterator (or UNDEFINED). Drain `inner`, then open the next pair.
                    if inner != Value::UNDEFINED {
                        match self.iterator_step(inner)? {
                            Some(v) => return Ok(self.iter_result(v, false)),
                            None => self.ih_set_inner(idx, Value::UNDEFINED),
                        }
                    }
                    let pairs = match self.heap.get(source.heap_index()) {
                        HeapObj::Array(items) => items.clone(),
                        _ => Vec::new(),
                    };
                    if (cidx as usize) >= pairs.len() {
                        self.ih_set_done(idx);
                        return Ok(self.iter_result(Value::UNDEFINED, true));
                    }
                    let (iterable, method) = match self.heap.get(pairs[cidx as usize].heap_index()) {
                        HeapObj::Array(p) => (p[0], p[1]),
                        _ => (Value::UNDEFINED, Value::UNDEFINED),
                    };
                    self.ih_inc_idx(idx);
                    let it = self.call_value(method, iterable, &[])?;
                    if !self.is_object_value(it) {
                        self.ih_set_done(idx);
                        return Err(Thrown(
                            "TypeError: Iterator.concat: the iterator method did not return an object"
                                .into(),
                        ));
                    }
                    self.ih_set_inner(idx, it);
                    continue;
                }
                _ => {
                    // 5 = %WrapForValidIteratorPrototype%.next (Iterator.from of a
                    // foreign iterator). Unlike every other kind this is NOT a
                    // generator closure: the whole spec body is
                    // `Return ? Call(record.[[NextMethod]], record.[[Iterator]])`.
                    // The result object is forwarded VERBATIM — no IteratorComplete /
                    // IteratorValue, so `done`/`value` getters are never fired here,
                    // a non-object (even a primitive) passes straight through instead
                    // of being rejected, and the wrapper keeps no [[Done]] state.
                    return self.call_value(next, source, &[]);
                }
            }
        }
    }

    /// `Iterator.from(O)` — wrap an iterable/iterator as an Iterator Helper so it
    /// gains the helper methods.
    pub(crate) fn iterator_from(&mut self, o: Value) -> Result<Value, Thrown> {
        // A string yields its code-point iterator; otherwise get the iterable's
        // iterator (or use it directly if it is one).
        let it = self.get_iterator_flattenable(o, false)?;
        // GetIteratorFlattenable ENDS with GetIteratorDirect(iterator), so the
        // observable `Get(iterator, "next")` is step 1 — it precedes the
        // OrdinaryHasInstance brand check below. A Proxy source must therefore log
        // `get: next` BEFORE `getPrototypeOf`, and an iterator that IS returned
        // unwrapped still has its `next` getter fired exactly once.
        let next = self.get_prop(it, "next")?;
        // If the iterator ALREADY inherits %Iterator.prototype% (OrdinaryHasInstance
        // (%Iterator%, it) — e.g. a generator or a built-in iterator), return it
        // unwrapped; only a foreign iterator gets the WrapForValidIterator wrapper.
        // Walk via object_get_prototype_of so a Generator's gen_proto link is followed
        // (generator instances resolve their [[Prototype]] specially, not via proto_of).
        // %Iterator% is the intrinsic of the REALM of the `Iterator.from` running now,
        // so `otherGlobal.Iterator.from(mainRealmIterator)` must WRAP: the argument
        // inherits the main realm's %Iterator.prototype%, not the child realm's image.
        let iter_root = self.native_home(self.iterator_proto_root);
        if it.is_heap() && iter_root != 0 {
            let mut cur = it;
            for _ in 0..64 {
                let p = self.object_get_prototype_of(cur);
                if !p.is_heap() {
                    break;
                }
                if p.heap_index() == iter_root {
                    return Ok(it);
                }
                cur = p;
            }
        }
        // The wrapper's [[Iterated]] record keeps the `next` read above — the spec
        // never re-reads it, so a later mutation of `iterator.next` is not observed.
        self.alloc_iter_helper(it, 5, Value::UNDEFINED, 0, next)
    }
}
