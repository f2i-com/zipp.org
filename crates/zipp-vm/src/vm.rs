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

use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap, PropAttr, PromiseState, Reaction,
};
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

/// Whether a promise reaction is the fulfill or reject handler.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReactionKind {
    Fulfill,
    Reject,
}

/// How a suspended async activation is resumed: with an awaited value, or by
/// throwing a rejection into it at the await point.
#[derive(Clone, Copy)]
enum Resume {
    Value(Value),
    Throw(Value),
}

/// A queued microtask (the whole event loop). `Reaction` runs a promise reaction
/// — `callback` (a JS fn, a native BoundResolver, or undefined for pass-through)
/// applied to the settled `arg`, settling `dependent`. `AsyncResume` resumes a
/// suspended async activation.
enum Microtask {
    Reaction { callback: Value, arg: Value, dependent: u32, kind: ReactionKind, finally: bool },
    AsyncResume { activation: u32, input: Resume },
}

/// Native (built-in) function ids — the discriminant carried by `HeapObj::Native`.
/// Each maps to an arm of `Vm::call_native`.
mod native {
    pub const OBJ_DEFINE_PROPERTY: u16 = 1;
    pub const OBJ_DEFINE_PROPERTIES: u16 = 2;
    pub const OBJ_GET_OWN_DESC: u16 = 3;
    pub const OBJ_GET_OWN_NAMES: u16 = 4;
    pub const OBJ_GET_PROTO: u16 = 5;
    pub const OBJ_KEYS: u16 = 6;
    pub const OBJ_VALUES: u16 = 7;
    pub const OBJ_ENTRIES: u16 = 8;
    pub const OBJ_ASSIGN: u16 = 9;
    pub const OBJ_CREATE: u16 = 10;
    pub const PROTO_HAS_OWN: u16 = 11;
    pub const PROTO_PROP_ENUM: u16 = 12;
    pub const PROTO_IS_PROTO_OF: u16 = 13;
    pub const PROTO_VALUE_OF: u16 = 14;
    pub const PROTO_TO_STRING: u16 = 15;
    pub const FN_CALL: u16 = 16;
    pub const FN_APPLY: u16 = 17;
    pub const FN_BIND: u16 = 18;
    pub const ARR_IS_ARRAY: u16 = 19;
    pub const ARR_FROM: u16 = 20;
    pub const ARR_OF: u16 = 21;
    pub const ARR_JOIN: u16 = 22;
    pub const ARR_PUSH: u16 = 23;
}

/// What `object_enum_own` collects.
#[derive(Clone, Copy)]
enum EnumWhat {
    Keys,
    Values,
    Entries,
}

pub struct Vm<'p> {
    program: &'p Program,
    /// Most-recent class value per class_id (filled by `MakeClass`), so a
    /// `super` call can reach its lexical superclass value at runtime.
    class_values: Vec<Option<Value>>,
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
    /// Set by a `Yield` op to hand a generator's yielded value (+ the yield's
    /// bytecode ip, for the resume point) back to `generator_method`, which
    /// `.take()`s it to distinguish a suspension from a normal return.
    pending_yield: Option<(Value, usize)>,
    /// Set by an `Await` op (the awaited value + the Await's ip + the activation's
    /// live `try` handlers); `drive_async` `.take()`s it to suspend the async
    /// activation, mirroring `pending_yield`. Unlike generators, async activations
    /// PRESERVE handlers across a suspension so `try { await p } catch` works.
    pending_await: Option<(Value, usize, Vec<Handler>)>,
    /// FIFO microtask queue — the entire event loop (no timers/IO exist). Drained
    /// to empty by `drain_microtasks` after the main script returns; a microtask
    /// may enqueue more, which run in the same drain.
    microtasks: std::collections::VecDeque<Microtask>,
    /// The `.raw` array of a tagged-template strings object, keyed by the cooked
    /// array's heap index. Arrays don't carry named properties here, so a
    /// template object's `raw` lives in this side table (read by `get_prop`).
    template_raws: std::collections::HashMap<u32, Value>,
    /// Lazily-created `.prototype` object for a function/class value, keyed by the
    /// callable's heap index. `Fn.prototype` / `Class.prototype` must return a
    /// stable object (identity: `C.prototype === C.prototype`), so it is built on
    /// first access and cached here. For a class it carries the own methods +
    /// `constructor`; for a plain function just `constructor`.
    prototypes: std::collections::HashMap<u32, u32>,
    /// Explicit `[[Prototype]]` recorded for an `Object.create(proto)` object,
    /// keyed by the new object's heap index (read by `Object.getPrototypeOf`).
    proto_of: std::collections::HashMap<u32, Value>,
    /// Own properties set on a function value (`fn.x = y`, e.g. `assert.sameValue`),
    /// keyed by the callable's heap index. Functions can't carry an inline ObjMap,
    /// so their (rare) own props live here.
    fn_props: std::collections::HashMap<u32, ObjMap>,
    /// Callables expose `name`/`length` as synthesized own properties (computed
    /// from the proto, not stored). They're `configurable: true`, so `delete
    /// fn.name` must make them vanish — recorded here as `(heap_idx, 0=name |
    /// 1=length)`. Empty in normal programs; only `delete` on these keys fills it.
    deleted_callable_intrinsics: std::collections::HashSet<(u32, u8)>,
    /// Heap indices of the built-in prototype objects (`Object.prototype`,
    /// `Function.prototype`, `Array.prototype`), built by `setup_globals`. Used as
    /// the [[Prototype]] for plain objects / functions / arrays so their methods
    /// resolve as values and `getPrototypeOf` returns them. 0 until set up.
    obj_proto: u32,
    fn_proto: u32,
    arr_proto: u32,
    /// `Math.random()` PRNG state (xorshift64*). Deterministically seeded, so a
    /// program's random sequence is reproducible run-to-run (and JIT-on == off).
    rng_state: u64,
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
            class_values: vec![None; program.classes.len()],
            heap,
            globals,
            regs: Vec::new(),
            frames: Vec::new(),
            output: Vec::new(),
            errput: Vec::new(),
            start: std::time::Instant::now(),
            pending_throw: None,
            pending_yield: None,
            pending_await: None,
            microtasks: std::collections::VecDeque::new(),
            template_raws: std::collections::HashMap::new(),
            prototypes: std::collections::HashMap::new(),
            proto_of: std::collections::HashMap::new(),
            fn_props: std::collections::HashMap::new(),
            deleted_callable_intrinsics: std::collections::HashSet::new(),
            obj_proto: 0,
            fn_proto: 0,
            arr_proto: 0,
            rng_state: 0x9E37_79B9_7F4A_7C15, // fixed seed (golden-ratio constant)
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
    #[allow(dead_code)] // used by the differential test harness (run_nojit)
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
    ///
    /// NOTE: superseded by the inline native→native fast path + `jit_self_call_at`
    /// (the codegen now calls its own entry directly, no per-call Rust). Retained
    /// for reference / potential reuse; not on any hot path.
    #[allow(dead_code)]
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

    /// Slow/finish path for the JIT's inline native→native self-call. Called
    /// when the inline fast path can't complete a recursive call purely natively:
    /// either the native depth limit was hit, or the callee bailed mid-body. The
    /// caller passes its window base EXPLICITLY (`caller_base_ptr`, the native
    /// `rbx`) because the fast path tracks windows by raw pointer, not
    /// `self.regs.len()`. Runs the activation on the interpreter over a transient
    /// frame at the callee window, holding `jit_recurse_depth` ELEVATED for the
    /// duration so the dispatch JIT-entry gate (`== 0`) stays closed and the
    /// recursion can't re-enter native and livelock — frames then accumulate
    /// monotonically to `MAX_FRAMES` → catchable RangeError. Returns the result
    /// bits, or `SELF_CALL_DEOPT` if the activation threw (the throw is left in
    /// `pending_throw`; the native chain unwinds and the top-level interpreter
    /// re-raises it).
    ///
    /// # Safety
    /// `caller_base_ptr` is the caller's window base within `self.regs`; `args`
    /// points to `argc` valid `Value` bits.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn jit_self_call_at_impl(
        &mut self,
        func_id: u32,
        caller_base_ptr: *const u64,
        args: *const u64,
        argc: usize,
    ) -> u64 {
        let proto = &self.program.functions[func_id as usize];
        let reg_count = (proto.reg_count as usize).max(1);
        let params = proto.param_count as usize;

        // Caller window base as a slot index (the fast path placed it by raw
        // pointer); the callee window sits contiguously above it.
        let regs_base = self.regs.as_ptr() as *const u64;
        // SAFETY: caller_base_ptr lies within self.regs' (non-reallocating) buffer.
        let caller_base =
            unsafe { (caller_base_ptr).offset_from(regs_base) } as usize;
        let new_base = caller_base + reg_count;
        let needed = new_base + reg_count;
        if self.regs_would_overflow(needed) {
            // Out of reserved register headroom (very deep): treat as stack
            // overflow — throw so the interpreter surfaces a catchable RangeError.
            let e =
                self.alloc_error_from_message("RangeError: Maximum call stack size exceeded");
            self.pending_throw = Some(e);
            return crate::codegen::SELF_CALL_DEOPT;
        }

        // RESYNC self.regs.len() to span the callee window so the transient
        // interpreter frame + MAX_FRAMES accounting are consistent. Save the
        // entry length and restore it on the way out (the native caller doesn't
        // use `len`, but the eventual return to the dispatch loop expects it
        // unchanged).
        let saved_len = self.regs.len();
        // CRITICAL: grow `len` with `set_len`, NOT `resize`. The native fast path
        // advanced the register windows by raw pointer WITHOUT touching
        // `self.regs.len()`, so on entry here `len` (≈ the warmup top) is far below
        // the live native windows, which occupy slots up to `new_base`. A
        // `resize(needed, UNDEFINED)` would ZERO-FILL `[len, needed)` — overwriting
        // every parked native frame's registers with `undefined` and corrupting the
        // recursion (this was the bug that capped JIT recursion below the
        // interpreter). The native windows hold valid `Value`s already (each native
        // frame defs its registers before reading — the same def-before-use
        // invariant the leaf JIT relies on), and the buffer is pinned to capacity
        // by `reserve_jit_regs`, so simply exposing them via `set_len` is correct.
        // Bounds: `needed ≤ capacity` (guarded above by `regs_would_overflow`).
        // SAFETY: `needed ≤ capacity`; slots `[0, needed)` are live `Value`s —
        // `[0, len)` from the interpreter, `[len, new_base+reg_count)` written by
        // the native frames whose windows we're spanning.
        unsafe { self.regs.set_len(needed); }
        if needed > self.regs_hw {
            self.regs_hw = needed;
        }
        self.regs[new_base] = Value::UNDEFINED;
        let n = argc.min(params);
        for i in 0..n {
            self.regs[new_base + 1 + i] = Value::from_bits(unsafe { *args.add(i) });
        }

        // Run this activation on the interpreter via a transient frame. Depth is
        // held ELEVATED across the whole run (we only restore it after), so any
        // self-call inside stays interpreted (the dispatch gate sees depth != 0)
        // and the recursion can't re-enter native → no livelock; frames grow to
        // MAX_FRAMES → RangeError on runaway.
        self.jit_recurse_depth += 1;
        self.frames.push(Frame {
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure: NO_CLOSURE,
            handlers: Vec::new(),
        });
        let stop = self.frames.len() - 1;
        let r = self.run_loop(stop);
        self.jit_recurse_depth -= 1;
        // SAFETY: restore the entry length (allocation unchanged, slots valid).
        unsafe { self.regs.set_len(saved_len); }
        match r {
            Ok(v) => v.bits(),
            // Threw (e.g. RangeError): leave it in pending_throw and signal the
            // native caller to unwind; the top-level interpreter re-raises it.
            Err(_) => crate::codegen::SELF_CALL_DEOPT,
        }
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

    /// Run the top-level function (id 0) to completion.
    pub fn run(&mut self) -> Result<Value, Thrown> {
        // Inject the built-in global objects (Object/Array/Function + their
        // prototypes) into their reserved slots BEFORE hoisting, so a user
        // declaration of the same name shadows the builtin.
        self.setup_globals();
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
        // Run until the top-level frame returns (frames drains back to 0), then
        // run the event loop: drain queued microtasks (promise reactions, async
        // resumes) to empty. Drains even on a main throw (matches node ordering),
        // then returns the original result.
        let main = self.run_loop(0);
        self.drain_microtasks();
        main
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
        // A bound function: invoke its target with the fixed `this` and the bound
        // arguments prepended (handles bind-of-bind by recursing).
        if callee.is_heap() {
            if let HeapObj::Bound { target, this: bthis, args: bargs } = self.heap.get(callee.heap_index()) {
                let (t, th) = (*target, *bthis);
                let mut all = bargs.clone();
                all.extend_from_slice(args);
                return self.call_value(t, th, &all);
            }
            if let HeapObj::Native(id) = self.heap.get(callee.heap_index()) {
                let id = *id;
                return self.call_native(id, this, args);
            }
        }
        // A native resolve/reject function settles its bound promise.
        if callee.is_heap() {
            if let HeapObj::BoundResolver { promise, is_reject } = self.heap.get(callee.heap_index()) {
                let (p, isr) = (*promise, *is_reject);
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                if isr {
                    self.reject(p, arg);
                } else {
                    self.resolve(p, arg);
                }
                return Ok(Value::UNDEFINED);
            }
        }
        let (func_id, closure) = self.resolve_callable(callee)?;
        // Calling a generator function builds a suspended Generator, not a frame.
        if self.program.functions[func_id as usize].is_generator {
            return Ok(self.alloc_generator(func_id, closure, this, args));
        }
        // Calling an async function runs synchronously up to the first `await`,
        // then returns its result Promise.
        if self.program.functions[func_id as usize].is_async {
            return Ok(self.alloc_async(func_id, closure, this, args));
        }
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
        // Rest parameter: gather any args beyond the fixed params into an array.
        if let Some(rreg) = proto.rest_reg {
            let extra: Vec<Value> = args.get(callee_params..).unwrap_or(&[]).to_vec();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
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
                            // explicit thrown value: synthesise a real Error
                            // object so `catch (e)` sees `e.name`/`e.message` and
                            // `e instanceof TypeError`, matching JS.
                            let v = self.alloc_error_from_message(&t.0);
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
    /// a `try` handler. A `Catch` deposits `tv` in its register and resumes at the
    /// catch target. A `Finally` deposits a throw completion (kind 2 + the reason)
    /// into its registers and resumes at the finally target — `EndFinally`
    /// re-throws after the finally runs. Either way execution resumes (`true`). If
    /// the boundary is reached with no handler, return `false` (propagate).
    fn unwind_to_handler(&mut self, tv: Value, stop_depth: usize) -> bool {
        while self.frames.len() > stop_depth {
            let top = self.frames.len() - 1;
            if let Some(h) = self.frames[top].handlers.pop() {
                let base = self.frames[top].base;
                match h {
                    Handler::Catch { target, reg } => {
                        self.regs[base + reg as usize] = tv;
                        self.frames[top].ip = target as usize;
                    }
                    Handler::Finally { target, kind_reg, val_reg } => {
                        self.regs[base + kind_reg as usize] = Value::int(2); // throw
                        self.regs[base + val_reg as usize] = tv;
                        self.frames[top].ip = target as usize;
                    }
                }
                return true;
            }
            // No handler in this frame: discard it and its register window.
            let f = self.frames.pop().unwrap();
            self.regs.truncate(f.base);
        }
        false
    }

    /// On a non-throw leave of the top frame (`return`, and later break/continue),
    /// run any pending `finally` first. Discards `Catch` handlers we are exiting;
    /// on the innermost `Finally`, deposits the completion (`kind` 1=return + the
    /// `value`) into its registers and returns its target so the caller resumes
    /// there (`EndFinally` later re-leaves). Returns `None` when no finally is
    /// pending — the caller performs the real leave (pop the frame).
    fn route_through_finally(&mut self, kind: i32, value: Value) -> Option<u32> {
        let top = self.frames.len() - 1;
        let base = self.frames[top].base;
        while let Some(h) = self.frames[top].handlers.last().copied() {
            match h {
                Handler::Finally { target, kind_reg, val_reg } => {
                    self.frames[top].handlers.pop();
                    self.regs[base + kind_reg as usize] = Value::int(kind);
                    self.regs[base + val_reg as usize] = value;
                    return Some(target);
                }
                Handler::Catch { .. } => {
                    self.frames[top].handlers.pop();
                }
            }
        }
        None
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
            if ip == 0
                && self.jit_enabled
                && self.jit_recurse_depth == 0
                && !self.program.functions[func_id as usize].is_generator
                && !self.program.functions[func_id as usize].is_async
            {
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
                        jit_self_call_at as usize,
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
                        let resolved = self.resolve_const(func_id, v);
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
                    // Identical to `Add` — a JIT routing hint only (see bytecode).
                    Instr::StrConcat { dst, a, b } => {
                        let r = self.add(base, a, b)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // In-place string append (emitter proved `a` uniquely owned).
                    Instr::StrAppendInPlace { dst, a, b } => {
                        let av = self.get(base, a);
                        let bv = self.get(base, b);
                        let r = self.str_append_inplace(av, bv);
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
                    Instr::ToNum { dst, a } => {
                        let va = self.get(base, a);
                        // `+x`: numbers pass through (keep Int tag); else ToNumber.
                        let r = if va.is_number() { va } else { Value::num(self.to_number(va)?) };
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
                    Instr::Bitwise { dst, a, b, op } => {
                        use crate::bytecode::BitwiseOp as B;
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let x = to_int32(self.to_number(va)?);
                        // Shift counts use the low 5 bits per the JS spec.
                        let r = match op {
                            B::And => Value::int(x & to_int32(self.to_number(vb)?)),
                            B::Or => Value::int(x | to_int32(self.to_number(vb)?)),
                            B::Xor => Value::int(x ^ to_int32(self.to_number(vb)?)),
                            B::Shl => {
                                let s = to_uint32(self.to_number(vb)?) & 31;
                                Value::int(x.wrapping_shl(s))
                            }
                            B::Shr => {
                                let s = to_uint32(self.to_number(vb)?) & 31;
                                Value::int(x >> s)
                            }
                            B::Ushr => {
                                let s = to_uint32(self.to_number(vb)?) & 31;
                                let u = to_uint32(self.to_number(va)?) >> s;
                                // u32 may exceed i32::MAX → keep numeric range.
                                if u <= i32::MAX as u32 {
                                    Value::int(u as i32)
                                } else {
                                    Value::num(u as f64)
                                }
                            }
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Pow { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = self.to_number(va)?.powf(self.to_number(vb)?);
                        self.set(base, dst, Value::num(r));
                        ip += 1;
                    }
                    Instr::BitNot { dst, a } => {
                        let va = self.get(base, a);
                        let r = !to_int32(self.to_number(va)?);
                        self.set(base, dst, Value::int(r));
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
                    Instr::JsonParse { dst, a } => {
                        let s = self.display(self.get(base, a)); // ToString of the arg
                        let v = self.json_parse(&s)?; // propagates SyntaxError as a throw
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ArrayAppend { arr, val, spread } => {
                        let aidx = self.get(base, arr).heap_index();
                        let vv = self.get(base, val);
                        if spread {
                            // A generator or a custom iterable (object) is drained
                            // via the iterator protocol (iterate_to_vec also errors
                            // for a plain, non-iterable object, as a spread should).
                            if vv.is_heap()
                                && matches!(
                                    self.heap.get(vv.heap_index()),
                                    HeapObj::Generator { .. } | HeapObj::Object(_)
                                )
                            {
                                let elems = self.iterate_to_vec(vv)?;
                                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                                    dst_items.extend(elems);
                                }
                                ip += 1;
                                continue;
                            }
                            // Materialize the spread source's elements (array/set →
                            // elements; string → chars; map → [k,v] entries) WITHOUT
                            // holding a heap borrow across the fresh allocations.
                            let mut chars: Option<Vec<char>> = None;
                            let mut map_pairs: Option<Vec<(Value, Value)>> = None;
                            if vv.is_heap() {
                                match self.heap.get(vv.heap_index()) {
                                    HeapObj::Array(items) => {
                                        let elems = items.clone();
                                        if let HeapObj::Array(d) = self.heap.get_mut(aidx) {
                                            d.extend(elems);
                                        }
                                    }
                                    HeapObj::Set(items) => {
                                        let elems = items.clone();
                                        if let HeapObj::Array(d) = self.heap.get_mut(aidx) {
                                            d.extend(elems);
                                        }
                                    }
                                    HeapObj::Str(_) | HeapObj::Cons { .. } => {
                                        chars = Some(self.heap.str_cow(vv.heap_index()).unwrap().chars().collect());
                                    }
                                    HeapObj::Map { keys, vals } => {
                                        map_pairs = Some(keys.iter().copied().zip(vals.iter().copied()).collect());
                                    }
                                    _ => return Err(Thrown("TypeError: spread value is not iterable".into())),
                                }
                            } else {
                                return Err(Thrown("TypeError: spread value is not iterable".into()));
                            }
                            if let Some(chars) = chars {
                                let elems: Vec<Value> =
                                    chars.into_iter().map(|c| self.alloc_str(c.to_string())).collect();
                                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                                    dst_items.extend(elems);
                                }
                            }
                            if let Some(pairs) = map_pairs {
                                let elems: Vec<Value> = pairs
                                    .into_iter()
                                    .map(|(k, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))))
                                    .collect();
                                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                                    dst_items.extend(elems);
                                }
                            }
                        } else if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                            dst_items.push(vv);
                        }
                        ip += 1;
                    }
                    Instr::ArrayRest { dst, src, start } => {
                        let sv = self.get(base, src);
                        let mut elems = self.iterate_to_vec(sv)?;
                        let start = (start as usize).min(elems.len());
                        let rest = elems.split_off(start);
                        let arr = Value::heap(self.heap.alloc(HeapObj::Array(rest)));
                        self.set(base, dst, arr);
                        ip += 1;
                    }
                    Instr::ObjectSpread { target, src } => {
                        let t = self.get(base, target);
                        let s = self.get(base, src);
                        self.object_assign(&[t, s])?; // mutates target in place
                        ip += 1;
                    }
                    Instr::ObjectRest { dst, src, exclude_start, exclude_count } => {
                        let s = self.get(base, src);
                        let prog: &'p Program = self.program;
                        let consts = &prog.functions[func_id as usize].string_constants;
                        let excluded =
                            &consts[exclude_start as usize..exclude_start as usize + exclude_count as usize];
                        // Copy src's own keys except the destructured siblings.
                        let pairs: Vec<(String, Value)> = if s.is_heap() {
                            match self.heap.get(s.heap_index()) {
                                HeapObj::Object(map) => map
                                    .keys
                                    .iter()
                                    .cloned()
                                    .zip(map.vals.iter().copied())
                                    .filter(|(k, _)| !excluded.iter().any(|e| e == k))
                                    .collect(),
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let mut m = ObjMap::new();
                        for (k, v) in pairs {
                            m.set(&k, v);
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeClass { dst, class_id, parent } => {
                        let cd = self.program.classes[class_id as usize].clone();
                        let parent_idx = parent.and_then(|p| {
                            let pv = self.get(base, p);
                            pv.is_heap().then(|| pv.heap_index())
                        });
                        // Materialize each method as a Func value once; instances
                        // share these (no per-access alloc, no per-instance copy).
                        let mk = |heap: &mut Heap, defs: &[(String, u32)]| -> Vec<(String, Value)> {
                            defs.iter()
                                .map(|(n, fid)| {
                                    (n.clone(), Value::heap(heap.alloc(HeapObj::Func(*fid))))
                                })
                                .collect()
                        };
                        let methods = mk(&mut self.heap, &cd.methods);
                        let getters = mk(&mut self.heap, &cd.getters);
                        let setters = mk(&mut self.heap, &cd.setters);
                        let static_getters = mk(&mut self.heap, &cd.static_getters);
                        let static_setters = mk(&mut self.heap, &cd.static_setters);
                        let mut statics = ObjMap::new();
                        for (n, fid) in &cd.statics {
                            let fv = Value::heap(self.heap.alloc(HeapObj::Func(*fid)));
                            statics.set(n, fv);
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Class(Box::new(ClassData {
                            name: cd.name,
                            ctor: cd.ctor,
                            has_explicit_ctor: cd.has_explicit_ctor,
                            methods,
                            getters,
                            setters,
                            statics,
                            static_getters,
                            static_setters,
                            parent: parent_idx,
                        }))));
                        // Remember it so `super` in a derived class can reach it.
                        self.class_values[class_id as usize] = Some(v);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ClassAddMember { class, key, func, kind } => {
                        let cv = self.get(base, class);
                        let k = self.get(base, key);
                        let kstr = self.display(k);
                        let fv = Value::heap(self.heap.alloc(HeapObj::Func(func)));
                        if let HeapObj::Class(c) = self.heap.get_mut(cv.heap_index()) {
                            if kind == 3 {
                                c.statics.set(&kstr, fv); // static method
                            } else {
                                let list = match kind {
                                    1 => &mut c.getters,
                                    2 => &mut c.setters,
                                    _ => &mut c.methods,
                                };
                                // Replace a same-key member, else append.
                                if let Some(slot) = list.iter_mut().find(|(n, _)| *n == kstr) {
                                    slot.1 = fv;
                                } else {
                                    list.push((kstr, fv));
                                }
                            }
                        }
                        ip += 1;
                    }
                    Instr::New { dst, callee, arg_base, argc } => {
                        let cv = self.get(base, callee);
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let result = self.construct(cv, &args)?;
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::SuperCtor { parent_class_id, arg_base, argc } => {
                        let parent = self.class_values[parent_class_id as usize]
                            .ok_or_else(|| Thrown("TypeError: superclass is not a constructor".into()))?;
                        let this = self.get(base, 0);
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        self.run_class_ctor(parent, this, &args)?;
                        ip += 1;
                    }
                    Instr::SuperMethod { dst, parent_class_id, name, arg_base, argc } => {
                        let prog: &'p Program = self.program;
                        let key: &'p str =
                            &prog.functions[func_id as usize].string_constants[name as usize];
                        let parent = self.class_values[parent_class_id as usize]
                            .ok_or_else(|| Thrown("TypeError: bad super reference".into()))?;
                        // Find the method up the parent's class chain.
                        let mut method = None;
                        let mut cur = parent.is_heap().then(|| parent.heap_index());
                        while let Some(cidx) = cur {
                            match self.heap.get(cidx) {
                                HeapObj::Class(c) => {
                                    if let Some((_, v)) = c.methods.iter().find(|(k, _)| k == key) {
                                        method = Some(*v);
                                        break;
                                    }
                                    cur = c.parent;
                                }
                                _ => break,
                            }
                        }
                        let m = method.ok_or_else(|| {
                            Thrown(format!("TypeError: super.{key} is not a function"))
                        })?;
                        let this = self.get(base, 0);
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let r = self.call_value(m, this, &args)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::ArrayCtor { dst, arg_base, argc } => {
                        let arr = if argc == 1 && self.get(base, arg_base).is_number() {
                            // `Array(n)` → n empty slots (undefined).
                            let n = self.get(base, arg_base).as_f64();
                            if n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                                return Err(Thrown("RangeError: Invalid array length".into()));
                            }
                            vec![Value::UNDEFINED; n as usize]
                        } else {
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect()
                        };
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(arr)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewMap { dst, src } => {
                        let (mut keys, mut vals): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                // Each iterated entry is a [key, value]-indexable.
                                for e in self.iterate_to_vec(sv)? {
                                    let k = normalize_zero(self.get_index(e, Value::int(0))?);
                                    let v = self.get_index(e, Value::int(1))?;
                                    match keys.iter().position(|kk| self.same_value_zero(*kk, k)) {
                                        Some(i) => vals[i] = v,
                                        None => {
                                            keys.push(k);
                                            vals.push(v);
                                        }
                                    }
                                }
                            }
                        }
                        let m = Value::heap(self.heap.alloc(HeapObj::Map { keys, vals }));
                        self.set(base, dst, m);
                        ip += 1;
                    }
                    Instr::NewSet { dst, src } => {
                        let mut items: Vec<Value> = Vec::new();
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                for e in self.iterate_to_vec(sv)? {
                                    let v = normalize_zero(e);
                                    if !items.iter().any(|x| self.same_value_zero(*x, v)) {
                                        items.push(v);
                                    }
                                }
                            }
                        }
                        let s = Value::heap(self.heap.alloc(HeapObj::Set(items)));
                        self.set(base, dst, s);
                        ip += 1;
                    }
                    Instr::NewPromise { dst, executor } => {
                        let exec = self.get(base, executor);
                        let p = self.alloc_promise();
                        let res = Value::heap(
                            self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: false }),
                        );
                        let rej = Value::heap(
                            self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: true }),
                        );
                        // A throwing executor rejects the promise.
                        if self.call_value(exec, Value::UNDEFINED, &[res, rej]).is_err() {
                            let reason = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                            self.reject(p, reason);
                        }
                        self.set(base, dst, Value::heap(p));
                        ip += 1;
                    }
                    Instr::CallSpread { dst, callee, args } => {
                        let callee_v = self.get(base, callee);
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        let result = self.call_value(callee_v, Value::UNDEFINED, &arg_vec)?;
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::CallMethodSpread { dst, obj, name, args } => {
                        let recv = self.get(base, obj);
                        let prog: &'p Program = self.program;
                        let key: &'p str =
                            &prog.functions[func_id as usize].string_constants[name as usize];
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        // Builtin (array/string/number) method, else a user method
                        // resolved off the receiver and called with `this = recv`.
                        let result = match self.dispatch_builtin_method(recv, key, &arg_vec)? {
                            Some(r) => r,
                            None => {
                                let prop = self.get_prop(recv, key)?;
                                self.call_value(prop, recv, &arg_vec)?
                            }
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
                            G::String => {
                                if argc == 0 {
                                    self.alloc_str(String::new())
                                } else {
                                    let s = self.display(a0);
                                    self.alloc_str(s)
                                }
                            }
                            G::Boolean => Value::bool(argc >= 1 && self.truthy(a0)),
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
                            // isNaN/isFinite coerce and never throw for the values
                            // in this subset; treat any coercion failure as NaN.
                            G::IsNaN => {
                                Value::bool(self.to_number(a0).unwrap_or(f64::NAN).is_nan())
                            }
                            G::IsFinite => {
                                Value::bool(self.to_number(a0).unwrap_or(f64::NAN).is_finite())
                            }
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::InstanceOf { dst, val, ctor } => {
                        let v = self.get(base, val);
                        let r = self.eval_instanceof(v, ctor);
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::HasProp { dst, key, obj } => {
                        let k = self.get(base, key);
                        let o = self.get(base, obj);
                        let r = self.has_property(o, k);
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::InstanceOfDyn { dst, val, ctor } => {
                        let v = self.get(base, val);
                        let c = self.get(base, ctor);
                        // A class uses its `extends` chain; a constructor FUNCTION
                        // checks whether `F.prototype` is in `v`'s prototype chain.
                        let kind = if c.is_heap() {
                            match self.heap.get(c.heap_index()) {
                                HeapObj::Class(_) => 1u8,
                                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } => 2,
                                _ => 0,
                            }
                        } else {
                            0
                        };
                        let r = match kind {
                            1 => v.is_heap() && self.instance_of_class(v, c.heap_index()),
                            2 => self.instanceof_via_proto(v, c),
                            _ => false,
                        };
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::StaticFn { dst, op, arg_base, argc } => {
                        use crate::bytecode::StaticFn as S;
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                        let v = match op {
                            S::ArrayOf => Value::heap(self.heap.alloc(HeapObj::Array(args))),
                            S::NumberIsInteger => Value::bool(num_is_integer(a0)),
                            S::NumberIsNaN => Value::bool(a0.is_double() && a0.as_f64().is_nan()),
                            S::NumberIsFinite => Value::bool(num_is_finite(a0)),
                            S::NumberIsSafeInteger => Value::bool(num_is_safe_integer(a0)),
                            S::StringFromCharCode => {
                                let s: String = args
                                    .iter()
                                    .map(|&v| {
                                        // ToUint16 of each code unit.
                                        let u = to_uint32(self.to_number(v).unwrap_or(0.0)) as u16;
                                        char::from_u32(u as u32).unwrap_or('\u{FFFD}')
                                    })
                                    .collect();
                                self.alloc_str(s)
                            }
                            S::ObjectAssign => self.object_assign(&args)?,
                            S::ObjectFromEntries => {
                                let entries = self.iterate_to_vec(a0)?;
                                let mut map = ObjMap::new();
                                for e in entries {
                                    let kv = self.get_index(e, Value::int(0))?;
                                    let k = self.display(kv);
                                    let v = self.get_index(e, Value::int(1))?;
                                    map.set(&k, v);
                                }
                                Value::heap(self.heap.alloc(HeapObj::Object(map)))
                            }
                            S::PromiseResolve => {
                                // Promise.resolve(p) of an existing Promise is identity.
                                if a0.is_heap()
                                    && matches!(self.heap.get(a0.heap_index()), HeapObj::Promise { .. })
                                {
                                    a0
                                } else {
                                    let p = self.alloc_promise();
                                    self.resolve(p, a0);
                                    Value::heap(p)
                                }
                            }
                            S::PromiseReject => {
                                let p = self.alloc_promise();
                                self.reject(p, a0);
                                Value::heap(p)
                            }
                            S::PromiseAll => self.promise_combine(crate::heap::CombKind::All, a0)?,
                            S::PromiseAllSettled => {
                                self.promise_combine(crate::heap::CombKind::AllSettled, a0)?
                            }
                            S::PromiseRace => self.promise_combine(crate::heap::CombKind::Race, a0)?,
                            S::PromiseAny => self.promise_combine(crate::heap::CombKind::Any, a0)?,
                            S::ObjectDefineProperty => {
                                let key = self.display(args.get(1).copied().unwrap_or(Value::UNDEFINED));
                                let desc = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                                self.object_define_property(a0, &key, desc)?;
                                a0
                            }
                            S::ObjectDefineProperties => {
                                let props = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                                self.object_define_properties(a0, props)?;
                                a0
                            }
                            S::ObjectGetOwnPropertyDescriptor => {
                                let key = self.display(args.get(1).copied().unwrap_or(Value::UNDEFINED));
                                self.object_get_own_property_descriptor(a0, &key)
                            }
                            S::ObjectGetOwnPropertyNames => self.object_own_property_names(a0),
                            S::ObjectGetPrototypeOf => self.object_get_prototype_of(a0),
                            S::ObjectCreate => {
                                let o = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                                if a0 != Value::UNDEFINED {
                                    self.proto_of.insert(o.heap_index(), a0);
                                }
                                if let Some(props) = args.get(1).copied() {
                                    if props != Value::UNDEFINED {
                                        self.object_define_properties(o, props)?;
                                    }
                                }
                                o
                            }
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ArrayFrom { dst, src, mapfn } => {
                        let sv = self.get(base, src);
                        let fnv = self.get(base, mapfn);
                        let out = self.array_from(sv, fnv)?;
                        self.set(base, dst, out);
                        ip += 1;
                    }
                    Instr::MathSpread { dst, op, args } => {
                        use crate::bytecode::MathFn as M;
                        let av = self.get(base, args);
                        let elems = self.array_snapshot(av.heap_index());
                        let nums: Vec<f64> =
                            elems.iter().map(|&v| self.to_number(v)).collect::<Result<_, _>>()?;
                        let r = match op {
                            M::Max => nums.iter().fold(f64::NEG_INFINITY, |a, &b| {
                                if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
                            }),
                            M::Min => nums.iter().fold(f64::INFINITY, |a, &b| {
                                if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
                            }),
                            M::Hypot => nums.iter().map(|&v| v * v).sum::<f64>().sqrt(),
                            // A non-variadic Math fn spread is unusual; apply to elem 0.
                            _ => self.eval_math_one(op, nums.first().copied().unwrap_or(f64::NAN)),
                        };
                        self.set(base, dst, Value::num(r));
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
                                        array_push: jit_array_push as usize,
                                        char_code_at: jit_char_code_at as usize,
                                        concat: jit_concat as usize,
                                        str_append: jit_str_append as usize,
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
                                // Only OWN ENUMERABLE keys (skip non-enumerable).
                                HeapObj::Object(map) => map
                                    .keys
                                    .iter()
                                    .zip(map.attrs.iter())
                                    .filter(|(_, a)| a.enumerable)
                                    .map(|(k, _)| k.clone())
                                    .collect(),
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
                                HeapObj::Object(map) => map
                                    .vals
                                    .iter()
                                    .zip(map.attrs.iter())
                                    .filter(|(_, a)| a.enumerable)
                                    .map(|(v, _)| *v)
                                    .collect(),
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
                                HeapObj::Object(map) => map
                                    .keys
                                    .iter()
                                    .cloned()
                                    .zip(map.vals.iter().copied())
                                    .zip(map.attrs.iter())
                                    .filter(|(_, a)| a.enumerable)
                                    .map(|(kv, _)| kv)
                                    .collect(),
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
                                // for-of over a Map/Set iterates `size` slots.
                                HeapObj::Map { keys, .. } => len_value(keys.len()),
                                HeapObj::Set(items) => len_value(items.len()),
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
                    Instr::DeleteProp { dst, obj, name } => {
                        let o = self.get(base, obj);
                        let key = self.program.functions[func_id as usize]
                            .string_constants[name as usize]
                            .clone();
                        let r = self.delete_prop(o, &key);
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::DeleteIndex { dst, obj, key } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let ks = self.display(k); // ToPropertyKey (string form)
                        let r = self.delete_prop(o, &ks);
                        self.set(base, dst, r);
                        ip += 1;
                    }

                    Instr::Call { dst, callee, arg_base, argc } => {
                        let callee_v = self.get(base, callee);
                        // A native resolve/reject function (from `new Promise`).
                        if callee_v.is_heap() {
                            if let HeapObj::BoundResolver { promise, is_reject } =
                                self.heap.get(callee_v.heap_index())
                            {
                                let (p, isr) = (*promise, *is_reject);
                                let arg = if argc >= 1 {
                                    self.get(base, arg_base)
                                } else {
                                    Value::UNDEFINED
                                };
                                if isr {
                                    self.reject(p, arg);
                                } else {
                                    self.resolve(p, arg);
                                }
                                self.set(base, dst, Value::UNDEFINED);
                                ip += 1;
                                continue;
                            }
                            // A bound or native function: run via call_value (fixes
                            // `this`/prepends bound args, or dispatches the builtin).
                            if matches!(
                                self.heap.get(callee_v.heap_index()),
                                HeapObj::Bound { .. } | HeapObj::Native(_)
                            ) {
                                let argv: Vec<Value> =
                                    (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                                let r = self.call_value(callee_v, Value::UNDEFINED, &argv)?;
                                self.set(base, dst, r);
                                ip += 1;
                                continue;
                            }
                        }
                        let (fid, closure) = self.resolve_callable(callee_v)?;
                        // A generator function returns a Generator object, unrun.
                        if self.program.functions[fid as usize].is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, Value::UNDEFINED, &argv);
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        // An async function runs to its first `await` then returns
                        // its result Promise.
                        if self.program.functions[fid as usize].is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, Value::UNDEFINED, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
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
                        // Hot fast path: `arr.push(x)` — the most common
                        // per-element array idiom. Append directly, skipping the
                        // try_builtin_method → dispatch_builtin_method → array_method
                        // layering (and the args-gather), then return the new length.
                        if argc == 1 && key == "push" && recv.is_heap() {
                            let v = self.get(base, arg_base);
                            let len = if let HeapObj::Array(items) =
                                self.heap.get_mut(recv.heap_index())
                            {
                                items.push(v);
                                Some(items.len() as i32)
                            } else {
                                None
                            };
                            if let Some(len) = len {
                                self.set(base, dst, Value::int(len));
                                ip += 1;
                                continue;
                            }
                        }
                        // Builtin methods (array/string) execute inline and
                        // produce a result without pushing a frame.
                        if let Some(result) = self.try_builtin_method(recv, key, base, arg_base, argc)? {
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        // Otherwise the property must resolve to a function; call it
                        // with `this = recv`.
                        let prop = self.get_prop(recv, key)?;
                        // A native or bound method value (e.g. inherited from a
                        // prototype) is invoked via call_value with this = recv.
                        if prop.is_heap()
                            && matches!(
                                self.heap.get(prop.heap_index()),
                                HeapObj::Native(_) | HeapObj::Bound { .. }
                            )
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(prop, recv, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        let (fid, closure) = self.resolve_callable(prop)?;
                        // A generator method returns a Generator object, unrun.
                        if self.program.functions[fid as usize].is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, recv, &argv);
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        // An async method runs to its first `await` then returns
                        // its result Promise.
                        if self.program.functions[fid as usize].is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, recv, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
                        self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1)?;
                        break;
                    }

                    Instr::CallMethodComputed { dst, obj, key, arg_base, argc } => {
                        let recv = self.get(base, obj);
                        let k = self.get(base, key);
                        // `obj["push"](x)` etc: a builtin array/string method first.
                        let kstr = self.display(k);
                        if let Some(result) =
                            self.try_builtin_method(recv, &kstr, base, arg_base, argc)?
                        {
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        // Else resolve the method off the receiver (own/inherited)
                        // and call it with `this = recv`.
                        let method = self.get_index(recv, k)?;
                        let (fid, closure) = self.resolve_callable(method)?;
                        if self.program.functions[fid as usize].is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, recv, &argv);
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        if self.program.functions[fid as usize].is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, recv, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
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
                        self.frames[top]
                            .handlers
                            .push(Handler::Catch { target: catch_target, reg: catch_reg });
                        ip += 1;
                    }
                    Instr::PopHandler => {
                        let top = self.frames.len() - 1;
                        self.frames[top].handlers.pop();
                        ip += 1;
                    }
                    Instr::PushFinally { target, kind_reg, val_reg } => {
                        let top = self.frames.len() - 1;
                        self.frames[top]
                            .handlers
                            .push(Handler::Finally { target, kind_reg, val_reg });
                        ip += 1;
                    }
                    Instr::PopFinally => {
                        let top = self.frames.len() - 1;
                        self.frames[top].handlers.pop();
                        ip += 1;
                    }
                    Instr::EndFinally { kind_reg, val_reg } => {
                        // Resume the completion deposited when this finally was
                        // entered: 1 = return (re-leave through any outer finally,
                        // else return), 2 = throw (re-raise), else 0 = normal.
                        match self.regs[base + kind_reg as usize].as_int() {
                            1 => {
                                let v = self.regs[base + val_reg as usize];
                                if let Some(target) = self.route_through_finally(1, v) {
                                    ip = target as usize;
                                    continue;
                                }
                                if self.pop_frame_with(v, stop_depth) {
                                    return Ok(v);
                                }
                                break;
                            }
                            2 => {
                                let v = self.regs[base + val_reg as usize];
                                let top = self.frames.len() - 1;
                                self.frames[top].ip = ip;
                                self.pending_throw = Some(v);
                                return Err(Thrown(self.throw_message(v)));
                            }
                            _ => {
                                ip += 1;
                            }
                        }
                    }
                    Instr::SetRaw { arr, raw } => {
                        let a = self.get(base, arr);
                        let r = self.get(base, raw);
                        if a.is_heap() {
                            self.template_raws.insert(a.heap_index(), r);
                        }
                        ip += 1;
                    }
                    Instr::GetIterator { dst, src } => {
                        let s = self.get(base, src);
                        let it = self.get_iterator(s)?;
                        self.set(base, dst, it);
                        ip += 1;
                    }
                    Instr::IterToArray { dst, src, count } => {
                        let s = self.get(base, src);
                        let a = self.iter_to_array(s, count)?;
                        self.set(base, dst, a);
                        ip += 1;
                    }
                    Instr::Random { dst } => {
                        // xorshift64* → a uniform double in [0, 1) (top 53 bits).
                        let mut x = self.rng_state;
                        x ^= x >> 12;
                        x ^= x << 25;
                        x ^= x >> 27;
                        self.rng_state = x;
                        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
                        let f = (r >> 11) as f64 / (1u64 << 53) as f64;
                        self.set(base, dst, Value::num(f));
                        ip += 1;
                    }
                    Instr::DateNew { dst, arg_base, argc } => {
                        let args: Vec<Value> =
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                        let ms = self.date_new_ms(&args)?;
                        let v = Value::heap(self.heap.alloc(HeapObj::Date(ms)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::DateUTC { dst, arg_base, argc } => {
                        let args: Vec<Value> =
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                        let ms = self.date_utc_ms(&args)?;
                        self.set(base, dst, Value::num(ms));
                        ip += 1;
                    }
                    Instr::DateParse { dst, src } => {
                        let s = self.get(base, src);
                        let str = self.display(s);
                        self.set(base, dst, Value::num(parse_date(&str)));
                        ip += 1;
                    }
                    Instr::Return { src } => {
                        let v = self.regs[base + src as usize];
                        // Run any pending `finally` in this frame first.
                        if let Some(target) = self.route_through_finally(1, v) {
                            ip = target as usize;
                            continue;
                        }
                        if self.pop_frame_with(v, stop_depth) {
                            return Ok(v);
                        }
                        break;
                    }
                    Instr::ReturnUndefined => {
                        if let Some(target) = self.route_through_finally(1, Value::UNDEFINED) {
                            ip = target as usize;
                            continue;
                        }
                        if self.pop_frame_with(Value::UNDEFINED, stop_depth) {
                            return Ok(Value::UNDEFINED);
                        }
                        break;
                    }
                    Instr::Yield { val, .. } => {
                        // Suspend the generator: pop the frame ENTRY but leave its
                        // register window live at the top of `self.regs` so the
                        // resumer (generator_method) can copy it back into the heap
                        // Generator. The generator frame is always the top (and the
                        // run_loop's stop frame) at a yield, so popping returns to
                        // the resumer. `pending_yield` carries the value + this ip.
                        let v = self.get(base, val);
                        self.frames.pop();
                        self.pending_yield = Some((v, ip));
                        return Ok(v);
                    }
                    Instr::Await { val, .. } => {
                        // Suspend the async activation: pop the frame ENTRY but
                        // leave its register window live at the top of `self.regs`
                        // for `drive_async` to park into the heap AsyncState. Unlike
                        // a generator yield, we CAPTURE the frame's `try` handlers
                        // (carried in `pending_await`) so they can be restored on
                        // resume — letting `try { await p } catch (e)` see a
                        // rejection thrown back in at the await point. The async
                        // frame is always the top (and the run_loop stop frame) at
                        // an await, so popping returns to `drive_async`.
                        let v = self.get(base, val);
                        let f = self.frames.pop().unwrap();
                        self.pending_await = Some((v, ip, f.handlers));
                        return Ok(v);
                    }
                    Instr::IterNext { value_dst, done_dst, iter, idx } => {
                        let it = self.get(base, iter);
                        if !it.is_heap() {
                            return Err(Thrown(format!(
                                "TypeError: {} is not iterable",
                                self.display(it)
                            )));
                        }
                        // A generator is driven by `.next()`; the cursor is unused.
                        if matches!(self.heap.get(it.heap_index()), HeapObj::Generator { .. }) {
                            let res = self
                                .generator_method(it.heap_index(), "next", &[])?
                                .unwrap_or(Value::UNDEFINED);
                            let done = self.get_prop(res, "done")?;
                            let val = self.get_prop(res, "value")?;
                            self.set(base, value_dst, val);
                            self.set(base, done_dst, done);
                            ip += 1;
                            continue;
                        }
                        // A user iterator object (`@@iterator` already resolved by
                        // GetIterator): pull the next result via `.next()`. Lazy —
                        // a `break` simply stops calling it.
                        if matches!(self.heap.get(it.heap_index()), HeapObj::Object(_)) {
                            let next = self.get_prop(it, "next")?;
                            if self.is_callable(next) {
                                let res = self.call_value(next, it, &[])?;
                                let done = self.get_prop(res, "done")?;
                                let val = self.get_prop(res, "value")?;
                                self.set(base, value_dst, val);
                                self.set(base, done_dst, done);
                                ip += 1;
                                continue;
                            }
                        }
                        // Array/Set element, string char, or Map [k,v] at the cursor.
                        let cursor = array_index(self.get(base, idx)).unwrap_or(0);
                        let len = match self.heap.get(it.heap_index()) {
                            HeapObj::Array(items) => items.len(),
                            HeapObj::Set(items) => items.len(),
                            HeapObj::Str(s) => s.char_len,
                            HeapObj::Cons { len, .. } => *len,
                            HeapObj::Map { keys, .. } => keys.len(),
                            _ => {
                                return Err(Thrown(format!(
                                    "TypeError: {} is not iterable",
                                    self.display(it)
                                )))
                            }
                        };
                        if cursor < len {
                            let val = self.get_index(it, Value::int(cursor as i32))?;
                            self.set(base, value_dst, val);
                            self.set(base, done_dst, Value::bool(false));
                            self.set(base, idx, Value::int((cursor + 1) as i32));
                        } else {
                            self.set(base, done_dst, Value::bool(true));
                        }
                        ip += 1;
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
        // Rest parameter: collect args beyond the fixed params into a fresh array.
        if let Some(rreg) = self.program.functions[func_id as usize].rest_reg {
            let extra: Vec<Value> = ((arg_base as usize + callee_params)
                ..(arg_base as usize + argc as usize))
                .map(|i| self.regs[caller_base + i])
                .collect();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
        }
        // `arguments`: an array of ALL actual args (a function that references it).
        if let Some(areg) = self.program.functions[func_id as usize].arguments_reg {
            let argsv: Vec<Value> = (0..argc as usize)
                .map(|i| self.regs[caller_base + arg_base as usize + i])
                .collect();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(argsv)));
            self.regs[new_base + areg as usize] = arr;
        }

        let last = self.frames.len() - 1;
        self.frames[last].ip = caller_ip_next;
        self.frames.push(Frame { func: func_id, base: new_base, ip: 0, ret_dst: dst, closure, handlers: Vec::new() });
        Ok(())
    }

    /// Calling a `function*` does NOT run its body — it allocates a suspended
    /// Generator whose DETACHED register window holds `this` + the bound args
    /// (incl. a rest array). Resumed later by `generator_method`.
    fn alloc_generator(&mut self, func_id: u32, closure: u32, this: Value, args: &[Value]) -> Value {
        let proto = &self.program.functions[func_id as usize];
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        Value::heap(self.heap.alloc(HeapObj::Generator {
            func: func_id,
            closure,
            state: GenState::Suspended(0),
            regs,
        }))
    }

    /// Resume / query a generator (`gen.next(v)` / `gen.return(v)` / `gen.throw(e)`).
    /// Returns an iterator-result object `{value, done}` (or propagates a throw).
    fn generator_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let (state, fid, closure) = match self.heap.get(idx) {
            HeapObj::Generator { state, func, closure, .. } => (*state, *func, *closure),
            _ => return Ok(None),
        };
        match name {
            "return" => {
                // Complete the generator (v1 does not run finally blocks).
                if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                    *state = GenState::Completed;
                    regs.clear();
                }
                Ok(Some(self.iter_result(arg0, true)))
            }
            "throw" => {
                if matches!(state, GenState::Completed) {
                    return Err(Thrown(self.throw_message(arg0)));
                }
                // v1: complete the generator and surface the throw at the call
                // site (no resume into a `try` inside the body).
                if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                    *state = GenState::Completed;
                    regs.clear();
                }
                self.pending_throw = Some(arg0);
                Err(Thrown(self.throw_message(arg0)))
            }
            "next" => {
                let resume_ip = match state {
                    GenState::Completed => return Ok(Some(self.iter_result(Value::UNDEFINED, true))),
                    GenState::Running => {
                        return Err(Thrown("TypeError: generator is already running".into()))
                    }
                    GenState::Suspended(ip) => ip,
                };
                // Take the saved window out of the heap object and splice it onto
                // the top of the live register file.
                let saved = match self.heap.get_mut(idx) {
                    HeapObj::Generator { state, regs, .. } => {
                        *state = GenState::Running;
                        std::mem::take(regs)
                    }
                    _ => return Ok(None),
                };
                let reg_count = saved.len();
                let new_base = self.regs.len();
                if self.regs_would_overflow(new_base + reg_count) {
                    if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                        *state = GenState::Suspended(resume_ip);
                        *regs = saved;
                    }
                    return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
                }
                self.regs.extend_from_slice(&saved);
                if new_base + reg_count > self.regs_hw {
                    self.regs_hw = new_base + reg_count;
                }
                // First next() runs from ip 0; a later one resumes after the Yield,
                // delivering the sent value into the yield expression's dst.
                let ip = if resume_ip == 0 {
                    0
                } else {
                    if let Instr::Yield { dst, .. } =
                        self.program.functions[fid as usize].code[resume_ip]
                    {
                        self.regs[new_base + dst as usize] = arg0;
                    }
                    resume_ip + 1
                };
                let stop = self.frames.len();
                self.frames.push(Frame {
                    func: fid,
                    base: new_base,
                    ip,
                    ret_dst: 0,
                    closure,
                    handlers: Vec::new(),
                });
                let outcome = self.run_loop(stop);
                if let Some((y, yield_ip)) = self.pending_yield.take() {
                    // Suspended: the window is still live at [new_base..]; park it.
                    let back = self.regs.split_off(new_base);
                    if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                        *state = GenState::Suspended(yield_ip);
                        *regs = back;
                    }
                    return Ok(Some(self.iter_result(y, false)));
                }
                match outcome {
                    Ok(ret) => {
                        // Returned / fell off the end (pop_frame_with already truncated).
                        if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                            *state = GenState::Completed;
                            regs.clear();
                        }
                        Ok(Some(self.iter_result(ret, true)))
                    }
                    Err(t) => {
                        self.regs.truncate(new_base);
                        if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                            *state = GenState::Completed;
                            regs.clear();
                        }
                        Err(t)
                    }
                }
            }
            _ => Ok(None),
        }
    }

    /// Build an iterator-result object `{ value, done }` (insertion order matches
    /// the spec / node).
    fn iter_result(&mut self, value: Value, done: bool) -> Value {
        let mut map = ObjMap::new();
        map.set("value", value);
        map.set("done", Value::bool(done));
        Value::heap(self.heap.alloc(HeapObj::Object(map)))
    }

    // ── promises / microtasks ──

    fn alloc_promise(&mut self) -> u32 {
        self.heap.alloc(HeapObj::Promise {
            state: PromiseState::Pending,
            result: Value::UNDEFINED,
            fulfill: Vec::new(),
            reject: Vec::new(),
            handled: false,
        })
    }

    /// Settle a pending promise (no-op if already settled — the one-shot guard
    /// covers double-resolve / resolve-then-reject / race losers), scheduling its
    /// matching reactions as microtasks.
    fn settle(&mut self, p: u32, state: PromiseState, val: Value) {
        let reactions = match self.heap.get_mut(p) {
            HeapObj::Promise { state: s, result, fulfill, reject, .. } => {
                if *s != PromiseState::Pending {
                    return;
                }
                *s = state;
                *result = val;
                match state {
                    PromiseState::Fulfilled => std::mem::take(fulfill),
                    PromiseState::Rejected => std::mem::take(reject),
                    PromiseState::Pending => return,
                }
            }
            _ => return,
        };
        let kind = if state == PromiseState::Fulfilled {
            ReactionKind::Fulfill
        } else {
            ReactionKind::Reject
        };
        for r in reactions {
            if r.is_async {
                // `dependent` is a suspended async activation; resume it with the
                // value (fulfill) or by throwing the reason in (reject).
                let input = match kind {
                    ReactionKind::Fulfill => Resume::Value(val),
                    ReactionKind::Reject => Resume::Throw(val),
                };
                self.microtasks
                    .push_back(Microtask::AsyncResume { activation: r.dependent, input });
            } else {
                self.microtasks.push_back(Microtask::Reaction {
                    callback: r.callback,
                    arg: val,
                    dependent: r.dependent,
                    kind,
                    finally: r.finally,
                });
            }
        }
    }

    /// JS `[[Resolve]]`: a thenable/Promise value is ADOPTED (p forwards when it
    /// settles); a self-resolution rejects with a TypeError; else fulfill.
    fn resolve(&mut self, p: u32, value: Value) {
        if value.is_heap() {
            if value.heap_index() == p {
                let e = self.alloc_error_from_message("TypeError: Chaining cycle detected for promise");
                self.reject(p, e);
                return;
            }
            if matches!(self.heap.get(value.heap_index()), HeapObj::Promise { .. }) {
                let inner = value.heap_index();
                self.then_internal(inner, Value::UNDEFINED, Value::UNDEFINED, Some(p));
                return;
            }
        }
        self.settle(p, PromiseState::Fulfilled, value);
    }

    fn reject(&mut self, p: u32, reason: Value) {
        self.settle(p, PromiseState::Rejected, reason);
    }

    /// Register reactions on `p` (creating/reusing the dependent promise `into`),
    /// or schedule a microtask immediately if `p` is already settled. Returns the
    /// dependent promise's heap index. The basis of `.then`/`.catch`/`.finally`
    /// and of internal promise adoption.
    fn then_internal(&mut self, p: u32, on_f: Value, on_r: Value, into: Option<u32>) -> u32 {
        let dep = into.unwrap_or_else(|| self.alloc_promise());
        let (state, result) = match self.heap.get(p) {
            HeapObj::Promise { state, result, .. } => (*state, *result),
            _ => return dep,
        };
        match state {
            PromiseState::Pending => {
                if let HeapObj::Promise { fulfill, reject, handled, .. } = self.heap.get_mut(p) {
                    fulfill.push(Reaction { callback: on_f, dependent: dep, finally: false, is_async: false });
                    reject.push(Reaction { callback: on_r, dependent: dep, finally: false, is_async: false });
                    if !on_r.is_undefined() {
                        *handled = true;
                    }
                }
            }
            PromiseState::Fulfilled => {
                self.microtasks.push_back(Microtask::Reaction {
                    callback: on_f,
                    arg: result,
                    dependent: dep,
                    kind: ReactionKind::Fulfill,
                    finally: false,
                });
            }
            PromiseState::Rejected => {
                if let HeapObj::Promise { handled, .. } = self.heap.get_mut(p) {
                    *handled = true;
                }
                self.microtasks.push_back(Microtask::Reaction {
                    callback: on_r,
                    arg: result,
                    dependent: dep,
                    kind: ReactionKind::Reject,
                    finally: false,
                });
            }
        }
        dep
    }

    /// `p.finally(cb)`: register a finally reaction on both settle paths (or
    /// schedule immediately if already settled). Returns the dependent promise.
    fn finally_internal(&mut self, p: u32, cb: Value) -> u32 {
        let dep = self.alloc_promise();
        let (state, result) = match self.heap.get(p) {
            HeapObj::Promise { state, result, .. } => (*state, *result),
            _ => return dep,
        };
        match state {
            PromiseState::Pending => {
                if let HeapObj::Promise { fulfill, reject, .. } = self.heap.get_mut(p) {
                    fulfill.push(Reaction { callback: cb, dependent: dep, finally: true, is_async: false });
                    reject.push(Reaction { callback: cb, dependent: dep, finally: true, is_async: false });
                }
            }
            PromiseState::Fulfilled => self.microtasks.push_back(Microtask::Reaction {
                callback: cb,
                arg: result,
                dependent: dep,
                kind: ReactionKind::Fulfill,
                finally: true,
            }),
            PromiseState::Rejected => self.microtasks.push_back(Microtask::Reaction {
                callback: cb,
                arg: result,
                dependent: dep,
                kind: ReactionKind::Reject,
                finally: true,
            }),
        }
        dep
    }

    // ── async functions ──

    /// Build a suspended `async function` activation and run it synchronously up
    /// to its first `await` (or to completion / a throw). Returns the activation's
    /// result Promise — the value an `async` call evaluates to.
    fn alloc_async(&mut self, func_id: u32, closure: u32, this: Value, args: &[Value]) -> Value {
        let proto = &self.program.functions[func_id as usize];
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        let result = self.alloc_promise();
        let idx = self.heap.alloc(HeapObj::AsyncState(Box::new(AsyncStateData {
            func: func_id,
            closure,
            state: GenState::Suspended(0),
            regs,
            result,
            handlers: Vec::new(),
        })));
        // Run from the top until the first await suspends it (or it finishes —
        // settling `result` either way).
        self.drive_async(idx, Resume::Value(Value::UNDEFINED));
        Value::heap(result)
    }

    /// `Promise.resolve` as an internal helper: a Promise passes through (identity
    /// preserved); any other value is wrapped in a fulfilled promise. The basis of
    /// awaiting a non-promise (`await 5` still yields a microtask tick).
    fn to_promise(&mut self, v: Value) -> u32 {
        if v.is_heap() {
            if matches!(self.heap.get(v.heap_index()), HeapObj::Promise { .. }) {
                return v.heap_index();
            }
        }
        let p = self.alloc_promise();
        self.resolve(p, v);
        p
    }

    /// Subscribe a suspended async `activation` to promise `p`: when `p` settles,
    /// the activation resumes with the value, or has the reason thrown back in at
    /// the await point. If `p` is already settled, schedule the resume as a
    /// microtask (so `await` always yields to the queue, per spec).
    fn settle_subscribe(&mut self, p: u32, activation: u32) {
        let (state, result) = match self.heap.get(p) {
            HeapObj::Promise { state, result, .. } => (*state, *result),
            _ => {
                self.microtasks.push_back(Microtask::AsyncResume {
                    activation,
                    input: Resume::Value(Value::UNDEFINED),
                });
                return;
            }
        };
        match state {
            PromiseState::Pending => {
                if let HeapObj::Promise { fulfill, reject, handled, .. } = self.heap.get_mut(p) {
                    fulfill.push(Reaction {
                        callback: Value::UNDEFINED,
                        dependent: activation,
                        finally: false,
                        is_async: true,
                    });
                    reject.push(Reaction {
                        callback: Value::UNDEFINED,
                        dependent: activation,
                        finally: false,
                        is_async: true,
                    });
                    *handled = true; // an `await` consumes the rejection
                }
            }
            PromiseState::Fulfilled => self.microtasks.push_back(Microtask::AsyncResume {
                activation,
                input: Resume::Value(result),
            }),
            PromiseState::Rejected => {
                if let HeapObj::Promise { handled, .. } = self.heap.get_mut(p) {
                    *handled = true;
                }
                self.microtasks.push_back(Microtask::AsyncResume {
                    activation,
                    input: Resume::Throw(result),
                });
            }
        }
    }

    // ── Promise combinators ──

    /// `Promise.all/allSettled/race/any(iterable)`. Coerces each input to a
    /// promise and subscribes a native combinator reaction; the shared
    /// `Combinator` state settles the returned promise per the combinator's rule.
    fn promise_combine(&mut self, kind: crate::heap::CombKind, iterable: Value) -> Result<Value, Thrown> {
        use crate::heap::CombKind;
        let inputs = self.iterate_to_vec(iterable)?;
        let total = inputs.len() as u32;
        let result = self.alloc_promise();
        if total == 0 {
            // Empty-iterable terminal cases (race stays pending forever).
            match kind {
                CombKind::All | CombKind::AllSettled => {
                    let arr = Value::heap(self.heap.alloc(HeapObj::Array(Vec::new())));
                    self.resolve(result, arr);
                }
                CombKind::Any => {
                    let e = self.alloc_aggregate_error(Vec::new());
                    self.reject(result, e);
                }
                CombKind::Race => {}
            }
            return Ok(Value::heap(result));
        }
        let comb = self.heap.alloc(HeapObj::Combinator {
            kind,
            results: vec![Value::UNDEFINED; total as usize],
            remaining: total,
            result,
        });
        for (i, inp) in inputs.into_iter().enumerate() {
            let p = self.to_promise(inp);
            let resolver = Value::heap(self.heap.alloc(HeapObj::CombinatorResolver {
                combinator: comb,
                index: i as u32,
            }));
            // Both settle paths route to the resolver (it dispatches on the kind).
            self.then_internal(p, resolver, resolver, None);
        }
        Ok(Value::heap(result))
    }

    /// Perform one combinator step: the input at `index` settled (`kind`) with
    /// `value`. Updates the shared state and settles the combinator's promise
    /// when its rule is met (the one-shot `settle` guard absorbs later inputs).
    fn combinator_step(&mut self, comb: u32, index: u32, kind: ReactionKind, value: Value) {
        use crate::heap::CombKind;
        let (ckind, result) = match self.heap.get(comb) {
            HeapObj::Combinator { kind, result, .. } => (*kind, *result),
            _ => return,
        };
        match (ckind, kind) {
            (CombKind::Race, ReactionKind::Fulfill) => self.resolve(result, value),
            (CombKind::Race, ReactionKind::Reject) => self.reject(result, value),
            (CombKind::All, ReactionKind::Reject) => self.reject(result, value),
            (CombKind::Any, ReactionKind::Fulfill) => self.resolve(result, value),
            (CombKind::All, ReactionKind::Fulfill)
            | (CombKind::Any, ReactionKind::Reject)
            | (CombKind::AllSettled, _) => {
                // Record the per-input outcome and decrement the outstanding count.
                let stored = if ckind == CombKind::AllSettled {
                    self.make_settled_record(kind, value)
                } else {
                    value
                };
                let done = if let HeapObj::Combinator { results, remaining, .. } =
                    self.heap.get_mut(comb)
                {
                    results[index as usize] = stored;
                    *remaining -= 1;
                    *remaining == 0
                } else {
                    false
                };
                if done {
                    let collected = match self.heap.get(comb) {
                        HeapObj::Combinator { results, .. } => results.clone(),
                        _ => Vec::new(),
                    };
                    match ckind {
                        CombKind::Any => {
                            // All inputs rejected → AggregateError of the reasons.
                            let e = self.alloc_aggregate_error(collected);
                            self.reject(result, e);
                        }
                        _ => {
                            let arr = Value::heap(self.heap.alloc(HeapObj::Array(collected)));
                            self.resolve(result, arr);
                        }
                    }
                }
            }
        }
    }

    /// Build a `Promise.allSettled` record: `{status:'fulfilled', value}` or
    /// `{status:'rejected', reason}`.
    fn make_settled_record(&mut self, kind: ReactionKind, value: Value) -> Value {
        let mut map = ObjMap::new();
        match kind {
            ReactionKind::Fulfill => {
                let s = self.alloc_str("fulfilled".to_string());
                map.set("status", s);
                map.set("value", value);
            }
            ReactionKind::Reject => {
                let s = self.alloc_str("rejected".to_string());
                map.set("status", s);
                map.set("reason", value);
            }
        }
        Value::heap(self.heap.alloc(HeapObj::Object(map)))
    }

    /// Build an `AggregateError`-like object `{name, message, errors}` for a
    /// failed `Promise.any`.
    fn alloc_aggregate_error(&mut self, errors: Vec<Value>) -> Value {
        let mut map = ObjMap::new();
        let name = self.alloc_str("AggregateError".to_string());
        map.set("name", name);
        let msg = self.alloc_str("All promises were rejected".to_string());
        map.set("message", msg);
        let errs = Value::heap(self.heap.alloc(HeapObj::Array(errors)));
        map.set("errors", errs);
        Value::heap(self.heap.alloc(HeapObj::Object(map)))
    }

    /// Resume (or start) a suspended async activation `idx` with `input` — the
    /// awaited value (fulfill) or the reason to throw in at the await point
    /// (reject). Runs until the next `await` (re-parks the window + subscribes to
    /// the awaited promise), a normal return (resolves the result Promise), or an
    /// uncaught throw (rejects it). Mirrors `generator_method`'s resume path, but
    /// restores the activation's `try` handlers so a rejection can be caught.
    fn drive_async(&mut self, idx: u32, input: Resume) {
        let (state, fid, closure, result) = match self.heap.get(idx) {
            HeapObj::AsyncState(a) => (a.state, a.func, a.closure, a.result),
            _ => return,
        };
        let resume_ip = match state {
            GenState::Completed | GenState::Running => return,
            GenState::Suspended(ip) => ip,
        };
        // Detach the saved window + handlers, then splice the window onto the top
        // of the live register file.
        let (saved, saved_handlers) = match self.heap.get_mut(idx) {
            HeapObj::AsyncState(a) => {
                a.state = GenState::Running;
                (std::mem::take(&mut a.regs), std::mem::take(&mut a.handlers))
            }
            _ => return,
        };
        let reg_count = saved.len();
        let new_base = self.regs.len();
        if self.regs_would_overflow(new_base + reg_count) {
            // Can't make progress — abandon the activation and reject its result.
            if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                a.state = GenState::Completed;
                a.regs.clear();
                a.handlers.clear();
            }
            let e = self.alloc_error_from_message("RangeError: Maximum call stack size exceeded");
            self.reject(result, e);
            return;
        }
        self.regs.extend_from_slice(&saved);
        if new_base + reg_count > self.regs_hw {
            self.regs_hw = new_base + reg_count;
        }
        let stop = self.frames.len();
        self.frames.push(Frame {
            func: fid,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure,
            handlers: saved_handlers,
        });
        // Position the resume point and deliver the awaited value / rejection.
        let outcome = if resume_ip == 0 {
            self.run_loop(stop)
        } else {
            match input {
                Resume::Value(v) => {
                    if let Instr::Await { dst, .. } =
                        self.program.functions[fid as usize].code[resume_ip]
                    {
                        self.regs[new_base + dst as usize] = v;
                    }
                    self.frames[stop].ip = resume_ip + 1;
                    self.run_loop(stop)
                }
                Resume::Throw(e) => {
                    // Throw the rejection in at the await point: unwind to a
                    // handler within this activation (down to `stop`). If caught,
                    // resume at the catch; otherwise it propagates out as the
                    // function's rejection (pending_throw stays set for the Err
                    // arm below).
                    self.pending_throw = Some(e);
                    if self.unwind_to_handler(e, stop) {
                        self.pending_throw = None;
                        self.run_loop(stop)
                    } else {
                        Err(Thrown(String::new()))
                    }
                }
            }
        };
        // Suspended again at an await?
        if let Some((awaited, await_ip, handlers)) = self.pending_await.take() {
            let back = self.regs.split_off(new_base);
            if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                a.state = GenState::Suspended(await_ip);
                a.regs = back;
                a.handlers = handlers;
            }
            let p = self.to_promise(awaited);
            self.settle_subscribe(p, idx);
            return;
        }
        // Otherwise the activation finished — settle `result`.
        match outcome {
            Ok(ret) => {
                if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                    a.state = GenState::Completed;
                    a.regs.clear();
                    a.handlers.clear();
                }
                self.resolve(result, ret);
            }
            Err(_) => {
                let e = match self.pending_throw.take() {
                    Some(v) => v,
                    None => self.alloc_error_from_message("Error"),
                };
                // The unwind already truncated the window; keep regs consistent.
                self.regs.truncate(new_base);
                if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                    a.state = GenState::Completed;
                    a.regs.clear();
                    a.handlers.clear();
                }
                self.reject(result, e);
            }
        }
    }

    /// Run one microtask. A reaction's callback may be a JS function (re-enters
    /// the VM; a throw REJECTS the dependent, never unwinds the drain), a native
    /// BoundResolver, or undefined (pass-through). `AsyncResume` resumes an async
    /// activation (Stage 2).
    fn run_microtask(&mut self, t: Microtask) {
        match t {
            Microtask::Reaction { callback, arg, dependent, kind, finally } => {
                if finally {
                    // Run cb (no args) for its side effect, then forward the
                    // original value/reason — unless cb itself throws.
                    if !callback.is_undefined() {
                        if let Err(_) = self.call_value(callback, Value::UNDEFINED, &[]) {
                            let r = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                            self.reject(dependent, r);
                            return;
                        }
                    }
                    match kind {
                        ReactionKind::Fulfill => self.resolve(dependent, arg),
                        ReactionKind::Reject => self.reject(dependent, arg),
                    }
                    return;
                }
                if callback.is_undefined() {
                    match kind {
                        ReactionKind::Fulfill => self.resolve(dependent, arg),
                        ReactionKind::Reject => self.reject(dependent, arg),
                    }
                    return;
                }
                if callback.is_heap() {
                    if let HeapObj::BoundResolver { promise, is_reject } =
                        self.heap.get(callback.heap_index())
                    {
                        let (pr, isr) = (*promise, *is_reject);
                        if isr {
                            self.reject(pr, arg);
                        } else {
                            self.resolve(pr, arg);
                        }
                        return;
                    }
                    // A combinator reaction (Promise.all/allSettled/race/any).
                    if let HeapObj::CombinatorResolver { combinator, index } =
                        self.heap.get(callback.heap_index())
                    {
                        let (c, i) = (*combinator, *index);
                        self.combinator_step(c, i, kind, arg);
                        return;
                    }
                }
                match self.call_value(callback, Value::UNDEFINED, &[arg]) {
                    Ok(ret) => self.resolve(dependent, ret),
                    Err(_) => {
                        let r = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                        self.reject(dependent, r);
                    }
                }
            }
            // Resumes a suspended async activation with the settled value (or by
            // throwing the rejection reason in at the await point).
            Microtask::AsyncResume { activation, input } => {
                self.drive_async(activation, input);
            }
        }
    }

    /// Drain the microtask queue to empty (FIFO; tasks enqueued during the drain
    /// run in the same drain). The whole event loop.
    fn drain_microtasks(&mut self) {
        while let Some(t) = self.microtasks.pop_front() {
            self.run_microtask(t);
        }
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
        // Object / callable / class index access is property access: delegate to
        // `get_prop` so a computed key reaches inherited methods/getters (e.g. a
        // class instance's `obj[Symbol.iterator]`), a callable's `fn["name"]`, and
        // static members (`C["m"]`) — not just own data properties.
        if matches!(
            self.heap.get(obj.heap_index()),
            HeapObj::Object(_)
                | HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Bound { .. }
                | HeapObj::Native(_)
        ) {
            let k = self.display(key);
            return self.get_prop(obj, &k);
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
                    // A single ASCII char is interned at heap index == its byte
                    // (see Heap::new), so return that slot DIRECTLY — no temporary
                    // 1-char String + re-intern per access (that alloc dominated
                    // `s[i]` scans). O(1) for ASCII (i-th char == i-th byte); a
                    // multi-byte string walks scalars (O(i), correct).
                    if s.ascii {
                        return Ok(match s.bytes.as_bytes().get(i) {
                            Some(&b) => Value::heap(b as u32),
                            None => Value::UNDEFINED,
                        });
                    }
                    match s.bytes.chars().nth(i) {
                        Some(ch) if (ch as u32) < 128 => return Ok(Value::heap(ch as u32)),
                        Some(ch) => {
                            let cs = ch.to_string();
                            return Ok(self.alloc_str(cs));
                        }
                        None => return Ok(Value::UNDEFINED),
                    }
                }
                // Non-numeric key: only `s["length"]` is meaningful — mirror the
                // array and `s.length` paths.
                let char_len = s.char_len;
                if self.display(key) == "length" {
                    return Ok(len_value(char_len));
                }
                Ok(Value::UNDEFINED)
            }
            // Positional access drives for-of / spread over a Map (the i-th
            // [key, value] entry) and a Set (the i-th value). Insertion order.
            HeapObj::Map { keys, vals } => {
                if let Some(i) = array_index(key) {
                    if i < keys.len() {
                        let (k, v) = (keys[i], vals[i]);
                        return Ok(Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))));
                    }
                }
                Ok(Value::UNDEFINED)
            }
            HeapObj::Set(items) => {
                if let Some(i) = array_index(key) {
                    if i < items.len() {
                        return Ok(items[i]);
                    }
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
        // Callable / class computed assignment (`fn["x"] = v`, `C["s"] = v`) is
        // property assignment: route through `set_prop` (honours non-writable
        // `name`/`length`, static setters, function own props).
        if matches!(
            self.heap.get(idx),
            HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Bound { .. }
                | HeapObj::Native(_)
        ) {
            let k = self.display(key);
            return self.set_prop(obj, &k, val);
        }
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

    /// `new Date(...)` → epoch ms. 0 args = now; 1 number = ms (time-clipped);
    /// 1 Date = copy; 1 string = parsed; ≥2 = UTC components (month0-based).
    fn date_new_ms(&self, args: &[Value]) -> Result<f64, Thrown> {
        match args.len() {
            0 => Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as f64)
                .unwrap_or(0.0)),
            1 => {
                let a = args[0];
                if a.is_heap() {
                    if let HeapObj::Date(ms) = self.heap.get(a.heap_index()) {
                        return Ok(*ms);
                    }
                    if matches!(self.heap.get(a.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. }) {
                        let s = self.heap.str_cow(a.heap_index()).unwrap().into_owned();
                        return Ok(parse_date(&s));
                    }
                }
                Ok(time_clip(self.to_number(a)?))
            }
            _ => {
                let mut comp = [0i64, 0, 1, 0, 0, 0, 0]; // y, mo0, day, h, mi, s, ms
                for (i, &v) in args.iter().enumerate().take(7) {
                    let n = self.to_number(v)?;
                    if n.is_nan() {
                        return Ok(f64::NAN);
                    }
                    comp[i] = n as i64;
                }
                comp[0] = legacy_year(comp[0]);
                Ok(time_clip(ms_from_utc(comp[0], comp[1], comp[2], comp[3], comp[4], comp[5], comp[6])))
            }
        }
    }

    /// `Date.UTC(year, month0, …)` → epoch ms (NaN with no args / a NaN field).
    fn date_utc_ms(&self, args: &[Value]) -> Result<f64, Thrown> {
        if args.is_empty() {
            return Ok(f64::NAN);
        }
        let mut comp = [0i64, 0, 1, 0, 0, 0, 0];
        for (i, &v) in args.iter().enumerate().take(7) {
            let n = self.to_number(v)?;
            if n.is_nan() {
                return Ok(f64::NAN);
            }
            comp[i] = n as i64;
        }
        comp[0] = legacy_year(comp[0]);
        Ok(time_clip(ms_from_utc(comp[0], comp[1], comp[2], comp[3], comp[4], comp[5], comp[6])))
    }

    /// Dispatch a method on a `Date` receiver (`idx` is its heap index). All
    /// getters/setters are UTC. Returns `Ok(None)` if `name` isn't a Date method.
    fn date_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let ms = match self.heap.get(idx) {
            HeapObj::Date(m) => *m,
            _ => return Ok(None),
        };
        let p = date_parts(ms); // (year, month0, day, hour, min, sec, ms, weekday)
        let field = |v: i64| if ms.is_nan() { Value::num(f64::NAN) } else { Value::num(v as f64) };
        let r = match name {
            "getTime" | "valueOf" => Value::num(ms),
            "getFullYear" | "getUTCFullYear" => field(p.0),
            "getMonth" | "getUTCMonth" => field(p.1),
            "getDate" | "getUTCDate" => field(p.2),
            "getHours" | "getUTCHours" => field(p.3),
            "getMinutes" | "getUTCMinutes" => field(p.4),
            "getSeconds" | "getUTCSeconds" => field(p.5),
            "getMilliseconds" | "getUTCMilliseconds" => field(p.6),
            "getDay" | "getUTCDay" => field(p.7),
            "getTimezoneOffset" => Value::num(if ms.is_nan() { f64::NAN } else { 0.0 }),
            "toISOString" => {
                if ms.is_nan() {
                    return Err(Thrown("RangeError: Invalid time value".into()));
                }
                self.alloc_str(date_to_iso(ms))
            }
            "toJSON" => {
                if ms.is_nan() {
                    Value::NULL
                } else {
                    self.alloc_str(date_to_iso(ms))
                }
            }
            // Simplified: ISO (node's local/tz-formatted strings aren't matched).
            "toString" | "toUTCString" | "toDateString" | "toTimeString"
            | "toLocaleString" | "toLocaleDateString" | "toLocaleTimeString" => {
                if ms.is_nan() {
                    self.alloc_str("Invalid Date".to_string())
                } else {
                    self.alloc_str(date_to_iso(ms))
                }
            }
            "setTime" => {
                let n = match args.first() {
                    Some(&v) => time_clip(self.to_number(v)?),
                    None => f64::NAN,
                };
                if let HeapObj::Date(m) = self.heap.get_mut(idx) {
                    *m = n;
                }
                Value::num(n)
            }
            "setFullYear" | "setUTCFullYear" => self.date_set(idx, &p, args, 0)?,
            "setMonth" | "setUTCMonth" => self.date_set(idx, &p, args, 1)?,
            "setDate" | "setUTCDate" => self.date_set(idx, &p, args, 2)?,
            "setHours" | "setUTCHours" => self.date_set(idx, &p, args, 3)?,
            "setMinutes" | "setUTCMinutes" => self.date_set(idx, &p, args, 4)?,
            "setSeconds" | "setUTCSeconds" => self.date_set(idx, &p, args, 5)?,
            "setMilliseconds" | "setUTCMilliseconds" => self.date_set(idx, &p, args, 6)?,
            _ => return Ok(None),
        };
        Ok(Some(r))
    }

    /// A Date setter starting at component `start` (0=year … 6=ms): overwrite that
    /// field and the following ones from `args`, recompute, store, return the new ms.
    fn date_set(
        &mut self,
        idx: u32,
        p: &(i64, i64, i64, i64, i64, i64, i64, i64),
        args: &[Value],
        start: usize,
    ) -> Result<Value, Thrown> {
        let mut comp = [p.0, p.1, p.2, p.3, p.4, p.5, p.6];
        let mut any_nan = false;
        for (i, &v) in args.iter().enumerate() {
            if start + i >= 7 {
                break;
            }
            let n = self.to_number(v)?;
            if n.is_nan() {
                any_nan = true;
            }
            comp[start + i] = n as i64;
        }
        let ms = if any_nan {
            f64::NAN
        } else {
            time_clip(ms_from_utc(comp[0], comp[1], comp[2], comp[3], comp[4], comp[5], comp[6]))
        };
        if let HeapObj::Date(m) = self.heap.get_mut(idx) {
            *m = ms;
        }
        Ok(Value::num(ms))
    }

    /// The `.prototype` object of a function/class value — lazily created and
    /// cached so it has stable identity (`C.prototype === C.prototype`). A class's
    /// prototype carries its OWN methods plus a `constructor` back-reference; a
    /// plain function's prototype just has `constructor`. `None` for non-callables
    /// (a plain object / array / instance has no `.prototype`).
    fn prototype_of(&mut self, obj: Value) -> Option<Value> {
        if !obj.is_heap() {
            return None;
        }
        let idx = obj.heap_index();
        if !matches!(
            self.heap.get(idx),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Class(_)
        ) {
            return None;
        }
        if let Some(&p) = self.prototypes.get(&idx) {
            return Some(Value::heap(p));
        }
        // Collect own methods first (ends the immutable heap borrow before alloc).
        let methods: Vec<(String, Value)> = match self.heap.get(idx) {
            HeapObj::Class(c) => c.methods.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            _ => Vec::new(),
        };
        // Methods and the constructor back-reference are NON-enumerable
        // (writable + configurable), matching ES `class`/function semantics that
        // test262's verifyProperty checks.
        let nonenum =
            PropAttr { writable: true, enumerable: false, configurable: true, accessor: false, setter: Value::UNDEFINED };
        let mut map = ObjMap::new();
        for (k, v) in &methods {
            map.define(k, *v, nonenum);
        }
        map.define("constructor", obj, nonenum);
        let p = self.heap.alloc(HeapObj::Object(map));
        self.prototypes.insert(idx, p);
        Some(Value::heap(p))
    }

    /// Build the built-in global object graph (Object/Array/Function + their
    /// prototypes, with methods as native function VALUES) and inject it into the
    /// global slots the compiler reserved for those free identifiers. Makes
    /// `Array.isArray`, `Object.defineProperty`, `Function.prototype.call`, etc.
    /// usable as first-class values (what the test262 harness binds).
    fn setup_globals(&mut self) {
        use native::*;
        // A built-in method property: a native function, non-enumerable but
        // writable + configurable (matching built-in method descriptors).
        let method_attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let proto_attr = PropAttr {
            writable: false,
            enumerable: false,
            configurable: false,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let mut build = |vm: &mut Self, methods: &[(&str, u16)], protolink: Option<u32>| -> u32 {
            let mut m = ObjMap::new();
            for &(name, id) in methods {
                let nv = Value::heap(vm.heap.alloc(HeapObj::Native(id)));
                m.define(name, nv, method_attr);
            }
            if let Some(p) = protolink {
                m.define("prototype", Value::heap(p), proto_attr);
            }
            vm.heap.alloc(HeapObj::Object(m))
        };
        // Prototypes.
        self.obj_proto = build(
            self,
            &[
                ("hasOwnProperty", PROTO_HAS_OWN),
                ("propertyIsEnumerable", PROTO_PROP_ENUM),
                ("isPrototypeOf", PROTO_IS_PROTO_OF),
                ("valueOf", PROTO_VALUE_OF),
                ("toString", PROTO_TO_STRING),
            ],
            None,
        );
        self.fn_proto = build(
            self,
            &[("call", FN_CALL), ("apply", FN_APPLY), ("bind", FN_BIND)],
            None,
        );
        self.arr_proto = build(self, &[("join", ARR_JOIN), ("push", ARR_PUSH)], None);
        // Constructors.
        let obj_proto = self.obj_proto;
        let arr_proto = self.arr_proto;
        let fn_proto = self.fn_proto;
        let object_ctor = build(
            self,
            &[
                ("defineProperty", OBJ_DEFINE_PROPERTY),
                ("defineProperties", OBJ_DEFINE_PROPERTIES),
                ("getOwnPropertyDescriptor", OBJ_GET_OWN_DESC),
                ("getOwnPropertyNames", OBJ_GET_OWN_NAMES),
                ("getPrototypeOf", OBJ_GET_PROTO),
                ("keys", OBJ_KEYS),
                ("values", OBJ_VALUES),
                ("entries", OBJ_ENTRIES),
                ("assign", OBJ_ASSIGN),
                ("create", OBJ_CREATE),
            ],
            Some(obj_proto),
        );
        let array_ctor = build(self, &[("isArray", ARR_IS_ARRAY), ("from", ARR_FROM), ("of", ARR_OF)], Some(arr_proto));
        let function_ctor = build(self, &[], Some(fn_proto));
        // Inject into the reserved global slots (collect first to end the program
        // borrow before mutating `self.globals`).
        let mut sets: Vec<(usize, u32)> = Vec::new();
        for (slot, name) in self.program.global_names.iter().enumerate() {
            let v = match name.as_str() {
                "Object" => Some(object_ctor),
                "Array" => Some(array_ctor),
                "Function" => Some(function_ctor),
                _ => None,
            };
            if let Some(v) = v {
                sets.push((slot, v));
            }
        }
        for (slot, v) in sets {
            if slot < self.globals.len() {
                self.globals[slot] = Value::heap(v);
            }
        }
    }

    /// Invoke a native (built-in) function by id with `this` and `args`. Backs
    /// first-class builtin values (`Object.defineProperty`, `Array.isArray`,
    /// `Object.prototype.hasOwnProperty`, `Function.prototype.call`, …).
    fn call_native(&mut self, id: u16, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        use native::*;
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        Ok(match id {
            OBJ_DEFINE_PROPERTY => {
                let key = self.display(a1);
                self.object_define_property(a0, &key, args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
                a0
            }
            OBJ_DEFINE_PROPERTIES => {
                self.object_define_properties(a0, a1)?;
                a0
            }
            OBJ_GET_OWN_DESC => {
                let key = self.display(a1);
                self.object_get_own_property_descriptor(a0, &key)
            }
            OBJ_GET_OWN_NAMES => self.object_own_property_names(a0),
            OBJ_GET_PROTO => self.object_get_prototype_of(a0),
            OBJ_KEYS => self.object_enum_own(a0, EnumWhat::Keys),
            OBJ_VALUES => self.object_enum_own(a0, EnumWhat::Values),
            OBJ_ENTRIES => self.object_enum_own(a0, EnumWhat::Entries),
            OBJ_ASSIGN => self.object_assign(args)?,
            OBJ_CREATE => {
                let o = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                if a0 != Value::UNDEFINED {
                    self.proto_of.insert(o.heap_index(), a0);
                }
                if a1 != Value::UNDEFINED {
                    self.object_define_properties(o, a1)?;
                }
                o
            }
            PROTO_HAS_OWN => Value::bool(self.has_own_property(this, &self.display(a0))),
            PROTO_PROP_ENUM => Value::bool(self.own_is_enumerable(this, &self.display(a0))),
            PROTO_IS_PROTO_OF => Value::bool(self.is_prototype_of(this, a0)),
            PROTO_VALUE_OF => this,
            PROTO_TO_STRING => self.alloc_str("[object Object]".to_string()),
            FN_CALL => {
                let rest: &[Value] = if args.len() > 1 { &args[1..] } else { &[] };
                self.call_value(this, a0, rest)?
            }
            FN_APPLY => {
                let callargs = if a1.is_heap() { self.iterate_to_vec(a1)? } else { Vec::new() };
                self.call_value(this, a0, &callargs)?
            }
            FN_BIND => {
                let bound: Vec<Value> = if args.len() > 1 { args[1..].to_vec() } else { Vec::new() };
                Value::heap(self.heap.alloc(HeapObj::Bound { target: this, this: a0, args: bound }))
            }
            ARR_IS_ARRAY => {
                Value::bool(a0.is_heap() && matches!(self.heap.get(a0.heap_index()), HeapObj::Array(_)))
            }
            ARR_FROM => self.array_from(a0, a1)?,
            ARR_OF => Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec()))),
            // `Array.prototype.{join,push}` as values: `this` is the receiver array.
            ARR_JOIN | ARR_PUSH => {
                let m = if id == ARR_JOIN { "join" } else { "push" };
                if this.is_heap() && matches!(self.heap.get(this.heap_index()), HeapObj::Array(_)) {
                    self.array_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
                } else {
                    Value::UNDEFINED
                }
            }
            _ => Value::UNDEFINED,
        })
    }

    /// Own ENUMERABLE keys / values / [k,v] entries of `obj` as an array (the
    /// shared core of `Object.keys`/`values`/`entries`).
    fn object_enum_own(&mut self, obj: Value, what: EnumWhat) -> Value {
        let pairs: Vec<(String, Value)> = if obj.is_heap() {
            match self.heap.get(obj.heap_index()) {
                HeapObj::Object(m) => m
                    .keys
                    .iter()
                    .cloned()
                    .zip(m.vals.iter().copied())
                    .zip(m.attrs.iter())
                    .filter(|(_, a)| a.enumerable)
                    .map(|(kv, _)| kv)
                    .collect(),
                HeapObj::Array(items) => {
                    items.iter().enumerate().map(|(i, v)| (i.to_string(), *v)).collect()
                }
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let out: Vec<Value> = pairs
            .into_iter()
            .map(|(k, v)| match what {
                EnumWhat::Keys => self.alloc_str(k),
                EnumWhat::Values => v,
                EnumWhat::Entries => {
                    let ks = self.alloc_str(k);
                    Value::heap(self.heap.alloc(HeapObj::Array(vec![ks, v])))
                }
            })
            .collect();
        Value::heap(self.heap.alloc(HeapObj::Array(out)))
    }

    /// Build a data property descriptor object `{value, writable, enumerable,
    /// configurable}` (for `Object.getOwnPropertyDescriptor`).
    fn make_data_descriptor(&mut self, value: Value, w: bool, e: bool, c: bool) -> Value {
        let mut m = ObjMap::new();
        m.set("value", value);
        m.set("writable", Value::bool(w));
        m.set("enumerable", Value::bool(e));
        m.set("configurable", Value::bool(c));
        Value::heap(self.heap.alloc(HeapObj::Object(m)))
    }

    /// Build an accessor descriptor object `{get, set, enumerable, configurable}`.
    fn make_accessor_descriptor(&mut self, get: Value, set: Value, e: bool, c: bool) -> Value {
        let mut m = ObjMap::new();
        m.set("get", get);
        m.set("set", set);
        m.set("enumerable", Value::bool(e));
        m.set("configurable", Value::bool(c));
        Value::heap(self.heap.alloc(HeapObj::Object(m)))
    }

    /// `Object.getOwnPropertyDescriptor(obj, key)` — the property's descriptor, or
    /// undefined for a missing own property / non-object.
    fn object_get_own_property_descriptor(&mut self, obj: Value, key: &str) -> Value {
        if !obj.is_heap() {
            return Value::UNDEFINED;
        }
        let idx = obj.heap_index();
        // A callable's `name`/`length`: non-writable, non-enumerable, configurable.
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            if let Some(v) = self.callable_intrinsic_value(obj, key) {
                return self.make_data_descriptor(v, false, false, true);
            }
        }
        let own = match self.heap.get(idx) {
            HeapObj::Object(m) => m.pos(key).map(|i| (m.attrs[i], m.vals[i])),
            HeapObj::Array(items) => {
                if key == "length" {
                    let len = len_value(items.len());
                    return self.make_data_descriptor(len, true, false, false);
                }
                match key.parse::<usize>() {
                    Ok(i) if i < items.len() => {
                        let v = items[i];
                        return self.make_data_descriptor(v, true, true, true);
                    }
                    _ => return Value::UNDEFINED,
                }
            }
            // Class static members: data props, plus `static get`/`set` rendered
            // as an accessor descriptor (raw = getter, attr.setter = setter).
            HeapObj::Class(c) => {
                if let Some(i) = c.statics.pos(key) {
                    Some((c.statics.attrs[i], c.statics.vals[i]))
                } else if let Some((_, g)) = c.static_getters.iter().find(|(n, _)| n == key) {
                    let setter = c
                        .static_setters
                        .iter()
                        .find(|(n, _)| n == key)
                        .map(|(_, s)| *s)
                        .unwrap_or(Value::UNDEFINED);
                    let attr = PropAttr {
                        writable: false,
                        enumerable: false,
                        configurable: true,
                        accessor: true,
                        setter,
                    };
                    Some((attr, *g))
                } else if let Some((_, s)) = c.static_setters.iter().find(|(n, _)| n == key) {
                    let attr = PropAttr {
                        writable: false,
                        enumerable: false,
                        configurable: true,
                        accessor: true,
                        setter: *s,
                    };
                    Some((attr, Value::UNDEFINED))
                } else {
                    None
                }
            }
            // A function's assigned own properties (`fn.x = y`).
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                self.fn_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i])))
            }
            _ => None,
        };
        match own {
            Some((a, raw)) if a.accessor => {
                self.make_accessor_descriptor(raw, a.setter, a.enumerable, a.configurable)
            }
            Some((a, raw)) => self.make_data_descriptor(raw, a.writable, a.enumerable, a.configurable),
            None => Value::UNDEFINED,
        }
    }

    /// `Object.getOwnPropertyNames(obj)` — all own string keys (enumerable or not).
    fn object_own_property_names(&mut self, obj: Value) -> Value {
        // Collect the key strings under the (immutable) heap borrow, then allocate
        // the result strings afterwards (alloc needs `&mut self`).
        let mut keys: Vec<String> = Vec::new();
        if obj.is_heap() {
            let idx = obj.heap_index();
            // `length`, then `name` — the spec order for ordinary callables.
            let has_length = self.callable_has_intrinsic(obj, "length");
            let has_name = self.callable_has_intrinsic(obj, "name");
            match self.heap.get(idx) {
                HeapObj::Object(m) => keys.extend(m.keys.iter().cloned()),
                HeapObj::Array(items) => {
                    for i in 0..items.len() {
                        keys.push(i.to_string());
                    }
                    keys.push("length".to_string());
                }
                HeapObj::Class(c) => {
                    if has_length {
                        keys.push("length".to_string());
                    }
                    if has_name {
                        keys.push("name".to_string());
                    }
                    keys.extend(c.statics.keys.iter().cloned());
                    for (n, _) in &c.static_getters {
                        if !keys.iter().any(|k| k == n) {
                            keys.push(n.clone());
                        }
                    }
                    for (n, _) in &c.static_setters {
                        if !keys.iter().any(|k| k == n) {
                            keys.push(n.clone());
                        }
                    }
                }
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                    if has_length {
                        keys.push("length".to_string());
                    }
                    if has_name {
                        keys.push("name".to_string());
                    }
                    if let Some(m) = self.fn_props.get(&idx) {
                        keys.extend(m.keys.iter().cloned());
                    }
                }
                _ => {}
            }
        }
        let names: Vec<Value> = keys.into_iter().map(|k| self.alloc_str(k)).collect();
        Value::heap(self.heap.alloc(HeapObj::Array(names)))
    }

    /// `Object.getPrototypeOf(obj)` — the prototype: a class instance's is its
    /// class's `.prototype`; an `Object.create`d object's is the recorded proto;
    /// otherwise `null` (a plain object's real `Object.prototype` isn't modelled).
    fn object_get_prototype_of(&mut self, obj: Value) -> Value {
        if !obj.is_heap() {
            return Value::NULL;
        }
        let idx = obj.heap_index();
        if let Some(&p) = self.proto_of.get(&idx) {
            return p;
        }
        if idx == self.obj_proto {
            return Value::NULL; // Object.prototype's [[Prototype]] is null
        }
        // kind: 0=plain/instance object, 1=callable, 2=array, 3=other.
        let (class, kind) = match self.heap.get(idx) {
            HeapObj::Object(m) => (m.class, 0u8),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                (None, 1)
            }
            HeapObj::Array(_) => (None, 2),
            _ => (None, 3),
        };
        match kind {
            0 => {
                if let Some(cidx) = class {
                    if let Some(p) = self.prototype_of(Value::heap(cidx)) {
                        return p;
                    }
                }
                if self.obj_proto != 0 {
                    Value::heap(self.obj_proto)
                } else {
                    Value::NULL
                }
            }
            1 if self.fn_proto != 0 => Value::heap(self.fn_proto),
            2 if self.arr_proto != 0 => Value::heap(self.arr_proto),
            _ => Value::NULL,
        }
    }

    /// Read a property-descriptor object's fields (present-or-absent) for
    /// `Object.defineProperty`. Throws if `desc` is not an object.
    fn read_descriptor(
        &mut self,
        desc: Value,
    ) -> Result<(Option<Value>, Option<Value>, Option<Value>, Option<bool>, Option<bool>, Option<bool>), Thrown>
    {
        if !desc.is_heap() || !matches!(self.heap.get(desc.heap_index()), HeapObj::Object(_)) {
            return Err(Thrown("TypeError: Property description must be an object".into()));
        }
        let idx = desc.heap_index();
        let present = |vm: &Self, k: &str| -> bool {
            matches!(vm.heap.get(idx), HeapObj::Object(m) if m.pos(k).is_some())
        };
        let value = if present(self, "value") { Some(self.get_prop(desc, "value")?) } else { None };
        let get = if present(self, "get") { Some(self.get_prop(desc, "get")?) } else { None };
        let set = if present(self, "set") { Some(self.get_prop(desc, "set")?) } else { None };
        let writable = if present(self, "writable") {
            let v = self.get_prop(desc, "writable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        let enumerable = if present(self, "enumerable") {
            let v = self.get_prop(desc, "enumerable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        let configurable = if present(self, "configurable") {
            let v = self.get_prop(desc, "configurable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        Ok((value, get, set, writable, enumerable, configurable))
    }

    /// `Object.defineProperty(obj, key, descriptor)` — define/redefine an own
    /// property with explicit attributes (unspecified attrs default to false on a
    /// new property; an existing non-configurable property rejects most changes).
    fn object_define_property(&mut self, obj: Value, key: &str, desc: Value) -> Result<(), Thrown> {
        if !obj.is_heap() || !matches!(self.heap.get(obj.heap_index()), HeapObj::Object(_)) {
            return Err(Thrown("TypeError: Object.defineProperty called on non-object".into()));
        }
        let (value, get, set, d_wr, d_en, d_cf) = self.read_descriptor(desc)?;
        let idx = obj.heap_index();
        let existing = match self.heap.get(idx) {
            HeapObj::Object(m) => m.pos(key).map(|i| (m.attrs[i], m.vals[i])),
            _ => None,
        };
        let is_accessor = get.is_some() || set.is_some();
        // Start from the existing attrs (redefine) or all-false (new property).
        let (mut wr, mut en, mut cf) = match existing {
            Some((a, _)) => (a.writable, a.enumerable, a.configurable),
            None => (false, false, false),
        };
        if let Some(b) = d_wr {
            wr = b;
        }
        if let Some(b) = d_en {
            en = b;
        }
        if let Some(b) = d_cf {
            cf = b;
        }
        // A non-configurable existing property rejects illegal redefinitions.
        if let Some((a, oldv)) = existing {
            if !a.configurable {
                let make_cfg = d_cf == Some(true);
                let change_enum = d_en.is_some_and(|b| b != a.enumerable);
                let change_kind = is_accessor != a.accessor;
                let make_writable = !a.writable && d_wr == Some(true);
                let change_frozen_value =
                    !a.accessor && !a.writable && value.is_some_and(|v| v != oldv);
                if make_cfg || change_enum || change_kind || make_writable || change_frozen_value {
                    return Err(Thrown(format!("TypeError: Cannot redefine property: {key}")));
                }
            }
        }
        let attr = PropAttr {
            writable: wr,
            enumerable: en,
            configurable: cf,
            accessor: is_accessor,
            setter: set.unwrap_or(Value::UNDEFINED),
        };
        let stored = if is_accessor {
            get.unwrap_or(Value::UNDEFINED)
        } else {
            value.or(existing.map(|(_, v)| v)).unwrap_or(Value::UNDEFINED)
        };
        if let HeapObj::Object(m) = self.heap.get_mut(idx) {
            m.define(key, stored, attr);
        }
        self.heap.bump_version(idx);
        Ok(())
    }

    /// `Object.defineProperties(obj, props)` — define each own enumerable key of
    /// `props` as a descriptor on `obj`.
    fn object_define_properties(&mut self, obj: Value, props: Value) -> Result<(), Thrown> {
        if !props.is_heap() {
            return Ok(());
        }
        let keys: Vec<String> = match self.heap.get(props.heap_index()) {
            HeapObj::Object(m) => m
                .keys
                .iter()
                .zip(m.attrs.iter())
                .filter(|(_, a)| a.enumerable)
                .map(|(k, _)| k.clone())
                .collect(),
            _ => Vec::new(),
        };
        for k in keys {
            let desc = self.get_prop(props, &k)?;
            self.object_define_property(obj, &k, desc)?;
        }
        Ok(())
    }

    /// The `(name, length)` of a callable value (function, closure, or class) for
    /// its `.name`/`.length` properties — `None` for non-callables. A synthetic
    /// proto name (`<arrow>`, `<script>`, …) reads as the empty string (anonymous).
    fn callable_name_length(&self, obj: Value) -> Option<(String, i32)> {
        let clean = |n: &str| -> String {
            if n.starts_with('<') { String::new() } else { n.to_string() }
        };
        match self.heap.get(obj.heap_index()) {
            HeapObj::Func(fid) => {
                let p = &self.program.functions[*fid as usize];
                Some((clean(&p.name), p.param_count as i32))
            }
            HeapObj::Closure { func, .. } => {
                let p = &self.program.functions[*func as usize];
                Some((clean(&p.name), p.param_count as i32))
            }
            HeapObj::Class(c) => {
                let len = c
                    .ctor
                    .map(|f| self.program.functions[f as usize].param_count as i32)
                    .unwrap_or(0);
                Some((clean(&c.name), len))
            }
            _ => None,
        }
    }

    /// Does this callable expose `key` (`name`/`length`) as an own property right
    /// now? True for any named callable unless that intrinsic was `delete`d.
    fn callable_has_intrinsic(&self, obj: Value, key: &str) -> bool {
        let bit = match key {
            "name" => 0u8,
            "length" => 1u8,
            _ => return false,
        };
        if !obj.is_heap() || self.deleted_callable_intrinsics.contains(&(obj.heap_index(), bit)) {
            return false;
        }
        self.callable_name_length(obj).is_some()
    }

    /// The current value of a callable's `name`/`length` own property (allocating
    /// the name string), or None if absent/deleted.
    fn callable_intrinsic_value(&mut self, obj: Value, key: &str) -> Option<Value> {
        if !self.callable_has_intrinsic(obj, key) {
            return None;
        }
        let (nm, len) = self.callable_name_length(obj)?;
        Some(if key == "name" { self.alloc_str(nm) } else { Value::int(len) })
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
        // A function's / class's `.name` and `.length` — synthesized own data
        // properties (configurable, so a prior `delete` suppresses them).
        if key == "name" || key == "length" {
            if let Some(v) = self.callable_intrinsic_value(obj, key) {
                return Ok(v);
            }
        }
        // A function's / class's `.prototype` (a lazily-created, stable object).
        if key == "prototype" {
            if let Some(p) = self.prototype_of(obj) {
                return Ok(p);
            }
        }
        // Own data/accessor property on a plain object. Extracted BEFORE the type
        // match so an accessor's getter can be invoked outside the heap borrow.
        let own = match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => m.pos(key).map(|i| (m.attrs[i], m.vals[i])),
            _ => None,
        };
        if let Some((a, raw)) = own {
            if a.accessor {
                // `raw` is the getter (UNDEFINED ⇒ no getter ⇒ read is undefined).
                return if raw == Value::UNDEFINED { Ok(Value::UNDEFINED) } else { self.call_value(raw, obj, &[]) };
            }
            return Ok(raw);
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Array(items) => {
                if key == "length" {
                    Ok(len_value(items.len()))
                } else if key == "raw" {
                    // A tagged-template strings array's `.raw` (side table).
                    Ok(self.template_raws.get(&obj.heap_index()).copied().unwrap_or(Value::UNDEFINED))
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
            HeapObj::Object(map) => {
                if let Some(v) = map.get(key) {
                    return Ok(v);
                }
                // Own-property miss: walk the class chain for an inherited method
                // (return its func) or getter (invoke it with this = obj).
                let class = map.class;
                let (mut method, mut getter) = (None, None);
                let mut cur = class;
                while let Some(cidx) = cur {
                    match self.heap.get(cidx) {
                        HeapObj::Class(c) => {
                            if let Some((_, v)) = c.methods.iter().find(|(k, _)| k == key) {
                                method = Some(*v);
                                break;
                            }
                            if let Some((_, v)) = c.getters.iter().find(|(k, _)| k == key) {
                                getter = Some(*v);
                                break;
                            }
                            cur = c.parent;
                        }
                        _ => break,
                    }
                }
                if let Some(m) = method {
                    return Ok(m);
                }
                if let Some(g) = getter {
                    return self.call_value(g, obj, &[]);
                }
                // Own + class miss: delegate up the prototype chain — an explicit
                // `Object.create` proto, else a class instance's `C.prototype`
                // (carries `constructor` + inherited methods, and itself chains to
                // Object.prototype), else the base Object.prototype.
                let proto = if let Some(&p) = self.proto_of.get(&obj.heap_index()) {
                    p.is_heap().then_some(p)
                } else if let Some(cidx) = class {
                    self.prototype_of(Value::heap(cidx))
                } else if self.obj_proto != 0 && obj.heap_index() != self.obj_proto {
                    Some(Value::heap(self.obj_proto))
                } else {
                    None
                };
                match proto {
                    Some(p) => self.get_prop(p, key),
                    None => Ok(Value::UNDEFINED),
                }
            }
            // Static members are own properties of the class value; statics are
            // inherited, so walk the `extends` chain (`C.method`, `Sub.parentStatic`).
            // A `static get name()` is invoked with `this` = the class value.
            HeapObj::Class(c) => {
                if let Some(v) = c.statics.get(key) {
                    return Ok(v);
                }
                if let Some((_, g)) = c.static_getters.iter().find(|(k, _)| k == key) {
                    let g = *g;
                    return self.call_value(g, obj, &[]);
                }
                let mut cur = c.parent;
                while let Some(pidx) = cur {
                    match self.heap.get(pidx) {
                        HeapObj::Class(pc) => {
                            if let Some(v) = pc.statics.get(key) {
                                return Ok(v);
                            }
                            if let Some((_, g)) = pc.static_getters.iter().find(|(k, _)| k == key) {
                                let g = *g;
                                return self.call_value(g, obj, &[]);
                            }
                            cur = pc.parent;
                        }
                        _ => break,
                    }
                }
                Ok(Value::UNDEFINED)
            }
            // `map.size` / `set.size` — an accessor property, not a method.
            HeapObj::Map { keys, .. } if key == "size" => Ok(len_value(keys.len())),
            HeapObj::Set(items) if key == "size" => Ok(len_value(items.len())),
            // Functions / natives / bound functions: own props set on them
            // (`assert.sameValue`), then Function.prototype (`call`/`apply`/`bind`).
            _ if matches!(
                self.heap.get(obj.heap_index()),
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
            ) =>
            {
                if let Some(m) = self.fn_props.get(&obj.heap_index()) {
                    if let Some(v) = m.get(key) {
                        return Ok(v);
                    }
                }
                if self.fn_proto != 0 {
                    if let HeapObj::Object(m) = self.heap.get(self.fn_proto) {
                        if let Some(v) = m.get(key) {
                            return Ok(v);
                        }
                    }
                }
                Ok(Value::UNDEFINED)
            }
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
            _ => math_unary(op, arg(0)?),
        })
    }

    /// `Math.<op>` reduced to a single f64 result (used by the `MathSpread`
    /// fallback for an unusual non-variadic spread like `Math.abs(...arr)`).
    fn eval_math_one(&self, op: crate::bytecode::MathFn, x: f64) -> f64 {
        math_unary(op, x)
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
            // A Map/Set/Generator has no enumerable own properties, so
            // JSON.stringify renders it as an empty object (not omitted).
            HeapObj::Map { .. } | HeapObj::Set(_) | HeapObj::Generator { .. } => Some("{}".into()),
            _ => None,
        }
    }

    /// Parse a JSON string into a Value, or throw SyntaxError. Recursive-descent
    /// over the byte string (structure tokens are ASCII; string content is
    /// flushed as UTF-8 slices). Allocates heap objects/arrays/strings.
    fn json_parse(&mut self, src: &str) -> Result<Value, Thrown> {
        let mut i = 0;
        json_skip_ws(src.as_bytes(), &mut i);
        let v = self.json_parse_value(src, &mut i)?;
        json_skip_ws(src.as_bytes(), &mut i);
        if i != src.len() {
            return Err(Thrown("SyntaxError: Unexpected non-whitespace character after JSON".into()));
        }
        Ok(v)
    }

    fn json_parse_value(&mut self, src: &str, i: &mut usize) -> Result<Value, Thrown> {
        let b = src.as_bytes();
        match b.get(*i).copied() {
            Some(b'{') => self.json_parse_object(src, i),
            Some(b'[') => self.json_parse_array(src, i),
            Some(b'"') => {
                let s = json_parse_string(src, i)?;
                Ok(self.alloc_str(s))
            }
            Some(b't') => {
                json_expect(b, i, "true")?;
                Ok(Value::bool(true))
            }
            Some(b'f') => {
                json_expect(b, i, "false")?;
                Ok(Value::bool(false))
            }
            Some(b'n') => {
                json_expect(b, i, "null")?;
                Ok(Value::NULL)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => json_parse_number(b, i),
            _ => Err(Thrown("SyntaxError: Unexpected token in JSON".into())),
        }
    }

    fn json_parse_array(&mut self, src: &str, i: &mut usize) -> Result<Value, Thrown> {
        let b = src.as_bytes();
        *i += 1; // '['
        let mut items = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) == Some(&b']') {
            *i += 1;
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(items))));
        }
        loop {
            json_skip_ws(b, i);
            let v = self.json_parse_value(src, i)?;
            items.push(v);
            json_skip_ws(b, i);
            match b.get(*i) {
                Some(b',') => *i += 1,
                Some(b']') => {
                    *i += 1;
                    break;
                }
                _ => return Err(Thrown("SyntaxError: Expected ',' or ']' in JSON array".into())),
            }
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(items))))
    }

    fn json_parse_object(&mut self, src: &str, i: &mut usize) -> Result<Value, Thrown> {
        let b = src.as_bytes();
        *i += 1; // '{'
        let mut pairs: Vec<(String, Value)> = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) != Some(&b'}') {
            loop {
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b'"') {
                    return Err(Thrown("SyntaxError: Expected property name string in JSON".into()));
                }
                let key = json_parse_string(src, i)?;
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return Err(Thrown("SyntaxError: Expected ':' in JSON object".into()));
                }
                *i += 1;
                json_skip_ws(b, i);
                let val = self.json_parse_value(src, i)?;
                pairs.push((key, val));
                json_skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b'}') => break,
                    _ => return Err(Thrown("SyntaxError: Expected ',' or '}' in JSON object".into())),
                }
            }
        }
        *i += 1; // '}'
        let mut map = crate::heap::ObjMap::new();
        for (k, v) in pairs {
            map.set(&k, v);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Object(map))))
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
                // A class is callable (with `new`), so `typeof C === "function"`.
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::BoundResolver { .. } => "function",
                HeapObj::Cell(inner) => self.type_of(*inner), // see through an upvalue cell
                _ => "object", // Array, Object
            }
        } else {
            "undefined"
        }
    }

    /// `delete obj[key]` — remove an own property, returning the boolean result.
    /// Without property descriptors every own property is configurable, so this
    /// yields `true` (matching `delete` on a missing property / non-object too).
    /// An array element delete leaves a hole (reads as `undefined`), length kept.
    fn delete_prop(&mut self, obj: Value, key: &str) -> Value {
        if !obj.is_heap() {
            return Value::bool(true);
        }
        let idx = obj.heap_index();
        // A non-configurable own property cannot be deleted (`delete` yields false).
        if let HeapObj::Object(m) = self.heap.get(idx) {
            if let Some(i) = m.pos(key) {
                if !m.attrs[i].configurable {
                    return Value::bool(false);
                }
            }
        }
        // A callable's `name`/`length` are configurable: record the deletion so
        // the synthesized property stops appearing (own-property queries + reads).
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            self.deleted_callable_intrinsics
                .insert((idx, if key == "name" { 0 } else { 1 }));
            return Value::bool(true);
        }
        let removed = match self.heap.get_mut(idx) {
            HeapObj::Object(map) => map.remove(key),
            HeapObj::Array(items) => {
                if let Ok(i) = key.parse::<usize>() {
                    if i < items.len() {
                        items[i] = Value::UNDEFINED;
                    }
                }
                false // array slot stays (a hole); no version bump needed
            }
            HeapObj::Class(c) => c.statics.remove(key),
            // A function's assigned own property (`delete fn.x`).
            _ => self.fn_props.get_mut(&idx).map_or(false, |m| m.remove(key)),
        };
        if removed {
            self.heap.bump_version(idx); // a key was removed → slots shifted (IC)
        }
        Value::bool(true)
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
        // A callable's `name`/`length` are non-writable: assignment is a sloppy
        // no-op while the synthesized intrinsic is present. (Once `delete`d it
        // falls through and becomes an ordinary assigned property.)
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            return Ok(());
        }
        // An OWN property's descriptor governs assignment: an accessor invokes its
        // setter; a non-writable data property silently ignores the write (sloppy).
        let own_attr = match self.heap.get(idx) {
            HeapObj::Object(m) => m.pos(key).map(|i| m.attrs[i]),
            _ => None,
        };
        if let Some(a) = own_attr {
            if a.accessor {
                if a.setter != Value::UNDEFINED {
                    self.call_value(a.setter, obj, &[val])?;
                }
                return Ok(()); // accessor with no setter ⇒ no-op (sloppy)
            }
            if !a.writable {
                return Ok(()); // non-writable own data property ⇒ no-op (sloppy)
            }
            // writable own data property → fall through to overwrite its value.
        }
        // A class instance with an inherited `set x(v)` accessor: assigning a
        // property that is NOT an own data property invokes the setter (own data
        // properties shadow an inherited accessor, per JS [[Set]]).
        if let HeapObj::Object(map) = self.heap.get(idx) {
            if map.class.is_some() && map.get(key).is_none() {
                if let Some(setter) = self.lookup_setter(map.class, key) {
                    self.call_value(setter, obj, &[val])?;
                    return Ok(());
                }
            }
        }
        // A function value's own property (`fn.x = …`, e.g. `assert.sameValue`)
        // lives in a side table (functions carry no inline property map).
        if matches!(
            self.heap.get(idx),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
        ) {
            // Reassigning `fn.prototype = obj` redirects what `new fn()` / the
            // `.prototype` getter see (the lazily-cached prototype object).
            if key == "prototype" && val.is_heap() {
                self.prototypes.insert(idx, val.heap_index());
            } else {
                self.fn_props.entry(idx).or_insert_with(ObjMap::new).set(key, val);
            }
            return Ok(());
        }
        // A `static set name(v)` (or getter-only accessor) on the class chain
        // intercepts the write before it becomes a static data property.
        if matches!(self.heap.get(idx), HeapObj::Class(_)) {
            match self.lookup_static_accessor(Some(idx), key) {
                Some(Some(setter)) => {
                    self.call_value(setter, obj, &[val])?;
                    return Ok(());
                }
                Some(None) => return Ok(()), // getter-only ⇒ sloppy no-op
                None => {}                    // fall through to a data write
            }
        }
        let mut added = false;
        match self.heap.get_mut(idx) {
            HeapObj::Object(map) => added = map.set(key, val),
            // Static-member assignment on a class value (`C.x = …`).
            HeapObj::Class(c) => {
                c.statics.set(key, val);
            }
            _ => {}
        }
        if added {
            self.heap.bump_version(idx); // invalidate any JIT inline cache (vals realloc)
        }
        Ok(())
    }

    /// Walk a class chain for a `set key(v)` accessor, returning the setter fn.
    fn lookup_setter(&self, class: Option<u32>, key: &str) -> Option<Value> {
        let mut cur = class;
        while let Some(cidx) = cur {
            match self.heap.get(cidx) {
                HeapObj::Class(c) => {
                    if let Some((_, v)) = c.setters.iter().find(|(k, _)| k == key) {
                        return Some(*v);
                    }
                    cur = c.parent;
                }
                _ => break,
            }
        }
        None
    }

    /// Resolve a static-property write against the class chain starting at heap
    /// index `start`. The first chain level that owns the key decides:
    ///   `Some(Some(setter))` → invoke `setter`;
    ///   `Some(None)`         → a getter-only accessor shadows the write (no-op);
    ///   `None`               → no accessor shadows it → write a static data prop.
    fn lookup_static_accessor(&self, start: Option<u32>, key: &str) -> Option<Option<Value>> {
        let mut cur = start;
        while let Some(cidx) = cur {
            match self.heap.get(cidx) {
                HeapObj::Class(c) => {
                    if let Some((_, s)) = c.static_setters.iter().find(|(k, _)| k == key) {
                        return Some(Some(*s));
                    }
                    if c.static_getters.iter().any(|(k, _)| k == key) {
                        return Some(None); // accessor with no setter ⇒ sloppy no-op
                    }
                    if c.statics.get(key).is_some() {
                        return None; // own data property shadows inherited accessors
                    }
                    cur = c.parent;
                }
                _ => break,
            }
        }
        None
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
        let heapbuf: Vec<Value>;
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
        self.dispatch_builtin_method(recv, name, args)
    }

    /// Dispatch a builtin method on `recv` with an already-materialized args
    /// slice. Shared by `try_builtin_method` (args gathered from registers) and
    /// the spread method-call path (args taken from an array). `Ok(None)` means
    /// no builtin matched the receiver kind.
    fn dispatch_builtin_method(
        &mut self,
        recv: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        // Number receivers (Int or double) support a small method set.
        if recv.is_number() {
            return self.number_method(recv, name, args);
        }
        if !recv.is_heap() {
            return Ok(None);
        }
        let idx = recv.heap_index();
        // ── Function.prototype.call / apply / bind (callable receivers) ──
        if self.is_callable(recv) {
            match name {
                "call" => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let rest: &[Value] = if args.len() > 1 { &args[1..] } else { &[] };
                    return Ok(Some(self.call_value(recv, this, rest)?));
                }
                "apply" => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let arr = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                    let callargs = if arr.is_heap() { self.iterate_to_vec(arr)? } else { Vec::new() };
                    return Ok(Some(self.call_value(recv, this, &callargs)?));
                }
                "bind" => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let bound: Vec<Value> = if args.len() > 1 { args[1..].to_vec() } else { Vec::new() };
                    let b = self.heap.alloc(HeapObj::Bound { target: recv, this, args: bound });
                    return Ok(Some(Value::heap(b)));
                }
                _ => {}
            }
        }
        // ── Object.prototype methods (available on every object) ──
        match name {
            "hasOwnProperty" => {
                let key = self.display(args.first().copied().unwrap_or(Value::UNDEFINED));
                return Ok(Some(Value::bool(self.has_own_property(recv, &key))));
            }
            "propertyIsEnumerable" => {
                let key = self.display(args.first().copied().unwrap_or(Value::UNDEFINED));
                return Ok(Some(Value::bool(self.own_is_enumerable(recv, &key))));
            }
            "isPrototypeOf" => {
                let target = args.first().copied().unwrap_or(Value::UNDEFINED);
                return Ok(Some(Value::bool(self.is_prototype_of(recv, target))));
            }
            "valueOf" => return Ok(Some(recv)), // default valueOf returns the object
            "toString" => {
                // Generic `Object.prototype.toString` for a plain object; arrays /
                // numbers / dates etc. have their own toString in the type dispatch.
                if matches!(self.heap.get(idx), HeapObj::Object(_)) {
                    return Ok(Some(self.alloc_str("[object Object]".to_string())));
                }
            }
            _ => {}
        }
        match self.heap.get(idx) {
            HeapObj::Array(_) => self.array_method(idx, name, args),
            HeapObj::Str(_) | HeapObj::Cons { .. } => self.string_method(idx, name, args),
            HeapObj::Map { .. } => self.map_method(idx, name, args),
            HeapObj::Set(_) => self.set_method(idx, name, args),
            HeapObj::Generator { .. } => self.generator_method(idx, name, args),
            HeapObj::Promise { .. } => self.promise_method(idx, name, args),
            HeapObj::Date(_) => self.date_method(idx, name, args),
            _ => Ok(None),
        }
    }

    /// `Promise.prototype.then/catch/finally`. Returns a NEW dependent promise.
    /// All handlers run as microtasks (never synchronously). `idx` is the
    /// receiver promise's heap index.
    fn promise_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "then" => {
                let on_r = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let dep = self.then_internal(idx, a0, on_r, None);
                Ok(Some(Value::heap(dep)))
            }
            "catch" => {
                let dep = self.then_internal(idx, Value::UNDEFINED, a0, None);
                Ok(Some(Value::heap(dep)))
            }
            "finally" => {
                // `cb` runs (no args) on both settle paths; the original value /
                // reason forwards (FinallyReaction handles the value pass-through).
                let dep = self.finally_internal(idx, a0);
                Ok(Some(Value::heap(dep)))
            }
            _ => Ok(None),
        }
    }

    /// `Map.prototype.*`. `idx` is the Map's heap index. Returns `Ok(None)` for an
    /// unknown method (→ TypeError at the call site). `forEach` snapshots the
    /// entries before invoking the callback (which may mutate the map).
    fn map_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "get" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Map { keys, vals } => keys
                        .iter()
                        .position(|k| self.same_value_zero(*k, a0))
                        .map(|i| vals[i]),
                    _ => None,
                };
                Ok(Some(v.unwrap_or(Value::UNDEFINED)))
            }
            "has" => {
                let found = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.iter().any(|k| self.same_value_zero(*k, a0)),
                    _ => false,
                };
                Ok(Some(Value::bool(found)))
            }
            "set" => {
                let key = normalize_zero(a0);
                let val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let pos = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.iter().position(|k| self.same_value_zero(*k, key)),
                    _ => None,
                };
                if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                    match pos {
                        Some(i) => vals[i] = val, // update in place, keep position
                        None => {
                            keys.push(key);
                            vals.push(val);
                        }
                    }
                }
                Ok(Some(recv)) // chainable
            }
            "delete" => {
                let pos = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.iter().position(|k| self.same_value_zero(*k, a0)),
                    _ => None,
                };
                if let (Some(i), HeapObj::Map { keys, vals }) = (pos, self.heap.get_mut(idx)) {
                    keys.remove(i);
                    vals.remove(i);
                    return Ok(Some(Value::bool(true)));
                }
                Ok(Some(Value::bool(false)))
            }
            "clear" => {
                if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                    keys.clear();
                    vals.clear();
                }
                Ok(Some(Value::UNDEFINED))
            }
            "forEach" => {
                let cb = a0;
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (ks, vs) = match self.heap.get(idx) {
                    HeapObj::Map { keys, vals } => (keys.clone(), vals.clone()),
                    _ => (Vec::new(), Vec::new()),
                };
                for (k, v) in ks.into_iter().zip(vs) {
                    // callback(value, key, map)
                    self.call_value(cb, this_arg, &[v, k, recv])?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            // Iterators are approximated as arrays (iterable / spreadable alike).
            "keys" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.clone(),
                    _ => Vec::new(),
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(v)))))
            }
            "values" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Map { vals, .. } => vals.clone(),
                    _ => Vec::new(),
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(v)))))
            }
            "entries" => {
                let pairs: Vec<(Value, Value)> = match self.heap.get(idx) {
                    HeapObj::Map { keys, vals } => {
                        keys.iter().copied().zip(vals.iter().copied()).collect()
                    }
                    _ => Vec::new(),
                };
                let entries: Vec<Value> = pairs
                    .into_iter()
                    .map(|(k, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))))
                    .collect();
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(entries)))))
            }
            _ => Ok(None),
        }
    }

    /// `Set.prototype.*`. `idx` is the Set's heap index. `keys`/`values`/`entries`
    /// return arrays (the iterator approximation).
    fn set_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "has" => {
                let found = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.iter().any(|v| self.same_value_zero(*v, a0)),
                    _ => false,
                };
                Ok(Some(Value::bool(found)))
            }
            "add" => {
                let val = normalize_zero(a0);
                let present = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.iter().any(|v| self.same_value_zero(*v, val)),
                    _ => true,
                };
                if !present {
                    if let HeapObj::Set(items) = self.heap.get_mut(idx) {
                        items.push(val);
                    }
                }
                Ok(Some(recv)) // chainable
            }
            "delete" => {
                let pos = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.iter().position(|v| self.same_value_zero(*v, a0)),
                    _ => None,
                };
                if let (Some(i), HeapObj::Set(items)) = (pos, self.heap.get_mut(idx)) {
                    items.remove(i);
                    return Ok(Some(Value::bool(true)));
                }
                Ok(Some(Value::bool(false)))
            }
            "clear" => {
                if let HeapObj::Set(items) = self.heap.get_mut(idx) {
                    items.clear();
                }
                Ok(Some(Value::UNDEFINED))
            }
            "forEach" => {
                let cb = a0;
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let items = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                for v in items {
                    // callback(value, value, set) — value passed twice, mirroring Map.
                    self.call_value(cb, this_arg, &[v, v, recv])?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            // keys() === values() for a Set; both yield the values.
            "keys" | "values" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(v)))))
            }
            "entries" => {
                let items = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                let entries: Vec<Value> = items
                    .into_iter()
                    .map(|v| Value::heap(self.heap.alloc(HeapObj::Array(vec![v, v]))))
                    .collect();
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(entries)))))
            }
            _ => Ok(None),
        }
    }

    /// `key in obj` — does `obj` have the property `key`? Own object keys, a
    /// class instance's inherited methods/getters, array indices / `length`,
    /// Map/Set `size`, and class static members. `in` on a primitive throws
    /// in JS; here it's `false` (rare).
    fn has_property(&self, obj: Value, key: Value) -> bool {
        if !obj.is_heap() {
            return false;
        }
        let idx = obj.heap_index();
        match self.heap.get(idx) {
            HeapObj::Object(map) => {
                let k = self.display(key);
                if map.get(&k).is_some() {
                    return true;
                }
                // Inherited method/getter through the class chain.
                let mut cur = map.class;
                while let Some(cidx) = cur {
                    match self.heap.get(cidx) {
                        HeapObj::Class(c) => {
                            if c.methods.iter().any(|(n, _)| *n == k)
                                || c.getters.iter().any(|(n, _)| *n == k)
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
            HeapObj::Array(items) => match array_index(key) {
                Some(i) => i < items.len(),
                None => self.display(key) == "length",
            },
            HeapObj::Str(s) => match array_index(key) {
                Some(i) => i < s.char_len,
                None => self.display(key) == "length",
            },
            HeapObj::Cons { len, .. } => match array_index(key) {
                Some(i) => i < *len,
                None => self.display(key) == "length",
            },
            HeapObj::Map { .. } | HeapObj::Set(_) => self.display(key) == "size",
            // Static members (data + `static get`/`set` accessors) are own
            // properties of the class value and are inherited up the chain.
            HeapObj::Class(_) => {
                let k = self.display(key);
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
            _ => false,
        }
    }

    /// `val instanceof <built-in ctor>`. With no user prototype chain the result
    /// is structural: by heap kind for Array/Object/Function, and by the `name`
    /// field for the Error family (any error subtype satisfies `instanceof
    /// Error`). Primitives are never an instance of anything.
    fn eval_instanceof(&self, val: Value, ctor: InstanceCtor) -> bool {
        use InstanceCtor as C;
        if !val.is_heap() {
            return false;
        }
        let idx = val.heap_index();
        match ctor {
            C::Array => matches!(self.heap.get(idx), HeapObj::Array(_)),
            C::Function => {
                matches!(self.heap.get(idx), HeapObj::Func(_) | HeapObj::Closure { .. })
            }
            // Every non-primitive (array, object, function, error) is an Object.
            C::Object => matches!(
                self.heap.get(idx),
                HeapObj::Array(_) | HeapObj::Object(_) | HeapObj::Func(_) | HeapObj::Closure { .. }
            ),
            C::Error => self.error_name(idx).is_some(),
            C::TypeError => self.error_name(idx).as_deref() == Some("TypeError"),
            C::RangeError => self.error_name(idx).as_deref() == Some("RangeError"),
            C::SyntaxError => self.error_name(idx).as_deref() == Some("SyntaxError"),
        }
    }

    /// Build an Error object from an internal throw message. A message like
    /// `"TypeError: cannot read …"` splits into `name="TypeError"` and
    /// `message="cannot read …"`; anything else becomes a generic `Error` whose
    /// message is the whole text. Mirrors the `{name, message}` shape the
    /// compiler emits for `new TypeError(…)`, so both catch paths are uniform.
    fn alloc_error_from_message(&mut self, raw: &str) -> Value {
        let (name, message) = match raw.split_once(": ") {
            Some((pre, rest))
                if matches!(pre, "Error" | "TypeError" | "RangeError" | "SyntaxError") =>
            {
                (pre.to_string(), rest.to_string())
            }
            _ => ("Error".to_string(), raw.to_string()),
        };
        let name_v = self.alloc_str(name);
        let msg_v = self.alloc_str(message);
        let mut map = ObjMap::new();
        map.set("name", name_v);
        map.set("message", msg_v);
        Value::heap(self.heap.alloc(HeapObj::Object(map)))
    }

    /// `new <class>(args)`: build a plain object, install the class's methods as
    /// own Func properties, then run the constructor (if any) with `this` = the
    /// new object. A constructor that returns an object/array replaces the
    /// instance (JS semantics); otherwise the instance is returned.
    fn construct(&mut self, cv: Value, args: &[Value]) -> Result<Value, Thrown> {
        if !cv.is_heap() {
            return Err(Thrown("TypeError: value is not a constructor".into()));
        }
        // Constructor FUNCTION (`new F()`, the pre-class OOP idiom): make an object
        // whose [[Prototype]] is `F.prototype` (so its methods + `constructor`
        // resolve), run `F` with `this` = that object, and use F's return value if
        // it returns an object (else the new object).
        if matches!(
            self.heap.get(cv.heap_index()),
            HeapObj::Func(_) | HeapObj::Closure { .. }
        ) {
            let proto = self.prototype_of(cv).unwrap_or(Value::UNDEFINED);
            let obj = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
            if proto.is_heap() {
                self.proto_of.insert(obj.heap_index(), proto);
            }
            let ret = self.call_value(cv, obj, args)?;
            if ret.is_heap()
                && matches!(self.heap.get(ret.heap_index()), HeapObj::Object(_) | HeapObj::Array(_))
            {
                return Ok(ret);
            }
            return Ok(obj);
        }
        let (ctor, has_explicit, parent) = match self.heap.get(cv.heap_index()) {
            HeapObj::Class(c) => (c.ctor, c.has_explicit_ctor, c.parent),
            _ => return Err(Thrown("TypeError: value is not a constructor".into())),
        };
        // The instance links to its class for method lookup + instanceof; its own
        // keys hold only the fields (so enumeration / JSON stay method-free).
        let mut map = ObjMap::new();
        map.class = Some(cv.heap_index());
        let obj = Value::heap(self.heap.alloc(HeapObj::Object(map)));
        if has_explicit {
            // The explicit constructor runs its own `super(...)`; a ctor that
            // returns an object/array replaces the instance.
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                let ret = self.call_value(f, obj, args)?;
                if ret.is_heap()
                    && matches!(self.heap.get(ret.heap_index()), HeapObj::Object(_) | HeapObj::Array(_))
                {
                    return Ok(ret);
                }
            }
        } else {
            // No own constructor: run the parent's ctor (implicit `super(...args)`)
            // then this class's field initializers.
            if let Some(pidx) = parent {
                self.run_class_ctor(Value::heap(pidx), obj, args)?;
            }
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                self.call_value(f, obj, &[])?;
            }
        }
        Ok(obj)
    }

    /// `v instanceof F` for a constructor FUNCTION `F`: true iff `F.prototype` is
    /// somewhere in `v`'s prototype chain.
    fn instanceof_via_proto(&mut self, v: Value, ctor: Value) -> bool {
        let target = match self.prototype_of(ctor) {
            Some(p) => p,
            None => return false,
        };
        let mut cur = self.object_get_prototype_of(v);
        for _ in 0..10_000 {
            if !cur.is_heap() {
                return false;
            }
            if cur == target {
                return true;
            }
            cur = self.object_get_prototype_of(cur);
        }
        false
    }

    /// True iff `v` is an object whose class chain includes the class at heap
    /// index `class_idx` (`v instanceof C`, walking `extends` links).
    fn instance_of_class(&self, v: Value, class_idx: u32) -> bool {
        if !v.is_heap() {
            return false;
        }
        let mut cur = match self.heap.get(v.heap_index()) {
            HeapObj::Object(m) => m.class,
            _ => None,
        };
        while let Some(cidx) = cur {
            if cidx == class_idx {
                return true;
            }
            cur = match self.heap.get(cidx) {
                HeapObj::Class(c) => c.parent,
                _ => None,
            };
        }
        false
    }

    /// Run a class's constructor contribution on an existing instance `obj` —
    /// for `super(...)` and the implicit-super chain. An explicit ctor runs its
    /// own `super`; an implicit one runs the parent chain then its fields.
    fn run_class_ctor(&mut self, cval: Value, obj: Value, args: &[Value]) -> Result<(), Thrown> {
        if !cval.is_heap() {
            return Ok(());
        }
        let (ctor, has_explicit, parent) = match self.heap.get(cval.heap_index()) {
            HeapObj::Class(c) => (c.ctor, c.has_explicit_ctor, c.parent),
            _ => return Ok(()),
        };
        if has_explicit {
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                self.call_value(f, obj, args)?;
            }
        } else {
            if let Some(pidx) = parent {
                self.run_class_ctor(Value::heap(pidx), obj, args)?;
            }
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                self.call_value(f, obj, &[])?;
            }
        }
        Ok(())
    }

    /// `Object.assign(target, ...sources)`: copy each source's own enumerable
    /// keys (object keys, or an array's index strings) onto `target`; returns
    /// `target`. Primitive (incl. null/undefined) sources contribute nothing.
    fn object_assign(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let target = args.first().copied().unwrap_or(Value::UNDEFINED);
        if !target.is_heap() || !matches!(self.heap.get(target.heap_index()), HeapObj::Object(_)) {
            return Err(Thrown("TypeError: Object.assign target must be an object".into()));
        }
        let tidx = target.heap_index();
        let mut added_any = false;
        for &src in &args[1..] {
            if !src.is_heap() {
                continue;
            }
            // Gather (key, val) pairs under the immutable borrow, then write.
            // (A string source spreads as index→char, like an array.)
            let str_chars: Option<Vec<char>> = match self.heap.get(src.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => {
                    Some(self.heap.str_cow(src.heap_index()).unwrap().chars().collect())
                }
                _ => None,
            };
            let pairs: Vec<(String, Value)> = if let Some(chars) = str_chars {
                chars
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| (i.to_string(), self.alloc_str(c.to_string())))
                    .collect()
            } else {
                match self.heap.get(src.heap_index()) {
                    HeapObj::Object(map) => {
                        map.keys.iter().cloned().zip(map.vals.iter().copied()).collect()
                    }
                    HeapObj::Array(items) => {
                        items.iter().enumerate().map(|(i, &v)| (i.to_string(), v)).collect()
                    }
                    _ => Vec::new(),
                }
            };
            for (k, v) in pairs {
                if let HeapObj::Object(map) = self.heap.get_mut(tidx) {
                    added_any |= map.set(&k, v);
                }
            }
        }
        if added_any {
            self.heap.bump_version(tidx);
        }
        Ok(target)
    }

    /// `Array.from(src[, mapFn])`: build an array from an array, a string's
    /// chars, or an array-like (`{length, 0:…}`), applying `mapFn(value, index)`
    /// when it is a function.
    /// Materialize a value's iteration elements: an array or set → its items, a
    /// string → its chars (as 1-char strings), a map → fresh `[key, value]` entry
    /// arrays. Throws a TypeError for a non-iterable. Allocations happen after the
    /// heap borrow is released (two phases).
    /// Whether `v` is a user-callable value (function or closure).
    fn is_callable(&self, v: Value) -> bool {
        v.is_heap()
            && matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
            )
    }

    /// `obj.hasOwnProperty(key)` — own data/accessor property, array index/length,
    /// or string index/length.
    fn has_own_property(&self, obj: Value, key: &str) -> bool {
        if !obj.is_heap() {
            return false;
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => m.pos(key).is_some(),
            HeapObj::Array(items) => {
                key == "length" || key.parse::<usize>().map_or(false, |i| i < items.len())
            }
            HeapObj::Str(s) => {
                key == "length" || key.parse::<usize>().map_or(false, |i| i < s.char_len)
            }
            HeapObj::Cons { len, .. } => {
                key == "length" || key.parse::<usize>().map_or(false, |i| i < *len)
            }
            // A class value: own statics (data + `static get`/`set`) + name/length.
            HeapObj::Class(c) => {
                c.statics.pos(key).is_some()
                    || c.static_getters.iter().any(|(n, _)| n == key)
                    || c.static_setters.iter().any(|(n, _)| n == key)
                    || self.callable_has_intrinsic(obj, key)
            }
            // Functions/closures/etc.: assigned own props (`fn.x`) + name/length.
            _ => {
                self.fn_props.get(&obj.heap_index()).map_or(false, |m| m.pos(key).is_some())
                    || self.callable_has_intrinsic(obj, key)
            }
        }
    }

    /// `obj.propertyIsEnumerable(key)` — true if `key` is an own enumerable
    /// property. Array indices are enumerable; `length` is not.
    fn own_is_enumerable(&self, obj: Value, key: &str) -> bool {
        if !obj.is_heap() {
            return false;
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => m.pos(key).map_or(false, |i| m.attrs[i].enumerable),
            HeapObj::Array(items) => key.parse::<usize>().map_or(false, |i| i < items.len()),
            _ => false,
        }
    }

    /// `proto.isPrototypeOf(obj)` — is `proto` anywhere in `obj`'s prototype chain?
    fn is_prototype_of(&mut self, proto: Value, obj: Value) -> bool {
        let mut cur = self.object_get_prototype_of(obj);
        for _ in 0..10_000 {
            if !cur.is_heap() {
                return false;
            }
            if cur == proto {
                return true;
            }
            cur = self.object_get_prototype_of(cur);
        }
        false
    }

    /// Resolve an iterable's iterator: a plain object with a `@@iterator` method
    /// (a custom iterable) yields `obj[@@iterator]()`; everything else (arrays,
    /// strings, Map/Set, generators) iterates directly and passes through.
    fn get_iterator(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
            let m = self.get_prop(v, "@@iterator")?;
            if self.is_callable(m) {
                return self.call_value(m, v, &[]);
            }
        }
        Ok(v)
    }

    /// Normalize a destructuring source to a positionally-indexable value: a
    /// generator or a custom iterable (object with `@@iterator`) is drained into a
    /// fresh array — LAZILY, at most `max` elements (so `let [a,b] = infinite`
    /// pulls 2, not forever); everything else (arrays/strings/Map/Set, or a
    /// non-iterable) passes through unchanged.
    fn iter_to_array(&mut self, v: Value, max: u32) -> Result<Value, Thrown> {
        if !v.is_heap() {
            return Ok(v);
        }
        let drain = match self.heap.get(v.heap_index()) {
            HeapObj::Generator { .. } => true,
            HeapObj::Object(_) => {
                let it = self.get_prop(v, "@@iterator")?;
                self.is_callable(it)
            }
            _ => false,
        };
        if !drain {
            return Ok(v);
        }
        let iter = self.get_iterator(v)?; // generator → itself; iterable → its iterator
        let lim = max as usize;
        let mut out = Vec::new();
        while out.len() < lim {
            let res = if matches!(self.heap.get(iter.heap_index()), HeapObj::Generator { .. }) {
                self.generator_method(iter.heap_index(), "next", &[])?
                    .unwrap_or(Value::UNDEFINED)
            } else {
                let next = self.get_prop(iter, "next")?;
                if !self.is_callable(next) {
                    break;
                }
                self.call_value(next, iter, &[])?
            };
            let done = self.get_prop(res, "done")?;
            if self.truthy(done) {
                break;
            }
            out.push(self.get_prop(res, "value")?);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

    fn iterate_to_vec(&mut self, v: Value) -> Result<Vec<Value>, Thrown> {
        let v = self.get_iterator(v)?;
        // A generator is drained eagerly via repeated next() (spread / Array.from
        // produce a buffer; an infinite generator hangs here, matching V8).
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Generator { .. }) {
            let gidx = v.heap_index();
            let mut out = Vec::new();
            loop {
                let res = self
                    .generator_method(gidx, "next", &[])?
                    .unwrap_or(Value::UNDEFINED);
                let done = self.get_prop(res, "done")?;
                if self.truthy(done) {
                    break;
                }
                out.push(self.get_prop(res, "value")?);
            }
            return Ok(out);
        }
        // A user iterator object (one with a `next()` method): drain it.
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
            let next = self.get_prop(v, "next")?;
            if self.is_callable(next) {
                let mut out = Vec::new();
                loop {
                    let res = self.call_value(next, v, &[])?;
                    let done = self.get_prop(res, "done")?;
                    if self.truthy(done) {
                        break;
                    }
                    out.push(self.get_prop(res, "value")?);
                }
                return Ok(out);
            }
        }
        enum Plan {
            Vals(Vec<Value>),
            Chars(Vec<char>),
            Pairs(Vec<(Value, Value)>),
        }
        let plan = if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Array(items) => Plan::Vals(items.clone()),
                HeapObj::Set(items) => Plan::Vals(items.clone()),
                HeapObj::Str(_) | HeapObj::Cons { .. } => {
                    Plan::Chars(self.heap.str_cow(v.heap_index()).unwrap().chars().collect())
                }
                HeapObj::Map { keys, vals } => {
                    Plan::Pairs(keys.iter().copied().zip(vals.iter().copied()).collect())
                }
                _ => return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v)))),
            }
        } else {
            return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))));
        };
        Ok(match plan {
            Plan::Vals(v) => v,
            Plan::Chars(cs) => cs.into_iter().map(|c| self.alloc_str(c.to_string())).collect(),
            Plan::Pairs(ps) => ps
                .into_iter()
                .map(|(k, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))))
                .collect(),
        })
    }

    fn array_from(&mut self, src: Value, mapfn: Value) -> Result<Value, Thrown> {
        // Classify the source under a short-lived borrow, then materialize its
        // elements (the object/array-like path needs &mut self for get_prop).
        enum Kind {
            Iterable,
            Obj,
            Other,
        }
        let mut elems: Vec<Value> = Vec::new();
        let kind = if src.is_heap() {
            match self.heap.get(src.heap_index()) {
                HeapObj::Array(_)
                | HeapObj::Str(_)
                | HeapObj::Cons { .. }
                | HeapObj::Set(_)
                | HeapObj::Map { .. }
                | HeapObj::Generator { .. } => Kind::Iterable,
                HeapObj::Object(_) => Kind::Obj,
                _ => Kind::Other,
            }
        } else {
            Kind::Other
        };
        match kind {
            Kind::Iterable => elems = self.iterate_to_vec(src)?,
            Kind::Obj => {
                // A custom iterable object (`@@iterator`) → iterate it; otherwise
                // treat it as array-like (read `length`, then indices 0..length).
                let it = self.get_prop(src, "@@iterator")?;
                if self.is_callable(it) {
                    elems = self.iterate_to_vec(src)?;
                } else {
                    let len = self.get_prop(src, "length")?;
                    let n = if len.is_number() && len.as_f64() >= 0.0 {
                        len.as_f64() as usize
                    } else {
                        0
                    };
                    for i in 0..n {
                        elems.push(self.get_index(src, Value::int(i as i32))?);
                    }
                }
            }
            Kind::Other => {}
        }
        // Apply the map callback, if given.
        let has_map = mapfn.is_heap()
            && matches!(
                self.heap.get(mapfn.heap_index()),
                HeapObj::Func(_) | HeapObj::Closure { .. }
            );
        if has_map {
            for (i, slot) in elems.iter_mut().enumerate() {
                let args = [*slot, Value::int(i as i32)];
                *slot = self.call_value(mapfn, Value::UNDEFINED, &args)?;
            }
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(elems))))
    }

    /// If `idx` is an Error-like object — an object whose `name` is one of the
    /// engine's error kinds — return that name, else `None`.
    fn error_name(&self, idx: u32) -> Option<String> {
        let map = match self.heap.get(idx) {
            HeapObj::Object(m) => m,
            _ => return None,
        };
        let nv = map.get("name")?;
        let name = self.display(nv);
        matches!(name.as_str(), "Error" | "TypeError" | "RangeError" | "SyntaxError")
            .then_some(name)
    }

    /// Methods on a number receiver: `toFixed`, `toString`. Returns `Ok(None)`
    /// for an unrecognised name (the caller then treats it as a missing property
    /// → TypeError, matching JS).
    fn number_method(&mut self, recv: Value, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let n = recv.as_f64();
        match name {
            "toFixed" => {
                let digits = args.first().map(|a| a.as_f64() as usize).unwrap_or(0).min(100);
                Ok(Some(self.alloc_str(to_fixed(n, digits))))
            }
            "toString" => {
                let radix = args.first().map(|a| a.as_f64() as u32).unwrap_or(10);
                // Base 10 (or a default/out-of-range radix) uses the engine's
                // canonical rendering; 2..=36 do an integer-radix conversion.
                if radix == 10 || !(2..=36).contains(&radix) {
                    Ok(Some(self.alloc_str(self.display(recv))))
                } else {
                    Ok(Some(self.alloc_str(num_to_radix(n, radix))))
                }
            }
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
            self.jit.compile(fid, proto_ref, jit_self_call_at as usize, self_val);
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

        // Fused native map kernel: inline the callback into a native loop over
        // the snapshot for the leading run of integer elements — eliminating the
        // per-element call boundary (the gap to V8, which inlines callbacks). Map
        // only (dense, ordered store). On a type-guard bail the kernel returns
        // the index it reached, having written results `[0, start)`; the
        // per-element loop below finishes `[start, len)` correctly (handling
        // doubles/strings/etc.), so a mixed array can never give a wrong answer.
        let mut start = 0usize;
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if matches!(mode, EachMode::Map)
            && self.jit_enabled
            && self.jit_recurse_depth == 0
            && cb.is_heap()
            && snapshot.len() <= i32::MAX as usize
        {
            if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                if ups.is_empty() {
                    let proto: *const crate::bytecode::FuncProto =
                        &self.program.functions[fid as usize];
                    // SAFETY: program functions are immutable during execution;
                    // the raw ptr dodges the self.jit (&mut) vs self.program (&)
                    // borrow conflict (same pattern as native_cb_entry).
                    let proto_ref = unsafe { &*proto };
                    let min_window = if proto_ref.param_count >= 2 { 3 } else { 2 };
                    let reg_count = (proto_ref.reg_count as usize).max(min_window);
                    if let Some(entry) = self.jit.map_kernel(fid, proto_ref) {
                        let win = self.regs.len();
                        if !self.regs_would_overflow(win + reg_count) {
                            self.regs.resize(win + reg_count, Value::UNDEFINED);
                            let len = snapshot.len();
                            let window_ptr =
                                unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
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
            && self.jit_enabled
            && self.jit_recurse_depth == 0
            && cb.is_heap()
            && snapshot.len() <= i32::MAX as usize
        {
            if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                if ups.is_empty() {
                    let proto: *const crate::bytecode::FuncProto =
                        &self.program.functions[fid as usize];
                    // SAFETY: as the map branch above.
                    let proto_ref = unsafe { &*proto };
                    let min_window = if proto_ref.param_count >= 2 { 3 } else { 2 };
                    let reg_count = (proto_ref.reg_count as usize).max(min_window);
                    if let Some(entry) = self.jit.filter_kernel(fid, proto_ref) {
                        let win = self.regs.len();
                        if !self.regs_would_overflow(win + reg_count) {
                            self.regs.resize(win + reg_count, Value::UNDEFINED);
                            let len = snapshot.len();
                            let window_ptr =
                                unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
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
        let run_tail = start < snapshot.len();
        let mut native = if run_tail { self.native_cb_entry(cb) } else { None };
        let win = self.regs.len();
        if let Some((_, callee_regs, _)) = native {
            if self.regs_would_overflow(win + callee_regs) {
                native = None; // can't fit a window → interpreter path
            } else {
                self.regs.resize(win + callee_regs, Value::UNDEFINED);
            }
        }

        let mut err = None;
        for i in start..snapshot.len() {
            let v = snapshot[i];
            let args = [v, Value::int(i as i32)];
            match self.run_cb_elem(native, win, cb, &args) {
                Ok(r) => match mode {
                    EachMode::Map => out.push(r),
                    EachMode::Filter => {
                        if self.truthy(r) {
                            out.push(v);
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
            // `Array.prototype.toString()` is `join()` with the default "," sep.
            "join" | "toString" => {
                let sep = if name == "toString" || args.is_empty() {
                    ",".to_string()
                } else {
                    self.display(arg0)
                };
                let snapshot = self.array_snapshot(idx);
                let parts: Vec<String> = snapshot
                    .iter()
                    .map(|v| if v.is_nullish() { String::new() } else { self.display(*v) })
                    .collect();
                Ok(Some(self.alloc_str(parts.join(&sep))))
            }
            "at" => {
                // Negative index counts from the end; out of range → undefined.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let i = arg0.is_number().then(|| arg0.as_f64()).unwrap_or(0.0) as i64;
                let abs = if i < 0 { i + len as i64 } else { i };
                let v = if abs >= 0 && (abs as usize) < len {
                    match self.heap.get(idx) {
                        HeapObj::Array(items) => items[abs as usize],
                        _ => Value::UNDEFINED,
                    }
                } else {
                    Value::UNDEFINED
                };
                Ok(Some(v))
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
                let has_init = args.len() >= 2;
                // Seed + first index to process: with an initial value, start at
                // element 0; otherwise the first element seeds and we start at 1.
                let mut start = if has_init { 0 } else { 1 };
                let mut acc = if has_init {
                    args[1]
                } else if !snapshot.is_empty() {
                    snapshot[0]
                } else {
                    return Err(Thrown(
                        "TypeError: Reduce of empty array with no initial value".into(),
                    ));
                };

                // Fused native reduce kernel: inline the `(acc, element)`
                // callback into a native loop over the leading numeric run — no
                // per-element call. On a guard bail it returns the index reached
                // and the accumulated value (via the in/out acc pointer); the
                // per-element tail below finishes `[start, len)` correctly.
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                if self.jit_enabled
                    && self.jit_recurse_depth == 0
                    && cb.is_heap()
                    && start < snapshot.len()
                {
                    if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                        if ups.is_empty() {
                            let proto: *const crate::bytecode::FuncProto =
                                &self.program.functions[fid as usize];
                            // SAFETY: immutable program functions; raw ptr dodges
                            // the jit-vs-program borrow conflict (as elsewhere).
                            let proto_ref = unsafe { &*proto };
                            let reg_count = (proto_ref.reg_count as usize).max(3);
                            if let Some(entry) = self.jit.reduce_kernel(fid, proto_ref) {
                                let win = self.regs.len();
                                if !self.regs_would_overflow(win + reg_count) {
                                    self.regs.resize(win + reg_count, Value::UNDEFINED);
                                    let count = snapshot.len() - start;
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
                                    ) -> usize = unsafe { core::mem::transmute(entry) };
                                    let processed =
                                        kernel(window_ptr, snap_ptr, count, &mut acc_bits as *mut u64);
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
                let run_tail = start < snapshot.len();
                let mut native = if run_tail { self.native_cb_entry(cb) } else { None };
                let win = self.regs.len();
                if let Some((_, callee_regs, _)) = native {
                    if self.regs_would_overflow(win + callee_regs) {
                        native = None;
                    } else {
                        self.regs.resize(win + callee_regs, Value::UNDEFINED);
                    }
                }
                let mut err = None;
                for i in start..snapshot.len() {
                    let cbargs = [acc, snapshot[i], Value::int(i as i32)];
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
            "reduceRight" => {
                let cb = arg0;
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
                while i > 0 {
                    i -= 1;
                    acc = self.call_value(cb, Value::UNDEFINED, &[acc, snapshot[i], Value::int(i as i32)])?;
                }
                Ok(Some(acc))
            }
            "flatMap" => {
                // map(cb) then flatten one level (array results spliced in).
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                let mut out: Vec<Value> = Vec::new();
                for (i, v) in snapshot.iter().enumerate() {
                    let r = self.call_value(cb, Value::UNDEFINED, &[*v, Value::int(i as i32)])?;
                    if r.is_heap() {
                        if let HeapObj::Array(items) = self.heap.get(r.heap_index()) {
                            out.extend(items.iter().copied());
                            continue;
                        }
                    }
                    out.push(r);
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "findLast" | "findLastIndex" => {
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                for i in (0..snapshot.len()).rev() {
                    let v = snapshot[i];
                    let r = self.call_value(cb, Value::UNDEFINED, &[v, Value::int(i as i32)])?;
                    if self.truthy(r) {
                        return Ok(Some(if name == "findLast" {
                            v
                        } else {
                            Value::int(i as i32)
                        }));
                    }
                }
                Ok(Some(if name == "findLast" { Value::UNDEFINED } else { Value::int(-1) }))
            }
            "toSorted" => {
                // Like sort() but returns a NEW array; the receiver is unchanged.
                let cmp = arg0;
                let mut snapshot = self.array_snapshot(idx);
                if cmp.is_heap() && self.heap.as_callable(cmp.heap_index()).is_some() {
                    self.comparator_sort(&mut snapshot, cmp)?;
                } else {
                    snapshot.sort_by(|a, b| self.display(*a).cmp(&self.display(*b)));
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(snapshot)))))
            }
            "toReversed" => {
                let mut snapshot = self.array_snapshot(idx);
                snapshot.reverse();
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(snapshot)))))
            }
            "splice" => {
                // splice(start, deleteCount?, ...items): mutate in place, return
                // the removed elements (start may be negative).
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let s = if arg0.is_number() { arg0.as_f64() as i64 } else { 0 };
                let start = if s < 0 { (len as i64 + s).max(0) as usize } else { (s as usize).min(len) };
                let del = if args.len() < 2 {
                    len - start
                } else {
                    let d = if args[1].is_number() { args[1].as_f64() as i64 } else { 0 };
                    (d.max(0) as usize).min(len - start)
                };
                let insert: Vec<Value> = args.get(2..).unwrap_or(&[]).to_vec();
                let removed: Vec<Value> = match self.heap.get_mut(idx) {
                    HeapObj::Array(items) => items.splice(start..start + del, insert).collect(),
                    _ => Vec::new(),
                };
                self.heap.bump_version(idx); // length/contents changed
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(removed)))))
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
        // that re-enters the VM and allocates can't invalidate them).
        let mut a: Vec<Value> = items.to_vec();
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
                    let c = match self.run_cb_elem(native, win, cmp, &[a[l], a[r]]) {
                        Ok(c) => c,
                        Err(e) => {
                            err = Some(e);
                            break 'outer;
                        }
                    };
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
        if native.is_some() {
            self.regs.truncate(win); // release the reused window (success or error)
        }
        if let Some(e) = err {
            return Err(e);
        }
        items.copy_from_slice(&a);
        Ok(())
    }

    /// The i-th char of a flat string by heap index, WITHOUT cloning the string —
    /// O(1) for ASCII (i-th byte), else an O(i) scalar scan. `None` if out of range
    /// or not a flat string. (A full-string clone here would make `charCodeAt(i)`
    /// in a loop O(n²) in the string length — the real cost of these methods.)
    fn heap_char_at(&self, idx: u32, i: usize) -> Option<char> {
        match self.heap.get(idx) {
            HeapObj::Str(js) => {
                if js.ascii {
                    js.bytes.as_bytes().get(i).map(|&b| b as char)
                } else {
                    js.bytes.chars().nth(i)
                }
            }
            _ => None,
        }
    }

    /// Char length of a flat string by heap index — O(1) for ASCII.
    fn heap_char_len(&self, idx: u32) -> usize {
        match self.heap.get(idx) {
            HeapObj::Str(js) => {
                if js.ascii {
                    js.bytes.len()
                } else {
                    js.bytes.chars().count()
                }
            }
            _ => 0,
        }
    }

    fn string_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        self.heap.flatten(idx); // materialize a rope receiver before reading it
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // Single-char index methods: read one char directly from the heap with NO
        // full-string clone (the clone below is O(n), so these would be O(n²) in a
        // per-char loop — `s.charCodeAt(i)` scanning is a very common idiom).
        match name {
            "charCodeAt" => {
                let i = arg0.as_f64() as i32;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => Value::int(c as i32),
                    None => Value::num(f64::NAN),
                }));
            }
            "codePointAt" => {
                let i = arg0.as_f64() as i32;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => Value::int(c as i32),
                    None => Value::UNDEFINED,
                }));
            }
            "charAt" => {
                let i = arg0.as_f64() as i32;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(self.alloc_str(c.map(|c| c.to_string()).unwrap_or_default())));
            }
            "at" => {
                let len = self.heap_char_len(idx) as i64;
                let i = if arg0.is_number() { arg0.as_f64() as i64 } else { 0 };
                let abs = if i < 0 { i + len } else { i };
                let c = if abs >= 0 && abs < len { self.heap_char_at(idx, abs as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => self.alloc_str(c.to_string()),
                    None => Value::UNDEFINED,
                }));
            }
            _ => {}
        }
        // Other methods need an owned String (slice/replace/split/…).
        let (s, ascii) = match self.heap.get(idx) {
            HeapObj::Str(js) => (js.bytes.clone(), js.ascii),
            _ => return Ok(None),
        };
        let char_len = |s: &str| -> usize {
            if ascii {
                s.len()
            } else {
                s.chars().count()
            }
        };
        match name {
            "indexOf" => {
                let needle = self.display(arg0);
                // Optional fromIndex (a char position) to start searching at.
                let from = if args.len() >= 2 && args[1].is_number() {
                    args[1].as_f64().max(0.0) as usize
                } else {
                    0
                };
                let byte_from = s.char_indices().nth(from).map(|(b, _)| b).unwrap_or(s.len());
                let pos = s[byte_from..]
                    .find(&needle)
                    .map(|b| s[..byte_from + b].chars().count() as i32)
                    .unwrap_or(-1);
                Ok(Some(Value::int(pos)))
            }
            "includes" => {
                let needle = self.display(arg0);
                Ok(Some(Value::bool(s.contains(&needle))))
            }
            "toUpperCase" => Ok(Some(self.alloc_str(s.to_uppercase()))),
            "toLowerCase" => Ok(Some(self.alloc_str(s.to_lowercase()))),
            "slice" | "substring" => {
                let len = char_len(&s) as i32;
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
                let cur = char_len(&s);
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
    /// SameValueZero — Map/Set key & element equality. Like `===` but NaN equals
    /// NaN (so NaN is a usable key and all NaNs dedupe). +0/-0 are equal here too
    /// (matching `===`); the store side normalizes -0 → +0. Strings compare by
    /// value, objects by reference identity, and there is no type coercion.
    fn same_value_zero(&self, a: Value, b: Value) -> bool {
        if a.is_number() && b.is_number() {
            let (na, nb) = (a.as_f64(), b.as_f64());
            return na == nb || (na.is_nan() && nb.is_nan());
        }
        self.values_strict_eq(a, b)
    }

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
        self.add_values(va, vb)
    }

    /// The `+` operator on two already-fetched Values (shared by the interpreter's
    /// `Add`/`StrConcat` and the JIT's `jit_concat` helper).
    #[inline]
    pub(crate) fn add_values(&mut self, va: Value, vb: Value) -> Result<Value, Thrown> {
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

    /// `acc + val` as a string append that MUTATES `acc`'s buffer in place when
    /// `acc` is a uniquely-owned, non-interned flat string (`Str` at a user heap
    /// index). Otherwise — `acc` is the interned `""`/single-char (first append),
    /// a rope, or not a string — it allocates a FRESH non-interned flat string
    /// `display(acc) + display(val)` (never interned, so the NEXT append mutates
    /// it). Correctness rests on the emitter's linearity proof: the only reference
    /// to the mutated buffer is the accumulator itself, so the mutation is
    /// unobservable. Returns the (possibly unchanged) accumulator Value.
    pub(crate) fn str_append_inplace(&mut self, acc: Value, val: Value) -> Value {
        let mutable = acc.is_heap()
            && acc.heap_index() > crate::heap::INTERN_EMPTY
            && matches!(self.heap.get(acc.heap_index()), HeapObj::Str(_));
        // Fast path: appending a single decimal digit (the `s += i%10` shape) —
        // no temporary allocation for the value's string form.
        if mutable && val.is_int() {
            let n = val.as_int();
            if (0..=9).contains(&n) {
                if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                    js.bytes.push((b'0' + n as u8) as char);
                    js.char_len += 1;
                    return acc;
                }
            }
        }
        // General: materialise `val`'s string form (same coercion as `+`).
        let ri = self.to_str_idx(val);
        let add: String = self.heap.str_cow(ri).map(|c| c.into_owned()).unwrap_or_default();
        if mutable {
            if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                let cl = add.chars().count();
                let asc = add.is_ascii();
                js.bytes.push_str(&add);
                js.char_len += cl;
                js.ascii &= asc;
                return acc;
            }
        }
        // Fresh buffer (first append / interned / rope acc): flatten acc + add into
        // a NON-interned `Str` (bypass `alloc_str`'s interning so it's mutable next).
        let li = self.to_str_idx(acc);
        let mut s: String =
            self.heap.str_cow(li).map(|c| c.into_owned()).unwrap_or_default();
        s.push_str(&add);
        Value::heap(self.heap.alloc(HeapObj::Str(crate::heap::JsStr::new(s))))
    }

    /// Heap index of a string-like object representing `v`: `v`'s own index when
    /// it is already a string (flat or rope), else a freshly allocated flat
    /// string from `v`'s string coercion. Used to build rope children.
    fn to_str_idx(&mut self, v: Value) -> u32 {
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            return v.heap_index();
        }
        // A single-digit int is a 1-char ASCII string, already interned at its
        // byte — return that slot directly (no temporary `String` alloc). This is
        // the hot `s += (i % 10)` digit-concat case.
        if v.is_int() {
            let n = v.as_int();
            if (0..=9).contains(&n) {
                return (b'0' as i32 + n) as u32;
            }
        }
        let s = self.display(v);
        self.heap.alloc_str(s)
    }

    #[inline]
    fn cmp_lt(&mut self, base: usize, a: u16, b: u16) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() < vb.as_int());
        }
        if let Some(o) = self.str_relational(va, vb) {
            return Ok(o.is_lt());
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
        if let Some(o) = self.str_relational(va, vb) {
            return Ok(o.is_le());
        }
        Ok(self.to_number(va)? <= self.to_number(vb)?)
    }

    /// JS relational comparison of two STRING operands is lexicographic (by code
    /// unit) — not numeric. Returns the `Ordering` when both are string-like, else
    /// `None` (the caller falls back to numeric comparison). Mirrors the engine's
    /// code-point ordering (≈ UTF-16 for the BMP; astral chars are a known edge).
    fn str_relational(&self, va: Value, vb: Value) -> Option<std::cmp::Ordering> {
        if va.is_heap()
            && vb.is_heap()
            && self.heap.is_str_like(va.heap_index())
            && self.heap.is_str_like(vb.heap_index())
        {
            let sa = self.heap.str_cow(va.heap_index())?;
            let sb = self.heap.str_cow(vb.heap_index())?;
            return Some(sa.as_ref().cmp(sb.as_ref()));
        }
        None
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
            // Two DISTINCT interned single-ASCII-char slots (idx < INTERN_EMPTY,
            // see Heap::new) are different chars — bits already differ here, so
            // they can't be equal; skip the content compare. This is the hot
            // `s[i] === 'x'` char-check in scanners/lexers.
            if ai < crate::heap::INTERN_EMPTY && bi < crate::heap::INTERN_EMPTY {
                return false;
            }
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
        // A Date coerces to its epoch ms (so `d2 - d1`, `+d`, `d1 < d2` work).
        if let HeapObj::Date(ms) = self.heap.get(v.heap_index()) {
            return Ok(*ms);
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
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                    "function".into()
                }
                HeapObj::Cell(inner) => self.display(*inner),
                HeapObj::Array(items) => items
                    .iter()
                    .map(|e| if e.is_nullish() { String::new() } else { self.display(*e) })
                    .collect::<Vec<_>>()
                    .join(","),
                HeapObj::Object(_) => "[object Object]".into(),
                HeapObj::Class(c) => format!("class {} {{ }}", c.name),
                HeapObj::Map { .. } => "[object Map]".into(),
                HeapObj::Set(_) => "[object Set]".into(),
                HeapObj::Generator { .. } => "[object Generator]".into(),
                HeapObj::Promise { .. } => "[object Promise]".into(),
                HeapObj::BoundResolver { .. } => "function".into(),
                // Internal: never user-visible (an async call yields its Promise).
                HeapObj::AsyncState(_) => "[object Promise]".into(),
                HeapObj::Combinator { .. } | HeapObj::CombinatorResolver { .. } => {
                    "[object Object]".into()
                }
                // `String(date)` / `"" + date` → the date string (ISO here).
                HeapObj::Date(ms) => {
                    if ms.is_nan() {
                        "Invalid Date".into()
                    } else {
                        date_to_iso(*ms)
                    }
                }
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

    /// `console.log` label for a function value: `[Function: name]`, or
    /// `[Function (anonymous)]` for an arrow / unnamed expression (synthetic
    /// names start with `<`). Class methods are stored as `Class.method`; show
    /// just the method part, as node does.
    fn func_label(&self, fid: u32) -> String {
        let name = &self.program.functions[fid as usize].name;
        if name.is_empty() || name.starts_with('<') {
            "[Function (anonymous)]".into()
        } else {
            let short = name.rsplit('.').next().unwrap_or(name);
            format!("[Function: {short}]")
        }
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
            HeapObj::Func(id) => self.func_label(*id),
            HeapObj::Closure { func, .. } => self.func_label(*func),
            HeapObj::Bound { .. } => "[Function: bound]".into(),
            HeapObj::Native(_) => "[Function (native)]".into(),
            HeapObj::Cell(inner) => self.inspect_nested(*inner),
            HeapObj::Array(items) => {
                if items.is_empty() {
                    return "[]".into();
                }
                let parts: Vec<String> = items.iter().map(|e| self.inspect_nested(*e)).collect();
                format!("[ {} ]", parts.join(", "))
            }
            HeapObj::Object(map) => {
                // A class instance prints with its constructor name (`Pt { … }`).
                let prefix = match map.class {
                    Some(cidx) => match self.heap.get(cidx) {
                        HeapObj::Class(c) => format!("{} ", c.name),
                        _ => String::new(),
                    },
                    None => String::new(),
                };
                if map.keys.is_empty() {
                    return format!("{prefix}{{}}");
                }
                let parts: Vec<String> = map
                    .keys
                    .iter()
                    .zip(map.vals.iter())
                    .map(|(k, val)| format!("{k}: {}", self.inspect_nested(*val)))
                    .collect();
                format!("{prefix}{{ {} }}", parts.join(", "))
            }
            HeapObj::Class(c) => format!("[class {}]", c.name),
            HeapObj::Map { keys, vals } => {
                if keys.is_empty() {
                    return "Map(0) {}".into();
                }
                let parts: Vec<String> = keys
                    .iter()
                    .zip(vals.iter())
                    .map(|(k, v)| format!("{} => {}", self.inspect_nested(*k), self.inspect_nested(*v)))
                    .collect();
                format!("Map({}) {{ {} }}", keys.len(), parts.join(", "))
            }
            HeapObj::Set(items) => {
                if items.is_empty() {
                    return "Set(0) {}".into();
                }
                let parts: Vec<String> = items.iter().map(|v| self.inspect_nested(*v)).collect();
                format!("Set({}) {{ {} }}", items.len(), parts.join(", "))
            }
            HeapObj::Generator { .. } => "Object [Generator] {}".into(),
            HeapObj::Promise { state, result, .. } => match state {
                crate::heap::PromiseState::Pending => "Promise { <pending> }".into(),
                crate::heap::PromiseState::Fulfilled => {
                    format!("Promise {{ {} }}", self.inspect_nested(*result))
                }
                crate::heap::PromiseState::Rejected => {
                    format!("Promise {{ <rejected> {} }}", self.inspect_nested(*result))
                }
            },
            HeapObj::BoundResolver { .. } => "[Function (anonymous)]".into(),
            // Internal: never user-visible (an async call yields its Promise).
            HeapObj::AsyncState(_) => "Promise { <pending> }".into(),
            HeapObj::Combinator { .. } | HeapObj::CombinatorResolver { .. } => "[object Object]".into(),
            // node renders a Date in console.log as its ISO string (unquoted).
            HeapObj::Date(ms) => {
                if ms.is_nan() {
                    "Invalid Date".into()
                } else {
                    date_to_iso(*ms)
                }
            }
        }
    }

    /// Resolve a constant slot: most are plain Values; string constants are
    /// stored as a sentinel index into the function's `string_constants` and
    /// interned to a heap string on first use.
    #[inline]
    fn resolve_const(&mut self, func_id: u32, v: Value) -> Value {
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

/// Public mirror of `JIT_SELF_RECURSE_MAX` for codegen's inline depth guard (the
/// native fast path compares `vm.jit_recurse_depth` against this before a direct
/// recursive call), kept identical so the inline guard and the slow path agree.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_SELF_RECURSE_MAX_PUB: u32 = JIT_SELF_RECURSE_MAX;

/// Byte offset of `jit_recurse_depth` within `Vm`, for the JIT's inline
/// native→native self-call: the compiled code reads/bumps the counter directly
/// through the `vm` pointer (rdi) rather than crossing into Rust per recursive
/// call. Computed at compile time (verified to match the live field address
/// during bring-up).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_RECURSE_DEPTH_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, jit_recurse_depth);

/// Win64 helper for the slow/finish path of the JIT's inline native→native
/// self-call (see `jit_self_call_at_impl`). The native fast path tracks register
/// windows by raw pointer, so it passes its window base EXPLICITLY in
/// `caller_base_ptr` (the native `rbx`). `packed` carries `func_id` in the low 24
/// bits and `argc` in the high 8. Returns the result bits or `SELF_CALL_DEOPT`
/// (the activation threw — `pending_throw` is set, the native chain unwinds, and
/// the top-level interpreter re-raises it). ABI: rcx=vm, rdx=caller_base_ptr,
/// r8=args_ptr, r9=packed.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `caller_base_ptr` is the caller's window base
/// within `vm.regs`; `args` points to `argc` valid `Value` bits.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_self_call_at(
    vm: *mut core::ffi::c_void,
    caller_base_ptr: *const u64,
    args: *const u64,
    packed: u32,
) -> u64 {
    let func_id = packed & 0x00FF_FFFF;
    let argc = (packed >> 24) as usize;
    // Catch Rust panics at the FFI boundary (UB to unwind across `extern`).
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_self_call_at_impl(func_id, caller_base_ptr, args, argc)
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
        // Flat ASCII string `s[i]`: mirror the interpreter's get_index Str path
        // EXACTLY (vm.rs `get_index`, the `js.ascii` branch). The i-th char is
        // the i-th byte, and a single ASCII char is interned at heap index ==
        // its byte (Heap::new), so the result is that interned slot. In range →
        // that slot; out of range → undefined. Only the O(1)-and-identical
        // flat-ASCII case is handled; a non-ASCII string (char-walk) or a rope
        // `Cons` (must flatten first, a &mut op) deopts to the interpreter. A
        // negative/fractional/non-integer key (`array_index` → None) also defers
        // (the interpreter handles `s["length"]`, methods, etc.).
        HeapObj::Str(s) if s.ascii => match array_index(key) {
            Some(i) => match s.bytes.as_bytes().get(i) {
                Some(&b) => Value::heap(b as u32).bits(),
                None => Value::UNDEFINED.bits(),
            },
            None => crate::codegen::SELF_CALL_DEOPT,
        },
        _ => crate::codegen::SELF_CALL_DEOPT, // non-ASCII str / rope / other → interpreter
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

/// Win64 helper for a JIT'd `arr.push(x)` in a region. Appends and returns the
/// new length (Int bits), or `SELF_CALL_DEOPT` for a non-array receiver (the
/// interpreter then resolves the real method). Pins only the register file; the
/// array's Vec may reallocate — safe, no cached pointer.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_array_push(
    vm: *mut core::ffi::c_void,
    arr_bits: u64,
    val_bits: u64,
) -> u64 {
    let arr = Value::from_bits(arr_bits);
    if !arr.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: exclusive view; pins only the register file, not the array's Vec.
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.heap.get_mut(arr.heap_index()) {
        HeapObj::Array(items) => {
            items.push(Value::from_bits(val_bits));
            Value::int(items.len() as i32).bits()
        }
        _ => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper for a JIT'd `str.charCodeAt(i)` in a region. Returns the UTF
/// scalar value (Int bits), NaN bits for an out-of-range index, or
/// `SELF_CALL_DEOPT` for a non-int index / non-flat-string receiver (a rope or
/// non-string → the interpreter, which flattens). O(1) for ASCII.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_char_code_at(
    vm: *mut core::ffi::c_void,
    str_bits: u64,
    i_bits: u64,
) -> u64 {
    let sv = Value::from_bits(str_bits);
    let iv = Value::from_bits(i_bits);
    if !sv.is_heap() || !iv.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let i = match array_index(iv) {
        Some(i) => i,
        None => return crate::codegen::SELF_CALL_DEOPT, // negative/fractional
    };
    // SAFETY: read-only view; the running region holds no conflicting borrow.
    let vm = unsafe { &*(vm as *const Vm) };
    match vm.heap.get(sv.heap_index()) {
        HeapObj::Str(js) => {
            let ch = if js.ascii {
                js.bytes.as_bytes().get(i).map(|&b| b as char)
            } else {
                js.bytes.chars().nth(i)
            };
            match ch {
                Some(c) => Value::int(c as i32).bits(),
                None => Value::num(f64::NAN).bits(),
            }
        }
        _ => crate::codegen::SELF_CALL_DEOPT, // rope/non-string → interpreter
    }
}

/// `dst = a + b` for the OSR region's `StrConcat` op: the `+` operator (rope
/// concat or numeric add) on two boxed Values, returning the result bits. A
/// throwing coercion (only possible for exotic operands a `StrConcat` hint
/// shouldn't target) returns `SELF_CALL_DEOPT` so the region bails and the
/// interpreter redoes it (raising the throw properly).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_concat(
    vm: *mut core::ffi::c_void,
    a_bits: u64,
    b_bits: u64,
) -> u64 {
    let a = Value::from_bits(a_bits);
    let b = Value::from_bits(b_bits);
    // SAFETY: exclusive view to allocate the rope node; the running region holds
    // no conflicting borrow (it touches only the reg file / globals base, and the
    // heap grows in a separate field).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.add_values(a, b) {
        Ok(v) => v.bits(),
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// `dst = a + b` for the OSR region's `StrAppendInPlace` op: appends into `a`'s
/// buffer in place when uniquely owned (see `str_append_inplace`). Never deopts
/// (string append doesn't throw); always returns the result bits.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_str_append(
    vm: *mut core::ffi::c_void,
    a_bits: u64,
    b_bits: u64,
) -> u64 {
    let a = Value::from_bits(a_bits);
    let b = Value::from_bits(b_bits);
    // SAFETY: exclusive view to mutate/allocate the string; the running region
    // holds no conflicting borrow (reg file / globals base only).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    vm.str_append_inplace(a, b).bits()
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
            // Missing own key: a class instance may resolve it as a method, so
            // defer to the interpreter; a plain object yields undefined.
            None if map.class.is_some() => return crate::codegen::SELF_CALL_DEOPT,
            None => return Value::UNDEFINED.bits(),
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

fn json_skip_ws(b: &[u8], i: &mut usize) {
    while matches!(b.get(*i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        *i += 1;
    }
}

/// Match a literal `word` (true/false/null) at `*i`, advancing past it.
fn json_expect(b: &[u8], i: &mut usize, word: &str) -> Result<(), Thrown> {
    if b[*i..].starts_with(word.as_bytes()) {
        *i += word.len();
        Ok(())
    } else {
        Err(Thrown("SyntaxError: Unexpected token in JSON".into()))
    }
}

/// Read exactly 4 hex digits at `pos` as a code unit.
fn json_hex4(b: &[u8], pos: usize) -> Result<u32, Thrown> {
    if pos + 4 > b.len() {
        return Err(Thrown("SyntaxError: Bad unicode escape in JSON".into()));
    }
    let mut v = 0u32;
    for k in 0..4 {
        let d = match b[pos + k] {
            c @ b'0'..=b'9' => (c - b'0') as u32,
            c @ b'a'..=b'f' => (c - b'a' + 10) as u32,
            c @ b'A'..=b'F' => (c - b'A' + 10) as u32,
            _ => return Err(Thrown("SyntaxError: Bad unicode escape in JSON".into())),
        };
        v = v * 16 + d;
    }
    Ok(v)
}

/// Parse a JSON string literal starting at the opening `"` (index `*i`), applying
/// escapes (incl. `\uXXXX` and surrogate pairs). Plain content is flushed as UTF-8
/// slices so multi-byte characters survive intact.
fn json_parse_string(src: &str, i: &mut usize) -> Result<String, Thrown> {
    let b = src.as_bytes();
    *i += 1; // opening quote
    let mut out = String::new();
    let mut run = *i;
    loop {
        match b.get(*i).copied() {
            None => return Err(Thrown("SyntaxError: Unterminated string in JSON".into())),
            Some(b'"') => {
                out.push_str(&src[run..*i]);
                *i += 1;
                return Ok(out);
            }
            Some(b'\\') => {
                out.push_str(&src[run..*i]); // flush the plain run before the escape
                *i += 1;
                match b.get(*i).copied() {
                    Some(b'"') => out.push('"'),
                    Some(b'\\') => out.push('\\'),
                    Some(b'/') => out.push('/'),
                    Some(b'n') => out.push('\n'),
                    Some(b'r') => out.push('\r'),
                    Some(b't') => out.push('\t'),
                    Some(b'b') => out.push('\u{0008}'),
                    Some(b'f') => out.push('\u{000c}'),
                    Some(b'u') => {
                        let cp = json_hex4(b, *i + 1)?;
                        *i += 4; // past the 4 hex (now at the last one)
                        let ch = if (0xD800..=0xDBFF).contains(&cp) {
                            // High surrogate: combine with a following \uXXXX low.
                            if b.get(*i + 1) == Some(&b'\\') && b.get(*i + 2) == Some(&b'u') {
                                let lo = json_hex4(b, *i + 3)?;
                                if (0xDC00..=0xDFFF).contains(&lo) {
                                    *i += 6;
                                    let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    char::from_u32(c).unwrap_or('\u{FFFD}')
                                } else {
                                    '\u{FFFD}'
                                }
                            } else {
                                '\u{FFFD}'
                            }
                        } else {
                            char::from_u32(cp).unwrap_or('\u{FFFD}')
                        };
                        out.push(ch);
                    }
                    _ => return Err(Thrown("SyntaxError: Invalid escape in JSON string".into())),
                }
                *i += 1;
                run = *i;
            }
            // A raw control character (< 0x20) is invalid in a JSON string — it
            // must be escaped (`\n`, `	`, …). (Matches the spec / node.)
            Some(c) if c < 0x20 => {
                return Err(Thrown("SyntaxError: Bad control character in string literal in JSON".into()));
            }
            Some(_) => *i += 1, // plain byte (ASCII or UTF-8 continuation) — sliced later
        }
    }
}

/// Parse a JSON number token at `*i`.
fn json_parse_number(b: &[u8], i: &mut usize) -> Result<Value, Thrown> {
    let start = *i;
    if b.get(*i) == Some(&b'-') {
        *i += 1;
    }
    while matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
        *i += 1;
    }
    if b.get(*i) == Some(&b'.') {
        *i += 1;
        while matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
            *i += 1;
        }
    }
    if matches!(b.get(*i), Some(b'e' | b'E')) {
        *i += 1;
        if matches!(b.get(*i), Some(b'+' | b'-')) {
            *i += 1;
        }
        while matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
            *i += 1;
        }
    }
    match std::str::from_utf8(&b[start..*i]).unwrap_or("").parse::<f64>() {
        Ok(n) => Ok(Value::num(n)),
        Err(_) => Err(Thrown("SyntaxError: Invalid number in JSON".into())),
    }
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

/// A single-argument `Math.<op>` computation, matching JS where it diverges
/// from Rust (`round` half-up; `sign` preserves ±0 and maps NaN→NaN). The
/// variadic/binary ops never reach here with the real call paths; they fall
/// back to operating on the one value provided.
fn math_unary(op: crate::bytecode::MathFn, x: f64) -> f64 {
    use crate::bytecode::MathFn as M;
    match op {
        M::Abs => x.abs(),
        M::Floor => x.floor(),
        M::Ceil => x.ceil(),
        M::Round => (x + 0.5).floor(),
        M::Trunc => x.trunc(),
        M::Sign => {
            if x.is_nan() {
                f64::NAN
            } else if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                x
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
        // Pow/Atan2/Min/Max/Hypot aren't unary; degrade gracefully.
        M::Min | M::Max => x,
        M::Hypot => x.abs(),
        M::Pow | M::Atan2 => f64::NAN,
    }
}

/// `Number.isInteger`: a number with no fractional part (no coercion).
fn num_is_integer(v: Value) -> bool {
    if v.is_int() {
        true
    } else if v.is_double() {
        let n = v.as_f64();
        n.is_finite() && n.fract() == 0.0
    } else {
        false
    }
}

/// `Number.isFinite`: a finite number (no coercion).
fn num_is_finite(v: Value) -> bool {
    v.is_int() || (v.is_double() && v.as_f64().is_finite())
}

/// `Number.isSafeInteger`: an integer within ±(2^53 − 1).
fn num_is_safe_integer(v: Value) -> bool {
    num_is_integer(v) && {
        let n = if v.is_int() { v.as_int() as f64 } else { v.as_f64() };
        n.abs() <= 9_007_199_254_740_991.0
    }
}

/// `Number.prototype.toString(radix)` for `radix` in 2..=36. Renders the integer
/// part in the given base (matching JS for whole numbers; a fractional part is
/// truncated — full fractional-radix rendering is out of the subset). NaN and
/// ±Infinity render via the canonical path (handled by the caller for radix 10).
// ── Date helpers (proleptic Gregorian, UTC; Howard Hinnant's algorithms) ──

/// Days since 1970-01-01 for (year, month 1..=12, day) — `day` may be out of
/// [1,31] and is carried linearly (so day 0 = the prior day), matching JS's
/// field normalization.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// (year, month 1..=12, day) from days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Break epoch ms into UTC parts: (year, month0, day, hour, min, sec, ms,
/// weekday 0=Sun..6=Sat). Uses floored division so negative ms work.
fn date_parts(ms: f64) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
    let total = ms.floor() as i64;
    let day = total.div_euclid(86_400_000);
    let rem = total.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(day);
    let h = rem / 3_600_000;
    let mi = (rem / 60_000) % 60;
    let s = (rem / 1000) % 60;
    let mss = rem % 1000;
    let wd = (day.rem_euclid(7) + 4) % 7; // 1970-01-01 was a Thursday (4)
    (y, m - 1, d, h, mi, s, mss, wd)
}

/// Epoch ms from UTC components (month0-based; out-of-range fields normalized
/// like JS). NOTE: the legacy 2-digit-year→19xx mapping is applied by the numeric
/// CONSTRUCTORS (`Date.UTC`, `new Date(y,m,…)`), NOT here — ISO string parsing
/// must take the year literally (year 1 = 1, not 1901).
fn ms_from_utc(y: i64, mo0: i64, d: i64, h: i64, mi: i64, s: i64, ms: i64) -> f64 {
    let year = y + mo0.div_euclid(12);
    let month = mo0.rem_euclid(12); // 0-based → 1-based below
    let days = days_from_civil(year, month + 1, d);
    days as f64 * 86_400_000.0
        + h as f64 * 3_600_000.0
        + mi as f64 * 60_000.0
        + s as f64 * 1000.0
        + ms as f64
}

/// The legacy 2-digit-year mapping for the numeric Date constructors: 0..=99 →
/// 1900+y (so `Date.UTC(99,…)` is 1999). Years ≥100 (and negative) pass through.
fn legacy_year(y: i64) -> i64 {
    if (0..=99).contains(&y) {
        1900 + y
    } else {
        y
    }
}

/// JS TimeClip: NaN if non-finite or |t| > 8.64e15 (±100M days); else truncate
/// toward zero to an integer millisecond.
fn time_clip(n: f64) -> f64 {
    if !n.is_finite() || n.abs() > 8.64e15 {
        f64::NAN
    } else {
        n.trunc()
    }
}

/// `toISOString` form: `YYYY-MM-DDTHH:mm:ss.sssZ` (±YYYYYY outside 0..=9999).
fn date_to_iso(ms: f64) -> String {
    let (y, mo0, d, h, mi, s, mss, _) = date_parts(ms);
    if (0..=9999).contains(&y) {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo0 + 1, d, h, mi, s, mss)
    } else {
        let sign = if y < 0 { '-' } else { '+' };
        format!("{}{:06}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", sign, y.abs(), mo0 + 1, d, h, mi, s, mss)
    }
}

/// Parse the ISO-8601 subset JS accepts (`YYYY`, `YYYY-MM`, `YYYY-MM-DD`,
/// optionally `THH:mm[:ss[.sss]]` and a trailing `Z`). Treated as UTC. Returns
/// NaN if unrecognised.
fn parse_date(s: &str) -> f64 {
    let s = s.trim();
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let dp: Vec<&str> = date.split('-').collect();
    // A leading '-' (negative year) splits into an empty first field; reject.
    if dp.is_empty() || dp[0].is_empty() {
        return f64::NAN;
    }
    let parse = |x: &str| x.parse::<i64>().ok();
    let year = match parse(dp[0]) {
        Some(y) => y,
        None => return f64::NAN,
    };
    let mo = if dp.len() > 1 { match parse(dp[1]) { Some(v) => v, None => return f64::NAN } } else { 1 };
    let day = if dp.len() > 2 { match parse(dp[2]) { Some(v) => v, None => return f64::NAN } } else { 1 };
    let (mut h, mut mi, mut sec, mut msec) = (0i64, 0i64, 0i64, 0i64);
    if let Some(t) = time {
        let t = t.trim_end_matches('Z');
        // Drop a timezone offset (we treat everything as UTC).
        let t = t.split(['+']).next().unwrap_or(t);
        let (hms, frac) = match t.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (t, None),
        };
        let tp: Vec<&str> = hms.split(':').collect();
        if !tp.is_empty() {
            h = parse(tp[0]).unwrap_or(0);
        }
        if tp.len() > 1 {
            mi = parse(tp[1]).unwrap_or(0);
        }
        if tp.len() > 2 {
            sec = parse(tp[2]).unwrap_or(0);
        }
        if let Some(f) = frac {
            // First 3 digits = milliseconds.
            let f3: String = f.chars().take(3).chain(std::iter::repeat('0')).take(3).collect();
            msec = f3.parse::<i64>().unwrap_or(0);
        }
    }
    // mo here is 1-based from the string; ms_from_utc wants 0-based.
    ms_from_utc(year, mo - 1, day, h, mi, sec, msec)
}

/// `Number.prototype.toFixed(f)`. JS rounds half AWAY from zero — `(0.5).toFixed(0)`
/// is "1", `(2.5).toFixed(0)` is "3" — whereas Rust's `{:.*}` formatter rounds
/// half-to-even. We round the EXACT decimal of the f64 (not `x*10^f`, whose
/// product error would mis-round e.g. `0.15` whose true value is `0.14999…`):
/// format with guard digits to expose the exact value, then round the decimal
/// string half-up at `f` places. Huge magnitudes (≥1e21) defer to the default
/// rendering (JS switches to exponential there too).
fn to_fixed(n: f64, f: usize) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n.abs() >= 1e21 {
        return format!("{n}");
    }
    let neg = n.is_sign_negative();
    // Exact decimal of |n| with 30 guard digits past `f`; the digit at index `f`
    // (first dropped) decides the rounding, and the formatter computes it exactly.
    let s = format!("{:.*}", f + 30, n.abs());
    let dot = s.find('.').unwrap();
    let int_part = &s[..dot];
    let frac = s[dot + 1..].as_bytes();
    let round_up = frac[f] >= b'5';
    // Digits we keep (integer + first `f` fractional), as a mutable byte buffer.
    let mut digits: Vec<u8> = int_part.bytes().chain(frac[..f].iter().copied()).collect();
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1'); // carried past the most-significant digit
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    // Place the decimal point `f` digits from the right.
    let mut out = String::from_utf8(digits).unwrap();
    if f > 0 {
        let point = out.len() - f;
        out.insert(point, '.');
    }
    if neg {
        out.insert(0, '-');
    }
    out
}

fn num_to_radix(n: f64, radix: u32) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let neg = n < 0.0;
    let mut int = n.abs().trunc() as u64;
    if int == 0 {
        return "0".into();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    while int > 0 {
        buf.push(DIGITS[(int % radix as u64) as usize]);
        int /= radix as u64;
    }
    if neg {
        buf.push(b'-');
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

/// Normalize a Map key / Set element: `-0` becomes `+0` (SameValueZero treats
/// them equal, and iteration must yield `+0`). Everything else is unchanged.
fn normalize_zero(v: Value) -> Value {
    if v.is_double() && v.as_f64() == 0.0 {
        Value::num(0.0)
    } else {
        v
    }
}

/// JS `ToInt32`: truncate toward zero, take modulo 2^32, interpret as signed.
/// NaN/±Infinity → 0. Used by the bitwise operators.
fn to_int32(n: f64) -> i32 {
    to_uint32(n) as i32
}

/// JS `ToUint32`: truncate toward zero, take modulo 2^32 as an unsigned value.
fn to_uint32(n: f64) -> u32 {
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    // rem_euclid keeps the result in [0, 2^32); `as u32` then wraps exactly.
    let m = n.trunc().rem_euclid(4_294_967_296.0);
    m as u32
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
