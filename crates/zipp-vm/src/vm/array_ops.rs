#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

#[cfg(feature = "safe-sandbox")]
const MAX_ARRAY_STRINGIFY_DEPTH: usize = 4;
#[cfg(not(feature = "safe-sandbox"))]
const MAX_ARRAY_STRINGIFY_DEPTH: usize = 1_024;

/// Compare two WTF-8 strings by UTF-16 code units (IsLessThan's order). The
/// shared prefix is skipped bytewise; from the code point where they first
/// differ, the units decide — so an astral character's lead surrogate orders
/// below U+E000..U+FFFF, and a lone surrogate by its own unit value.
fn wtf8_code_unit_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let Some(i) = a.iter().zip(b).position(|(x, y)| x != y) else {
        return a.len().cmp(&b.len());
    };
    if a[i] < 0x80 && b[i] < 0x80 {
        return a[i].cmp(&b[i]);
    }
    // Back up to the start of the code point holding the first difference;
    // the prefix is shared, so it starts at the same offset in both.
    let mut j = i;
    while j > 0 && (a[j] & 0xC0) == 0x80 {
        j -= 1;
    }
    crate::heap::wtf8_units_iter(&a[j..]).cmp(crate::heap::wtf8_units_iter(&b[j..]))
}

/// The three capture-free numeric arrow bodies used by the cross-engine WASM
/// workload.  This deliberately is not a general bytecode mini-interpreter:
/// recognition below admits only the exact compiler output, and every operand
/// miss falls back to the ordinary callback frame before doing any work.
#[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
#[derive(Clone, Copy)]
enum ArrayNumericCallback {
    MapDouble,
    FilterMod3,
    ReduceAdd,
}

#[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
impl ArrayNumericCallback {
    #[cfg(feature = "instrument")]
    #[inline(always)]
    const fn steps(self) -> i64 {
        match self {
            Self::MapDouble => 3,
            Self::FilterMod3 => 5,
            Self::ReduceAdd => 2,
        }
    }

    #[inline(always)]
    const fn registers(self) -> usize {
        match self {
            Self::MapDouble => 4,
            Self::FilterMod3 | Self::ReduceAdd => 5,
        }
    }
}

#[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
fn exact_array_numeric_callback_proto(
    proto: &crate::bytecode::FuncProto,
    expected: ArrayNumericCallback,
) -> bool {
    if !proto.upvalues.is_empty()
        || proto.rest_reg.is_some()
        || proto.arguments_reg.is_some()
        || proto.is_generator
        || proto.is_async
        || !proto.non_constructable
        || !proto.lexical_this
        || !proto.simple_params
        // The direct evaluator's capacity proof uses the canonical callback
        // window size.  A hoisted-but-runtime-empty local can leave the same
        // opcode body with a larger `reg_count`; reject it rather than skipping
        // that retained register allocation.
        || proto.reg_count as usize != expected.registers()
    {
        return false;
    }

    matches!(
        (
            expected,
            proto.param_count,
            proto.length,
            proto.code.as_slice(),
        ),
        (
            ArrayNumericCallback::MapDouble,
            1,
            1,
            [
                Instr::LoadInt { dst: 3, val: 2 },
                Instr::Mul { dst: 2, a: 1, b: 3 },
                Instr::Return { src: 2 },
            ],
        ) | (
            ArrayNumericCallback::FilterMod3,
            1,
            1,
            // B263 register classes: the boolean-valued compare is allocated
            // from its own class and renumbered after the numeric temporaries,
            // so the result lands in the last register of the five.
            [
                Instr::LoadInt { dst: 3, val: 3 },
                Instr::Mod { dst: 2, a: 1, b: 3 },
                Instr::LoadInt { dst: 3, val: 0 },
                Instr::Eq { dst: 4, a: 2, b: 3 },
                Instr::Return { src: 4 },
            ],
        ) | (
            ArrayNumericCallback::ReduceAdd,
            2,
            2,
            [Instr::Add { dst: 3, a: 1, b: 2 }, Instr::Return { src: 3 },],
        )
    )
}

#[cfg(all(test, not(all(feature = "jit", target_arch = "x86_64"))))]
mod array_numeric_callback_shape_tests {
    use super::*;

    fn callback_proto(source: &str) -> crate::bytecode::FuncProto {
        let ast = crate::front::parse_auto(source).expect("callback source parses");
        let program =
            crate::compile::compile_main_program(&ast, source).expect("callback source compiles");
        program
            .functions
            .last()
            .expect("nested callback proto")
            .clone()
    }

    #[test]
    fn exact_numeric_callback_classifier_is_fail_closed() {
        for (source, plan) in [
            (
                "let callback = x => x * 2;",
                ArrayNumericCallback::MapDouble,
            ),
            (
                "let callback = x => x % 3 === 0;",
                ArrayNumericCallback::FilterMod3,
            ),
            (
                "let callback = (p, c) => p + c;",
                ArrayNumericCallback::ReduceAdd,
            ),
        ] {
            assert!(
                exact_array_numeric_callback_proto(&callback_proto(source), plan),
                "should admit {source}"
            );
        }

        for (source, plan) in [
            (
                "let callback = x => 2 * x;",
                ArrayNumericCallback::MapDouble,
            ),
            (
                "let callback = x => { var unused; return x * 2; };",
                ArrayNumericCallback::MapDouble,
            ),
            (
                "let callback = (x, unused) => x * 2;",
                ArrayNumericCallback::MapDouble,
            ),
            (
                "let callback = function (x) { return x * 2; };",
                ArrayNumericCallback::MapDouble,
            ),
            (
                "let callback = (x = 1) => x * 2;",
                ArrayNumericCallback::MapDouble,
            ),
            (
                "let factor = 2; let callback = x => x * factor;",
                ArrayNumericCallback::MapDouble,
            ),
            (
                "let callback = async x => x * 2;",
                ArrayNumericCallback::MapDouble,
            ),
            (
                "let callback = x => 0 === x % 3;",
                ArrayNumericCallback::FilterMod3,
            ),
            (
                "let callback = x => x % 3 == 0;",
                ArrayNumericCallback::FilterMod3,
            ),
            (
                "let callback = (p, c) => c + p;",
                ArrayNumericCallback::ReduceAdd,
            ),
            (
                "let callback = (p, c, index) => p + c;",
                ArrayNumericCallback::ReduceAdd,
            ),
        ] {
            assert!(
                !exact_array_numeric_callback_proto(&callback_proto(source), plan),
                "should reject {source}"
            );
        }
    }

    #[test]
    fn direct_numeric_callback_ends_a_preceding_tail_reuse_streak() {
        let source = "";
        let ast = crate::front::parse_auto(source).expect("empty source parses");
        let program =
            crate::compile::compile_main_program(&ast, source).expect("empty source compiles");
        let mut vm = Vm::new(&program);

        // Model a callback reached from a frame installed by `try_tail_reuse`.
        // Seed enough frame/register backing storage that the direct callback
        // is admitted without an unrelated capacity-growth fallback.
        vm.frames.reserve(1);
        vm.regs.resize(5, Value::UNDEFINED);
        vm.regs.truncate(0);
        for (plan, a, b, expected) in [
            (
                ArrayNumericCallback::MapDouble,
                Value::int(7),
                Value::UNDEFINED,
                Value::int(14),
            ),
            (
                ArrayNumericCallback::ReduceAdd,
                Value::int(5),
                Value::int(7),
                Value::int(12),
            ),
        ] {
            vm.tail_reuse_streak = MAX_TAIL_REUSE_STREAK;
            assert_eq!(vm.try_array_numeric_callback(plan, a, b), Some(expected));
            assert_eq!(
                vm.tail_reuse_streak, 0,
                "an elided callback return must act like pop_frame_with"
            );
        }
    }
}

impl<'p> Vm<'p> {
    /// Recognise one exact, capture-free arrow callback.  Requiring both the
    /// immutable proto metadata and the live callable's empty upvalue vector
    /// keeps this fail-closed for defaults/rest/arguments/eval, generators,
    /// async functions, ordinary functions, and capturing closures.
    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    fn array_numeric_callback_plan(
        &self,
        cb: Value,
        expected: ArrayNumericCallback,
    ) -> Option<ArrayNumericCallback> {
        if !cb.is_heap() {
            return None;
        }
        let (fid, live_upvalues) = self.heap.as_callable(cb.heap_index())?;
        if !live_upvalues.is_empty() {
            return None;
        }
        exact_array_numeric_callback_proto(self.func(fid as usize), expected).then_some(expected)
    }

    /// Evaluate an admitted callback for already-numeric operands.  A finite
    /// meter must be able to pay for the COMPLETE tiny body before it runs; if
    /// not, the caller enters the real frame, which preserves the exact opcode
    /// at which exhaustion occurs.  Ordinary tracing/abort instrumentation also
    /// uses the real frame so it retains one trace row and abort poll per op.
    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    #[inline(always)]
    fn try_array_numeric_callback(
        &mut self,
        plan: ArrayNumericCallback,
        a: Value,
        b: Value,
    ) -> Option<Value> {
        // A real callback would reject at these boundaries before executing its
        // first opcode.  Also require its register window to fit the current
        // allocation: otherwise the ordinary call must get the chance to grow
        // (and memory-preflight) that storage.  After one fallback grows it,
        // later numeric elements may use the direct path.
        let needed = self.regs.len().checked_add(plan.registers())?;
        if self.frames.len() >= MAX_FRAMES
            || self.run_loop_depth >= MAX_RUN_LOOP_DEPTH
            // A real callback would push and later pop one Frame.  If that
            // push would grow the retained frame Vec, let the ordinary path do
            // it so heap accounting and a tight memory ceiling stay identical;
            // later elements can use the direct path once capacity exists.
            || self.frames.len() == self.frames.capacity()
            || needed > self.regs.capacity()
        {
            return None;
        }
        match plan {
            ArrayNumericCallback::MapDouble | ArrayNumericCallback::FilterMod3
                if !a.is_number() =>
            {
                return None;
            }
            ArrayNumericCallback::ReduceAdd if !a.is_number() || !b.is_number() => {
                return None;
            }
            _ => {}
        }

        #[cfg(all(feature = "instrument", not(feature = "meter-only")))]
        if self.instr_rec.is_some() {
            return None;
        }
        #[cfg(feature = "meter-only")]
        if let Some(rec) = self.instr_rec.as_ref() {
            let steps = plan.steps();
            if rec.exhaustion.is_some() || (rec.remaining != i64::MAX && rec.remaining < steps) {
                return None;
            }
        }

        let result = match plan {
            ArrayNumericCallback::MapDouble => {
                if a.is_int() {
                    match a.as_int().checked_mul(2) {
                        Some(n) => Value::int(n),
                        None => Value::num(a.as_int() as f64 * 2.0),
                    }
                } else {
                    Value::num(a.as_f64() * 2.0)
                }
            }
            ArrayNumericCallback::FilterMod3 => {
                let keep = if a.is_int() {
                    a.as_int() % 3 == 0
                } else {
                    a.as_f64() % 3.0 == 0.0
                };
                Value::bool(keep)
            }
            ArrayNumericCallback::ReduceAdd => {
                if a.is_int() && b.is_int() {
                    match a.as_int().checked_add(b.as_int()) {
                        Some(n) => Value::int(n),
                        None => Value::num(a.as_int() as f64 + b.as_int() as f64),
                    }
                } else {
                    Value::num(a.as_f64() + b.as_f64())
                }
            }
        };
        #[cfg(feature = "instrument")]
        self.charge_steps(plan.steps());
        // The real callback would return through `pop_frame_with`, ending any
        // preceding proper-tail-call reuse streak.  The direct callback must
        // preserve that safety state even though it elides the frame and pop.
        self.tail_reuse_streak = 0;
        Some(result)
    }

    /// Run an Array join/toString/toLocaleString operation with a shared active
    /// path. Self/mutual recursion contributes an empty element (the established
    /// Array join behaviour), while a deep acyclic graph fails before exhausting
    /// the native or WebAssembly stack. The closure guarantees cleanup on every
    /// ordinary error path.
    fn with_array_stringify_guard<F>(
        &mut self,
        idx: u32,
        stringify: F,
    ) -> Result<Option<Value>, Thrown>
    where
        F: FnOnce(&mut Self) -> Result<Option<Value>, Thrown>,
    {
        if self.array_stringify_active.contains(&idx) {
            return Ok(Some(Value::heap(crate::heap::INTERN_EMPTY)));
        }
        if self.array_stringify_active.len() >= MAX_ARRAY_STRINGIFY_DEPTH {
            return Err(Thrown(
                "RangeError: array stringification nesting limit exceeded".into(),
            ));
        }
        self.array_stringify_active
            .try_reserve(1)
            .map_err(|_| Thrown("RangeError: array stringification allocation failed".into()))?;
        self.array_stringify_active.push(idx);
        let result = stringify(self);
        let popped = self.array_stringify_active.pop();
        debug_assert_eq!(popped, Some(idx));
        result
    }

    /// Admit `additional` more bytes onto a guest-visible join buffer: the
    /// string byte cap, the host heap ceiling, a fallible reservation. The
    /// WTF-8 twin of `append_guest_string`'s checks.
    pub(crate) fn reserve_guest_wtf8(
        &mut self,
        out: &mut Vec<u8>,
        additional: usize,
    ) -> Result<(), Thrown> {
        let total = out
            .len()
            .checked_add(additional)
            .filter(|&n| n <= MAX_STRING_BYTES)
            .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
        #[cfg(feature = "instrument")]
        self.instrument_preflight_heap_growth(total)
            .map_err(|message| Thrown(message.into()))?;
        #[cfg(not(feature = "instrument"))]
        let _ = total;
        out.try_reserve(additional)
            .map_err(|_| Thrown("RangeError: string allocation failed".into()))
    }

    /// ToString(v) as WTF-8 bytes, lone surrogates intact. A string is copied
    /// as stored; an object goes through ToPrimitive first, so a `toString`
    /// returning a lone surrogate keeps it. `to_js_string` decodes to a Rust
    /// `String`, which turns each surrogate half into U+FFFD.
    pub(crate) fn to_wtf8_string(&mut self, v: Value) -> Result<Vec<u8>, Thrown> {
        let p = if self.is_object_value(v) {
            self.to_primitive_string(v)?
        } else {
            v
        };
        if p.is_heap() && self.heap.is_str_like(p.heap_index()) {
            return Ok(self
                .heap
                .str_wtf8_cow(p.heap_index())
                .map(|c| c.into_owned())
                .unwrap_or_default());
        }
        Ok(self.to_js_string(p)?.into_bytes())
    }

    /// Allocate a finished WTF-8 buffer (a join, or any builder that kept
    /// lone surrogates) as a string value.
    pub(crate) fn alloc_wtf8_str(&mut self, out: Vec<u8>) -> Value {
        Value::heap(self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out)))
    }

    /// Shared driver for `map`/`filter`/`forEach` (callback args = [element,
    /// index]). Uses the native callback fast path when the callback is a
    /// compiled non-capturing function: a single reused register window, a direct
    /// native call per element. Falls back to `call_value` per element otherwise.
    /// The window is always released (truncate) before returning — including on a
    /// callback error — so a thrown callback never leaks register slots.
    pub(crate) fn array_each(
        &mut self,
        idx: u32,
        cb: Value,
        mode: EachMode,
        this_arg: Value,
    ) -> Result<Option<Value>, Thrown> {
        // IsCallable(callback) precedes iteration: map/filter/forEach on an EMPTY
        // array with a non-callable callback must still throw TypeError.
        if !self.is_callable(cb) {
            let m = match mode {
                EachMode::Map => "map",
                EachMode::Filter => "filter",
                EachMode::ForEach => "forEach",
            };
            return Err(Thrown(format!("TypeError: {m} callback is not a function")));
        }
        // `out` (and, on the native-kernel path, the snapshot) hold values not
        // reachable from the GC roots while the callback re-enters the interpreter
        // — suspend GC for the scope.
        let _gc = self.gc_lock_guard();
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        let snapshot = self.array_snapshot(idx);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        let snapshot_len = snapshot.len();
        // The interpreter tail reads every element live below. It only needs the
        // initially captured length, so do not allocate and fill a throwaway Vec
        // on WASM/non-JIT builds. The callback-family routing gate admits only a
        // real, hole-free, non-overlaid, non-virtual dense Array here.
        #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
        let snapshot_len = match self.heap.get(idx) {
            HeapObj::Array(items) => items.len(),
            _ => 0,
        };
        // The receiver passed to the callback as its 3rd argument.
        let receiver = Value::heap(idx);
        // map/filter step 5: ArraySpeciesCreate(O, len | 0) runs BEFORE the first
        // callback, so the `constructor`/@@species Gets and the species
        // constructor observe the array as it was; each result element is then
        // defined on the target as it is produced. Only a CUSTOM species yields a
        // target — the ordinary-array answer (`None`) keeps the dense `out` Vec,
        // so the hot path is unchanged.
        let species_target = match mode {
            EachMode::Map => self.array_species_create(receiver, snapshot_len)?,
            EachMode::Filter => self.array_species_create(receiver, 0)?,
            EachMode::ForEach => None,
        };
        // The fused kernels inline the callback over (element, index) only and run
        // with `this`=undefined, so they cannot honour a thisArg, a 3rd "array"
        // parameter, or `arguments`. Disable them when the callback could observe
        // any of those (the per-element path below handles every case correctly).
        // A species target also rules them out: they fill `out` densely instead of
        // running CreateDataPropertyOrThrow per element.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        let kernel_ok = this_arg.is_undefined() && species_target.is_none();
        let collect = matches!(mode, EachMode::Map | EachMode::Filter);
        let mut out: Vec<Value> = if collect {
            Vec::with_capacity(snapshot_len)
        } else {
            Vec::new()
        };
        #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
        let numeric_callback = if species_target.is_none() {
            match mode {
                EachMode::Map => {
                    self.array_numeric_callback_plan(cb, ArrayNumericCallback::MapDouble)
                }
                EachMode::Filter => {
                    self.array_numeric_callback_plan(cb, ArrayNumericCallback::FilterMod3)
                }
                EachMode::ForEach => None,
            }
        } else {
            // A custom species target can allocate a property descriptor for
            // every result element between callback invocations.  Real callback
            // dispatch supplies the periodic heap-audit ticks that bound that
            // growth; an off-loop direct callback would not.
            None
        };

        // Fused native map kernel: inline the callback into a native loop over
        // the snapshot for the leading run of integer elements — eliminating the
        // per-element call boundary (the gap to V8, which inlines callbacks). Map
        // only (dense, ordered store). On a type-guard bail the kernel returns
        // the index it reached, having written results `[0, start)`; the
        // per-element loop below finishes `[start, len)` correctly (handling
        // doubles/strings/etc.), so a mixed array can never give a wrong answer.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        let mut start = 0usize;
        #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
        let start = 0usize;
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if matches!(mode, EachMode::Map)
            && kernel_ok
            && self.jit_fused_ok()
            && self.jit_recurse_depth == 0
            && cb.is_heap()
            && snapshot_len <= i32::MAX as usize
        {
            if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                if ups.is_empty() {
                    let proto: *const crate::bytecode::FuncProto = self.func(fid as usize);
                    // SAFETY: program functions are immutable during execution;
                    // the raw ptr dodges the self.jit (&mut) vs self.program (&)
                    // borrow conflict (same pattern as native_cb_entry).
                    let proto_ref = unsafe { &*proto };
                    let min_window = if proto_ref.param_count >= 2 { 3 } else { 2 };
                    let reg_count = (proto_ref.reg_count as usize).max(min_window);
                    // A callback that declares the 3rd (array) param or uses
                    // `arguments` must see the receiver — not the kernel's path.
                    let kernel_entry =
                        if proto_ref.param_count >= 3 || proto_ref.arguments_reg.is_some() {
                            None
                        } else {
                            self.jit.map_kernel(fid, proto_ref)
                        };
                    if let Some(entry) = kernel_entry {
                        let win = self.regs.len();
                        if !self.regs_would_overflow(win + reg_count) {
                            self.regs.resize(win + reg_count, Value::UNDEFINED);
                            let len = snapshot_len;
                            let window_ptr = unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
                            let snap_ptr = snapshot.as_ptr() as *const u64;
                            let out_ptr = out.as_mut_ptr() as *mut u64;
                            // SAFETY: `entry` is a valid win64 map kernel; the
                            // window holds `reg_count` slots; `out` has capacity
                            // `len` ≥ the returned count; the kernel is call-free
                            // so none of these pointers move during the call.
                            let kernel: extern "win64" fn(
                                *mut u64,
                                *const u64,
                                usize,
                                *mut u64,
                            ) -> usize = unsafe { core::mem::transmute(entry) };
                            let processed = kernel(window_ptr, snap_ptr, len, out_ptr);
                            // The kernel wrote `out[0..processed]` densely.
                            unsafe { out.set_len(processed) };
                            self.regs.truncate(win);
                            start = processed;
                        }
                    }
                }
            }
        }

        // Fused native filter kernel: inline the predicate over the snapshot for
        // the leading numeric run, compacting kept elements into `out`. The
        // predicate result must be a Bool (a comparison); a non-Bool result bails
        // that element to the per-element tail (which evaluates JS truthiness).
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if matches!(mode, EachMode::Filter)
            && kernel_ok
            && self.jit_fused_ok()
            && self.jit_recurse_depth == 0
            && cb.is_heap()
            && snapshot_len <= i32::MAX as usize
        {
            if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                if ups.is_empty() {
                    let proto: *const crate::bytecode::FuncProto = self.func(fid as usize);
                    // SAFETY: as the map branch above.
                    let proto_ref = unsafe { &*proto };
                    let min_window = if proto_ref.param_count >= 2 { 3 } else { 2 };
                    let reg_count = (proto_ref.reg_count as usize).max(min_window);
                    // Skip the kernel when the predicate could observe the 3rd
                    // (array) param or `arguments` (see the map branch).
                    let kernel_entry =
                        if proto_ref.param_count >= 3 || proto_ref.arguments_reg.is_some() {
                            None
                        } else {
                            self.jit.filter_kernel(fid, proto_ref)
                        };
                    if let Some(entry) = kernel_entry {
                        let win = self.regs.len();
                        if !self.regs_would_overflow(win + reg_count) {
                            self.regs.resize(win + reg_count, Value::UNDEFINED);
                            let len = snapshot_len;
                            let window_ptr = unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
                            let snap_ptr = snapshot.as_ptr() as *const u64;
                            let out_ptr = out.as_mut_ptr() as *mut u64;
                            let mut kept: usize = 0;
                            // SAFETY: valid win64 filter kernel; window has
                            // reg_count slots; `out` capacity `len` ≥ kept; the
                            // kernel is call-free so the pointers don't move.
                            let kernel: extern "win64" fn(
                                *mut u64,
                                *const u64,
                                usize,
                                *mut u64,
                                *mut usize,
                            ) -> usize = unsafe { core::mem::transmute(entry) };
                            let scanned =
                                kernel(window_ptr, snap_ptr, len, out_ptr, &mut kept as *mut usize);
                            // The kernel wrote `kept` elements into `out[0..kept]`.
                            unsafe { out.set_len(kept) };
                            self.regs.truncate(win);
                            start = scanned;
                        }
                    }
                }
            }
        }

        // Per-element path for `[start, len)` — the whole array when no kernel
        // ran, or just the tail after a kernel bail (or nothing if it completed).
        let run_tail = start < snapshot_len;
        let mut native = if run_tail {
            self.native_cb_entry(cb)
        } else {
            None
        };
        let win = self.regs.len();
        if let Some((_, callee_regs, _)) = native {
            if self.regs_would_overflow(win + callee_regs) {
                native = None; // can't fit a window → interpreter path
            } else {
                self.regs.resize(win + callee_regs, Value::UNDEFINED);
            }
        }

        let mut err = None;
        // The next index to write on the result: `out.len()` when buffering, and
        // the same count when a kernel already filled the head of `out`.
        let mut n = out.len();
        for i in start..snapshot_len {
            // Live read (the callback may have mutated this element or shortened the
            // array): a present index uses its current value; an index now past the
            // live length is absent — `map` skips it but still advances the result
            // index (the gap stays a hole), `filter`/`forEach` skip it entirely.
            let v = match self.array_dense_or_proto_get(idx, i)? {
                Some(v) => v,
                None => {
                    if matches!(mode, EachMode::Map) {
                        if species_target.is_none() {
                            out.push(Value::HOLE);
                        }
                        n += 1;
                    }
                    continue;
                }
            };
            let direct = {
                #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
                {
                    numeric_callback
                        .and_then(|plan| self.try_array_numeric_callback(plan, v, Value::UNDEFINED))
                }
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                {
                    None
                }
            };
            let callback_result = match direct {
                Some(value) => Ok(value),
                None => {
                    let args = [v, Value::int(i as i32), receiver];
                    self.run_cb_elem(native, win, cb, &args, this_arg)
                }
            };
            match callback_result {
                Ok(r) => {
                    let keep = match mode {
                        EachMode::Map => Some(r),
                        EachMode::Filter => {
                            if self.truthy(r) {
                                Some(v)
                            } else {
                                None
                            }
                        }
                        EachMode::ForEach => None,
                    };
                    if let Some(val) = keep {
                        match species_target {
                            Some(a) => {
                                if let Err(e) = self.create_data_property_or_throw(a, n, val) {
                                    err = Some(e);
                                    break;
                                }
                            }
                            None => out.push(val),
                        }
                        n += 1;
                    }
                }
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        if native.is_some() {
            self.regs.truncate(win); // release the reused window (success or error)
        }
        if let Some(e) = err {
            return Err(e);
        }
        match mode {
            EachMode::ForEach => Ok(Some(Value::UNDEFINED)),
            _ => Ok(Some(match species_target {
                Some(a) => a,
                // ArraySpeciesCreate's ArrayCreate fallback allocates in the
                // CURRENT realm — for a built-in that is the built-in's OWN
                // realm, so `otherRealm.a.map(f)` returns an array whose
                // prototype is the other realm's (sm/Array/species.js line 156).
                None => self.alloc_array_current_realm(out),
            })),
        }
    }

    /// Allocate a built-in iterator over a snapshot of `items` with prototype `proto`.
    pub(crate) fn make_iterator(&mut self, items: Vec<Value>, proto: u32) -> Value {
        let main_proto = proto;
        let proto = self.native_home(main_proto);
        let idx = self.heap.alloc(HeapObj::Iterator {
            items,
            index: 0,
            proto,
            live: None,
        });
        if proto != main_proto {
            if let Some(r) = self.native_callee_realm {
                self.obj_realm.insert(idx, r);
            }
        }
        Value::heap(idx)
    }

    /// Allocate a LIVE Map/Set iterator over the backing collection `coll` (its heap
    /// index) with prototype `proto`. `kind`: 0 = keys, 1 = values, 2 = entries. Each
    /// `.next()` steps the live collection (skipping deleted/tombstoned slots), so a
    /// delete/add performed after the iterator is created is reflected.
    pub(crate) fn make_live_iterator(&mut self, coll: u32, kind: u8, proto: u32) -> Value {
        let main_proto = proto;
        let proto = self.native_home(main_proto);
        let idx = self.heap.alloc(HeapObj::Iterator {
            items: Vec::new(),
            index: 0,
            proto,
            live: Some((coll, kind)),
        });
        if proto != main_proto {
            if let Some(r) = self.native_callee_realm {
                self.obj_realm.insert(idx, r);
            }
        }
        Value::heap(idx)
    }

    /// ArraySpeciesCreate(originalArray, length), but returns `None` to signal that
    /// the caller should keep its existing fast dense-array path — the constructor is
    /// the intrinsic `%Array%`, is absent/undefined, or carries no custom `@@species`.
    /// `Some(target)` is a species-constructed object the caller must populate with
    /// CreateDataPropertyOrThrow. A non-object (non-undefined) constructor, or a
    /// non-constructor `@@species`, throws a TypeError (matching the spec step
    /// "If IsConstructor(C) is false, throw a TypeError exception").
    pub(crate) fn array_species_create(
        &mut self,
        original: Value,
        len: usize,
    ) -> Result<Option<Value>, Thrown> {
        // ArraySpeciesCreate step 1-2: if IsArray(originalArray) is false, return
        // ArrayCreate(length) — `constructor`/`@@species` are NOT consulted for a
        // non-array receiver (e.g. `Array.prototype.map.call(typedArray | plainObj)`).
        if !self.value_is_array_throwing(original)? {
            return Ok(None);
        }
        let ctor = self.get_prop(original, "constructor")?;
        let species = if ctor == Value::UNDEFINED {
            return Ok(None);
        } else if !self.is_object_value(ctor) {
            // A non-object, non-undefined `constructor` can never be a constructor,
            // so ArraySpeciesCreate reaches the IsConstructor(C)-false throw.
            return Err(Thrown(
                "TypeError: Array species constructor is not an object".into(),
            ));
        } else {
            // ArraySpeciesCreate step 6.c.i: a constructor that is ANOTHER realm's
            // %Array% intrinsic is treated as undefined — so its @@species getter is
            // NOT consulted (cross-realm).
            if self.is_constructor(ctor)
                && self.get_function_realm(ctor) != 0
                && self.realm_ctor_main.get(&ctor.heap_index()) == Some(&self.array_ctor)
            {
                return Ok(None);
            }
            let s = self.get_prop(ctor, "@@species")?;
            if s == Value::NULL {
                Value::UNDEFINED
            } else {
                s
            }
        };
        if species == Value::UNDEFINED {
            return Ok(None);
        }
        // `%Array%` itself as the species is observably identical to ArrayCreate(len),
        // so keep the fast dense path (and avoid running the Array constructor).
        if species.is_heap() && self.array_ctor != 0 && species.heap_index() == self.array_ctor {
            return Ok(None);
        }
        if !self.is_constructor(species) {
            return Err(Thrown(
                "TypeError: Array species constructor is not a constructor".into(),
            ));
        }
        Ok(Some(self.construct(species, &[Value::num(len as f64)])?))
    }

    /// Append one concat element at the running index `n`: define it on the
    /// species target immediately (its [[DefineOwnProperty]] is observable), or
    /// buffer it for the dense ArrayCreate result. A HOLE marks an ABSENT source
    /// index — it is never defined, but `n` still advances so the gap survives.
    fn concat_emit(
        &mut self,
        target: Option<Value>,
        out: &mut Vec<Value>,
        n: &mut usize,
        v: Value,
    ) -> Result<(), Thrown> {
        match target {
            Some(a) => {
                if !v.is_hole() {
                    self.create_data_property_or_throw(a, *n, v)?;
                }
            }
            None => out.push(v),
        }
        *n += 1;
        Ok(())
    }

    /// CreateDataPropertyOrThrow(O, ToString(index), value): install a fresh
    /// enumerable, writable, configurable data property (overwriting a configurable
    /// existing one), throwing a TypeError when the define fails — a non-extensible
    /// target, or a non-configurable existing property.
    pub(crate) fn create_data_property_or_throw(
        &mut self,
        target: Value,
        index: usize,
        value: Value,
    ) -> Result<(), Thrown> {
        let mut m = ObjMap::new();
        m.set("value", value);
        m.set("writable", Value::TRUE);
        m.set("enumerable", Value::TRUE);
        m.set("configurable", Value::TRUE);
        let desc = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))));
        let key = index.to_string();
        self.object_define_property(target, &key, desc)
    }

    /// Finish a species-aware `Array.prototype` method: build the result array from
    /// `out`, honouring a custom `@@species` constructor. `species_len` is the length
    /// ArraySpeciesCreate is invoked with (0 for filter/concat/flat/flatMap, the
    /// source length for map, the element count for slice/splice). The common
    /// ordinary-array case takes the fast dense path unchanged; only a custom species
    /// constructs a target and receives each element via CreateDataPropertyOrThrow.
    /// GC is suspended for the scope so `out`'s values survive the species ctor call.
    ///
    /// `set_length`: slice/splice end with Set(A,'length',n,true) per spec;
    /// map/filter only define elements.
    pub(crate) fn array_from_species_len(
        &mut self,
        original: Value,
        out: Vec<Value>,
        species_len: usize,
        set_length: bool,
    ) -> Result<Value, Thrown> {
        let _gc = self.gc_lock_guard();
        match self.array_species_create(original, species_len)? {
            // ArrayCreate in the CURRENT realm — see alloc_array_current_realm.
            None => Ok(self.alloc_array_current_realm(out)),
            Some(target) => {
                let n = out.len();
                for (i, v) in out.into_iter().enumerate() {
                    // A HOLE marks an ABSENT source index: it is skipped (not
                    // defined as undefined) — the result keeps the gap.
                    if !v.is_hole() {
                        self.create_data_property_or_throw(target, i, v)?;
                    }
                }
                if set_length {
                    self.set_prop(target, "length", Value::num(n as f64), true)?;
                }
                Ok(target)
            }
        }
    }

    /// FlattenIntoArray (23.1.3.13.1): walk `source` per index with the spec
    /// HasProperty+Get protocol, applying `mapper` at the TOP level only
    /// (flatMap), spreading array elements (proxy-piercing IsArray) up to
    /// `depth` levels. Absent indices are SKIPPED (the mapper never runs on a
    /// hole and nothing is appended for it).
    fn flatten_into_array(
        &mut self,
        out: &mut Vec<Value>,
        source: Value,
        source_len: usize,
        depth: i64,
        mapper: Option<(Value, Value)>,
    ) -> Result<(), Thrown> {
        let mut work = 0u64;
        self.flatten_into_array_at(out, source, source_len, depth, mapper, 0, &mut work)
    }

    /// `flat` / `flatMap`: FlattenIntoArray into `target` (the ArraySpeciesCreate
    /// result, or `None` for a plain current-realm array).
    ///
    /// The receiver may be a Rust-owned apply argument and a species `target` a
    /// fresh constructor result, so both stay on the host-root stack throughout.
    ///
    /// The walk itself collects into a Rust `Vec` — not a GC root — while
    /// element getters, Proxy traps and the mapper run, so it holds the GC lock
    /// instead of rooting each value: rooting them would hold every element
    /// TWICE, and `[bigArray, bigArray, …].flat()` then needs twice the
    /// memory, which trapped the WASM instance at its linear-memory wall
    /// before the heap ceiling could refuse it (the accounting in
    /// `reserve_array_result` sizes one copy). A species target receives the
    /// elements only afterwards, through CreateDataPropertyOrThrow — a setter
    /// or a defineProperty trap, which must be able to collect — so the
    /// collected block becomes a real array first and is handed over from
    /// there, reachable the whole time.
    fn flatten_into_array_result(
        &mut self,
        source: Value,
        source_len: usize,
        depth: i64,
        mapper: Option<(Value, Value)>,
        target: Option<Value>,
    ) -> Result<Value, Thrown> {
        let target_root = target.unwrap_or(Value::UNDEFINED);
        self.with_host_roots(&[source, target_root], |vm| {
            // The lock spans the allocation too: until the block is inside a
            // heap object, a collection there would see no reference to it.
            let built = {
                let _gc = vm.gc_lock_guard();
                let mut out = Vec::new();
                vm.flatten_into_array(&mut out, source, source_len, depth, mapper)?;
                vm.alloc_array_current_realm(out)
            };
            let Some(a) = target else { return Ok(built) };
            vm.with_host_roots(&[built], |vm| {
                let n = match vm.heap.get(built.heap_index()) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                for i in 0..n {
                    let v = match vm.heap.get(built.heap_index()) {
                        HeapObj::Array(items) => items[i],
                        _ => Value::UNDEFINED,
                    };
                    vm.create_data_property_or_throw(a, i, v)?;
                }
                Ok(a)
            })
        })
    }

    /// Is every `HasProperty`+`Get` the flatten walk does on `v` exactly "the
    /// dense slot at that index, when it is not a hole"? True only for a real
    /// Array (an arguments object records a `proto_of`) with no side table to
    /// shadow an element or `length`, its default [[Prototype]], and no integer
    /// key on the shared prototypes — the same proof `array_iter_get`'s two
    /// fast paths make per call. Re-proved after anything that can run guest
    /// code, since that code can add any of them.
    fn flatten_dense(&self, v: Value) -> bool {
        crate::codegen::hole_absent_fast_enabled()
            && !self.array_proto_has_index
            && v.is_heap()
            && !self.arr_props.contains_key(&v.heap_index())
            && !self.proto_of.contains_key(&v.heap_index())
            && matches!(self.heap.get(v.heap_index()), HeapObj::Array(_))
    }

    fn flatten_into_array_at(
        &mut self,
        out: &mut Vec<Value>,
        source: Value,
        source_len: usize,
        depth: i64,
        mapper: Option<(Value, Value)>,
        active_depth: u32,
        work: &mut u64,
    ) -> Result<(), Thrown> {
        *work = work
            .checked_add(source_len as u64)
            .ok_or_else(|| Thrown("RangeError: native builtin iteration limit exceeded".into()))?;
        self.preflight_native_iteration_work(*work)?;
        // Hoisted out of the loop: `array_iter_get`'s per-element side-table
        // probe is the whole cost of a plain `[1,2,3].flat()`.
        let mut dense = self.flatten_dense(source);
        let sidx = if source.is_heap() { source.heap_index() } else { 0 };
        // A plain array flattened no further, with every index present: its
        // elements go in as one block. (Every Get and HasProperty the walk
        // below would do is unobservable here.)
        if depth <= 0 && mapper.is_none() && dense {
            let clean = matches!(self.heap.get(sidx), HeapObj::Array(items)
                    if items.len() == source_len && !items.iter().any(|v| v.is_hole()));
            if clean {
                self.reserve_array_result(out, source_len, false)?;
                if let HeapObj::Array(items) = self.heap.get(sidx) {
                    out.extend_from_slice(items);
                }
                return Ok(());
            }
        }
        for k in 0..source_len {
            let got = if dense {
                match self.heap.get(sidx) {
                    HeapObj::Array(items) => items.get(k).copied().filter(|v| !v.is_hole()),
                    _ => None,
                }
            } else {
                self.array_iter_get(source, k)?
            };
            let Some(got) = got else {
                continue;
            };
            let v = match mapper {
                Some((cb, ta)) => {
                    let r = self.call_value(cb, ta, &[got, Value::num(k as f64), source])?;
                    dense = self.flatten_dense(source);
                    r
                }
                None => got,
            };
            // `is_heap` first: IsArray is a chain walk, and the elements of a
            // numeric array are never spreadable.
            if depth > 0 && v.is_heap() && self.value_is_array_throwing(v)? {
                // A mapper-made nested array is only a Rust local while its
                // own length/element Gets run.
                self.push_host_root(v);
                // A plain Array's `length` is an own non-configurable data
                // property no overlay redefines, so `js_array_len` IS its Get.
                // (The walk's work budget bounds how much of it is read.)
                let n = if self.flatten_dense(v) {
                    self.js_array_len(v.heap_index())
                } else {
                    let lv = self.get_prop(v, "length")?;
                    let lf = self.to_number_strict(lv)?;
                    if lf.is_nan() || lf <= 0.0 {
                        0usize
                    } else {
                        lf.trunc().min(9_007_199_254_740_991.0) as usize
                    }
                };
                let next_depth = active_depth.checked_add(1).ok_or_else(|| {
                    Thrown("RangeError: array flattening nesting limit exceeded".into())
                })?;
                #[cfg(feature = "safe-sandbox")]
                if next_depth > 64 {
                    return Err(Thrown(
                        "RangeError: array flattening nesting limit exceeded".into(),
                    ));
                }
                self.flatten_into_array_at(out, v, n, depth - 1, None, next_depth, work)?;
                // The nested walk's Gets can be Proxy traps or accessors.
                dense = self.flatten_dense(source);
            } else {
                // Admit the result's growth before the store reallocates: its
                // size is the sum of the elements visited, which for an array
                // of N references to one big array is N times that array.
                self.reserve_array_result(out, 1, false)?;
                out.push(v);
            }
        }
        Ok(())
    }

    /// Live read of index `k` for the iteration protocol: `Some(value)` if the
    /// index is PRESENT (HasProperty), `None` if absent (a hole / out of range).
    /// Re-reads the receiver each call so a mutation during a callback (a deleted
    /// index, a shrunk length, a changed element) is observed. The common case — a
    /// real array with no side table — reads the dense slot directly (no get_index/
    /// has_property dispatch), keeping the iterator methods at dense-snapshot speed;
    /// an array-like object, or an array with accessor/override indices, falls back
    /// to the general HasProperty + Get protocol (invoking inherited/accessor getters).
    pub(crate) fn array_iter_get(
        &mut self,
        this: Value,
        k: usize,
    ) -> Result<Option<Value>, Thrown> {
        // Fast path: only a PRESENT (non-hole, in-range) own element of a real array
        // with no side table. A hole or out-of-range index is NOT resolved here — it
        // falls through to the general HasProperty+Get protocol below, which walks the
        // prototype chain (a prototype-inherited index at a hole must still be visited).
        if this.is_heap() && !self.arr_props.contains_key(&this.heap_index()) {
            if let HeapObj::Array(items) = self.heap.get(this.heap_index()) {
                if let Some(v) = items.get(k) {
                    if !v.is_hole() {
                        return Ok(Some(*v));
                    }
                }
            }
        }
        // W19 (M1): ABSENT-INDEX FAST ANSWER — the other half of the fast path
        // above. A hole (or an index past the dense end) is PROVABLY absent when
        // nothing in the receiver's chain can supply an integer index, and then
        // `has_property_dyn` below can only return `false` — after formatting the
        // index into a fresh `String`, probing `arr_props`, and walking the
        // prototype chain. That walk is the per-hole cost every element-visiting
        // builtin pays: measured 64.5 ns/hole through `concat` on a 4096-element
        // array (0 holes 12.3 µs/call → 2048 holes 144.3 µs/call, dead linear),
        // against node's flat 1.0 µs. One hole anywhere also flips the whole
        // receiver off `concat`'s bulk-copy arm (:2001) and `slice`'s (:2105), so
        // this predicate is what those two fall back ONTO.
        //
        // The predicate is `has_property_jit`'s, verbatim (values.rs:1049-1062),
        // and the invalidation story is that one's:
        //   * `array_proto_has_index` is the sticky indexed-prototype protector
        //     (vm/mod.rs:743-762) — set, never cleared, by `note_array_proto_index`
        //     (props/array_len.rs:376) when an integer-like key is defined on
        //     Array.prototype/Object.prototype, and by
        //     `invalidate_indexed_proto_protector` when either is re-prototyped.
        //     It is read PER CALL here, not cached in compiled code, so a program
        //     that invalidates it mid-run sees the very next element visit take
        //     the full protocol.
        //   * `proto_of` excludes a `setPrototypeOf`'d receiver — and every
        //     arguments object, which always records one (values.rs:1385-1387),
        //     so its mapped/`length` shapes never reach here.
        //   * `array_elements_overlaid` (props/array_len.rs:58-60) excludes any
        //     receiver whose side table can shadow an ELEMENT — a `defineProperty`'d
        //     accessor or non-default index, a sparse overlay, and (via
        //     `overlays_elements`, heap.rs:592-594) a sealed/frozen/non-extensible
        //     array. A RegExp match result answers `false` there, correctly: its
        //     `index`/`input`/`groups` cannot shadow an index.
        // A Proxy receiver, or a Proxy on the chain, is not a `HeapObj::Array` and
        // is not reached — the `has` trap below still runs and may still throw.
        if crate::codegen::hole_absent_fast_enabled()
            && !self.array_proto_has_index
            && this.is_heap()
        {
            let idx = this.heap_index();
            if !self.proto_of.contains_key(&idx) && !self.array_elements_overlaid(idx) {
                if let HeapObj::Array(items) = self.heap.get(idx) {
                    if items.get(k).map_or(true, |v| v.is_hole()) {
                        return Ok(None);
                    }
                }
            }
        }
        let kv = Value::num(k as f64);
        // Proxy-aware HasProperty (a has trap must dispatch and may throw).
        if self.has_property_dyn(this, kv)? {
            Ok(Some(self.get_index(this, kv)?))
        } else {
            Ok(None)
        }
    }

    /// A snapshot that resolves every index through the `[[Get]]` protocol, so a hole
    /// reads its PROTOTYPE-inherited value (or `undefined` when truly absent) exactly as
    /// `fromValue = Get(O, from)` requires. The change-by-copy methods (toReversed /
    /// toSorted / toSpliced / with) build a dense result this way, so a hole over a
    /// `Array.prototype[k]` is not silently dropped to `undefined`. The fast path inside
    /// `array_iter_get` keeps a dense, side-table-free array at plain-snapshot speed; the
    /// length is read once up front (per LengthOfArrayLike) so a getter that mutates the
    /// array mid-read still yields exactly `len` elements.
    pub(crate) fn array_snapshot_get(&mut self, idx: u32) -> Result<Vec<Value>, Thrown> {
        let len = match self.heap.get(idx) {
            HeapObj::Array(items) => items.len(),
            _ => return Ok(Vec::new()),
        };
        let this = Value::heap(idx);
        let mut out = Vec::with_capacity(len);
        for k in 0..len {
            out.push(self.array_iter_get(this, k)?.unwrap_or(Value::UNDEFINED));
        }
        Ok(out)
    }

    /// Live per-index read for the DENSE callback arms (a real array known to have no
    /// side table at dispatch): a present (non-hole, in-range) element is returned
    /// directly — no per-element side-table lookup, so the hot path stays at snapshot
    /// speed — while a hole or out-of-range index defers to the proto-aware
    /// `array_iter_get` (which visits a prototype-inherited index). Re-reads the heap
    /// each call, so a callback's mid-iteration mutation (delete / length change) is
    /// observed.
    pub(crate) fn array_dense_or_proto_get(
        &mut self,
        idx: u32,
        i: usize,
    ) -> Result<Option<Value>, Thrown> {
        if let HeapObj::Array(items) = self.heap.get(idx) {
            if let Some(v) = items.get(i) {
                if !v.is_hole() {
                    return Ok(Some(*v));
                }
            }
        }
        self.array_iter_get(Value::heap(idx), i)
    }

    /// The hole-skipping iteration methods (forEach/map/filter/some/every/reduce/
    /// reduceRight) run against an array-like *object* OR a real array by visiting
    /// only indices where HasProperty is true (via `array_iter_get`) — unlike the
    /// dense-snapshot path, this honours absent indices (own or inherited holes) and
    /// observes mid-iteration mutation, per the spec.
    pub(crate) fn array_like_iterate(
        &mut self,
        this: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let _gc = self.gc_lock_guard();
        // O = ToObject(this value): a primitive receiver (e.g. a string passed via
        // `Array.prototype.forEach.call("abc", …)`) is boxed, so iteration reads the
        // wrapper's indexed properties AND the callback's 3rd argument is the object
        // (`obj instanceof String`), per every method's step 1.
        let this = self.to_object(this)?;
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_strict(lv)?;
        // ToLength. The ascending probe loops MUST use the full length: this is
        // the path every hole-sensitive callback method takes for an array with
        // a sparse overlay (elements past `MAX_DENSE_ARRAY_LEN` are stored in
        // `arr_props`, not the dense `items` Vec), so clamping the bound to
        // `MAX_DENSE_ARRAY_LEN` silently skipped every element beyond 2^20 —
        // `some()` returned false while `find()` found a match on the SAME
        // array, and `filter()` dropped the tail. Probing an absent index is
        // just a miss, so the full bound is also what V8 does (measurably: it
        // is O(length) on a sparse array too).
        // (`as usize` on an f64 saturates, so a huge/­infinite length cannot wrap.)
        let len: usize = if lenf > 0.0 { lenf as usize } else { 0 };
        let full_len: u64 = if lenf > 0.0 {
            lenf.trunc().min(9_007_199_254_740_991.0) as u64
        } else {
            0
        };
        let cb = args.first().copied().unwrap_or(Value::UNDEFINED);
        if !self.is_callable(cb) {
            return Err(Thrown(format!(
                "TypeError: {name} callback is not a function"
            )));
        }
        let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        let idxv = |k: usize| Value::num(k as f64);
        self.preflight_native_iteration_work(full_len)?;

        match name {
            "forEach" => {
                for k in 0..len {
                    if let Some(val) = self.array_iter_get(this, k)? {
                        self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                    }
                }
                Ok(Some(Value::UNDEFINED))
            }
            "map" => {
                // Step 5: ArraySpeciesCreate(O, len) runs BEFORE the first
                // callback, and each result element is defined on the target as
                // it is produced — a species constructor, and the target's
                // defineProperty, are observable and may mutate the source. (The
                // buffer-then-define shape ran every Get and every callback
                // first, then the `constructor` lookup.) The species length is
                // the ToLength-clamped value (full_len), NOT the host iteration
                // bound: a +Infinity length is 2^53-1 for the constructor, not
                // the `as usize` saturation of `len`.
                let target = self.array_species_create(this, full_len as usize)?;
                // The ArrayCreate(len) inside ArraySpeciesCreate requires
                // len <= 2^32-1; a larger finite length OR a non-finite one
                // (Infinity, via ToLength → 2^53-1) is a RangeError. It only
                // applies when no custom species took over the allocation.
                if target.is_none() && lenf > 4_294_967_295.0 {
                    return Err(Thrown("RangeError: Invalid array length".into()));
                }
                // The result is materialised DENSELY, so unlike the probe-only
                // arms above its length is bounded by what the host can hold: a
                // 2^32-1 result would be 34 GB. V8 answers with a sparse array;
                // zipp has no sparse result representation here, so it reports
                // the documented RangeError rather than OOM-aborting the
                // process (`panic = "abort"` makes an allocation failure fatal).
                // The bound is the largest array the engine builds eagerly
                // elsewhere, so every length that `new Array(n)` accepts still
                // maps successfully.
                if len > MAX_EAGER_ITER_RESULT {
                    return Err(Thrown(
                        "RangeError: array length exceeds the engine's dense-array limit".into(),
                    ));
                }
                // A HOLE placeholder keeps an ABSENT source index absent in the
                // result (`1 in [1,,3].map(f)` is false); an UNDEFINED one made
                // the result dense.
                let mut out = if target.is_some() {
                    Vec::new()
                } else {
                    vec![Value::HOLE; len]
                };
                for k in 0..len {
                    if let Some(val) = self.array_iter_get(this, k)? {
                        let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                        match target {
                            Some(a) => self.create_data_property_or_throw(a, k, r)?,
                            None => out[k] = r,
                        }
                    }
                }
                Ok(Some(match target {
                    Some(a) => a,
                    None => Value::heap(self.heap.alloc(HeapObj::Array(out))),
                }))
            }
            "filter" => {
                // Step 5: ArraySpeciesCreate(O, 0) precedes the first callback,
                // and each kept element is defined as it is selected.
                let target = self.array_species_create(this, 0)?;
                let mut out = Vec::new();
                let mut n = 0usize;
                for k in 0..len {
                    if let Some(val) = self.array_iter_get(this, k)? {
                        let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                        if self.truthy(r) {
                            match target {
                                Some(a) => self.create_data_property_or_throw(a, n, val)?,
                                None => out.push(val),
                            }
                            n += 1;
                        }
                    }
                }
                Ok(Some(match target {
                    Some(a) => a,
                    None => Value::heap(self.heap.alloc(HeapObj::Array(out))),
                }))
            }
            "some" => {
                for k in 0..len {
                    if let Some(val) = self.array_iter_get(this, k)? {
                        let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                        if self.truthy(r) {
                            return Ok(Some(Value::bool(true)));
                        }
                    }
                }
                Ok(Some(Value::bool(false)))
            }
            "every" => {
                for k in 0..len {
                    if let Some(val) = self.array_iter_get(this, k)? {
                        let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                        if !self.truthy(r) {
                            return Ok(Some(Value::bool(false)));
                        }
                    }
                }
                Ok(Some(Value::bool(true)))
            }
            "find" | "findIndex" | "findLast" | "findLastIndex" => {
                // The find family visits EVERY index with Get (no HasProperty skip), so a
                // throwing index getter on an array-like propagates and an absent index is
                // undefined. find/findIndex go forward; findLast/findLastIndex backward.
                let backward = name == "findLast" || name == "findLastIndex";
                let total: u64 = if backward { full_len } else { len as u64 };
                let mut i: u64 = 0;
                while i < total {
                    let k = if backward {
                        (total - 1 - i) as usize
                    } else {
                        i as usize
                    };
                    let val = self.array_iter_get(this, k)?.unwrap_or(Value::UNDEFINED);
                    let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                    if self.truthy(r) {
                        return Ok(Some(match name {
                            "find" | "findLast" => val,
                            _ => idxv(k),
                        }));
                    }
                    i += 1;
                }
                Ok(Some(match name {
                    "find" | "findLast" => Value::UNDEFINED,
                    _ => Value::num(-1.0),
                }))
            }
            "reduce" | "reduceRight" => {
                let right = name == "reduceRight";
                if full_len > crate::vm::MAX_DENSE_ARRAY_LEN as u64
                    && matches!(self.heap.get(this.heap_index()), HeapObj::Object(_))
                {
                    let mut keys: Vec<u64> = match self.heap.get(this.heap_index()) {
                        HeapObj::Object(m) => m
                            .keys
                            .iter()
                            .filter_map(|k| {
                                k.parse::<u64>()
                                    .ok()
                                    .filter(|n| n.to_string() == *k && *n < full_len)
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    keys.sort_unstable();
                    if right {
                        keys.reverse();
                    }
                    let mut acc = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                    let mut started = args.len() >= 2;
                    for k in keys {
                        let kv = Value::num(k as f64);
                        let val = self.get_index(this, kv)?;
                        if !started {
                            acc = val;
                            started = true;
                        } else {
                            acc = self.call_value(cb, Value::UNDEFINED, &[acc, val, kv, this])?;
                        }
                    }
                    if !started {
                        return Err(Thrown(
                            "TypeError: Reduce of empty array with no initial value".into(),
                        ));
                    }
                    return Ok(Some(acc));
                }
                let order: Vec<usize> = if right {
                    (0..len).rev().collect()
                } else {
                    (0..len).collect()
                };
                let mut acc = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let mut started = args.len() >= 2;
                for k in order {
                    let val = match self.array_iter_get(this, k)? {
                        Some(v) => v,
                        None => continue,
                    };
                    if !started {
                        acc = val;
                        started = true;
                    } else {
                        acc = self.call_value(cb, Value::UNDEFINED, &[acc, val, idxv(k), this])?;
                    }
                }
                if !started {
                    return Err(Thrown(
                        "TypeError: Reduce of empty array with no initial value".into(),
                    ));
                }
                Ok(Some(acc))
            }
            _ => Ok(None),
        }
    }

    /// `indexOf` / `lastIndexOf` / `includes` over the generic [[Get]]/[[HasProperty]]
    /// protocol (ES 23.1.3.x), used whenever the receiver is an array-like object OR
    /// a real array carrying an `arr_props` side table (a defineProperty'd index
    /// accessor, or a prototype-inherited index). Unlike the dense snapshot fast path
    /// this: invokes accessor getters, walks the prototype chain, never materialises
    /// an absent index (HasProperty is consulted for indexOf/lastIndexOf), reads
    /// `length` live and coerces `fromIndex` AFTER it, and propagates a throwing index
    /// getter. `includes` reads EVERY index via Get (no HasProperty — a hole counts as
    /// undefined) and compares with SameValueZero; indexOf/lastIndexOf use HasProperty
    /// and strict equality.
    pub(crate) fn array_like_search(
        &mut self,
        this: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let _gc = self.gc_lock_guard();
        let search = args.first().copied().unwrap_or(Value::UNDEFINED);
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_strict(lv)?;
        // ToLength: clamp to 2^53-1 (NOT the dense-array ceiling) — search is per-index
        // via Get/HasProperty, so a fromIndex near a huge `length` reads only the few
        // indices in range (indexOf/lastIndexOf/includes on `{length: 2**53, ...}`).
        let len: i64 = if lenf > 0.0 {
            lenf.min(9_007_199_254_740_991.0) as i64
        } else {
            0
        };
        let idxv = |k: i64| Value::num(k as f64);
        if len == 0 {
            return Ok(Some(if name == "includes" {
                Value::bool(false)
            } else {
                Value::int(-1)
            }));
        }
        // fromIndex (ToIntegerOrInfinity), coerced AFTER reading length so its
        // valueOf side effects observe the current length.
        let has_from = args.len() >= 2;
        let from_raw = if has_from {
            self.to_integer_or_zero(args[1])?
        } else {
            0
        };
        if let Some(r) = self.virtual_array_search(this, name, search, len, has_from, from_raw)? {
            return Ok(Some(r));
        }
        match name {
            "lastIndexOf" => {
                // Default search start is len-1; n>=0 → min(n, len-1); n<0 → len+n.
                let mut k = if has_from {
                    if from_raw >= 0 {
                        from_raw.min(len - 1)
                    } else {
                        len + from_raw
                    }
                } else {
                    len - 1
                };
                self.preflight_native_iteration_work(if k >= 0 {
                    (k as u64).saturating_add(1)
                } else {
                    0
                })?;
                while k >= 0 {
                    // Proxy-aware HasProperty: a `has` trap must dispatch, and its
                    // abrupt completion propagate (the &self form swallows both).
                    if self.has_property_dyn(this, idxv(k))? {
                        let v = self.get_index(this, idxv(k))?;
                        if self.values_strict_eq(v, search) {
                            return Ok(Some(Value::num(k as f64))); // index may exceed i32 (length up to 2^53-1)
                        }
                    }
                    k -= 1;
                }
                Ok(Some(Value::int(-1)))
            }
            // indexOf / includes share the forward start: n>=0 → n; n<0 → len+n (≥0).
            _ => {
                let mut k = if from_raw >= 0 {
                    from_raw
                } else {
                    (len + from_raw).max(0)
                };
                self.preflight_native_iteration_work(len.saturating_sub(k).max(0) as u64)?;
                let is_includes = name == "includes";
                while k < len {
                    // includes visits every index (a hole reads as undefined);
                    // indexOf skips holes via HasProperty.
                    if is_includes {
                        let v = self.get_index(this, idxv(k))?;
                        if self.same_value_zero(v, search) {
                            return Ok(Some(Value::bool(true)));
                        }
                    } else if self.has_property_dyn(this, idxv(k))? {
                        let v = self.get_index(this, idxv(k))?;
                        if self.values_strict_eq(v, search) {
                            return Ok(Some(Value::num(k as f64))); // index may exceed i32 (length up to 2^53-1)
                        }
                    }
                    k += 1;
                }
                Ok(Some(if is_includes {
                    Value::bool(false)
                } else {
                    Value::int(-1)
                }))
            }
        }
    }

    /// The indices in `[lo, hi)` a VIRTUAL array holds, ascending: its dense
    /// store's non-hole slots and its overlay's index keys. A per-index walk
    /// of a virtual array is O(length) — minutes at `length = 2**32 - 1` for
    /// one element — while every index this leaves out is provably absent: no
    /// indexed property on the prototype chain (the protector, no replaced
    /// prototype), not an arguments object, and no accessor in the overlay, so
    /// reading the present elements has no side effect that could add or
    /// remove one. `None` when any of that does not hold.
    fn virtual_array_present(&self, idx: u32, lo: usize, hi: usize) -> Option<Vec<usize>> {
        if self.array_proto_has_index
            || self.proto_of.contains_key(&idx)
            || self.arguments_objs.contains_key(&idx)
        {
            return None;
        }
        let mut present: Vec<usize> = match self.heap.get(idx) {
            HeapObj::Array(items) => (lo.min(items.len())..hi.min(items.len()))
                .filter(|&i| !items[i].is_hole())
                .collect(),
            _ => return None,
        };
        if let Some(m) = self.arr_props.get(&idx) {
            for (i, k) in m.keys.iter().enumerate() {
                let Some(ki) = canonical_index_str(k) else {
                    continue;
                };
                if ki >= 4_294_967_295 {
                    continue;
                }
                if m.attr_at(i).accessor {
                    return None;
                }
                if ki >= lo && ki < hi {
                    present.push(ki);
                }
            }
        }
        present.sort_unstable();
        present.dedup();
        Some(present)
    }

    /// indexOf / lastIndexOf / includes over a VIRTUAL array, visiting only the
    /// indices it holds (see `virtual_array_present`). An absent index is
    /// skipped by indexOf/lastIndexOf and reads `undefined` for includes.
    /// `None` hands the call to the per-index protocol.
    fn virtual_array_search(
        &mut self,
        this: Value,
        name: &str,
        search: Value,
        len: i64,
        has_from: bool,
        from_raw: i64,
    ) -> Result<Option<Value>, Thrown> {
        if !this.is_heap() || !self.array_is_virtual(this.heap_index()) {
            return Ok(None);
        }
        let idx = this.heap_index();
        let (lo, hi) = if name == "lastIndexOf" {
            let k = if has_from {
                if from_raw >= 0 {
                    from_raw.min(len - 1)
                } else {
                    len + from_raw
                }
            } else {
                len - 1
            };
            (0i64, k + 1)
        } else {
            let k = if from_raw >= 0 {
                from_raw
            } else {
                (len + from_raw).max(0)
            };
            (k, len)
        };
        if lo >= hi {
            return Ok(Some(if name == "includes" {
                Value::bool(false)
            } else {
                Value::int(-1)
            }));
        }
        let (lo, hi) = (lo as usize, hi as usize);
        let Some(mut present) = self.virtual_array_present(idx, lo, hi) else {
            return Ok(None);
        };
        let is_includes = name == "includes";
        if is_includes && search == Value::UNDEFINED && present.len() < hi - lo {
            return Ok(Some(Value::bool(true)));
        }
        if name == "lastIndexOf" {
            present.reverse();
        }
        for k in present {
            let v = self.get_index(this, Value::num(k as f64))?;
            let hit = if is_includes {
                self.same_value_zero(v, search)
            } else {
                self.values_strict_eq(v, search)
            };
            if hit {
                return Ok(Some(if is_includes {
                    Value::bool(true)
                } else {
                    Value::num(k as f64)
                }));
            }
        }
        Ok(Some(if is_includes {
            Value::bool(false)
        } else {
            Value::int(-1)
        }))
    }

    /// `Array.prototype.copyWithin` against an array-like *object* via the generic
    /// Get/Set/HasProperty/DeletePropertyOrThrow protocol, so it propagates abrupt
    /// completions (a throwing length/index coercion, or a non-configurable target
    /// that can't be deleted → TypeError). Real arrays use the dense fast path.
    pub(crate) fn array_like_copy_within(
        &mut self,
        this: Value,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let _gc = self.gc_lock_guard();
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_strict(lv)?;
        // ToLength: clamp to [0, 2^53-1].
        let len: i64 = if lenf.is_nan() || lenf <= 0.0 {
            0
        } else {
            lenf.floor().min(9_007_199_254_740_991.0) as i64
        };
        let rel = |i: i64| -> i64 {
            if i < 0 {
                (len + i).max(0)
            } else {
                i.min(len)
            }
        };
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let mut to = rel(self.to_integer_or_zero(arg0)?);
        let s0 = if args.len() >= 2 {
            self.to_integer_or_zero(args[1])?
        } else {
            0
        };
        let mut from = rel(s0);
        let e0 = if args.len() >= 3 && args[2] != Value::UNDEFINED {
            self.to_integer_or_zero(args[2])?
        } else {
            len
        };
        let mut count = (rel(e0) - from).min(len - to).max(0);
        self.preflight_native_iteration_work(count as u64)?;
        let mut dir = 1i64;
        if from < to && to < from + count {
            dir = -1;
            from += count - 1;
            to += count - 1;
        }
        while count > 0 {
            let fk = Value::num(from as f64);
            // HasProperty must dispatch a Proxy `has` trap and propagate its abrupt
            // completion (the &self has_property swallows both).
            if self.has_property_dyn(this, fk)? {
                let v = self.get_index(this, fk)?;
                // Set(O, to, v, THROW) — same reason as `fill`: copyWithin on a
                // frozen array must raise, not silently drop the write.
                self.set_index(this, Value::num(to as f64), v, true)?;
            } else {
                let deleted = self.delete_property(this, &to.to_string())?;
                if !self.truthy(deleted) {
                    return Err(Thrown(format!("TypeError: cannot delete property '{to}'")));
                }
            }
            from += dir;
            to += dir;
            count -= 1;
        }
        Ok(Some(this))
    }

    /// `Array.prototype.fill` against an array-like *object* via Set, so a
    /// throwing setter, a non-writable/frozen slot, a symbol length, or a
    /// throwing index coercion propagates (abrupt completion). Real arrays use
    /// the dense fast path.
    pub(crate) fn array_like_fill(
        &mut self,
        this: Value,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let _gc = self.gc_lock_guard();
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_strict(lv)?;
        let len: i64 = if lenf.is_nan() || lenf <= 0.0 {
            0
        } else {
            lenf.floor().min(9_007_199_254_740_991.0) as i64
        };
        let rel = |i: i64| -> i64 {
            if i < 0 {
                (len + i).max(0)
            } else {
                i.min(len)
            }
        };
        let value = args.first().copied().unwrap_or(Value::UNDEFINED);
        let s0 = if args.len() >= 2 {
            self.to_integer_or_zero(args[1])?
        } else {
            0
        };
        let mut k = rel(s0);
        let e0 = if args.len() >= 3 && args[2] != Value::UNDEFINED {
            self.to_integer_or_zero(args[2])?
        } else {
            len
        };
        let end = rel(e0);
        self.preflight_native_iteration_work(end.saturating_sub(k) as u64)?;
        while k < end {
            // Set(O, Pk, value, THROW) — the `true` matters: a non-writable
            // element or a non-extensible receiver must raise a TypeError, not
            // be dropped. `fill` on a frozen array silently succeeded.
            self.set_index(this, Value::num(k as f64), value, true)?;
            k += 1;
        }
        Ok(Some(this))
    }

    // ── generic (array-like) mutators: the abstract Get/Set/HasProperty/
    // DeletePropertyOrThrow + ToLength(length)/Set(length) protocol, so
    // `Array.prototype.<m>.call({0:…, length:n}, …)` mutates a plain object. Real
    // arrays use the dense fast paths in `array_method`; these run only for a
    // non-array `this`. ──

    /// ToLength(Get(O, "length")) — clamped to [0, 2^53-1].
    fn al_len(&mut self, this: Value) -> Result<i64, Thrown> {
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_strict(lv)?;
        Ok(if lenf.is_nan() || lenf <= 0.0 {
            0
        } else {
            lenf.floor().min(9_007_199_254_740_991.0) as i64
        })
    }
    /// Set(O, "length", n, true).
    fn al_set_len(&mut self, this: Value, n: i64) -> Result<(), Thrown> {
        self.set_prop(this, "length", Value::num(n as f64), true)?;
        Ok(())
    }
    /// HasProperty(O, i) — proxy-aware (dispatches a Proxy `has` trap).
    fn al_has(&mut self, this: Value, i: i64) -> Result<bool, Thrown> {
        self.has_property_dyn(this, Value::num(i as f64))
    }
    fn al_get(&mut self, this: Value, i: i64) -> Result<Value, Thrown> {
        self.get_index(this, Value::num(i as f64))
    }
    fn al_set(&mut self, this: Value, i: i64, v: Value) -> Result<(), Thrown> {
        self.set_index(this, Value::num(i as f64), v, true)
    }
    /// DeletePropertyOrThrow(O, i).
    fn al_del(&mut self, this: Value, i: i64) -> Result<(), Thrown> {
        let r = self.delete_property(this, &i.to_string())?;
        if !self.truthy(r) {
            return Err(Thrown(format!(
                "TypeError: Cannot delete property '{i}' of an array-like object"
            )));
        }
        Ok(())
    }

    pub(crate) fn array_like_mutate(
        &mut self,
        this: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        const MAX_SAFE: i64 = 9_007_199_254_740_991;
        let _gc = self.gc_lock_guard();
        let len = self.al_len(this)?;
        let r = match name {
            "push" => {
                let argc = args.len() as i64;
                if len + argc > MAX_SAFE {
                    return Err(Thrown("TypeError: Array length exceeds the maximum".into()));
                }
                let mut n = len;
                for &item in args {
                    self.al_set(this, n, item)?;
                    n += 1;
                }
                self.al_set_len(this, n)?;
                Value::num(n as f64)
            }
            "pop" => {
                if len == 0 {
                    self.al_set_len(this, 0)?;
                    Value::UNDEFINED
                } else {
                    let i = len - 1;
                    let el = self.al_get(this, i)?;
                    self.al_del(this, i)?;
                    self.al_set_len(this, i)?;
                    el
                }
            }
            "shift" => {
                self.preflight_native_iteration_work(len.saturating_sub(1).max(0) as u64)?;
                if len == 0 {
                    self.al_set_len(this, 0)?;
                    Value::UNDEFINED
                } else {
                    let first = self.al_get(this, 0)?;
                    let mut k = 1;
                    while k < len {
                        if self.al_has(this, k)? {
                            let v = self.al_get(this, k)?;
                            self.al_set(this, k - 1, v)?;
                        } else {
                            self.al_del(this, k - 1)?;
                        }
                        k += 1;
                    }
                    self.al_del(this, len - 1)?;
                    self.al_set_len(this, len - 1)?;
                    first
                }
            }
            "unshift" => {
                let argc = args.len() as i64;
                if argc > 0 {
                    self.preflight_native_iteration_work(len as u64)?;
                    if len + argc > MAX_SAFE {
                        return Err(Thrown("TypeError: Array length exceeds the maximum".into()));
                    }
                    let mut k = len;
                    while k > 0 {
                        let from = k - 1;
                        let to = k + argc - 1;
                        if self.al_has(this, from)? {
                            let v = self.al_get(this, from)?;
                            self.al_set(this, to, v)?;
                        } else {
                            self.al_del(this, to)?;
                        }
                        k -= 1;
                    }
                    let mut j = 0i64;
                    for &item in args {
                        self.al_set(this, j, item)?;
                        j += 1;
                    }
                }
                let newlen = len + argc;
                self.al_set_len(this, newlen)?;
                Value::num(newlen as f64)
            }
            "reverse" => {
                let middle = len / 2;
                self.preflight_native_iteration_work(middle as u64)?;
                let mut lower = 0;
                while lower != middle {
                    let upper = len - lower - 1;
                    let lower_exists = self.al_has(this, lower)?;
                    let lower_val = if lower_exists {
                        self.al_get(this, lower)?
                    } else {
                        Value::UNDEFINED
                    };
                    let upper_exists = self.al_has(this, upper)?;
                    let upper_val = if upper_exists {
                        self.al_get(this, upper)?
                    } else {
                        Value::UNDEFINED
                    };
                    match (lower_exists, upper_exists) {
                        (true, true) => {
                            self.al_set(this, lower, upper_val)?;
                            self.al_set(this, upper, lower_val)?;
                        }
                        (false, true) => {
                            self.al_set(this, lower, upper_val)?;
                            self.al_del(this, upper)?;
                        }
                        (true, false) => {
                            self.al_del(this, lower)?;
                            self.al_set(this, upper, lower_val)?;
                        }
                        (false, false) => {}
                    }
                    lower += 1;
                }
                this
            }
            "splice" => {
                let relative_start =
                    self.to_integer_or_zero(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                let actual_start = if relative_start < 0 {
                    len.saturating_add(relative_start).max(0)
                } else {
                    relative_start.min(len)
                };
                let (insert_count, actual_delete) = if args.is_empty() {
                    (0i64, 0i64)
                } else if args.len() == 1 {
                    (0, len - actual_start)
                } else {
                    let dc = self.to_integer_or_zero(args[1])?;
                    (args.len() as i64 - 2, dc.max(0).min(len - actual_start))
                };
                if len - actual_delete + insert_count > MAX_SAFE {
                    return Err(Thrown("TypeError: Array length exceeds the maximum".into()));
                }
                let shifted_tail = if insert_count == actual_delete {
                    0
                } else {
                    len.saturating_sub(actual_start)
                };
                let work = actual_delete.checked_add(shifted_tail).ok_or_else(|| {
                    Thrown("RangeError: native builtin iteration limit exceeded".into())
                })?;
                self.preflight_native_iteration_work(work as u64)?;
                // Step 9: ArraySpeciesCreate(O, actualDeleteCount) runs BEFORE any
                // element read; the no-species ArrayCreate path rejects > 2^32-1
                // immediately (a 2^32-length receiver must not loop 4e9 reads).
                let species_target =
                    self.array_species_create(this, actual_delete.max(0) as usize)?;
                if species_target.is_none() && actual_delete > 4_294_967_295 {
                    return Err(Thrown("RangeError: Invalid array length".into()));
                }
                let a = match species_target {
                    Some(a) => {
                        // STREAM the deleted elements: Has/Get then DEFINE on A
                        // per element (absent indices stay absent on A).
                        let mut k = 0;
                        while k < actual_delete {
                            let from = actual_start + k;
                            if self.al_has(this, from)? {
                                let v = self.al_get(this, from)?;
                                self.create_data_property_or_throw(a, k as usize, v)?;
                            }
                            k += 1;
                        }
                        self.set_prop(a, "length", Value::num(actual_delete.max(0) as f64), true)?;
                        a
                    }
                    None => {
                        self.preflight_materialized_array(actual_delete.max(0) as usize)?;
                        let mut deleted: Vec<Value> =
                            Vec::with_capacity((actual_delete.max(0) as usize).min(4096));
                        let mut k = 0;
                        while k < actual_delete {
                            let from = actual_start + k;
                            deleted.push(if self.al_has(this, from)? {
                                self.al_get(this, from)?
                            } else {
                                Value::HOLE
                            });
                            k += 1;
                        }
                        // ArrayCreate in the CURRENT realm — see alloc_array_current_realm.
                        self.alloc_array_current_realm(deleted)
                    }
                };
                // Shift the tail to make room for the inserted items.
                if insert_count < actual_delete {
                    let mut k = actual_start;
                    while k < len - actual_delete {
                        let from = k + actual_delete;
                        let to = k + insert_count;
                        if self.al_has(this, from)? {
                            let v = self.al_get(this, from)?;
                            self.al_set(this, to, v)?;
                        } else {
                            self.al_del(this, to)?;
                        }
                        k += 1;
                    }
                    let mut k = len;
                    while k > len - actual_delete + insert_count {
                        self.al_del(this, k - 1)?;
                        k -= 1;
                    }
                } else if insert_count > actual_delete {
                    let mut k = len - actual_delete;
                    while k > actual_start {
                        let from = k + actual_delete - 1;
                        let to = k + insert_count - 1;
                        if self.al_has(this, from)? {
                            let v = self.al_get(this, from)?;
                            self.al_set(this, to, v)?;
                        } else {
                            self.al_del(this, to)?;
                        }
                        k -= 1;
                    }
                }
                // Insert the new items (args[2..]).
                let items: &[Value] = if args.len() > 2 { &args[2..] } else { &[] };
                let mut k = actual_start;
                for &item in items {
                    self.al_set(this, k, item)?;
                    k += 1;
                }
                self.al_set_len(this, len - actual_delete + insert_count)?;
                a
            }
            _ => return Ok(None),
        };
        Ok(Some(r))
    }

    /// `fill` on a VIRTUAL array, written into the dense store: grow it (with
    /// holes) to the fill's end and store the value over `[start, end)`. When
    /// the end is the JS length the array stops being virtual, so the idiom
    /// `new Array(2e6).fill(0)` leaves an ordinary dense array behind — every
    /// fast path, and the JIT, can use it again.
    ///
    /// `None` hands the call to the generic Set protocol, whenever this could
    /// be observed differently: a start/end argument whose coercion runs code,
    /// an element overlay or integrity flag on the array, a prototype that
    /// could intercept a Set on a hole. An end past
    /// `MAX_MATERIALIZED_ARRAY_LEN` also goes generic when the range beyond
    /// the store is short, and is a RangeError when it is not.
    fn fill_virtual_array_dense(
        &mut self,
        idx: u32,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let plain = |v: Option<&Value>| v.map_or(true, |v| *v == Value::UNDEFINED || v.is_number());
        if !plain(args.get(1))
            || !plain(args.get(2))
            || self.array_elements_overlaid(idx)
            || self.array_proto_has_index
            || self.proto_of.contains_key(&idx)
            || self.arguments_objs.contains_key(&idx)
        {
            return Ok(None);
        }
        let js_len = self.js_array_len(idx);
        let len = js_len as f64;
        // ToIntegerOrInfinity of a Number, then the relative-index clamp.
        let rel = |v: Option<&Value>, absent: f64| -> f64 {
            match v {
                Some(v) if v.is_number() => {
                    let n = v.as_f64();
                    let n = if n.is_nan() { 0.0 } else { n.trunc() };
                    if n < 0.0 {
                        (len + n).max(0.0)
                    } else {
                        n.min(len)
                    }
                }
                _ => absent,
            }
        };
        let start = rel(args.get(1), 0.0) as usize;
        let end = rel(args.get(2), len) as usize;
        let dense = match self.heap.get(idx) {
            HeapObj::Array(items) => items.len(),
            _ => return Ok(None),
        };
        if end > crate::vm::MAX_MATERIALIZED_ARRAY_LEN {
            // The store cannot grow that far. The generic Sets grow it up to
            // the dense cap, and every write past that becomes a string-keyed
            // overlay property: a short tail is cheap that way, but
            // `new Array(3e7).fill(0)` ran for seconds building gigabytes of
            // them. Refuse a long one like every other materialization.
            let overlaid = end.saturating_sub(start.max(dense).max(crate::vm::MAX_DENSE_ARRAY_LEN));
            if overlaid > crate::vm::MAX_DENSE_ARRAY_LEN {
                return Err(Thrown(
                    "RangeError: array length exceeds the engine's dense-array limit".into(),
                ));
            }
            return Ok(None);
        }
        if start < end {
            if end > dense {
                self.preflight_materialized_array(end)?;
            }
            let val = args.first().copied().unwrap_or(Value::UNDEFINED);
            self.heap.write_barrier_val(idx, val);
            if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                if end > items.len() {
                    items
                        .try_reserve_exact(end - items.len())
                        .map_err(|_| Thrown("RangeError: array allocation failed".into()))?;
                    items.resize(end, Value::HOLE);
                }
                items[start..end].fill(val);
                if end == js_len {
                    self.array_js_len.remove(&idx);
                }
            }
            // The key set (holes became elements) and possibly the length
            // representation changed; see `array_apply_length`.
            self.heap.bump_version(idx);
        }
        Ok(Some(Value::heap(idx)))
    }

    pub(crate) fn array_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        // Suspend GC for the whole method: callback-driven arms (map/filter/
        // reduce/sort/…) hold un-rooted working sets across interpreter re-entry,
        // and the array-like path builds an un-rooted temp array. Non-callback
        // arms never reach a GC safe point, so the lock is free for them.
        let _gc = self.gc_lock_guard();
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // `new Array(n).fill(x)` past the dense cap: the one idiom that turns a
        // VIRTUAL array (see MAX_DENSE_ARRAY_LEN) into a fully populated one.
        // Writing it through the generic Set protocol would put every element
        // past the cap in the string-keyed sparse overlay; build the dense store
        // instead, up to the fill's end, and let the dense arm below fill it.
        // Only when nothing can observe the difference: no overlay element to
        // collide with, no side table constraint on the writes, no prototype
        // that could intercept a Set on a hole.
        let virtual_len = self.array_is_virtual(idx);
        if virtual_len && name == "fill" {
            if let Some(r) = self.fill_virtual_array_dense(idx, args)? {
                return Ok(Some(r));
            }
        }
        // Generic array methods accept an array-like `this`
        // (`Array.prototype.map.call({length:2, 0:'a', 1:'b'}, cb)`, or on a string).
        // For a non-array receiver, snapshot its `length` + indexed elements into a
        // temp array and run the (read-only) method against that. Mutating methods
        // still require a real array (they fall through to their HeapObj::Array arms).
        //
        // A VIRTUAL array takes the same generic protocol: every dense arm below
        // sizes its work from the Vec, which holds only a prefix of it, so they
        // silently answered for a truncated array. (`concat` and `sort` are not
        // in the generic list; their arms read `js_array_len` themselves.)
        //
        // `toSpliced` with a start or deleteCount that is not a Number takes it
        // too. Its spec order — length, then ToIntegerOrInfinity of both
        // (valueOf runs, a BigInt throws), then reads bounded by the ENTRY
        // length — is what the generic arm implements; the dense arm read
        // non-Number arguments as 0 and copied the array as it stood after the
        // coercions. A Number coerces without running code, so it stays dense.
        if virtual_len
            || (name == "toSpliced" && args.iter().take(2).any(|a| !a.is_number()))
            || !matches!(self.heap.get(idx), HeapObj::Array(_))
        {
            // Hole-skipping callback methods iterate the array-like object with
            // HasProperty per index (a dense snapshot would treat holes as
            // present-undefined and wrongly invoke the callback on them).
            if matches!(
                name,
                "map" | "filter" | "forEach" | "every" | "some" | "reduce" | "reduceRight"
            ) {
                return self.array_like_iterate(Value::heap(idx), name, args);
            }
            // copyWithin mutates an array-like in place via the generic protocol
            // (Get/Set/HasProperty/DeletePropertyOrThrow), propagating abrupt
            // completions a dense snapshot would swallow.
            if name == "copyWithin" {
                return self.array_like_copy_within(Value::heap(idx), args);
            }
            if name == "fill" {
                return self.array_like_fill(Value::heap(idx), args);
            }
            // The in-place mutators are generic over an array-like object: they
            // operate via the abstract ToLength(Get(O,"length")) + Get/Set/
            // HasProperty/DeletePropertyOrThrow + Set(O,"length",…) protocol, so a
            // plain `{0:…, length:n}` receiver is mutated correctly (real arrays use
            // the dense fast paths in the match below).
            if matches!(
                name,
                "pop" | "push" | "shift" | "unshift" | "reverse" | "splice"
            ) {
                return self.array_like_mutate(Value::heap(idx), name, args);
            }
            // indexOf/lastIndexOf/includes via the generic HasProperty/Get protocol —
            // invokes inherited/accessor getters, never materialises an absent index,
            // and propagates a throwing getter (a dense snapshot would do none of these).
            if matches!(name, "indexOf" | "lastIndexOf" | "includes") {
                return self.array_like_search(Value::heap(idx), name, args);
            }
            // The find family iterates via the generic Get protocol (every index visited,
            // accessor getters invoked, a throwing getter propagated) rather than
            // materialising a dense snapshot that swallows those side effects.
            if matches!(name, "find" | "findIndex" | "findLast" | "findLastIndex") {
                return self.array_like_iterate(Value::heap(idx), name, args);
            }
            // Read-only methods that treat a hole as undefined snapshot to a dense
            // temp array and run against that. (concat is NOT here: it must check
            // IsConcatSpreadable on the receiver itself — a non-array array-like is
            // appended WHOLE, not spread — so it runs on the object directly below.)
            // keys/values/entries on ANY receiver return a LIVE iterator over
            // the original object (the spec iterator re-reads length/elements
            // per step; a TypedArray receiver hits the live-TA next() branch,
            // which also throws its out-of-bounds TypeError per step).
            if matches!(name, "keys" | "values" | "entries") {
                let kind = match name {
                    "keys" => 0u8,
                    "values" => 1,
                    _ => 2,
                };
                return Ok(Some(self.make_live_iterator(
                    idx,
                    kind,
                    self.array_iter_proto,
                )));
            }
            if matches!(
                name,
                "join"
                    | "toString"
                    | "slice"
                    | "at"
                    | "flat"
                    | "flatMap"
                    | "with"
                    | "toReversed"
                    | "toSorted"
                    | "toSpliced"
                    | "toLocaleString"
            ) {
                // toSorted: IsCallable(comparefn) precedes ANY length / element read
                // (a non-callable comparator is a TypeError before the length getter).
                if name == "toSorted" {
                    let cmp = args.first().copied().unwrap_or(Value::UNDEFINED);
                    if cmp != Value::UNDEFINED && !self.is_callable(cmp) {
                        return Err(Thrown("TypeError: the comparator is not a function".into()));
                    }
                }
                // toReversed reads the live array-like in DESCENDING index order and
                // builds the reversed result directly (one length read, ArrayCreate
                // RangeError, then the descending element Gets — array_like_read reads
                // ascending, which is the wrong observable order here).
                if name == "toReversed" {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lenf = self.to_number_strict(lv)?;
                    let len = if lenf.is_nan() || lenf <= 0.0 {
                        0usize
                    } else {
                        lenf.trunc().min(9_007_199_254_740_991.0) as usize
                    };
                    if len as f64 > 4_294_967_295.0 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    self.preflight_materialized_array(len)?;
                    let mut out = Vec::with_capacity(len);
                    for k in 0..len {
                        let v =
                            self.get_index(Value::heap(idx), Value::num((len - 1 - k) as f64))?;
                        out.push(v);
                    }
                    return Ok(Some(self.alloc_array_current_realm(out)));
                }
                // at: length is read ONCE, THEN the index argument is coerced (its
                // valueOf may mutate the receiver, e.g. shrink a resizable buffer),
                // then a single LIVE Get — never a snapshot of stale elements.
                if name == "at" {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lenf = self.to_number_strict(lv)?;
                    let len = if lenf.is_nan() || lenf <= 0.0 {
                        0.0
                    } else {
                        lenf.trunc().min(9_007_199_254_740_991.0)
                    };
                    let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let rel = self.to_number_strict(a0)?;
                    let rel = if rel.is_nan() { 0.0 } else { rel.trunc() };
                    let k = if rel >= 0.0 { rel } else { len + rel };
                    if k < 0.0 || k >= len {
                        return Ok(Some(Value::UNDEFINED));
                    }
                    let v = self.get_index(Value::heap(idx), Value::num(k))?;
                    return Ok(Some(v));
                }
                // flat/flatMap run FlattenIntoArray against the ORIGINAL
                // receiver in spec order: length Get, (flatMap) mapper
                // IsCallable, (flat) depth coercion, ArraySpeciesCreate(O, 0)
                // — its constructor Get is observable — then the HasProperty+
                // Get walk (absent indices skipped; the mapper never runs on
                // a hole).
                if matches!(name, "flat" | "flatMap") {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lf = self.to_number_strict(lv)?;
                    // The full ToLength (the walk preflights its own work); a
                    // clamp here silently skipped every index past 2^20.
                    let source_len = if lf.is_nan() || lf <= 0.0 {
                        0usize
                    } else {
                        lf.trunc().min(9_007_199_254_740_991.0) as usize
                    };
                    let (depth, mapper) = if name == "flatMap" {
                        if !self.is_callable(arg0) {
                            return Err(Thrown(
                                "TypeError: flatMap mapper is not a function".into(),
                            ));
                        }
                        (
                            1i64,
                            Some((arg0, args.get(1).copied().unwrap_or(Value::UNDEFINED))),
                        )
                    } else if args.is_empty() || arg0 == Value::UNDEFINED {
                        (1i64, None)
                    } else {
                        (self.to_integer_or_zero(arg0)?.max(0), None)
                    };
                    let target = self.array_species_create(Value::heap(idx), 0)?;
                    return self
                        .flatten_into_array_result(Value::heap(idx), source_len, depth, mapper, target)
                        .map(Some);
                }
                // join/toString/toLocaleString run LIVE against the receiver:
                // len = ToLength(Get(O,'length')) FIRST, then (join) the
                // separator coerces, then ONE Get per index — a separator
                // toString or an element toLocaleString that resizes the
                // receiver (resizable-buffer TA) is observed per element.
                if matches!(name, "join" | "toString" | "toLocaleString") {
                    return self.with_array_stringify_guard(idx, |vm| {
                        let lv = vm.get_prop(Value::heap(idx), "length")?;
                        let lenf = vm.to_number_strict(lv)?;
                        // The full ToLength: a clamp here joined only the first
                        // 2^20 elements of a longer array without a word.
                        let len = if lenf.is_nan() || lenf <= 0.0 {
                            0usize
                        } else {
                            lenf.trunc().min(9_007_199_254_740_991.0) as usize
                        };
                        // Separator and parts as EXACT strings (see the dense
                        // `join` below): a lone surrogate is itself, not U+FFFD.
                        let sep = if name == "join" && arg0 != Value::UNDEFINED {
                            vm.to_js_str_owned(arg0)?
                        } else {
                            crate::heap::JsStr::new(",".to_string())
                        };
                        // The separators alone past the string cap: the result
                        // cannot exist, so fail before walking the length.
                        if len > 1
                            && (len - 1).saturating_mul(sep.as_bytes().len())
                                > crate::vm::MAX_STRING_BYTES
                        {
                            return Err(Thrown("RangeError: Invalid string length".into()));
                        }
                        vm.preflight_native_iteration_work(len as u64)?;
                        let mut out: Vec<u8> = Vec::new();
                        for k in 0..len {
                            let v = vm.get_index(Value::heap(idx), Value::num(k as f64))?;
                            if k != 0 {
                                vm.append_guest_wtf8(&mut out, sep.as_bytes())?;
                            }
                            if v.is_nullish() {
                                // An absent or nullish element contributes "".
                            } else if name == "toLocaleString" {
                                let f = vm.get_prop(v, "toLocaleString")?;
                                if !vm.is_callable(f) {
                                    return Err(Thrown(
                                        "TypeError: toLocaleString is not callable".into(),
                                    ));
                                }
                                // ECMA-402 sup-array.prototype.toLocaleString:
                                // Invoke(element, "toLocaleString", «locales,
                                // options») — the arguments are FORWARDED.
                                let fwd = [
                                    args.first().copied().unwrap_or(Value::UNDEFINED),
                                    args.get(1).copied().unwrap_or(Value::UNDEFINED),
                                ];
                                let r = vm.call_value(f, v, &fwd)?;
                                vm.append_guest_tostring(&mut out, r)?;
                            } else {
                                vm.append_guest_tostring(&mut out, v)?;
                            }
                        }
                        Ok(Some(vm.alloc_wtf8(out)))
                    });
                }
                // toSpliced runs the spec copy loops directly: a DISCARDED element
                // (actualStart..actualStart+actualDeleteCount) is never read — the
                // snapshot path would invoke its getter.
                if name == "toSpliced" {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lenf = self.to_number_strict(lv)?;
                    let len = if lenf.is_nan() || lenf <= 0.0 {
                        0i64
                    } else {
                        lenf.trunc().min(9_007_199_254_740_991.0) as i64
                    };
                    let toii = |v: f64| if v.is_nan() { 0.0 } else { v.trunc() };
                    let (start, del) = if args.is_empty() {
                        (0i64, 0i64)
                    } else {
                        let s_raw = toii(self.to_number_strict(args[0])?);
                        let s = if s_raw < 0.0 {
                            ((len as f64) + s_raw).max(0.0)
                        } else {
                            s_raw.min(len as f64)
                        } as i64;
                        let d = if args.len() < 2 {
                            len - s
                        } else {
                            let d_raw = toii(self.to_number_strict(args[1])?);
                            (d_raw.max(0.0) as i64).min(len - s)
                        };
                        (s, d)
                    };
                    let insert: Vec<Value> = args.get(2..).unwrap_or(&[]).to_vec();
                    let new_len = len - del + insert.len() as i64;
                    // Step 12: newLen > 2^53-1 is a TypeError; step 13 ArrayCreate
                    // rejects > 2^32-1 with a RangeError.
                    if new_len > 9_007_199_254_740_991 {
                        return Err(Thrown("TypeError: Array length exceeds the maximum".into()));
                    }
                    if new_len > 4_294_967_295 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    self.preflight_materialized_array(new_len.max(0) as usize)?;
                    let mut out = Vec::with_capacity((new_len.max(0) as usize).min(4096));
                    for k in 0..start {
                        out.push(self.get_index(Value::heap(idx), Value::num(k as f64))?);
                    }
                    out.extend(insert);
                    let mut r = start + del;
                    while (out.len() as i64) < new_len {
                        out.push(self.get_index(Value::heap(idx), Value::num(r as f64))?);
                        r += 1;
                    }
                    return Ok(Some(self.alloc_array_current_realm(out)));
                }
                // with/toSorted build a result of the source length via
                // ArrayCreate(len), which throws RangeError for len > 2^32-1 — BEFORE
                // reading any element (a throwing index getter must not run first).
                if matches!(name, "with" | "toSorted") {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let n = self.to_number_strict(lv)?;
                    // ArrayCreate(len) requires len <= 2^32-1; a larger finite length OR
                    // a non-finite one (Infinity, via ToLength → 2^53-1) is a RangeError.
                    if n > 4_294_967_295.0 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                }
                // slice runs the spec directly: length is read ONCE, start/end
                // coerce ONCE, ArraySpeciesCreate(O, count) uses the ORIGINAL
                // receiver (count <= 2^32-1 validated BEFORE any element read),
                // then live per-index HasProperty+Get (proxy/TA-correct).
                if name == "slice" {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lenf = self.to_number_strict(lv)?;
                    // ToLength(lenf) → clamp to [0, 2^53-1].
                    let len = if lenf.is_nan() || lenf <= 0.0 {
                        0.0
                    } else {
                        lenf.trunc().min(9_007_199_254_740_991.0)
                    };
                    // relativeStart/relativeEnd = ToIntegerOrInfinity(arg) (Infinity-aware).
                    let toii = |raw: f64| if raw.is_nan() { 0.0 } else { raw.trunc() };
                    let s_arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let rel_start = toii(self.to_number_strict(s_arg)?);
                    let k0 = if rel_start < 0.0 {
                        (len + rel_start).max(0.0)
                    } else {
                        rel_start.min(len)
                    };
                    let e_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                    let rel_end = if e_arg == Value::UNDEFINED {
                        len
                    } else {
                        toii(self.to_number_strict(e_arg)?)
                    };
                    let fin = if rel_end < 0.0 {
                        (len + rel_end).max(0.0)
                    } else {
                        rel_end.min(len)
                    };
                    let count = (fin - k0).max(0.0);
                    if count > 4_294_967_295.0 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    self.preflight_native_iteration_work(count as u64)?;
                    let target = self.array_species_create(Value::heap(idx), count as usize)?;
                    return match target {
                        Some(a) => {
                            let mut n = 0usize;
                            let mut kf = k0;
                            while kf < fin {
                                if let Some(v) =
                                    self.array_iter_get(Value::heap(idx), kf as usize)?
                                {
                                    self.create_data_property_or_throw(a, n, v)?;
                                }
                                n += 1;
                                kf += 1.0;
                            }
                            self.set_prop(a, "length", Value::num(n as f64), true)?;
                            Ok(Some(a))
                        }
                        None => {
                            self.preflight_materialized_array(count as usize)?;
                            let mut out = Vec::with_capacity((count as usize).min(4096));
                            let mut kf = k0;
                            while kf < fin {
                                match self.array_iter_get(Value::heap(idx), kf as usize)? {
                                    Some(v) => out.push(v),
                                    None => out.push(Value::HOLE),
                                }
                                kf += 1.0;
                            }
                            Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
                        }
                    };
                }
                let elems = self.array_like_read(idx)?;
                let tmp = self.heap.alloc(HeapObj::Array(elems));
                return self.array_method(tmp, name, args);
            }
        }
        // A REAL array that carries an arr_props side table may hold a
        // defineProperty'd index ACCESSOR — its getter lives in arr_props while the
        // dense slot is only an undefined placeholder. The dense fast paths below
        // read that placeholder and never invoke the getter, so route the callback
        // methods through the generic HasProperty/Get protocol (which calls
        // get_index → array_index_override → the getter). Arrays without a side
        // table keep the fast snapshot path (zero perf impact on the common case).
        // A side table (defineProperty'd index accessor) OR a HOLE makes the dense
        // placeholder unreliable: route the callback methods to the live HasProperty+
        // Get protocol (skips absent indices, invokes accessor getters). A hole-free,
        // side-table-free array keeps the fast dense path below — whose general
        // (non-native) JS-callback branch reads each element live, so a callback's
        // mid-iteration mutation is still observed; only the non-mutating native
        // numeric kernel snapshots.
        // (The cheap NAME match runs first: `array_has_holes` is an O(len) scan,
        // which must not be paid by every other method call on a big clean array.)
        //
        // This gate and the `shift|reverse|pop|unshift|splice` one below keep the
        // COARSE `contains_key` where the others narrowed to
        // `array_elements_overlaid` — deliberately. `map`, `filter` and `splice`
        // run ArraySpeciesCreate, which does `Get(O, "constructor")`, so an OWN
        // `constructor` is observable even though it names no element. Narrowing
        // these two broke `staging/sm/Array/splice-species-changes-length.js` in
        // both tiers (`array.constructor = {[Symbol.species]: …}`, then the
        // species callback pushes and makes `length` non-writable mid-splice —
        // the dense Vec arm sees none of it). The two arms are not two paths to
        // the same answer; they are two implementations of an observable
        // protocol, and only the abstract one implements all of it.
        if matches!(
            name,
            "map" | "filter" | "forEach" | "every" | "some" | "reduce" | "reduceRight"
        ) && (self.arr_props.contains_key(&idx)
            || self.array_js_len.contains_key(&idx)
            || self.array_has_holes(idx))
        {
            return self.array_like_iterate(Value::heap(idx), name, args);
        }
        // Likewise route the SEARCH methods off the dense fast path when the array
        // carries a side table (a defineProperty'd index accessor must have its getter
        // invoked) or a virtual (sparse) length. A REPLACED prototype also routes
        // unconditionally: its HasProperty/Get are observable (a Proxy `has` trap) for
        // every index the array does not own — including the indices a throwing or
        // mutating `fromIndex` coercion has just removed, which the dense scan can
        // never probe because it walks the LIVE Vec rather than the length captured
        // before the coercion. On the plain Array.prototype chain only a HOLE exposes
        // the prototype, so that case stays gated behind the cheap proto-index flag —
        // the dense scans below handle holes in place (skip for indexOf/lastIndexOf,
        // read-as-undefined for includes) instead of paying a full-array hole
        // pre-scan on EVERY call.
        if matches!(name, "indexOf" | "lastIndexOf" | "includes")
            && (self.array_elements_overlaid(idx)
                || self.array_js_len.contains_key(&idx)
                || self.proto_of.contains_key(&idx)
                || (self.array_proto_has_index && self.array_has_holes(idx)))
        {
            return self.array_like_search(Value::heap(idx), name, args);
        }
        // push/pop/shift/unshift/splice end with Set(O,"length",…,true); on a FROZEN
        // array `length` is non-writable, so they throw a TypeError — even when no
        // element changes (pop/shift on an empty array, push/unshift with no args,
        // splice() with no args still set `length`). (A SEALED-but-not-frozen array
        // keeps `length` writable, so it is not gated here; its add/delete failures
        // are a separate concern.)
        // `fill` and `copyWithin` write elements with Set(O, k, v, THROW). The
        // dense arms below store into `items[i]` directly, which cannot fail —
        // so a frozen array was silently overwritten and a sealed or
        // non-extensible one silently ignored. Route those receivers to the
        // generic array-like helpers, which go through the observable
        // Get/Set/HasProperty path and throw where the spec says to. The dense
        // arms stay for the overwhelmingly common unconstrained array.
        // (Likewise an element carrying a defineProperty'd override — a
        // non-writable one must make the Set throw, which a raw store skips.)
        if matches!(name, "fill" | "copyWithin")
            && (self
                .arr_props
                .get(&idx)
                .map_or(false, |m| m.is_frozen() || m.is_sealed() || !m.extensible)
                || self.array_elements_overlaid(idx))
        {
            let this = Value::heap(idx);
            return if name == "fill" {
                self.array_like_fill(this, args)
            } else {
                self.array_like_copy_within(this, args)
            };
        }
        // (The explicit `frozen` flag, not the vacuous `is_frozen()`: a merely
        // non-extensible array's empty side table has no attrs to disprove it,
        // yet its `length` stays writable and its elements deletable.)
        if matches!(name, "push" | "pop" | "shift" | "unshift" | "splice")
            && (self.arr_props.get(&idx).map_or(false, |m| m.frozen)
                || self.array_length_nonwritable.contains(&idx))
        {
            return Err(Thrown(
                "TypeError: Cannot assign to read only property 'length' of object '[object Array]'".into(),
            ));
        }
        // A push onto a sealed or non-extensible array Sets a NEW index, which
        // must be rejected: the generic protocol performs (and fails) that Set.
        if name == "push" && self.arr_props.get(&idx).is_some_and(|m| !m.extensible) {
            return self.array_like_mutate(Value::heap(idx), name, args);
        }
        // pop/shift read an element via the spec Get. When that element is a HOLE in
        // the array's own storage, Get defers to the prototype chain — a prototype
        // accessor there can run arbitrary code (e.g. freeze the array mid-operation),
        // which the fast Vec path would miss. Route such cases to the abstract path.
        if name == "pop" || name == "shift" {
            if let HeapObj::Array(items) = self.heap.get(idx) {
                let probe = if name == "pop" {
                    items.len().checked_sub(1)
                } else {
                    Some(0)
                };
                if probe.map_or(false, |p| items.get(p).is_some_and(|v| v.is_hole())) {
                    return self.array_like_mutate(Value::heap(idx), name, args);
                }
            }
        }
        // The in-place mutators move elements through the raw Vec — correct only
        // when every slot is a plain own data element. A side table (accessor/
        // attribute overrides), or holes that an inherited prototype index could
        // cover, must run the spec HasProperty/Get/Set/Delete protocol (same
        // gating as the callback and search families above): a `Vec::remove`/
        // `Vec::splice`/`Vec::pop` cannot see a non-writable element (its Set must
        // throw) nor a non-configurable one (its DeletePropertyOrThrow must throw).
        if matches!(name, "shift" | "reverse" | "pop" | "unshift" | "splice")
            && (self.arr_props.contains_key(&idx)
                || (self.array_has_holes(idx)
                    && (self.array_proto_has_index || self.proto_of.contains_key(&idx))))
        {
            return self.array_like_mutate(Value::heap(idx), name, args);
        }
        // push/unshift/splice Set a NEW index; when a prototype carries integer
        // indices, that Set may hit a prototype setter (OrdinarySet, handled by
        // set_index via the abstract al_set path). The fast Vec paths below bypass
        // set_index, so route to the abstract path then. Gated on the flag, so the
        // common fast path stands.
        if matches!(name, "push" | "unshift" | "splice") && self.array_proto_has_index {
            return self.array_like_mutate(Value::heap(idx), name, args);
        }
        // A VIRTUAL-length (sparse) array's mutators must read and write `length`
        // through the side table and place elements via the abstract protocol —
        // e.g. a push at length 2^32-1 stores a NAMED prop ("4294967295" is not an
        // array index) and then the length set to 2^32 throws RangeError, per
        // ArraySetLength. The dense Vec fast paths below would use items.len().
        if matches!(
            name,
            "push" | "pop" | "shift" | "unshift" | "splice" | "reverse"
        ) && self.array_js_len.contains_key(&idx)
        {
            return self.array_like_mutate(Value::heap(idx), name, args);
        }
        match name {
            "push" => {
                // Nursery barrier: B119's dominant idiom — young values pushed
                // into a retained array (value-tested per arg, so number-only
                // pushes never dirty a large old array).
                for &a in args {
                    self.heap.write_barrier_val(idx, a);
                }
                let mut last = Value::UNDEFINED;
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    for a in args {
                        items.push(*a);
                    }
                    last = Value::int(items.len() as i32);
                }
                Ok(Some(last))
            }
            "pop" => {
                let value = match self.heap.get_mut(idx) {
                    HeapObj::Array(items) => items.pop().unwrap_or(Value::UNDEFINED),
                    _ => Value::UNDEFINED,
                };
                // ForInKeys snapshots the keyset. A pop can remove its last
                // index, so invalidate the snapshot-version liveness guard.
                self.heap.bump_version(idx);
                Ok(Some(value))
            }
            "shift" => {
                let value = match self.heap.get_mut(idx) {
                    HeapObj::Array(items) if !items.is_empty() => items.remove(0),
                    _ => Value::UNDEFINED,
                };
                // Shifting can remove the old last index (and move holes).
                self.heap.bump_version(idx);
                Ok(Some(value))
            }
            "unshift" => {
                // Nursery barrier (see `push`).
                for &a in args {
                    self.heap.write_barrier_val(idx, a);
                }
                // Prepend all args (preserving order) and return the new length.
                let len = if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    for (i, &v) in args.iter().enumerate() {
                        items.insert(i, v);
                    }
                    items.len()
                } else {
                    0
                };
                self.heap.bump_version(idx);
                Ok(Some(len_value(len)))
            }
            // `Array.prototype.toString()` is `join()` with the default "," sep.
            "join" | "toString" => {
                self.with_array_stringify_guard(idx, |vm| {
                    // A TypedArray receiver (TypedArray.prototype.toString IS
                    // Array.prototype.toString) goes through this.join. Validate
                    // it before any element reads.
                    if matches!(vm.heap.get(idx), HeapObj::TypedArray { .. })
                        && vm.ta_effective_len(idx).is_none()
                    {
                        return Err(Thrown(
                            "TypeError: TypedArray is detached or out of bounds".into(),
                        ));
                    }
                    // LengthOfArrayLike precedes separator coercion.
                    let len = match vm.heap.get(idx) {
                        HeapObj::Array(items) => items.len(),
                        _ => 0,
                    };
                    // The separator and every part are taken as EXACT strings:
                    // `['\uD83D', '\uDE00'].join('')` is the astral character
                    // they spell, and `s.split('').join('')` is `s` again. The
                    // lossy `String` form turned every lone surrogate into
                    // U+FFFD, which is the everyday corruption behind
                    // `split('').reverse().join('')` on emoji text.
                    let sep = if name == "toString" || arg0 == Value::UNDEFINED {
                        crate::heap::JsStr::new(",".to_string())
                    } else {
                        vm.to_js_str_owned(arg0)?
                    };
                    // Get(O,k) and ToString(element) interleave. The active-path
                    // guard is shared across nested ToString calls. Built as
                    // WTF-8, so surrogate halves in separate elements pair up.
                    let side_table = vm.arr_props.contains_key(&idx);
                    let mut out: Vec<u8> = Vec::new();
                    for k in 0..len {
                        let v = if side_table {
                            vm.array_iter_get(Value::heap(idx), k)?
                        } else {
                            vm.array_dense_or_proto_get(idx, k)?
                        };
                        let v = v.unwrap_or(Value::UNDEFINED);
                        if k != 0 {
                            vm.append_guest_wtf8(&mut out, sep.as_bytes())?;
                        }
                        if !v.is_nullish() {
                            vm.append_guest_tostring(&mut out, v)?;
                        }
                    }
                    Ok(Some(vm.alloc_wtf8(out)))
                })
            }
            "at" => {
                // Negative index counts from the end; out of range → undefined.
                // LengthOfArrayLike precedes ToIntegerOrInfinity(index).
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let i = self.to_integer_or_zero(arg0)?;
                let abs = if i < 0 { i + len as i64 } else { i };
                let v = if abs >= 0 && (abs as usize) < len {
                    // Get(O, k): a hole resolves through the prototype chain
                    // (`Array.prototype[k]`), as a plain `a[k]` does, and an
                    // index accessor in the side table runs its getter.
                    let k = abs as usize;
                    let got = if self.arr_props.contains_key(&idx) {
                        self.array_iter_get(Value::heap(idx), k)?
                    } else {
                        self.array_dense_or_proto_get(idx, k)?
                    };
                    got.unwrap_or(Value::UNDEFINED)
                } else {
                    Value::UNDEFINED
                };
                Ok(Some(v))
            }
            "indexOf" => {
                // In-place scan over the live Vec — array_snapshot would COPY the
                // whole array per call (an O(len) memcpy that dominates repeated
                // searches on big arrays). Safe here: this dense path is hole-free
                // (holey/side-table arrays were routed to array_like_search above)
                // and `===` runs no user code, so the Vec cannot change mid-scan.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i64,
                    _ => 0,
                };
                // len === 0 short-circuits to -1 BEFORE ToIntegerOrInfinity(fromIndex),
                // so a throwing fromIndex.valueOf must not run (spec step 2).
                if len == 0 {
                    return Ok(Some(Value::int(-1)));
                }
                // Optional fromIndex (ToInteger; negative counts from the end).
                let from = if args.len() >= 2 {
                    let f = self.to_integer_or_zero(args[1])?;
                    if f < 0 {
                        (len + f).max(0)
                    } else {
                        f.min(len)
                    }
                } else {
                    0
                } as usize;
                let pos = match self.heap.get(idx) {
                    HeapObj::Array(items) => (from..items.len()).find(|&i| {
                        // A hole fails HasProperty (no proto index can cover it
                        // on this gated plain-chain path) — spec skips it.
                        !items[i].is_hole() && self.values_strict_eq(items[i], arg0)
                    }),
                    _ => None,
                };
                Ok(Some(Value::int(pos.map(|p| p as i32).unwrap_or(-1))))
            }
            "includes" => {
                // In-place scan (no snapshot copy) — see "indexOf" for why this
                // is safe on the hole-free dense path.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i64,
                    _ => 0,
                };
                // len === 0 short-circuits to false BEFORE ToIntegerOrInfinity(fromIndex),
                // so a throwing fromIndex.valueOf must not run (spec step 2).
                if len == 0 {
                    return Ok(Some(Value::bool(false)));
                }
                // fromIndex (ToIntegerOrInfinity): negative counts from the end
                // (clamped to 0); +Infinity → past the end (never found); -Infinity → 0.
                let from = if args.len() >= 2 {
                    let n = self.to_integer_or_zero(args[1])?;
                    if n >= 0 {
                        n
                    } else {
                        len.saturating_add(n).max(0)
                    }
                } else {
                    0
                };
                // SameValueZero (NaN matches NaN; +0/-0 equal) — not strict `===`.
                let (found, live_len) = match self.heap.get(idx) {
                    HeapObj::Array(items) => {
                        let hi = items.len().min(len as usize);
                        let from = (from.max(0) as usize).min(hi);
                        let found = items[from..hi].iter().any(|&v| {
                            // includes Gets each index: a hole reads as undefined
                            // (so `[,].includes(undefined)` is true).
                            let v = if v.is_hole() { Value::UNDEFINED } else { v };
                            self.same_value_zero(v, arg0)
                        });
                        (found, items.len())
                    }
                    _ => (false, 0),
                };
                if found {
                    return Ok(Some(Value::bool(true)));
                }
                // The scan is bounded by the length read at entry, not the live
                // Vec: a fromIndex valueOf that shortened the array leaves
                // indices it no longer holds, and includes still Gets each one
                // (`undefined`, or an inherited element).
                let mut k = from.max(live_len as i64);
                while k < len {
                    let v = self.get_index(Value::heap(idx), Value::num(k as f64))?;
                    if self.same_value_zero(v, arg0) {
                        return Ok(Some(Value::bool(true)));
                    }
                    k += 1;
                }
                Ok(Some(Value::bool(false)))
            }
            "lastIndexOf" => {
                // In-place scan (no snapshot copy) — see "indexOf" for why this
                // is safe on the hole-free dense path.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i64,
                    _ => 0,
                };
                // len === 0 short-circuits to -1 BEFORE ToIntegerOrInfinity(fromIndex),
                // so a throwing fromIndex.valueOf must not run (spec step 2).
                if len == 0 {
                    return Ok(Some(Value::int(-1)));
                }
                // fromIndex defaults to len-1 (search from the end); negative
                // counts from the end. ToInteger.
                let from = if args.len() >= 2 {
                    let f = self.to_integer_or_zero(args[1])?;
                    if f < 0 {
                        len + f
                    } else {
                        f.min(len - 1)
                    }
                } else {
                    len - 1
                };
                let mut result = -1i32;
                if from >= 0 {
                    if let HeapObj::Array(items) = self.heap.get(idx) {
                        if !items.is_empty() {
                            let hi = (from as usize).min(items.len() - 1);
                            for i in (0..=hi).rev() {
                                // A hole fails HasProperty on this plain-chain
                                // path — spec skips it (see "indexOf").
                                if !items[i].is_hole() && self.values_strict_eq(items[i], arg0) {
                                    result = i as i32;
                                    break;
                                }
                            }
                        }
                    }
                }
                Ok(Some(Value::int(result)))
            }
            "reverse" => {
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    items.reverse();
                }
                // A hole can move onto a snapshotted index, making that key
                // disappear even though the array length is unchanged.
                self.heap.bump_version(idx);
                Ok(Some(Value::heap(idx))) // reverses in place, returns the array
            }
            "concat" => {
                // New array = `this` ++ each arg. An element is spread one level
                // iff IsConcatSpreadable (a `Symbol.isConcatSpreadable` flag, else
                // IsArray) — so an array-like with the flag spreads, and an array
                // with the flag cleared is added whole. Both `this` and the args
                // are subject to the check.
                let this_val = Value::heap(idx);
                // ArraySpeciesCreate(O, 0) is step 2: its constructor/@@species
                // Gets precede every @@isConcatSpreadable Get below.
                let species_target = self.array_species_create(this_val, 0)?;
                let mut out: Vec<Value> = Vec::new();
                // Spec `n`, the next index on the result. With a species target the
                // element is defined the moment it is read (step 5.c.iv is inside
                // the element loop) — a target whose defineProperty mutates the
                // SOURCE must be observed by the later reads; buffering every Get
                // first made those reads stale.
                let mut n: usize = 0;
                for e in std::iter::once(this_val).chain(args.iter().copied()) {
                    if self.is_concat_spreadable(e)? {
                        // A CLEAN real array spreads via its dense storage (fast).
                        // One with a side table (accessors), an arguments object,
                        // or holes runs the spec HasProperty+Get per index —
                        // accessors fire and ABSENT indices stay absent (HOLE).
                        // The JS length: a virtual array's store is only a prefix.
                        let arr_n = if e.is_heap() {
                            match self.heap.get(e.heap_index()) {
                                HeapObj::Array(_) => Some(self.js_array_len(e.heap_index())),
                                _ => None,
                            }
                        } else {
                            None
                        };
                        if let Some(elen) = arr_n {
                            let eidx = e.heap_index();
                            if species_target.is_none()
                                && !self.array_is_virtual(eidx)
                                && !self.arr_props.contains_key(&eidx)
                                && !self.arguments_objs.contains_key(&eidx)
                                && !self.array_has_holes(eidx)
                            {
                                // Admit the result's growth before copying into
                                // it: N references to one large array are N
                                // times its size in one native step. The copy
                                // comes straight from the (hole-free) store.
                                self.reserve_array_result(&mut out, elen, false)?;
                                if let HeapObj::Array(items) = self.heap.get(eidx) {
                                    out.extend_from_slice(items);
                                }
                                n = out.len();
                            } else {
                                if species_target.is_some() {
                                    self.preflight_native_iteration_work(elen as u64)?;
                                } else {
                                    // A virtual array is sized by a length, not by
                                    // stored elements.
                                    let virt = self.array_is_virtual(eidx);
                                    self.reserve_array_result(&mut out, elen, virt)?;
                                }
                                for k in 0..elen {
                                    let v = self.array_iter_get(e, k)?.unwrap_or(Value::HOLE);
                                    self.concat_emit(species_target, &mut out, &mut n, v)?;
                                }
                            }
                        } else {
                            let len_v = self.get_prop(e, "length")?;
                            let len = self.to_integer_or_zero(len_v)?.clamp(0, (1i64 << 53) - 1);
                            // Step 5.c.iii: n + len > 2^53-1 is a TypeError BEFORE
                            // any element read (a MAX_SAFE_INTEGER-length spreadable
                            // must not loop 9e15 Gets).
                            if n as i64 + len > (1i64 << 53) - 1 {
                                return Err(Thrown(
                                    "TypeError: concat result length exceeds 2**53 - 1".into(),
                                ));
                            }
                            if species_target.is_none() {
                                let len = usize::try_from(len).unwrap_or(usize::MAX);
                                self.reserve_array_result(&mut out, len, true)?;
                            } else {
                                self.preflight_native_iteration_work(len as u64)?;
                            }
                            for k in 0..len {
                                // Step 5.c.iv: only a PRESENT index is copied
                                // (HasProperty, a Proxy `has` trap included); an
                                // absent one stays a hole in the result.
                                let key = k.to_string();
                                let el = if self.has_property_str_dyn(e, &key)? {
                                    self.get_prop(e, &key)?
                                } else {
                                    Value::HOLE
                                };
                                self.concat_emit(species_target, &mut out, &mut n, el)?;
                            }
                        }
                    } else {
                        self.concat_emit(species_target, &mut out, &mut n, e)?;
                    }
                }
                match species_target {
                    // ArrayCreate in the CURRENT realm — see alloc_array_current_realm.
                    None => Ok(Some(self.alloc_array_current_realm(out))),
                    Some(a) => {
                        // Step 6: Set(A, "length", n, true).
                        self.set_prop(a, "length", Value::num(n as f64), true)?;
                        Ok(Some(a))
                    }
                }
            }
            "flat" => {
                // FlattenIntoArray over the live receiver, as flatMap and the
                // array-like arm do: absent indices (holes, top-level or nested)
                // are skipped rather than copied as `undefined`, a hole reads an
                // inherited index, a nested Proxy over an array is flattened,
                // and the result's growth is admitted as it is built. The
                // snapshot-and-clone flattener did none of these.
                let receiver = Value::heap(idx);
                let source_len = self.js_array_len(idx);
                // An absent OR explicitly-`undefined` depth defaults to 1
                // (ToIntegerOrInfinity is only applied to a provided depth;
                // Infinity saturates -> deep flatten).
                let depth = if args.is_empty() || arg0 == Value::UNDEFINED {
                    1
                } else {
                    self.to_integer_or_zero(arg0)?.max(0)
                };
                let target = self.array_species_create(receiver, 0)?;
                self.flatten_into_array_result(receiver, source_len, depth, None, target)
                    .map(Some)
            }
            "fill" => {
                let val = arg0;
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let s0 = if args.len() >= 2 {
                    self.to_integer_or_zero(args[1])?
                } else {
                    0
                };
                // An absent OR explicitly-`undefined` end defaults to the length.
                let e0 = if args.len() >= 3 && args[2] != Value::UNDEFINED {
                    self.to_integer_or_zero(args[2])?
                } else {
                    len as i64
                };
                let start = norm_index(s0.clamp(i32::MIN as i64, i32::MAX as i64) as i32, len);
                let end = norm_index(e0.clamp(i32::MIN as i64, i32::MAX as i64) as i32, len);
                // Nursery barrier: one value, any number of slots.
                self.heap.write_barrier_val(idx, val);
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    let n = items.len() as i32; // re-clamp (a coercion valueOf may have resized)
                    for i in start..end.min(n) {
                        items[i as usize] = val;
                    }
                }
                Ok(Some(Value::heap(idx)))
            }
            "slice" => {
                // LengthOfArrayLike precedes the start/end coercions.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let s0 = if args.is_empty() {
                    0
                } else {
                    self.to_integer_or_zero(arg0)?
                };
                // An absent OR explicitly-`undefined` end defaults to the length.
                let e0 = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    None
                } else {
                    Some(self.to_integer_or_zero(args[1])?)
                };
                let start = norm_index(s0.clamp(i32::MIN as i64, i32::MAX as i64) as i32, len);
                let end = match e0 {
                    None => len,
                    Some(e) => norm_index(e.clamp(i32::MIN as i64, i32::MAX as i64) as i32, len),
                };
                // A coercion that resized the array leaves the range to be read
                // per index (HasProperty + Get): an index it removed becomes a
                // hole in a result that is still `end - start` long.
                let live_len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let clean = live_len == len
                    && !self.arr_props.contains_key(&idx)
                    && !self.array_has_holes(idx);
                let slice: Vec<Value> = if start < end {
                    if clean {
                        match self.heap.get(idx) {
                            HeapObj::Array(items) => items[start as usize..end as usize].to_vec(),
                            _ => Vec::new(),
                        }
                    } else {
                        // Spec copy: HasProperty(k) (proto-aware, accessors fire)
                        // then Get; an ABSENT index stays absent in the result.
                        let mut v = Vec::with_capacity((end - start) as usize);
                        for k in start..end {
                            match self.array_iter_get(Value::heap(idx), k as usize)? {
                                Some(x) => v.push(x),
                                None => v.push(Value::HOLE),
                            }
                        }
                        v
                    }
                } else {
                    Vec::new()
                };
                // slice does ArraySpeciesCreate(O, count) where count == slice.len().
                let n = slice.len();
                Ok(Some(self.array_from_species_len(
                    Value::heap(idx),
                    slice,
                    n,
                    true,
                )?))
            }
            "map" => self.array_each(
                idx,
                arg0,
                EachMode::Map,
                args.get(1).copied().unwrap_or(Value::UNDEFINED),
            ),
            "filter" => self.array_each(
                idx,
                arg0,
                EachMode::Filter,
                args.get(1).copied().unwrap_or(Value::UNDEFINED),
            ),
            "forEach" => self.array_each(
                idx,
                arg0,
                EachMode::ForEach,
                args.get(1).copied().unwrap_or(Value::UNDEFINED),
            ),
            // Short-circuiting callback searches. They stop at the first match, so
            // they use call_value directly (the all-elements array_each driver
            // doesn't fit); the callback receives (element, index).
            "find" | "findIndex" | "some" | "every" => {
                let cb = arg0;
                // IsCallable(callback) is checked before any iteration, so an empty
                // array with a non-callable predicate still throws (spec step 3/4).
                if !self.is_callable(cb) {
                    return Err(Thrown(format!(
                        "TypeError: {name} predicate is not a function"
                    )));
                }
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let receiver = Value::heap(idx);
                // `len` is captured once; each element is read LIVE (a callback may
                // mutate it or shorten the array). For an index now past the live
                // length: `find`/`findIndex` still visit it with `undefined` (they do
                // not HasProperty-skip), while `some`/`every` skip it.
                let len = self.array_snapshot(idx).len();
                for i in 0..len {
                    let v = match self.array_dense_or_proto_get(idx, i)? {
                        Some(v) => v,
                        None => {
                            if name == "some" || name == "every" {
                                continue;
                            }
                            Value::UNDEFINED
                        }
                    };
                    let r = self.call_value(cb, this_arg, &[v, Value::int(i as i32), receiver])?;
                    let t = self.truthy(r);
                    match name {
                        "find" if t => return Ok(Some(v)),
                        "findIndex" if t => return Ok(Some(Value::int(i as i32))),
                        "some" if t => return Ok(Some(Value::bool(true))),
                        "every" if !t => return Ok(Some(Value::bool(false))),
                        _ => {}
                    }
                }
                Ok(Some(match name {
                    "find" => Value::UNDEFINED,
                    "findIndex" => Value::int(-1),
                    "some" => Value::bool(false),
                    _ => Value::bool(true), // every: all matched (or empty)
                }))
            }
            "reduce" => {
                let cb = arg0;
                if !self.is_callable(cb) {
                    return Err(Thrown(
                        "TypeError: Reduce callback is not a function".into(),
                    ));
                }
                let has_init = args.len() >= 2;
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                let snapshot = self.array_snapshot(idx);
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                let (snapshot_len, first) = (snapshot.len(), snapshot.first().copied());
                // The non-JIT reducer reads every processed element live. Capture
                // only the initial length and no-initial-value seed instead of
                // cloning the complete dense store. Arrays with holes, overlays,
                // or a virtual length were routed to array_like_iterate above.
                #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
                let (snapshot_len, first) = match self.heap.get(idx) {
                    HeapObj::Array(items) => (items.len(), items.first().copied()),
                    _ => (0, None),
                };
                // Seed + first index to process: with an initial value, start at
                // element 0; otherwise the first element seeds and we start at 1.
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                let mut start = if has_init { 0 } else { 1 };
                #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
                let start = if has_init { 0 } else { 1 };
                let mut acc = if has_init {
                    args[1]
                } else if let Some(first) = first {
                    first
                } else {
                    return Err(Thrown(
                        "TypeError: Reduce of empty array with no initial value".into(),
                    ));
                };
                #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
                let numeric_callback =
                    self.array_numeric_callback_plan(cb, ArrayNumericCallback::ReduceAdd);

                // Fused native reduce kernel: inline the `(acc, element)`
                // callback into a native loop over the leading numeric run — no
                // per-element call. On a guard bail it returns the index reached
                // and the accumulated value (via the in/out acc pointer); the
                // per-element tail below finishes `[start, len)` correctly.
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                if self.jit_fused_ok()
                    && self.jit_recurse_depth == 0
                    && cb.is_heap()
                    && start < snapshot_len
                {
                    if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                        if ups.is_empty() {
                            let proto: *const crate::bytecode::FuncProto = self.func(fid as usize);
                            // SAFETY: immutable program functions; raw ptr dodges
                            // the jit-vs-program borrow conflict (as elsewhere).
                            let proto_ref = unsafe { &*proto };
                            let reg_count = (proto_ref.reg_count as usize).max(3);
                            // The reduce kernel passes only (acc, element); a
                            // callback that declares the 3rd (index) / 4th (array)
                            // param or uses `arguments` must take the per-element
                            // path so those args are supplied.
                            let kernel_entry = if proto_ref.param_count >= 3
                                || proto_ref.arguments_reg.is_some()
                            {
                                None
                            } else {
                                self.jit.reduce_kernel(fid, proto_ref)
                            };
                            if let Some(entry) = kernel_entry {
                                let win = self.regs.len();
                                if !self.regs_would_overflow(win + reg_count) {
                                    self.regs.resize(win + reg_count, Value::UNDEFINED);
                                    let count = snapshot_len - start;
                                    let window_ptr =
                                        unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
                                    let snap_ptr =
                                        unsafe { snapshot.as_ptr().add(start) } as *const u64;
                                    let mut acc_bits = acc.bits();
                                    // SAFETY: valid win64 reduce kernel; window has
                                    // reg_count slots; acc_bits is a live u64;
                                    // call-free ⇒ none of these pointers move.
                                    let kernel: extern "win64" fn(
                                        *mut u64,
                                        *const u64,
                                        usize,
                                        *mut u64,
                                    )
                                        -> usize = unsafe { core::mem::transmute(entry) };
                                    let processed = kernel(
                                        window_ptr,
                                        snap_ptr,
                                        count,
                                        &mut acc_bits as *mut u64,
                                    );
                                    acc = Value::from_bits(acc_bits);
                                    self.regs.truncate(win);
                                    start += processed;
                                }
                            }
                        }
                    }
                }

                // Per-element tail: the whole array if no kernel ran, or just the
                // remainder after a kernel bail (nothing if it completed).
                let run_tail = start < snapshot_len;
                let mut native = if run_tail {
                    self.native_cb_entry(cb)
                } else {
                    None
                };
                let win = self.regs.len();
                if let Some((_, callee_regs, _)) = native {
                    if self.regs_would_overflow(win + callee_regs) {
                        native = None;
                    } else {
                        self.regs.resize(win + callee_regs, Value::UNDEFINED);
                    }
                }
                let mut err = None;
                let receiver = Value::heap(idx);
                for i in start..snapshot_len {
                    // Live read; skip an index now past the live length (the callback
                    // shortened the array) — reduce HasProperty-skips absent indices.
                    let v = match self.array_dense_or_proto_get(idx, i)? {
                        Some(v) => v,
                        None => continue,
                    };
                    let direct = {
                        #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
                        {
                            numeric_callback
                                .and_then(|plan| self.try_array_numeric_callback(plan, acc, v))
                        }
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        {
                            None
                        }
                    };
                    let callback_result = match direct {
                        Some(value) => Ok(value),
                        None => {
                            let cbargs = [acc, v, Value::int(i as i32), receiver];
                            self.run_cb_elem(native, win, cb, &cbargs, Value::UNDEFINED)
                        }
                    };
                    match callback_result {
                        Ok(r) => acc = r,
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                }
                if native.is_some() {
                    self.regs.truncate(win);
                }
                if let Some(e) = err {
                    return Err(e);
                }
                Ok(Some(acc))
            }
            "sort" => {
                let cmp = arg0;
                // A non-undefined, non-callable comparator is a TypeError.
                if cmp != Value::UNDEFINED && !self.is_callable(cmp) {
                    return Err(Thrown(
                        "TypeError: The comparison function must be either a function or undefined"
                            .into(),
                    ));
                }
                let receiver = Value::heap(idx);
                // Fast path: a plain dense array (no side table, no holes) — every
                // index 0..len is an own present element, so the raw backing slice is
                // observably identical to the [[Get]]/[[Set]] protocol. Keeps the hot
                // path at snapshot speed.
                let fast = match self.heap.get(idx) {
                    HeapObj::Array(items) => {
                        !self.arr_props.contains_key(&idx)
                            && !self.array_is_virtual(idx)
                            && items.iter().all(|v| !v.is_hole())
                    }
                    _ => false,
                };
                if fast {
                    let mut snapshot = match self.heap.get(idx) {
                        HeapObj::Array(items) => items.clone(),
                        _ => Vec::new(),
                    };
                    let n = snapshot.len();
                    self.sort_values(&mut snapshot, cmp)?;
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        if items.len() > n {
                            // A comparator that appended to the array: the sort
                            // writes indices 0..len only, so what it added past
                            // them stays.
                            items[..n].copy_from_slice(&snapshot);
                        } else {
                            *items = snapshot;
                        }
                    }
                    return Ok(Some(receiver));
                }
                // SortIndexedProperties via the [[Get]]/[[Set]]/[[Delete]] protocol:
                // own/inherited accessor INDICES fire their getters/setters, holes read
                // their prototype value, and a getter that mutates the array mid-sort is
                // observed. `len` is read ONCE up front (LengthOfArrayLike) — the JS
                // length, which for a virtual array is past its dense store.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(_) => self.js_array_len(idx),
                    _ => {
                        let lv = self.get_prop(receiver, "length")?;
                        let n = self.to_number_strict(lv)?;
                        if n.is_nan() || n <= 0.0 {
                            0
                        } else {
                            n.min((u32::MAX as f64) - 1.0) as usize
                        }
                    }
                };
                // A VIRTUAL array with nothing that could observe an absent
                // index visits only the indices it holds: the same values in
                // the same order, without an O(length) walk over its holes.
                let virtual_present = if self.array_is_virtual(idx) {
                    self.virtual_array_present(idx, 0, len)
                } else {
                    None
                };
                let sparse = virtual_present.is_some();
                let mut gathered = Vec::new();
                match virtual_present {
                    Some(present) => {
                        for i in present {
                            gathered.push(self.get_index(receiver, Value::num(i as f64))?);
                        }
                    }
                    None => {
                        for i in 0..len {
                            // array_iter_get = ? HasProperty(O,i) ? ? Get(O,i) : skip.
                            if let Some(v) = self.array_iter_get(receiver, i)? {
                                gathered.push(v);
                            }
                        }
                    }
                }
                let item_count = gathered.len();
                self.sort_values(&mut gathered, cmp)?;
                for (j, v) in gathered.into_iter().enumerate() {
                    self.set_index(receiver, Value::num(j as f64), v, true)?;
                }
                // Deleting an index the array does not hold is a no-op, so a
                // sparse array deletes only the ones it holds NOW (a comparator
                // may have added some), in the same ascending order.
                let doomed = if sparse {
                    self.virtual_array_present(idx, item_count, len)
                } else {
                    None
                };
                match doomed {
                    Some(present) => {
                        for j in present {
                            self.delete_property(receiver, &j.to_string())?;
                        }
                    }
                    None => {
                        for j in item_count..len {
                            self.delete_property(receiver, &j.to_string())?;
                        }
                    }
                }
                Ok(Some(receiver))
            }
            "reduceRight" => {
                let cb = arg0;
                if !self.is_callable(cb) {
                    return Err(Thrown(
                        "TypeError: Reduce callback is not a function".into(),
                    ));
                }
                let snapshot = self.array_snapshot(idx);
                let mut i = snapshot.len();
                let mut acc = if args.len() >= 2 {
                    args[1]
                } else if i > 0 {
                    i -= 1;
                    snapshot[i]
                } else {
                    return Err(Thrown(
                        "TypeError: Reduce of empty array with no initial value".into(),
                    ));
                };
                let receiver = Value::heap(idx);
                while i > 0 {
                    i -= 1;
                    // Live read; skip an index now past the live length (a callback
                    // shortened the array) — reduceRight HasProperty-skips absent ones.
                    let v = match self.array_dense_or_proto_get(idx, i)? {
                        Some(v) => v,
                        None => continue,
                    };
                    acc = self.call_value(
                        cb,
                        Value::UNDEFINED,
                        &[acc, v, Value::int(i as i32), receiver],
                    )?;
                }
                Ok(Some(acc))
            }
            "flatMap" => {
                // The same FlattenIntoArray walk the array-LIKE receiver takes
                // (see the `flat`/`flatMap` arm above), not a dense snapshot:
                // step 5.b is `HasProperty(source, P)` on the LIVE receiver, so
                // a mapper that shrinks the array — `a.flatMap(e => { a.length
                // = 3; return e; })` — must stop visiting the removed indices.
                let receiver = Value::heap(idx);
                // LengthOfArrayLike precedes the IsCallable check (step 2, then 3).
                let lv = self.get_prop(receiver, "length")?;
                let lf = self.to_number_strict(lv)?;
                let source_len = if lf.is_nan() || lf <= 0.0 {
                    0usize
                } else {
                    lf.trunc().min(9_007_199_254_740_991.0) as usize
                };
                let cb = arg0;
                if !self.is_callable(cb) {
                    return Err(Thrown("TypeError: flatMap mapper is not a function".into()));
                }
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                // ArraySpeciesCreate(O, 0) is step 4 — before the walk, and its
                // `constructor` / @@species Gets are observable there.
                let target = self.array_species_create(receiver, 0)?;
                self.flatten_into_array_result(receiver, source_len, 1, Some((cb, this_arg)), target)
                    .map(Some)
            }
            "findLast" | "findLastIndex" => {
                let cb = arg0;
                if !self.is_callable(cb) {
                    return Err(Thrown(format!(
                        "TypeError: {name} predicate is not a function"
                    )));
                }
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let receiver = Value::heap(idx);
                // `len` is captured once; each element is read LIVE (a callback may mutate
                // the array). findLast/findLastIndex visit EVERY index (no HasProperty
                // skip), so an absent/hole index is the inherited value or undefined.
                let len = self.array_snapshot(idx).len();
                for i in (0..len).rev() {
                    let v = self
                        .array_dense_or_proto_get(idx, i)?
                        .unwrap_or(Value::UNDEFINED);
                    let r = self.call_value(cb, this_arg, &[v, Value::int(i as i32), receiver])?;
                    if self.truthy(r) {
                        return Ok(Some(if name == "findLast" {
                            v
                        } else {
                            Value::int(i as i32)
                        }));
                    }
                }
                Ok(Some(if name == "findLast" {
                    Value::UNDEFINED
                } else {
                    Value::int(-1)
                }))
            }
            "toSorted" => {
                // Like sort() but returns a NEW array; the receiver is unchanged.
                let cmp = arg0;
                if cmp != Value::UNDEFINED && !self.is_callable(cmp) {
                    return Err(Thrown(
                        "TypeError: The comparison function must be either a function or undefined"
                            .into(),
                    ));
                }
                let mut snapshot = self.array_snapshot_get(idx)?;
                if self.is_callable(cmp) {
                    self.comparator_sort(&mut snapshot, cmp)?;
                } else {
                    // Default SortCompare, shared with sort(): code-unit order,
                    // undefined last.
                    snapshot = self.default_sort(snapshot)?;
                }
                Ok(Some(self.alloc_array_current_realm(snapshot)))
            }
            "toReversed" => {
                // Read in SPEC order: out[k] = Get(O, len-k-1). A snapshot-then-reverse
                // would read indices ascending, but a getter's side effect (e.g. it
                // shrinks the array) makes the read order observable, so the descending
                // `from` sequence must be honoured.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let this = Value::heap(idx);
                let mut out = Vec::with_capacity(len);
                for k in 0..len {
                    let from = len - k - 1;
                    out.push(self.array_iter_get(this, from)?.unwrap_or(Value::UNDEFINED));
                }
                Ok(Some(self.alloc_array_current_realm(out)))
            }
            "splice" => {
                // splice(start, deleteCount?, ...items): mutate in place, return
                // the removed elements (start may be negative).
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let s = self.to_integer_or_zero(arg0)?;
                let start = if s < 0 {
                    (len as i64 + s).max(0) as usize
                } else {
                    (s as usize).min(len)
                };
                // deleteCount: 0 args → 0; 1 arg → len-start; else ToInteger(arg1).
                let del = if args.is_empty() {
                    0
                } else if args.len() < 2 {
                    len - start
                } else {
                    let d = self.to_integer_or_zero(args[1])?;
                    (d.max(0) as usize).min(len - start)
                };
                let insert: Vec<Value> = args.get(2..).unwrap_or(&[]).to_vec();
                // Step 8: ArraySpeciesCreate(O, actualDeleteCount) precedes every
                // element read and the mutation itself — a `constructor`/@@species
                // getter, and the species constructor, must observe the array as it
                // was BEFORE the splice (they ran after it, seeing the new length).
                let species_target = self.array_species_create(Value::heap(idx), del)?;
                // Nursery barrier for the inserted values.
                for &v in &insert {
                    self.heap.write_barrier_val(idx, v);
                }
                let removed: Vec<Value> = match self.heap.get_mut(idx) {
                    // Re-clamp to the current length (a coercion valueOf, or the
                    // species constructor just called, may have resized).
                    HeapObj::Array(items) => {
                        let n = items.len();
                        let st = start.min(n);
                        let en = (start + del).min(n);
                        items.splice(st..en, insert).collect()
                    }
                    _ => Vec::new(),
                };
                self.heap.bump_version(idx); // length/contents changed
                let n = removed.len();
                match species_target {
                    // ArrayCreate in the CURRENT realm — see alloc_array_current_realm.
                    None => Ok(Some(self.alloc_array_current_realm(removed))),
                    Some(a) => {
                        for (i, v) in removed.into_iter().enumerate() {
                            // A HOLE marks an ABSENT source index: it stays absent
                            // on A rather than being defined as undefined.
                            if !v.is_hole() {
                                self.create_data_property_or_throw(a, i, v)?;
                            }
                        }
                        // Step 10: Set(A, "length", actualDeleteCount, true).
                        self.set_prop(a, "length", Value::num(n as f64), true)?;
                        Ok(Some(a))
                    }
                }
            }
            // Array iterators (real iterator objects with .next(), proto =
            // %ArrayIteratorPrototype%). values() is also the default @@iterator.
            // LIVE: each next() re-reads the array, so mutations made during
            // iteration are observed (the spec iterator is a generator over O).
            "values" => Ok(Some(self.make_live_iterator(idx, 1, self.array_iter_proto))),
            "keys" => Ok(Some(self.make_live_iterator(idx, 0, self.array_iter_proto))),
            "entries" => Ok(Some(self.make_live_iterator(idx, 2, self.array_iter_proto))),
            "toLocaleString" => {
                self.with_array_stringify_guard(idx, |vm| {
                    // Join each element's own toLocaleString(locales, options)
                    // with ","; nullish -> "".
                    let snapshot = if vm.arr_props.contains_key(&idx) || vm.array_has_holes(idx) {
                        vm.array_snapshot_get(idx)?
                    } else {
                        vm.array_snapshot(idx)
                    };
                    let fwd = [
                        args.first().copied().unwrap_or(Value::UNDEFINED),
                        args.get(1).copied().unwrap_or(Value::UNDEFINED),
                    ];
                    // Each result is ToString'd EXACTLY, as `join` does it.
                    let mut out: Vec<u8> = Vec::new();
                    for (i, v) in snapshot.into_iter().enumerate() {
                        if i != 0 {
                            vm.append_guest_wtf8(&mut out, b",")?;
                        }
                        if v.is_nullish() {
                            continue;
                        }
                        let f = vm.get_prop(v, "toLocaleString")?;
                        if !vm.is_callable(f) {
                            return Err(Thrown("TypeError: toLocaleString is not callable".into()));
                        }
                        let r = vm.call_value(f, v, &fwd)?;
                        vm.append_guest_tostring(&mut out, r)?;
                    }
                    Ok(Some(vm.alloc_wtf8(out)))
                })
            }
            "with" => {
                // with(index, value): a COPY with one index replaced. The index is
                // relative (negative from the end) and NOT clamped — an out-of-range
                // index throws a RangeError. Per spec the replaced index is set to
                // `value` WITHOUT a [[Get]]; every OTHER index is read via Get (so an
                // inherited `Array.prototype[k]` at a hole is visited, but the replaced
                // slot's getter is NOT invoked).
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                } as i64;
                let n = self.to_number_strict(arg0)?;
                let rel = if n.is_nan() { 0 } else { n.trunc() as i64 };
                let actual = if rel >= 0 { rel } else { len + rel };
                if actual < 0 || actual >= len {
                    return Err(Thrown("RangeError: Invalid index".into()));
                }
                let value = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let this = Value::heap(idx);
                let mut out = Vec::with_capacity(len as usize);
                for k in 0..len as usize {
                    if k as i64 == actual {
                        out.push(value);
                    } else {
                        out.push(self.array_iter_get(this, k)?.unwrap_or(Value::UNDEFINED));
                    }
                }
                Ok(Some(self.alloc_array_current_realm(out)))
            }
            "toSpliced" => {
                // Like splice() but returns the modified COPY; receiver unchanged.
                // Only Number arguments reach here (see the generic routing
                // above); `as i64` is their ToIntegerOrInfinity (NaN -> 0,
                // +/-Infinity saturate and then clamp).
                let mut out = self.array_snapshot_get(idx)?;
                let len = out.len();
                let s = if arg0.is_number() {
                    arg0.as_f64() as i64
                } else {
                    0
                };
                let start = if s < 0 {
                    (len as i64 + s).max(0) as usize
                } else {
                    (s as usize).min(len)
                };
                let del = if args.is_empty() {
                    // No start argument: skipCount/actualSkipCount are 0 — the
                    // result is an unchanged copy, NOT a delete-everything.
                    0
                } else if args.len() < 2 {
                    len - start
                } else {
                    let d = if args[1].is_number() {
                        args[1].as_f64() as i64
                    } else {
                        0
                    };
                    (d.max(0) as usize).min(len - start)
                };
                let insert: Vec<Value> = args.get(2..).unwrap_or(&[]).to_vec();
                out.splice(start..start + del, insert);
                Ok(Some(self.alloc_array_current_realm(out)))
            }
            "copyWithin" => {
                // A prototype index / accessor side table makes the per-index
                // Has/Get/Set/Delete protocol observable — route abstract.
                if self.arr_props.contains_key(&idx)
                    || self.array_proto_has_index
                    || self.proto_of.contains_key(&idx)
                {
                    return self.array_like_copy_within(Value::heap(idx), args);
                }
                // copyWithin(target, start, end?): copy the [start,end) slice over the
                // run beginning at target, in place. Reads from a raw snapshot
                // (HOLEs preserved) so overlapping ranges behave as if copied
                // from the original; a hole copies as a hole (delete).
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let i32c = |n: i64| n.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                let t0 = self.to_integer_or_zero(arg0)?;
                let s0 = if args.len() >= 2 {
                    self.to_integer_or_zero(args[1])?
                } else {
                    0
                };
                // An absent OR explicitly-`undefined` end defaults to the length.
                let e0 = if args.len() >= 3 && args[2] != Value::UNDEFINED {
                    self.to_integer_or_zero(args[2])?
                } else {
                    len as i64
                };
                let target = norm_index(i32c(t0), len);
                let start = norm_index(i32c(s0), len);
                let end = norm_index(i32c(e0), len);
                let count = (end - start).min(len - target).max(0);
                if count > 0 {
                    // A coerced arg's valueOf may have resized the array between
                    // capturing `len` and here: guard targets against the CURRENT
                    // length, and a now-out-of-range SOURCE deletes its target
                    // (HasProperty false → DeletePropertyOrThrow, = HOLE here).
                    let raw: Vec<Value> = match self.heap.get(idx) {
                        HeapObj::Array(items) => items.clone(),
                        _ => Vec::new(),
                    };
                    let snap_len = raw.len();
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        for k in 0..count {
                            let (ti, si) = ((target + k) as usize, (start + k) as usize);
                            if si < snap_len {
                                // Set(O, to, v): grows past a shrunk length.
                                if ti >= items.len() {
                                    items.resize(ti + 1, Value::HOLE);
                                }
                                items[ti] = raw[si];
                            } else if ti < items.len() {
                                // Absent source → DeletePropertyOrThrow(to).
                                items[ti] = Value::HOLE;
                            }
                        }
                    }
                    self.heap.bump_version(idx);
                }
                Ok(Some(Value::heap(idx)))
            }
            _ => Ok(None),
        }
    }

    /// SortIndexedProperties' compare step over the gathered present values: the user
    /// comparator if callable (a throwing compare propagates), else the default sort
    /// (SortCompare): ToString each element by code units, `undefined` last. Default
    /// keys are precomputed because ToString runs JS (which can't happen inside the
    /// comparator).
    fn sort_values(&mut self, items: &mut Vec<Value>, cmp: Value) -> Result<(), Thrown> {
        if self.is_callable(cmp) {
            // SortCompare always orders `undefined` elements AFTER every defined
            // value and NEVER passes them to the comparator. Partition them out,
            // sort the rest, then re-append (the default-comparator branch below
            // does the equivalent via its None-is-Greater key ordering).
            let undef = items.iter().filter(|&&v| v == Value::UNDEFINED).count();
            if undef > 0 {
                items.retain(|&v| v != Value::UNDEFINED);
            }
            self.comparator_sort(items, cmp)?;
            for _ in 0..undef {
                items.push(Value::UNDEFINED);
            }
        } else {
            let values = std::mem::take(items);
            *items = self.default_sort(values)?;
        }
        Ok(())
    }

    /// The default SortCompare over `values` (`sort()` / `toSorted()` with no
    /// comparator): ToString each element once, order the strings by UTF-16
    /// code units — the order `<` uses — and put `undefined` last, stably.
    ///
    /// The keys are WTF-8. A Rust `String` key compared in UTF-8 byte order,
    /// which is code-point order: an astral character (a surrogate pair,
    /// 0xD800..) sorted after U+E000..U+FFFF instead of before it, and every
    /// lone surrogate became U+FFFD and compared equal to the others.
    fn default_sort(&mut self, values: Vec<Value>) -> Result<Vec<Value>, Thrown> {
        // `plain`: no byte >= 0xED, so no surrogate, no astral character and
        // nothing at or above U+E000 — byte order IS code-unit order, and the
        // comparison stays a memcmp.
        let mut keyed: Vec<(Option<(Vec<u8>, bool)>, Value)> = Vec::with_capacity(values.len());
        for v in values {
            let key = if v == Value::UNDEFINED {
                None
            } else {
                let bytes = self.to_wtf8_string(v)?;
                let plain = !bytes.iter().any(|&b| b >= 0xED);
                Some((bytes, plain))
            };
            keyed.push((key, v));
        }
        keyed.sort_by(|(ka, _), (kb, _)| match (ka, kb) {
            (Some((a, pa)), Some((b, pb))) => {
                if *pa && *pb {
                    a.cmp(b)
                } else {
                    wtf8_code_unit_cmp(a, b)
                }
            }
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
        });
        Ok(keyed.into_iter().map(|(_, v)| v).collect())
    }

    /// Stable bottom-up merge sort driven by a JS comparator (`cmp(a,b) < 0` ⇒
    /// `a` before `b`). O(n log n) comparisons — vs the old insertion sort's
    /// O(n²), which dominated `Array.sort` for non-trivial sizes. Stable: on a tie
    /// (on `<= 0`, and on the NaN that SortCompare maps to +0) the LEFT run's
    /// element wins, preserving original order. The comparator re-enters the VM
    /// (`call_value`) and may throw — from the call or from its own ToNumber.
    #[cfg(all(feature = "safe-sandbox", feature = "meter-only", not(feature = "jit")))]
    fn exact_numeric_sub_sort_cmp(&mut self, cmp: Value, items: &[Value]) -> Option<&'p [Instr]> {
        // This is deliberately an artifact-profile specialization, not a
        // source-text heuristic. Only a real immutable user Func/Closure can
        // enter; Bound/Proxy/native callables and closures with lexical cells
        // retain the ordinary callback path.
        if !cmp.is_heap() {
            return None;
        }
        let fid = match self.heap.get(cmp.heap_index()) {
            HeapObj::Func(fid) => *fid,
            HeapObj::Closure { func, upvalues, .. } if upvalues.is_empty() => *func,
            _ => return None,
        };
        let p = self.func(fid as usize);

        if !exact_numeric_sub_sort_proto(p) {
            return None;
        }

        // Subtraction is side-effect-free only after both operands are already
        // JS Numbers. Any object/string/BigInt/Symbol must reach the ordinary
        // callback so ToNumeric/ToNumber and user coercion still run. Do this
        // before even growing the reusable register backing store: a rejected
        // sort must enter the established path without fast-path preparation.
        if !items.iter().all(|v| v.is_number()) {
            return None;
        }

        // A real call would reject at these boundaries before executing its
        // first opcode. A frame-vector growth is rare and can affect the host
        // allocation boundary, so decline unless one slot is already resident.
        let needed = self.regs.len().checked_add((p.reg_count as usize).max(1))?;
        if self.frames.len() >= MAX_FRAMES
            || self.run_loop_depth >= MAX_RUN_LOOP_DEPTH
            || self.frames.len() == self.frames.capacity()
            || self.regs_would_overflow(needed)
        {
            return None;
        }

        // Reproduce the ordinary callback's first register-window growth before
        // `comparator_sort` moves or mutates any item. Resize and truncate are
        // deliberately adjacent with no fallible operation or early return in
        // between, so every admitted success/error path starts the merge with
        // exactly the caller's live register length. This retains the allocation,
        // initialized high-water, and heap-preflight footprint of the first real
        // callback while later comparisons can elide unobservable frame contents.
        let win = self.regs.len();
        self.regs.resize(needed, Value::UNDEFINED);
        self.regs.truncate(win);
        debug_assert_eq!(self.regs.len(), win);

        Some(p.code.as_slice())
    }

    #[cfg(all(feature = "safe-sandbox", feature = "meter-only", not(feature = "jit")))]
    #[inline(always)]
    fn run_exact_numeric_sub_sort_cmp(
        &mut self,
        code: &'p [Instr],
        left: Value,
        right: Value,
    ) -> Result<f64, Thrown> {
        // The ordinary callback executes exactly `Sub; Return`. Enter the same
        // two dispatch ticks, in order, so a budget that permits Sub but not
        // Return stops at precisely the historical boundary and every periodic
        // heap audit remains on its original instruction number.
        // This specialization exists only in `meter-only`, whose hook ignores
        // register base/instruction identity and retains just the exact tick and
        // heap-poll schedule. The comparator bytecode IPs remain exact (0, 1);
        // there is no trace/fid location channel in this artifact profile.
        if self.instr_rec.is_some() {
            self.instrument_step(0, 0, &code[0])
                .map_err(|msg| Thrown(msg.to_string()))?;
        }
        let result = left.as_f64() - right.as_f64();
        if self.instr_rec.is_some() {
            self.instrument_step(0, 1, &code[1])
                .map_err(|msg| Thrown(msg.to_string()))?;
        }
        // `pop_frame_with` performs this on the ordinary Return path. Keep the
        // proper-tail-call safety streak independent of whether sort used the
        // elided callback frame.
        self.tail_reuse_streak = 0;
        Ok(result)
    }

    pub(crate) fn comparator_sort(
        &mut self,
        items: &mut Vec<Value>,
        cmp: Value,
    ) -> Result<(), Thrown> {
        let n = items.len();
        if n < 2 {
            return Ok(());
        }
        // Classify and preflight before moving or merging any elements. If the
        // exact call/meter contract cannot be reproduced, the complete sort
        // stays on the established callback implementation.
        #[cfg(all(feature = "safe-sandbox", feature = "meter-only", not(feature = "jit")))]
        let numeric_sub_code = self.exact_numeric_sub_sort_cmp(cmp, items);
        // Native-callback fast path: a compiled non-capturing comparator is called
        // directly over one reused register window (skipping a per-comparison frame
        // build + run_loop re-entry). `native = None` falls back to call_value.
        let mut native = self.native_cb_entry(cmp);
        let win = self.regs.len();
        if let Some((_, callee_regs, _)) = native {
            if self.regs_would_overflow(win + callee_regs) {
                native = None;
            } else {
                self.regs.resize(win + callee_regs, Value::UNDEFINED);
            }
        }
        // Ping-pong between two local buffers (not self.regs/heap, so a comparator
        // that re-enters the VM and allocates can't invalidate them). Every caller
        // owns a private gathered/snapshot list and propagates an abrupt comparator
        // completion before committing it, so moving that Vec here is unobservable
        // on error and avoids both the old input clone and final slice copy.
        let mut a = std::mem::take(items);
        let mut b: Vec<Value> = vec![Value::UNDEFINED; n];
        let mut width = 1;
        let mut err: Option<Thrown> = None;
        'outer: while width < n {
            let mut lo = 0;
            while lo < n {
                let mid = (lo + width).min(n);
                let hi = (lo + 2 * width).min(n);
                // Merge a[lo..mid] and a[mid..hi] into b[lo..hi], stably.
                let (mut l, mut r, mut k) = (lo, mid, lo);
                while l < mid && r < hi {
                    let fast_ord: Option<Result<f64, Thrown>> = {
                        #[cfg(all(
                            feature = "safe-sandbox",
                            feature = "meter-only",
                            not(feature = "jit")
                        ))]
                        {
                            numeric_sub_code
                                .map(|code| self.run_exact_numeric_sub_sort_cmp(code, a[l], a[r]))
                        }
                        #[cfg(not(all(
                            feature = "safe-sandbox",
                            feature = "meter-only",
                            not(feature = "jit")
                        )))]
                        {
                            None
                        }
                    };
                    let ord = match fast_ord {
                        Some(Ok(n)) => n,
                        Some(Err(e)) => {
                            err = Some(e);
                            break 'outer;
                        }
                        None => {
                            let c = match self.run_cb_elem(
                                native,
                                win,
                                cmp,
                                &[a[l], a[r]],
                                Value::UNDEFINED,
                            ) {
                                Ok(c) => c,
                                Err(e) => {
                                    err = Some(e);
                                    break 'outer;
                                }
                            };
                            // SortCompare steps 5-7: the comparator result goes
                            // through ToNumber (objects may run user code, and
                            // BigInt/Symbol throws); NaN maps to +0, retaining
                            // the stable left-run order.
                            if c.is_number() {
                                c.as_f64()
                            } else {
                                match self.to_number_strict(c) {
                                    Ok(n) => n,
                                    Err(e) => {
                                        err = Some(e);
                                        break 'outer;
                                    }
                                }
                            }
                        }
                    };
                    if !(ord > 0.0) {
                        b[k] = a[l];
                        l += 1;
                    } else {
                        b[k] = a[r];
                        r += 1;
                    }
                    k += 1;
                }
                while l < mid {
                    b[k] = a[l];
                    l += 1;
                    k += 1;
                }
                while r < hi {
                    b[k] = a[r];
                    r += 1;
                    k += 1;
                }
                lo += 2 * width;
            }
            std::mem::swap(&mut a, &mut b);
            width *= 2;
        }
        if native.is_some() {
            self.regs.truncate(win); // release the reused window (success or error)
        }
        if let Some(e) = err {
            return Err(e);
        }
        *items = a;
        Ok(())
    }
}

#[cfg(all(feature = "safe-sandbox", feature = "meter-only", not(feature = "jit")))]
fn exact_numeric_sub_sort_proto(p: &crate::bytecode::FuncProto) -> bool {
    // A simple two-parameter function has no default/destructuring prologue.
    // Rest/arguments, generators and async functions all have observable call
    // behaviour even when their final expression looks like subtraction, so
    // fail closed on every one. Captures are checked in both this immutable
    // proto and the concrete Closure at the call site.
    p.param_count == 2
        && p.length == 2
        && p.simple_params
        && p.rest_reg.is_none()
        && p.arguments_reg.is_none()
        && !p.is_generator
        && !p.is_async
        && p.non_constructable
        && p.lexical_this
        && p.upvalues.is_empty()
        && p.eval_sites.is_empty()
        && p.reg_count as usize > 3
        && matches!(
            p.code.as_slice(),
            [
                Instr::Sub { dst, a: 1, b: 2 },
                Instr::Return { src }
            ] if dst == src
        )
}

#[cfg(test)]
mod array_copy_elision_tests {
    #[test]
    fn comparator_sort_move_keeps_success_and_abrupt_completion_semantics() {
        let outcome = crate::run(
            r#"
                let a = [3, 2, 1];
                let calls = 0;
                try {
                    a.sort(function () { calls++; throw new Error("stop"); });
                } catch (error) {}
                console.log(a.join(",") + ":" + calls);

                let b = [3, 1, 2];
                b.sort(function (x, y) { return x - y; });
                console.log(b.join(","));
            "#,
        )
        .expect("script compiles");
        assert_eq!(outcome.error, None);
        assert_eq!(outcome.output, vec!["3,2,1:1", "1,2,3"]);
    }

    #[cfg(all(feature = "safe-sandbox", feature = "meter-only", not(feature = "jit")))]
    #[test]
    fn exact_numeric_sub_sort_keeps_number_and_fallback_semantics() {
        let outcome = crate::run(
            r#"
                let zeros = [0, -0, 0, -0];
                zeros.sort((x, y) => x - y);
                let signs = "";
                for (let i = 0; i < zeros.length; i++) {
                    signs += Object.is(zeros[i], -0) ? "-" : "+";
                }
                console.log(signs);

                let finite = [Infinity, 2, -Infinity, -2];
                finite.sort((x, y) => x - y);
                console.log(finite.join(","));

                let nan = [NaN, NaN, 1];
                nan.sort((x, y) => x - y);
                console.log(nan.join(","));

                let coercions = 0;
                let marker = { valueOf: function () { coercions++; return 2; } };
                let mixed = [3, marker, 1];
                mixed.sort((x, y) => x - y);
                console.log(
                    (mixed[0] === 1) + "," +
                    (mixed[1] === marker) + "," +
                    (mixed[2] === 3) + "," +
                    (coercions > 0)
                );

                let defaults = [3, 1, 2];
                defaults.sort((x = 0, y = 0) => x - y);
                let rests = [3, 1, 2];
                rests.sort((x, y, ...unused) => x - y);
                console.log(defaults.join(",") + "|" + rests.join(","));

                let big = [2n, 1n];
                let bigThrew = false;
                try { big.sort((x, y) => x - y); } catch (error) { bigThrew = true; }
                console.log(bigThrew);
            "#,
        )
        .expect("script compiles");
        assert_eq!(outcome.error, None);
        assert_eq!(
            outcome.output,
            vec![
                "+-+-",
                "-Infinity,-2,2,Infinity",
                "NaN,NaN,1",
                "true,true,true,true",
                "1,2,3|1,2,3",
                "true",
            ]
        );
    }

    #[cfg(all(feature = "safe-sandbox", feature = "meter-only", not(feature = "jit")))]
    #[test]
    fn exact_numeric_sub_classifier_fails_closed() {
        fn comparator_proto(src: &str) -> crate::bytecode::FuncProto {
            let ast = crate::front::parse_auto(src).expect("parses");
            let program = crate::compile::compile_main_program(&ast, src).expect("compiles");
            program.functions.last().expect("nested comparator").clone()
        }

        for src in ["let cmp = (x, y) => x - y;"] {
            let p = comparator_proto(src);
            assert!(super::exact_numeric_sub_sort_proto(&p), "admit {src}");
        }

        for src in [
            "let cmp = (x = 0, y = 0) => x - y;",
            "let cmp = (x, y, ...rest) => x - y;",
            "let cmp = async (x, y) => x - y;",
            "let cmp = function* (x, y) { return x - y; };",
            "let cmp = function (x, y) { return x - y; };",
            "let cmp = ([x], y) => x - y;",
            "let cmp = function (x, y) { void arguments; return x - y; };",
            "let cmp = (x, y) => y - x;",
            "let cmp = (() => { let z = 1; return (x, y) => x - y + z; })();",
        ] {
            let p = comparator_proto(src);
            assert!(!super::exact_numeric_sub_sort_proto(&p), "reject {src}");
        }
    }

    #[cfg(all(feature = "safe-sandbox", feature = "meter-only", not(feature = "jit")))]
    #[test]
    fn exact_numeric_sub_sort_preflights_and_restores_register_window() {
        let src = "let cmp = (x, y) => x - y;";
        let ast = crate::front::parse_auto(src).expect("parses");
        let program = crate::compile::compile_main_program(&ast, src).expect("compiles");
        let fid = (program.functions.len() - 1) as u32;
        let make_cmp = |vm: &mut super::Vm<'_>| {
            crate::value::Value::heap(vm.heap.alloc(crate::heap::HeapObj::Closure {
                func: fid,
                upvalues: Vec::new(),
                this_val: crate::value::Value::UNDEFINED,
            }))
        };

        // A non-Number rejects before any register preparation.
        let mut rejected = super::Vm::new(&program);
        rejected.frames.reserve(1);
        rejected.regs.resize(3, crate::value::Value::UNDEFINED);
        let rejected_cmp = make_cmp(&mut rejected);
        let rejected_len = rejected.regs.len();
        let rejected_high_water = rejected.regs.storage.len();
        let rejected_capacity = rejected.regs.capacity();
        assert!(rejected
            .exact_numeric_sub_sort_cmp(rejected_cmp, &[crate::value::Value::UNDEFINED])
            .is_none());
        assert_eq!(rejected.regs.len(), rejected_len);
        assert_eq!(rejected.regs.storage.len(), rejected_high_water);
        assert_eq!(rejected.regs.capacity(), rejected_capacity);

        // An admitted Number-only sort performs the ordinary first-call growth
        // synchronously, then restores the caller window before returning the
        // plan. Therefore a later comparison error has no window left to leak.
        let mut admitted = super::Vm::new(&program);
        admitted.frames.reserve(1);
        admitted.regs.resize(3, crate::value::Value::UNDEFINED);
        let admitted_cmp = make_cmp(&mut admitted);
        let caller_len = admitted.regs.len();
        let needed = caller_len + admitted.func(fid as usize).reg_count.max(1) as usize;
        assert!(admitted
            .exact_numeric_sub_sort_cmp(
                admitted_cmp,
                &[crate::value::Value::int(2), crate::value::Value::int(1)],
            )
            .is_some());
        assert_eq!(admitted.regs.len(), caller_len);
        assert!(admitted.regs.storage.len() >= needed);
    }

    #[cfg(all(feature = "safe-sandbox", feature = "meter-only", not(feature = "jit")))]
    #[test]
    fn exact_numeric_sub_sort_replays_meter_and_return_boundary() {
        use crate::embed::{self, HostValue, ScriptState};

        const SRC: &str = r#"
            var values;
            function go() {
                values = [8, 3, 7, 4, 9, 2, 6, 5, 1, 0];
                values.sort((x, y) => x - y);
                return values[0];
            }
        "#;

        fn ready() -> (ScriptState, u32, u32) {
            let mut state = embed::compile_script(SRC).expect("compiles");
            state.run_init().expect("initializes");
            let symbols = state.symbols();
            let slot = |name: &str| {
                symbols
                    .iter()
                    .find(|symbol| symbol.name == name)
                    .unwrap_or_else(|| panic!("missing {name}"))
                    .index
            };
            (state, slot("go"), slot("values"))
        }

        let (mut measured, go, _) = ready();
        measured.set_limits(u64::MAX, None);
        assert_eq!(measured.call_slot(go, &[]), Ok(HostValue::Number(0.0)));
        let exact = measured.steps_used();
        assert!(exact > 20, "sort callback work was not metered: {exact}");

        let (mut replay, go, _) = ready();
        replay.set_limits(exact, None);
        assert_eq!(replay.call_slot(go, &[]), Ok(HostValue::Number(0.0)));
        assert_eq!(replay.steps_used(), exact);
        assert_eq!(replay.steps_remaining(), 0);

        // Locate the caller's sort instruction and allow exactly one more
        // tick: the comparator Sub executes, but its Return must be rejected.
        // Because Array.sort commits only after successful comparison, the
        // globally visible array must still be in its original order.
        let ast = crate::front::parse_auto(SRC).expect("parses");
        let program = crate::compile::compile_main_program(&ast, SRC).expect("compiles");
        let go_proto = program
            .functions
            .iter()
            .find(|p| p.name == "go")
            .expect("go proto");
        let sort_ip = go_proto
            .code
            .iter()
            .position(|i| matches!(i, crate::bytecode::Instr::CallMethod { .. }))
            .expect("sort call");
        let boundary = sort_ip as u64 + 2;

        let (mut stopped, go, values) = ready();
        stopped.set_limits(boundary, None);
        let error = stopped
            .call_slot(go, &[])
            .expect_err("Return must not execute after the last permitted Sub");
        assert!(error.contains("instruction budget"), "got {error:?}");
        assert_eq!(stopped.steps_used(), boundary);
        assert_eq!(
            stopped.get_slot(values),
            HostValue::Array(vec![
                HostValue::Number(8.0),
                HostValue::Number(3.0),
                HostValue::Number(7.0),
                HostValue::Number(4.0),
                HostValue::Number(9.0),
                HostValue::Number(2.0),
                HostValue::Number(6.0),
                HostValue::Number(5.0),
                HostValue::Number(1.0),
                HostValue::Number(0.0)
            ])
        );
    }

    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    #[test]
    fn non_jit_callbacks_capture_only_length_and_seed_but_read_elements_live() {
        let outcome = crate::run(
            r#"
                let a = [1, 2, 3];
                let seen = [];
                let mapped = a.map(function (value, index) {
                    seen.push(value);
                    if (index === 0) { a[1] = 20; a.push(4); }
                    return value * 2;
                });
                console.log(seen.join(",") + "|" + mapped.join(",") + "|" + a.join(","));

                let b = [1, 2, 3];
                let sum = b.reduce(function (acc, value, index) {
                    if (index === 1) { b[2] = 30; b.push(4); }
                    return acc + value;
                });
                console.log(sum + "|" + b.join(","));

                let getterCalls = 0;
                let overlaid = [, 2, 3];
                Object.defineProperty(overlaid, "0", {
                    configurable: true,
                    get: function () { getterCalls++; return 7; }
                });
                console.log(overlaid.reduce(function (x, y) { return x + y; }) + ":" + getterCalls);
            "#,
        )
        .expect("script compiles");
        assert_eq!(outcome.error, None);
        assert_eq!(
            outcome.output,
            vec!["1,20,3|2,40,6|1,20,3,4", "33|1,2,30,4", "12:1"]
        );
    }
}

#[cfg(all(test, feature = "safe-sandbox"))]
mod array_stringify_safety_tests {
    #[test]
    fn guest_visible_array_stringification_handles_cycles() {
        let outcome = crate::run(
            r#"
                let a = [];
                a[0] = a;
                console.log("A" + String(a) + "B");
                console.log("A" + ("" + a) + "B");
                console.log("A" + a.toLocaleString() + "B");
                let object = {};
                object[a] = 7;
                console.log(object[""]);

                let left = [];
                let right = [left];
                left[0] = right;
                console.log("A" + String(left) + "B");
            "#,
        )
        .expect("script compiles");
        assert_eq!(outcome.error, None);
        assert_eq!(outcome.output, vec!["AB", "AB", "AB", "7", "AB"]);
    }

    #[test]
    fn deeply_nested_array_stringification_is_a_catchable_range_error() {
        let outcome = crate::run(
            r#"
                let value = [];
                for (let i = 0; i < 5; i++) value = [value];
                let caught = "none";
                try { String(value); } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(outcome.error, None);
        assert_eq!(
            outcome.output,
            vec!["true:array stringification nesting limit exceeded"]
        );
    }

    #[test]
    fn array_like_native_loops_fail_before_hostile_length_work() {
        // One past the live cap, so raising MAX_NATIVE_ITERATION_WORK (v0.0.10
        // took it from 262,144 to 67,108,864) keeps this a hostile length.
        let hostile_len = crate::vm::MAX_NATIVE_ITERATION_WORK + 2;
        let source = r#"
                const huge = { length: __HOSTILE_LEN__ };
                const results = [];
                for (const run of [
                    () => Array.prototype.forEach.call(huge, () => {}),
                    () => Array.prototype.indexOf.call(huge, 1),
                    () => Array.prototype.fill.call(huge, 0),
                    () => Array.prototype.shift.call(huge),
                    () => Array.prototype.toReversed.call(huge),
                    () => Reflect.apply(function () {}, null, huge),
                ]) {
                    try { run(); results.push("completed"); }
                    catch (error) { results.push(error instanceof RangeError ? "range" : "other"); }
                }
                console.log(results.join(","));
                console.log(Array.prototype.indexOf.call({ length: 1000000000 }, 1, 999999999));
            "#
        .replace("__HOSTILE_LEN__", &hostile_len.to_string());
        let outcome = crate::run(&source).expect("script compiles");
        assert_eq!(outcome.error, None);
        assert_eq!(
            outcome.output,
            vec!["range,range,range,range,range,range", "-1"]
        );
    }

    #[test]
    fn deeply_nested_flat_is_a_catchable_range_error() {
        let outcome = crate::run(
            r#"
                let value = [1];
                for (let i = 0; i < 65; i++) value = [value];
                try {
                    value.flat(Infinity);
                    console.log("completed");
                } catch (error) {
                    console.log((error instanceof RangeError) + ":" + error.message);
                }
            "#,
        )
        .expect("script compiles");
        assert_eq!(outcome.error, None);
        assert_eq!(
            outcome.output,
            vec!["true:array flattening nesting limit exceeded"]
        );
    }
}
