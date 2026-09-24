//! The Python runtime's generator step, native (`__zipp_py_gen`).
//!
//! A Python generator is a runtime record (`rt.makeGenerator` in
//! `runtime/core.js`) around an engine generator (`js`). Its `next` member
//! is what every consumer calls for one step: a `for` loop's header, the
//! runtime's `fornext`, `next()`, `list()`, `sum()` and the rest. In a
//! Python program that member is this native; it performs `genNext` (the
//! runtime's JavaScript step, kept as the fallback and for every other
//! build) for the one state that matters for speed, a plain resume, and
//! hands every other state to `genNext` itself:
//!
//! * finished: `returned = null`, the STOP sentinel (as `genNext`);
//! * running (re-entered from its own body), or holding exceptions it was
//!   handling when it last yielded (`excs`), or a record or engine value of
//!   an unexpected shape: `genNext` runs, and raises or restores exactly as
//!   it always did;
//! * a plain resume: `running`/`started` set, the engine generator resumed,
//!   and then exactly `genNext`'s bookkeeping: the current-exception stack
//!   cut back to its depth (what the body left pushed at a yield becomes
//!   `excs`), `done`/`returned` on completion. An exception escaping the
//!   body cuts the stack back and is converted by the runtime's own
//!   `genEscape` (PEP 479), whose result is thrown.
//!
//! Nothing here runs guest code except through the resumed body and the
//! runtime functions named above.

use super::*;
use crate::heap::HeapObj;
use crate::value::Value;

/// The record's fields this step reads or writes, with the slot each has in
/// the record `makeGenerator` builds (the literal's key order; an `id` a
/// later `id()` adds goes after them).
const F_JS: (usize, &str) = (1, "js");
const F_DONE: (usize, &str) = (2, "done");
const F_STARTED: (usize, &str) = (3, "started");
const F_RUNNING: (usize, &str) = (4, "running");
const F_RETURNED: (usize, &str) = (5, "returned");
const F_EXCS: (usize, &str) = (6, "excs");
const F_GRT: (usize, &str) = (13, "grt");

thread_local! {
    /// The shape of a generator record whose fields above were all found at
    /// their usual slots, as data properties (`shape::DICT`, which never
    /// matches, until one is seen). Shapes are this thread's.
    static GEN_SHAPE: std::cell::Cell<u32> = const { std::cell::Cell::new(crate::shape::DICT) };
}

/// The runtime values the step needs (`grt`, one array shared by every
/// generator record): the JavaScript `genNext`, the current-exception
/// stack, the STOP sentinel and `genEscape`.
struct GenRuntime {
    gen_next: Value,
    exc_stack: u32,
    stop: Value,
    escape: Value,
}

impl<'p> Vm<'p> {
    /// The slot of the record's own data property `field` (its usual slot
    /// when the key is there, else a lookup).
    fn py_gen_slot(&self, idx: u32, field: (usize, &str)) -> Option<usize> {
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        let slot = if field.0 < m.len() && m.key_at(field.0) == field.1 {
            field.0
        } else {
            m.pos(field.1)?
        };
        (!m.attr_at(slot).accessor).then_some(slot)
    }

    fn py_gen_read(&self, idx: u32, slot: usize) -> Value {
        match self.heap.get(idx) {
            HeapObj::Object(m) => m.val_at(slot),
            _ => Value::UNDEFINED,
        }
    }

    fn py_gen_write(&mut self, idx: u32, slot: usize, v: Value) {
        if v.is_heap() {
            self.heap.write_barrier_val(idx, v);
        }
        if let HeapObj::Object(m) = self.heap.get_mut(idx) {
            m.set_val_at(slot, v);
        }
    }

    fn py_gen_runtime(&self, grt: Value) -> Option<GenRuntime> {
        if !grt.is_heap() {
            return None;
        }
        let HeapObj::Array(items) = self.heap.get(grt.heap_index()) else {
            return None;
        };
        let [gen_next, exc_stack, stop, escape] = items.get(..4)? else {
            return None;
        };
        if !exc_stack.is_heap() || !matches!(self.heap.get(exc_stack.heap_index()), HeapObj::Array(_)) {
            return None;
        }
        Some(GenRuntime {
            gen_next: *gen_next,
            exc_stack: exc_stack.heap_index(),
            stop: *stop,
            escape: *escape,
        })
    }

    fn py_exc_depth(&self, stack: u32) -> Option<usize> {
        match self.heap.get(stack) {
            HeapObj::Array(items) => Some(items.len()),
            _ => None,
        }
    }

    /// `__zipp_py_gen`, called as `g.next()`: one step of the Python
    /// generator record `this` (see the module comment).
    pub(crate) fn py_gen_next(&mut self, this: Value) -> Result<Value, Thrown> {
        if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::Object(m) if !m.is_ctor) {
            return Err(Thrown("TypeError: generator step on a non-generator".into()));
        }
        let g = this.heap_index();
        // A record of the shape already seen with every field at its usual
        // slot has them all there (a shape fixes keys, order and kinds).
        let known = match self.heap.get(g) {
            HeapObj::Object(m) => m.shape() != crate::shape::DICT && m.shape() == GEN_SHAPE.with(|c| c.get()),
            _ => false,
        };
        let rt = if known {
            self.py_gen_runtime(self.py_gen_read(g, F_GRT.0))
        } else {
            self.py_gen_slot(g, F_GRT)
                .and_then(|s| self.py_gen_runtime(self.py_gen_read(g, s)))
        };
        let Some(rt) = rt else {
            return Err(Thrown("TypeError: generator step on a non-generator".into()));
        };
        let slots = if known {
            Some((F_JS.0, F_DONE.0, F_STARTED.0, F_RUNNING.0, F_RETURNED.0, F_EXCS.0))
        } else {
            (|| {
                Some((
                    self.py_gen_slot(g, F_JS)?,
                    self.py_gen_slot(g, F_DONE)?,
                    self.py_gen_slot(g, F_STARTED)?,
                    self.py_gen_slot(g, F_RUNNING)?,
                    self.py_gen_slot(g, F_RETURNED)?,
                    self.py_gen_slot(g, F_EXCS)?,
                ))
            })()
        };
        if !known && slots == Some((F_JS.0, F_DONE.0, F_STARTED.0, F_RUNNING.0, F_RETURNED.0, F_EXCS.0)) && self.py_gen_slot(g, F_GRT) == Some(F_GRT.0) {
            if let HeapObj::Object(m) = self.heap.get(g) {
                if m.shape_guardable() {
                    let shape = m.shape();
                    GEN_SHAPE.with(|c| c.set(shape));
                }
            }
        }
        let Some((s_js, s_done, s_started, s_running, s_returned, s_excs)) = slots else {
            return self.call_value(rt.gen_next, this, &[]);
        };
        let done = self.py_gen_read(g, s_done);
        if done == Value::TRUE {
            self.py_gen_write(g, s_returned, Value::NULL);
            return Ok(rt.stop);
        }
        let js = self.py_gen_read(g, s_js);
        let plain = done == Value::FALSE
            && self.py_gen_read(g, s_running) == Value::FALSE
            && self.py_gen_read(g, s_excs) == Value::NULL
            && js.is_heap()
            && matches!(self.heap.get(js.heap_index()), HeapObj::Generator { .. });
        let depth = self.py_exc_depth(rt.exc_stack);
        let (true, Some(depth)) = (plain, depth) else {
            return self.call_value(rt.gen_next, this, &[]);
        };
        self.py_gen_write(g, s_running, Value::TRUE);
        if self.py_gen_read(g, s_started) != Value::TRUE {
            self.py_gen_write(g, s_started, Value::TRUE);
        }
        // The engine's `next` step, its iterator result not built.
        let step = self.generator_next_step(js.heap_index());
        let res = match step {
            Ok(Some(super::async_runtime::GenStep::Yield(v))) => Ok((v, false)),
            Ok(Some(super::async_runtime::GenStep::Done(v))) => Ok((v, true)),
            Ok(Some(super::async_runtime::GenStep::Raw(res))) => Err(res),
            Ok(None) => Err(Value::UNDEFINED),
            Err(t) => {
                // `catch (e) { excStack.length = depth; throw genEscape(g, e); }`
                let e = match self.pending_throw.take() {
                    Some(v) => v,
                    None => {
                        let v = self.alloc_error_from_message(&t.0);
                        self.realm_adopt_error(v);
                        v
                    }
                };
                self.py_exc_truncate(rt.exc_stack, depth)?;
                let converted = self.call_value(rt.escape, Value::UNDEFINED, &[this, e])?;
                self.pending_throw = Some(converted);
                return Err(Thrown(self.throw_message(converted)));
            }
        };
        let (value, finished) = match res {
            Ok(step) => step,
            Err(res) => match self.iter_result_unwrap(res) {
                Some((v, d)) => (v, self.truthy(d)),
                None => {
                    let d = self.get_prop(res, "done")?;
                    let v = self.get_prop(res, "value")?;
                    (v, self.truthy(d))
                }
            },
        };
        // `if (!r.done && excStack.length > depth) g.excs = excStack.splice(depth);
        //  else if (excStack.length !== depth) excStack.length = depth;`
        let now = self.py_exc_depth(rt.exc_stack).unwrap_or(depth);
        if !finished && now > depth {
            let kept = match self.heap.get(rt.exc_stack) {
                HeapObj::Array(items) => items[depth..].to_vec(),
                _ => Vec::new(),
            };
            let arr = self.heap.alloc(HeapObj::Array(kept));
            self.py_gen_write(g, s_excs, Value::heap(arr));
            self.py_exc_truncate(rt.exc_stack, depth)?;
        } else if now != depth {
            self.py_exc_truncate(rt.exc_stack, depth)?;
        }
        self.py_gen_write(g, s_running, Value::FALSE);
        if finished {
            // genFinish: `done = true; returned = r.value ?? null` (undefined only).
            self.py_gen_write(g, s_done, Value::TRUE);
            let r = if value.is_undefined() { Value::NULL } else { value };
            self.py_gen_write(g, s_returned, r);
            return Ok(rt.stop);
        }
        Ok(value)
    }

    /// `excStack.length = depth`, as the runtime writes it.
    fn py_exc_truncate(&mut self, stack: u32, depth: usize) -> Result<(), Thrown> {
        self.set_prop(Value::heap(stack), "length", Value::num(depth as f64), true)?;
        Ok(())
    }
}
