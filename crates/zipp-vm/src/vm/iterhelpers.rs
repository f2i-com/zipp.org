#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap, PropAttr,
    PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// GetIteratorDirect receiver check: `this` must be an object (not a string/
    /// symbol/bigint primitive).
    fn iter_receiver_ok(&self, this: Value) -> bool {
        this.is_heap()
            && !matches!(
                self.heap.get(this.heap_index()),
                HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_)
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
    /// `value`; `ret` ⇒ the generator must RETURN `value`; both false ⇒ yield
    /// `value` and keep delegating.
    pub(crate) fn iter_delegate_step(
        &mut self,
        iter: Value,
        mode: i32,
        sent: Value,
    ) -> Result<(Value, bool, bool), Thrown> {
        // Read result.value/result.done after validating `result` is an object.
        let unpack = |vm: &mut Self, result: Value| -> Result<(Value, bool), Thrown> {
            if !vm.is_object_value(result) {
                return Err(Thrown(
                    "TypeError: iterator result is not an object".into(),
                ));
            }
            let d = vm.get_prop(result, "done")?;
            let done = vm.truthy(d);
            let value = vm.get_prop(result, "value")?;
            Ok((value, done))
        };
        match mode {
            // next: result = iter.next(sent) → done completes the yield* expression.
            0 => {
                let next = self.get_prop(iter, "next")?;
                let result = self.call_value(next, iter, &[sent])?;
                let (value, done) = unpack(self, result)?;
                Ok((value, done, false))
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
                let (value, done) = unpack(self, result)?;
                Ok((value, done, false))
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
                let (value, done) = unpack(self, result)?;
                Ok((value, false, done))
            }
        }
    }

    /// GetIteratorFlattenable: obtain a steppable iterator from any iterable
    /// (arrays/strings/Map/Set via @@iterator) or an object that is itself an
    /// iterator (has a callable `.next`).
    pub(crate) fn get_iterator_flattenable(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap()
            && !matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Symbol { .. } | HeapObj::BigInt(_)
            )
        {
            let m = self.get_prop(v, "@@iterator")?;
            if self.is_callable(m) {
                return self.call_value(m, v, &[]);
            }
            let next = self.get_prop(v, "next")?;
            if self.is_callable(next) {
                return Ok(v);
            }
        }
        Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))))
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
                match self.to_js_string(m)?.as_str() {
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
            let key_arr = self.object_enum_own(iterables, crate::vm::EnumWhat::Keys)?;
            let key_list = match self.heap.get(key_arr.heap_index()) {
                HeapObj::Array(a) => a.clone(),
                _ => Vec::new(),
            };
            for k in key_list {
                let ks = self.to_js_string(k)?;
                // Any abrupt completion while opening closes the already-opened
                // iterators (in reverse) before propagating.
                let value = match self.get_member(iterables, &ks, iterables) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = self.iz_close_except(&iters, usize::MAX);
                        return Err(e);
                    }
                };
                match self.get_iterator_flattenable(value) {
                    Ok(it) => {
                        iters.push(it);
                        keys.push(k);
                    }
                    Err(e) => {
                        let _ = self.iz_close_except(&iters, usize::MAX);
                        return Err(e);
                    }
                }
            }
        } else {
            // A real (steppable) iterator over the input — `get_iterator` returns a
            // plain array unchanged (no `.next()`), so use the @@iterator call form.
            let input_iter = self.get_iterator_flattenable(iterables)?;
            loop {
                match self.iterator_step(input_iter) {
                    Ok(None) => break,
                    Ok(Some(value)) => match self.get_iterator_flattenable(value) {
                        Ok(it) => iters.push(it),
                        Err(e) => {
                            // Close the input iterator + the already-opened inner
                            // iterators (in reverse), then propagate.
                            let _ = self.iterator_close(input_iter);
                            let _ = self.iz_close_except(&iters, usize::MAX);
                            return Err(e);
                        }
                    },
                    Err(e) => {
                        // The input iterator's step threw; close the opened inners.
                        let _ = self.iz_close_except(&iters, usize::MAX);
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
            if keyed {
                for (i, slot) in padding.iter_mut().enumerate() {
                    let ks = self.to_js_string(keys[i])?;
                    *slot = self.get_member(padding_option, &ks, padding_option)?;
                }
            } else {
                let pad_iter = self.get_iterator_flattenable(padding_option)?;
                for slot in padding.iter_mut() {
                    match self.iterator_step(pad_iter)? {
                        Some(v) => *slot = v,
                        None => break,
                    }
                }
                let _ = self.iterator_close(pad_iter);
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
    fn iz_close_except(&mut self, iters: &[Value], except: usize) -> Option<Thrown> {
        let mut err = None;
        for j in (0..iters.len()).rev() {
            if j != except && iters[j] != Value::NULL {
                if let Err(e) = self.iterator_close(iters[j]) {
                    if err.is_none() {
                        err = Some(e);
                    }
                }
            }
        }
        err
    }

    /// Close every still-open iterator of a zip helper (for `.return()`).
    pub(crate) fn iz_close_all(&mut self, helper_idx: u32) {
        let source = match self.heap.get(helper_idx) {
            HeapObj::IterHelper { source, .. } => *source,
            _ => return,
        };
        let iters = match self.heap.get(source.heap_index()) {
            HeapObj::Array(a) => a.clone(),
            _ => Vec::new(),
        };
        for it in iters {
            if it != Value::NULL {
                let _ = self.iterator_close(it);
            }
        }
    }

    /// One step of a zip Iterator Helper (kind 7): step every open iterator in
    /// lockstep and assemble one tuple (an Array for zip, a keyed object for
    /// zipKeyed) per the mode.
    pub(crate) fn iter_zip_next(&mut self, idx: u32) -> Result<Value, Thrown> {
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
        let iters: Vec<Value> = match self.heap.get(source.heap_index()) {
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
                    match self.iterator_step(iters[i]) {
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
                            let _ = self.iz_close_except(&iters, i);
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
                    match self.iterator_step(iters[i]) {
                        Ok(None) => {
                            self.iz_close_slot(idx, i);
                            results[i] = pad(i);
                        }
                        Ok(Some(v)) => {
                            results[i] = v;
                            all_done = false;
                        }
                        Err(e) => {
                            self.ih_set_done(idx);
                            let _ = self.iz_close_except(&iters, i);
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
                    match self.iterator_step(iters[i]) {
                        Ok(None) => {
                            self.ih_set_done(idx);
                            if i == 0 {
                                // The rest must also be done now.
                                for j in 1..count {
                                    match self.iterator_step(iters[j]) {
                                        Ok(None) => {}
                                        Ok(Some(_)) => {
                                            for &it in iters.iter().skip(j + 1) {
                                                let _ = self.iterator_close(it);
                                            }
                                            return Err(Thrown(
                                                "TypeError: Iterator.zip strict: iterators have different lengths".into(),
                                            ));
                                        }
                                        Err(e) => {
                                            for &it in iters.iter().skip(j + 1) {
                                                let _ = self.iterator_close(it);
                                            }
                                            return Err(e);
                                        }
                                    }
                                }
                                return Ok(self.iter_result(Value::UNDEFINED, true));
                            }
                            // An earlier iterator yielded a value but this one ended.
                            for &it in iters.iter().skip(i + 1) {
                                let _ = self.iterator_close(it);
                            }
                            return Err(Thrown(
                                "TypeError: Iterator.zip strict: iterators have different lengths".into(),
                            ));
                        }
                        Ok(Some(v)) => results[i] = v,
                        Err(e) => {
                            // Abrupt step: close the others, the original wins.
                            self.ih_set_done(idx);
                            let _ = self.iz_close_except(&iters, i);
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
                let ks = self.to_js_string(keys[i])?;
                m.set(&ks, results[i]);
            }
            // The zipKeyed result is a NULL-prototype ordinary object (its keyed
            // properties keep the default data attributes from `set`).
            let o = self.heap.alloc(HeapObj::Object(m));
            self.proto_of.insert(o, Value::NULL);
            Value::heap(o)
        } else {
            Value::heap(self.heap.alloc(HeapObj::Array(results)))
        };
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
        let idx = self.heap.alloc(HeapObj::IterHelper {
            source,
            kind,
            arg,
            n,
            idx: 0,
            done: false,
            inner: Value::UNDEFINED,
            next,
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

    /// take()/drop() count (spec 27.1.4.3/.7): numLimit = ToNumber(limit); a NaN
    /// numLimit is a RangeError; integerLimit = ToIntegerOrInfinity(numLimit) (so a
    /// fraction truncates toward zero — `take(-0.5)` → 0, not negative); a negative
    /// integerLimit (incl. -∞) is a RangeError; +∞ means "all".
    fn iter_limit_arg(&mut self, v: Value) -> Result<i64, Thrown> {
        // ToNumber so an object limit's valueOf/@@toPrimitive runs (a throwing one
        // propagates); a Symbol/BigInt is a TypeError.
        let n = self.to_number_coerce(v)?;
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
                // thrown value wins even if the source's return() also throws. Since the
                // thrown VALUE lives in `pending_throw` (which the return() call would
                // overwrite), save and restore it around the close.
                let saved = self.pending_throw;
                let _ = self.iterator_close(src);
                self.pending_throw = saved;
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
                Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
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
    pub(crate) fn iter_helper_next(&mut self, idx: u32) -> Result<Value, Thrown> {
        loop {
            let (source, kind, arg, n, cidx, done, inner, next) = match self.heap.get(idx) {
                HeapObj::IterHelper { source, kind, arg, n, idx, done, inner, next } => {
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
                        match self.iterator_step(inner)? {
                            Some(v) => return Ok(self.iter_result(v, false)),
                            None => {
                                self.ih_set_inner(idx, Value::UNDEFINED);
                                continue;
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
                            let it = self.get_iterator_flattenable(mapped)?;
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
                    // 5 = passthrough wrapper (Iterator.from of a foreign iterator):
                    // step via the cached next (GetIteratorDirect read it once).
                    match self.ih_step(source, next)? {
                        None => {
                            self.ih_set_done(idx);
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        Some(v) => return Ok(self.iter_result(v, false)),
                    }
                }
            }
        }
    }

    /// `Iterator.from(O)` — wrap an iterable/iterator as an Iterator Helper so it
    /// gains the helper methods.
    pub(crate) fn iterator_from(&mut self, o: Value) -> Result<Value, Thrown> {
        // A string yields its code-point iterator; otherwise get the iterable's
        // iterator (or use it directly if it is one).
        let it = self.get_iterator_flattenable(o)?;
        // If the iterator ALREADY inherits %Iterator.prototype% (OrdinaryHasInstance
        // (%Iterator%, it) — e.g. a generator or a built-in iterator), return it
        // unwrapped; only a foreign iterator gets the WrapForValidIterator wrapper.
        // Walk via object_get_prototype_of so a Generator's gen_proto link is followed
        // (generator instances resolve their [[Prototype]] specially, not via proto_of).
        if it.is_heap() && self.iterator_proto_root != 0 {
            let mut cur = it;
            for _ in 0..64 {
                let p = self.object_get_prototype_of(cur);
                if !p.is_heap() {
                    break;
                }
                if p.heap_index() == self.iterator_proto_root {
                    return Ok(it);
                }
                cur = p;
            }
        }
        self.make_iter_helper(it, 5, Value::UNDEFINED, 0)
    }
}
