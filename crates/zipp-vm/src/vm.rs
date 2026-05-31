//! Explicit-frame register virtual machine.
//!
//! The defining choice: **JS recursion does not use the native Rust stack**.
//! Every activation is a frame in `frames: Vec<Frame>` over one flat register
//! file `regs: Vec<Value>`. A call pushes a frame and continues the same
//! dispatch loop; a return pops it. Consequences:
//!
//! * Deep recursion is bounded by a counter, not by the OS stack — it throws a
//!   catchable `RangeError` instead of segfaulting (a real bug in the old
//!   engine's JIT path).
//! * There is exactly one hot loop to optimise, and registers are explicit —
//!   the shape a register-allocating JIT consumes directly. Keeping unboxed
//!   `i32` live across a call boundary (where V8 wins and the old engine lost)
//!   becomes a property of *this* loop's frame model rather than something
//!   bolted on.
//!
//! Arithmetic has typed-`i32` fast paths inline; anything else falls to the
//! generic `f64` path. v1 is an interpreter — it will be slower than the old
//! JIT'd engine and than V8; the point is a clean substrate that a JIT can
//! later make faster.

use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{Heap, HeapObj, ObjMap};
use crate::value::Value;

/// Hard cap on simultaneous JS frames. Throws a catchable RangeError rather
/// than growing unbounded. 100k is far beyond any non-pathological recursion
/// and the flat register file makes each frame cheap.
const MAX_FRAMES: usize = 100_000;

/// Extra global slots reserved past `global_count` as JIT scratch "field globals"
/// for object scalar-replacement (SROA). A field-promoted region uses pool slots
/// `[global_count, global_count + n_fields)`; regions reuse the pool (synced per
/// native run, never concurrent), so this caps fields-per-region, not total.
const FIELD_POOL: usize = 64;

/// Sentinel `closure` value for a frame whose callee is a plain (capture-free)
/// function rather than a closure. Real heap indices are always `< u32::MAX`.
const NO_CLOSURE: u32 = u32::MAX;

/// An active `try` handler within a frame.
#[derive(Clone, Copy)]
struct Handler {
    /// Instruction index of the catch block.
    catch_target: u32,
    /// Register (frame-relative) that receives the thrown value.
    catch_reg: u16,
}

/// One activation record.
struct Frame {
    func: u32,
    /// Base index into `regs` of this frame's register window.
    base: usize,
    /// Instruction pointer within the function's code.
    ip: usize,
    /// Register in the *caller's* window that receives this call's result.
    ret_dst: u16,
    /// Heap index of the `Closure` object this frame is executing, or
    /// `NO_CLOSURE` for a plain function. `UpvalGet`/`UpvalSet` read the
    /// closure's captured cell indices through it.
    closure: u32,
    /// Active `try` handlers in this frame, innermost last. A `Throw` (or a
    /// thrown error bubbling up from a builtin call) unwinds to the innermost
    /// handler here, else propagates to the caller frame.
    handlers: Vec<Handler>,
}

/// Which array higher-order method `array_each` is driving (callback args are
/// `[element, index]` for all three; only the result handling differs).
#[derive(Clone, Copy)]
enum EachMode {
    Map,
    Filter,
    ForEach,
}

pub struct Vm<'p> {
    program: &'p Program,
    heap: Heap,
    globals: Vec<Value>,
    /// One contiguous register file shared by all live frames; each frame owns
    /// the window `[base, base + reg_count)`.
    regs: Vec<Value>,
    frames: Vec<Frame>,
    /// Lines produced by `Print` (console.log/info/debug → stdout), in order.
    pub output: Vec<String>,
    /// Lines produced by `console.error`/`console.warn` (→ stderr in node).
    pub errput: Vec<String>,
    /// VM start instant — the zero point for `performance.now()` (which reports
    /// fractional milliseconds elapsed since the program began).
    start: std::time::Instant,
    /// The JS value currently being thrown, set when a `Throw` (or an internal
    /// error) begins unwinding and cleared when a `catch` handler receives it.
    /// Carrying the real `Value` (not just a message) lets `catch (e)` bind the
    /// exact thrown object/string/number, and survives propagation across
    /// nested `run_loop` invocations (builtin callbacks) until caught.
    pending_throw: Option<Value>,
    /// Native JIT tier (x86-64 only, `feature = "jit"`). Compiles hot leaf
    /// integer functions to native code that shares this VM's register window;
    /// any non-int/heap/call op bails back to the interpreter at the exact ip.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit: crate::codegen::Jit,
    /// JIT on/off (set from `ZIPP_NOJIT` env var at construction) — lets a
    /// single binary A/B the JIT against the pure interpreter for honest
    /// measurement.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit_enabled: bool,
    /// Current native self-recursion depth (guards `jit_self_call`).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit_recurse_depth: u32,
    /// Pinned register-file capacity: `self.regs` is reserved to this at startup
    /// and NEVER allowed to grow past it (every call/recursion site checks),
    /// so the Vec never reallocates while native JIT code holds a raw pointer
    /// into it. 0 until `reserve_jit_regs` runs (interpreter-only builds ignore
    /// it). Exceeding it throws RangeError — a tighter bound than MAX_FRAMES.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    reg_capacity: usize,
    /// High-water mark: the largest `regs.len()` ever reached (and thus
    /// initialized). A native self-call window at or below this can be exposed
    /// with `set_len` instead of a zero-filling `resize` — its slots already hold
    /// valid `Value` bits (stale, but the compiled code defs-before-use). This
    /// avoids re-zeroing the callee window on every recursive call once the
    /// recursion has reached its deepest native level. Backing buffer is pinned
    /// (`reserve_jit_regs`) so initialized slots stay valid for the VM's life.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    regs_hw: usize,
}

/// A thrown JS value rendered to a message (v1 throws are strings/RangeError).
#[derive(Debug)]
pub struct Thrown(pub String);

impl<'p> Vm<'p> {
    pub fn new(program: &'p Program) -> Vm<'p> {
        let mut heap = Heap::new();
        // Pre-load string constants of every function into the heap so
        // `LoadConst` of a string resolves to a stable heap index. We rewrite
        // string-constant slots to carry their heap index as an Int payload
        // marker is avoided — instead the compiler emits heap Values directly
        // (see `intern_strings`).
        // `global_count` real slots, plus a fixed POOL of extra slots the JIT uses
        // as scratch "field globals" for object scalar-replacement (SROA): a
        // field-promoted region's GetProp/SetProp are rewritten to Load/StoreGlobal
        // on pool slots, and the interpreter syncs object.field ↔ pool slot around
        // the native run. Sized once here so the globals Vec never reallocates at
        // runtime (the JIT pins its base pointer).
        let globals = vec![Value::UNDEFINED; program.global_count as usize + FIELD_POOL];
        let _ = &mut heap;
        Vm {
            program,
            heap,
            globals,
            regs: Vec::new(),
            frames: Vec::new(),
            output: Vec::new(),
            errput: Vec::new(),
            start: std::time::Instant::now(),
            pending_throw: None,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit: crate::codegen::Jit::new(),
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_enabled: std::env::var_os("ZIPP_NOJIT").is_none(),
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_recurse_depth: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            reg_capacity: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            regs_hw: 0,
        }
    }

    /// Force the JIT on/off (overrides the `ZIPP_NOJIT` default). Used by the
    /// test suite to run a program both ways and assert the outputs match.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn set_jit_enabled(&mut self, on: bool) {
        self.jit_enabled = on;
    }

    /// Would growing `self.regs` to `needed` slots exceed the pinned capacity?
    /// (Interpreter-only builds: never — there is no pinned native pointer to
    /// protect, so the Vec may grow/reallocate freely.)
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    fn regs_would_overflow(&self, needed: usize) -> bool {
        self.reg_capacity != 0 && needed > self.reg_capacity
    }
    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    #[inline]
    fn regs_would_overflow(&self, _needed: usize) -> bool {
        false
    }

    /// The native self-recursive-call implementation behind the `jit_self_call`
    /// FFI trampoline. Runs `self`-recursion natively on a fresh register window
    /// appended to `self.regs`. Returns result Value bits, or
    /// `codegen::SELF_CALL_DEOPT` to make the native caller bail to the interp.
    ///
    /// Register-stability invariant: `self.regs` has reserved capacity for the
    /// whole recursion (`reserve_jit_regs`), so appending the callee window
    /// NEVER reallocates — the native CALLER's window pointer (`rbx`) therefore
    /// stays valid across this call. We `truncate` back to the caller's length
    /// before returning so the register file is exactly as the caller left it.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn jit_self_call_impl(&mut self, func_id: u32, args: *const u64, argc: usize) -> u64 {
        // Depth guard: deopt (not crash) past the native recursion budget; the
        // interpreter path then enforces MAX_FRAMES / throws RangeError.
        if self.jit_recurse_depth >= JIT_SELF_RECURSE_MAX {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        // Native entry via the one-entry self-call cache (skips the HashMap
        // lookup on the hot recursive path — it always targets the same func_id).
        let entry = match self.jit.self_call_entry(func_id) {
            Some(e) => e,
            None => return crate::codegen::SELF_CALL_DEOPT,
        };
        let proto = &self.program.functions[func_id as usize];
        let reg_count = (proto.reg_count as usize).max(1);
        let params = proto.param_count as usize;

        // Fresh window appended to regs. Reserved capacity guarantees no realloc.
        let new_base = self.regs.len();
        let needed = new_base + reg_count;
        if needed > self.regs.capacity() {
            // Out of reserved headroom — deopt rather than risk a realloc that
            // would invalidate the caller's live `rbx`.
            return crate::codegen::SELF_CALL_DEOPT;
        }
        if needed > self.regs_hw {
            // New ground: zero-fill the freshly exposed slots and advance the mark.
            self.regs.resize(needed, Value::UNDEFINED);
            self.regs_hw = needed;
        } else {
            // Window lies within already-initialized memory (a previous recursion
            // reached at least this deep). Slots hold valid Value bits (stale, but
            // the compiled code writes before it reads), so skip the zero-fill —
            // this is the hot path for all but the deepest recursive call.
            // SAFETY: needed ≤ regs_hw ≤ a prior len ≤ capacity; [0..regs_hw] was
            // initialized by an earlier resize and the buffer is pinned, so these
            // slots are live, valid `Value`s.
            unsafe {
                self.regs.set_len(needed);
            }
        }
        // reg 0 = `this` (undefined for a plain self-call); params at 1..
        self.regs[new_base] = Value::UNDEFINED;
        let n = argc.min(params);
        for i in 0..n {
            // SAFETY: args points to `argc` valid Value bits (the caller's reg
            // window); n ≤ argc.
            self.regs[new_base + 1 + i] = Value::from_bits(unsafe { *args.add(i) });
        }

        self.jit_recurse_depth += 1;
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(new_base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // Call the cached native entry directly (same win64 ABI as JitFn::run).
        // SAFETY: `entry` is this function's compiled win64 code (stable across
        // HashMap rehashes); the window has `reg_count` valid slots; vm is valid.
        let (bits, bail) = unsafe {
            let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
                core::mem::transmute(entry);
            let mut bail: u32 = crate::codegen::NO_BAIL;
            let r = f(regs_ptr, &mut bail as *mut u32, vm_ptr);
            (r, bail)
        };

        let result_bits = if bail == crate::codegen::NO_BAIL {
            bits
        } else {
            // The native callee bailed mid-body: finish this activation on the
            // interpreter over the SAME window via a transient frame. The frame
            // base is `new_base` into self.regs (stable — reserved capacity).
            self.frames.push(Frame {
                func: func_id,
                base: new_base,
                ip: bail as usize,
                ret_dst: 0,
                closure: NO_CLOSURE,
                handlers: Vec::new(),
            });
            let stop = self.frames.len() - 1;
            match self.run_loop(stop) {
                Ok(v) => v.bits(),
                // A throw inside the recursion: there is no JS-level way to
                // surface it through the native ABI here, so deopt the whole
                // self-call. pending_throw stays set; the interpreter caller
                // (the original top-level run_loop) re-raises it. We restore
                // regs and return the sentinel.
                Err(_) => {
                    self.jit_recurse_depth -= 1;
                    self.regs.truncate(new_base);
                    return crate::codegen::SELF_CALL_DEOPT;
                }
            }
        };

        self.jit_recurse_depth -= 1;
        self.regs.truncate(new_base);
        result_bits
    }

    /// Reserve enough register-file capacity that a full JIT self-recursion
    /// never reallocates `self.regs` (which would dangle native window
    /// pointers). Called before entering the top-level run.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn reserve_jit_regs(&mut self) {
        // The register file must NEVER reallocate while a native JIT frame holds
        // a raw pointer into it (the caller's window pointer lives in a callee-
        // saved register across the self-call helper). A realloc there dangles
        // it → memory corruption. So reserve the absolute worst case up front:
        // every possible frame (`MAX_FRAMES`) holding the largest window. Then
        // no growth site can ever exceed capacity (each is also guarded), so the
        // Vec is pinned for the VM's lifetime.
        //
        // Cost is bounded: capacity is clamped so the reserve can't exceed
        // ~256 MiB even for a pathological max_window; if the cap is hit, deep
        // recursion simply throws RangeError sooner (a `reg_capacity` field
        // records the real limit so the growth guards agree).
        let max_window = self
            .program
            .functions
            .iter()
            .map(|f| (f.reg_count as usize).max(1))
            .max()
            .unwrap_or(1);
        const MAX_REGS_BYTES: usize = 256 * 1024 * 1024; // 256 MiB ceiling
        let worst_case = max_window.saturating_mul(MAX_FRAMES);
        let capped = worst_case.min(MAX_REGS_BYTES / std::mem::size_of::<Value>());
        let target = self.regs.len() + capped;
        self.regs.reserve(target - self.regs.len());
        // Record the pinned capacity: growth sites must not exceed it (else the
        // Vec would realloc). Use the ACTUAL capacity Rust gave us (≥ requested).
        self.reg_capacity = self.regs.capacity();
    }

    /// Allocate a string on the heap and return its boxed Value.
    pub fn alloc_str(&mut self, s: String) -> Value {
        Value::heap(self.heap.alloc_str(s))
    }

    /// Allocate a function object and return its boxed Value.
    pub fn alloc_func(&mut self, id: u32) -> Value {
        Value::heap(self.heap.alloc(HeapObj::Func(id)))
    }

    /// Run the top-level function (id 0) to completion.
    pub fn run(&mut self) -> Result<Value, Thrown> {
        // Materialise function objects for every top-level function into the
        // globals that the compiler reserved for them. The compiler records,
        // per function, the global slot its name binds to (or u32::MAX if it is
        // an anonymous/nested function not hoisted to a global).
        self.hoist_functions();

        let top = &self.program.functions[0];
        let base = 0usize;
        let top_regs = top.reg_count as usize;
        self.regs.resize(top_regs, Value::UNDEFINED);
        // Reserve register-file capacity up front so JIT self-recursion can
        // append callee windows without reallocating `self.regs` (which would
        // dangle the native code's window pointer). Must happen while regs holds
        // only the top frame so the reservation math is relative to a known base.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        self.reserve_jit_regs();
        self.frames.push(Frame { func: 0, base, ip: 0, ret_dst: 0, closure: NO_CLOSURE, handlers: Vec::new() });
        // Run until the top-level frame returns (frames drains back to 0).
        self.run_loop(0)
    }

    /// Invoke a callable `Value` with `this` and `args`, running it to
    /// completion, and return its result. Used by builtin methods that take
    /// callbacks (`map`/`filter`/`reduce`/`sort`). The callee executes on the
    /// explicit frame stack like any other call; we run a nested dispatch loop
    /// that returns when the callee's frame pops back to the current depth.
    ///
    /// Note: this re-enters `run_loop` on the native stack, so deeply *nested
    /// callbacks* use native recursion. Ordinary JS recursion (a function
    /// calling itself) does NOT — it stays on the frame stack. The frame cap
    /// still bounds total depth.
    fn call_value(&mut self, callee: Value, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        let (func_id, closure) = self.resolve_callable(callee)?;
        if self.frames.len() >= MAX_FRAMES {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        let proto = &self.program.functions[func_id as usize];
        let callee_regs = (proto.reg_count as usize).max(1);
        let callee_params = proto.param_count as usize;

        let new_base = self.regs.len();
        // Never grow past the pinned capacity (would realloc and dangle a live
        // native window pointer) — throw a catchable RangeError instead.
        if self.regs_would_overflow(new_base + callee_regs) {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);
        self.regs[new_base] = this; // reg 0 = this
        let n = args.len().min(callee_params);
        for i in 0..n {
            self.regs[new_base + 1 + i] = args[i];
        }

        let stop_depth = self.frames.len();
        self.frames.push(Frame { func: func_id, base: new_base, ip: 0, ret_dst: 0, closure, handlers: Vec::new() });
        self.run_loop(stop_depth)
    }

    /// Bind each named top-level function to its reserved global slot as a
    /// heap function object, so `Call` of a global resolves correctly. The
    /// compiler marks function-name globals; here we fill them.
    fn hoist_functions(&mut self) {
        for (id, f) in self.program.functions.iter().enumerate() {
            if let Some(slot) = function_global_slot(f) {
                let v = Value::heap(self.heap.alloc(HeapObj::Func(id as u32)));
                if (slot as usize) < self.globals.len() {
                    self.globals[slot as usize] = v;
                }
            }
        }
    }

    /// Drives execution from the current frame until the frame that was current
    /// on entry returns (frames drops to `stop_depth`), catching thrown values
    /// at `try` handlers along the way. `run()` passes 0 (drain everything);
    /// `call_value` passes the pre-call depth (run one nested call).
    ///
    /// On a throw, [`Self::dispatch_body`] returns `Err`; we look up the thrown
    /// value and unwind to the nearest handler at or above `stop_depth`. If one
    /// exists, execution resumes at its catch target; otherwise the throw
    /// propagates out (with `pending_throw` left set so an enclosing `run_loop`
    /// — e.g. the caller of a builtin callback — can still catch it).
    fn run_loop(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
        loop {
            match self.dispatch_body(stop_depth) {
                Ok(v) => return Ok(v),
                Err(t) => {
                    let tv = match self.pending_throw {
                        Some(v) => v,
                        None => {
                            // Internal error (TypeError/RangeError/…) with no
                            // explicit thrown value: synthesise a string so it
                            // is still catchable as `e`.
                            let v = self.alloc_str(t.0.clone());
                            self.pending_throw = Some(v);
                            v
                        }
                    };
                    if self.unwind_to_handler(tv, stop_depth) {
                        self.pending_throw = None; // caught — resume at catch
                        continue;
                    }
                    // Uncaught here; propagate. If the carried message is empty
                    // (e.g. a JIT-bail unwind that signalled via pending_throw
                    // with no text), recompute it from the thrown value so the
                    // top-level report shows the real error, not "".
                    if t.0.is_empty() {
                        return Err(Thrown(self.throw_message(tv)));
                    }
                    return Err(t); // pending_throw stays set for an outer catch
                }
            }
        }
    }

    /// Pop frames from the top down to (but not below) `stop_depth`, looking for
    /// a `try` handler. On finding one, deposit `tv` into its catch register,
    /// set that frame's ip to the catch target, and return `true` (execution
    /// resumes there). If the boundary is reached with no handler, return
    /// `false` (the throw propagates to the caller).
    fn unwind_to_handler(&mut self, tv: Value, stop_depth: usize) -> bool {
        while self.frames.len() > stop_depth {
            let top = self.frames.len() - 1;
            if let Some(h) = self.frames[top].handlers.pop() {
                let base = self.frames[top].base;
                self.regs[base + h.catch_reg as usize] = tv;
                self.frames[top].ip = h.catch_target as usize;
                return true;
            }
            // No handler in this frame: discard it and its register window.
            let f = self.frames.pop().unwrap();
            self.regs.truncate(f.base);
        }
        false
    }

    /// The inner execution loop: runs ops in the current frame until a frame
    /// transition (a call pushes / a return pops) or a throw. Returns the value
    /// when the `stop_depth` frame returns, or `Err` to begin unwinding.
    fn dispatch_body(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
        loop {
            // Snapshot the current frame's coordinates. `ip` is advanced as a
            // local and written back only on frame transitions / loops.
            let frame_idx = self.frames.len() - 1;
            let func_id = self.frames[frame_idx].func;
            let base = self.frames[frame_idx].base;
            let mut ip = self.frames[frame_idx].ip;
            let cur_closure = self.frames[frame_idx].closure;
            let code: *const Vec<Instr> = &self.program.functions[func_id as usize].code;
            // SAFETY: `code` borrows immutable program data that outlives the
            // loop; we never mutate program functions during execution.
            let code: &Vec<Instr> = unsafe { &*code };

            // ── JIT tier ──
            // On fresh frame entry (ip == 0), if this function has compiled
            // native code, run it over the frame's register window. The native
            // code shares `self.regs`, so on a bail the interpreter resumes with
            // consistent state. Only entered at ip==0: a bail sets `ip` to the
            // resume point and falls into the interpreter for the rest of this
            // activation (never re-enters native mid-function). We also count
            // entries here and compile on crossing the threshold.
            // Only enter native code from a NON-recursive interpreter context
            // (`jit_recurse_depth == 0`). Once a native self-call has deopted and
            // we're finishing it on the interpreter, re-entering the JIT for the
            // continuation would livelock: native recurses 256, deopts, the
            // interpreter re-enters native, recurses 256, deopts… forever,
            // because the per-call native depth counter resets each return and
            // interpreter frames never reach MAX_FRAMES. Staying interpreted in
            // that subtree lets frames accumulate monotonically → RangeError.
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            if ip == 0 && self.jit_enabled && self.jit_recurse_depth == 0 {
                if let Some((result, bail)) = self.try_run_jit(func_id, base) {
                    if bail == crate::codegen::NO_BAIL {
                        // Native code returned: behave like a `Return`.
                        if self.pop_frame_with(result, stop_depth) {
                            return Ok(result);
                        }
                        continue; // re-enter outer loop with caller frame
                    }
                    // A bail can mean two things:
                    // (a) a normal deopt (non-int operand, overflow): resume the
                    //     interpreter at the recorded ip with consistent regs.
                    // (b) a self-recursive call threw (e.g. RangeError) and the
                    //     helper signalled deopt with `pending_throw` set — the
                    //     whole native chain must UNWIND, not resume. Detect (b)
                    //     by the pending throw and return Err so `run_loop`
                    //     dispatches it to the nearest handler / propagates it.
                    if self.pending_throw.is_some() {
                        // Persist ip for coherence, then unwind. The message is
                        // recomputed by run_loop from pending_throw.
                        let top = self.frames.len() - 1;
                        self.frames[top].ip = bail as usize;
                        return Err(Thrown(String::new()));
                    }
                    // (a): resume the interpreter at the recorded ip.
                    ip = bail as usize;
                } else if self.jit.record_and_should_compile(func_id) {
                    let proto: *const crate::bytecode::FuncProto =
                        &self.program.functions[func_id as usize];
                    // SAFETY: program functions are immutable during execution.
                    let proto_ref = unsafe { &*proto };
                    // The self-function's current global Value (a heap Func),
                    // stable since hoist_functions ran at startup. Embedded so a
                    // JIT'd `LoadGlobal(self_slot)` stores the REAL function (not
                    // a placeholder) — required for a deopted self-Call to
                    // resolve the callee correctly in the interpreter.
                    let self_val = proto_ref
                        .name_global
                        .and_then(|s| self.globals.get(s as usize).copied())
                        .unwrap_or(Value::UNDEFINED)
                        .bits();
                    self.jit.compile(
                        func_id,
                        proto_ref,
                        jit_self_call as usize,
                        self_val,
                    );
                }
            }

            // Inner loop: execute within the current frame until a call pushes
            // a new frame or a return pops this one.
            loop {
                let instr = &code[ip];
                match *instr {
                    Instr::LoadConst { dst, idx } => {
                        let v = self.program.functions[func_id as usize].constants[idx as usize];
                        // String constants are stored with a sentinel; resolve
                        // to a freshly-interned heap string the first time.
                        let resolved = self.resolve_const(func_id, idx, v);
                        self.set(base, dst, resolved);
                        ip += 1;
                    }
                    Instr::LoadInt { dst, val } => {
                        self.set(base, dst, Value::int(val));
                        ip += 1;
                    }
                    Instr::LoadUndefined { dst } => {
                        self.set(base, dst, Value::UNDEFINED);
                        ip += 1;
                    }
                    Instr::LoadNull { dst } => {
                        self.set(base, dst, Value::NULL);
                        ip += 1;
                    }
                    Instr::LoadBool { dst, val } => {
                        self.set(base, dst, Value::bool(val));
                        ip += 1;
                    }
                    Instr::Move { dst, src } => {
                        let v = self.get(base, src);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LoadGlobal { dst, idx } => {
                        let v = self.globals[idx as usize];
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::StoreGlobal { idx, src } => {
                        let v = self.get(base, src);
                        self.globals[idx as usize] = v;
                        ip += 1;
                    }
                    Instr::Now { dst, epoch } => {
                        let ms = if epoch {
                            // Date.now(): integer ms since the Unix epoch.
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as f64)
                                .unwrap_or(0.0)
                        } else {
                            // performance.now(): fractional ms since VM start.
                            self.start.elapsed().as_secs_f64() * 1000.0
                        };
                        self.set(base, dst, Value::num(ms));
                        ip += 1;
                    }

                    Instr::Add { dst, a, b } => {
                        let r = self.add(base, a, b)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Sub { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if va.is_int() && vb.is_int() {
                            match va.as_int().checked_sub(vb.as_int()) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 - vb.as_int() as f64),
                            }
                        } else {
                            Value::num(self.to_number(va)? - self.to_number(vb)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Mul { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if va.is_int() && vb.is_int() {
                            match va.as_int().checked_mul(vb.as_int()) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 * vb.as_int() as f64),
                            }
                        } else {
                            Value::num(self.to_number(va)? * self.to_number(vb)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Div { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = Value::num(self.to_number(va)? / self.to_number(vb)?);
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Mod { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = Value::num(self.to_number(va)? % self.to_number(vb)?);
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Neg { dst, a } => {
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            match va.as_int().checked_neg() {
                                Some(v) => Value::int(v),
                                None => Value::num(-(va.as_int() as f64)),
                            }
                        } else {
                            Value::num(-self.to_number(va)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::AddInt { dst, a, imm } => {
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            match va.as_int().checked_add(imm) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 + imm as f64),
                            }
                        } else {
                            Value::num(self.to_number(va)? + imm as f64)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }

                    Instr::Lt { dst, a, b } => {
                        let r = self.cmp_lt(base, a, b)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Le { dst, a, b } => {
                        let r = self.cmp_le(base, a, b)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Gt { dst, a, b } => {
                        let r = self.cmp_lt(base, b, a)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Ge { dst, a, b } => {
                        let r = self.cmp_le(base, b, a)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::LooseEq { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = self.loose_eq(va, vb)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::LooseNe { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = self.loose_eq(va, vb)?;
                        self.set(base, dst, Value::bool(!r));
                        ip += 1;
                    }
                    Instr::Eq { dst, a, b } => {
                        let r = self.strict_eq(base, a, b);
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Ne { dst, a, b } => {
                        let r = self.strict_eq(base, a, b);
                        self.set(base, dst, Value::bool(!r));
                        ip += 1;
                    }
                    Instr::Not { dst, a } => {
                        let va = self.get(base, a);
                        let t = self.truthy(va);
                        self.set(base, dst, Value::bool(!t));
                        ip += 1;
                    }
                    Instr::TypeOf { dst, a } => {
                        let va = self.get(base, a);
                        let t = self.type_of(va);
                        let v = self.alloc_str(t.to_string());
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::IsArray { dst, a } => {
                        let v = self.get(base, a);
                        let is_arr = v.is_heap()
                            && matches!(self.heap.get(v.heap_index()), HeapObj::Array(_));
                        self.set(base, dst, Value::bool(is_arr));
                        ip += 1;
                    }
                    Instr::JsonStringify { dst, val, space } => {
                        let v = self.get(base, val);
                        let indent = self.json_indent(self.get(base, space));
                        // `JSON.stringify(undefined)` (and of a function) is undefined.
                        let result = match self.json_value(v, &indent, 0) {
                            Some(s) => self.alloc_str(s),
                            None => Value::UNDEFINED,
                        };
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::MathOp { dst, op, arg_base, argc } => {
                        let r = self.eval_math(op, base, arg_base, argc)?;
                        self.set(base, dst, Value::num(r));
                        ip += 1;
                    }
                    Instr::GlobalFn { dst, op, arg_base, argc } => {
                        use crate::bytecode::GlobalFn as G;
                        let a0 = if argc >= 1 { self.get(base, arg_base) } else { Value::UNDEFINED };
                        let v = match op {
                            G::Number => {
                                if argc == 0 { Value::num(0.0) } else { Value::num(self.to_number(a0)?) }
                            }
                            G::ParseInt => {
                                let s = self.display(a0);
                                let radix = if argc >= 2 {
                                    self.to_number(self.get(base, arg_base + 1))? as i32
                                } else {
                                    0
                                };
                                Value::num(parse_int(&s, radix))
                            }
                            G::ParseFloat => Value::num(parse_float(&self.display(a0))),
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }

                    Instr::Jump { target } => {
                        let t = target as usize;
                        // ── OSR tier ── a backward jump is a loop back-edge. After
                        // the region heats up, compile `[target, ip]` (the loop
                        // body, headed at `target`) and run it natively; the
                        // native code returns the ip to resume at (a clean loop
                        // exit or a guard bail). Gated like the function JIT:
                        // enabled, and not inside a native self-recursion.
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        if self.jit_enabled && self.jit_recurse_depth == 0 && t < ip {
                            if let Some(resume) = self.try_run_osr(func_id, t as u32, base) {
                                ip = resume;
                                continue;
                            }
                            if self.jit.record_region(func_id, t as u32) {
                                let proto: *const crate::bytecode::FuncProto =
                                    &self.program.functions[func_id as usize];
                                // SAFETY: program functions are immutable during run.
                                let proto_ref = unsafe { &*proto };
                                self.jit.compile_region(
                                    func_id,
                                    proto_ref,
                                    t as u32,
                                    ip as u32,
                                    jit_globals_base as usize,
                                    crate::codegen::HeapHelperAddrs {
                                        get_prop_miss: jit_get_prop_miss as usize,
                                        set_prop_miss: jit_set_prop_miss as usize,
                                        versions_base: jit_heap_versions_base as usize,
                                        ic_base: jit_ic_base as usize,
                                        get_index: jit_get_index as usize,
                                        set_index: jit_set_index as usize,
                                    },
                                    self.program.global_count, // field-global pool base
                                    FIELD_POOL as u32,
                                );
                                if let Some(resume) = self.try_run_osr(func_id, t as u32, base) {
                                    ip = resume;
                                    continue;
                                }
                            }
                        }
                        ip = t;
                    }
                    Instr::JumpIfFalse { cond, target } => {
                        let v = self.get(base, cond);
                        if !self.truthy(v) {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfTrue { cond, target } => {
                        let v = self.get(base, cond);
                        if self.truthy(v) {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfNotLt { a, b, target } => {
                        let r = self.cmp_lt(base, a, b)?;
                        if !r {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfNotLe { a, b, target } => {
                        let r = self.cmp_le(base, a, b)?;
                        if !r {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }

                    Instr::Print { arg_base, argc, to_stderr } => {
                        let mut parts = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            let v = self.get(base, arg_base + i);
                            parts.push(self.inspect(v));
                        }
                        let line = parts.join(" ");
                        if to_stderr {
                            self.errput.push(line);
                        } else {
                            self.output.push(line);
                        }
                        ip += 1;
                    }

                    Instr::MakeFunc { dst, func_id } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::Func(func_id)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectKeys { dst, obj } => {
                        let o = self.get(base, obj);
                        // Collect the raw key strings first (immutable heap
                        // borrow), then intern them (mutable) — can't hold both.
                        let key_strs: Vec<String> = if o.is_heap() {
                            match self.heap.get(o.heap_index()) {
                                HeapObj::Object(map) => map.keys.clone(),
                                HeapObj::Array(items) => {
                                    (0..items.len()).map(|i| i.to_string()).collect()
                                }
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let keys: Vec<Value> =
                            key_strs.into_iter().map(|k| self.alloc_str(k)).collect();
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(keys)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectValues { dst, obj } => {
                        let o = self.get(base, obj);
                        let vals: Vec<Value> = if o.is_heap() {
                            match self.heap.get(o.heap_index()) {
                                HeapObj::Object(map) => map.vals.clone(),
                                HeapObj::Array(items) => items.clone(),
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(vals)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectEntries { dst, obj } => {
                        let o = self.get(base, obj);
                        // Snapshot (key string, value) pairs under the immutable
                        // borrow, then build `[key, value]` arrays (which allocate).
                        let pairs: Vec<(String, Value)> = if o.is_heap() {
                            match self.heap.get(o.heap_index()) {
                                HeapObj::Object(map) => {
                                    map.keys.iter().cloned().zip(map.vals.iter().copied()).collect()
                                }
                                HeapObj::Array(items) => {
                                    items.iter().enumerate().map(|(i, v)| (i.to_string(), *v)).collect()
                                }
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let mut entries = Vec::with_capacity(pairs.len());
                        for (k, val) in pairs {
                            let ks = self.alloc_str(k);
                            let inner = self.heap.alloc(HeapObj::Array(vec![ks, val]));
                            entries.push(Value::heap(inner));
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(entries)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LenOf { dst, obj } => {
                        let o = self.get(base, obj);
                        let v = if o.is_heap() {
                            match self.heap.get(o.heap_index()) {
                                HeapObj::Array(items) => len_value(items.len()),
                                HeapObj::Str(s) => len_value(s.char_len),
                                HeapObj::Cons { len, .. } => len_value(*len),
                                _ => Value::int(0),
                            }
                        } else {
                            Value::int(0)
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeClosure { dst, func_id } => {
                        // Capture each upvalue's cell index, resolved in THIS
                        // (defining) frame: a ParentLocal source reads the cell
                        // index from a local register (the local was boxed via
                        // MakeCell); a ParentUpval source forwards one of this
                        // frame's own captured cells.
                        let sources = &self.program.functions[func_id as usize].upvalues;
                        let mut cells = Vec::with_capacity(sources.len());
                        for src in sources {
                            let cell = match *src {
                                UpvalSource::ParentLocal(reg) => {
                                    self.get(base, reg).heap_index()
                                }
                                UpvalSource::ParentUpval(idx) => {
                                    self.closure_upvalue(cur_closure, idx)
                                }
                            };
                            cells.push(cell);
                        }
                        let v = Value::heap(
                            self.heap.alloc(HeapObj::Closure { func: func_id, upvalues: cells }),
                        );
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeCell { reg } => {
                        let v = self.get(base, reg);
                        let cell = self.heap.alloc(HeapObj::Cell(v));
                        self.set(base, reg, Value::heap(cell));
                        ip += 1;
                    }
                    Instr::CellGet { dst, cell } => {
                        let cell_idx = self.get(base, cell).heap_index();
                        let v = self.heap.cell_get(cell_idx);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::CellSet { cell, src } => {
                        let cell_idx = self.get(base, cell).heap_index();
                        let v = self.get(base, src);
                        self.heap.cell_set(cell_idx, v);
                        ip += 1;
                    }
                    Instr::UpvalGet { dst, idx } => {
                        let cell = self.closure_upvalue(cur_closure, idx);
                        let v = self.heap.cell_get(cell);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::UpvalSet { idx, src } => {
                        let cell = self.closure_upvalue(cur_closure, idx);
                        let v = self.get(base, src);
                        self.heap.cell_set(cell, v);
                        ip += 1;
                    }
                    Instr::NewArray { dst, arg_base, argc } => {
                        let mut items = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            items.push(self.get(base, arg_base + i));
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(items)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewObject { dst } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::GetIndex { dst, obj, key } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let r = self.get_index(o, k)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SetIndex { obj, key, val } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let v = self.get(base, val);
                        self.set_index(o, k, v)?;
                        ip += 1;
                    }
                    Instr::GetProp { dst, obj, name } => {
                        let o = self.get(base, obj);
                        let key = self.program.functions[func_id as usize]
                            .string_constants[name as usize]
                            .clone();
                        let r = self.get_prop(o, &key)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SetProp { obj, name, val } => {
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        let key = self.program.functions[func_id as usize]
                            .string_constants[name as usize]
                            .clone();
                        self.set_prop(o, &key, v)?;
                        ip += 1;
                    }

                    Instr::Call { dst, callee, arg_base, argc } => {
                        let callee_v = self.get(base, callee);
                        let (fid, closure) = self.resolve_callable(callee_v)?;
                        self.setup_call(
                            fid,
                            closure,
                            Value::UNDEFINED,
                            base,
                            arg_base,
                            argc,
                            dst,
                            ip + 1,
                        )?;
                        break;
                    }

                    Instr::CallMethod { dst, obj, name, arg_base, argc } => {
                        let recv = self.get(base, obj);
                        // `program` outlives the VM, so borrow the method name
                        // with the program's lifetime (NOT self's) — avoids
                        // cloning the name string on every method call (a heap
                        // alloc per `a.push(i)` / `a.map(cb)` etc.).
                        let prog: &'p Program = self.program;
                        let key: &'p str =
                            &prog.functions[func_id as usize].string_constants[name as usize];
                        // Builtin methods (array/string) execute inline and
                        // produce a result without pushing a frame.
                        if let Some(result) = self.try_builtin_method(recv, key, base, arg_base, argc)? {
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        // Otherwise the property must resolve to a user function
                        // (a method on an object); call it with `this = recv`.
                        let prop = self.get_prop(recv, key)?;
                        let (fid, closure) = self.resolve_callable(prop)?;
                        self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1)?;
                        break;
                    }

                    Instr::Throw { src } => {
                        let v = self.get(base, src);
                        let msg = self.throw_message(v);
                        // Persist ip so the (unused) frame state is coherent,
                        // then signal unwinding via pending_throw + Err.
                        let top = self.frames.len() - 1;
                        self.frames[top].ip = ip;
                        self.pending_throw = Some(v);
                        return Err(Thrown(msg));
                    }
                    Instr::PushHandler { catch_target, catch_reg } => {
                        let top = self.frames.len() - 1;
                        self.frames[top].handlers.push(Handler { catch_target, catch_reg });
                        ip += 1;
                    }
                    Instr::PopHandler => {
                        let top = self.frames.len() - 1;
                        self.frames[top].handlers.pop();
                        ip += 1;
                    }
                    Instr::Return { src } => {
                        let v = self.regs[base + src as usize];
                        if self.pop_frame_with(v, stop_depth) {
                            return Ok(v);
                        }
                        break;
                    }
                    Instr::ReturnUndefined => {
                        if self.pop_frame_with(Value::UNDEFINED, stop_depth) {
                            return Ok(Value::UNDEFINED);
                        }
                        break;
                    }
                }
            }
        }
    }

    /// If `func_id` has compiled native code, run it over the register window
    /// at `base` and return `(result_bits_as_Value, bail_ip)`. `None` if there
    /// is no compiled code for this function.
    ///
    /// The native code reads/writes `self.regs[base..]` directly via a raw
    /// pointer taken here and used ONLY for the duration of the call — nothing
    /// in between can resize `self.regs` (the JIT subset issues no calls/allocs).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn try_run_jit(&mut self, func_id: u32, base: usize) -> Option<(Value, u32)> {
        let jitfn = self.jit.get(func_id)? as *const crate::codegen::JitFn;
        // SAFETY: `jitfn` points into self.jit.compiled (stable for the call).
        // `regs_ptr` is valid for the frame's reg_count slots. A self-call op
        // routes through `jit_self_call` (passed the `vm` pointer below) which
        // may resize self.regs for the recursive frame — but it RESTORES regs to
        // this length before returning, and the native code re-reads its window
        // base from the callee-saved register only relative to `regs_ptr`, which
        // stays valid because jit_self_call uses a SEPARATE save/restore of the
        // regs Vec around the recursion (see its safety note).
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        let (bits, bail) = unsafe { (*jitfn).run(regs_ptr, vm_ptr) };
        Some((Value::from_bits(bits), bail))
    }

    /// Run the compiled OSR region for the loop headed at `entry_ip` (in
    /// `func_id`) over the frame's register window at `base`, returning the ip to
    /// resume interpreting at. `None` if no region is compiled for this header.
    ///
    /// The region's native code reads/writes `self.regs[base..]` and
    /// `self.globals` directly (the latter via a base pointer it fetches in its
    /// prologue). The numeric region issues NO calls that push frames or grow
    /// `self.regs`/`self.globals`, so the raw pointers stay valid for the call.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn try_run_osr(&mut self, func_id: u32, entry_ip: u32, base: usize) -> Option<usize> {
        let region = self.jit.get_region(func_id, entry_ip)? as *const crate::codegen::Region;
        // Object scalar-replacement (SROA): clone the sync plan so no region
        // borrow is held while the sync mutates globals/heap below.
        let field_plan = unsafe { (*region).field_plan().cloned() };

        // ── pre-run sync ── load the promoted object's fields into the scratch
        // pool globals the native code reads as ordinary globals.
        if let Some(ref p) = field_plan {
            let obj = self.globals[p.obj_global as usize];
            for &(name_idx, slot) in &p.fields {
                let key = self.program.functions[p.func_id as usize].string_constants
                    [name_idx as usize]
                    .clone();
                let v = self.get_prop(obj, &key).unwrap_or(Value::UNDEFINED);
                self.globals[slot as usize] = v;
            }
        }

        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // SAFETY: `region` is stable for the call (we don't mutate self.jit until
        // after); regs/globals do not move during a region run.
        let resume = unsafe { (*region).run(regs_ptr, vm_ptr) };

        // ── post-run sync ── flush the pool globals back to the object's fields,
        // so the interpreter (which resumes on the ORIGINAL bytecode, reading the
        // object) sees consistent values. Runs on EVERY exit (clean or bail).
        if let Some(ref p) = field_plan {
            let obj = self.globals[p.obj_global as usize];
            for &(name_idx, slot) in &p.fields {
                let key = self.program.functions[p.func_id as usize].string_constants
                    [name_idx as usize]
                    .clone();
                let v = self.globals[slot as usize];
                let _ = self.set_prop(obj, &key, v);
            }
        }
        // Bookkeeping: a resume INSIDE the region is a deopt; evict if chronic.
        self.jit.note_region_resume(func_id, entry_ip, resume);
        Some(resume as usize)
    }

    /// Pop the current frame. If this returns control to `stop_depth` (the
    /// frame the active `run_loop` was asked to run), report `true` so the loop
    /// returns `ret`. Otherwise deliver `ret` into the caller's `ret_dst` and
    /// report `false` to keep executing the caller.
    #[inline]
    fn pop_frame_with(&mut self, ret: Value, stop_depth: usize) -> bool {
        let finished = self.frames.pop().expect("frame underflow");
        // Shrink the register file back to the caller's window top.
        self.regs.truncate(finished.base);
        if self.frames.len() == stop_depth {
            return true;
        }
        let caller_base = self.frames.last().unwrap().base;
        self.regs[caller_base + finished.ret_dst as usize] = ret;
        false
    }

    /// Render a thrown value for the UNCAUGHT-throw message (the `Outcome.error`
    /// string). An Error-like object (`{message,…}` or one with a `.message`)
    /// prints `name: message`; otherwise the value's string form. Catchable
    /// throws bind the real `Value`, so this is only the top-level report.
    fn throw_message(&self, v: Value) -> String {
        if v.is_heap() {
            if let HeapObj::Object(map) = self.heap.get(v.heap_index()) {
                let name = map.get("name").map(|n| self.display(n));
                let msg = map.get("message").map(|m| self.display(m));
                return match (name, msg) {
                    (Some(n), Some(m)) => format!("{n}: {m}"),
                    (None, Some(m)) => format!("Error: {m}"),
                    _ => self.display(v),
                };
            }
        }
        format!("Uncaught {}", self.display(v))
    }

    // ── register access ──
    //
    // Unchecked: the compiler allocates `reg_count` registers per function and
    // never emits a register index ≥ `reg_count` (it tracks a `max_reg`
    // high-water mark), and every frame resizes `self.regs` to
    // `base + reg_count` on entry — so `base + r` is always in bounds. We index
    // `self.regs` freshly each call (no cached pointer), so a reallocation of
    // the register Vec by a re-entrant call/alloc is handled correctly. The
    // `debug_assert!` turns any compiler bug into a loud test failure in debug
    // builds while release elides the bounds check.
    #[inline(always)]
    fn get(&self, base: usize, r: u16) -> Value {
        debug_assert!((base + r as usize) < self.regs.len(), "reg read out of bounds");
        unsafe { *self.regs.get_unchecked(base + r as usize) }
    }
    #[inline(always)]
    fn set(&mut self, base: usize, r: u16, v: Value) {
        debug_assert!((base + r as usize) < self.regs.len(), "reg write out of bounds");
        unsafe {
            *self.regs.get_unchecked_mut(base + r as usize) = v;
        }
    }

    // ── call setup ──

    /// Resolve a value to a callable function id, or throw a TypeError.
    /// The cell heap-index captured at upvalue slot `idx` of the closure heap
    /// object `closure`. Panics only on a miscompiled program (an UpvalGet in a
    /// frame with no closure, or an out-of-range slot), which the compiler must
    /// not emit.
    #[inline]
    fn closure_upvalue(&self, closure: u32, idx: u16) -> u32 {
        match self.heap.get(closure) {
            HeapObj::Closure { upvalues, .. } => upvalues[idx as usize],
            _ => panic!("UpvalGet/Set in a frame without a closure"),
        }
    }

    /// Resolve a value to `(func_id, closure_heap_idx)`. `closure_heap_idx` is
    /// the value's heap index when it is a `Closure` (so the frame can reach its
    /// captured cells), or `NO_CLOSURE` for a plain `Func`.
    fn resolve_callable(&self, v: Value) -> Result<(u32, u32), Thrown> {
        if v.is_heap() {
            let idx = v.heap_index();
            match self.heap.get(idx) {
                HeapObj::Func(id) => return Ok((*id, NO_CLOSURE)),
                HeapObj::Closure { func, .. } => return Ok((*func, idx)),
                _ => {}
            }
        }
        Err(Thrown(format!("TypeError: {} is not a function", self.display(v))))
    }

    /// Push a new frame for `func_id`, binding `this_val` to register 0 and the
    /// `argc` arguments (staged at `caller_base + arg_base ..`) into registers
    /// `1..`. Records the caller's resume ip and result register.
    #[allow(clippy::too_many_arguments)]
    fn setup_call(
        &mut self,
        func_id: u32,
        closure: u32,
        this_val: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        dst: u16,
        caller_ip_next: usize,
    ) -> Result<(), Thrown> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        let proto = &self.program.functions[func_id as usize];
        let callee_regs = (proto.reg_count as usize).max(1);
        let callee_params = proto.param_count as usize;

        let new_base = self.regs.len();
        // Never grow past the pinned capacity (would realloc and dangle a live
        // native window pointer) — throw a catchable RangeError instead.
        if self.regs_would_overflow(new_base + callee_regs) {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);

        // Register 0 = `this`; parameters at registers 1..1+param_count.
        self.regs[new_base] = this_val;
        let n = (argc as usize).min(callee_params);
        for i in 0..n {
            let v = self.regs[caller_base + arg_base as usize + i];
            self.regs[new_base + 1 + i] = v;
        }

        let last = self.frames.len() - 1;
        self.frames[last].ip = caller_ip_next;
        self.frames.push(Frame { func: func_id, base: new_base, ip: 0, ret_dst: dst, closure, handlers: Vec::new() });
        Ok(())
    }

    // ── property / index access ──

    fn get_index(&mut self, obj: Value, key: Value) -> Result<Value, Thrown> {
        // A rope must be materialized before random access; no-op (one tag
        // check) for arrays, objects, and already-flat strings.
        if obj.is_heap() {
            self.heap.flatten(obj.heap_index());
        }
        if !obj.is_heap() {
            return Err(Thrown(format!(
                "TypeError: cannot read property of {}",
                self.display(obj)
            )));
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Array(items) => {
                // Numeric key (incl. an integral double like 1.0 — the JIT region
                // produces f64 indices): direct element access, else undefined.
                if let Some(i) = array_index(key) {
                    if i < items.len() {
                        return Ok(items[i]);
                    }
                    return Ok(Value::UNDEFINED);
                }
                // Non-int key on an array: "length" or out of range → undefined.
                let k = self.display(key);
                if k == "length" {
                    return Ok(len_value(items.len()));
                }
                Ok(Value::UNDEFINED)
            }
            HeapObj::Object(map) => {
                let k = self.display(key);
                Ok(map.get(&k).unwrap_or(Value::UNDEFINED))
            }
            HeapObj::Str(s) => {
                // Numeric key (incl. an integral double — a JIT region produces
                // f64 indices, and a deopted string index must agree): char at i.
                if let Some(i) = array_index(key) {
                    // O(1) for ASCII (i-th char == i-th byte); otherwise walk
                    // scalars (O(i), correct for multi-byte UTF-8).
                    let ch = if s.ascii {
                        s.bytes.as_bytes().get(i).map(|&b| b as char)
                    } else {
                        s.bytes.chars().nth(i)
                    };
                    if let Some(ch) = ch {
                        let cs = ch.to_string();
                        return Ok(self.alloc_str(cs));
                    }
                    return Ok(Value::UNDEFINED);
                }
                // Non-numeric key: only `s["length"]` is meaningful — mirror the
                // array and `s.length` paths.
                let char_len = s.char_len;
                if self.display(key) == "length" {
                    return Ok(len_value(char_len));
                }
                Ok(Value::UNDEFINED)
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    fn set_index(&mut self, obj: Value, key: Value, val: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: cannot set property of non-object".into()));
        }
        let idx = obj.heap_index();
        match self.heap.get_mut(idx) {
            HeapObj::Array(items) => {
                // Numeric key (incl. an integral double — the JIT region produces
                // f64 indices): store, growing with `undefined` holes past the end.
                if let Some(i) = array_index(key) {
                    if i >= items.len() {
                        items.resize(i + 1, Value::UNDEFINED);
                    }
                    items[i] = val;
                }
                // Non-numeric / negative / fractional key: no-op in this subset.
                Ok(())
            }
            HeapObj::Object(_) => {
                let k = self.display(key);
                let mut added = false;
                if let HeapObj::Object(map) = self.heap.get_mut(idx) {
                    added = map.set(&k, val);
                }
                if added {
                    self.heap.bump_version(idx);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn get_prop(&mut self, obj: Value, key: &str) -> Result<Value, Thrown> {
        if !obj.is_heap() {
            // Reading a property of null/undefined throws a TypeError (matches
            // JS); other primitives (number/bool) have no own props here → undef.
            if obj.is_nullish() {
                return Err(Thrown(format!(
                    "TypeError: Cannot read properties of {} (reading '{key}')",
                    if obj.is_null() { "null" } else { "undefined" }
                )));
            }
            return Ok(Value::UNDEFINED);
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Array(items) => {
                if key == "length" {
                    Ok(len_value(items.len()))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }
            HeapObj::Str(s) => {
                if key == "length" {
                    Ok(len_value(s.char_len))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }
            HeapObj::Cons { len, .. } => {
                if key == "length" {
                    Ok(len_value(*len))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }
            HeapObj::Object(map) => Ok(map.get(key).unwrap_or(Value::UNDEFINED)),
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// Evaluate a `Math.<fn>` call over `argc` argument registers (coerced to
    /// numbers). Mirrors JS semantics where they differ from Rust's f64 methods:
    /// `round` is half-up (so −2.5 → −2, not −3); `sign` preserves ±0 and maps
    /// NaN→NaN; `min`/`max` are NaN-sticky (any NaN arg ⇒ NaN).
    fn eval_math(&self, op: crate::bytecode::MathFn, base: usize, arg_base: u16, argc: u16) -> Result<f64, Thrown> {
        use crate::bytecode::MathFn as M;
        let arg = |i: u16| -> Result<f64, Thrown> {
            if i < argc {
                self.to_number(self.get(base, arg_base + i))
            } else {
                Ok(f64::NAN)
            }
        };
        Ok(match op {
            M::Min | M::Max | M::Hypot => {
                let mut acc = match op {
                    M::Min => f64::INFINITY,
                    M::Max => f64::NEG_INFINITY,
                    _ => 0.0, // Hypot: sum of squares
                };
                for i in 0..argc {
                    let v = arg(i)?;
                    acc = match op {
                        M::Min => {
                            if v.is_nan() || acc.is_nan() { f64::NAN } else { acc.min(v) }
                        }
                        M::Max => {
                            if v.is_nan() || acc.is_nan() { f64::NAN } else { acc.max(v) }
                        }
                        _ => acc + v * v,
                    };
                }
                if matches!(op, M::Hypot) { acc.sqrt() } else { acc }
            }
            M::Pow => arg(0)?.powf(arg(1)?),
            M::Atan2 => arg(0)?.atan2(arg(1)?),
            _ => {
                let x = arg(0)?;
                match op {
                    M::Abs => x.abs(),
                    M::Floor => x.floor(),
                    M::Ceil => x.ceil(),
                    M::Round => (x + 0.5).floor(), // JS half-up, ≠ Rust's half-away-from-zero
                    M::Trunc => x.trunc(),
                    M::Sign => {
                        if x.is_nan() {
                            f64::NAN
                        } else if x > 0.0 {
                            1.0
                        } else if x < 0.0 {
                            -1.0
                        } else {
                            x // preserve +0 / -0
                        }
                    }
                    M::Sqrt => x.sqrt(),
                    M::Cbrt => x.cbrt(),
                    M::Exp => x.exp(),
                    M::Log => x.ln(),
                    M::Log2 => x.log2(),
                    M::Log10 => x.log10(),
                    M::Sin => x.sin(),
                    M::Cos => x.cos(),
                    M::Tan => x.tan(),
                    M::Asin => x.asin(),
                    M::Acos => x.acos(),
                    M::Atan => x.atan(),
                    // Pow/Atan2/Min/Max/Hypot handled above.
                    _ => unreachable!(),
                }
            }
        })
    }

    /// The per-level indent string for `JSON.stringify`'s `space` argument: a
    /// number → that many spaces (clamped 0..10); a string → its first 10 chars;
    /// anything else → empty (compact output).
    fn json_indent(&self, space: Value) -> String {
        if space.is_number() {
            let n = space.as_f64();
            let n = if n.is_finite() && n > 0.0 { (n as usize).min(10) } else { 0 };
            " ".repeat(n)
        } else if space.is_heap() {
            match self.heap.str_cow(space.heap_index()) {
                Some(s) => s.chars().take(10).collect(),
                None => String::new(),
            }
        } else {
            String::new()
        }
    }

    /// Serialize `v` to JSON (`None` ⇒ omit: undefined / function). `indent` is
    /// the per-level pad (empty ⇒ compact); `depth` is the current nesting.
    fn json_value(&self, v: Value, indent: &str, depth: usize) -> Option<String> {
        if depth > 512 {
            return None; // guard against pathological / circular structures
        }
        if v.is_undefined() {
            return None;
        }
        if v.is_null() {
            return Some("null".to_string());
        }
        if v.is_bool() {
            return Some(if v.as_bool() { "true" } else { "false" }.to_string());
        }
        if v.is_number() {
            let n = v.as_f64();
            return Some(if n.is_finite() { fmt_f64(n) } else { "null".to_string() });
        }
        if !v.is_heap() {
            return None;
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => {
                let s = self.heap.str_cow(v.heap_index()).unwrap();
                Some(json_quote(&s))
            }
            HeapObj::Func(_) | HeapObj::Closure { .. } => None, // functions are omitted
            HeapObj::Array(items) => {
                let items = items.clone(); // release the heap borrow before recursing
                if items.is_empty() {
                    return Some("[]".to_string());
                }
                // A missing element value serializes as null inside an array.
                let parts: Vec<String> = items
                    .iter()
                    .map(|e| self.json_value(*e, indent, depth + 1).unwrap_or_else(|| "null".to_string()))
                    .collect();
                Some(wrap_json(&parts, '[', ']', indent, depth))
            }
            HeapObj::Object(map) => {
                let keys = map.keys.clone();
                let vals = map.vals.clone();
                let sep = if indent.is_empty() { ":" } else { ": " };
                let mut parts = Vec::new();
                for (k, val) in keys.iter().zip(vals.iter()) {
                    if let Some(vs) = self.json_value(*val, indent, depth + 1) {
                        parts.push(format!("{}{}{}", json_quote(k), sep, vs));
                    }
                }
                if parts.is_empty() {
                    return Some("{}".to_string());
                }
                Some(wrap_json(&parts, '{', '}', indent, depth))
            }
            _ => None,
        }
    }

    /// JS `typeof` type-name. `null` is `"object"` (a historic quirk); functions
    /// and closures are `"function"`; arrays and objects are `"object"`.
    fn type_of(&self, v: Value) -> &'static str {
        if v.is_int() || v.is_double() {
            "number"
        } else if v.is_bool() {
            "boolean"
        } else if v.is_undefined() {
            "undefined"
        } else if v.is_null() {
            "object"
        } else if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => "string",
                HeapObj::Func(_) | HeapObj::Closure { .. } => "function",
                HeapObj::Cell(inner) => self.type_of(*inner), // see through an upvalue cell
                _ => "object", // Array, Object
            }
        } else {
            "undefined"
        }
    }

    fn set_prop(&mut self, obj: Value, key: &str, val: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: cannot set property of non-object".into()));
        }
        let idx = obj.heap_index();
        // `arr.length = n` truncates (n < len) or extends-with-holes (n > len) a
        // dense array — a very common idiom (`arr.length = 0` clears it). Per JS,
        // n must be a non-negative integer < 2^32, else a RangeError.
        if key == "length" && matches!(self.heap.get(idx), HeapObj::Array(_)) {
            let n = self.to_number(val)?;
            if !(n >= 0.0 && n.fract() == 0.0 && n < 4_294_967_296.0) {
                return Err(Thrown("RangeError: Invalid array length".into()));
            }
            if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                items.resize(n as usize, Value::UNDEFINED);
            }
            self.heap.bump_version(idx);
            return Ok(());
        }
        let mut added = false;
        if let HeapObj::Object(map) = self.heap.get_mut(idx) {
            added = map.set(key, val);
        }
        if added {
            self.heap.bump_version(idx); // invalidate any JIT inline cache (vals realloc)
        }
        Ok(())
    }

    /// Try a builtin method on an array or string receiver. Returns
    /// `Ok(Some(result))` when `name` is a recognised builtin, `Ok(None)` when
    /// it isn't (the caller then treats it as a user-defined method/property).
    ///
    /// Dispatch is split by receiver type into focused helpers so each stays
    /// readable. Methods that take a JS callback (`map`/`filter`/`reduce`/
    /// `sort`) clone the element snapshot out of the heap BEFORE invoking the
    /// callback, because a callback can mutate the same array (which would
    /// reallocate its `Vec` and invalidate any borrow held across the call).
    fn try_builtin_method(
        &mut self,
        recv: Value,
        name: &str,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Result<Option<Value>, Thrown> {
        // Gather args into a stack buffer for the common small-arity case (1-2
        // args for push/map/filter/…), avoiding a heap Vec alloc per call; only
        // a rare >8-arg call falls back to the heap.
        let mut stackbuf = [Value::UNDEFINED; 8];
        let mut heapbuf: Vec<Value>;
        let n = arg_base as usize;
        let args: &[Value] = if argc as usize <= stackbuf.len() {
            for i in 0..argc as usize {
                stackbuf[i] = self.regs[base + n + i];
            }
            &stackbuf[..argc as usize]
        } else {
            heapbuf = (0..argc as usize).map(|i| self.regs[base + n + i]).collect();
            &heapbuf
        };
        // Number receivers (Int or double) support a small method set.
        if recv.is_number() {
            return self.number_method(recv, name, args);
        }
        if !recv.is_heap() {
            return Ok(None);
        }
        let idx = recv.heap_index();
        match self.heap.get(idx) {
            HeapObj::Array(_) => self.array_method(idx, name, args),
            HeapObj::Str(_) | HeapObj::Cons { .. } => self.string_method(idx, name, args),
            _ => Ok(None),
        }
    }

    /// Methods on a number receiver: `toFixed`, `toString`. Returns `Ok(None)`
    /// for an unrecognised name (the caller then treats it as a missing property
    /// → TypeError, matching JS).
    fn number_method(&mut self, recv: Value, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let n = recv.as_f64();
        match name {
            "toFixed" => {
                let digits = args.first().map(|a| a.as_f64() as usize).unwrap_or(0).min(100);
                Ok(Some(self.alloc_str(format!("{n:.digits$}"))))
            }
            // Base-10 uses the engine's canonical number rendering for node
            // parity; a radix argument (toString(2|16|…)) is out of v1 scope.
            "toString" => Ok(Some(self.alloc_str(self.display(recv)))),
            _ => Ok(None),
        }
    }

    /// Resolve `cb` to the native entry of a COMPILED, non-capturing JIT function
    /// for the array-builtin fast path (`map`/`filter`/`forEach`/`reduce`).
    /// Returns `(entry, callee_reg_count, param_count)` or `None` if `cb` must go
    /// through the interpreter `call_value` (not a plain function, a capturing
    /// closure, JIT disabled, inside a deopted self-call continuation, or not
    /// JIT-compilable). Compiles `cb` on first use if eligible — array builtins
    /// call the same callback many times, so we don't wait for the call-count
    /// threshold; an ineligible proto is blacklisted by `compile` and returns
    /// `None` cheaply thereafter.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn native_cb_entry(&mut self, cb: Value) -> Option<(*const u8, usize, usize)> {
        // Mirror the interpreter's JIT-entry guard: respect ZIPP_NOJIT and never
        // enter native code from a deopted self-call continuation (livelock).
        if !self.jit_enabled || self.jit_recurse_depth != 0 || !cb.is_heap() {
            return None;
        }
        let (fid, ups) = self.heap.as_callable(cb.heap_index())?;
        // A capturing closure reads upvalue cells (heap) — outside the leaf-int JIT.
        if !ups.is_empty() {
            return None;
        }
        if self.jit.get(fid).is_none() {
            let proto: *const crate::bytecode::FuncProto =
                &self.program.functions[fid as usize];
            // SAFETY: program functions are immutable during execution; the raw
            // ptr dodges the self.jit (&mut) vs self.program (&) borrow conflict.
            let proto_ref = unsafe { &*proto };
            let self_val = proto_ref
                .name_global
                .and_then(|s| self.globals.get(s as usize).copied())
                .unwrap_or(Value::UNDEFINED)
                .bits();
            self.jit.compile(fid, proto_ref, jit_self_call as usize, self_val);
        }
        let entry = self.jit.get(fid)?.entry();
        let proto = &self.program.functions[fid as usize];
        Some((entry, (proto.reg_count as usize).max(1), proto.param_count as usize))
    }

    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    fn native_cb_entry(&mut self, _cb: Value) -> Option<(*const u8, usize, usize)> {
        None
    }

    /// Invoke a compiled callback natively over the reused window at `win`
    /// (`regs[win..win+callee_regs]`), writing `this`=undefined + the first
    /// `param_count` args. On a native deopt (bail), re-runs the element through
    /// the interpreter `call_value` — which nests its frame ABOVE this window
    /// (base = `regs.len()`) and pops back, leaving the window intact for the
    /// next element. This is the fast path that skips the per-element frame push
    /// + `run_loop` re-entry + callee re-resolution that `call_value` incurs.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn invoke_cb_windowed(
        &mut self,
        entry: *const u8,
        win: usize,
        param_count: usize,
        cb: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        self.regs[win] = Value::UNDEFINED; // reg 0 = this
        let n = args.len().min(param_count);
        for i in 0..n {
            self.regs[win + 1 + i] = args[i];
        }
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // SAFETY: `entry` is a valid compiled win64 fn (regs, bail_out, vm)->bits
        // (from JitFn::entry); the window has callee_regs ≥ param_count+1 valid
        // slots; `vm_ptr` is valid for the call. A self-recursive callee routes
        // through `jit_self_call` which is capacity-pinned (no regs realloc).
        let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
            unsafe { core::mem::transmute(entry) };
        let mut bail: u32 = crate::codegen::NO_BAIL;
        let bits = f(regs_ptr, &mut bail as *mut u32, vm_ptr);
        if bail == crate::codegen::NO_BAIL {
            return Ok(Value::from_bits(bits));
        }
        // A deopt that left `pending_throw` set means a native self-recursive
        // callee already THREW (e.g. a recursive callback hit the RangeError
        // frame cap) — UNWIND with that exception. Re-running via call_value
        // would execute the callback a second time and propagate a stale thrown
        // value. Mirrors the try_run_jit ip==0 bail handling.
        if self.pending_throw.is_some() {
            return Err(Thrown(String::new()));
        }
        // Plain deopt (non-int operand / overflow): re-run this element on the
        // interpreter, which nests its frame above the reused window.
        self.call_value(cb, Value::UNDEFINED, args)
    }

    /// One per-element callback invocation: native fast path when `native` is
    /// set, else the interpreter `call_value`.
    #[inline]
    fn run_cb_elem(
        &mut self,
        native: Option<(*const u8, usize, usize)>,
        win: usize,
        cb: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if let Some((entry, _callee_regs, param_count)) = native {
            return self.invoke_cb_windowed(entry, win, param_count, cb, args);
        }
        let _ = (native, win);
        self.call_value(cb, Value::UNDEFINED, args)
    }

    /// Shared driver for `map`/`filter`/`forEach` (callback args = [element,
    /// index]). Uses the native callback fast path when the callback is a
    /// compiled non-capturing function: a single reused register window, a direct
    /// native call per element. Falls back to `call_value` per element otherwise.
    /// The window is always released (truncate) before returning — including on a
    /// callback error — so a thrown callback never leaks register slots.
    fn array_each(&mut self, idx: u32, cb: Value, mode: EachMode) -> Result<Option<Value>, Thrown> {
        let snapshot = self.array_snapshot(idx);
        let collect = matches!(mode, EachMode::Map | EachMode::Filter);
        let mut out: Vec<Value> =
            if collect { Vec::with_capacity(snapshot.len()) } else { Vec::new() };

        let mut native = self.native_cb_entry(cb);
        let win = self.regs.len();
        if let Some((_, callee_regs, _)) = native {
            if self.regs_would_overflow(win + callee_regs) {
                native = None; // can't fit a window → interpreter path
            } else {
                self.regs.resize(win + callee_regs, Value::UNDEFINED);
            }
        }

        let mut err = None;
        for (i, v) in snapshot.iter().enumerate() {
            let args = [*v, Value::int(i as i32)];
            match self.run_cb_elem(native, win, cb, &args) {
                Ok(r) => match mode {
                    EachMode::Map => out.push(r),
                    EachMode::Filter => {
                        if self.truthy(r) {
                            out.push(*v);
                        }
                    }
                    EachMode::ForEach => {}
                },
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
            _ => Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out))))),
        }
    }

    fn array_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "push" => {
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
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    return Ok(Some(items.pop().unwrap_or(Value::UNDEFINED)));
                }
                Ok(Some(Value::UNDEFINED))
            }
            "shift" => {
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    if items.is_empty() {
                        return Ok(Some(Value::UNDEFINED));
                    }
                    return Ok(Some(items.remove(0)));
                }
                Ok(Some(Value::UNDEFINED))
            }
            "join" => {
                let sep = if args.is_empty() { ",".to_string() } else { self.display(arg0) };
                let snapshot = self.array_snapshot(idx);
                let parts: Vec<String> = snapshot
                    .iter()
                    .map(|v| if v.is_nullish() { String::new() } else { self.display(*v) })
                    .collect();
                Ok(Some(self.alloc_str(parts.join(&sep))))
            }
            "indexOf" => {
                let snapshot = self.array_snapshot(idx);
                let pos = snapshot.iter().position(|v| self.values_strict_eq(*v, arg0));
                Ok(Some(Value::int(pos.map(|p| p as i32).unwrap_or(-1))))
            }
            "includes" => {
                let snapshot = self.array_snapshot(idx);
                let found = snapshot.iter().any(|v| self.values_strict_eq(*v, arg0));
                Ok(Some(Value::bool(found)))
            }
            "lastIndexOf" => {
                let snapshot = self.array_snapshot(idx);
                let pos = snapshot.iter().rposition(|v| self.values_strict_eq(*v, arg0));
                Ok(Some(Value::int(pos.map(|p| p as i32).unwrap_or(-1))))
            }
            "reverse" => {
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    items.reverse();
                }
                Ok(Some(Value::heap(idx))) // reverses in place, returns the array
            }
            "concat" => {
                // New array = this ++ each arg, spreading array args one level.
                let mut out = self.array_snapshot(idx);
                for a in args {
                    if a.is_heap() && matches!(self.heap.get(a.heap_index()), HeapObj::Array(_)) {
                        out.extend(self.array_snapshot(a.heap_index()));
                    } else {
                        out.push(*a);
                    }
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "flat" => {
                let depth = if args.is_empty() {
                    1
                } else {
                    let d = arg0.as_f64();
                    if d.is_infinite() && d > 0.0 {
                        i32::MAX
                    } else if d.is_finite() && d >= 0.0 {
                        d as i32
                    } else {
                        0
                    }
                };
                let snapshot = self.array_snapshot(idx);
                let out = self.flatten_array(&snapshot, depth);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "fill" => {
                let val = arg0;
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let start = norm_index(if args.len() >= 2 { args[1].as_f64() as i32 } else { 0 }, len);
                let end = norm_index(if args.len() >= 3 { args[2].as_f64() as i32 } else { len }, len);
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    for i in start..end {
                        items[i as usize] = val;
                    }
                }
                Ok(Some(Value::heap(idx)))
            }
            "slice" => {
                let snapshot = self.array_snapshot(idx);
                let len = snapshot.len() as i32;
                let start = norm_index(if args.is_empty() { 0 } else { arg0.as_f64() as i32 }, len);
                let end = if args.len() < 2 {
                    len
                } else {
                    norm_index(args[1].as_f64() as i32, len)
                };
                let slice: Vec<Value> = if start < end {
                    snapshot[start as usize..end as usize].to_vec()
                } else {
                    Vec::new()
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(slice)))))
            }
            "map" => self.array_each(idx, arg0, EachMode::Map),
            "filter" => self.array_each(idx, arg0, EachMode::Filter),
            "forEach" => self.array_each(idx, arg0, EachMode::ForEach),
            // Short-circuiting callback searches. They stop at the first match, so
            // they use call_value directly (the all-elements array_each driver
            // doesn't fit); the callback receives (element, index).
            "find" | "findIndex" | "some" | "every" => {
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                for (i, v) in snapshot.iter().enumerate() {
                    let r = self.call_value(cb, Value::UNDEFINED, &[*v, Value::int(i as i32)])?;
                    let t = self.truthy(r);
                    match name {
                        "find" if t => return Ok(Some(*v)),
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
                let snapshot = self.array_snapshot(idx);
                let mut iter = snapshot.iter().enumerate();
                let mut acc = if args.len() >= 2 {
                    args[1]
                } else {
                    match iter.next() {
                        Some((_, v)) => *v,
                        None => return Err(Thrown("TypeError: Reduce of empty array with no initial value".into())),
                    }
                };
                // Native callback fast path over a single reused window (the
                // accumulator + element + index are the callback args).
                let mut native = self.native_cb_entry(cb);
                let win = self.regs.len();
                if let Some((_, callee_regs, _)) = native {
                    if self.regs_would_overflow(win + callee_regs) {
                        native = None;
                    } else {
                        self.regs.resize(win + callee_regs, Value::UNDEFINED);
                    }
                }
                let mut err = None;
                for (i, v) in iter {
                    let cbargs = [acc, *v, Value::int(i as i32)];
                    match self.run_cb_elem(native, win, cb, &cbargs) {
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
                let mut snapshot = self.array_snapshot(idx);
                if cmp.is_heap() && self.heap.as_callable(cmp.heap_index()).is_some() {
                    // Comparator sort: stable O(n log n) bottom-up merge sort,
                    // re-entering the VM for each comparison.
                    self.comparator_sort(&mut snapshot, cmp)?;
                } else {
                    // Default sort: by string coercion (JS spec default).
                    snapshot.sort_by(|a, b| self.display(*a).cmp(&self.display(*b)));
                }
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    *items = snapshot;
                }
                Ok(Some(Value::heap(idx)))
            }
            _ => Ok(None),
        }
    }

    /// Stable bottom-up merge sort driven by a JS comparator (`cmp(a,b) < 0` ⇒
    /// `a` before `b`). O(n log n) comparisons — vs the old insertion sort's
    /// O(n²), which dominated `Array.sort` for non-trivial sizes. Stable: on a tie
    /// (and on `<= 0`) the LEFT run's element wins, preserving original order. The
    /// comparator re-enters the VM (`call_value`) and may throw (propagated).
    fn comparator_sort(&mut self, items: &mut [Value], cmp: Value) -> Result<(), Thrown> {
        let n = items.len();
        if n < 2 {
            return Ok(());
        }
        // Ping-pong between two local buffers (not self.regs/heap, so a comparator
        // that re-enters the VM and allocates can't invalidate them).
        let mut a: Vec<Value> = items.to_vec();
        let mut b: Vec<Value> = vec![Value::UNDEFINED; n];
        let mut width = 1;
        while width < n {
            let mut lo = 0;
            while lo < n {
                let mid = (lo + width).min(n);
                let hi = (lo + 2 * width).min(n);
                // Merge a[lo..mid] and a[mid..hi] into b[lo..hi], stably.
                let (mut l, mut r, mut k) = (lo, mid, lo);
                while l < mid && r < hi {
                    let c = self.call_value(cmp, Value::UNDEFINED, &[a[l], a[r]])?;
                    if c.as_f64() <= 0.0 {
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
        items.copy_from_slice(&a);
        Ok(())
    }

    fn string_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        self.heap.flatten(idx); // materialize a rope receiver before reading it
        let s = match self.heap.get(idx) {
            HeapObj::Str(s) => s.bytes.clone(),
            _ => return Ok(None),
        };
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "charAt" => {
                let i = arg0.as_f64() as i32;
                let ch = if i >= 0 { s.chars().nth(i as usize) } else { None };
                Ok(Some(self.alloc_str(ch.map(|c| c.to_string()).unwrap_or_default())))
            }
            "charCodeAt" => {
                let i = arg0.as_f64() as i32;
                let cc = if i >= 0 { s.chars().nth(i as usize) } else { None };
                Ok(Some(match cc {
                    Some(c) => Value::int(c as i32),
                    None => Value::num(f64::NAN),
                }))
            }
            "indexOf" => {
                let needle = self.display(arg0);
                let pos = s.find(&needle).map(|b| s[..b].chars().count() as i32).unwrap_or(-1);
                Ok(Some(Value::int(pos)))
            }
            "includes" => {
                let needle = self.display(arg0);
                Ok(Some(Value::bool(s.contains(&needle))))
            }
            "toUpperCase" => Ok(Some(self.alloc_str(s.to_uppercase()))),
            "toLowerCase" => Ok(Some(self.alloc_str(s.to_lowercase()))),
            "slice" | "substring" => {
                let len = s.chars().count() as i32;
                let start = norm_index(if args.is_empty() { 0 } else { arg0.as_f64() as i32 }, len);
                let end = if args.len() < 2 { len } else { norm_index(args[1].as_f64() as i32, len) };
                let out: String = if start < end {
                    s.chars().skip(start as usize).take((end - start) as usize).collect()
                } else {
                    String::new()
                };
                Ok(Some(self.alloc_str(out)))
            }
            "repeat" => {
                let n = arg0.as_f64();
                if n < 0.0 || !n.is_finite() {
                    return Err(Thrown("RangeError: Invalid count value".into()));
                }
                Ok(Some(self.alloc_str(s.repeat(n as usize))))
            }
            "split" => {
                let sep = self.display(arg0);
                let parts: Vec<Value> = if args.is_empty() {
                    vec![self.alloc_str(s.clone())]
                } else if sep.is_empty() {
                    s.chars().map(|c| self.alloc_str(c.to_string())).collect()
                } else {
                    s.split(&sep).map(|p| self.alloc_str(p.to_string())).collect()
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(parts)))))
            }
            "trim" => Ok(Some(self.alloc_str(s.trim().to_string()))),
            "trimStart" => Ok(Some(self.alloc_str(s.trim_start().to_string()))),
            "trimEnd" => Ok(Some(self.alloc_str(s.trim_end().to_string()))),
            "startsWith" => Ok(Some(Value::bool(s.starts_with(&self.display(arg0))))),
            "endsWith" => Ok(Some(Value::bool(s.ends_with(&self.display(arg0))))),
            "padStart" | "padEnd" => {
                let cur = s.chars().count();
                let t = arg0.as_f64();
                let target = if t.is_finite() && t > 0.0 { t as usize } else { 0 };
                if cur >= target {
                    return Ok(Some(self.alloc_str(s.clone())));
                }
                let pad = if args.len() >= 2 { self.display(args[1]) } else { " ".to_string() };
                let padchars: Vec<char> = pad.chars().collect();
                if padchars.is_empty() {
                    return Ok(Some(self.alloc_str(s.clone())));
                }
                let mut padding = String::new();
                for k in 0..(target - cur) {
                    padding.push(padchars[k % padchars.len()]);
                }
                let out = if name == "padStart" {
                    format!("{padding}{s}")
                } else {
                    format!("{s}{padding}")
                };
                Ok(Some(self.alloc_str(out)))
            }
            "replace" => {
                // String search: replaces only the FIRST occurrence (JS semantics).
                let search = self.display(arg0);
                let repl = if args.len() >= 2 { self.display(args[1]) } else { "undefined".to_string() };
                let out = match s.find(&search) {
                    Some(pos) => {
                        let mut r = String::with_capacity(s.len() + repl.len());
                        r.push_str(&s[..pos]);
                        r.push_str(&repl);
                        r.push_str(&s[pos + search.len()..]);
                        r
                    }
                    None => s.clone(),
                };
                Ok(Some(self.alloc_str(out)))
            }
            "replaceAll" => {
                let search = self.display(arg0);
                let repl = if args.len() >= 2 { self.display(args[1]) } else { "undefined".to_string() };
                Ok(Some(self.alloc_str(s.replace(&search, &repl))))
            }
            _ => Ok(None),
        }
    }

    /// Clone an array's current elements out of the heap. Used before invoking
    /// callbacks so a heap reallocation during the call can't dangle a borrow.
    fn array_snapshot(&self, idx: u32) -> Vec<Value> {
        match self.heap.get(idx) {
            HeapObj::Array(items) => items.clone(),
            _ => Vec::new(),
        }
    }

    /// Recursively flatten nested arrays up to `depth` levels (for `Array.flat`).
    /// Each nested array is cloned out before recursing (releases the heap borrow).
    fn flatten_array(&self, items: &[Value], depth: i32) -> Vec<Value> {
        let mut out = Vec::new();
        for v in items {
            let nested: Option<Vec<Value>> = if depth > 0 && v.is_heap() {
                match self.heap.get(v.heap_index()) {
                    HeapObj::Array(a) => Some(a.clone()),
                    _ => None,
                }
            } else {
                None
            };
            match nested {
                Some(a) => out.extend(self.flatten_array(&a, depth - 1)),
                None => out.push(*v),
            }
        }
        out
    }

    /// Strict equality between two raw values (no register indirection). Mirrors
    /// `strict_eq` but takes values directly, for builtin use.
    fn values_strict_eq(&self, a: Value, b: Value) -> bool {
        if a.bits() == b.bits() {
            if a.is_double() && a.as_f64().is_nan() {
                return false;
            }
            return true;
        }
        if a.is_number() && b.is_number() {
            return a.as_f64() == b.as_f64();
        }
        if a.is_heap() && b.is_heap() {
            let (ai, bi) = (a.heap_index(), b.heap_index());
            if self.heap.is_str_like(ai) && self.heap.is_str_like(bi) {
                return self.heap.str_eq(ai, bi);
            }
        }
        false
    }

    /// JS loose equality `==` (the Abstract Equality Comparison). Same-type
    /// compares like `===`; cross-type coerces per spec: null == undefined;
    /// number vs string coerces the string to a number; boolean coerces to a
    /// number; an object vs a primitive coerces the object to its primitive
    /// (here: string coercion, since we have no valueOf). NaN is never equal.
    fn loose_eq(&self, a: Value, b: Value) -> Result<bool, Thrown> {
        // Same NaN-box tag class → strict semantics already cover it.
        if (a.is_number() && b.is_number())
            || (a.is_bool() && b.is_bool())
            || (a.is_heap() && b.is_heap())
        {
            return Ok(self.values_strict_eq(a, b));
        }
        // null == undefined (and each with itself), but not with anything else.
        if a.is_nullish() || b.is_nullish() {
            return Ok(a.is_nullish() && b.is_nullish());
        }
        // From here neither side is null/undefined. Coerce toward numbers,
        // except string-vs-string (handled above via the heap case) and
        // string-vs-heapobject which JS compares by string.
        // boolean → number, then retry.
        if a.is_bool() {
            return self.loose_eq(Value::num(if a.as_bool() { 1.0 } else { 0.0 }), b);
        }
        if b.is_bool() {
            return self.loose_eq(a, Value::num(if b.as_bool() { 1.0 } else { 0.0 }));
        }
        // number vs string: coerce string to number.
        // string vs object / number vs object: coerce via to_number (objects
        // become NaN here, matching `1 == {}` → false; `"[object Object]"`
        // string comparisons aren't reached because both-heap is handled above).
        let an = self.to_number(a)?;
        let bn = self.to_number(b)?;
        Ok(an == bn)
    }

    // ── arithmetic / coercion helpers ──

    #[inline]
    fn add(&mut self, base: usize, a: u16, b: u16) -> Result<Value, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        // Fast path: int + int with overflow check.
        if va.is_int() && vb.is_int() {
            return Ok(match va.as_int().checked_add(vb.as_int()) {
                Some(v) => Value::int(v),
                None => Value::num(va.as_int() as f64 + vb.as_int() as f64),
            });
        }
        // If either side is a heap value, JS `+` is string concatenation (arrays
        // and objects coerce to a string primitive, and string+anything joins).
        // Build a rope (cons-string) in O(1) — children point at existing flat
        // strings / ropes, so a `s += x` loop is O(n) overall, not O(n²).
        if va.is_heap() || vb.is_heap() {
            let li = self.to_str_idx(va);
            let ri = self.to_str_idx(vb);
            let llen = self.heap.str_char_len(li).unwrap_or(0);
            let rlen = self.heap.str_char_len(ri).unwrap_or(0);
            return Ok(Value::heap(self.heap.alloc_cons(li, ri, llen + rlen)));
        }
        Ok(Value::num(self.to_number(va)? + self.to_number(vb)?))
    }

    /// Heap index of a string-like object representing `v`: `v`'s own index when
    /// it is already a string (flat or rope), else a freshly allocated flat
    /// string from `v`'s string coercion. Used to build rope children.
    fn to_str_idx(&mut self, v: Value) -> u32 {
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            v.heap_index()
        } else {
            let s = self.display(v);
            self.heap.alloc_str(s)
        }
    }

    #[inline]
    fn cmp_lt(&mut self, base: usize, a: u16, b: u16) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() < vb.as_int());
        }
        Ok(self.to_number(va)? < self.to_number(vb)?)
    }
    #[inline]
    fn cmp_le(&mut self, base: usize, a: u16, b: u16) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() <= vb.as_int());
        }
        Ok(self.to_number(va)? <= self.to_number(vb)?)
    }

    fn strict_eq(&self, base: usize, a: u16, b: u16) -> bool {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        // Same bits → equal (covers int, bool, null, undefined, same heap idx).
        if va.bits() == vb.bits() {
            // NaN !== NaN even with identical bits.
            if va.is_double() && va.as_f64().is_nan() {
                return false;
            }
            return true;
        }
        // Numeric cross-representation (int vs double) compares by value.
        if va.is_number() && vb.is_number() {
            return va.as_f64() == vb.as_f64();
        }
        // Distinct heap strings with equal contents are `===` equal.
        if va.is_heap() && vb.is_heap() {
            let (ai, bi) = (va.heap_index(), vb.heap_index());
            if self.heap.is_str_like(ai) && self.heap.is_str_like(bi) {
                return self.heap.str_eq(ai, bi);
            }
        }
        false
    }

    #[inline]
    fn truthy(&self, v: Value) -> bool {
        if let Some(t) = v.truthy_primitive() {
            return t;
        }
        // Heap: empty string is falsy; everything else truthy.
        if let Some(empty) = self.heap.str_is_empty(v.heap_index()) {
            return !empty;
        }
        true
    }

    fn to_number(&self, v: Value) -> Result<f64, Thrown> {
        if v.is_number() {
            return Ok(v.as_f64());
        }
        if v.is_bool() {
            return Ok(if v.as_bool() { 1.0 } else { 0.0 });
        }
        if v.is_null() {
            return Ok(0.0);
        }
        if v.is_undefined() {
            return Ok(f64::NAN);
        }
        if let Some(s) = self.heap.str_cow(v.heap_index()) {
            let t = s.trim();
            if t.is_empty() {
                return Ok(0.0);
            }
            return Ok(t.parse::<f64>().unwrap_or(f64::NAN));
        }
        Ok(f64::NAN)
    }

    /// String COERCION (`String(v)`, `'' + v`, property keys). Arrays join with
    /// commas; objects become `[object Object]` — JS `toString` semantics.
    fn display(&self, v: Value) -> String {
        if v.is_int() {
            v.as_int().to_string()
        } else if v.is_double() {
            fmt_f64(v.as_f64())
        } else if v.is_bool() {
            v.as_bool().to_string()
        } else if v.is_null() {
            "null".into()
        } else if v.is_undefined() {
            "undefined".into()
        } else if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Str(s) => s.bytes.clone(),
                HeapObj::Cons { .. } => {
                    let mut out = String::new();
                    self.heap.write_str(v.heap_index(), &mut out);
                    out
                }
                HeapObj::Func(_) | HeapObj::Closure { .. } => "function".into(),
                HeapObj::Cell(inner) => self.display(*inner),
                HeapObj::Array(items) => items
                    .iter()
                    .map(|e| if e.is_nullish() { String::new() } else { self.display(*e) })
                    .collect::<Vec<_>>()
                    .join(","),
                HeapObj::Object(_) => "[object Object]".into(),
            }
        } else {
            "undefined".into()
        }
    }

    /// INSPECT (`console.log` rendering). Strings are quoted only when nested;
    /// arrays/objects use node's spaced bracket style (`[ 1, 2, 3 ]`,
    /// `{ a: 1 }`).
    fn inspect(&self, v: Value) -> String {
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Str(s) => return s.bytes.clone(), // top-level strings unquoted
                HeapObj::Cons { .. } => {
                    let mut out = String::new();
                    self.heap.write_str(v.heap_index(), &mut out);
                    return out;
                }
                _ => return self.inspect_nested(v),
            }
        }
        self.display(v)
    }

    fn inspect_nested(&self, v: Value) -> String {
        if !v.is_heap() {
            return self.display(v);
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Str(s) => format!("'{}'", s.bytes),
            HeapObj::Cons { .. } => {
                let mut out = String::new();
                self.heap.write_str(v.heap_index(), &mut out);
                format!("'{out}'")
            }
            HeapObj::Func(_) | HeapObj::Closure { .. } => "[Function]".into(),
            HeapObj::Cell(inner) => self.inspect_nested(*inner),
            HeapObj::Array(items) => {
                if items.is_empty() {
                    return "[]".into();
                }
                let parts: Vec<String> = items.iter().map(|e| self.inspect_nested(*e)).collect();
                format!("[ {} ]", parts.join(", "))
            }
            HeapObj::Object(map) => {
                if map.keys.is_empty() {
                    return "{}".into();
                }
                let parts: Vec<String> = map
                    .keys
                    .iter()
                    .zip(map.vals.iter())
                    .map(|(k, val)| format!("{k}: {}", self.inspect_nested(*val)))
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
        }
    }

    /// Resolve a constant slot: most are plain Values; string constants are
    /// stored as a sentinel index into the function's `string_constants` and
    /// interned to a heap string on first use.
    #[inline]
    fn resolve_const(&mut self, func_id: u32, idx: u32, v: Value) -> Value {
        // String constants are encoded as `Value::heap(STRING_CONST_BIT | i)`.
        if v.is_heap() && (v.heap_index() & STRING_CONST_BIT) != 0 {
            let si = (v.heap_index() & !STRING_CONST_BIT) as usize;
            let s = self.program.functions[func_id as usize].string_constants[si].clone();
            return self.alloc_str(s);
        }
        v
    }
}

/// High bit of a heap index marks a "string constant pending interning" slot
/// in a `LoadConst` Value (see `resolve_const`). Real heap indices never set
/// this bit (the heap would need 2^31 objects).
pub const STRING_CONST_BIT: u32 = 0x8000_0000;

/// Per-function: which global slot the function's name binds to, if any. The
/// compiler stores it in `param_count`'s sibling — but to keep `FuncProto`
/// simple we encode it via a convention: a function whose name is hoisted to a
/// global has that slot recorded in a side table. For v1 the compiler sets it
/// through `FuncProto`-adjacent metadata; we read it here.
fn function_global_slot(f: &crate::bytecode::FuncProto) -> Option<u16> {
    f.name_global
}

/// Maximum native self-recursion depth before the JIT self-call helper deopts
/// to the interpreter (which continues on its EXPLICIT frame stack and enforces
/// MAX_FRAMES → catchable RangeError). This MUST stay well below what the native
/// Rust stack can hold, because each native self-call nests
/// `jit_self_call → JitFn::run → call helper → jit_self_call_impl → JitFn::run`
/// on the OS stack. 256 levels is safe on a default stack and is plenty to keep
/// realistic recursion (fib, etc.) native; deeper legal recursion transparently
/// continues on the interpreter (correct, just not JIT-accelerated past 256),
/// and runaway recursion deopts → interpreter → RangeError, never a segfault.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const JIT_SELF_RECURSE_MAX: u32 = 256;

/// Win64 helper invoked by JIT'd native code for a self-recursive call:
/// `result_bits = self(args[0..argc])`, where `self` is the function with id
/// `func_id`. Returns the result `Value` bits, or `codegen::SELF_CALL_DEOPT`
/// when it can't run natively (depth exceeded, a non-int arg, or the callee
/// isn't int-JIT'd) — the native caller then bails that Call to the interpreter.
///
/// SAFETY / why the caller's register window survives: the recursive frame runs
/// on a SEPARATE, freshly-allocated register buffer (`window`), NOT `vm.regs`.
/// So nothing here resizes `vm.regs`, and the native caller's `rbx` (pointer
/// into its own window) stays valid across this call. The native callee gets
/// `window.as_mut_ptr()` as its regs base. If the callee itself bails mid-body,
/// we finish that activation through the interpreter over the SAME window via a
/// transient frame, then restore vm state.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `args` points to `argc` valid `Value` bits.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_self_call(
    vm: *mut core::ffi::c_void,
    func_id: u32,
    args: *const u64,
    argc: u32,
) -> u64 {
    // Catch Rust panics at the FFI boundary (UB to unwind across `extern`).
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_self_call_impl(func_id, args, argc as usize)
    }));
    match r {
        Ok(bits) => bits,
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper: the INLINE-CACHE MISS path for a JIT'd `GetProp`. The native
/// fast path (identity + version check, direct `vals[slot]` read) only calls this
/// when its cache misses. Looks up `obj.<key>`, and on the fast-path-eligible case
/// (a plain Object that HAS the key) fills inline-cache slot `site` with
/// `(obj_bits, vals.as_ptr(), version, slot)` so subsequent accesses are call-free.
/// Returns the property bits, or `SELF_CALL_DEOPT` (non-Object → interpreter
/// re-executes at this ip; arrays/strings/`.length`/null/undefined handled there).
/// A missing key on an Object returns `undefined` WITHOUT caching (rare).
/// `packed = (func_id << 32) | name_idx`.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
/// Win64 helper for a JIT'd dense-array element read `a[i]` (`GetIndex`).
/// Returns the element's Value bits; `undefined` bits for an in-bounds-checks-fail
/// (negative or `>= len`) index, matching JS `a[oob] === undefined`; or
/// `SELF_CALL_DEOPT` for a non-array receiver or a non-int key (string indexing,
/// `arr["foo"]`, etc.) so the interpreter re-executes this op. Read-only — no
/// caching needed (a dense array's element address is a direct `vals[i]`).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_index(
    vm: *mut core::ffi::c_void,
    arr_bits: u64,
    key_bits: u64,
) -> u64 {
    let arr = Value::from_bits(arr_bits);
    let key = Value::from_bits(key_bits);
    // Only a numeric key on a heap object is handled here; a string/other key
    // (or non-heap receiver) deopts so the interpreter applies full semantics.
    if !arr.is_heap() || !key.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: read-only view; the running region holds no conflicting borrow.
    let vm = unsafe { &*(vm as *const Vm) };
    match vm.heap.get(arr.heap_index()) {
        HeapObj::Array(items) => match array_index(key) {
            // In range → the element; out of range / negative / non-integral →
            // undefined (matches JS and the interpreter's get_index).
            Some(i) if i < items.len() => items[i].bits(),
            _ => Value::UNDEFINED.bits(),
        },
        _ => crate::codegen::SELF_CALL_DEOPT, // strings etc → interpreter
    }
}

/// Win64 helper for a JIT'd dense-array element write `a[i] = v` (`SetIndex`).
/// Stores in place when `i < len`, grows the array with `undefined` holes when
/// `i >= len` (matching JS and the interpreter's set_index). Returns `0` on
/// success, or `SELF_CALL_DEOPT` for a non-array receiver / negative / fractional
/// / non-numeric key (the interpreter then applies its no-op fallback). Reads the
/// live array fresh each call — no cached pointer, so a grow that reallocates is
/// safe (the region pins only the register file, never array storage).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_set_index(
    vm: *mut core::ffi::c_void,
    arr_bits: u64,
    key_bits: u64,
    val_bits: u64,
) -> u64 {
    let arr = Value::from_bits(arr_bits);
    let key = Value::from_bits(key_bits);
    if !arr.is_heap() || !key.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let i = match array_index(key) {
        Some(i) => i,
        None => return crate::codegen::SELF_CALL_DEOPT, // negative/fractional → interpreter
    };
    // SAFETY: exclusive view; the running region holds no conflicting borrow and
    // pins only the register file (not the array's Vec, which may reallocate).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.heap.get_mut(arr.heap_index()) {
        HeapObj::Array(items) => {
            let len = items.len();
            if i < len {
                items[i] = Value::from_bits(val_bits); // in-range store
            } else if i == len {
                items.push(Value::from_bits(val_bits)); // append (grow by one)
            } else {
                // A sparse write (i > len) would resize-with-holes — possibly a
                // huge allocation. Deopt so the INTERPRETER does the resize: its
                // panic on a giant/failed allocation unwinds through normal Rust,
                // not across this `extern "win64"` boundary (which would be UB).
                return crate::codegen::SELF_CALL_DEOPT;
            }
            0
        }
        _ => crate::codegen::SELF_CALL_DEOPT, // non-array → interpreter
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_prop_miss(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    site_idx: u32,
    packed: u64,
) -> u64 {
    let obj = Value::from_bits(obj_bits);
    if !obj.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: exclusive view (updates the IC table); the running region holds no
    // conflicting borrow (the IC table and the region live in different fields).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let func_id = (packed >> 32) as u32;
    let name_idx = packed as u32;
    let idx = obj.heap_index();
    let prog = vm.program; // &'p Program, independent of `vm`'s borrow
    let key = &prog.functions[func_id as usize].string_constants[name_idx as usize];
    let (val, vals_ptr, slot) = match vm.heap.get(idx) {
        HeapObj::Object(map) => match map.keys.iter().position(|k| k == key) {
            Some(s) => (map.vals[s], map.vals.as_ptr() as u64, s as u32),
            None => return Value::UNDEFINED.bits(), // missing key: undefined, don't cache
        },
        // `arr.length` / `str.length` in a region: return the length WITHOUT
        // caching — it's derived from the container's element count, not a fixed
        // slot, so a stale cache would be wrong after the container grows. The IC
        // entry stays unset, so this site simply misses (helper call) each time —
        // cheap, and it lets a `for (i < a.length) a[i]` loop run as a region
        // instead of bailing on the first `.length` access.
        HeapObj::Array(items) if key == "length" => return len_value(items.len()).bits(),
        HeapObj::Str(s) if key == "length" => return len_value(s.char_len).bits(),
        HeapObj::Cons { len, .. } if key == "length" => return len_value(*len).bits(),
        _ => return crate::codegen::SELF_CALL_DEOPT, // other array/string props → interpreter
    };
    let version = vm.heap.version_of(idx);
    vm.jit.set_ic(site_idx, obj_bits, vals_ptr, version, slot);
    val.bits()
}

/// Win64 helper: the INLINE-CACHE MISS path for a JIT'd `SetProp`. Performs
/// `obj.<key> = val`, then (for a plain Object) fills inline-cache slot `site` so
/// later writes are call-free. Returns `0` (success — incl. a heap non-Object,
/// which no-ops, matching the interpreter) or `SELF_CALL_DEOPT` (null/undefined →
/// the interpreter throws). `packed = (func_id << 32) | name_idx`; `site_idx` is
/// the 5th argument (passed on the stack by the caller).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_set_prop_miss(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    val_bits: u64,
    packed: u64,
    site_idx: u32,
) -> u64 {
    let obj = Value::from_bits(obj_bits);
    if !obj.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let func_id = (packed >> 32) as u32;
    let name_idx = packed as u32;
    let idx = obj.heap_index();
    let prog = vm.program;
    let key = &prog.functions[func_id as usize].string_constants[name_idx as usize];
    let (added, vals_ptr, slot) = match vm.heap.get_mut(idx) {
        HeapObj::Object(map) => {
            let added = map.set(key, Value::from_bits(val_bits));
            // Position AFTER the set (existing key: unchanged; new key: appended).
            let s = map.keys.iter().position(|k| k == key).unwrap() as u32;
            (added, map.vals.as_ptr() as u64, s)
        }
        // `arr.length = n` truncates/grows — deopt so the interpreter's set_prop
        // applies it (no-op here would diverge from the interpreter).
        HeapObj::Array(_) if key == "length" => return crate::codegen::SELF_CALL_DEOPT,
        _ => return 0, // other heap non-Object props: silent no-op (matches interpreter)
    };
    if added {
        vm.heap.bump_version(idx);
    }
    let version = vm.heap.version_of(idx);
    vm.jit.set_ic(site_idx, obj_bits, vals_ptr, version, slot);
    0
}

/// Win64 helper: base pointer of the heap's per-object version array, pinned by a
/// heap-op region's prologue. Stable for the run (a region never allocates a heap
/// object, so the array doesn't reallocate).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_heap_versions_base(vm: *mut core::ffi::c_void) -> *const u32 {
    let vm = unsafe { &*(vm as *const Vm) };
    vm.heap.versions_ptr()
}

/// Win64 helper: base pointer of the JIT inline-cache table, pinned by a heap-op
/// region's prologue. Stable for the run (the table grows only at compile time,
/// and a `*_miss` only updates an existing slot — never grows it).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_ic_base(vm: *mut core::ffi::c_void) -> *const core::ffi::c_void {
    let vm = unsafe { &*(vm as *const Vm) };
    vm.jit.ic_base_ptr() as *const core::ffi::c_void
}

/// Win64 helper: the base pointer of `vm.globals`, fetched once by an OSR loop
/// region's prologue and pinned in a callee-saved register for direct
/// `LoadGlobal`/`StoreGlobal`. Sound because `globals` is allocated once at VM
/// construction (`global_count` slots) and never reallocates at runtime.
///
/// # Safety
/// `vm` is a valid `*mut Vm` that outlives the region run.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_globals_base(vm: *mut core::ffi::c_void) -> *mut u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    vm.globals.as_mut_ptr() as *mut u64
}

/// Normalise a (possibly negative) slice index into `[0, len]`. Negative
/// indices count from the end; out-of-range clamps. Matches JS slice/substring.
fn norm_index(i: i32, len: i32) -> i32 {
    let v = if i < 0 { len + i } else { i };
    v.clamp(0, len)
}

/// A `.length` / array-length result as a JS Number. An `Int` when it fits in
/// i32 (the overwhelmingly common case), otherwise a double — so a length beyond
/// 2^31 (cheap to reach now that ropes concatenate lazily without flattening)
/// reports its true magnitude instead of wrapping negative through `as i32`.
/// Integers up to 2^53 are exact in f64, matching JS.
#[inline]
fn len_value(n: usize) -> Value {
    if n <= i32::MAX as usize {
        Value::int(n as i32)
    } else {
        Value::num(n as f64)
    }
}

/// JS `parseInt(s, radix)`: skip leading whitespace, an optional sign, an
/// optional `0x` prefix (radix 16), then digits in `radix` (default 10); stop at
/// the first invalid digit. `NaN` if no digits parse. `radix == 0` means "auto".
fn parse_int(s: &str, radix: i32) -> f64 {
    let b = s.trim_start().as_bytes();
    let mut i = 0;
    let mut sign = 1.0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        if b[i] == b'-' {
            sign = -1.0;
        }
        i += 1;
    }
    let mut radix = radix;
    if (radix == 16 || radix == 0)
        && i + 1 < b.len()
        && b[i] == b'0'
        && (b[i + 1] == b'x' || b[i + 1] == b'X')
    {
        i += 2;
        radix = 16;
    }
    if radix == 0 {
        radix = 10;
    }
    if !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let start = i;
    let mut val = 0.0;
    while i < b.len() {
        let d = match b[i] {
            c @ b'0'..=b'9' => (c - b'0') as i32,
            c @ b'a'..=b'z' => (c - b'a' + 10) as i32,
            c @ b'A'..=b'Z' => (c - b'A' + 10) as i32,
            _ => break,
        };
        if d >= radix {
            break;
        }
        val = val * radix as f64 + d as f64;
        i += 1;
    }
    if i == start {
        f64::NAN
    } else {
        sign * val
    }
}

/// JS `parseFloat(s)`: skip leading whitespace, then parse the longest leading
/// decimal-float prefix (sign, digits, `.`, exponent, or `Infinity`). `NaN` if
/// none.
fn parse_float(s: &str) -> f64 {
    let t = s.trim_start();
    let b = t.as_bytes();
    let mut end = 0;
    if end < b.len() && (b[end] == b'+' || b[end] == b'-') {
        end += 1;
    }
    if t[end..].starts_with("Infinity") {
        return if t.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let mut saw_digit = false;
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
        saw_digit = true;
    }
    if end < b.len() && b[end] == b'.' {
        end += 1;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
            saw_digit = true;
        }
    }
    if !saw_digit {
        return f64::NAN;
    }
    // Optional exponent — only consumed if it has at least one digit.
    if end < b.len() && (b[end] == b'e' || b[end] == b'E') {
        let mut e = end + 1;
        if e < b.len() && (b[e] == b'+' || b[e] == b'-') {
            e += 1;
        }
        let exp_start = e;
        while e < b.len() && b[e].is_ascii_digit() {
            e += 1;
        }
        if e > exp_start {
            end = e;
        }
    }
    t[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// A non-negative array index from a numeric key, coercing an integral double
/// the way JS does (`a[1.0]` is `a[1]`). `None` for a negative, non-integral, or
/// non-numeric key (those address no dense element → `undefined`). The JIT region
/// computes loop counters as f64, so `a[i]` arrives here with a double key.
#[inline]
fn array_index(key: Value) -> Option<usize> {
    if key.is_int() {
        let i = key.as_int();
        (i >= 0).then_some(i as usize)
    } else if key.is_double() {
        let d = key.as_f64();
        // Reject negatives, fractions, and absurdly large indices (≥ 2^32).
        if d >= 0.0 && d.fract() == 0.0 && d < 4_294_967_296.0 {
            Some(d as usize)
        } else {
            None
        }
    } else {
        None
    }
}

/// Quote a string as a JSON string literal (escaping per the JSON spec).
fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Wrap JSON `parts` in `open`/`close`, compact when `indent` is empty, else
/// one element per line indented `depth+1` deep with the closing bracket at `depth`.
fn wrap_json(parts: &[String], open: char, close: char, indent: &str, depth: usize) -> String {
    if indent.is_empty() {
        return format!("{}{}{}", open, parts.join(","), close);
    }
    let pad = indent.repeat(depth + 1);
    let pad_close = indent.repeat(depth);
    let sep = format!(",\n{pad}");
    format!("{open}\n{pad}{}\n{pad_close}{close}", parts.join(&sep))
}

fn fmt_f64(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n == 0.0 {
        return "0".into();
    }
    // Integer-valued doubles print without a decimal point (JS semantics). Use
    // Rust's shortest-round-trip f64 Display (matches JS Number→String, which
    // prints the shortest decimal that round-trips, e.g. 4660046610375530000 not
    // ...496) — NOT `n as i64`, which prints excess digits the f64 can't
    // distinguish and overflows for whole doubles above i64::MAX.
    if n.fract() == 0.0 && n.abs() < 1e21 {
        return format!("{n}");
    }
    let mut s = format!("{n}");
    if s.contains('e') {
        // JS uses e+/e- exponent formatting; Rust already does e.g. 1e21.
        s = s.replace('e', "e+").replace("e+-", "e-");
    }
    s
}
