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
        Ok(self.make_iter_helper(src, 6, Value::UNDEFINED, 0))
    }

    fn make_iter_helper(&mut self, source: Value, kind: u8, arg: Value, n: i64) -> Value {
        let idx = self.heap.alloc(HeapObj::IterHelper {
            source,
            kind,
            arg,
            n,
            idx: 0,
            done: false,
            inner: Value::UNDEFINED,
        });
        if self.iterator_helper_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.iterator_helper_proto));
        }
        Value::heap(idx)
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

    /// take()/drop() count: ToIntegerOrInfinity, clamped non-negative (NaN→0,
    /// +∞ → "all"); a negative or -∞ value is a RangeError.
    fn iter_limit_arg(&mut self, v: Value) -> Result<i64, Thrown> {
        let n = self.to_number(v)?;
        if n.is_nan() {
            return Ok(0);
        }
        if n < 0.0 {
            return Err(Thrown("RangeError: take/drop limit must be non-negative".into()));
        }
        if n.is_infinite() {
            return Ok(i64::MAX);
        }
        Ok(n as i64)
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
        if needs_fn && !self.is_callable(a0) {
            return Err(Thrown("TypeError: the callback argument is not a function".into()));
        }
        match id {
            ITER_MAP => Ok(self.make_iter_helper(this, 0, a0, 0)),
            ITER_FILTER => Ok(self.make_iter_helper(this, 1, a0, 0)),
            ITER_FLATMAP => Ok(self.make_iter_helper(this, 4, a0, 0)),
            ITER_TAKE => {
                let n = self.iter_limit_arg(a0)?;
                Ok(self.make_iter_helper(this, 2, Value::UNDEFINED, n))
            }
            ITER_DROP => {
                let n = self.iter_limit_arg(a0)?;
                Ok(self.make_iter_helper(this, 3, Value::UNDEFINED, n))
            }
            ITER_TOARRAY => {
                let mut out = Vec::new();
                while let Some(v) = self.iterator_step(this)? {
                    out.push(v);
                }
                Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
            }
            ITER_FOREACH => {
                let mut i = 0i64;
                while let Some(v) = self.iterator_step(this)? {
                    self.call_value(a0, Value::UNDEFINED, &[v, Value::num(i as f64)])?;
                    i += 1;
                }
                Ok(Value::UNDEFINED)
            }
            ITER_SOME => {
                let mut i = 0i64;
                while let Some(v) = self.iterator_step(this)? {
                    let r = self.call_value(a0, Value::UNDEFINED, &[v, Value::num(i as f64)])?;
                    if self.truthy(r) {
                        return Ok(Value::bool(true));
                    }
                    i += 1;
                }
                Ok(Value::bool(false))
            }
            ITER_EVERY => {
                let mut i = 0i64;
                while let Some(v) = self.iterator_step(this)? {
                    let r = self.call_value(a0, Value::UNDEFINED, &[v, Value::num(i as f64)])?;
                    if !self.truthy(r) {
                        return Ok(Value::bool(false));
                    }
                    i += 1;
                }
                Ok(Value::bool(true))
            }
            ITER_FIND => {
                let mut i = 0i64;
                while let Some(v) = self.iterator_step(this)? {
                    let r = self.call_value(a0, Value::UNDEFINED, &[v, Value::num(i as f64)])?;
                    if self.truthy(r) {
                        return Ok(v);
                    }
                    i += 1;
                }
                Ok(Value::UNDEFINED)
            }
            ITER_REDUCE => {
                if !self.is_callable(a0) {
                    return Err(Thrown("TypeError: reduce reducer is not a function".into()));
                }
                let has_init = args.len() >= 2;
                let mut acc = if has_init { args[1] } else { Value::UNDEFINED };
                let mut i = 0i64;
                if !has_init {
                    match self.iterator_step(this)? {
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
                while let Some(v) = self.iterator_step(this)? {
                    acc = self.call_value(a0, Value::UNDEFINED, &[acc, v, Value::num(i as f64)])?;
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
            let (source, kind, arg, n, cidx, done, inner) = match self.heap.get(idx) {
                HeapObj::IterHelper { source, kind, arg, n, idx, done, inner } => {
                    (*source, *kind, *arg, *n, *idx, *done, *inner)
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
                    match self.iterator_step(source)? {
                        None => {
                            self.ih_set_done(idx);
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        Some(v) => {
                            let mapped =
                                self.call_value(arg, Value::UNDEFINED, &[v, Value::num(cidx as f64)])?;
                            self.ih_inc_idx(idx);
                            return Ok(self.iter_result(mapped, false));
                        }
                    }
                }
                1 => {
                    // filter
                    match self.iterator_step(source)? {
                        None => {
                            self.ih_set_done(idx);
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        Some(v) => {
                            let keep =
                                self.call_value(arg, Value::UNDEFINED, &[v, Value::num(cidx as f64)])?;
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
                        self.ih_set_done(idx);
                        return Ok(self.iter_result(Value::UNDEFINED, true));
                    }
                    self.ih_set_n(idx, n - 1);
                    match self.iterator_step(source)? {
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
                        match self.iterator_step(source)? {
                            None => {
                                self.ih_set_done(idx);
                                return Ok(self.iter_result(Value::UNDEFINED, true));
                            }
                            Some(_) => nn -= 1,
                        }
                    }
                    self.ih_set_n(idx, 0);
                    match self.iterator_step(source)? {
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
                    match self.iterator_step(source)? {
                        None => {
                            self.ih_set_done(idx);
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        Some(v) => {
                            let mapped =
                                self.call_value(arg, Value::UNDEFINED, &[v, Value::num(cidx as f64)])?;
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
                    // 5 = passthrough wrapper (Iterator.from of a foreign iterator)
                    match self.iterator_step(source)? {
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
        Ok(self.make_iter_helper(it, 5, Value::UNDEFINED, 0))
    }
}
