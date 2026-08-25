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

#![allow(unused_imports)]
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// Hard cap on simultaneous JS frames. Throws a catchable RangeError rather
/// than growing unbounded. 100k is far beyond any non-pathological recursion
/// and the flat register file makes each frame cheap.
#[cfg(feature = "safe-sandbox")]
const MAX_FRAMES: usize = 4_096;
#[cfg(not(feature = "safe-sandbox"))]
const MAX_FRAMES: usize = 100_000;

/// Hard cap on NESTED `run_loop` entries (native re-entries: builtin
/// callbacks, generator/async resumes, direct evals, JIT bail-outs). Each one
/// is a Rust frame on the OS stack — unlike interpreter calls, which stay flat
/// inside one `run_loop` — so runaway recursion routed through a re-entry
/// (e.g. a generator body that re-enters its driver) must hit this catchable
/// RangeError before the native stack overflows. The safe profile's deliberately
/// small ceiling was validated on a 1 MiB Windows main-thread stack; the native
/// footprint of one re-entry varies substantially across builtin call paths.
#[cfg(feature = "safe-sandbox")]
// A JavaScript callback reached from a native meta-operation carries a much
// larger Rust frame than transparent Proxy/prototype forwarding. On the 1 MiB
// worker stack, allowing a fourth simultaneous `run_loop` entry can overflow
// before a larger numeric ceiling is observed (nested Proxy get/has traps are a
// compact reproducer). The outer script itself occupies depth one, so three
// still preserves two nested observable callback/trap invocations.
const MAX_RUN_LOOP_DEPTH: u32 = 3;
#[cfg(not(feature = "safe-sandbox"))]
const MAX_RUN_LOOP_DEPTH: u32 = 4096;

/// Hard cap on Rust recursion through guest-controlled object meta-operations.
/// Ordinary JavaScript calls use the VM's explicit frame stack, but transparent
/// Proxy forwarding and a few exotic/prototype algorithms call their Rust
/// implementation recursively. A guest can build an arbitrarily deep wrapper
/// chain before performing one `get`/`has`/`define`/etc.; without a separate
/// budget that single bytecode instruction can exhaust the native/Wasm stack.
#[cfg(feature = "safe-sandbox")]
const MAX_NATIVE_RECURSION_DEPTH: u32 = 32;
#[cfg(not(feature = "safe-sandbox"))]
const MAX_NATIVE_RECURSION_DEPTH: u32 = 4096;

/// Hard cap on CONSECUTIVE tail-reuse activations (`try_tail_reuse`) with no
/// intervening frame pop. Proper tail calls run in O(1) frames, so runaway
/// strict-mode tail recursion (`return f()` forever) never trips MAX_FRAMES —
/// it would hang. Engines that shipped without PTC (node/V8) throw RangeError
/// on that shape at ~10k depth; this budget matches that OUTCOME while staying
/// ~100x above node's tolerance, so legitimate deep-but-terminating tail loops
/// keep their constant-stack win.
const MAX_TAIL_REUSE_STREAK: u32 = 1_000_000;

/// Extra global slots reserved past `global_count` as JIT scratch "field globals"
/// for object scalar-replacement (SROA). A field-promoted region uses pool slots
/// `[global_count, global_count + n_fields)`; regions reuse the pool (synced per
/// native run, never concurrent), so this caps fields-per-region, not total.
const FIELD_POOL: usize = 64;

/// Extra global slots reserved past the JIT field pool for globals *created or
/// first referenced inside `eval`* (sloppy `x = 1`, `var x`, hoisted function
/// declarations, and reads of builtins the main program never named). Sized once
/// at startup so the globals Vec never reallocates at runtime (the JIT pins its
/// base pointer); `eval` draws from this pool by name and throws once it is
/// exhausted rather than growing the Vec.
const EVAL_POOL: usize = 1024;

/// Sentinel `closure` value for a frame whose callee is a plain (capture-free)
/// function rather than a closure. Real heap indices are always `< u32::MAX`.
const NO_CLOSURE: u32 = u32::MAX;

/// Exact roots for one active Tier-C native activation.
/// `active` is explicit because a compiled script/plain activation can have no
/// Closure identity while still suspending an outer frame-free activation.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TiercActivationState {
    active: bool,
    frame_free: bool,
    closure: u32,
    callee: u32,
}

/// By-value restoration record for one Tier-C native entry. A suspended
/// frame-free `prior` is also duplicated in `jit_tierc_activation_stack` so GC
/// can see it while this token lives only in a Rust local. A frame-backed prior
/// needs no duplicate root: its real interpreter `Frame` remains installed.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct TiercActivationToken {
    prior: TiercActivationState,
    rooted_prior: bool,
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
impl TiercActivationState {
    const EMPTY: Self = Self {
        active: false,
        frame_free: false,
        closure: NO_CLOSURE,
        callee: NO_CLOSURE,
    };
}

/// Largest length zipp will EAGERLY materialize for a dense array (`Vec<Value>`).
/// The spec allows up to 2^32-1, but a dense Vec of that many `Value`s would be
/// 32 GB; real engines store such arrays sparsely. Until zipp has sparse arrays,
/// a `new Array(n)` / `arr.length = n` / defineProperty('length') / large-index
/// assignment / array-like materialization beyond this cap throws a RangeError
/// instead of OOMing the host. 2^22 elements ≈ 32 MB per array — far larger than
/// any realistic program needs, while keeping a 12-way-parallel test262 run (each
/// process possibly building several arrays) comfortably bounded.
#[cfg(feature = "safe-sandbox")]
pub(crate) const MAX_DENSE_ARRAY_LEN: usize = 1 << 17;
#[cfg(not(feature = "safe-sandbox"))]
pub(crate) const MAX_DENSE_ARRAY_LEN: usize = 1 << 20;

/// Maximum materialized string size. The hardened profile keeps a single
/// allocation small enough that the periodic heap poll cannot overshoot a
/// host's budget by hundreds of megabytes. Rope length is capped separately in
/// UTF-16 units; three WTF-8 bytes per unit is its worst-case representation.
#[cfg(feature = "safe-sandbox")]
pub(crate) const MAX_STRING_BYTES: usize = 1 << 20;
#[cfg(not(feature = "safe-sandbox"))]
pub(crate) const MAX_STRING_BYTES: usize = 1 << 28;
#[cfg(feature = "safe-sandbox")]
pub(crate) const MAX_STRING_UNITS: usize = 1 << 18;
#[cfg(not(feature = "safe-sandbox"))]
pub(crate) const MAX_STRING_UNITS: usize = 1 << 28;

/// Maximum amount of host-side iteration one JavaScript builtin may perform
/// without returning to the bytecode dispatch loop. The instruction meter can
/// interrupt JavaScript callbacks, but it cannot see a native loop over holes
/// or an attacker-controlled array-like `length`; keep those operations
/// independently bounded in the hostile-code profile.
#[cfg(feature = "safe-sandbox")]
pub(crate) const MAX_NATIVE_ITERATION_WORK: u64 = 1 << 18;
#[cfg(not(feature = "safe-sandbox"))]
pub(crate) const MAX_NATIVE_ITERATION_WORK: u64 = u64::MAX;

/// Maximum owned memory retained by one compiled RegExp program in the
/// hostile-code profile. Source length alone is not a sufficient bound:
/// Unicode properties expand into owned interval tables or string-alternative
/// programs. The regex parser has independent transient-expansion budgets;
/// this cap covers the immutable program that survives compilation.
#[cfg(feature = "safe-sandbox")]
pub(crate) const MAX_REGEX_PROGRAM_BYTES: usize = 4 << 20;
#[cfg(not(feature = "safe-sandbox"))]
pub(crate) const MAX_REGEX_PROGRAM_BYTES: usize = usize::MAX;

/// Largest length an ITERATION METHOD will materialize eagerly as a dense
/// result (`Array.prototype.map`). Distinct from `MAX_DENSE_ARRAY_LEN`, which
/// is the point at which *storage* spills to the sparse overlay: an array can
/// legitimately be far longer than that (`new Array(1 << 24)` works), and such
/// an array must still `map` correctly rather than silently returning a
/// truncated result. 2^24 elements ≈ 134 MB — the largest array the engine
/// already builds eagerly — beyond which `map` reports a RangeError instead of
/// attempting a multi-gigabyte allocation the host cannot satisfy.
#[cfg(feature = "safe-sandbox")]
pub(crate) const MAX_EAGER_ITER_RESULT: usize = 1 << 17;
#[cfg(not(feature = "safe-sandbox"))]
pub(crate) const MAX_EAGER_ITER_RESULT: usize = 1 << 24;

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
    /// The function VALUE being invoked in this activation (the object the caller
    /// actually called), or `UNDEFINED` when unknown/irrelevant (top-level script,
    /// eval, generator/async resume). `LoadCallee` returns it so a named function
    /// expression's own name has the SAME identity as the outer reference (rather
    /// than a freshly-allocated `Func`); falls back to the closure/fresh-Func when
    /// `UNDEFINED`.
    callee: Value,
    /// The `new.target` value for this activation: the constructor when entered
    /// via `new` / `Reflect.construct` / `super(...)`, else `undefined` (a plain
    /// call, `.call`/`.apply`, a method, a tagged template, …). Read by the
    /// `LoadNewTarget` op; consumed from `pending_new_target` at frame setup.
    new_target: Value,
    /// A sloppy direct eval's dynamic variable environment for this
    /// activation (heap index of a HeapObj::EvalScope), or u32::MAX. Created
    /// lazily by the DirectEval op for function-context evals.
    eval_scope: u32,
    /// Whether a `super(...)` has completed in THIS activation. A second
    /// `super()` in the same constructor frame throws a ReferenceError (after
    /// running the parent ctor — spec evaluates the SuperCall fully, then
    /// BindThisValue throws on re-initialization).
    super_done: bool,
    /// Heap index of THIS activation's `arguments` object, or u32::MAX. A
    /// MAPPED arguments object's [[ParameterMap]] (see `ArgsMap`) aliases the
    /// param registers only while `frames[frame_idx].args_obj` still equals
    /// its own heap index — the liveness proof (each activation allocates a
    /// fresh object, so a recycled frame slot can never spoof it).
    args_obj: u32,
    /// Absolute index into `regs` of this call's ARGUMENT window — the caller's
    /// staging registers — with `argc` the number of values there. `u32::MAX`
    /// when the arguments were not staged in a register window (a native
    /// `call_value`, a generator/async resume, a JIT bail-out). Only the legacy
    /// `f.arguments` accessor reads them, and only while this frame is live: the
    /// caller is suspended at its call instruction, so its staging registers
    /// cannot change underneath us, and they sit BELOW this frame's base so a
    /// pop can never truncate them away first.
    arg_win: u32,
    argc: u16,
    /// True for the frame running an `eval` program's body. The legacy
    /// `f.caller` walk steps PAST these: an eval is not a function activation,
    /// so `function nest(){ return eval("innermost();"); }` must report `nest`
    /// as `innermost`'s caller, not the eval script.
    is_eval: bool,
}

/// [[ParameterMap]] bookkeeping for a MAPPED arguments object (sloppy callee
/// with a simple parameter list). While the creating frame is live, a still-
/// mapped index i reads/writes the formal's register `regs[base + 1 + i]`
/// (through its cell when the param is captured); the dense element store
/// remains the descriptor/escape store. `None` in `arguments_objs` = an
/// UNMAPPED arguments object (strict, or non-simple params): pure snapshot.
pub(crate) struct ArgsMap {
    /// Expected position of the creating activation in `frames`.
    frame_idx: usize,
    /// The creating frame's register-window base (revalidated with args_obj).
    base: usize,
    /// Indices `0..mapped_count` started mapped: min(param_count, argc).
    mapped_count: usize,
    /// Bit i set = formal i SEVERED from the map (delete / accessor redefine /
    /// writable:false redefine) — permanently back to ordinary semantics.
    unmapped: u64,
}

/// One in-flight `AsyncDisposableStack.prototype.disposeAsync`: the spec's
/// DisposeResources loop with an Await after EVERY disposer call. `remaining`
/// is popped from the END (LIFO); a `Value::NULL` entry is the marker a
/// nullish `use()` records (contributes the needs-await tick, calls nothing).
pub(crate) struct DisposeAsyncState {
    remaining: Vec<Value>,
    /// The pending throw completion: a single error stays as-is; each later
    /// error wraps it as SuppressedError{error: new, suppressed: prior}.
    error_chain: Option<Value>,
    /// Whether any real disposer ran (its result got a real Await).
    has_awaited: bool,
    /// Whether a nullish-resource marker was seen (spec still performs one
    /// Await(undefined) before resolving when nothing else awaited).
    needs_await: bool,
}

/// Which array higher-order method `array_each` is driving (callback args are
/// `[element, index]` for all three; only the result handling differs).
#[derive(Clone, Copy)]
pub(crate) enum EachMode {
    Map,
    Filter,
    ForEach,
}

/// Whether a promise reaction is the fulfill or reject handler.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReactionKind {
    Fulfill,
    Reject,
}

/// How a suspended async activation is resumed: with an awaited value, or by
/// throwing a rejection into it at the await point.
#[derive(Clone, Copy)]
pub(crate) enum Resume {
    Value(Value),
    Throw(Value),
    /// Resume an async generator suspended at an async `yield*` (AsyncYieldDelegate)
    /// because the consumer called `.return(v)` — the loop delegates to the inner
    /// iterator's `return`. Only meaningful for an async generator at a delegate point.
    Return(Value),
}

/// A queued microtask (the whole event loop). `Reaction` runs a promise reaction
/// — `callback` (a JS fn, a native BoundResolver, or undefined for pass-through)
/// applied to the settled `arg`, settling `dependent`. `AsyncResume` resumes a
/// suspended async activation.
pub(crate) enum Microtask {
    Reaction {
        callback: Value,
        arg: Value,
        dependent: u32,
        kind: ReactionKind,
        finally: bool,
    },
    AsyncResume {
        activation: u32,
        input: Resume,
    },
    /// A native intrinsic-`Promise.all` resolve-element job. This is the same
    /// FIFO job as the `CombinatorResolver` callback path, represented directly
    /// so the unobservable resolver does not need a heap object of its own.
    CombinatorStep {
        combinator: u32,
        index: u32,
        kind: ReactionKind,
        arg: Value,
    },
    /// PromiseResolveThenableJob: resolving `promise` with a thenable defers
    /// `then.call(thenable, resolveFn, rejectFn)` to this microtask (spec ordering).
    ThenableJob {
        thenable: Value,
        then: Value,
        promise: u32,
    },
}

/// Native (built-in) function ids — the discriminant carried by `HeapObj::Native`.
/// Each maps to an arm of `Vm::call_native`.

/// What `object_enum_own` collects.
#[derive(Clone, Copy)]
pub(crate) enum EnumWhat {
    Keys,
    Values,
    Entries,
}

/// A module whose dependencies are still evaluating (top-level await):
/// everything needed to execute its body once they settle. Holds NO heap
/// Values (slots/ids only) — the namespace itself is rooted via module_cache.
pub(crate) struct DeferredModuleExec {
    pub remaining: usize,
    pub base_func: u32,
    pub ns_idx: u32,
    pub full2: Vec<(String, u32)>,
}

/// The deferred Annex B legacy statics: a ROOTED subject plus unit ranges for
/// `regexp_last` slots 2..=13 (lastParen, leftContext, rightContext, `$1`..`$9`).
///
/// `subj` exists purely so GC keeps the subject string alive while the ranges
/// point into it — dropping it would let a collection free the bytes a later
/// `RegExp.leftContext` read still has to slice. `subj_idx` is its heap index (the
/// form `ascii_slice_value` takes). `None` in `ranges` is the empty string, which
/// is what the eager form pushed for a non-participating capture.
pub(crate) struct RegexpLastLazy {
    pub subj: Value,
    pub subj_idx: u32,
    /// Unit ranges for `regexp_last` slots **1..=13** — lastMatch, lastParen,
    /// leftContext, rightContext, `$1`..`$9`. Slot 0 (`input`) is a whole Value and
    /// stays eager.
    ///
    /// Slot 1 joined this in B71. It used to be eager because `whole` "is computed
    /// for the result array regardless" — true for `exec`, false for `test`, which
    /// returns a boolean and so paid a malloc + memcpy + `is_ascii` rescan of the
    /// matched text per successful call for nothing.
    pub ranges: [Option<(u32, u32)>; 13],
}

/// The standard named properties of a pristine RegExp match-result Array.
///
/// Keeping these four values in a fixed record avoids three owned key strings
/// and the `ObjMap`'s three per-object vectors on every successful
/// `exec`/`matchAll` step. `values[3] == undefined` means the optional `indices`
/// property is absent (under `/d` its value is always an Array, so that sentinel
/// is unambiguous). Reads and presence checks use this record directly;
/// mutation, reflection, and integrity operations materialise ordinary
/// writable/enumerable/configurable data properties into `arr_props` first.
#[derive(Clone, Copy)]
pub(crate) struct RegexpResultProps {
    /// `index`, `input`, `groups`, and optional `indices`, in creation order.
    pub values: [Value; 4],
}

/// Storage for object-method `[[HomeObject]]` edges.
///
/// Heap indices are dense slot ids, so the default uses a direct vector plus an
/// authoritative presence bitmap. An absent slot's value is cleared as well,
/// but the bitmap (rather than a `Value` sentinel) distinguishes absence: every
/// possible `Value`, including `undefined`, remains a valid strong edge. The
/// vectors grow only to the heap's own high-water slot id and, like the heap,
/// retain that capacity after collection.
///
/// The HashMap variant is retained behind `ZIPP_NO_DENSE_CLOSURE_HOME=1` for
/// same-binary A/B and as a simple differential oracle. Both variants expose
/// the same operations and preserve the edge's GC semantics exactly.
#[derive(Default)]
struct DenseClosureHomes {
    values: Vec<Value>,
    present: Vec<u64>,
    len: usize,
}

impl DenseClosureHomes {
    #[inline]
    fn position(key: u32) -> (usize, u64) {
        let slot = key as usize;
        (slot >> 6, 1u64 << (slot & 63))
    }

    #[inline]
    fn contains(&self, key: u32) -> bool {
        let (word, mask) = Self::position(key);
        self.present
            .get(word)
            .is_some_and(|present| present & mask != 0)
    }

    #[cfg(test)]
    #[inline]
    fn len(&self) -> usize {
        self.len
    }

    #[inline]
    fn retain_scan_cost(&self) -> usize {
        // `retain` reads each high-water bitmap word and invokes `keep` once
        // per populated slot. Include both pieces so a very sparse old table
        // prefers direct removals during a minor.
        self.present.len().saturating_add(self.len)
    }

    #[inline]
    fn get(&self, key: &u32) -> Option<&Value> {
        if self.contains(*key) {
            // `insert` grows `values` before publishing the presence bit.
            Some(&self.values[*key as usize])
        } else {
            None
        }
    }

    #[inline]
    fn insert(&mut self, key: u32, value: Value) -> Option<Value> {
        let slot = key as usize;
        let needed = slot
            .checked_add(1)
            .expect("closure-home slot index overflow");
        if self.values.len() < needed {
            self.values.resize(needed, Value::UNDEFINED);
        }
        let (word, mask) = Self::position(key);
        if self.present.len() <= word {
            self.present.resize(word + 1, 0);
        }

        let old = if self.present[word] & mask != 0 {
            Some(self.values[slot])
        } else {
            self.present[word] |= mask;
            self.len += 1;
            None
        };
        self.values[slot] = value;
        old
    }

    #[inline]
    fn remove(&mut self, key: &u32) -> Option<Value> {
        let slot = *key as usize;
        let (word, mask) = Self::position(*key);
        let present = self.present.get_mut(word)?;
        if *present & mask == 0 {
            return None;
        }
        *present &= !mask;
        self.len -= 1;
        Some(std::mem::replace(&mut self.values[slot], Value::UNDEFINED))
    }

    fn retain<F: FnMut(&u32, &mut Value) -> bool>(&mut self, mut keep: F) {
        for word_idx in 0..self.present.len() {
            // Walk only populated slots. Keep a snapshot because removals update
            // the authoritative word while the iteration is in progress.
            let mut occupied = self.present[word_idx];
            while occupied != 0 {
                let bit = occupied.trailing_zeros() as usize;
                occupied &= occupied - 1;
                let slot = (word_idx << 6) + bit;
                let key = slot as u32;
                if !keep(&key, &mut self.values[slot]) {
                    self.present[word_idx] &= !(1u64 << bit);
                    self.values[slot] = Value::UNDEFINED;
                    self.len -= 1;
                }
            }
        }
    }
}

enum ClosureHomeTable {
    Dense(DenseClosureHomes),
    Map(std::collections::HashMap<u32, Value>),
}

impl Default for ClosureHomeTable {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU8, Ordering};
        static STATE: AtomicU8 = AtomicU8::new(0);
        let dense = match STATE.load(Ordering::Relaxed) {
            1 => true,
            2 => false,
            _ => {
                let on = std::env::var_os("ZIPP_NO_DENSE_CLOSURE_HOME").is_none();
                STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
                on
            }
        };
        if dense {
            Self::Dense(DenseClosureHomes::default())
        } else {
            Self::Map(std::collections::HashMap::new())
        }
    }
}

impl ClosureHomeTable {
    #[inline]
    fn get(&self, key: &u32) -> Option<&Value> {
        match self {
            Self::Dense(table) => table.get(key),
            Self::Map(table) => table.get(key),
        }
    }

    #[inline]
    fn insert(&mut self, key: u32, value: Value, heap_slots: usize) -> Option<Value> {
        // All production callers pass a freshly allocated or otherwise live
        // heap holder. Keep that invariant explicit so a corrupt u32 can never
        // turn the direct-index backend into an attacker-sized allocation.
        assert!(
            (key as usize) < heap_slots,
            "closure-home holder is outside the VM heap"
        );
        match self {
            Self::Dense(table) => table.insert(key, value),
            Self::Map(table) => table.insert(key, value),
        }
    }

    #[inline]
    fn remove(&mut self, key: &u32) -> Option<Value> {
        match self {
            Self::Dense(table) => table.remove(key),
            Self::Map(table) => table.remove(key),
        }
    }

    #[cfg(test)]
    #[inline]
    fn contains_key(&self, key: &u32) -> bool {
        self.get(key).is_some()
    }

    #[cfg(test)]
    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Dense(table) => table.len(),
            Self::Map(table) => table.len(),
        }
    }

    fn retain<F: FnMut(&u32, &mut Value) -> bool>(&mut self, keep: F) {
        match self {
            Self::Dense(table) => table.retain(keep),
            Self::Map(table) => table.retain(keep),
        }
    }

    /// Drop entries whose holder slot was reclaimed by a minor. The dense
    /// backend's retain scans its high-water bitmap and visits populated entries;
    /// HashMap's retain walks bucket capacity. Choose between a table scan and
    /// O(1) removals using the appropriate cost.
    fn prune_freed(&mut self, freed: &[u32], live_bits: &[bool]) {
        let scan_cost = match self {
            Self::Dense(table) => table.retain_scan_cost(),
            Self::Map(table) => table.capacity(),
        };
        if scan_cost <= freed.len() {
            self.retain(|&key, _| live_bits[key as usize]);
        } else {
            for key in freed {
                self.remove(key);
            }
        }
    }

    #[cfg(test)]
    fn dense_for_test() -> Self {
        Self::Dense(DenseClosureHomes::default())
    }

    #[cfg(test)]
    fn map_for_test() -> Self {
        Self::Map(std::collections::HashMap::new())
    }
}

pub struct Vm<'p> {
    program: &'p Program,
    /// Functions compiled at runtime by `eval` / `new Function`. Each is a leaked
    /// `Box<FuncProto>` so its address is stable (the whole-function JIT and the
    /// run loop hold raw pointers into FuncProtos, so they must never move). A
    /// unified `func_id` addresses `program.functions` for `id < main_func_count`
    /// and `eval_funcs[id - main_func_count]` beyond it.
    eval_funcs: Vec<&'static crate::bytecode::FuncProto>,
    /// Aggregate retained static-key plans across the main Program and every
    /// successfully/partially prepared eval/module Program. Incoming programs
    /// beyond either ceiling are rewritten to legacy NewObject before leaking.
    static_key_plan_sites: usize,
    static_key_plan_retained_bytes: usize,
    /// Number of functions in the compile-time `program` (the boundary between
    /// program function ids and runtime `eval_funcs` ids).
    main_func_count: usize,
    /// Classes compiled at runtime by `eval`. Like `eval_funcs`: each is a leaked
    /// `Box<ClassDef>` addressed by a unified class_id (`program.classes` below
    /// `main_class_count`, `eval_classes` beyond). Their func-id members and the
    /// referencing `MakeClass`/`Super*` ops are re-indexed at install time.
    eval_classes: Vec<&'static crate::bytecode::ClassDef>,
    /// Number of classes in the compile-time `program` (boundary between program
    /// and runtime `eval_classes` class ids).
    main_class_count: usize,
    /// Globals introduced by `eval`: maps a global NAME (one not present in the
    /// compile-time `program.global_names`) to the EVAL_POOL slot it was assigned.
    /// Persists across `eval` calls so repeated evals see each other's globals.
    eval_global_map: std::collections::HashMap<String, u32>,
    /// Next free EVAL_POOL slot. Starts at `global_count + FIELD_POOL`; bumped as
    /// new eval globals are assigned, capped at `+ EVAL_POOL`.
    eval_global_next: u32,
    /// Every builtin global NAME → its heap value, recorded at setup regardless of
    /// whether the running program referenced it. Lets `eval`'d code resolve
    /// builtins the main program never named (`eval("new RangeError()")`,
    /// `eval("Object.keys(x)")`) instead of seeing the never-declared sentinel.
    /// Values are permanent roots (traced in gc, never pruned).
    builtin_globals: std::collections::HashMap<String, u32>,
    /// Most-recent class value per class_id (filled by `MakeClass`), so a
    /// `super` call can reach its lexical superclass value at runtime.
    class_values: Vec<Option<Value>>,
    /// Bumped on every `class_values` write (class declaration / redefinition).
    /// The JIT method-inline `super.m()` arm bakes this epoch + a pointer to it
    /// and re-checks it each call: a re-executed class declaration swaps
    /// `class_values[home_class_id]` to a new class WITHOUT mutating the old
    /// prototype objects the inline's hop guards watch, so this coarse epoch is
    /// the discriminator that makes the inline match the interpreter's live
    /// `class_values[id]` resolution (a mismatch falls to the helper).
    mi_class_epoch: u32,
    /// Q7 method/accessor-inline receiver recording: per `(func_id<<32)|ip`
    /// CallMethod/GetProp/SetProp site that resolved a Class method/getter/setter,
    /// the ≤8 distinct receiver Value-bits seen at IC-fill time (warmup). The JIT
    /// planner reads this to bake a per-receiver-instance inline arm — the
    /// class-keyed IC records no instances, and the obj-reg/array trace is
    /// unreliable for `var o = arr[i]; o.x` (o is loaded indirectly). NOT a GC
    /// root: a stale/reused-slot entry is rebuilt-from-live at compile time and
    /// each arm is runtime identity+version-guarded, so a dead entry is harmless.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    mi_recv: rustc_hash::FxHashMap<u64, Vec<u64>>,
    /// Reusable scratch buffer for the `GetIndexConcat`/`SetIndexConcat` fused
    /// computed-key fast path: the `"prefix" + i` key is assembled here and
    /// looked up by `&str`, so the dictionary-churn idiom never allocates a
    /// throwaway heap string per access. Taken via `mem::take` for the duration
    /// of a single op (no aliasing) and returned; grows once, then reused.
    idx_key_scratch: String,
    /// JSON.stringify fast-path cache: `(obj_proto version, arr_proto version,
    /// either default prototype has a callable `toJSON`)`. Keyed on the two proto
    /// shape versions so any mutation that adds/removes `toJSON` on the default
    /// Object/Array prototype auto-invalidates it (no manual invalidation). Lets
    /// the serializer skip the per-value `get_prop(v,"toJSON")` chain walk for a
    /// plain object/array when no default prototype carries `toJSON`.
    json_default_tj: Option<(u32, u32, bool)>,
    /// Interpreter inline caches: per main-program or loader-recorded module
    /// function (outer index = func_id), per instruction (inner index = ip), a lazily-allocated
    /// per-site cache for the hot call/property paths. See `vm/ic.rs` for the
    /// guard model; intentionally NOT a GC root (entries are validated or
    /// re-read against live, guard-checked objects before any use).
    site_ics: Vec<Option<Box<[Option<Box<ic::SiteIc>>]>>>,
    /// Bounded interpreter `LoadConst` memo for short string literals, keyed by
    /// the immutable unified `(func_id, constant-slot)` pair. JavaScript strings
    /// are primitives, so repeated loads may share a representation; entries
    /// are explicit GC roots because the map is VM-internal. See
    /// `const_cache.rs` for the mutation exclusion and resource bounds.
    const_string_cache: rustc_hash::FxHashMap<u64, Value>,
    /// Per-function result of the conservative in-place-string-op scan. A
    /// function that can mutate a compiler-proved-unique string buffer must keep
    /// receiving a fresh literal representation, so none of its literals enter
    /// the shared cache.
    const_string_cache_funcs: rustc_hash::FxHashMap<u32, bool>,
    /// Latched at VM construction so `ZIPP_NO_CONST_STRING_CACHE=1` is a
    /// same-binary ablation with no environment lookup in the dispatch loop.
    const_string_cache_enabled: bool,
    heap: Heap,
    globals: Vec<Value>,
    /// One contiguous register file shared by all live frames; each frame owns
    /// the window `[base, base + reg_count)`.
    regs: Vec<Value>,
    frames: Vec<Frame>,
    /// Step budget / abort flag / execution trace, when an embedder has asked
    /// for them. `None` — the only state a default build can be in — means the
    /// dispatch hook returns immediately. Boxed so the `Option` costs one
    /// pointer in the hot struct rather than the recorder's whole footprint.
    #[cfg(feature = "instrument")]
    pub(crate) instr_rec: Option<Box<instrument::Recorder>>,
    /// Steps lent to the native tier, charged directly by compiled code as
    /// `sub QWORD [rdi + off], <block length>` — `rdi` is the VM pointer every
    /// region and whole-function body already holds, so this needs no spare
    /// register and no frame slot. Reconciled against the real budget by
    /// `meter_lend`/`meter_return`; 0 and unread in an unmetered VM.
    ///
    /// A plain field rather than part of the boxed `Recorder` so its offset from
    /// the VM pointer is fixed for compiled code, and so the hot path is one
    /// memory operand rather than a pointer chase.
    #[cfg(feature = "instrument")]
    pub(crate) jit_steps: i64,
    /// Lines produced by `Print` (console.log/info/debug → stdout), in order.
    pub output: Vec<String>,
    /// Lines produced by `console.error`/`console.warn` (→ stderr in node).
    pub errput: Vec<String>,
    /// Embedder host hook, backing the `HOST_CALL` native. `None` in every
    /// engine-internal path (`run`, `run_module_file`, the CLI, test262), so a
    /// stock build has no host surface at all; only `crate::embed` installs one.
    /// Taken out of the `Vm` for the duration of the call so the closure may
    /// re-enter the VM (a host that evaluates JS re-borrows `&mut self`).
    pub(crate) host: Option<Box<dyn FnMut(&str, &[String]) -> Result<String, String>>>,
    /// Monotonic-clock reading at VM start — the zero point for
    /// `performance.now()` (which reports fractional milliseconds elapsed
    /// since the program began). A host-installed clock's reading when one
    /// is installed (see `clock::now_mono_ms`).
    start_mono_ms: f64,
    /// The JS value currently being thrown, set when a `Throw` (or an internal
    /// error) begins unwinding and cleared when a `catch` handler receives it.
    /// Carrying the real `Value` (not just a message) lets `catch (e)` bind the
    /// exact thrown object/string/number, and survives propagation across
    /// nested `run_loop` invocations (builtin callbacks) until caught.
    pending_throw: Option<Value>,
    /// One-shot `new.target` for the NEXT frame entered: `construct` /
    /// `Reflect.construct` / `super(...)` set it to the constructor right before
    /// invoking the body; the frame-setup path consumes it into `Frame::new_target`
    /// and resets it to `undefined`, so ordinary calls observe `new.target` undefined.
    pending_new_target: Value,
    /// One-shot "the NEXT frame entered runs an eval program" flag, set right
    /// before `execute_eval_program` invokes the compiled eval script and
    /// consumed into `Frame::is_eval` at frame setup (same one-shot idiom as
    /// `pending_new_target`). Lets the legacy `f.caller` walk skip eval frames.
    pending_eval_frame: bool,
    /// One-shot "the NEXT do_eval compiles a CreateDynamicFunction wrapper"
    /// flag, set by `build_function_kind` (`new Function` & kin) and consumed
    /// into `compile_eval`'s `fn_ctor` (same one-shot idiom as
    /// `pending_eval_frame`). Suppresses the "anonymous" wrapper's self-name
    /// binding (constructor-binding.js).
    pending_fn_ctor_eval: bool,
    /// Set by a `Yield` op to hand a generator's yielded value (+ the yield's
    /// bytecode ip, for the resume point) back to `generator_method`, which
    /// `.take()`s it to distinguish a suspension from a normal return.
    pending_yield: Option<(Value, usize)>,
    /// Set by the `Yield`/`GenStart` ops to the suspending frame's active `try`
    /// handlers, so a SYNC generator's `generator_method` can park them in the
    /// `HeapObj::Generator` and restore them on resume (enabling `gen.throw(e)`
    /// to land in an enclosing `try`/`catch`). The ASYNC `drive_async_gen`
    /// consumer ignores this — async-gen suspension semantics are unchanged.
    pending_yield_handlers: Vec<Handler>,
    /// The suspending frame's dynamic EvalScope (or u32::MAX), captured with
    /// `pending_yield`; the parker stamps it on the generator so a resume can
    /// restore it (param-prologue evals outlive the GenStart suspension).
    pending_yield_eval_scope: u32,
    /// Set by `IterDelegate` when its not-done step is about to be yielded by the
    /// following `YieldDelegate`: the yielded value IS the inner iterator's raw
    /// result object, and `gen_resume` must forward it to the caller VERBATIM
    /// (spec GeneratorYield(innerResult)) instead of re-wrapping in a fresh
    /// `{value, done}` — the inner object's identity/extra props are observable.
    pending_yield_raw: bool,
    /// Set by an `Await` op (the awaited value + the Await's ip + the activation's
    /// live `try` handlers); `drive_async` `.take()`s it to suspend the async
    /// activation, mirroring `pending_yield`. Unlike generators, async activations
    /// PRESERVE handlers across a suspension so `try { await p } catch` works.
    pending_await: Option<(Value, usize, Vec<Handler>)>,
    /// Scratch slot for NewPromiseCapability: the capturing executor (CAP_EXECUTOR)
    /// writes its (resolve, reject) arguments here during `new C(executor)`, which
    /// `new_promise_capability` `.take()`s immediately after construction. `Some`
    /// while a capture is in flight (a second executor call → TypeError).
    cap_capture: Option<(Value, Value)>,
    /// FIFO microtask queue — the entire event loop (no timers/IO exist). Drained
    /// to empty by `drain_microtasks` after the main script returns; a microtask
    /// may enqueue more, which run in the same drain.
    microtasks: std::collections::VecDeque<Microtask>,
    /// The `.raw` array of a tagged-template strings object, keyed by the cooked
    /// array's heap index. Arrays don't carry named properties here, so a
    /// template object's `raw` lives in this side table (read by `get_prop`).
    template_raws: std::collections::HashMap<u32, Value>,
    /// GetTemplateObject memoization: the canonical frozen tagged-template object
    /// per source call site, keyed by (function id, per-function site index). The
    /// cached objects are permanent GC roots (they live as long as the realm).
    template_cache: std::collections::HashMap<(u32, u32), Value>,
    /// `(JIT ic site, receiver shape) -> slot`, for the JIT's named-property
    /// Get/Set MISS paths.
    ///
    /// The JIT's cache ways are keyed on receiver IDENTITY, so a site reading one
    /// property from many objects of the SAME shape thrashes all eight ways and
    /// misses every time — measured as a cliff at exactly 9 receivers, flat and
    /// 100% miss thereafter. Every one of those misses then re-ran `map.pos(key)`,
    /// a string scan, to rediscover a slot the shape already determines.
    ///
    /// Sound for the same reason the interpreter's shape guard is: a named
    /// property's key is a compile-time constant (`string_constants[name]`),
    /// and a shape fixes the whole key -> slot mapping. Both helpers still
    /// revalidate live bounds, key and descriptor before reading/writing.
    /// Bounded because shapes and sites are bounded; pure memo entries may be
    /// dropped freely.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit_shape_slot: rustc_hash::FxHashMap<(u32, u32), u32>,
    /// Prototype objects known to contribute NOTHING to a `for-in`: no own key
    /// is enumerable, and none can shadow a farther level because the walk ends
    /// there. Keyed by heap index, valued by the heap version the answer was
    /// computed at, so any structural change to the object invalidates it.
    ///
    /// This exists for one shape, which is almost every shape:
    /// `obj -> %Object.prototype% -> null`. `%Object.prototype%` carries a dozen
    /// own methods, all non-enumerable, and the walk re-derived that on EVERY
    /// `for-in` — running `spec_key_order` over its whole key list and testing
    /// each key. Measured, `for (k in o)` on a one-key object cost 185ns against
    /// node's 3.
    for_in_barren: rustc_hash::FxHashMap<u32, u32>,
    /// Lazy %RegExpStringIterator% state, keyed by the iterator's heap index —
    /// see [`proxy_regexp::RegexpIterRec`] for the record's fields.
    // W11 (B124): FxHashMap — the fused matchAll step probes this map once
    // per step (600k/run on regex-log-scan); SipHash was ~17ns of that probe.
    regexp_string_iters: rustc_hash::FxHashMap<u32, proxy_regexp::RegexpIterRec>,
    /// Drained matchAll scans for live `ITFB_FUSED` iterators, keyed by the
    /// same iterator heap index as (and pruned alongside) the
    /// `regexp_string_iters` record — see [`proxy_regexp::MatchBatch`].
    /// Integers only: GC never traces it (the paired record roots the
    /// matcher and subject).
    matchall_batches: rustc_hash::FxHashMap<u32, proxy_regexp::MatchBatch>,
    /// Allocation scratch for the batch's per-step publish (the capture Vec a
    /// `regress::Match` owns) and per-drain triple storage: taken empty,
    /// returned cleared, so the steady state re-mallocs neither. Plain ranges
    /// and integers — never traced, never pruned.
    matchall_caps_scratch: Vec<Option<std::ops::Range<usize>>>,
    matchall_flat_scratch: Vec<u32>,
    /// Single pending result for the exact MEM-only non-global exec region.
    /// No native/user-code re-entry is permitted while populated; every
    /// region exit first materializes it into its skipped global binding.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    regexp_scalar_exec_pending: Option<proxy_regexp::RegexpScalarExecPending>,
    /// RegExp legacy statics backing `RegExp.input`/`$_`/`lastMatch`/`$&`/
    /// `lastParen`/`$+`/`leftContext`/`$``/`rightContext`/`$'`/`$1`–`$9`, laid
    /// out as [input, lastMatch, lastParen, leftContext, rightContext, $1..$9]
    /// (14 slots). Refreshed by every successful RegExpBuiltinExec; empty until
    /// the first match (the accessors then yield their empty-string defaults).
    regexp_last: Vec<Value>,
    /// The eight `typeof` result strings, interned once in `setup_globals` (so
    /// below `gc_floor` and pinned for the VM's lifetime). Indexed by position in
    /// [`crate::bytecode::TYPEOF_NAMES`]. `UNDEFINED` before setup runs, which
    /// `typeof_value` treats as "not ready" and falls back to allocating.
    ///
    /// Before this, every unfused `typeof` allocated a fresh string and the
    /// `t === "number"` that follows content-compared it: 65ns per evaluation
    /// against the fused `TypeOfIs` form's 4ns.
    typeof_strs: [Value; 8],
    /// Deferred form of `regexp_last`'s slots 2..=13 after a successful match on a
    /// flat-ASCII subject: the twelve of them are all slices of that one subject,
    /// and materialising them cost ~8.7% of `regex-log-scan` for values almost no
    /// program reads (see `regexp_exec_impl` and `regexp_last_materialise`).
    /// `Some` means slots 2..=13 of `regexp_last` hold placeholders and the getter
    /// must materialise first. Cleared by the eager (non-ASCII) path.
    regexp_last_lazy: Option<RegexpLastLazy>,
    /// Native re-entry depth of `run_loop`: calls INSIDE one interpreter
    /// invocation stay flat (frame push + re-loop), but every NESTED entry — a
    /// builtin callback's `call_value`, a generator/async resume, a direct
    /// eval, a JIT bail-out — is a real Rust frame on the OS stack. Runaway
    /// recursion THROUGH those re-entries (a generator body re-entering its
    /// driver) would overflow the native stack long before MAX_FRAMES, so the
    /// nesting is capped (see MAX_RUN_LOOP_DEPTH) and surfaces as a catchable
    /// RangeError instead.
    run_loop_depth: u32,
    /// Active Rust recursion edges through guest-controlled Proxy/prototype
    /// meta-operations. Unlike `run_loop_depth`, this covers transparent native
    /// forwarding that never re-enters the bytecode loop.
    native_recursion_depth: u32,
    /// Consecutive `try_tail_reuse` activations since the last frame pop.
    /// Tail reuse grows neither `frames` nor the native stack, so runaway
    /// tail recursion is invisible to both depth guards — this streak counter
    /// is its budget (see MAX_TAIL_REUSE_STREAK). Reset by `pop_frame_with`;
    /// any terminating tail loop pops eventually and starts fresh.
    tail_reuse_streak: u32,
    /// Heap indices currently being stringified by Array join/toString/
    /// toLocaleString. A recursive encounter contributes an empty element;
    /// the bounded stack also prevents acyclic guest graphs from consuming the
    /// native/WASM stack inside one VM instruction.
    array_stringify_active: Vec<u32>,
    /// RegExp heap idx → EXACT WTF-8 bytes of its [[OriginalSource]], present
    /// ONLY when the pattern holds lone surrogates (the struct's `source:
    /// String` field is the LOSSY view — U+FFFD per surrogate). The `source`
    /// getter / `toString` / re-compile paths check this first so a
    /// lone-surrogate pattern round-trips exactly. Holds no `Value`s (pure
    /// bytes, idx-keyed): GC only needs the sweep-time retain in gc.rs.
    regexp_exact_source: std::collections::HashMap<u32, Vec<u8>>,
    /// Compiled-pattern cache, keyed by (source text, regress flags, byteopt):
    /// `RegExp.prototype[@@matchAll]` / `@@split` CONSTRUCT a species clone of
    /// the regex per call (per spec), which would re-parse + re-compile the
    /// same pattern on every iteration of a hot loop. The compiled program is
    /// immutable, so identical (source, flags) share one `Arc`.
    /// Lone-surrogate patterns (exact-bytes side channel) bypass the cache.
    /// Holds no `Value`s; cleared wholesale when it exceeds a small cap.
    regex_compile_cache:
        rustc_hash::FxHashMap<(String, String, bool), std::sync::Arc<regress::Regex>>,
    /// Reusable pointer/size pairs for deduplicating shared compiled RegExp
    /// programs during the periodic resident-memory audit. Regex literals,
    /// species clones, the compile cache, and inline ASCII twins can all hold
    /// the same `Arc`; charging it once per reference would reject harmless
    /// clones while still failing to describe actual resident memory.
    regex_program_audit_scratch: std::cell::RefCell<Vec<(usize, usize)>>,
    /// Lazy SameValueZero hash index over a Map/Set/WeakMap/WeakSet's backing
    /// Vec, keyed by the collection's heap index (see vm/collections.rs). An
    /// ABSENT entry means linear-scan behavior (always correct); an entry is
    /// built once a collection crosses the size threshold and maintained by
    /// the coll_* helpers every mutation routes through. Holds NO `Value`s
    /// (u32 hash tags -> u32 slot positions): GC only needs the sweep-time
    /// hygiene retain in gc.rs.
    collection_index: rustc_hash::FxHashMap<u32, collections::CollIndex>,
    /// Monotone counter minting a fresh private brand per class evaluation (1 = first;
    /// 0 = unbranded).
    next_private_brand: u64,
    /// A class method/getter/setter VALUE (and the class VALUE itself, for ctor /
    /// field-init / static blocks) heap index → the ORDERED lexical private-brand
    /// CHAIN of its class body: own brand first, then each lexically ENCLOSING
    /// class's brand. A private access threaded with lexical DEPTH d checks the
    /// receiver against `chain[d]` — the SPECIFIC declaring class's brand. Pruned by GC.
    method_brand: std::collections::HashMap<u32, Vec<u64>>,
    /// Extra private brands installed on a specific INSTANCE that its `map.class`
    /// chain does not cover — namely a constructor RETURN-OVERRIDE object, which
    /// receives a class's private fields/brand without becoming an instanceof that
    /// class. Checked by `instance_has_brand` alongside the class chain. Pruned by GC.
    instance_brand: std::collections::HashMap<u32, Vec<u64>>,
    /// Private NAMES (with the "#" prefix) declared by the class owning each brand.
    /// Lets a private access resolve "#x" to the innermost brand in the lexical
    /// chain whose class actually declares it (precise + shadow-aware) instead of
    /// accepting ANY chain brand. A name absent here falls back to the lenient
    /// any-brand check. Keyed by brand; pruned by GC to brands still referenced.
    brand_private_names: std::collections::HashMap<u64, Vec<(String, u8)>>,
    /// Brand -> heap index of the class VALUE that minted it (the DECLARING
    /// class), so a private access resolves members from the declaring class
    /// rather than the receiver's chain (kind-aware: shadowed/missing
    /// getter/setter errors). NOT a GC root: pruned when the class dies (no
    /// live accessor can then need it).
    brand_owner: std::collections::HashMap<u64, u32>,
    /// Lazily-created `.prototype` object for a function/class value, keyed by the
    /// callable's heap index. `Fn.prototype` / `Class.prototype` must return a
    /// stable object (identity: `C.prototype === C.prototype`), so it is built on
    /// first access and cached here. For a class it carries the own methods +
    /// `constructor`; for a plain function just `constructor`.
    prototypes: std::collections::HashMap<u32, u32>,
    /// Explicit `[[Prototype]]` recorded for an `Object.create(proto)` object,
    /// keyed by the new object's heap index (read by `Object.getPrototypeOf`).
    proto_of: crate::slot_table::SlotTable<Value>,
    /// Per-function-id LEARNED own-property count of the instances a user
    /// constructor produces, used to pre-size the next instance's `ObjMap`.
    ///
    /// An object literal gets its size from `NewObject { hint }` or its
    /// compiler-prepared `NewPlannedObject` key count; a constructor has no
    /// equivalent, so `new P(a, b)` started from an EMPTY map and every
    /// `this.x = v` paid an allocation-or-regrow of all three parallel vectors.
    /// Measured, that made a two-field constructor 71ns/field against the same
    /// two fields in a literal at 28ns/field.
    ///
    /// Learned rather than computed from the bytecode because the property count
    /// is a runtime fact — conditional assignments, a loop over a config object,
    /// a superclass — and getting it slightly wrong costs only capacity. It is a
    /// pure allocation hint: it never changes which properties exist, so a stale
    /// or absent entry is always correct.
    ctor_field_hint: Vec<u16>,
    /// Own properties set on a function value (`fn.x = y`, e.g. `assert.sameValue`),
    /// keyed by the callable's heap index. Functions can't carry an inline ObjMap,
    /// so their (rare) own props live here.
    fn_props: crate::slot_table::SlotTable<ObjMap>,
    /// The own-property state of every exotic object that has nowhere inline to
    /// put it, keyed by its heap index: an Array's named props (`arr.foo = 1`,
    /// and a regex match-result's `index`/`input`/`groups`), a Map/Set/Date/
    /// RegExp/Intl instance's assigned props, and the per-object
    /// extensible/sealed/frozen markers. `HeapObj::Array` is a dense `Vec<Value>`
    /// with no inline property map, so this mirrors `fn_props` for callables.
    ///
    /// NOT only named keys, despite the name: a SPARSE element past the dense
    /// prefix and a `defineProperty`'d index override both live here under the
    /// canonical decimal key, and `length` can too. That is why the dense-array
    /// fast paths cannot use mere presence as their disqualifier — they have to
    /// ask whether any key here actually names an element.
    ///
    /// Keyed by heap slot, so it is a [`crate::slot_table::SlotTable`] rather than
    /// a hash map: `exec`/`matchAll` park `index`/`input`/`groups` here, which on
    /// `regex-log-scan` inflates the table to hundreds of thousands of entries
    /// inside one phase. Ablating those four properties is −14.9% of that row and
    /// **79% of it survives when only the table insert is removed** — the cost was
    /// the container, not the properties. See the `slot_table` module note.
    arr_props: crate::slot_table::SlotTable<ObjMap>,
    /// Compact pristine `RegExpBuiltinExec` result metadata. This is disjoint
    /// from `arr_props`: the first operation that can change descriptors, key
    /// order, or extensibility moves the record there. Its Values are GC roots
    /// while the owning Array is live, just like an `arr_props` entry's values.
    regexp_result_props: crate::slot_table::SlotTable<RegexpResultProps>,
    /// Resizable ArrayBuffers: heap idx → maxByteLength. Presence marks a buffer
    /// as resizable (a side table avoids changing the ArrayBuffer heap variant).
    ab_max: std::collections::HashMap<u32, usize>,
    /// Length-tracking TypedArrays/DataViews (created on a resizable buffer with
    /// no explicit length): heap idx set. Their effective length follows the
    /// buffer's current byte length.
    ta_tracking: std::collections::HashSet<u32>,
    /// Heap indices of async activations (`AsyncState` / `AsyncGenerator`) that
    /// may still be suspended. A suspended activation is referenced only by the
    /// await-reaction it registered — a cycle the program cannot otherwise reach
    /// — so the collector must root it explicitly. It used to find them by
    /// LINEARLY SCANNING THE WHOLE HEAP on every collection, which costs
    /// ~2.8ns per slot in every program, including programs with no async code
    /// at all (measured 1.9% of the benchmark suite, streaming hundreds of MB to
    /// find nothing). Registering the two allocation sites instead makes the
    /// root phase proportional to the number of activations. Stale entries are
    /// pruned in the same pass that roots them, so completion needs no
    /// deregistration at any of the 11 sites that finish an activation.
    async_activations: Vec<u32>,
    /// Length-tracking DataViews (created on a resizable/growable buffer with no
    /// explicit byteLength): heap idx set. Their byteLength follows the buffer's
    /// current size, and byteLength/byteOffset throw (IsViewOutOfBounds) once the
    /// offset exceeds the shrunk buffer.
    dv_tracking: std::collections::HashSet<u32>,
    /// Callables expose `name`/`length` as synthesized own properties (computed
    /// from the proto, not stored). They're `configurable: true`, so `delete
    /// fn.name` must make them vanish — recorded here as `(heap_idx, 0=name |
    /// 1=length)`. Empty in normal programs; only `delete` on these keys fills it.
    deleted_callable_intrinsics: std::collections::HashSet<(u32, u8)>,
    /// Names of built-in globals (`Number`, `Date`, …) removed via
    /// `delete globalThis.X`. They live in `builtin_globals`/`globals`, not as own
    /// `global_this` entries, so deletion is recorded here and `global_by_name`
    /// gates on it — making get/has-own/descriptor all agree the property is gone.
    /// Empty in normal programs; cleared for a name when it's re-defined.
    deleted_globals: std::collections::HashSet<String>,
    /// THE GLOBAL-ROUTE EPOCH. Bumped whenever a global slot stops being the thing
    /// compiled code assumes every slot is — "a live, plain, writable data binding
    /// readable and writable as `[r12 + idx*8]`".
    ///
    /// Compiled code checks that assumption ONCE, at compile time
    /// (`globals_ok` in the Tier A/C gate, `region_globals_ok`,
    /// `build_leaf_inline_plan`), and those checks all rest on "once a slot holds a
    /// real value it can never go back". That is false in exactly two ways, and both
    /// were silent wrong answers at default thresholds (B66's first two open items):
    ///
    /// * `delete implicitG` returns the slot to `UNINITIALIZED`. The interpreter's
    ///   `LoadGlobal` throws ReferenceError on the sentinel; the emitted
    ///   `mov rax, [r12 + idx*8]` handed it back as a value, which prints
    ///   `undefined`.
    /// * a REAL own property appears on the global object behind a slot
    ///   (`Object.defineProperty(globalThis, "x", …)`), after which a store must
    ///   route through `[[Set]]` — `global_real_own_route`, which the interpreter
    ///   consults and native code did not.
    ///
    /// `0` — the value for every program that does neither — makes the entry check a
    /// single compare against zero, so the common path pays one `cmp`. See
    /// `Vm::jit_globals_still_routable`.
    global_route_epoch: u32,
    /// Memo for the (rare) non-zero-epoch entry check: `func_id` → the epoch at
    /// which its direct global accesses were last VALIDATED, and the verdict. Keeps a
    /// hot function that survived a global delete from re-scanning its whole body on
    /// every call. Only ever touched while `global_route_epoch != 0`.
    jit_global_route_ok: rustc_hash::FxHashMap<u32, (u32, bool)>,
    /// One u32 GENERATION per global slot, bumped by every NON-BYTECODE Rust
    /// write to `globals` (`Vm::bump_global_gen`). A spliced leaf call whose
    /// callee register provably holds slot g's value guards `global_gens[g]`
    /// (one 32-bit compare) instead of re-checking the callee bits+version per
    /// execution — sound only for slots NO bytecode store can ever hit (see
    /// `bytecode_stored_slots`), which makes the enumerated Rust writers
    /// exhaustive. Sized with `globals` at boot and NEVER reallocated (the JIT
    /// bakes element addresses); module bookkeeping draws from that same
    /// preallocated pool.
    global_gens: Vec<u32>,
    /// Global slots ANY bytecode store op targets (StoreGlobal / -Strict /
    /// -Resolved / -Dyn / EvalScopeSet), collected over the main program at
    /// boot and extended by every eval/Function registration. `slot_guard`
    /// keying declines for members — fail-closed: a slot writable by bytecode
    /// could change without a `bump_global_gen`.
    bytecode_stored_slots: rustc_hash::FxHashSet<u32>,
    /// Global-slot indices that `CheckGlobalResolvable` (a strict `name = …` on
    /// a name this program never declares) found UNRESOLVABLE when the reference
    /// was created, i.e. before the RHS evaluated. PutValue's ReferenceError
    /// fires only AFTER the RHS (an RHS throw wins), so the check records here
    /// and the matching `StoreGlobalResolved` raises at store time. Nested
    /// assignments stack (`a = (b = 1)`); a later check that finds the slot
    /// resolvable clears any stale entry a caught RHS throw left behind.
    strict_unresolvable_globals: Vec<u32>,
    /// Heap indices of arrays whose `length` was made non-writable via
    /// `Object.defineProperty(arr, "length", { writable: false })`. An array's
    /// length lives in the dense Vec with no per-array attribute state, so the
    /// (rare) non-writable flag is recorded here — read by the length descriptor,
    /// `arr.length = n`, and the push/pop/shift/unshift mutators.
    array_length_nonwritable: std::collections::HashSet<u32>,
    /// SPARSE arrays: heap idx → the JS `length` when it EXCEEDS the dense
    /// element count (`new Array(4e9)`, `a[3e9] = v`, `a.length = 2**32-1`).
    /// Sparse elements themselves live in `arr_props` under canonical index
    /// keys (GC-traced there); this table holds no Values — sweep-only. Absent
    /// for every fully-materialized array, so dense behavior is unchanged
    /// (`js_array_len` falls back to `items.len()`). Values are `1..=u32::MAX`
    /// (a JS length is at most 2^32-1) and always exceed both the dense length
    /// and `MAX_DENSE_ARRAY_LEN` at insertion time.
    array_js_len: crate::slot_table::SlotTable<u32>,
    /// The INDEXED-PROTOTYPE PROTECTOR, inverted: `false` means "nothing in a plain
    /// array's prototype chain can supply an integer index", which is the state every
    /// hole/OOB/append/bounded-gap fast path — interpreter and JIT helper alike —
    /// requires before it may answer absence itself.
    ///
    /// Sticky: it is set, never cleared, for the life of the VM. Reconstructing
    /// validity after the mutations that invalidate it is not worth the correctness
    /// risk, and they are rare. Two distinct mutations invalidate it, and BOTH must
    /// go through [`Vm::invalidate_indexed_proto_protector`]:
    ///
    /// 1. an integer-like own property DEFINED on `Array.prototype`/`Object.prototype`
    ///    (`note_array_proto_index`);
    /// 2. RE-PROTOTYPING one of them — `Object.setPrototypeOf(Array.prototype, x)`
    ///    splices a whole new chain in, and `x` may supply the index now or gain one
    ///    later. B66 left this half open: the protector saw only (1), so a hot
    ///    `a[5]` kept inventing `undefined` where the interpreter answered `"M5"`.
    ///
    /// Every reader must treat `true` as "MAY supply an index" — i.e. as a demand for
    /// the full protocol — never as an assertion that one exists.
    array_proto_has_index: bool,
    /// Heap indices of derived-class instances whose `super(...)` has run during
    /// construction (set by the SuperCtor ops, read+cleared in `construct`). A
    /// derived constructor that returns `undefined` without `super()` having run
    /// throws a ReferenceError. Transient — entries live only across one
    /// in-flight `construct`.
    super_called: std::collections::HashSet<u32>,
    /// Heap indices of derived-class instances whose `this` is still in the
    /// constructor TDZ (between derived-ctor entry and its `super()`
    /// completing). The `ThisCheck` op throws a ReferenceError while the
    /// instance is here. Transient — inserted by `construct`/`run_class_ctor`,
    /// removed on `super()` completion and cleared on every `construct` exit.
    this_tdz: std::collections::HashSet<u32>,
    /// The produced `this` of a completed `super(...)` whose parent ctor
    /// RETURN-OVERRODE the instance, keyed by the pre-allocated instance's heap
    /// index. A derived ctor that then returns `undefined` must yield THIS
    /// value as the construction result (reg 0 was rebound, but the original
    /// instance is what `construct` holds). Read+cleared by `construct` /
    /// `run_class_ctor`; transient like `super_called`.
    super_this: std::collections::HashMap<u32, Value>,
    /// Private FIELD storage — spec [[PrivateElements]] of kind "field":
    /// instance heap idx -> ((brand, name) -> value). Keyed by the
    /// per-evaluation brand so same-named fields of sibling classes or other
    /// evaluations never collide; invisible to reflection, proxy traps and
    /// property enumeration by construction. Values are GC roots; entries
    /// pruned when the instance dies.
    private_fields: std::collections::HashMap<u32, std::collections::HashMap<(u64, String), Value>>,
    /// Heap index of the canonical %eval% native — the DirectEval op's runtime
    /// identity check (a REBOUND global `eval` gets an ordinary call).
    eval_fn_idx: u32,
    /// EvalScope stamps for closures created in frames that carry one (so
    /// arrows/functions made during or after the eval still see its
    /// bindings). Keyed by the closure value's heap index; pruned at GC.
    closure_eval_scope: std::collections::HashMap<u32, u32>,
    /// EvalScope → the ENCLOSING EvalScope it was created under (child → parent).
    /// A closure stamped with its creator's scope that then runs a direct eval of
    /// its own gets a fresh scope of its own — which used to REPLACE the stamp,
    /// hiding the creator's eval `var`s from the rest of the closure body
    /// (staging/sm/regress/regress-554955-{2,3}.js: `b` read the global 1, not
    /// the 2 the creator's `eval("var b = 2")` had bound). Pruned at GC.
    eval_scope_parent: std::collections::HashMap<u32, u32>,
    /// Memo per func id: 0 unknown, 1 no, 2 yes — "code contains a sloppy
    /// function-context DirectEval". Drives EAGER EvalScope creation at
    /// closure-stamp time, so closures made before the eval call share the
    /// scope the eval later populates.
    sloppy_eval_memo: Vec<u8>,
    /// Heap indices of the built-in prototype objects (`Object.prototype`,
    /// `Function.prototype`, `Array.prototype`), built by `setup_globals`. Used as
    /// the [[Prototype]] for plain objects / functions / arrays so their methods
    /// resolve as values and `getPrototypeOf` returns them. 0 until set up.
    obj_proto: u32,
    fn_proto: u32,
    /// The `Function` constructor object — identified in `construct` /
    /// `call_ctor_as_function` so `new Function(args, body)` / `Function(...)`
    /// compile and return a real function (via `do_eval` of a function literal).
    function_ctor: u32,
    /// The %GeneratorFunction% / %AsyncFunction% / %AsyncGeneratorFunction%
    /// intrinsic constructors and their `.prototype` objects. Not global — reached
    /// via `Object.getPrototypeOf(function*(){}).constructor` etc. A generator/
    /// async/async-generator function's [[Prototype]] is the matching `*_fn_proto`
    /// (so its `.constructor` resolves to the matching ctor); the ctors build new
    /// functions via `do_eval` of `(function*|async ... anonymous(){...})`.
    gen_fn_ctor: u32,
    gen_fn_proto: u32,
    async_fn_ctor: u32,
    async_fn_proto: u32,
    asyncgen_fn_ctor: u32,
    asyncgen_fn_proto: u32,
    arr_proto: u32,
    /// %Array.prototype%'s own `length` (it is an Array EXOTIC object):
    /// tracks integer-index definitions on it; writable, non-enumerable,
    /// non-configurable.
    arr_proto_len: u32,
    /// The `Array` constructor (the `%Array%` intrinsic). 0 until setup. Used by
    /// ArraySpeciesCreate to take the fast dense path when the resolved species is
    /// just `%Array%` itself.
    array_ctor: u32,
    /// `String.prototype` — primitive string values delegate here for method
    /// access (`"x".charAt`, `"x".slice`, …, as values), 0 until `setup_globals`.
    str_proto: u32,
    /// `Map`/`Set`/`Date`/`Promise`.prototype — instances delegate here for
    /// method access as VALUES (`new Map().set`, `d.getHours`). 0 until set up.
    map_proto: u32,
    set_proto: u32,
    /// B183: memo for `collection_method_is_intrinsic`'s prototype half —
    /// per (kind: 0=Set,1=Map) × (method-name id) the proven
    /// `(proto_version, slot, method fn bits)`. A hit re-checks the LIVE
    /// version (key add/remove/redefine/freeze/setPrototypeOf all bump it)
    /// AND the live slot's Value bits (an in-place overwrite of the same
    /// slot bumps nothing — the B78-style value guard is what makes a
    /// version cache sound here, exactly as the string-method doc warns).
    /// Pure cache: entries self-validate, nothing invalidates them.
    coll_intrinsic_memo: [[Option<(u32, u32, u64)>; COLL_MEMO_NAMES]; 2],
    date_proto: u32,
    promise_proto: u32,
    /// `Number`/`Boolean`.prototype — number/boolean PRIMITIVES delegate here for
    /// method-as-value access (`(5).toFixed`, `true.toString`). 0 until set up.
    num_proto: u32,
    bool_proto: u32,
    /// `WeakMap`/`WeakSet`/`WeakRef`.prototype — instances delegate here.
    weakmap_proto: u32,
    weakset_proto: u32,
    weakref_proto: u32,
    finreg_proto: u32,
    /// The `WeakRef` / `FinalizationRegistry` constructor objects, so the
    /// value-form `[[Construct]]` (Reflect.construct / subclassing) can build an
    /// instance and honour `newTarget.prototype`.
    weakref_ctor: u32,
    finreg_ctor: u32,
    /// The `WeakMap` / `WeakSet` constructor objects, so the value-form
    /// `[[Construct]]` (Reflect.construct / subclassing) builds an instance,
    /// honours `newTarget.prototype`, and AddEntriesFromIterable via the adder.
    weakmap_ctor: u32,
    weakset_ctor: u32,
    /// Error prototypes, indexed by the canonical error kind (0=Error.prototype,
    /// 1=TypeError.prototype, …, 7=AggregateError.prototype). The subtype protos
    /// chain to `error_protos[0]`; every error instance links here via `proto_of`
    /// so `.constructor`/`.name`/`.message`/`.toString`/`instanceof` resolve. 0
    /// until `setup_globals`.
    error_protos: [u32; 8],
    /// The matching error constructor function values (`Error`, `TypeError`, …),
    /// indexed the same way. Stored on each proto as `.constructor` and used by the
    /// runtime `new (TypeError)()` / `Reflect.construct` path.
    error_ctors: [u32; 8],
    /// `Symbol.prototype` heap index (toString/valueOf/description) and the `Symbol`
    /// constructor object heap index (callable, NOT constructable). 0 until setup.
    symbol_proto: u32,
    symbol_ctor: u32,
    /// `BigInt.prototype` and the `BigInt` constructor object (callable, NOT
    /// constructable — like `Symbol`). 0 until setup.
    bigint_proto: u32,
    bigint_ctor: u32,
    /// `RegExp.prototype` and the `RegExp` constructor object. 0 until setup.
    regexp_proto: u32,
    regexp_ctor: u32,
    /// `%RegExpStringIteratorPrototype%` — the prototype of the iterator returned
    /// by `RegExp.prototype[Symbol.matchAll]` / `String.prototype.matchAll`.
    regexp_string_iter_proto: u32,
    /// The `%TypedArray%` intrinsic (abstract base ctor) + its prototype, the 11
    /// concrete TypedArray ctors + their prototypes (indexed by `kind`), and the
    /// `ArrayBuffer`/`DataView` ctors + prototypes. 0 until setup.
    ta_base_ctor: u32,
    ta_base_proto: u32,
    ta_ctors: [u32; 12],
    ta_protos: [u32; 12],
    arraybuffer_ctor: u32,
    arraybuffer_proto: u32,
    dataview_ctor: u32,
    dataview_proto: u32,
    /// `SharedArrayBuffer` ctor + prototype. A SharedArrayBuffer reuses the
    /// `HeapObj::ArrayBuffer` representation; its heap index is recorded in
    /// `shared_buffers` so property/prototype/`instanceof` resolution treats it as
    /// shared (growable instead of resizable, never detached, `SharedArrayBuffer`
    /// toStringTag). 0 until setup.
    sab_ctor: u32,
    sab_proto: u32,
    /// Heap indices of ArrayBuffers that are actually SharedArrayBuffers.
    shared_buffers: std::collections::HashSet<u32>,
    /// Heap indices of ArrayBuffers that are immutable (ES2026): their `immutable`
    /// getter is true, they are never resizable/detachable, and writes through a
    /// TypedArray view throw a TypeError.
    immutable_buffers: std::collections::HashSet<u32>,
    /// Heap indices of objects that have the [[ErrorData]] internal slot (real
    /// Error instances — built-in error ctors, internal throws, and
    /// `class X extends Error` instances). Distinguishes a true error from a
    /// plain object that merely inherits Error.prototype (which `is_error_instance`
    /// cannot, being prototype-chain based); drives `Error.prototype.stack`'s
    /// getter. Pruned on GC sweep.
    error_data: std::collections::HashSet<u32>,
    /// Set by the module loader just before executing a MODULE BODY: the
    /// next alloc_async (the body's own activation) marks its result promise
    /// in module_body_results. Consumed (mem::take) at alloc_async entry.
    pending_module_body_marker: bool,
    /// Result promises of MODULE BODY activations: they settle with
    /// UNDEFINED, never the body's completion value — spec Evaluate()
    /// resolves the capability with undefined, so a thenable completion
    /// value (e.g. a promise-valued last statement) must NOT be adopted
    /// (adoption makes a completed module look suspended and deadlocks its
    /// importers). Entries are removed at settle; pruned on GC sweep.
    module_body_results: std::collections::HashSet<u32>,
    /// Body promises of modules whose evaluation SUSPENDED (top-level await,
    /// or deps still pending): canonical path → the promise importers settle
    /// from. A LATER import of the same module (its namespace is already
    /// cached) re-publishes this as pending_module_body so the new importer
    /// chains on the SAME TopLevelCapability instead of resolving with the
    /// incomplete namespace. Entries persist after settlement.
    /// The bool: true = registered via the DEPS-PENDING (capability) path —
    /// an ANCESTOR whose body waits on other suspended modules (a cycle
    /// root); false = a direct top-level-await suspension. Late importers of
    /// a TLA module inside a cycle wait on the cycle ROOT's capability.
    module_body_promise: std::collections::HashMap<std::path::PathBuf, (Value, bool)>,
    /// Typed-module cache: (canonical path, type attribute) → namespace for
    /// `with { type: "json" | "text" }` imports — DISTINCT records from the
    /// same file's JS module. Values are GC roots like module_cache.
    typed_module_cache: std::collections::HashMap<(std::path::PathBuf, String), Value>,
    /// The function VALUE being invoked for the generator/async allocation in
    /// flight (set by the call site just before alloc_generator /
    /// alloc_async / alloc_async_generator; consumed at their entry).
    pending_gen_callee: Value,
    /// Generator / AsyncGenerator / AsyncState heap index → the function
    /// value that created it. Resumes bind it as Frame.callee so LoadCallee
    /// (a named function expression's self-name) keeps the caller-visible
    /// identity across suspensions. Values are GC roots; keys pruned on sweep.
    gen_callee: std::collections::HashMap<u32, Value>,
    /// Heap indices of IMMUTABLE cells — named function expressions' own-name
    /// bindings (MakeCellFnName). UpvalSet/StoreUpvalDyn writes through one
    /// no-op in sloppy code and throw TypeError in strict code. Pruned on GC
    /// sweep (the cells themselves are ordinary traced heap objects).
    fn_name_cells: std::collections::HashSet<u32>,
    /// Heap indices of `const`/`using` cells (MarkCellConst). A write through a
    /// closure or a direct eval (UpvalSet/StoreUpvalDyn) is a TypeError in BOTH
    /// modes — the compiler can only reject the declaring function's own
    /// assignments, since `Binding::Upvalue` carries no const-ness. Pruned on GC
    /// sweep like `fn_name_cells`.
    const_cells: std::collections::HashSet<u32>,
    /// `$262.evalScript` flag: the NEXT eval-program instantiation uses SCRIPT
    /// GlobalDeclarationInstantiation semantics (non-configurable brandNew
    /// bindings, lexical-collision SyntaxErrors, realm-persistent lexicals)
    /// instead of EvalDeclarationInstantiation. Consumed (mem::take) by
    /// prepare_eval_program.
    eval_script_gdi: bool,
    /// Realm slots bound as LEXICALS (let/const/class) by `$262.evalScript`
    /// scripts: collision SyntaxErrors + invisible to global-object property
    /// reflection (the main program's own lexicals live in
    /// program.lexical_globals).
    eval_lexical_globals: std::collections::HashSet<u32>,
    /// The `const` subset of eval_lexical_globals: a write to an INITIALIZED
    /// one throws TypeError (the initializing write sees UNINITIALIZED).
    eval_const_globals: std::collections::HashSet<u32>,
    /// Realm slots declared var/function by `$262.evalScript` scripts —
    /// HasVarDeclaration for later scripts' lexical-collision checks.
    eval_var_globals: std::collections::HashSet<u32>,
    /// Set while a PRELUDE script runs (`run_with_prelude` — the test262 harness).
    /// CreateGlobalVar/FunctionBinding then initializes the SLOT rather than
    /// parking an own property and leaving the slot UNINITIALIZED. A prelude is a
    /// realm-setup script, not observed code, and the own-property representation
    /// is only correct for paths that carry the interpreter's own-prop fallback:
    /// every JIT tier reads the slot directly, so an own-backed binding read from
    /// compiled code yields `undefined` and the two tiers disagree. Ordinary
    /// `$262.evalScript` keeps the own-property behaviour, which the
    /// non-configurability and reflection tests depend on.
    pub(crate) eval_prelude_mode: bool,
    /// `arguments` exotic objects (Array-backed): heap index → the live
    /// [[ParameterMap]] for a MAPPED one (sloppy + simple params), or `None`
    /// for an unmapped one (strict / non-simple). Presence alone drives the
    /// `[object Arguments]` toString tag and the Array-exotic carve-outs
    /// (ordinary `length`, no Vec growth). Pruned on GC sweep.
    arguments_objs: crate::slot_table::SlotTable<Option<ArgsMap>>,
    /// Generator/async-function activations with a MAPPED `arguments` object:
    /// state heap index (HeapObj::Generator / AsyncState / AsyncGenState) → the
    /// arguments object's heap index. Each resume re-links the [[ParameterMap]]
    /// to the freshly spliced frame (`relink_mapped_args`), so the live aliasing
    /// survives suspension. Pruned on GC sweep alongside `arguments_objs`.
    gen_args_obj: std::collections::HashMap<u32, u32>,
    /// Directory the running script was loaded from, used to resolve a dynamic
    /// `import(specifier)` against the filesystem (relative + bare specifiers).
    /// `None` when running from a string (eval/embedding) — then `import()` has no
    /// host loader and rejects.
    module_base_dir: Option<std::path::PathBuf>,
    /// Optional canonical filesystem boundary for the module loader. Every
    /// module read (including typed/deferred/source-phase imports and recursive
    /// re-exports) must remain below this directory. `None` preserves the
    /// unrestricted loader used by the compatibility CLI and test262. When set,
    /// aggregate path/byte/depth budgets in `engine::modules` also apply.
    module_root: Option<std::path::PathBuf>,
    /// Maximum bytes read from any one module while `module_root` is active.
    /// `None` preserves the unrestricted compatibility loader.
    module_max_bytes: Option<u64>,
    /// Canonical confined module paths and the largest byte length successfully
    /// observed for each. All loader variants share this ledger, so rereading a
    /// file (including as a typed/deferred/source-phase module) does not charge
    /// it twice; if it grows, only the growth is charged.
    module_read_bytes: std::collections::HashMap<std::path::PathBuf, u64>,
    /// Sum of `module_read_bytes`, bounded independently of the per-file cap.
    module_total_bytes: u64,
    /// Current confined module-loader recursion depth. This covers recursive
    /// evaluation, prescans, and export/request graph walks.
    module_load_depth: u32,
    /// Dynamic-import namespace cache: resolved module path → its namespace value.
    /// A module is evaluated at most once, so re-importing the same path returns the
    /// SAME namespace object (identity). Values are GC roots (modules persist).
    module_cache: std::collections::HashMap<std::path::PathBuf, Value>,
    /// Module Namespace exotic objects: namespace heap index → its (export name →
    /// global slot) map. Each module's exported bindings live in PER-MODULE fresh
    /// global slots (see run_eval_program module mode), so a namespace's [[Get]]
    /// reads the LIVE binding without colliding with another module's same-named
    /// export. [[Set]] is a no-op. Pruned on GC sweep.
    module_namespaces: std::collections::HashMap<u32, std::collections::HashMap<String, u32>>,
    /// Modules CURRENTLY being linked (cycle detection): resolved path → its OWN
    /// export name→live-slot map (re-exports not yet resolved). Present only during
    /// `import_module`; a re-export back into an in-progress module (a self/mutual
    /// cycle) resolves against this own-exports snapshot instead of recursing. Holds
    /// only slot indices into `globals` (a GC root), so it needs no separate rooting.
    module_own:
        std::collections::HashMap<std::path::PathBuf, std::collections::HashMap<String, u32>>,
    /// Per-namespace AMBIGUOUS export names (a name supplied by two different
    /// `export *` sources): excluded from the namespace; resolving one by name
    /// through `export {x} from` / `import {x}` is a SyntaxError.
    module_ambiguous: std::collections::HashMap<u32, std::collections::HashSet<String>>,
    /// CANONICAL live global slot of each module's NAMESPACE binding
    /// (canonical path → slot into `globals`, which is a GC root). Every
    /// `import * as ns from M` local and `export * as n from M` entry aliases
    /// THIS slot, so (a) the binding identity matches the spec's
    /// (module, ~namespace~) ResolvedBinding — two star-export routes to the
    /// same module's namespace are UNAMBIGUOUS — and (b) the namespace object
    /// is trivially identical everywhere. Slot indices only — no rooting.
    module_ns_slots: std::collections::HashMap<std::path::PathBuf, u32>,
    /// CANONICAL live global slot of each module's ModuleSource binding
    /// (`import source x from M`; the synthetic `<module source>` host module
    /// included). The slot holds the target's %AbstractModuleSource%-linked
    /// source object; aliasing it gives source re-exports the spec's
    /// (module, ~source~) binding identity. Slot indices only — no rooting.
    module_source_slots: std::collections::HashMap<std::path::PathBuf, u32>,
    /// Per-module `import.meta` objects (module key = its namespace heap index
    /// → the lazily-created meta object). Values are GC ROOTS (gc.rs).
    module_metas: std::collections::HashMap<u32, u32>,
    /// Unified-function-id ranges owned by loader-loaded modules:
    /// (start, end, namespace heap index). The `ImportMeta` op resolves the
    /// CURRENT module as the one whose range contains the executing frame's
    /// func id (each module's functions install contiguously at prepare time);
    /// ids outside every range (entry script / eval code) use the Vm-wide
    /// `import_meta` singleton.
    module_func_ranges: Vec<(u32, u32, u32)>,
    /// The lazily-created `import.meta` object (for NON-loader code: the entry
    /// script / direct `run_module` pipeline; host-defined, ordinary extensible
    /// null-proto). 0 = not yet allocated. Loader-loaded modules each get a
    /// DISTINCT object via `module_metas`.
    import_meta: u32,
    /// AgentCanSuspend: false when launched with ZIPP_CAN_BLOCK=0 (the
    /// test262 CanBlockIsFalse harness mode) — Atomics.wait then throws.
    can_block: bool,
    /// The body promise of the most recent `import_module` whose top-level
    /// await SUSPENDED (still Pending on return). Taken by the dynamic-import
    /// site (which settles its promise from it) or by a static importer
    /// (which rejects — stage-1 TLA). GC ROOT while set.
    pending_module_body: Option<Value>,
    /// Body/capability promises of STATIC dependencies that suspended at
    /// top-level await, collected during the CURRENT import_module's link
    /// (mark/split_off discipline keeps nested links separate). GC ROOTS.
    link_pending_deps: Vec<Value>,
    /// Registered Atomics.waitAsync waiters: (buffer heap idx, byte address,
    /// pending promise idx, optional deadline, global-registry waiter id).
    /// `notify` (local or via the cross-thread mailbox) resolves matching
    /// entries "ok"; a due deadline resolves "timed-out" in the event loop —
    /// unless its registry entry is already gone (a notify won the race).
    /// Buffer + promise indices are GC ROOTS; the id needs no rooting.
    async_waiters: Vec<(u32, usize, u32, Option<clock::Instant>, u64)>,
    /// `$262.agent.setTimeout` macrotasks: (due, callback). Callback Values
    /// are GC ROOTS.
    timer_queue: Vec<(clock::Instant, Value)>,
    /// Monotonic-clock reading at VM construction — the
    /// `$262.agent.monotonicNow()` epoch.
    vm_start_mono_ms: f64,
    /// Cross-agent state for the `$262.agent` worker subsystem (report FIFO +
    /// one handle per started agent). `None` until the first `agent.start` —
    /// a run that never starts an agent pays nothing; each worker Vm holds a
    /// clone of the same Arc.
    agent_shared: Option<std::sync::Arc<agents::AgentShared>>,
    /// Whether this Vm is the CLI entry agent (`Main`) or an `agent.start`
    /// worker thread's Vm (`Worker`).
    agent_role: agents::AgentRole,
    /// Worker-side `$262.agent.receiveBroadcast` callback, invoked as
    /// `cb(sab, id)` for each broadcast retrieved. GC ROOT (gc.rs).
    broadcast_cb: Value,
    /// This Vm's cross-thread `Atomics.waitAsync` wake channel: a `notify`
    /// in ANY agent pushes a woken waiter's id here and signals the condvar;
    /// the event loop sleeps on that condvar and drains the ids first each
    /// iteration. Created unconditionally so a remote notify never races a
    /// lazy init; holds no Values (nothing to root).
    mailbox: std::sync::Arc<agents::Mailbox>,
    /// The ShadowRealm whose code is CURRENTLY evaluating (heap index of the
    /// realm object), if any — global-name resolution inside `evaluate` binds
    /// non-builtin names to that realm's own slot table.
    active_realm: Option<u32>,
    /// Per-ShadowRealm global bindings: realm object idx → name → live global
    /// slot. Slot values live in `globals` (rooted wholesale); the map is
    /// pruned when its realm object dies.
    realm_globals: std::collections::HashMap<u32, std::collections::HashMap<String, u32>>,
    /// Canonical paths of modules whose BODY is currently executing — a
    /// deferred-namespace trigger targeting one is a TypeError (you cannot
    /// synchronously evaluate a module that is already mid-evaluation).
    executing_modules: std::collections::HashSet<std::path::PathBuf>,
    /// Modules whose evaluation completed ABRUPTLY: the thrown error is
    /// permanent — every later import re-throws it without re-running the
    /// body. Values are GC ROOTS.
    module_errors: std::collections::HashMap<std::path::PathBuf, Value>,
    /// Per-module DEFERRED namespace singleton (`import defer * as ns`):
    /// canonical path → the deferred namespace object. Values are GC ROOTS.
    deferred_ns_cache: std::collections::HashMap<(std::path::PathBuf, Option<String>), Value>,
    /// Deferred namespaces NOT YET evaluated: object idx → module path.
    /// Removed when a triggering access evaluates the module.
    /// Value carries the module TYPE alongside the path (`with { type: "json" }`):
    /// the trigger re-imports through `import_module`, and dropping the type there
    /// re-parsed a JSON module as JavaScript.
    deferred_ns_state: std::collections::HashMap<u32, (std::path::PathBuf, Option<String>)>,
    /// Async modules waiting on pending dependencies: capability-promise
    /// index → the state needed to run the body once the last dependency
    /// settles. Keys are GC roots (the capability promise must survive).
    deferred_mods: std::collections::HashMap<u32, DeferredModuleExec>,
    /// Canonical paths of modules whose IMPORT RESOLUTION is in flight —
    /// guards static-import cycles (self-imports alias instead).
    module_loading: std::collections::HashSet<std::path::PathBuf>,
    /// Static `export … from` (exported, imported, specifier) + `export *`
    /// specifiers + `export * as name from` (exported, specifier) entries +
    /// base dir of modules whose LINK is in flight: a cyclic resolve_export
    /// back into one walks these statically (spec ResolveExport through a
    /// cycle) instead of failing or re-evaluating.
    module_pending_reexports: std::collections::HashMap<
        std::path::PathBuf,
        (
            Vec<(String, String, String)>,
            Vec<String>,
            Vec<(String, String)>,
            Option<std::path::PathBuf>,
        ),
    >,
    /// `[[HomeObject]]` for OBJECT-LITERAL methods/accessors (and arrows nested in
    /// them), keyed by the closure's heap index → the object the method was defined
    /// in. `super.x` in such a method resolves via GetPrototypeOf(home). Class
    /// methods use the compile-time `home_class_id` path instead. GC treats each
    /// entry as a strong edge FROM the keyed closure to the value; dead closure/home
    /// cycles are collectible, and dead keys are pruned on sweep.
    closure_home: ClosureHomeTable,
    /// Lexically-captured `new.target` for ARROW closures created inside a
    /// constructed activation (and arrows nested in those), keyed by the
    /// closure's heap index. An arrow's own frame has no [[NewTarget]];
    /// `new.target` inside it must observe the enclosing function's. Only
    /// non-undefined values are recorded. GC traces the value from a reachable
    /// keyed closure; dead keys are pruned on sweep (mirrors `closure_home`).
    closure_new_target: std::collections::HashMap<u32, Value>,
    /// Memoized final shape per `FinalizeObject` plan, keyed by (unified
    /// func id, plan index). The value is the exact fold of `shape::add` over
    /// the plan's keys with data attributes — what per-field appends would
    /// produce. Shape ids are thread-local and the transition tree never prunes
    /// ids, so a memo computed on this VM's thread stays valid for its life;
    /// (fid, plan) keys are append-only over the program + eval tables, so an
    /// entry can never alias a different plan.
    finalize_shapes: rustc_hash::FxHashMap<(u32, u16), u32>,
    /// The lazily-compiled `Array.fromAsync` JS polyfill (an async function value).
    /// Compiled on first call via `do_eval`, then cached + GC-rooted. `None` until
    /// the first `Array.fromAsync(...)` invocation.
    from_async_fn: Option<Value>,
    /// The lazily-compiled `%AsyncIteratorPrototype%[@@asyncDispose]` JS polyfill
    /// (an async function). Compiled on first call via `do_eval`, cached + GC-rooted.
    async_dispose_fn: Option<Value>,
    /// The lazily-compiled body of GetDisposeMethod's async-hint fallback closure
    /// (see `sync_dispose_shim`). Cached + GC-rooted like the polyfills above.
    sync_dispose_shim_fn: Option<Value>,
    /// `DisposableStack` ctor + prototype. An instance is a plain Object linked to
    /// `disposablestack_proto`; its disposer stack + disposed flag live in
    /// `dispose_stacks` (the disposers are zero-arg callable thunks, GC-traced).
    disposablestack_ctor: u32,
    disposablestack_proto: u32,
    dispose_stacks: std::collections::HashMap<u32, (Vec<Value>, bool)>,
    /// Per-block `using`-declaration resource scopes, keyed by a monotonic id
    /// (`using_next_id`) that the `OpenUsingScope` op hands back in a register.
    /// Each value is the scope's list of disposers (a @@dispose method bound to its
    /// resource value), pushed by `RegisterDisposable` and drained LIFO by
    /// `DisposeScope` on block exit. The disposers are GC roots (see gc.rs); the
    /// entry is removed when its `DisposeScope` runs.
    using_resources: std::collections::HashMap<u32, Vec<Value>>,
    using_next_id: u32,
    /// `AsyncDisposableStack` ctor + prototype, and the set of dispose-stack
    /// instances that are ASYNC (their `use` prefers @@asyncDispose and their
    /// disposal goes through `disposeAsync`, which returns a Promise).
    asyncdisposablestack_ctor: u32,
    asyncdisposablestack_proto: u32,
    async_stacks: std::collections::HashSet<u32>,
    /// In-flight `AsyncDisposableStack.prototype.disposeAsync` drivers, keyed
    /// by the capability promise heap index. Each disposer's result is awaited
    /// before the next runs (the DISPOSE_ASYNC_STEP reactions re-enter
    /// `dispose_async_drive`). GC: keys + held Values are roots (gc.rs).
    dispose_async_state: std::collections::HashMap<u32, DisposeAsyncState>,
    /// `SuppressedError` ctor + prototype (ES2026). Unlike the 8 standard errors,
    /// it carries `error` + `suppressed` own properties; its prototype chains to
    /// %Error.prototype% so `instanceof Error` holds.
    suppressederror_ctor: u32,
    suppressederror_proto: u32,
    /// `ShadowRealm` ctor + prototype, and the set of branded instances. Note the
    /// realm is NOT truly isolated (evaluate reuses the shared global eval path);
    /// isolation-specific tests won't pass, but structure + evaluate of a string
    /// to a primitive + the argument/return TypeError checks do.
    shadowrealm_ctor: u32,
    shadowrealm_proto: u32,
    shadow_realms: std::collections::HashSet<u32>,
    /// Callable created by a ShadowRealm's `evaluate` → its realm INSTANCE heap
    /// idx. A WrappedFunction call switches `active_realm` to the target's
    /// realm so `globalThis.x` in its body binds the realm's slots at CALL time
    /// (not just during `evaluate`). u32→u32 (no Values); gc.rs roots the realm
    /// instances and retains live keys.
    shadow_fn_realm: std::collections::HashMap<u32, u32>,
    /// `%AbstractModuleSource%` ctor + prototype (source-phase-imports proposal).
    /// Reached via `$262.AbstractModuleSource`; the ctor always throws when
    /// called/constructed and the prototype carries the @@toStringTag accessor.
    abstractmodulesource_ctor: u32,
    abstractmodulesource_proto: u32,
    /// CreateResolvingFunctions [[AlreadyResolved]] records: `resolver_pair_next`
    /// hands out fresh pair ids; a pair id enters `resolved_pairs` the first time
    /// either function of the pair fires (ids only — no `Value`s, GC-inert).
    resolver_pair_next: u32,
    resolved_pairs: std::collections::HashSet<u32>,
    /// The intrinsic %Promise% constructor and %Promise.prototype.then% heap
    /// indices (the objects built in setup_globals), independent of later
    /// patches — identity anchors for the adoption fast-path check.
    promise_ctor_intrinsic: u32,
    promise_then_intrinsic: u32,
    /// The pristine-%Promise.prototype% proof resolved to slot indices plus
    /// the versions guarding them (see [`async_runtime::PromisePristineSlots`]).
    /// `None` until first proven pristine (and whenever last proven NOT
    /// pristine); validated per call by version compares, re-resolved by the
    /// full proof on any guard mismatch.
    promise_pristine_slots: Option<async_runtime::PromisePristineSlots>,
    /// The prototype/constructor half of `regexp_matchall_fast_ok` resolved
    /// to slot indices plus the versions guarding them (see
    /// [`proxy_regexp::MatchallFastSlots`]). `None` until first proven
    /// pristine (and whenever last proven NOT pristine); validated per call
    /// by version compares, re-resolved by the full gate on any mismatch.
    matchall_fast_slots: Option<proxy_regexp::MatchallFastSlots>,
    /// `$262.createRealm()` realm registry. `realms[r]` maps a MAIN-realm intrinsic
    /// prototype heap index to realm `r`'s corresponding prototype (realm 0 = the
    /// main realm, an empty map). `obj_realm` tags a heap index (a realm's
    /// constructor, its prototype, or an object created in it) with its realm id —
    /// `GetFunctionRealm`. Used so `Reflect.construct(C, args, newTargetFromRealmR)`
    /// with a non-object `newTarget.prototype` falls back to realm R's `%C.prototype%`.
    realms: Vec<std::collections::HashMap<u32, u32>>,
    obj_realm: std::collections::HashMap<u32, u32>,
    /// A realm constructor's heap index → the MAIN-realm constructor it mirrors, so
    /// `new other.Array()` / `other.Symbol('x')` route to the real construction /
    /// call behaviour (with the realm's prototype + realm tag).
    realm_ctor_main: std::collections::HashMap<u32, u32>,
    /// A `$262.createRealm()` child realm's GLOBAL object heap index → its realm
    /// id. Doubles as the `active_realm`/`realm_globals` key space for child
    /// realms (disjoint from ShadowRealm instance indices) and as the gate for
    /// the realm-global property interception in `get_member`/`set_prop`.
    /// Keys are GC roots (gc.rs) so the id mapping never goes stale.
    realm_global_objs: std::collections::HashMap<u32, u32>,
    /// `other.eval` / `realm.evalScript` function objects (per createRealm child):
    /// fn heap index → (the child's global-object index, kind 0=eval 1=evalScript).
    /// `call_value` intercepts these and runs the code with `active_realm` set to
    /// the child, so its globals bind in the CHILD's table. Keys are GC roots.
    realm_fns: std::collections::HashMap<u32, (u32, u8)>,
    /// The realm of the realm-COPIED built-in currently executing via
    /// `call_native` (None for main-realm built-ins): `this === %RegExp
    /// .prototype%`-style HOME checks resolve against the COPY's realm image
    /// (`native_home`). Saved/restored around the call; holds no Values.
    native_callee_realm: Option<u32>,
    /// Per-createRealm-child %ThrowTypeError% singleton (realm id → fn heap
    /// index): a strict arguments object built for a CHILD-realm function gets
    /// the child's thrower, so the `callee` accessor identity is stable within
    /// a realm and distinct across realms. Values are GC roots.
    realm_throw_type_errors: std::collections::HashMap<u32, u32>,
    /// An explicit `fn.prototype = value` assignment, for ANY value — including a
    /// NON-object (undefined/null/primitive) which the `prototypes` map (heap-only)
    /// can't hold. Consulted FIRST by the `.prototype` read / `prototype_of` /
    /// getOwnPropertyDescriptor, so a function whose prototype was set to a
    /// non-object reports it (and OrdinaryCreateFromConstructor then falls back).
    fn_proto_override: std::collections::HashMap<u32, Value>,
    /// Heap indices of `[[IsHTMLDDA]]` exotic objects (`$262.IsHTMLDDA`, the
    /// `document.all` emulation): `typeof` is "undefined", loose `== null`/`==
    /// undefined` is true, ToBoolean is false, and calling it returns undefined —
    /// yet it is otherwise an ordinary object (and `Object.is`/`===` do NOT treat
    /// it as undefined).
    is_htmldda: std::collections::HashSet<u32>,
    /// The `Proxy` constructor object (no `.prototype`). 0 until setup.
    proxy_ctor: u32,
    /// The `Temporal` namespace object + `Temporal.Duration`/`PlainDate` ctors/protos.
    temporal_ns: u32,
    duration_ctor: u32,
    duration_proto: u32,
    plaindate_ctor: u32,
    plaindate_proto: u32,
    plaintime_ctor: u32,
    plaintime_proto: u32,
    plaindatetime_ctor: u32,
    plaindatetime_proto: u32,
    instant_ctor: u32,
    instant_proto: u32,
    plainyearmonth_ctor: u32,
    plainyearmonth_proto: u32,
    plainmonthday_ctor: u32,
    plainmonthday_proto: u32,
    zoneddatetime_ctor: u32,
    zoneddatetime_proto: u32,
    /// `Temporal.ZonedDateTime` time-zone id per instance heap index (the id is a
    /// heap string, kept here so it's GC-traced — the `Temporal{fields:Vec<i64>}`
    /// layout can't hold a heap reference). The instance's `fields` are
    /// `[epochNs hi, epochNs lo, offsetNanoseconds]`.
    zdt_tz: std::collections::HashMap<u32, Value>,
    /// The `[[Calendar]]` slot of every calendar-bearing Temporal instance, as a
    /// `Cal` discriminant keyed by heap index. Absent means `iso8601`, so an
    /// ISO-only program never touches this map. Instances always store their
    /// *ISO* date in `fields`; the calendar is the view applied on read
    /// (see `vm/temporal/calendar.rs`).
    temporal_cal: std::collections::HashMap<u32, u8>,
    intl_ns: u32,
    intl_ctors: [u32; native::INTL_KINDS],
    intl_protos: [u32; native::INTL_KINDS],
    /// realm id (0 = main) → that realm's `%Intl%.[[FallbackSymbol]]`. Rooted
    /// indirectly: every entry is also in `symbol_keys`.
    intl_fallback_syms: std::collections::HashMap<u32, Value>,
    /// Monotonic counter giving each `Symbol()` a unique internal property key
    /// (`@@sym:N`), so distinct symbols never collide as object keys.
    symbol_counter: u64,
    /// The `Symbol.for` global registry: registry key string → the shared Symbol.
    symbol_registry: std::collections::HashMap<String, Value>,
    /// Internal prop_key (`@@iterator`, `@@sym:N`, …) → the Symbol value, so a
    /// symbol-keyed own property can be reflected back to its Symbol by
    /// `Object.getOwnPropertySymbols`.
    symbol_keys: std::collections::HashMap<String, Value>,
    /// `%Iterator.prototype%` (the shared root of all iterator prototypes; holds
    /// the ES2025 helper methods), `%IteratorHelperPrototype%` (next/return for
    /// lazy helpers, chains to the root), and the `Iterator` constructor.
    iterator_proto_root: u32,
    iterator_helper_proto: u32,
    /// `%GeneratorPrototype%` — the prototype of generator instances: next/return/
    /// throw + @@iterator + @@toStringTag, chaining to %Iterator.prototype% so a
    /// generator inherits the iterator-helper methods (`g().map(...)` etc.).
    gen_proto: u32,
    /// `%AsyncGeneratorPrototype%` — async-generator instances: next/return/throw
    /// (returning Promises) + @@asyncIterator + @@toStringTag, chaining to
    /// %AsyncIteratorPrototype%.
    asyncgen_proto: u32,
    /// The default `Array.prototype[Symbol.iterator]` (the `values` function), so
    /// array destructuring can fast-path plain arrays yet still honour a replaced
    /// `Array.prototype[Symbol.iterator]` (drain via the iterator protocol).
    default_array_iter: Value,
    /// The pristine %ArrayIteratorPrototype%.next (see default_array_iter).
    default_array_iter_next: Value,
    /// The single canonical %ThrowTypeError% intrinsic — shared by
    /// Function.prototype.{caller,arguments} and a strict (unmapped) arguments
    /// object's `callee` poison-pill, so all references compare `===`.
    throw_type_error: Value,
    iterator_ctor: u32,
    /// The test262 `$262` host object (0 until set up).
    dollar262: u32,
    /// Whether to install `$262` at all.
    ///
    /// It is a TEST HARNESS object — `agent.start()` spawns detached OS threads
    /// running their own VMs, `createRealm` builds a fresh global, `evalScript`
    /// and `detachArrayBuffer` reach past ordinary JS — and a host running code
    /// it did not write wants none of it. `zipp js` and the test262 runner keep
    /// it; `embed::compile_script` turns it off, because that API exists
    /// precisely for untrusted code.
    pub(crate) host_262: bool,
    /// `%ArrayIteratorPrototype%` — the prototype of Array entries/keys/values
    /// iterators (and the default array `@@iterator`). 0 until set up.
    array_iter_proto: u32,
    /// `%MapIteratorPrototype%` / `%SetIteratorPrototype%` — distinct prototypes so
    /// `getPrototypeOf(map.entries())` differs from a Set/Array iterator's.
    map_iter_proto: u32,
    set_iter_proto: u32,
    string_iter_proto: u32,
    /// The `globalThis` object (an empty Object at this heap index); property
    /// access on it is routed to the global slots by name. 0 until `setup_globals`.
    global_this: u32,
    /// `Math.random()` PRNG state (xorshift64*). Deterministically seeded, so a
    /// program's random sequence is reproducible run-to-run (and JIT-on == off).
    rng_state: u64,
    /// Native JIT tier (`feature = "jit"`): the mature x86-64 multi-tier backend
    /// and the guarded ARM64 integer baseline. Both share this VM's register
    /// window and bail to the interpreter at an exact bytecode ip.
    #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
    jit: crate::codegen::Jit,
    /// JIT on/off (set from `ZIPP_NOJIT` env var at construction) — lets a
    /// single binary A/B the JIT against the pure interpreter for honest
    /// measurement.
    #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
    jit_enabled: bool,
    /// Current native self-recursion depth (guards `jit_self_call`).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit_recurse_depth: u32,
    /// Current nesting of region call helpers (`jit_call_method_ic` /
    /// `jit_call_ic`): each level nests `run_loop` on the Rust stack, so it is
    /// capped at `JIT_REGION_CALL_MAX` (past it the call deopts to the
    /// interpreter's flat frames). Unlike `jit_recurse_depth`, a non-zero value
    /// does NOT close the JIT gates: nested execution may compile/enter
    /// functions and regions (the calling region re-derives its movable pinned
    /// pointers after every call helper).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit_call_depth: u32,
    /// Exact root identities for the current Tier-C native activation. Every
    /// nested activation saves the complete prior state below: frame-free
    /// callable identity must survive GC while native callers are suspended.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit_tierc_activation: TiercActivationState,
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit_tierc_activation_stack: Vec<TiercActivationState>,
    /// One-shot flag a region call helper sets when its bail is NOT a
    /// region-quality signal (depth-cap deopt, or a throw the call legitimately
    /// produced): `try_run_osr` consumes it and skips the deopt-eviction count
    /// for that exit, so legal recursion / caught throws don't evict hot regions.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    osr_deopt_exempt: bool,
    /// Heap strings interned AT REGION-COMPILE TIME for the region's multi-char
    /// string `LoadConst`s (their bits are embedded in the native code as
    /// immediates). A GC ROOT: compiled code is not traced, so without this the
    /// embedded strings could be swept and their slots reused while the region
    /// still materializes them. Grows only at compile time; bounded by the
    /// number of string constants across compiled regions.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit_const_strings: Vec<Value>,
    /// Method-inlining (MI) eligibility memo, indexed by unified `func_id`:
    /// `i32::MIN` = not yet computed; `-1` = ineligible (the off-frame evaluator
    /// declines this body's SHAPE — fall to the frame call); `≥ 0` = the
    /// straight-line body length to evaluate. Filled once per fid by
    /// `method_body_inlinable`, so the hot per-call path skips the body re-scan.
    /// Sound because a FuncProto's code is immutable for the program's life.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    mi_cache: Vec<i32>,
    /// B82 `%Function.prototype%` SLOT memo for the `f.call`/`f.apply` target
    /// splice: `(fn_proto version when computed, own "call" slot, own "apply"
    /// slot)`, `u32::MAX` = no such own key. Slot INDEXES are version-guarded
    /// (a key add/remove/descriptor change bumps `version_of(fn_proto)`), so
    /// the memo skips the per-call key scan; the slot's VALUE is re-read on
    /// every call (an in-place overwrite bumps nothing — the fn-bits re-read
    /// precedent), so `Function.prototype.call = g` falls back immediately.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    ci_pristine: (u32, u32, u32),
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
    /// Mark-sweep GC. `gc_floor` is the first collectable slot: everything below
    /// it (the interned strings + all built-ins allocated during setup) is pinned
    /// and never freed. `gc_lock` > 0 disables collection during native built-ins
    /// that hold un-rooted `Vec<Value>` working sets across a callback re-entry
    /// (array iteration, sort, iterate_to_vec, …). `gc_stress` (ZIPP_GC_STRESS)
    /// forces a collection at every safe point to flush out missed roots/edges.
    gc_floor: u32,
    #[cfg(not(feature = "safe-sandbox"))]
    gc_lock: u32,
    #[cfg(feature = "safe-sandbox")]
    gc_lock: std::rc::Rc<std::cell::Cell<u32>>,
    gc_stress: bool,
}

/// Method-name arity of [`Vm::coll_intrinsic_memo`]: get/set/has/add/delete/clear.
pub(crate) const COLL_MEMO_NAMES: usize = 6;

/// A thrown JS value rendered to a message (v1 throws are strings/RangeError).
#[derive(Debug)]
pub struct Thrown(pub String);

impl<'p> Vm<'p> {
    /// Execute one guest-controlled native recursion edge under the shared
    /// stack budget. Keeping the increment/decrement in this closure wrapper is
    /// deliberate: both `Ok` and ordinary abrupt completion (`Err`) restore the
    /// counter before control returns to JavaScript, so a caught RangeError does
    /// not poison later operations in the same VM.
    pub(crate) fn with_native_recursion_guard<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Thrown>,
    ) -> Result<T, Thrown> {
        if self.native_recursion_depth >= MAX_NATIVE_RECURSION_DEPTH {
            return Err(Thrown(
                "RangeError: Maximum call stack size exceeded".into(),
            ));
        }
        self.native_recursion_depth += 1;
        let result = f(self);
        self.native_recursion_depth -= 1;
        result
    }
}

// submodules (split from the former monolithic vm.rs)
mod access;
mod async_runtime;
mod builtins;
pub(crate) mod clock;
mod dispatch;
mod engine;
pub(crate) mod host_api;
mod indexing_date;
/// Step budget, cooperative abort, and the optional execution trace. Off by
/// default: the hook it adds to the inner dispatch loop is not something a
/// `zipp js file.js` run should pay for.
#[cfg(feature = "instrument")]
pub mod instrument;
mod mathjson;
mod natives;
mod props;
mod setup;
pub(crate) use builtins::builtin_stats;
mod agents;
mod array_ops;
pub(crate) mod bigint;
mod cldr_alias;
mod cldr_alias_data;
mod cldr_en;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
mod closure_make_jit;
mod coerce;
mod collections;
mod const_cache;
mod construct;
mod dtf_pattern;
mod enum_stream;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
mod field_stream;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
mod function_literal_jit;
mod gc;
mod helpers_datetime;
mod helpers_json;
mod helpers_misc;
pub(crate) mod helpers_num2;
mod helpers_numeric;
mod intl;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
mod iter_jit;
mod iterhelpers;
mod locale_tag;
mod misc_methods;
pub(crate) mod native;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
mod object_literal_jit;
mod proxy_regexp;
mod segmenter;
mod special_casing;
mod string_ops;
mod temporal;
mod typedarray;
mod values;

pub(crate) use async_runtime::async_inline_await_stats;
pub(crate) use async_runtime::async_stats;
pub(crate) use coerce::concat_pair_stats;
pub(crate) use coerce::pad2_concat_stats;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) use coerce::pad2_concat_stats_enabled;
pub(crate) use coerce::pad2_conditional_stats;
pub(crate) use gc::gc_gen_stats;
pub(crate) use gc::gc_nursery_stats;
pub(crate) use gc::gc_stats;
pub(crate) use gc::gc_young_budget_stats;
pub(crate) use helpers_misc::call_inline_stats;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) use helpers_misc::jit_shape_set_barrier;
pub(crate) use helpers_misc::computed_call_stats;
pub(crate) use helpers_misc::concat_set_stats;
pub(crate) use helpers_misc::cross_fill_stats;
pub(crate) use helpers_misc::ic_stats;
pub(crate) use helpers_misc::iter_region_stats;
pub(crate) use proxy_regexp::regexp_call_direct_enabled;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) use proxy_regexp::rx_scalar_exec_enabled;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) use proxy_regexp::rx_scalar_matchall_enabled;
pub(crate) use proxy_regexp::rxstats::dump as regexp_result_stats;
pub(crate) use proxy_regexp::rxstats::dump_call_direct as regexp_call_direct_stats;
pub(crate) use proxy_regexp::rxstats::dump_scalar_exec as regexp_scalar_exec_stats;
pub(crate) use proxy_regexp::rxstats::dump_scalar_matchall as regexp_scalar_matchall_stats;
pub(crate) use proxy_regexp::rxstats::dump_string_call_direct as regexp_string_call_direct_stats;
pub(crate) use proxy_regexp::string_regexp_call_direct_enabled;
pub(crate) mod prof;
pub(crate) use prof::dump as prof_stats;
pub(crate) use temporal::tzdb_version;
pub(crate) mod decorators;
mod ic;

pub(crate) use bigint::*;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) use closure_make_jit::*;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) use field_stream::*;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) use function_literal_jit::*;
pub(crate) use helpers_datetime::*;
pub(crate) use helpers_json::*;
pub(crate) use helpers_misc::*;
pub(crate) use helpers_num2::*;
pub(crate) use helpers_numeric::*;
pub(crate) use ic::{GetAct, SetAct, RET_DISCARD};
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) use iter_jit::*;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) use object_literal_jit::*;
