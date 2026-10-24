#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// `ZIPP_INTERPSTATS=1`: which functions the INTERPRETER executes, counted as
/// frame entries (a fresh activation the JIT did not take at ip 0) and resumes
/// (a return into a frame, or a JIT bail landing mid-function). One latched
/// flag check per frame transition and nothing per instruction, so the knob
/// is free when unset. It answers "what is still interpreted" on a row whose
/// profile shows dispatch time while every hot function reports compiled.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod interp_stats {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Mutex;

    static STATE: AtomicU8 = AtomicU8::new(0);
    static TABLE: Mutex<Option<HashMap<u32, (String, u64, u64)>>> = Mutex::new(None);
    /// `(func, region entry ip, resume ip) -> runs`: how often each compiled
    /// loop region was entered and where it handed control back. A resume ip
    /// inside the region's bounds is a guard bail; the interpreter then runs
    /// the rest of that iteration itself.
    static REGIONS: Mutex<Option<HashMap<(u32, u32, u32), u64>>> = Mutex::new(None);

    #[inline]
    pub(crate) fn enabled() -> bool {
        match STATE.load(Ordering::Relaxed) {
            1 => true,
            2 => false,
            _ => {
                let on = std::env::var_os("ZIPP_INTERPSTATS").is_some();
                STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
                on
            }
        }
    }

    pub(crate) fn bump(func_id: u32, name: &str, resume: bool) {
        let mut guard = TABLE.lock().unwrap_or_else(|e| e.into_inner());
        let table = guard.get_or_insert_with(HashMap::new);
        let entry = table
            .entry(func_id)
            .or_insert_with(|| (name.to_string(), 0, 0));
        if resume {
            entry.2 += 1;
        } else {
            entry.1 += 1;
        }
    }

    // The only region runner is the x86-64 JIT's.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn bump_region(func_id: u32, entry_ip: u32, resume_ip: u32) {
        let mut guard = REGIONS.lock().unwrap_or_else(|e| e.into_inner());
        *guard
            .get_or_insert_with(HashMap::new)
            .entry((func_id, entry_ip, resume_ip))
            .or_insert(0) += 1;
    }

    /// `(func_id, entry ip, resume ip, runs)`, most runs first.
    pub fn dump_regions() -> Vec<(u32, u32, u32, u64)> {
        let guard = REGIONS.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(u32, u32, u32, u64)> = guard
            .iter()
            .flat_map(|table| table.iter())
            .map(|(&(f, e, r), &n)| (f, e, r, n))
            .collect();
        crate::cold_sort::sort_by(&mut rows, |a, b| b.3.cmp(&a.3));
        rows
    }

    /// `(func_id, name, entries, resumes)`, busiest first.
    pub fn dump() -> Vec<(u32, String, u64, u64)> {
        let guard = TABLE.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(u32, String, u64, u64)> = guard
            .iter()
            .flat_map(|table| table.iter())
            .map(|(&id, (name, entries, resumes))| (id, name.clone(), *entries, *resumes))
            .collect();
        crate::cold_sort::sort_by(&mut rows, |a, b| (b.2 + b.3).cmp(&(a.2 + a.3)));
        rows
    }
}

/// `ZIPP_NO_LEAN_CALL=1`: every call-cache hit pushes its frame through
/// `setup_call` and every return pops through `pop_frame_with`, out of line
/// (the same-binary comparator for `setup_call_lean` / `pop_frame_fast`).
/// [`Vm::lean_call_desc`]'s flags: a plain call takes the lean push.
#[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
pub(crate) const LEAN_DESC_OK: u32 = 1 << 24;
/// ... and the callee is sloppy, so its `this` is the global object.
#[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
pub(crate) const LEAN_DESC_SLOPPY: u32 = 1 << 25;
/// Set in every descriptor, so 0 means none.
#[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
pub(crate) const LEAN_DESC_KNOWN: u32 = 1 << 26;

#[inline(always)]
fn lean_calls_enabled() -> bool {
    if crate::WASM_STATIC_SWITCHES {
        return true;
    }
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_LEAN_CALL").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

impl<'p> Vm<'p> {
    /// Drives execution from the current frame until the frame that was current
    /// on entry returns (frames drops to `stop_depth`), catching thrown values
    /// at `try` handlers along the way. `run()` passes 0 (drain everything);
    /// `call_value` passes the pre-call depth (run one nested call).
    ///
    /// On a throw, [`Self::dispatch_body`] returns `Err`; we look up the thrown
    /// value and unwind to the nearest handler at or above `stop_depth`. If one
    /// exists, execution resumes at its catch target; otherwise the throw
    /// propagates out (with `pending_throw` left set so an enclosing `run_loop`
    /// â€” e.g. the caller of a builtin callback â€” can still catch it).
    pub(crate) fn run_loop(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
        // Tagged explicitly (not merely "not Jit"): a region call helper
        // re-enters the interpreter, and without this its nested time would be
        // charged to the enclosing compiled region.
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Interp);
        // Native re-entry guard: a NESTED run_loop (builtin callback, generator
        // resume, direct eval, JIT bail) is a real Rust frame, and runaway
        // recursion through such re-entries would exhaust the OS stack before
        // MAX_FRAMES ever fires. Cap the nesting so it throws a catchable
        // RangeError (staging/sm/extensions/recursion.js) instead of crashing.
        if self.run_loop_depth >= MAX_RUN_LOOP_DEPTH {
            return Err(self.run_loop_depth_exceeded(stop_depth));
        }
        self.run_loop_depth += 1;
        let r = self.run_loop_body(stop_depth);
        self.run_loop_depth -= 1;
        r
    }

    /// The re-entry cap refused a nested `run_loop`. Every caller pushed the
    /// frame(s) it wanted run BEFORE calling in, so leave the stack exactly as
    /// an uncaught throw out of that loop would: a real RangeError in
    /// `pending_throw`, and every frame above `stop_depth` popped with its
    /// register window. Callers that swallow the error (the async driver
    /// rejecting its promise, a Promise executor) otherwise kept running with
    /// the refused frame still on top, so the caller's next `Return` popped
    /// it instead of its own and re-dispatched its own frame from a stale ip
    /// — the function body ran again, hit the cap again, forever. The refused
    /// frames' own handlers are deliberately not consulted: nothing may run in
    /// them at the cap (a resumed generator/async frame carries its handlers).
    #[cold]
    fn run_loop_depth_exceeded(&mut self, stop_depth: usize) -> Thrown {
        const MSG: &str = "RangeError: Maximum call stack size exceeded";
        while self.frames.len() > stop_depth {
            let top = self.frames.len() - 1;
            if self.frames[top].args_obj != u32::MAX {
                let aidx = self.frames[top].args_obj;
                self.args_sync_dense(aidx);
            }
            let f = self.frames.pop().unwrap();
            self.regs.truncate(f.base);
            if f.gen != u32::MAX {
                self.gen_complete(f.gen);
            }
        }
        // Created in the CALLER's context: the refused callee never ran.
        let v = self.alloc_error_from_message(MSG);
        self.realm_adopt_error(v);
        self.pending_throw = Some(v);
        Thrown(MSG.into())
    }

    /// CopyDataProperties (object rest) re-reads each snapshotted key's own
    /// descriptor AT COPY TIME: a getter run for an earlier key may have
    /// deleted a later one or made it non-enumerable, and such a key is
    /// skipped. Only an ordinary object's ObjMap can change under the walk; a
    /// string source's index keys are fixed.
    pub(crate) fn rest_key_still_enumerable(&self, src: Value, key: &str) -> bool {
        match self.heap.get(src.heap_index()) {
            HeapObj::Object(map) => map.pos(key).is_some_and(|i| map.attr_at(i).enumerable),
            _ => true,
        }
    }

    fn run_loop_body(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
        loop {
            // The main loop is compiled twice: with the lite loop's hook for an
            // interpreter-only VM (`lite_entry_ok`), and without it for every
            // other, whose code is then what it was before the lite loop (the
            // hook alone moved the JIT build's Python calls by 4-5%).
            #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
            let body = if self.lite_entry_ok() {
                self.dispatch_body::<true>(stop_depth)
            } else {
                self.dispatch_body::<false>(stop_depth)
            };
            #[cfg(any(feature = "meter-only", feature = "safe-sandbox"))]
            let body = self.dispatch_body::<false>(stop_depth);
            match body {
                Ok(v) => return Ok(v),
                Err(t) => {
                    let tv = match self.pending_throw {
                        Some(v) => v,
                        None => {
                            // Internal error (TypeError/RangeError/â€¦) with no
                            // explicit thrown value: synthesise a real Error
                            // object so `catch (e)` sees `e.name`/`e.message` and
                            // `e instanceof TypeError`, matching JS.
                            let v = self.alloc_error_from_message(&t.0);
                            // The frames are still intact here, so a throw inside
                            // a createRealm child's function adopts the CHILD's
                            // error-constructor identity (no-op in the main realm).
                            self.realm_adopt_error(v);
                            self.pending_throw = Some(v);
                            v
                        }
                    };
                    if self.unwind_to_handler(tv, stop_depth) {
                        self.pending_throw = None; // caught â€” resume at catch
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
    /// into its registers and resumes at the finally target â€” `EndFinally`
    /// re-throws after the finally runs. Either way execution resumes (`true`). If
    /// the boundary is reached with no handler, return `false` (propagate).
    pub(crate) fn unwind_to_handler(&mut self, tv: Value, stop_depth: usize) -> bool {
        // A Python program's frames record where the exception met them
        // (`vm::py_rt`): at its own instruction in the first frame, at the
        // call it made in every frame below.
        #[cfg(feature = "python")]
        let mut exact = true;
        while self.frames.len() > stop_depth {
            let top = self.frames.len() - 1;
            if let Some(h) = self.frames[top].handlers.pop() {
                #[cfg(feature = "python")]
                if self.py_unwind_candidate(top) {
                    self.py_unwind_frame(top, exact, tv, true);
                }
                let base = self.frames[top].base;
                match h {
                    Handler::Catch { target, reg } => {
                        self.regs[base + reg as usize] = tv;
                        self.frames[top].ip = target as usize;
                    }
                    Handler::Finally {
                        target,
                        kind_reg,
                        val_reg,
                    } => {
                        self.regs[base + kind_reg as usize] = Value::int(2); // throw
                        self.regs[base + val_reg as usize] = tv;
                        self.frames[top].ip = target as usize;
                    }
                }
                return true;
            }
            // A Python code object's frame guard (its implicit handler).
            #[cfg(feature = "python")]
            {
                if self.py_unwind_candidate(top) && self.py_unwind_frame(top, exact, tv, false) {
                    return true;
                }
                exact = false;
            }
            // No handler in this frame: discard it and its register window.
            // Flush a MAPPED arguments object's live formal registers into its
            // dense store first, or an escaped object reads stale entry values.
            if self.frames[top].args_obj != u32::MAX {
                let aidx = self.frames[top].args_obj;
                self.args_sync_dense(aidx);
            }
            let f = self.frames.pop().unwrap();
            self.regs.truncate(f.base);
            // A generator's step run in its consumer's loop
            // (`gen_enter_inline`): the throw completes the generator.
            if f.gen != u32::MAX {
                self.gen_complete(f.gen);
            }
        }
        false
    }

    /// Whether [`Self::unwind_to_handler`] with this `stop_depth` would find a
    /// handler: some frame above `stop_depth` has one.
    pub(crate) fn handler_within(&self, stop_depth: usize) -> bool {
        // A Python program's frames have implicit handlers (their frame
        // guards, `vm::py_rt`): answered as if one were found, which only
        // defers building the message (`run_loop` builds it when the throw
        // leaves uncaught).
        #[cfg(feature = "python")]
        if self.py_rt.is_some() {
            return true;
        }
        self.frames
            .get(stop_depth..)
            .is_some_and(|fs| fs.iter().rev().any(|f| !f.handlers.is_empty()))
    }

    /// On a non-throw leave of the top frame (`return`, and later break/continue),
    /// run any pending `finally` first. Discards `Catch` handlers we are exiting;
    /// on the innermost `Finally`, deposits the completion (`kind` 1=return + the
    /// `value`) into its registers and returns its target so the caller resumes
    /// there (`EndFinally` later re-leaves). Returns `None` when no finally is
    /// pending â€” the caller performs the real leave (pop the frame).
    pub(crate) fn route_through_finally(&mut self, kind: i32, value: Value) -> Option<u32> {
        let top = self.frames.len() - 1;
        let base = self.frames[top].base;
        while let Some(h) = self.frames[top].handlers.last().copied() {
            match h {
                Handler::Finally {
                    target,
                    kind_reg,
                    val_reg,
                } => {
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

    /// Route a `break`/`continue` (`JumpFinally`) that exits one or more `try`
    /// blocks. Pops handlers in the current frame until the stack is back to
    /// `floor` (the handler depth at the target loop): a `Catch` being exited is
    /// discarded, and the first `Finally` encountered is run first â€” a kind-3
    /// (jump) completion is deposited (the `floor` packed into the kind word's
    /// upper bits, the jump `target` in the value register) so `EndFinally`
    /// resumes the unwind toward `target`. Returns the finally body to run, or
    /// `None` when the stack is already at `floor` (jump straight to `target`).
    pub(crate) fn route_jump_through_finally(&mut self, target: u32, floor: usize) -> Option<u32> {
        let top = self.frames.len() - 1;
        let base = self.frames[top].base;
        while self.frames[top].handlers.len() > floor {
            let h = self.frames[top].handlers.pop().unwrap();
            if let Handler::Finally {
                target: ftarget,
                kind_reg,
                val_reg,
            } = h
            {
                self.regs[base + kind_reg as usize] = Value::int(3 | ((floor as i32) << 2));
                self.regs[base + val_reg as usize] = Value::int(target as i32);
                return Some(ftarget);
            }
            // A `Catch` handler being exited has no body to run â€” discard it.
        }
        None
    }

    /// The inner execution loop: runs ops in the current frame until a frame
    /// transition (a call pushes / a return pops) or a throw. Returns the value
    /// when the `stop_depth` frame returns, or `Err` to begin unwinding.
    ///
    /// `LITE`: the VM was interpreter-only when this call began
    /// (`lite_entry_ok`), so frames may run in the lite loop and the native
    /// tiers and the per-frame statistics are skipped. Neither the JIT nor the
    /// statistics switch on during a run, so nothing is lost.
    pub(crate) fn dispatch_body<const LITE: bool>(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
        // Set when the lite loop has just handed the top frame over: the main
        // loop then runs it to its next frame transition.
        #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
        let mut lite_skip = false;
        loop {
            // GC safe point on every frame transition (call/return): no native
            // built-in is mid-flight holding an un-rooted Vec here, and all live
            // Values are in regs/frames/globals/side-tables (the GC root set).
            self.maybe_gc();
            // Snapshot the current frame's coordinates. `ip` is advanced as a
            // local and written back only on frame transitions / loops. (They
            // stay immutable here: switching frames inside the inner loop by
            // reassigning them was tried and made every call 4-10% slower, as
            // the loop then carried them all.)
            let frame_idx = self.frames.len() - 1;
            let func_id = self.frames[frame_idx].func;
            let base = self.frames[frame_idx].base;
            let mut ip = self.frames[frame_idx].ip;
            let cur_closure = self.frames[frame_idx].closure;
            // This activation's upvalue list (`closure_upvalue_cached`),
            // taken on its first upvalue access.
            #[cfg(not(feature = "safe-sandbox"))]
            let mut upv: (*const u32, usize) = (std::ptr::null(), 0);
            // `func()` returns a reference with the immutable PROGRAM lifetime,
            // so the hardened interpreter can cache the current function's
            // bytecode once per frame transition without extending a borrow of
            // the VM across any mutating instruction arm.
            let running: &'p crate::bytecode::FuncProto = self.func(func_id as usize);
            #[cfg(feature = "safe-sandbox")]
            let code: &'p [Instr] = running.code.as_slice();
            #[cfg(not(feature = "safe-sandbox"))]
            let code: *const Vec<Instr> = &running.code;
            // SAFETY: `code` borrows immutable program data that outlives the
            // loop; we never mutate program functions during execution.
            #[cfg(not(feature = "safe-sandbox"))]
            let code: &Vec<Instr> = unsafe { &*code };
            // The running function's string constants, kept for the same
            // reason: a property read, a property write and a method call
            // each name their key through them, and each used to find the
            // function again first (a bounds-checked double index, and in a
            // size-first build a call).
            let strs: &'p [String] = running.string_constants.as_slice();
            // â”€â”€ JIT tier â”€â”€
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
            // interpreter re-enters native, recurses 256, deoptsâ€¦ forever,
            // because the per-call native depth counter resets each return and
            // interpreter frames never reach MAX_FRAMES. Staying interpreted in
            // that subtree lets frames accumulate monotonically â†’ RangeError.
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            if !LITE
                && ip == 0
                && self.jit_enabled
                && self.jit_recurse_depth == 0
                // Main-program, module and installed eval/new Function protos
                // are immutable and JIT-safe (each runtime kind has an off
                // switch; see `jit_func_eligible`).
                && self.jit_func_eligible(func_id)
                // Dense tier check first: a known-ineligible (FN_DEAD) function
                // pays ONE array read per call here, not 2-3 hash probes.
                && self.jit.fn_state(func_id) != crate::codegen::FN_DEAD
                && !self.func(func_id as usize).is_generator
                && !self.func(func_id as usize).is_async
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
                    //     helper signalled deopt with `pending_throw` set â€” the
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
                    self.jit_compile_on_entry(func_id, base);
                }
            }

            // ARM64 baseline whole-function tier. Its accepted bytecode is
            // deliberately call-free, so it needs neither the x86 self-call
            // depth guard nor heap/global helper plans: a guard miss simply
            // reports this instruction and the unchanged interpreter resumes.
            #[cfg(all(feature = "jit", target_arch = "aarch64"))]
            if !LITE
                && ip == 0
                && self.jit_enabled
                && self.jit_func_eligible(func_id)
                && self.jit.fn_state(func_id) != crate::codegen::FN_DEAD
                && !self.func(func_id as usize).is_generator
                && !self.func(func_id as usize).is_async
            {
                if let Some((result, bail)) = self.try_run_jit(func_id, base) {
                    if bail == crate::codegen::NO_BAIL {
                        if self.pop_frame_with(result, stop_depth) {
                            return Ok(result);
                        }
                        continue;
                    }
                    ip = bail as usize;
                } else if self.jit.record_and_should_compile(func_id) {
                    let proto: *const crate::bytecode::FuncProto = self.func(func_id as usize);
                    // SAFETY: program bytecode is immutable for the VM's life.
                    self.jit.compile(func_id, unsafe { &*proto });
                }
            }

            // A simple function runs in the lite loop (`vm/lite.rs`), and so
            // do the simple frames it calls or returns to; it hands back what
            // it does not run.
            #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
            {
                let try_lite = !std::mem::replace(&mut lite_skip, false);
                if LITE && try_lite && self.lite_eligible(func_id) {
                    self.frames[frame_idx].ip = ip;
                    match self.run_lite(stop_depth)? {
                        super::lite::LiteExit::Done(v) => return Ok(v),
                        super::lite::LiteExit::Resume => {
                            lite_skip = true;
                            continue;
                        }
                    }
                }
            }

            // What the dispatch below matches on: the function's
            // superinstruction copy (`FusedCode`, built on its first entry)
            // when anything in it fused, else its code. Only the `match`
            // reads it; every lookahead, cold path and tier reads `code`,
            // which it equals position for position. Chosen past the native
            // tiers: a frame they run whole needs none, and code placed
            // before their entry slowed a JIT row by 7% (warm-router).
            let icode: &'p [Instr] = if self.interp_fuse_ok() {
                running.fused.entered(&running.code).unwrap_or(running.code.as_slice())
            } else {
                running.code.as_slice()
            };

            // `ZIPP_INTERPSTATS=1`: this frame is about to be INTERPRETED from
            // `ip` (the native tiers above either bailed here or did not take
            // it). Free when unset: one latched load per frame transition.
            #[cfg(not(target_arch = "wasm32"))]
            if !LITE && interp_stats::enabled() {
                interp_stats::bump(func_id, &self.func(func_id as usize).name, ip != 0);
            }

            // Inner loop: execute within the current frame until a call pushes
            // a new frame or a return pops this one.
            loop {
                // The hardened interpreter never extends a borrow of VM-owned
                // data across an instruction that mutates the VM. The cached
                // slice above has the immutable PROGRAM lifetime; cloning the
                // whole function on every call/return made otherwise-cheap
                // calls an unmetered O(code) operation that hostile source
                // could amplify.
                // `Vm::func` returns `&'p FuncProto` — the PROGRAM lifetime, not
                // a reborrow of `&self` (see its doc comment: callers may hold it
                // across `&mut self` operations). Borrowing the instruction out of
                // it therefore extends no borrow of the VM across the arms below,
                // and the clone this replaces bought nothing.
                //
                // It cost a great deal. `Instr` had 228 fieldful variants and
                // derived `Clone` without `Copy` then, so `.clone()` lowered to a
                // second 229-target `br_table` plus a 32-byte copy — the hardened
                // interpreter dispatched TWICE per bytecode instruction, once to
                // copy the instruction and once to execute it, with two
                // unpredictable indirect branches where an interpreter should have
                // one. Two of them were visible 19 KB apart in `dispatch_body`.
                // (`Instr` is `Copy` now; borrowing it is still the right thing.)
                #[cfg(feature = "safe-sandbox")]
                let instr: &crate::bytecode::Instr = &icode[ip];
                #[cfg(not(feature = "safe-sandbox"))]
                let instr = &icode[ip];
                // Step budget / abort poll / execution trace. Compiled out
                // entirely without the `instrument` feature, and a null check
                // when it is present but unused — this is the hot loop.
                // `ip` (the local) is the pc, never `frames[..].ip`, which is
                // only synced at frame transitions and is stale in here.
                // (The general profile's hook is one decrement of `step_hot`
                // and one test, with or without a recorder: see `step_hot`.)
                // The lite-hooked copy runs only for a VM that had no
                // recorder when it began (`lite_entry_ok`), and one is only
                // ever installed between runs (`set_instrumentation`), so
                // `step_hot` holds `i64::MAX` there and the hook never fires:
                // it was a read-modify-write of the VM per instruction.
                #[cfg(all(feature = "instrument", not(feature = "meter-only")))]
                if !LITE {
                    self.step_hot -= 1;
                    if self.step_hot < 0 {
                        if let Err(msg) = self.instrument_step_hot_out(base, ip, instr) {
                            let top = self.frames.len() - 1;
                            self.frames[top].ip = ip;
                            return Err(Thrown(msg.to_string()));
                        }
                    }
                }
                #[cfg(feature = "meter-only")]
                if self.instr_rec.is_some() {
                    if let Err(msg) = self.instrument_step(base, ip, instr) {
                        let top = self.frames.len() - 1;
                        self.frames[top].ip = ip;
                        return Err(Thrown(msg.to_string()));
                    }
                }
                match *instr {
                    Instr::LoadConst { dst, idx } => {
                        // String constants are sentinels. Short, immutable-safe
                        // slots use the bounded rooted interpreter cache; every
                        // ineligible/capped case is the historical fresh resolve.
                        let resolved = self.resolve_const_slot(func_id, idx as u32);
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
                    Instr::LoadNewTarget { dst } => {
                        // An ARROW frame has no own [[NewTarget]] — frame_new_target
                        // reads the lexically-captured one of its closure (recorded
                        // at MakeArrow inside a constructed activation).
                        let nt = self.frame_new_target();
                        self.set(base, dst, nt);
                        ip += 1;
                    }
                    Instr::LoadCallee { dst } => {
                        // The function value of the running frame. Prefer the actual
                        // callee the caller invoked (so a named function expression's
                        // own name === the outer reference); fall back to the Closure
                        // object, or a fresh Func for a plain function whose caller is
                        // unknown (generator/async resume, top-level).
                        let (callee, clo, fid) = {
                            let fr = self.frames.last().unwrap();
                            (fr.callee, fr.closure, fr.func)
                        };
                        let v = if !callee.is_undefined() {
                            callee
                        } else if clo != NO_CLOSURE {
                            Value::heap(clo)
                        } else {
                            Value::heap(self.heap.alloc(HeapObj::Func(fid)))
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LoadHole { dst } => {
                        // The HOLE sentinel for an elided array-literal element; the
                        // following NewArray/ArrayAppend copies it into the array.
                        self.set(base, dst, Value::HOLE);
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
                        // Fast path: a live slot the global object does not
                        // shadow. Everything else — never declared, TDZ, a
                        // deleted builtin, a real own-property route — is
                        // `load_global_slow` (the former inline arm, verbatim).
                        let v = self.globals[idx as usize];
                        if v.is_uninitialized()
                            || (self.global_route_epoch != 0 && self.global_real_own_route(idx))
                        {
                            let val = self.load_global_slow(idx, func_id)?;
                            self.set(base, dst, val);
                            ip += 1;
                            continue;
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LoadGlobalOrUndefined { dst, idx } => {
                        let v = self.globals[idx as usize];
                        let v = if v.is_uninitialized() {
                            // The binding may be own-prop-backed on the global
                            // object (eval-created vars / `this.x = v`), or
                            // resolvable through the global object's PROTO CHAIN
                            // (a Proxy prototype's `has` trap / an inherited
                            // property — HasBinding is HasProperty on the
                            // binding object, chain included).
                            match self.global_slot_name(idx) {
                                Some(name) if self.global_this != 0 => {
                                    let has_own = matches!(
                                        self.heap.get(self.global_this),
                                        HeapObj::Object(m) if m.pos(&name).is_some()
                                    );
                                    let gobj = Value::heap(self.global_this);
                                    let proto = self.object_get_prototype_of(gobj);
                                    if has_own
                                        || (proto.is_heap()
                                            && self.has_property_str_dyn(proto, &name)?)
                                    {
                                        self.get_prop(gobj, &name)?
                                    } else {
                                        Value::UNDEFINED
                                    }
                                }
                                _ => Value::UNDEFINED,
                            }
                        } else {
                            v
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::StoreGlobal { idx, src } => {
                        // A $262.evalScript `const`: writes after the
                        // initializing one (which sees UNINITIALIZED) throw.
                        if !self.eval_const_globals.is_empty()
                            && self.eval_const_globals.contains(&idx)
                            && !self.globals[idx as usize].is_uninitialized()
                        {
                            return Err(Thrown(
                                "TypeError: Assignment to constant variable.".into(),
                            ));
                        }
                        let v = self.get(base, src);
                        if self.globals[idx as usize].is_uninitialized() {
                            // An own-prop-backed binding (eval-created /
                            // `this.x = v`) is the live binding: write through
                            // it and leave the slot uninitialized. Not for a
                            // script lexical's slot, whose TDZ store is its
                            // initialization (see StoreGlobalDyn).
                            if let Some(name) = self
                                .global_slot_name(idx)
                                .filter(|n| !self.global_name_is_lexical(n))
                            {
                                let has_own = self.global_this != 0
                                    && matches!(
                                        self.heap.get(self.global_this),
                                        HeapObj::Object(m) if m.pos(&name).is_some()
                                    );
                                if has_own {
                                    let gobj = Value::heap(self.global_this);
                                    self.set_prop(gobj, &name, v, false)?;
                                    ip += 1;
                                    continue;
                                }
                                // A name the global object's PROTO CHAIN resolves
                                // (an inherited accessor / non-writable data
                                // property, or a Proxy prototype's `has` trap) is
                                // assigned THROUGH [[Set]] on the global object,
                                // not as a shadowing slot binding
                                // (staging/sm/object/proto-property-change-
                                // writability-set, staging/sm/Proxy/global-receiver).
                                let gobj = Value::heap(self.global_this);
                                let proto = if self.global_this != 0 {
                                    self.object_get_prototype_of(gobj)
                                } else {
                                    Value::NULL
                                };
                                if proto.is_heap() && self.has_property_str_dyn(proto, &name)? {
                                    self.set_prop(gobj, &name, v, false)?;
                                    ip += 1;
                                    continue;
                                }
                                // Unresolvable: PutValue is Set(globalObject,
                                // name), and a Proxy on the global's chain gets
                                // that write through its `set` trap even when
                                // its `has` trap denied the name.
                                if self.global_proto_chain_has_proxy() {
                                    self.set_prop(gobj, &name, v, false)?;
                                    ip += 1;
                                    continue;
                                }
                                // Re-CREATING a deleted binding (`delete Function;
                                // Function = 1`) makes it a global-object property
                                // again, so lift the deletion record DeleteGlobal
                                // left behind. Empty until something is deleted.
                                if !self.deleted_globals.is_empty() {
                                    self.deleted_globals.remove(&name);
                                }
                            }
                        } else if self.global_route_epoch != 0 && self.global_real_own_route(idx) {
                            // A defineProperty'd REAL own property of the global
                            // object (its attributes live on the object, not in
                            // the slot) governs the write — route through [[Set]]
                            // (staging/sm/Proxy/regress-bug1037770). A global
                            // LEXICAL binding is not an object property: the slot
                            // stays authoritative for it. As in the strict arm and
                            // `LoadGlobal`, the route can only have appeared after
                            // `note_global_own_property_change` bumped the epoch;
                            // ungated, the heap and name probe ran on every `var`
                            // initializer (98M times in typedarray-math).
                            let name = self.global_slot_name(idx).unwrap_or_default();
                            if !self.global_name_is_lexical(&name) {
                                let gobj = Value::heap(self.global_this);
                                self.set_prop(gobj, &name, v, false)?;
                                ip += 1;
                                continue;
                            }
                        }
                        self.globals[idx as usize] = v;
                        ip += 1;
                    }
                    Instr::StoreGlobalStrict { idx, src } => {
                        // Strict assignment to an unresolvable reference (a global slot
                        // never declared/initialized) is a ReferenceError, not a global
                        // creation. (No own-prop fallback here: the reference's
                        // unresolvable-ness was fixed when the LHS was evaluated —
                        // a property the RHS created meanwhile must not resolve it,
                        // `undeclared = (this.undeclared = 5)` still throws. The
                        // read-modify-write forms, whose Get already PROVED the
                        // reference resolvable, use StoreGlobalResolved instead.)
                        if self.globals[idx as usize].is_uninitialized() {
                            let Some(name) = self.global_slot_name(idx) else {
                                return Err(self.unnamed_global_slot_error(idx));
                            };
                            // The reference IS resolvable when the name is a
                            // builtin global: undefined/NaN/Infinity are
                            // non-writable data props (strict write →
                            // TypeError); any other builtin is writable —
                            // the write lands on the global object (slot).
                            if matches!(name.as_str(), "undefined" | "NaN" | "Infinity") {
                                return Err(Thrown(format!(
                                    "TypeError: Cannot assign to read only property '{name}'"
                                )));
                            }
                            if self.global_by_name(&name).is_none() {
                                // A slot a script GlobalDeclarationInstantiation
                                // DECLARED (`$262.evalScript("var x;")`, or a
                                // top-level function) is resolvable even though
                                // its slot stays UNINITIALIZED: CreateGlobalVar-
                                // Binding parks those on the global object and
                                // routes access through the own prop. Without
                                // this the declaration is invisible to a STRICT
                                // write, so `"use strict"; var x; x = 1;` in one
                                // evalScript threw "x is not defined" while the
                                // sloppy form (StoreGlobal, which has the own-prop
                                // fallback) worked and the read of the same name
                                // resolved fine. Checked BEFORE the proto probe
                                // and keyed on the DECLARATION registry, not on
                                // own-property presence, so
                                // `undeclared = (this.undeclared = 5)` — which
                                // creates a property but declares nothing — still
                                // throws.
                                if self.eval_var_globals.contains(&idx) {
                                    let v = self.get(base, src);
                                    let gobj = Value::heap(self.global_this);
                                    self.set_prop(gobj, &name, v, true)?;
                                    ip += 1;
                                    continue;
                                }
                                // The reference can still resolve through the
                                // global object's PROTO CHAIN — an inherited
                                // property, or a Proxy prototype's `has` trap
                                // (global-receiver, strict variant). Probing the
                                // chain ONLY (never the own props) keeps
                                // `undeclared = (this.undeclared = 5)` throwing.
                                let gobj = Value::heap(self.global_this);
                                let proto = if self.global_this != 0 {
                                    self.object_get_prototype_of(gobj)
                                } else {
                                    Value::NULL
                                };
                                if proto.is_heap() && self.has_property_str_dyn(proto, &name)? {
                                    let v = self.get(base, src);
                                    self.set_prop(gobj, &name, v, true)?;
                                    ip += 1;
                                    continue;
                                }
                                return Err(Thrown(format!(
                                    "ReferenceError: {name} is not defined"
                                )));
                            }
                        } else if self.global_route_epoch != 0 && self.global_real_own_route(idx) {
                            // A defineProperty'd REAL own property of the global
                            // object governs the write — a strict assignment to
                            // a non-writable one is a TypeError, never a slot
                            // write (regress-bug1037770, strict analogue). As for
                            // LoadGlobal, the route can only have changed after
                            // `note_global_own_property_change` bumped the epoch;
                            // keep the heap/name probe off ordinary global loops.
                            let name = self.global_slot_name(idx).unwrap_or_default();
                            if !self.global_name_is_lexical(&name) {
                                let v = self.get(base, src);
                                let gobj = Value::heap(self.global_this);
                                self.set_prop(gobj, &name, v, true)?;
                                ip += 1;
                                continue;
                            }
                        }
                        // A $262.evalScript `const` is immutable once initialized.
                        if !self.eval_const_globals.is_empty()
                            && self.eval_const_globals.contains(&idx)
                        {
                            return Err(Thrown(
                                "TypeError: Assignment to constant variable.".into(),
                            ));
                        }
                        let v = self.get(base, src);
                        self.globals[idx as usize] = v;
                        ip += 1;
                    }
                    Instr::StoreGlobalResolved { idx, src } => {
                        // A CheckGlobalResolvable emitted before the RHS found
                        // this reference unresolvable: PutValue's ReferenceError
                        // lands NOW — after the RHS ran (an RHS throw would have
                        // won by never reaching this store).
                        if !self.strict_unresolvable_globals.is_empty() {
                            if let Some(p) = self
                                .strict_unresolvable_globals
                                .iter()
                                .rposition(|&i| i == idx)
                            {
                                self.strict_unresolvable_globals.remove(p);
                                let Some(name) = self.global_slot_name(idx) else {
                                    return Err(self.unnamed_global_slot_error(idx));
                                };
                                return Err(Thrown(format!(
                                    "ReferenceError: {name} is not defined"
                                )));
                            }
                        }
                        // PutValue for a reference this same expression already
                        // ran GetValue on (`x++`, `x += 1`, `x ||= v`): 6.2.5.6
                        // step 1 cannot be reached — a Get that succeeded proves
                        // the reference is resolvable — so this must never raise
                        // the "is not defined" ReferenceError StoreGlobalStrict
                        // raises. Emitted only where the compiler put a
                        // load_binding of the SAME binding immediately before.
                        if self.globals[idx as usize].is_uninitialized() {
                            let Some(name) = self.global_slot_name(idx) else {
                                return Err(self.unnamed_global_slot_error(idx));
                            };
                            // undefined/NaN/Infinity are non-writable data
                            // properties: a strict write is a TypeError.
                            if matches!(name.as_str(), "undefined" | "NaN" | "Infinity") {
                                return Err(Thrown(format!(
                                    "TypeError: Cannot assign to read only property '{name}'"
                                )));
                            }
                            // An own property of the global OBJECT is a global
                            // BINDING (9.1.1.4's object Environment Record), and
                            // the write belongs on that property — not in a slot
                            // that would shadow it. Without this,
                            //     "use strict"; this.count = 0;
                            //     ({ m(){ count++; } }).m();
                            // threw "count is not defined" even though the `count`
                            // READ one instruction earlier had just succeeded
                            // through exactly this property
                            // (language/types/object/S8.6.2_A5_T1, _T2).
                            // Global LEXICALs are excluded: an uninitialized
                            // `let x` slot is a TDZ, which a same-named
                            // global-object property does not resolve.
                            let own_backed = !self.global_name_is_lexical(&name)
                                && self.global_this != 0
                                && matches!(
                                    self.heap.get(self.global_this),
                                    HeapObj::Object(m) if m.pos(&name).is_some()
                                );
                            if own_backed {
                                let v = self.get(base, src);
                                let gobj = Value::heap(self.global_this);
                                // strict = true: a non-writable own property is a
                                // TypeError, as PutValue requires.
                                self.set_prop(gobj, &name, v, true)?;
                                ip += 1;
                                continue;
                            }
                            // 9.1.1.2.5 SetMutableBinding step 2 re-checks
                            // HasProperty: a binding that VANISHED between the Get
                            // and the Put — `Object.defineProperty(this, "x", {get(){
                            // delete this.x; return 2 }}); x ^= 3` — is a
                            // ReferenceError in strict code even though the Get
                            // succeeded (compound-assignment-operator-calls-putvalue-
                            // lref--v--*.js). Dropping this let the store silently
                            // create a slot value instead.
                            if self.global_by_name(&name).is_none() {
                                // The same proto-chain resolution
                                // StoreGlobalStrict performs: an inherited
                                // property still resolves the reference, and
                                // the write lands through [[Set]] on the
                                // global object.
                                let gobj = Value::heap(self.global_this);
                                let proto = if self.global_this != 0 {
                                    self.object_get_prototype_of(gobj)
                                } else {
                                    Value::NULL
                                };
                                if proto.is_heap() && self.has_property_str_dyn(proto, &name)? {
                                    let v = self.get(base, src);
                                    self.set_prop(gobj, &name, v, true)?;
                                    ip += 1;
                                    continue;
                                }
                                return Err(Thrown(format!(
                                    "ReferenceError: {name} is not defined"
                                )));
                            }
                        } else if self.global_route_epoch != 0 && self.global_real_own_route(idx) {
                            // A REAL own property of the global object shadows
                            // the live slot (a defineProperty'd `var`): it is the
                            // binding, and a strict write to a non-writable one is
                            // a TypeError, never a slot write. StoreGlobal and
                            // StoreGlobalStrict route the same way; this arm
                            // (strict `x = v` after CheckGlobalResolvable, `x++`)
                            // did not, so `"use strict"; x = 1` wrote through a
                            // read-only `x`. The JIT gates refuse a shadowed slot
                            // for this op (`global_store_slot_directly_routable`),
                            // so compiled code never reaches one.
                            let name = self.global_slot_name(idx).unwrap_or_default();
                            if !self.global_name_is_lexical(&name) {
                                let v = self.get(base, src);
                                let gobj = Value::heap(self.global_this);
                                self.set_prop(gobj, &name, v, true)?;
                                ip += 1;
                                continue;
                            }
                        }
                        if !self.eval_const_globals.is_empty()
                            && self.eval_const_globals.contains(&idx)
                        {
                            return Err(Thrown(
                                "TypeError: Assignment to constant variable.".into(),
                            ));
                        }
                        let v = self.get(base, src);
                        self.globals[idx as usize] = v;
                        ip += 1;
                    }
                    Instr::Add { dst, a, b } => {
                        let r = self.add(base, a, b)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // Identical to `Add` â€” a JIT routing hint only (see bytecode).
                    Instr::StrConcat { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = self.add_values(va, vb)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // Exact right-associated pair `a + (b + c)`.  The shared
                    // helper owns both the bounded primitive fast arm and the
                    // literal two-Add fallback, so interpreter/JIT semantics
                    // cannot drift.
                    Instr::AddRightPair {
                        dst,
                        a,
                        b,
                        c,
                        in_place,
                    } => {
                        let av = self.get(base, a);
                        let bv = self.get(base, b);
                        let cv = self.get(base, c);
                        let r = if in_place {
                            self.add_values_right_pair_inplace(av, bv, cv)?
                        } else {
                            self.add_values_right_pair(av, bv, cv)?
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Pad2Concat { dst, src, zero } => {
                        let v = self.get(base, src);
                        let r = self.pad2_concat(v, zero)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Pad2Conditional { dst, src } => {
                        let v = self.get(base, src);
                        let r = self.pad2_conditional(v)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // W11 (B124) fused concat-chain link: `+` semantics exactly
                    // (add_values_chain delegates every non-in-place case to
                    // add_values); `a` is the chain's dead fresh accumulator
                    // temp, so a flat-Str `a` may grow in place.
                    Instr::StrConcatChain { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = self.add_values_chain(va, vb)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // In-place string append (emitter proved `a` uniquely owned).
                    Instr::StrAppendInPlace { dst, a, b } => {
                        let av = self.get(base, a);
                        let bv = self.get(base, b);
                        // `None`: the appended value needs real ToPrimitive
                        // (user hooks / a Symbol's TypeError) — run the full
                        // generic `+`, exactly as the unfused op would.
                        let r = match self.str_append_inplace(av, bv) {
                            Some(r) => r,
                            None => self.add_values(av, bv)?,
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // Exact adjacent `GetIndex` + proven-linear append fusion.
                    // The ASCII hit performs no observable operation between
                    // the indexed read and append. A miss writes the compiler-
                    // dead scratch before running the historical append so it
                    // remains a GC root exactly like the original GetIndex dst.
                    Instr::StrAppendIndex {
                        dst,
                        a,
                        obj,
                        key,
                        scratch,
                    } => {
                        let av = self.get(base, a);
                        let ov = self.get(base, obj);
                        let kv = self.get(base, key);
                        let r = match self.str_append_index_ascii_fast(av, ov, kv) {
                            Some(r) => r,
                            None => {
                                let bv = self.get_index(ov, kv)?;
                                self.set(base, scratch, bv);
                                let av = self.get(base, a);
                                let bv = self.get(base, scratch);
                                match self.str_append_inplace(av, bv) {
                                    Some(r) => r,
                                    None => self.add_values(av, bv)?,
                                }
                            }
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // ToString(a) with the STRING hint (toString before valueOf),
                    // for template-literal substitutions.
                    Instr::ToStr { dst, a } => {
                        let av = self.get(base, a);
                        // Identity for strings (exact — lone surrogates survive
                        // template interpolation), coercion otherwise.
                        let r = self.to_str_value(av)?;
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
                        } else if va.is_number() && vb.is_number() {
                            Value::num(va.as_f64() - vb.as_f64())
                        } else {
                            self.numeric_binop(BigOp::Sub, va, vb)?
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Mul { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if va.is_int() && vb.is_int() {
                            // A ZERO product with a negative operand is -0 in JS
                            // (`0 * -5`), which no Int encodes: like `Mod` below,
                            // that case and overflow take the f64 product. One
                            // operand is 0 whenever `v == 0`, so the sign of the
                            // other is the sign of `ia | ib`.
                            let (ia, ib) = (va.as_int(), vb.as_int());
                            match ia.checked_mul(ib) {
                                Some(v) if v != 0 || (ia | ib) >= 0 => Value::int(v),
                                _ => Value::num(ia as f64 * ib as f64),
                            }
                        } else if va.is_number() && vb.is_number() {
                            Value::num(va.as_f64() * vb.as_f64())
                        } else {
                            self.numeric_binop(BigOp::Mul, va, vb)?
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Div { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if va.is_number() && vb.is_number() {
                            Value::num(va.as_f64() / vb.as_f64())
                        } else {
                            self.numeric_binop(BigOp::Div, va, vb)?
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Mod { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if va.is_int() && vb.is_int() {
                            // `Mul` above already narrows the int-int case; `Mod`
                            // did not, and on wasm that is expensive out of all
                            // proportion: there is no float-remainder instruction,
                            // so `f64 % f64` lowers to a call to a software `fmod`
                            // in compiler_builtins. Profiling `i % 7` in a loop put
                            // 18.4% of samples in `fmod`, and `%` measured 39%
                            // slower than `+` on the same loop shape.
                            //
                            // i32 `%` truncates toward zero, which is exactly JS
                            // `%`. `checked_rem` covers the two results i32 cannot
                            // represent — a zero divisor (JS: NaN) and
                            // `i32::MIN % -1` (JS: -0) — and a zero remainder from
                            // a NEGATIVE dividend is -0 in JS, a double. All three
                            // fall through to the f64 path, which produces them.
                            let ia = va.as_int();
                            match ia.checked_rem(vb.as_int()) {
                                Some(rem) if rem != 0 || ia >= 0 => Value::int(rem),
                                _ => Value::num(va.as_f64() % vb.as_f64()),
                            }
                        } else if va.is_number() && vb.is_number() {
                            Value::num(va.as_f64() % vb.as_f64())
                        } else {
                            self.numeric_binop(BigOp::Mod, va, vb)?
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::ToNum { dst, a } => {
                        let va = self.get(base, a);
                        // `+x`: numbers pass through (keep Int tag); `+bigint` throws
                        // (unary plus is not defined on BigInt); else ToNumber STRICT â€”
                        // a BigInt reached via ToPrimitive (a boxed `Object(1n)`, or an
                        // object whose valueOf returns a BigInt) is also a TypeError.
                        let r = if va.is_number() {
                            va
                        } else if self.is_bigint_prim(va) {
                            return Err(Thrown(
                                "TypeError: Cannot convert a BigInt value to a number".into(),
                            ));
                        } else {
                            Value::num(self.to_number_strict(va)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Neg { dst, a } => {
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            let i = va.as_int();
                            if i == 0 {
                                // `-0` is the negative-zero DOUBLE, not integer 0
                                // (so `1/-0` is -Infinity, `Object.is(-0,+0)` false).
                                Value::num(-0.0)
                            } else {
                                match i.checked_neg() {
                                    Some(v) => Value::int(v),
                                    None => Value::num(-(i as f64)),
                                }
                            }
                        } else if va.is_double() {
                            Value::num(-va.as_f64())
                        } else {
                            // Unary minus is ToNumeric: an object (incl. `Object(1n)`
                            // or a valueOf returning a BigInt) negates AS a BigInt.
                            let p = self.numeric_prim(va)?;
                            if let Some(b) = self.bigint_val(p) {
                                self.make_bigint_val(b.neg())
                            } else {
                                Value::num(-self.to_number(p)?)
                            }
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Bitwise { dst, a, b, op } => {
                        use crate::bytecode::BitwiseOp as B;
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let bop = match op {
                            B::And => BigOp::And,
                            B::Or => BigOp::Or,
                            B::Xor => BigOp::Xor,
                            B::Shl => BigOp::Shl,
                            B::Shr => BigOp::Shr,
                            B::Ushr => BigOp::Ushr,
                        };
                        // Number fast path (the hot `x & 31` and `(r * n) | 0`
                        // shapes: two ints, or a finite double whose ToInt32 is
                        // a truncate and a wrap); everything else takes the
                        // full ToNumeric path: BigInt &/|/^/<</>> on two BigInts
                        // (incl. wrappers/objects whose ToPrimitive yields one);
                        // `>>>` on BigInts is a TypeError AFTER coercion; mixing
                        // is a TypeError.
                        let ints = if va.is_int() && vb.is_int() {
                            Some((va.as_int(), vb.as_int()))
                        } else {
                            number_int32(va).zip(number_int32(vb))
                        };
                        let r = if let Some((x, y)) = ints {
                            match op {
                                B::And => Value::int(x & y),
                                B::Or => Value::int(x | y),
                                B::Xor => Value::int(x ^ y),
                                // Shift counts use the low 5 bits per the JS spec.
                                B::Shl => Value::int(x.wrapping_shl((y as u32) & 31)),
                                B::Shr => Value::int(x >> ((y as u32) & 31)),
                                B::Ushr => {
                                    let u = (x as u32) >> ((y as u32) & 31);
                                    // u32 may exceed i32::MAX â†’ keep numeric range.
                                    if u <= i32::MAX as u32 {
                                        Value::int(u as i32)
                                    } else {
                                        Value::num(u as f64)
                                    }
                                }
                            }
                        } else {
                            self.numeric_binop(bop, va, vb)?
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Pow { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = self.numeric_binop(BigOp::Pow, va, vb)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::BitNot { dst, a } => {
                        let va = self.get(base, a);
                        if va.is_int() {
                            let r = !va.as_int();
                            self.set(base, dst, Value::int(r));
                        } else {
                            // `~x` is ToNumeric: a BigInt operand (incl. via an
                            // object's ToPrimitive) stays a BigInt (~x = -x-1).
                            let p = self.numeric_prim(va)?;
                            let r = if let Some(b) = self.bigint_val(p) {
                                match b {
                                    BigVal::Small(n) => self.make_bigint(!n),
                                    BigVal::Big(big) => self.make_bigint_num(!big),
                                }
                            } else {
                                Value::int(!to_int32(self.to_number(p)?))
                            };
                            self.set(base, dst, r);
                        }
                        ip += 1;
                    }
                    // The Python frontend's fused fast paths: one out-of-line
                    // step (see `py_ops`), so this loop's own code is unchanged.
                    //
                    // The arithmetic and comparison instructions on two small
                    // ints (the operands of nearly every one a program runs)
                    // are settled here, with what the step answers for them
                    // (`py_ops`' inline forms); anything else takes the step,
                    // and so does every one under `ZIPP_PY_PROF`, which
                    // counts in the step.
                    #[cfg(feature = "python")]
                    Instr::PyArith { op, dst, a, b, .. } => {
                        match super::py_ops::py_small_int_arith(op, self.get(base, a), self.get(base, b)) {
                            Some(v) if !super::prof_py::on() => {
                                self.set(base, dst, v);
                                ip += 1;
                            }
                            _ => ip = self.py_step(func_id, base, ip, instr)?,
                        }
                    }
                    #[cfg(feature = "python")]
                    Instr::PyAddImm { dst, a, imm, .. } => {
                        match super::py_ops::py_small_int_add_imm(self.get(base, a), imm) {
                            Some(v) if !super::prof_py::on() => {
                                self.set(base, dst, v);
                                ip += 1;
                            }
                            _ => ip = self.py_step(func_id, base, ip, instr)?,
                        }
                    }
                    #[cfg(feature = "python")]
                    Instr::PyCompare { op, dst, a, b, .. } => {
                        match super::py_ops::py_small_int_compare(op, self.get(base, a), self.get(base, b)) {
                            Some(r) if !super::prof_py::on() => {
                                self.set(base, dst, Value::bool(r));
                                ip += 1;
                            }
                            _ => ip = self.py_step(func_id, base, ip, instr)?,
                        }
                    }
                    #[cfg(feature = "python")]
                    Instr::PyJumpCompare { op, a, b, when, target, .. } => {
                        match super::py_ops::py_small_int_compare(op, self.get(base, a), self.get(base, b)) {
                            Some(r) if !super::prof_py::on() => {
                                if r == when {
                                    // A loop back-edge polls the GC, as the
                                    // step does (and as `Jump` does).
                                    if (target as usize) < ip {
                                        self.maybe_gc();
                                    }
                                    ip = target as usize;
                                } else {
                                    ip += 1;
                                }
                            }
                            _ => ip = self.py_step(func_id, base, ip, instr)?,
                        }
                    }
                    #[cfg(feature = "python")]
                    Instr::PyClassOf { .. }
                    | Instr::PyDictGet { .. }
                    | Instr::PyDictSet { .. }
                    | Instr::PyCallEntry { .. }
                    | Instr::PyGetItem { .. }
                    | Instr::PySetItem { .. } => {
                        ip = self.py_step(func_id, base, ip, instr)?;
                    }
                    // The four instructions below ask their site's cache
                    // entry first (`py_ops` / `py_attr`'s hit forms: what
                    // the step's function answers at once for an entry that
                    // holds, in one small call) and take the step when it
                    // declines, as every one does under `ZIPP_PY_PROF`,
                    // which counts in the step.
                    //
                    // An attribute read. In the step, a property with a
                    // Python getter gets the accessor's frame pushed, as a
                    // call's is.
                    #[cfg(feature = "python")]
                    Instr::PyGetAttr { dst, obj, .. } => {
                        match self.py_attr_get_hit(func_id, ip, self.get(base, obj)) {
                            Some(v) if !super::prof_py::on() => {
                                self.set(base, dst, v);
                                ip += 1;
                            }
                            _ => {
                                let next = self.py_step_ext(func_id, base, ip, instr)?;
                                if next == usize::MAX {
                                    break;
                                }
                                ip = next;
                            }
                        }
                    }
                    // An attribute store (a property's setter likewise).
                    #[cfg(feature = "python")]
                    Instr::PySetAttr { obj, val, .. } => {
                        if !super::prof_py::on() && self.py_attr_set_hit(func_id, ip, self.get(base, obj), self.get(base, val)) {
                            ip += 1;
                        } else {
                            let next = self.py_step_ext(func_id, base, ip, instr)?;
                            if next == usize::MAX {
                                break;
                            }
                            ip = next;
                        }
                    }
                    #[cfg(feature = "python")]
                    Instr::PyGlobal { dst, globals, rt, .. } => {
                        match self.py_global_hit(func_id, ip, self.get(base, globals), self.get(base, rt)) {
                            Some(v) if !super::prof_py::on() => {
                                self.set(base, dst, v);
                                ip += 1;
                            }
                            _ => ip = self.py_step_ext(func_id, base, ip, instr)?,
                        }
                    }
                    // A method lookup: a site whose receivers are of several
                    // classes misses its one entry often, so a miss asks the
                    // step's function itself (its answer is the whole step).
                    #[cfg(feature = "python")]
                    Instr::PyMethod { dst, obj, rt, key, gb, slow } => {
                        if super::prof_py::on() {
                            ip = self.py_step_ext(func_id, base, ip, instr)?;
                        } else {
                            let (o, r) = (self.get(base, obj), self.get(base, rt));
                            let f = match self.py_method_hit(func_id, ip, o, r) {
                                Some(f) => Some(f),
                                None => self.py_method(func_id, ip, o, r, key, gb),
                            };
                            match f {
                                Some(f) => {
                                    self.set(base, dst, f);
                                    ip += 1;
                                }
                                None => ip = slow as usize,
                            }
                        }
                    }
                    #[cfg(feature = "python")]
                    Instr::PyStrItem { .. }
                    | Instr::PyStrLen { .. }
                    | Instr::PyIsInstance { .. }
                    | Instr::PyGenNext { .. }
                    | Instr::PyModGet { .. }
                    | Instr::PyLen { .. }
                    | Instr::PyAttrFn { .. }
                    | Instr::PySeq { .. }
                    | Instr::PyRaise { .. }
                    | Instr::PyCaught { .. }
                    | Instr::PyClassAttr { .. }
                    | Instr::PyDictLookup { .. }
                    | Instr::PyUnpack { .. } => {
                        ip = self.py_step_ext(func_id, base, ip, instr)?;
                    }
                    #[cfg(feature = "python")]
                    Instr::PyMakeExc { .. } | Instr::PyExcPop { .. } | Instr::PyNew { .. } => {
                        ip = self.py_step_ext2(func_id, base, ip, instr)?;
                    }
                    // A call: a frame pushed (the callee's) re-enters the loop.
                    #[cfg(feature = "python")]
                    Instr::PyCall { .. } => {
                        let next = self.py_step_call(func_id, base, ip, instr)?;
                        if next == usize::MAX {
                            break;
                        }
                        ip = next;
                    }
                    // Only the Python frontend emits these; a build without it
                    // carries none of their code.
                    #[cfg(all(not(feature = "python"), not(feature = "wasm-lite")))]
                    Instr::PyArith { .. }
                    | Instr::PyAddImm { .. }
                    | Instr::PyCompare { .. }
                    | Instr::PyJumpCompare { .. }
                    | Instr::PyClassOf { .. }
                    | Instr::PyDictGet { .. }
                    | Instr::PyDictSet { .. }
                    | Instr::PyCallEntry { .. }
                    | Instr::PyGetItem { .. }
                    | Instr::PySetItem { .. }
                    | Instr::PyGlobal { .. }
                    | Instr::PyStrItem { .. }
                    | Instr::PyStrLen { .. }
                    | Instr::PyGetAttr { .. }
                    | Instr::PySetAttr { .. }
                    | Instr::PyIsInstance { .. }
                    | Instr::PyGenNext { .. }
                    | Instr::PyMethod { .. }
                    | Instr::PyModGet { .. }
                    | Instr::PyLen { .. }
                    | Instr::PyAttrFn { .. }
                    | Instr::PySeq { .. }
                    | Instr::PyRaise { .. }
                    | Instr::PyCaught { .. }
                    | Instr::PyClassAttr { .. }
                    | Instr::PyDictLookup { .. }
                    | Instr::PyUnpack { .. }
                    | Instr::PyMakeExc { .. }
                    | Instr::PyExcPop { .. }
                    | Instr::PyNew { .. }
                    | Instr::PyCall { .. } => {
                        return Err(Thrown("InternalError: Python instruction in a build without Python".to_string()));
                    }
                    Instr::AddInt { dst, a, imm, upd } => {
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            match va.as_int().checked_add(imm) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 + imm as f64),
                            }
                        } else if va.is_double() {
                            Value::num(va.as_f64() + imm as f64)
                        } else {
                            // A small (immediate) BigInt under `upd` whose sum
                            // is one too: the counter of a Python `for` over
                            // a range, every iteration. What `bigint_op`
                            // answers below, without ToNumeric and the call.
                            #[cfg(feature = "python")]
                            if upd {
                                if let Some(v) = va.small_bigint_val().and_then(|n| Value::small_bigint(n as i128 + imm as i128)) {
                                    self.set(base, dst, v);
                                    ip += 1;
                                    continue;
                                }
                            }
                            // ToNumeric the operand once, then branch on its type.
                            let p = self.numeric_prim(va)?;
                            if let Some(b) = self.bigint_val(p) {
                                if upd {
                                    // `++`/`--` (UpdateExpression) on a BigInt stays
                                    // a BigInt — ToNumeric keeps the type (`n++` is
                                    // `n + 1n`, not a Number coercion).
                                    self.bigint_op(
                                        BigOp::Add,
                                        b,
                                        crate::vm::bigint::BigVal::Small(imm as i128),
                                    )?
                                } else {
                                    // Binary `x ± <int literal>` fast path: mixing a
                                    // BigInt with a Number is the spec's TypeError.
                                    return Err(Thrown(crate::vm::bigint::BIGINT_MIX_ERR.into()));
                                }
                            } else {
                                Value::num(self.to_number(p)? + imm as f64)
                            }
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }

                    // ── superinstructions (`FusedCode`) ── each is its two
                    // instructions' arms run back to back: the same number
                    // paths inline, the same helpers otherwise.
                    Instr::FuseAddIntMove { dst, a, imm, upd, mdst, msrc } => {
                        let va = self.get(base, a);
                        let r = self.add_int_value(va, imm, upd)?;
                        self.set(base, dst, r);
                        let v = self.get(base, msrc);
                        self.set(base, mdst, v);
                        ip += 2;
                    }
                    Instr::FuseAddIntJump { dst, a, imm, upd, target } => {
                        let va = self.get(base, a);
                        let r = self.add_int_value(va, imm, upd)?;
                        self.set(base, dst, r);
                        // The `Jump` at `ip + 1`, which goes backward (see the
                        // variant): its GC safe point. With the JIT on, a
                        // back-edge also feeds the OSR tier, so the `Jump` arm
                        // takes it.
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        if !LITE && self.jit_enabled {
                            ip += 1;
                            continue;
                        }
                        self.maybe_gc();
                        ip = target as usize;
                    }
                    Instr::FuseIntCmpJump { ldst, val, a, le, target } => {
                        self.set(base, ldst, Value::int(val));
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            if le {
                                va.as_int() <= val
                            } else {
                                va.as_int() < val
                            }
                        } else if va.is_double() {
                            let x = va.as_f64();
                            if le {
                                x <= val as f64
                            } else {
                                x < val as f64
                            }
                        } else if le {
                            self.cmp_le(base, a, ldst, true)?
                        } else {
                            self.cmp_lt(base, a, ldst, true)?
                        };
                        ip = if r { ip + 2 } else { target as usize };
                    }
                    Instr::FuseGlobalCmpJump { gdst, idx, a, le, target } => {
                        let g = self.globals[idx as usize];
                        let g = if g.is_uninitialized()
                            || (self.global_route_epoch != 0 && self.global_real_own_route(idx))
                        {
                            self.load_global_slow(idx, func_id)?
                        } else {
                            g
                        };
                        self.set(base, gdst, g);
                        let va = self.get(base, a);
                        let r = if va.is_int() && g.is_int() {
                            if le {
                                va.as_int() <= g.as_int()
                            } else {
                                va.as_int() < g.as_int()
                            }
                        } else if le {
                            self.cmp_le(base, a, gdst, true)?
                        } else {
                            self.cmp_lt(base, a, gdst, true)?
                        };
                        ip = if r { ip + 2 } else { target as usize };
                    }
                    Instr::FuseAddLoadInt { ldst, val, dst, a } => {
                        self.set(base, ldst, Value::int(val));
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            match va.as_int().checked_add(val) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 + val as f64),
                            }
                        } else if va.is_double() {
                            Value::num(va.as_f64() + val as f64)
                        } else {
                            self.add_values(va, Value::int(val))?
                        };
                        self.set(base, dst, r);
                        ip += 2;
                    }
                    Instr::FuseLoadGlobalAddInt { gdst, idx, dst, a, imm, upd } => {
                        let g = self.globals[idx as usize];
                        let g = if g.is_uninitialized()
                            || (self.global_route_epoch != 0 && self.global_real_own_route(idx))
                        {
                            self.load_global_slow(idx, func_id)?
                        } else {
                            g
                        };
                        self.set(base, gdst, g);
                        let va = self.get(base, a);
                        let r = self.add_int_value(va, imm, upd)?;
                        self.set(base, dst, r);
                        ip += 2;
                    }
                    Instr::FuseAddReturn { dst, a, b, src } => {
                        let r = self.add(base, a, b)?;
                        self.set(base, dst, r);
                        // A frame with `try` handlers may have a `finally` to
                        // run first: the `Return` at `ip + 1` routes it.
                        if !self.frames[frame_idx].handlers.is_empty() {
                            ip += 1;
                            continue;
                        }
                        let v = self.get(base, src);
                        if self.pop_frame_fast(v, stop_depth) {
                            return Ok(v);
                        }
                        break;
                    }
                    Instr::FuseLoadGlobal2 { dst, idx, dst2, idx2 } => {
                        let v = self.globals[idx as usize];
                        let v = if v.is_uninitialized()
                            || (self.global_route_epoch != 0 && self.global_real_own_route(idx))
                        {
                            self.load_global_slow(idx, func_id)?
                        } else {
                            v
                        };
                        self.set(base, dst, v);
                        let v = self.globals[idx2 as usize];
                        let v = if v.is_uninitialized()
                            || (self.global_route_epoch != 0 && self.global_real_own_route(idx2))
                        {
                            self.load_global_slow(idx2, func_id)?
                        } else {
                            v
                        };
                        self.set(base, dst2, v);
                        ip += 2;
                    }
                    Instr::FuseLoadIntBitwise { ldst, val, dst, a, b, op } => {
                        self.set(base, ldst, Value::int(val));
                        let (va, vb) = (self.get(base, a), self.get(base, b));
                        let r = self.fused_bitwise(va, vb, op)?;
                        self.set(base, dst, r);
                        ip += 2;
                    }
                    Instr::FuseEqJump { dst, a, b, ne, when, target } => {
                        let (va, vb) = (self.get(base, a), self.get(base, b));
                        let eq = if va.is_int() && vb.is_int() {
                            va.bits() == vb.bits()
                        } else {
                            self.strict_eq(base, a, b)
                        };
                        let r = eq != ne;
                        self.set(base, dst, Value::bool(r));
                        // Forward only (see the variant): no back-edge here.
                        ip = if r == when { target as usize } else { ip + 2 };
                    }
                    Instr::FuseIntEqJump { ldst, val, a, dst, target } => {
                        self.set(base, ldst, Value::int(val));
                        let va = self.get(base, a);
                        // `===` with an integer: an Int or a Number of that
                        // value (-0 equals 0, NaN nothing); any other type no.
                        let r = if va.is_int() {
                            va.as_int() == val
                        } else {
                            va.is_double() && va.as_f64() == val as f64
                        };
                        self.set(base, dst, Value::bool(r));
                        ip = if r { target as usize } else { ip + 3 };
                    }
                    Instr::FuseIntNeJump { ldst, val, a, dst, target } => {
                        self.set(base, ldst, Value::int(val));
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            va.as_int() == val
                        } else {
                            va.is_double() && va.as_f64() == val as f64
                        };
                        self.set(base, dst, Value::bool(r));
                        ip = if r { ip + 3 } else { target as usize };
                    }
                    Instr::FuseConstEqJump { ldst, idx, a, dst, when, target } => {
                        let c = self.resolve_const_slot(func_id, idx);
                        self.set(base, ldst, c);
                        let r = self.strict_eq(base, a, ldst);
                        self.set(base, dst, Value::bool(r));
                        ip = if r == when { target as usize } else { ip + 3 };
                    }
                    Instr::FuseTypeOfJump { dst, a, code, neg, when, target } => {
                        let va = self.get(base, a);
                        let t = self.type_of(va);
                        let m = crate::bytecode::TYPEOF_NAMES
                            .get(code as usize)
                            .is_some_and(|&n| n == t)
                            != neg;
                        self.set(base, dst, Value::bool(m));
                        ip = if m == when { target as usize } else { ip + 2 };
                    }
                    Instr::FuseNotJump { dst, a, when, target } => {
                        let m = !self.truthy(self.get(base, a));
                        self.set(base, dst, Value::bool(m));
                        ip = if m == when { target as usize } else { ip + 2 };
                    }

                    Instr::Lt { dst, a, b } => {
                        let r = self.cmp_lt(base, a, b, true)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Le { dst, a, b } => {
                        let r = self.cmp_le(base, a, b, true)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Gt { dst, a, b } => {
                        // `a > b` ≡ IsLessThan(b, a, LeftFirst=false): swap registers,
                        // but the source-left operand `a` must still coerce first.
                        let r = self.cmp_lt(base, b, a, false)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Ge { dst, a, b } => {
                        let r = self.cmp_le(base, b, a, false)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::LooseEq { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        // Two small integers, as `Eq` decides them: `loose_eq`
                        // tested objects, BigInts and tags before comparing.
                        let r = if va.is_int() && vb.is_int() {
                            va.bits() == vb.bits()
                        } else {
                            match self.loose_eq_nullish(va, vb) {
                                Some(r) => r,
                                None => self.loose_eq(va, vb)?,
                            }
                        };
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::LooseNe { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if va.is_int() && vb.is_int() {
                            va.bits() == vb.bits()
                        } else {
                            match self.loose_eq_nullish(va, vb) {
                                Some(r) => r,
                                None => self.loose_eq(va, vb)?,
                            }
                        };
                        self.set(base, dst, Value::bool(!r));
                        ip += 1;
                    }
                    Instr::Eq { dst, a, b } => {
                        // Two small integers (one bit pattern per value) are
                        // the loop counters and indices this compares most:
                        // decided here instead of in `strict_eq`'s call.
                        let (va, vb) = (self.get(base, a), self.get(base, b));
                        let r = if va.is_int() && vb.is_int() {
                            va.bits() == vb.bits()
                        } else {
                            self.strict_eq(base, a, b)
                        };
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Ne { dst, a, b } => {
                        let (va, vb) = (self.get(base, a), self.get(base, b));
                        let r = if va.is_int() && vb.is_int() {
                            va.bits() == vb.bits()
                        } else {
                            self.strict_eq(base, a, b)
                        };
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
                        let v = self.typeof_value(va);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::TypeOfIs { dst, a, code, neg } => {
                        // Fused `typeof a === "lit"`: compare the classifier's
                        // `&'static str` by content — no heap string, no Eq
                        // dispatch, and by construction identical to what the
                        // unfused pair computes (code 255 indexes nothing and
                        // so never matches).
                        let va = self.get(base, a);
                        let t = self.type_of(va);
                        let m = crate::bytecode::TYPEOF_NAMES
                            .get(code as usize)
                            .is_some_and(|&n| n == t);
                        self.set(base, dst, Value::bool(m != neg));
                        ip += 1;
                    }
                    Instr::TypeOfSame { dst, a, b, neg } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let same = self.type_of(va) == self.type_of(vb);
                        self.set(base, dst, Value::bool(same != neg));
                        ip += 1;
                    }
                    Instr::IsArray {
                        dst,
                        a,
                        callee,
                        this_v,
                    } => {
                        let v = self.get(base, a);
                        let callee = self.get(base, callee);
                        let this_v = self.get(base, this_v);
                        if !self.static_builtin_is_intrinsic(
                            "Array",
                            native::ARR_IS_ARRAY,
                            callee,
                            this_v,
                        ) {
                            let out = self.call_value(callee, this_v, &[v])?;
                            self.set(base, dst, out);
                            ip += 1;
                            continue;
                        }
                        let is_arr = self.value_is_array_throwing(v)?;
                        self.set(base, dst, Value::bool(is_arr));
                        ip += 1;
                    }
                    Instr::ArrayAppend { arr, val, spread } => {
                        let aidx = self.get(base, arr).heap_index();
                        let vv = self.get(base, val);
                        self.array_append_op(aidx, vv, spread)?;
                        ip += 1;
                    }
                    Instr::ObjectSpread { target, src } => {
                        let t = self.get(base, target);
                        let s = self.get(base, src);
                        self.object_spread(t, s)?; // defines into the literal in place
                        ip += 1;
                    }
                    Instr::New {
                        dst,
                        callee,
                        arg_base,
                        argc,
                    } => {
                        let cv = self.get(base, callee);
                        // A plain constructor function: its frame is pushed
                        // here, in this loop, as a call's is, and its pop
                        // hands back the instance (`construct_this`).
                        let flat = match self.flat_new_instance(cv) {
                            Some((fid, closure, obj)) => Some((fid, closure, obj, cv)),
                            None => self.flat_new_class_instance(cv)?,
                        };
                        if let Some((fid, closure, obj, callee_v)) = flat {
                            let rooted = self.host_result_roots.len();
                            self.host_result_roots.push(obj);
                            self.pending_new_target = cv;
                            let pushed = self.setup_call(
                                fid, closure, obj, base, arg_base, argc, dst, ip + 1, callee_v,
                            );
                            self.pending_new_target = Value::UNDEFINED;
                            self.host_result_roots.truncate(rooted);
                            pushed?;
                            if let Some(f) = self.frames.last_mut() {
                                f.construct_this = true;
                            }
                            break;
                        }
                        self.new_construct(base, ip, dst, cv, arg_base, argc)?;
                        ip += 1;
                    }
                    Instr::FieldInit {
                        key_index,
                        val,
                        class_id,
                    } => {
                        let this = self.get(base, 0);
                        let v = self.get(base, val);
                        // The computed key was evaluated once at class definition.
                        // Resolve via the EXECUTING class (a parent's return-
                        // override makes `this` a foreign object — possibly with
                        // no class link at all); fall back to `this`'s class for
                        // the sentinel (a base class, no override possible).
                        let key = if class_id != u32::MAX {
                            self.class_value_for(class_id)
                                .filter(|cv| cv.is_heap())
                                .and_then(|cv| match self.heap.get(cv.heap_index()) {
                                    HeapObj::Class(c) => {
                                        c.computed_field_keys.get(key_index as usize).copied()
                                    }
                                    _ => None,
                                })
                        } else {
                            match self.heap.get(this.heap_index()) {
                                HeapObj::Object(m) => {
                                    m.class.and_then(|cidx| match self.heap.get(cidx) {
                                        HeapObj::Class(c) => {
                                            c.computed_field_keys.get(key_index as usize).copied()
                                        }
                                        _ => None,
                                    })
                                }
                                _ => None,
                            }
                        };
                        if let Some(key) = key {
                            // CreateDataPropertyOrThrow (an own define, prototype
                            // setters never consulted; Proxy defineProperty fires).
                            let ks = self.key_of(key);
                            self.define_field(this, &ks, v)?;
                        }
                        ip += 1;
                    }
                    Instr::ThisCheck { src } => {
                        let v = self.get(base, src);
                        if v.is_heap() && self.this_tdz.contains(&v.heap_index()) {
                            return Err(Thrown(
                                "ReferenceError: must call super constructor before accessing 'this' in a derived class constructor".into(),
                            ));
                        }
                        // An arrow/eval closure that captured the derived ctor's
                        // uninitialized `this` placeholder still holds that object
                        // after a LATER `super()` (possibly from another escaped
                        // closure) initialized the binding to a return-override
                        // instance: resolve the read through the banked mapping.
                        if v.is_heap() {
                            if let Some(&produced) = self.super_this.get(&v.heap_index()) {
                                self.set(base, src, produced);
                            }
                        }
                        ip += 1;
                    }
                    Instr::SuperCtor {
                        ctor,
                        home_class_id,
                        arg_base,
                        argc,
                    } => {
                        self.super_ctor_call(base, ip, ctor, home_class_id, arg_base, argc)?;
                        ip += 1;
                    }
                    Instr::SuperBase { dst, home_class_id } => {
                        // GetSuperBase at MakeSuperPropertyReference time — the
                        // compiler places this BEFORE the args/RHS of the
                        // reference's use; the consumer reads the captured value.
                        let v = self
                            .super_base(home_class_id, self.func(func_id as usize).super_static);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::SuperGet {
                        dst,
                        home_class_id,
                        name,
                    } => {
                        // `super.name` read: resolve on the super base (the home
                        // object's [[Prototype]]) with `this` = the current receiver
                        // (so a getter sees it). For a base class the base is
                        // %Object.prototype%.
                        let key =
                            self.func(func_id as usize).string_constants[name as usize].as_str();
                        // ── interpreter super-property IC ── data reads hit
                        // directly; a cached plain GETTER frame-calls with
                        // `this` = the current receiver and ret_dst = dst.
                        {
                            let is_static = self.func(func_id as usize).super_static;
                            match self.ic_super_get(func_id, ip, home_class_id, is_static, key) {
                                GetAct::Value(v) => {
                                    self.set(base, dst, v);
                                    ip += 1;
                                    continue;
                                }
                                GetAct::Accessor {
                                    fid,
                                    closure,
                                    getter,
                                } => {
                                    let this = self.get(base, 0);
                                    self.setup_call(
                                        fid,
                                        closure,
                                        this,
                                        base,
                                        0,
                                        0,
                                        dst,
                                        ip + 1,
                                        getter,
                                    )?;
                                    break;
                                }
                                GetAct::None => {}
                            }
                        }
                        let proto = self
                            .super_base(home_class_id, self.func(func_id as usize).super_static);
                        // MakeSuperPropertyReference: RequireObjectCoercible(base).
                        self.require_object_coercible(proto)?;
                        let this = self.get(base, 0);
                        let r = self.get_member(proto, &key, this)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperSet {
                        base: base_reg,
                        home_class_id,
                        name,
                        val,
                    } => {
                        let key =
                            self.func(func_id as usize).string_constants[name as usize].as_str();
                        let this = self.get(base, 0);
                        let v = self.get(base, val);
                        let is_static = self.func(func_id as usize).super_static;
                        // ── interpreter super-property IC ── a cached plain
                        // SETTER on the super chain frame-calls with `this` =
                        // the receiver and ret_dst = RET_DISCARD; data writes
                        // (which go to the RECEIVER) stay on the slow path. The
                        // entry is version-guarded on the same chain the
                        // SuperBase op captured, so a retargeted base misses it.
                        if let SetAct::Setter {
                            fid,
                            closure,
                            setter,
                        } = self.ic_super_set(func_id, ip, home_class_id, is_static, key)
                        {
                            self.setup_call(
                                fid,
                                closure,
                                this,
                                base,
                                val,
                                1,
                                RET_DISCARD,
                                ip + 1,
                                setter,
                            )?;
                            break;
                        }
                        // PutValue strictness comes from the reference site (the
                        // enclosing function) — class methods are always strict.
                        let strict = self.func(func_id as usize).is_strict;
                        let proto = self.get(base, base_reg);
                        self.super_set_obj(proto, &key, this, v, strict)?;
                        ip += 1;
                    }
                    Instr::NewMap { dst, src } => {
                        // Entries are added through the `set` adder resolved off the
                        // new map, so an overridden `Map.prototype.set` is honoured
                        // (the builtin set inserts + normalizes -0 itself).
                        let m = Value::heap(self.heap.alloc(HeapObj::Map {
                            keys: Vec::new(),
                            vals: Vec::new(),
                        }));
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                let adder = self.get_member(m, "set", m)?;
                                if !self.is_callable(adder) {
                                    return Err(Thrown(
                                        "TypeError: Map.prototype.set is not callable".into(),
                                    ));
                                }
                                // AddEntriesFromIterable: step the iterator lazily;
                                // each entry must be an Object; read k/v and call the
                                // adder; an abrupt completion runs IteratorClose.
                                let iter = self.get_iterator_object(sv)?;
                                let mut native_work = 0u64;
                                loop {
                                    let e = match self.iterator_step(iter)? {
                                        Some(v) => v,
                                        None => break,
                                    };
                                    native_work = native_work.saturating_add(1);
                                    if let Err(err) =
                                        self.preflight_native_iteration_work(native_work)
                                    {
                                        self.iterator_close_quiet(iter);
                                        return Err(err);
                                    }
                                    if !self.is_object_value(e) {
                                        self.iterator_close_quiet(iter);
                                        return Err(Thrown(
                                            "TypeError: Map iterable entry is not an object".into(),
                                        ));
                                    }
                                    let k = match self.get_index(e, Value::int(0)) {
                                        Ok(k) => k,
                                        Err(err) => {
                                            self.iterator_close_quiet(iter);
                                            return Err(err);
                                        }
                                    };
                                    let v = match self.get_index(e, Value::int(1)) {
                                        Ok(v) => v,
                                        Err(err) => {
                                            self.iterator_close_quiet(iter);
                                            return Err(err);
                                        }
                                    };
                                    if let Err(err) = self.call_value(adder, m, &[k, v]) {
                                        self.iterator_close_quiet(iter);
                                        return Err(err);
                                    }
                                }
                            }
                        }
                        self.set(base, dst, m);
                        ip += 1;
                    }
                    Instr::NewSet { dst, src } => {
                        let set_v = Value::heap(self.heap.alloc(HeapObj::Set(Vec::new())));
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                let adder = self.get_member(set_v, "add", set_v)?;
                                if !self.is_callable(adder) {
                                    return Err(Thrown(
                                        "TypeError: Set.prototype.add is not callable".into(),
                                    ));
                                }
                                // Lazy iteration + IteratorClose on an abrupt adder.
                                let iter = self.get_iterator_object(sv)?;
                                let mut native_work = 0u64;
                                loop {
                                    let e = match self.iterator_step(iter)? {
                                        Some(v) => v,
                                        None => break,
                                    };
                                    native_work = native_work.saturating_add(1);
                                    if let Err(err) =
                                        self.preflight_native_iteration_work(native_work)
                                    {
                                        self.iterator_close_quiet(iter);
                                        return Err(err);
                                    }
                                    if let Err(err) = self.call_value(adder, set_v, &[e]) {
                                        self.iterator_close_quiet(iter);
                                        return Err(err);
                                    }
                                }
                            }
                        }
                        self.set(base, dst, set_v);
                        ip += 1;
                    }
                    Instr::MathOp {
                        dst,
                        op,
                        callee,
                        this_v,
                        arg_base,
                        argc,
                    } => {
                        if callee == crate::bytecode::NO_REG {
                            // BARE form: validate the live global + own slot.
                            if self.math_bare_is_intrinsic(op, this_v as u32) {
                                let r = self.eval_math(op, base, arg_base, argc)?;
                                self.set(base, dst, Value::num(r));
                                ip += 1;
                                continue;
                            }
                            // Miss: the ordinary member call, performed on the
                            // LIVE global — exactly `LoadGlobal Math; GetProp
                            // <op>; CallWithThis` (nothing between the two can
                            // have run user code: the arguments are transparent).
                            let Some(gslot) = self.bare_math_slot(this_v as u32) else {
                                return Err(Thrown(
                                    "ReferenceError: Math is not defined".to_string(),
                                ));
                            };
                            let recv = self.load_global_value(gslot, func_id)?;
                            let name = native::MATH_METHODS[op as usize].0;
                            let f = self.get_prop(recv, name)?;
                            let args: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let result = self.call_value(f, recv, &args)?;
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        let callee = self.get(base, callee);
                        let this_v = self.get(base, this_v);
                        let native_id = native::MATH_METHOD_BASE + op as u16;
                        if !self.static_builtin_is_intrinsic("Math", native_id, callee, this_v) {
                            let args: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let result = self.call_value(callee, this_v, &args)?;
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        let r = self.eval_math(op, base, arg_base, argc)?;
                        self.set(base, dst, Value::num(r));
                        ip += 1;
                    }
                    Instr::GlobalFn {
                        dst,
                        op,
                        callee,
                        arg_base,
                        argc,
                    } => {
                        use crate::bytecode::GlobalFn as G;
                        let live = self.get(base, callee);
                        if !self.global_fn_is_intrinsic(op, live) {
                            if let Ok((fid, closure)) = self.resolve_callable(live) {
                                if !self.func(fid as usize).is_generator
                                    && !self.func(fid as usize).is_async
                                {
                                    self.setup_call(
                                        fid,
                                        closure,
                                        Value::UNDEFINED,
                                        base,
                                        arg_base,
                                        argc,
                                        dst,
                                        ip + 1,
                                        live,
                                    )?;
                                    break;
                                }
                            }
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let out = self.call_value(live, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, out);
                            ip += 1;
                            continue;
                        }
                        let a0 = if argc >= 1 {
                            self.get(base, arg_base)
                        } else {
                            Value::UNDEFINED
                        };
                        let v = match op {
                            G::Number => {
                                if argc == 0 {
                                    Value::num(0.0)
                                } else {
                                    Value::num(self.to_number_coerce(a0)?)
                                }
                            }
                            G::String => {
                                if argc == 0 {
                                    self.alloc_str(String::new())
                                } else if a0.is_heap()
                                    && matches!(
                                        self.heap.get(a0.heap_index()),
                                        HeapObj::Symbol { .. }
                                    )
                                {
                                    // `String(symbol)` is allowed (unlike ToString,
                                    // which throws) and yields "Symbol(desc)".
                                    self.symbol_descriptive_value(a0)?
                                } else {
                                    // Proper ToString: routes objects/functions
                                    // through their `toString` (so a function yields
                                    // its real source via Function.prototype.toString).
                                    // Identity for strings (exact WTF-8 content).
                                    self.to_str_value(a0)?
                                }
                            }
                            G::Boolean => Value::bool(argc >= 1 && self.truthy(a0)),
                            G::ParseInt => {
                                // ToString(string) FIRST (observable toString, abrupt),
                                // then radix = ToInt32(ToNumber(radix)) (NaN/±Infinity
                                // → 0 = default base).
                                let s = self.to_js_string(a0)?;
                                // ToInt32 WRAPS mod 2^32; Rust's `as i32` on a
                                // float saturates, which turned
                                // `parseInt("0x10", 1e308)` into an
                                // out-of-range radix (NaN) instead of 0.
                                let radix = if argc >= 2 {
                                    let rv = self.get(base, arg_base + 1);
                                    let r = self.to_number_strict(rv)?;
                                    crate::vm::helpers_num2::to_int32(r)
                                } else {
                                    0
                                };
                                Value::num(parse_int(&s, radix))
                            }
                            G::ParseFloat => {
                                let s = self.to_js_string(a0)?;
                                Value::num(parse_float(&s))
                            }
                            // isNaN/isFinite are `Number::isNaN/isFinite(? ToNumber(x))`:
                            // ToNumber coerces objects (@@toPrimitive/valueOf/toString)
                            // and propagates abrupt completions (a throwing valueOf or
                            // a Symbol/BigInt argument → TypeError), so use strict ToNumber.
                            G::IsNaN => Value::bool(self.to_number_strict(a0)?.is_nan()),
                            G::IsFinite => Value::bool(self.to_number_strict(a0)?.is_finite()),
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::HasProp {
                        dst,
                        key,
                        obj,
                        brand,
                    } => {
                        if !brand {
                            if let Some(exit) = self.try_in_probe_reduce(func_id, ip) {
                                ip = exit;
                                continue;
                            }
                        }
                        let k = self.get(base, key);
                        let o = self.get(base, obj);
                        // Fast path: `i in arr` PRESENT on a dense array — the hot
                        // hole-aware iteration pattern. A numeric key needs no
                        // ToPropertyKey, and a non-hole in-range dense element is
                        // an own property regardless of side tables/prototypes.
                        // Misses (hole / out of range / overlay) take the full path.
                        if !brand && k.is_number() && o.is_heap() {
                            let aidx = o.heap_index();
                            if let HeapObj::Array(items) = self.heap.get(aidx) {
                                if let Some(i) = array_index(k) {
                                    if i < items.len() && !items[i].is_hole() {
                                        self.set(base, dst, Value::bool(true));
                                        ip += 1;
                                        continue;
                                    }
                                    // A hole, or an index past the dense end, is
                                    // absent unless an overlay holds it or the
                                    // chain can supply an index: the shortcut a
                                    // hole read takes (`get_index`), under the
                                    // same protector. It walked the full
                                    // HasProperty, key text and all.
                                    if crate::codegen::hole_undef_enabled()
                                        && !self.array_proto_has_index
                                        && !self.proto_of.contains_key(&aidx)
                                        && (self.arguments_objs.is_empty()
                                            || !self.arguments_objs.contains_key(&aidx))
                                        && self.array_index_override(aidx, i).is_none()
                                    {
                                        self.set(base, dst, Value::bool(false));
                                        ip += 1;
                                        continue;
                                    }
                                }
                            }
                        }
                        // The `in` operator (and `#x in`) require an Object right
                        // operand â€” a primitive RHS is a TypeError (checked before
                        // ToPropertyKey on the key, per spec order).
                        if !self.is_object_value(o) {
                            let kd = self.display(k);
                            return Err(Thrown(format!(
                                "TypeError: Cannot use 'in' operator to search for '{kd}' in a non-object"
                            )));
                        }
                        // A private name (`#x`) is not a string property key: a
                        // regular `in` (and Reflect.has) reports it absent. The
                        // ergonomic brand check `#x in obj` sets `brand` and skips
                        // this filter so it still observes the private element.
                        let r = if brand {
                            // `#x in obj` ergonomic brand check: a FIELD is
                            // present iff the side table has the entry; a
                            // method/accessor iff the receiver carries the
                            // declaring brand (textual fallback when none
                            // resolvable). The key is the name's string
                            // constant, so there is nothing to coerce, and the
                            // site (this instruction: its name has no constant
                            // index of its own, hence the high bit) resolves
                            // through the private memo without spelling it.
                            let ctx = self.frames.last().map_or(Value::UNDEFINED, |f| f.callee);
                            let site = super::values::private_site(func_id as u32, 0x8000_0000 | ip as u32);
                            let resolved = match self.private_memo_hit(ctx, site) {
                                Some(hit) => Some(hit),
                                None => {
                                    let key = self.key_of(k);
                                    self.resolve_private_site(ctx, site, &key)
                                }
                            };
                            if let Some((b2, kind, owner, pid)) = resolved {
                                if kind & 7 == 0 {
                                    self.private_field_get_id(o, b2, pid).is_some()
                                } else {
                                    self.private_receiver_ok(o, b2, kind, owner)
                                }
                            } else {
                                let key = self.key_of(k);
                                let textual = self.has_property_str(o, &key)
                                    || self.private_field_scan_has(o, &key);
                                match self.private_brand_ok(o, &key) {
                                    Some(b) => textual && b,
                                    None => textual,
                                }
                            }
                        } else {
                            // (Real private fields live in the side table, so an
                            // ordinary `in` never sees them; a PUBLIC "#..."
                            // string key is an ordinary property.)
                            // Proxy-aware [[HasProperty]]: dispatches a `has` trap
                            // when the proxy is the receiver OR is in the proto chain.
                            // ToPropertyKey first: an object key is ToString-coerced
                            // (toString/valueOf), not rendered "[object Object]".
                            let k = self.coerce_index_key(k)?;
                            self.has_property_dyn(o, k)?
                        };
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::InstanceOfDyn { dst, val, ctor } => {
                        let v = self.get(base, val);
                        let c = self.get(base, ctor);
                        let b = self.instance_of_op(c, v)?;
                        self.set(base, dst, Value::bool(b));
                        ip += 1;
                    }
                    Instr::StaticFn {
                        dst,
                        op,
                        callee,
                        this_v,
                        arg_base,
                        argc,
                    } => {
                        use crate::bytecode::StaticFn as S;
                        let callee = self.get(base, callee);
                        let this_v = self.get(base, this_v);
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        // The method reference was resolved before argument
                        // evaluation. On any identity miss, call that captured
                        // function with that captured receiver; never re-read the
                        // global/property after an argument may have mutated it.
                        if !self.static_fn_is_intrinsic(op, callee, this_v) {
                            let v = self.call_value(callee, this_v, &args)?;
                            self.set(base, dst, v);
                            ip += 1;
                            continue;
                        }
                        let v = self.static_fn_exec(op, args)?;
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ArrayFrom {
                        dst,
                        src,
                        mapfn,
                        callee,
                        this_v,
                        argc,
                    } => {
                        let sv = self.get(base, src);
                        let fnv = self.get(base, mapfn);
                        let callee = self.get(base, callee);
                        let this_v = self.get(base, this_v);
                        let out = if self.static_builtin_is_intrinsic(
                            "Array",
                            native::ARR_FROM,
                            callee,
                            this_v,
                        ) {
                            self.array_from(this_v, sv, fnv, Value::UNDEFINED)?
                        } else {
                            let args = if argc == 1 { vec![sv] } else { vec![sv, fnv] };
                            self.call_value(callee, this_v, &args)?
                        };
                        self.set(base, dst, out);
                        ip += 1;
                    }
                    Instr::Jump { target } => {
                        let t = target as usize;
                        // A backward jump is a loop back-edge â€” poll the GC here so
                        // a tight allocating loop (which never leaves this inner
                        // loop) still gets collected. Safe: all live Values are in
                        // regs, and gc_lock guards any native built-in up-stack.
                        if t < ip {
                            self.maybe_gc();
                        }
                        // â”€â”€ OSR tier â”€â”€ a backward jump is a loop back-edge. After
                        // the region heats up, compile `[target, ip]` (the loop
                        // body, headed at `target`) and run it natively; the
                        // native code returns the ip to resume at (a clean loop
                        // exit or a guard bail). Gated like the function JIT:
                        // enabled, and not inside a native self-recursion.
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        if !LITE
                            && self.jit_enabled
                            && self.jit_recurse_depth == 0
                            && self.jit_func_eligible(func_id)
                            && t < ip
                            // Dense region-state check first: a permanently
                            // blacklisted loop pays two array reads per
                            // back-edge here, not 2 hash probes (`get_region`
                            // miss + `region_blacklist` hit). Mirrors the
                            // FN_DEAD check at frame entry — see
                            // `Jit::region_dead`. `ZIPP_NO_DENSE_BACKEDGE=1`
                            // restores the old always-probe path.
                            && !self.jit.region_dead(func_id, t as u32)
                        {
                            if let Some(resume) = self.try_run_osr(func_id, t as u32, base) {
                                // A region exit with a pending exception (a
                                // frame-called function threw — `CALL_THREW`):
                                // UNWIND to this frame's handlers instead of
                                // resuming (re-executing the call would double
                                // its side effects). Mirrors the function-JIT
                                // bail-(b) path above.
                                if self.pending_throw.is_some() {
                                    let top = self.frames.len() - 1;
                                    self.frames[top].ip = resume;
                                    return Err(Thrown(String::new()));
                                }
                                ip = resume;
                                continue;
                            }
                            // A global op whose slot is still UNINITIALIZED may
                            // be own-prop-backed (eval-created / `this.x` bindings):
                            // raw JIT slot accesses would bypass the own property.
                            // Don't compile yet; a later attempt re-checks
                            // (`region_defer_globals` re-arms the threshold a
                            // bounded number of times). Checked ONLY
                            // when the back-edge counter trips, so a rejected /
                            // still-counting loop never pays the body scan.
                            if self.jit.record_region(func_id, t as u32) {
                                // ── whole-loop region end ── a loop whose body has
                                // several `continue`s compiles to MANY back-edges
                                // `Jump{target==header}`. The back-edge that heats
                                // up first (this `ip`) may be a MIDDLE one, leaving
                                // the branches below it (a later `if`/`else` arm)
                                // outside the region — the native code would then
                                // exit to the interpreter on every iteration that
                                // takes one of those arms and re-enter (death by
                                // OSR churn). Extend the region to the LAST back-edge
                                // to this header (`Jump{target==t}` at the highest
                                // ip): that is the loop's textual bottom, so the
                                // region spans the whole body. Every intermediate
                                // back-edge becomes an in-region jump to the header
                                // label (region_target handles it), and the only
                                // out-of-region transfer stays the loop-exit jump
                                // (an exit stub). A nested loop has its OWN header,
                                // so its back-edges don't target `t` and don't
                                // extend us past this loop.
                                let region_end = {
                                    let proto = self.func(func_id as usize);
                                    let last = proto.code.len() - 1;
                                    let mut e = (ip as usize).min(last);
                                    for p in (ip as usize + 1)..=last {
                                        if matches!(proto.code[p], Instr::Jump { target } if target as usize == t)
                                        {
                                            e = p;
                                        }
                                    }
                                    e
                                };
                                let region_globals_ok = {
                                    let proto = self.func(func_id as usize);
                                    let s = t as usize;
                                    let e = region_end;
                                    // Shared predicate — see `global_slot_directly_routable`.
                                    // A store additionally refuses a slot the global
                                    // object now shadows with a real descriptor.
                                    proto.code[s..=e].iter().all(|ins| match *ins {
                                        Instr::LoadGlobal { idx, .. }
                                        | Instr::LoadGlobalOrUndefined { idx, .. } => {
                                            self.global_slot_directly_routable(idx)
                                        }
                                        Instr::StoreGlobal { idx, .. }
                                        | Instr::StoreGlobalStrict { idx, .. }
                                        | Instr::StoreGlobalResolved { idx, .. } => {
                                            self.global_store_slot_directly_routable(idx)
                                        }
                                        _ => true,
                                    })
                                };
                                if !region_globals_ok {
                                    // Re-arm (bounded): a later threshold
                                    // re-checks in case the slot was
                                    // initialized meanwhile.
                                    self.jit.region_defer_globals(func_id, t as u32);
                                    ip = t;
                                    continue;
                                }
                                // ── call-mix gate ── a Call/CallMethod site
                                // whose interpreter IC stayed EMPTY through the
                                // warmup (a native/megamorphic callee) will
                                // fall back through the helper on EVERY
                                // iteration (~10ns FFI overhead vs the
                                // interpreter). Compile only when the region
                                // has enough other ops for the native savings
                                // to cover that (op count ≥ 20× such sites) —
                                // e.g. a DataView-call loop with heavy bit math
                                // wins, a bare `m.get(k)` loop would lose.
                                let region_call_mix_ok = {
                                    let proto = self.func(func_id as usize);
                                    let s = t as usize;
                                    let e = region_end;
                                    let mut fallback = 0usize;
                                    for (off, ins) in proto.code[s..=e].iter().enumerate() {
                                        match ins {
                                            Instr::CallMethod { name, argc, .. } => {
                                                let key = proto
                                                    .string_constants
                                                    .get(*name as usize)
                                                    .map(|x| x.as_str());
                                                if *argc == 1
                                                    && matches!(
                                                        key,
                                                        Some("push")
                                                            | Some("charCodeAt")
                                                            | Some("indexOf")
                                                            | Some("get")
                                                            | Some("has")
                                                    )
                                                {
                                                    continue; // dedicated helpers
                                                }
                                                if (*argc == 2
                                                    || (*argc == 1
                                                        && crate::codegen::substring1_intrinsic_enabled()))
                                                    && matches!(
                                                        key,
                                                        Some("substring") | Some("slice")
                                                    )
                                                {
                                                    continue; // dedicated helper
                                                }
                                                // Collection MUTATORS have no
                                                // intrinsic yet, but a region full
                                                // of them is still worth compiling
                                                // for the surrounding loop work —
                                                // map-set-heavy compiled NOTHING
                                                // because of this. Named
                                                // explicitly rather than by
                                                // loosening the ratio, which was
                                                // tried and cost
                                                // async-promise-chain 8%. Only
                                                // sites that never resolved a user
                                                // function reach here, so a user
                                                // `obj.set()` is unaffected.
                                                if matches!(
                                                    key,
                                                    Some("set") | Some("delete") | Some("add")
                                                ) {
                                                    continue;
                                                }
                                                if (*argc == 1 || *argc == 2)
                                                    && key.is_some_and(|k| {
                                                        crate::codegen::dv_get_kind(k).is_some()
                                                    })
                                                {
                                                    continue; // dedicated DataView get* helper
                                                }
                                                // `it.next()` on a generator or a
                                                // built-in iterator: the builtin
                                                // fallback steps it directly
                                                // (`generator_method`, the
                                                // `ITER_NEXT` lane), so the loop
                                                // around it — the `done`/`value`
                                                // reads and the work — is worth
                                                // compiling; it ran on the
                                                // interpreter.
                                                if *argc == 0 && key == Some("next") {
                                                    continue;
                                                }
                                                // `a.pop()`: the builtin fallback
                                                // pops a dense Array in place
                                                // (`try_builtin_method`); a stack
                                                // loop of push/pop ran on the
                                                // interpreter for want of it.
                                                if *argc == 0 && key == Some("pop") {
                                                    continue;
                                                }
                                                // B82: `f.call(…)` (0-3 forwarded
                                                // args) / `f.apply(this[, arr])`
                                                // sites are served per-call by
                                                // `try_fn_call_apply_inline` (an
                                                // inline-eligible user target) or
                                                // by the `hasOwnProperty.call`
                                                // intrinsic — the IC stays empty
                                                // (the resolved `call`/`apply` is
                                                // a native) but the helper does
                                                // NOT fall back per iteration.
                                                if crate::codegen::call_inline_enabled()
                                                    && ((*argc <= 4 && key == Some("call"))
                                                        || (*argc <= 2 && key == Some("apply")))
                                                {
                                                    continue;
                                                }
                                            }
                                            Instr::Call { .. } => {}
                                            _ => continue,
                                        }
                                        let site = (func_id as usize)
                                            .checked_sub(0)
                                            .and_then(|f| self.site_ics.get(f))
                                            .and_then(|x| x.as_ref())
                                            .and_then(|slots| slots.get(s + off))
                                            .and_then(|x| x.site.as_deref());
                                        if let Some(site) = site {
                                            if site.n == 0 && site.misses >= 4 {
                                                fallback += 1;
                                            }
                                        }
                                    }
                                    fallback == 0 || (e - s + 1) >= fallback * 20
                                };
                                if !region_call_mix_ok {
                                    self.jit.blacklist_region(func_id, t as u32);
                                    ip = t;
                                    continue;
                                }
                                // Pre-intern the region's multi-char string
                                // constants so the emitter can embed their bits
                                // as immediates. Rooted for the VM's life in
                                // `jit_const_strings` (compiled code isn't
                                // traced by the GC).
                                let pending: Vec<u32> = {
                                    let proto = self.func(func_id as usize);
                                    proto.code[t..=region_end]
                                        .iter()
                                        .filter_map(|ins| match *ins {
                                            Instr::LoadConst { idx, .. } => {
                                                let c = proto.constants[idx as usize];
                                                if c.is_heap()
                                                    && (c.heap_index() & STRING_CONST_BIT) != 0
                                                {
                                                    Some(idx)
                                                } else {
                                                    None
                                                }
                                            }
                                            _ => None,
                                        })
                                        .collect()
                                };
                                let mut const_strs: rustc_hash::FxHashMap<u32, u64> =
                                    rustc_hash::FxHashMap::default();
                                for idx in pending {
                                    if const_strs.contains_key(&idx) {
                                        continue;
                                    }
                                    let c = self.func(func_id as usize).constants[idx as usize];
                                    let v = self.resolve_const(func_id, c);
                                    self.jit_const_strings.push(v); // GC root
                                    const_strs.insert(idx, v.bits());
                                }
                                // ── TypedArray pin plan ── from LIVE state: tie
                                // each GetIndex/SetIndex to a pinned-TA snapshot
                                // when its receiver re-reads from a stable
                                // source (un-stored global / never-written reg)
                                // that holds a non-BigInt TypedArray NOW. The
                                // hint is verified per access by an identity
                                // guard, so a stale/wrong hint is always safe.
                                let ta_plan = self.build_ta_pin_plan(
                                    func_id,
                                    t as u32,
                                    region_end as u32,
                                    base,
                                );
                                // Q4 leaf-call inline plan: monomorphic plain-leaf
                                // callees at this region's Call sites (read-only —
                                // built before the &proto borrow below).
                                let leaf_plan = self.build_leaf_inline_plan(
                                    func_id,
                                    t as u32,
                                    region_end as u32,
                                );
                                // Bounded dense `array[key](args...)` leaves for
                                // the INTEGER splice. Built from the same live
                                // frame snapshot as TA/method plans; emitted code
                                // independently revalidates every identity/version
                                // tuple before running any region instruction.
                                let computed_leaf_plan = self.build_dense_computed_leaf_plan(
                                    func_id,
                                    t as u32,
                                    region_end as u32,
                                    base,
                                );
                                // Q7 method-call inline plan (reads the live receiver
                                // exemplar at each CallMethod's obj reg — needs `base`).
                                let method_plan = self.build_method_inline_plan(
                                    func_id,
                                    t as u32,
                                    region_end as u32,
                                    base,
                                    false,
                                );
                                // Nursery: a baked trivial-setter arm stores
                                // `this.<field>` CALL-FREE (`emit_mi_body`'s
                                // SetProp{obj:0} — `mov [vals_ptr+slot*8]`),
                                // so no barrier ever sees it. Register every
                                // storing arm's receiver as a persistent
                                // minor-trace root instead (at most eight arms
                                // per site, baked into the code).
                                if self.heap.nursery_on() {
                                    for plan in method_plan.values() {
                                        for shape in &plan.shapes {
                                            let stores = shape.body.iter().any(|op| {
                                                matches!(
                                                    op,
                                                    crate::bytecode::Instr::SetProp { obj: 0, .. }
                                                        | crate::bytecode::Instr::SuperSet { .. }
                                                )
                                            });
                                            if stores {
                                                let v =
                                                    crate::value::Value::from_bits(shape.recv_bits);
                                                if v.is_heap() {
                                                    self.heap.register_scan_root(v.heap_index());
                                                }
                                            }
                                        }
                                    }
                                }
                                let proto: *const crate::bytecode::FuncProto =
                                    self.func(func_id as usize);
                                // SAFETY: program functions are immutable during run.
                                let proto_ref = unsafe { &*proto };
                                let heap_helper_addrs = self.jit_heap_helper_addrs();
                                // ── globals that are NOT slot bindings ──
                                // An UNINITIALIZED slot is not an empty binding:
                                // a script GlobalDeclarationInstantiation
                                // ($262.evalScript, and the test262 harness
                                // prelude) parks its var/function bindings as OWN
                                // PROPERTIES of the global object and leaves the
                                // slot UNINITIALIZED on purpose, so the
                                // interpreter's Load/StoreGlobal own-prop
                                // fallbacks govern them. A region compiles
                                // LoadGlobal to a bare `mov rax,[r12+idx*8]` with
                                // no such fallback, so it would read the sentinel
                                // and hand the region `undefined`.
                                //
                                // That is a TIER DIVERGENCE, the failure mode this
                                // engine gates hardest against: a harness function
                                // called in a loop worked for the interpreted
                                // iterations and turned into "undefined is not a
                                // function" the instant the region tiered up —
                                // always at the same iteration, which reads like a
                                // scoping bug and is not one. Decline the region;
                                // the interpreter is already correct.
                                //
                                // The scan that used to sit HERE was a byte-for-byte
                                // duplicate of `region_globals_ok` above, over the
                                // identical `[t..=region_end]` range — and it declined
                                // WITHOUT `region_defer`, so a region rejected only by
                                // this copy was never reconsidered even after the
                                // binding became a real slot (`record_region` returns
                                // true once per key). Folded into the single gate
                                // above, which does re-arm. One fewer hand-maintained
                                // copy of a cross-tier fact is the whole point of B66.
                                // Tier-C cross-call plan (B83) — the region's Call
                                // sites get the native→native attempt too.
                                let (cross_plan, cross_pending, cross_baked) =
                                    self.build_cross_call_plan(func_id, Some(base));
                                self.jit.note_cross_baked(func_id, &cross_baked);
                                // B199: a "no entry yet" decline at a REGION is
                                // deferred (capped) instead of baked — the callee
                                // is being called from this very loop, so its
                                // entry lands within a threshold window and the
                                // region then compiles WITH the lane. An evicted
                                // live region cannot recompile mid-run, so
                                // prevention beats the whole-fn retry here.
                                if !cross_pending.is_empty()
                                    && self.jit.cross_defer_allowed(func_id, t as u32)
                                {
                                    self.jit.region_defer(func_id, t as u32);
                                    ip = t;
                                    continue;
                                }
                                self.jit.note_cross_pending(func_id, &cross_pending);
                                // A Python loop: which of its `PyCall` sites
                                // call a native, for the call-bound gate
                                // (`codegen::py`).
                                #[cfg(feature = "python")]
                                self.py_native_call_sites(func_id);
                                self.jit.compile_region(
                                    func_id,
                                    proto_ref,
                                    t as u32,
                                    region_end as u32,
                                    &self.globals,
                                    &self.heap,
                                    jit_globals_base as *const () as usize,
                                    heap_helper_addrs,
                                    self.program.global_count, // field-global pool base
                                    FIELD_POOL as u32,
                                    &const_strs,
                                    &ta_plan,
                                    &leaf_plan,
                                    &computed_leaf_plan,
                                    &method_plan,
                                    &cross_plan,
                                );
                                if let Some(resume) = self.try_run_osr(func_id, t as u32, base) {
                                    if self.pending_throw.is_some() {
                                        let top = self.frames.len() - 1;
                                        self.frames[top].ip = resume;
                                        return Err(Thrown(String::new()));
                                    }
                                    ip = resume;
                                    continue;
                                }
                            }
                        }
                        ip = t;
                    }
                    Instr::JumpIfFalse { cond, target } => {
                        let v = self.get(base, cond);
                        // A comparison's boolean, nearly always: no call.
                        let t = if v.is_bool() {
                            v.bits() == Value::TRUE.bits()
                        } else {
                            self.truthy(v)
                        };
                        if !t {
                            // A taken BACKWARD conditional closes a loop
                            // (`do … while`): the same GC safe point as the
                            // `Jump` back-edge, or an allocating loop with no
                            // frame transition never collects.
                            if (target as usize) < ip {
                                self.maybe_gc();
                            }
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfTrue { cond, target } => {
                        let v = self.get(base, cond);
                        let t = if v.is_bool() {
                            v.bits() == Value::TRUE.bits()
                        } else {
                            self.truthy(v)
                        };
                        if t {
                            // See `JumpIfFalse`: a backward taken branch is a
                            // loop back-edge.
                            if (target as usize) < ip {
                                self.maybe_gc();
                            }
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfNotLt { a, b, target } => {
                        // Two small (immediate) BigInts: the bound test of a
                        // Python `for` over a range, every iteration. What
                        // `cmp_lt` answers for them, without the call.
                        #[cfg(feature = "python")]
                        {
                            let (va, vb) = (self.get(base, a), self.get(base, b));
                            if va.is_small_bigint() && vb.is_small_bigint() {
                                if va.as_small_bigint() < vb.as_small_bigint() {
                                    ip += 1;
                                } else {
                                    ip = target as usize;
                                }
                                continue;
                            }
                        }
                        let r = self.cmp_lt(base, a, b, true)?;
                        if !r {
                            ip = target as usize;
                        } else {
                            #[cfg(all(feature = "meter-only", not(feature = "jit")))]
                            if ip >= 6
                                && target as usize >= 15
                                && target as usize - 15 == ip
                                && self.get(base, a) == Value::int(1)
                            {
                                // Run the first iteration normally so its
                                // SetProp sites allocate/fill the same retained
                                // IC storage as the ordinary interpreter.  The
                                // exact projection accepts the second header
                                // and fast-forwards the remaining pure cycle.
                                if let Some(exit) =
                                    self.try_metered_object_property_loop(func_id, base, ip)
                                {
                                    ip = exit;
                                    continue;
                                }
                            }
                            ip += 1;
                        }
                    }
                    Instr::JumpIfNotLe { a, b, target } => {
                        #[cfg(all(feature = "meter-only", not(feature = "jit")))]
                        if target as usize >= 5
                            && target as usize - 5 == ip
                            && self.get(base, a) == Value::int(1)
                            && self.try_metered_counted_sum_loop(func_id, base, ip, a, b, target)
                        {
                            ip = target as usize;
                            continue;
                        }
                        let r = self.cmp_le(base, a, b, true)?;
                        if !r {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }

                    Instr::MakeFunc { dst, func_id } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::Func(func_id)));
                        // A function created under a dynamic EvalScope keeps
                        // resolving its bindings (Dyn global ops consult it).
                        if let Some(sc) = self.ensure_frame_eval_scope(frame_idx) {
                            self.closure_eval_scope.insert(v.heap_index(), sc);
                        }
                        // A function created by createRealm-child code carries
                        // the child's realm tag (no-op in the main realm).
                        self.realm_tag_new(v.heap_index());
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ForInLive { dst, obj, key } => {
                        let o = self.get(base, obj);
                        let kv = self.get(base, key);
                        let live = self.forin_live(o, kv);
                        self.set(base, dst, Value::bool(live));
                        ip += 1;
                    }
                    Instr::LenOf { dst, obj } => {
                        let o = self.get(base, obj);
                        let v = if o.is_heap() {
                            match self.heap.get(o.heap_index()) {
                                // (For-in key snapshots are always dense, but stay
                                // virtual-length-aware for any future reuse.)
                                HeapObj::Array(items) => len_value(
                                    self.array_js_len
                                        .get(&o.heap_index())
                                        .map_or(items.len(), |&n| n as usize),
                                ),
                                HeapObj::Str(s) => len_value(s.units()),
                                HeapObj::Cons { len, .. } => len_value(*len),
                                // for-of over a Map/Set iterates `size` slots (a
                                // tombstoned/deleted entry doesn't count).
                                HeapObj::Map { .. } | HeapObj::Set(_) => {
                                    len_value(self.coll_live_len(o.heap_index()))
                                }
                                _ => Value::int(0),
                            }
                        } else {
                            Value::int(0)
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeClosure { dst, func_id } => {
                        let v = self.make_closure_in_frame(frame_idx, base, func_id, None);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeArrow {
                        dst,
                        func_id,
                        this_reg,
                    } => {
                        let v = self.make_closure_in_frame(frame_idx, base, func_id, Some(this_reg));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeCell { reg } => {
                        let v = self.get(base, reg);
                        let cell = self.heap.alloc_cell(v);
                        self.set(base, reg, Value::heap(cell));
                        ip += 1;
                    }
                    Instr::MarkCellConst { reg } => {
                        // The cell is already live (MakeCell / MakeCellTdz) and
                        // initialized; record it so a closure's write throws.
                        let cell = self.get(base, reg).heap_index();
                        self.const_cells.insert(cell);
                        self.const_cells_nonempty = 1;
                        ip += 1;
                    }
                    Instr::MakeCellTdz { reg } => {
                        // A captured lexical pre-created at entry: the cell starts in
                        // its TDZ (UNINITIALIZED) until the textual declaration runs.
                        let cell = self.heap.alloc_cell(Value::UNINITIALIZED);
                        self.set(base, reg, Value::heap(cell));
                        ip += 1;
                    }
                    Instr::CellGet { dst, cell } => {
                        let cell_idx = self.get(base, cell).heap_index();
                        let v = self.heap.cell_get(cell_idx);
                        if v.is_uninitialized() {
                            return Err(Thrown(
                                "ReferenceError: cannot access a lexical binding before initialization"
                                    .to_string(),
                            ));
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::CellSet { cell, src } => {
                        let cell_idx = self.get(base, cell).heap_index();
                        let v = self.get(base, src);
                        self.heap.cell_set(cell_idx, v);
                        ip += 1;
                    }
                    Instr::CellSetChecked { cell, src } => {
                        // An ASSIGNMENT to a lexical cell that may still be in
                        // its TDZ (`{ x = 1; …; let x; }`): writing before the
                        // declaration initializes it is a ReferenceError.
                        let cell_idx = self.get(base, cell).heap_index();
                        if self.heap.cell_get(cell_idx).is_uninitialized() {
                            return Err(Thrown(
                                "ReferenceError: cannot access a lexical binding before initialization"
                                    .to_string(),
                            ));
                        }
                        let v = self.get(base, src);
                        self.heap.cell_set(cell_idx, v);
                        ip += 1;
                    }
                    Instr::UpvalGet { dst, idx } => {
                        #[cfg(not(feature = "safe-sandbox"))]
                        let cell = self.closure_upvalue_cached(&mut upv, cur_closure, idx);
                        #[cfg(feature = "safe-sandbox")]
                        let cell = self.closure_upvalue(cur_closure, idx);
                        let v = self.heap.cell_get(cell);
                        if v.is_uninitialized() {
                            return Err(Thrown(
                                "ReferenceError: cannot access a lexical binding before initialization"
                                    .to_string(),
                            ));
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::UpvalSet { idx, src } => {
                        #[cfg(not(feature = "safe-sandbox"))]
                        let cell = self.closure_upvalue_cached(&mut upv, cur_closure, idx);
                        #[cfg(feature = "safe-sandbox")]
                        let cell = self.closure_upvalue(cur_closure, idx);
                        // Assignment to a captured `const` (PutValue on an
                        // immutable binding) throws in sloppy code too, so this
                        // precedes the strict-only fn-name check.
                        if !self.const_cells.is_empty() && self.const_cells.contains(&cell) {
                            return Err(Thrown(
                                "TypeError: Assignment to constant variable.".into(),
                            ));
                        }
                        // A named function expression's own-name binding is
                        // immutable: strict writer → TypeError, sloppy → no-op.
                        if !self.fn_name_cells.is_empty() && self.fn_name_cells.contains(&cell) {
                            let f = self.frames.last().unwrap().func;
                            if self.func(f as usize).is_strict {
                                return Err(Thrown(
                                    "TypeError: Assignment to constant variable.".into(),
                                ));
                            }
                            ip += 1;
                            continue;
                        }
                        // A lexical binding still in its TDZ (the cell holds the
                        // UNINITIALIZED sentinel until the declaration runs):
                        // an assignment through the closure throws — only the
                        // declaring function's own CellSet initializes it.
                        if self.heap.cell_get(cell).is_uninitialized() {
                            return Err(Thrown(
                                "ReferenceError: cannot access a lexical binding before initialization"
                                    .to_string(),
                            ));
                        }
                        let v = self.get(base, src);
                        self.heap.upval_cell_set(cell, v);
                        ip += 1;
                    }
                    Instr::NewArray {
                        dst,
                        arg_base,
                        argc,
                    } => {
                        let mut items = self.heap.take_arr_buf(argc as usize);
                        for i in 0..argc {
                            items.push(self.get(base, arg_base + i));
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(items)));
                        // ArrayCreate uses the CURRENT realm's %Array.prototype%.
                        self.realm_born(v.heap_index(), self.arr_proto);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewObject { dst, hint } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(
                            ObjMap::with_capacity(hint as usize),
                        ))));
                        // OrdinaryObjectCreate uses the CURRENT realm's %Object.prototype%.
                        self.realm_born(v.heap_index(), self.obj_proto);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewPlannedObject { dst, plan } => {
                        let plan = self
                            .func(func_id as usize)
                            .static_key_plans
                            .get(plan as usize)
                            .cloned()
                            .ok_or_else(|| {
                                Thrown("InternalError: invalid static key plan".to_string())
                            })?;
                        if !plan.runtime_valid() || plan.len() > 256 {
                            return Err(Thrown(
                                "InternalError: invalid static key plan".to_string(),
                            ));
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(
                            ObjMap::with_static_key_plan(plan),
                        ))));
                        self.realm_born(v.heap_index(), self.obj_proto);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::FinalizeObject {
                        dst,
                        plan,
                        val_base,
                        count,
                    } => {
                        let plan_idx = plan;
                        let plan = self
                            .func(func_id as usize)
                            .static_key_plans
                            .get(plan as usize)
                            .ok_or_else(|| {
                                Thrown("InternalError: invalid static key plan".to_string())
                            })?;
                        // `count` is redundant bytecode metadata; any mismatch
                        // with the authoritative plan is malformed input and
                        // fails closed before allocation.
                        if !plan.runtime_valid()
                            || plan.len() > crate::bytecode::FINALIZE_STAGE_SLOTS
                            || plan.is_empty()
                            || plan.len() != count as usize
                            || (val_base as usize)
                                .checked_add(count as usize)
                                .is_none_or(|end| end > u16::MAX as usize)
                        {
                            return Err(Thrown(
                                "InternalError: invalid static key plan".to_string(),
                            ));
                        }
                        let shape = self.finalize_shape(func_id, plan_idx, plan);
                        // B187: values staged in a stack buffer (count <= 16 by
                        // the compile gate AND the check above), the map's storage
                        // comes from the literal slab — no per-object Vec round-trip.
                        let mut buf = [Value::UNDEFINED; crate::bytecode::FINALIZE_STAGE_SLOTS];
                        for i in 0..count {
                            buf[i as usize] = self.get(base, val_base + i);
                        }
                        let v = Value::heap(self.alloc_finalized_site(
                            plan,
                            &buf[..count as usize],
                            shape,
                        ));
                        self.realm_born(v.heap_index(), self.obj_proto);
                        self.set(base, dst, v);
                        // Historical metering cost: the legacy sequence charged
                        // one step for the allocation plus one per append. The
                        // dispatch-loop tick above charged this op's single
                        // step; charge the per-field remainder now, after the
                        // completed work, exactly like the off-frame inliner.
                        #[cfg(feature = "instrument")]
                        if self.instr_rec.is_some() {
                            self.charge_steps(count as i64);
                        }
                        ip += 1;
                    }
                    Instr::ToObject { dst, src } => {
                        let v = self.get(base, src);
                        let o = self.to_object(v)?;
                        self.set(base, dst, o);
                        ip += 1;
                    }
                    Instr::CheckCoercible { src } => {
                        let v = self.get(base, src);
                        self.require_object_coercible(v)?;
                        ip += 1;
                    }
                    Instr::GetIndex { dst, obj, key } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        // Numeric TypedArray indices are integer-indexed exotic
                        // accesses, so an in/out-of-bounds result never consults
                        // a user prototype. Serve non-BigInt kinds directly;
                        // strings/coercive keys and every unsupported edge fall
                        // through before observable work.
                        if o.is_heap() {
                            let ta_idx = o.heap_index();
                            if let HeapObj::TypedArray {
                                buffer,
                                kind,
                                byte_offset,
                                length,
                            } = self.heap.get(ta_idx)
                            {
                                if !native::TA_KINDS[*kind as usize].2 {
                                    if let Some(i) = array_index(k) {
                                        let view = crate::vm::typedarray::TaInterpNumericView::from_matched_non_bigint(
                                            ta_idx,
                                            *buffer,
                                            *kind,
                                            *byte_offset,
                                            *length,
                                        );
                                        if let Some(r) = self.ta_interp_numeric_get_view(view, i) {
                                            self.set(base, dst, r);
                                            ip += 1;
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                        // Exact present dense-Array + Int leaf. This sits after
                        // the TypedArray lane so integer-indexed exotics retain
                        // their existing shortest path; every uncertain Array
                        // shape falls through to the authoritative dispatcher.
                        if let Some(r) = self.interp_dense_int_get(o, k) {
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        let r = self.get_index(o, k)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::GetIndexConcat {
                        dst,
                        obj,
                        name,
                        key,
                    } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let r = self.get_index_concat(o, name, k, func_id)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::ToPropKey { dst, obj, src } => {
                        // RequireObjectCoercible(base) precedes ToPropertyKey: a
                        // null/undefined base throws BEFORE the key's toString runs
                        // (mirrors get_index). Coercing once here makes the later
                        // GetIndex/SetIndex coercion a no-op (single key evaluation).
                        let o = self.get(base, obj);
                        if o.is_nullish() {
                            return Err(Thrown(format!(
                                "TypeError: cannot read property of {}",
                                self.display(o)
                            )));
                        }
                        let k = self.get(base, src);
                        let pk = self.coerce_index_key(k)?;
                        self.set(base, dst, pk);
                        ip += 1;
                    }
                    Instr::SetIndex { obj, key, val } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let v = self.get(base, val);
                        if o.is_heap() && v.is_number() {
                            let ta_idx = o.heap_index();
                            if let HeapObj::TypedArray {
                                buffer,
                                kind,
                                byte_offset,
                                length,
                            } = self.heap.get(ta_idx)
                            {
                                if !native::TA_KINDS[*kind as usize].2
                                    && (self.immutable_buffers.is_empty()
                                        || !self.immutable_buffers.contains(buffer))
                                {
                                    if let Some(i) = array_index(k) {
                                        // No store barrier: `v` is a number,
                                        // which neither generation's barrier
                                        // nor the oracle records (both want a
                                        // heap value), and the call was 3% of
                                        // a typed-array store loop.
                                        let view = crate::vm::typedarray::TaInterpNumericView::from_matched_non_bigint(
                                            ta_idx,
                                            *buffer,
                                            *kind,
                                            *byte_offset,
                                            *length,
                                        );
                                        if self.ta_interp_numeric_set_view(view, i, v) {
                                            ip += 1;
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                        let strict = self.func(func_id as usize).is_strict;
                        self.set_index(o, k, v, strict)?;
                        ip += 1;
                    }
                    Instr::ToConcatKey { dst, src } => {
                        // The `+`'s observable half, hoisted out of a fused
                        // `obj["k" + e] = v` so it runs BEFORE the RHS (where
                        // the `+` sits). Primitives and strings pass through —
                        // their deferred concat at the store runs no user
                        // code. A non-string heap value runs exactly what
                        // `add_values("k", v)` would: ToPrimitive(default),
                        // then the Symbol TypeError of the ensuing ToString.
                        let v = self.get(base, src);
                        let out = if !v.is_heap() || self.heap.is_str_like(v.heap_index()) {
                            v
                        } else {
                            let p = self.to_primitive_default(v)?;
                            if p.is_heap()
                                && matches!(self.heap.get(p.heap_index()), HeapObj::Symbol { .. })
                            {
                                return Err(Thrown(
                                    "TypeError: Cannot convert a Symbol value to a string".into(),
                                ));
                            }
                            p
                        };
                        self.set(base, dst, out);
                        ip += 1;
                    }
                    Instr::SetIndexConcat {
                        obj,
                        name,
                        key,
                        val,
                    } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let v = self.get(base, val);
                        let strict = self.func(func_id as usize).is_strict;
                        self.set_index_concat(o, name, k, v, strict, func_id)?;
                        ip += 1;
                    }
                    Instr::SetFnNameFromKey { func, key, prefix } => {
                        let f = self.get(base, func);
                        let k = self.get(base, key);
                        self.set_fn_name_from_key(f, k, prefix);
                        ip += 1;
                    }
                    Instr::GetProp { dst, obj, name } => {
                        let o = self.get(base, obj);
                        // The cache's way-0 own-data hit, inline and first: the
                        // steps below answer only names and receivers that hit
                        // never serves (a private name fills no way, and a
                        // string, array or typed array has no shape), and they
                        // read the key's text first. Anything else is the full
                        // `ic_get_prop` call further down.
                        #[cfg(not(feature = "safe-sandbox"))]
                        if let Some(v) = self.ic_get_prop_own_hit(func_id, ip, o) {
                            self.set(base, dst, v);
                            ip += 1;
                            continue;
                        }
                        let key = strs[name as usize].as_str();
                        // A private name the own hit did not answer (`private_get_slow`).
                        if is_private_key(key) {
                            if let Some(v) = self.private_get_slow(func_id, ip, name, o, key)? {
                                self.set(base, dst, v);
                                ip += 1;
                                continue;
                            }
                        }
                        // The cache's inherited-data hit for a receiver whose
                        // mirror holds no shape (any other was the own hit's).
                        // After the private names, which fill no way: a private
                        // read paid this probe's site and receiver fetches for
                        // nothing, a twentieth of a class's private-heavy method.
                        #[cfg(not(feature = "safe-sandbox"))]
                        if let Some(v) = self.ic_get_prop_proto_hit(func_id, ip, o, key) {
                            self.ic_proto_hit_settle(o);
                            self.set(base, dst, v);
                            ip += 1;
                            continue;
                        }
                        // `.length` of a string, an Array or a TypedArray is
                        // an internal slot, not a property the shape-keyed IC
                        // can hold, so every such read used to fall through
                        // the IC miss and the generic member walk: ~26ns on a
                        // string or Array and ~45ns on a TypedArray, against
                        // ~6ns for a cached field. It is the bound of nearly
                        // every loop, so answer it before the IC.
                        if key.len() == 6 && key == "length" && o.is_heap() {
                            if let Some(v) = self.length_fast(o) {
                                self.set(base, dst, v);
                                ip += 1;
                                continue;
                            }
                        }
                        // A receiver with no shape (an Array, a string, a
                        // DataView, a Map or Set, a promise) can never hit the
                        // shape-keyed IC below, so its prototype-intrinsic
                        // reads (`dv.getUint32`, `arr.push`) take the B191
                        // proof first instead of paying the IC's site probe and
                        // miss on every read. Any receiver the proof does not
                        // answer goes on to the IC exactly as before.
                        if o.is_heap() && !matches!(self.heap.get(o.heap_index()), HeapObj::Object(_)) {
                            // A Proxy receiver: its [[Get]] (`get` trap or
                            // target), which is all the member walk does for
                            // one once the IC misses and both prototype probes
                            // decline, each of which paid on every trap.
                            if let Some((target, handler, revoked)) = self.proxy_parts(o.heap_index()) {
                                let r = match self.proxy_get(target, handler, revoked, &key, o) {
                                    Ok(v) => v,
                                    Err(Thrown(msg)) => {
                                        let f = self.func(func_id as usize);
                                        let name: &str = if f.name.is_empty() {
                                            "<anonymous>"
                                        } else {
                                            &f.name
                                        };
                                        Err(Thrown(format!("{msg} (in {name})")))?
                                    }
                                };
                                self.set(base, dst, r);
                                ip += 1;
                                continue;
                            }
                            // A class value's static data property
                            // (`Entity.count`, `new.target.kind`), a
                            // function's own one (`counter.calls`), or a
                            // prototype intrinsic (`dv.getUint32`).
                            if let Some(v) = self.shapeless_read(o, &key) {
                                self.set(base, dst, v);
                                ip += 1;
                                continue;
                            }
                        } else if o.is_heap()
                            && matches!(self.heap.get(o.heap_index()), HeapObj::Object(m) if m.is_ctor)
                        {
                            // `Array.isArray`, `Object.keys`: a constructor's own
                            // data property. The property cache declines every
                            // constructor map, so its probe and miss came first.
                            if let Some(v) = self.ctor_own_data_read(o, &key) {
                                self.set(base, dst, v);
                                ip += 1;
                                continue;
                            }
                        }
                        // ── interpreter property IC ── own/chain data reads
                        // resolve through the per-site cache; a cached class /
                        // own / chain GETTER frame-calls on THIS dispatch loop
                        // (ret_dst = dst) instead of a nested run_loop.
                        match self.ic_get_prop(func_id, ip, o, &key) {
                            GetAct::Value(v) => {
                                self.set(base, dst, v);
                                ip += 1;
                                continue;
                            }
                            GetAct::Accessor {
                                fid,
                                closure,
                                getter,
                            } => {
                                self.setup_call(fid, closure, o, base, 0, 0, dst, ip + 1, getter)?;
                                break;
                            }
                            GetAct::None => {}
                        }
                        // (A shapeless receiver's prototype intrinsic was
                        // asked before the cache, `shapeless_read`; an object
                        // has none to answer.)
                        if o.is_heap() {
                            // `Object.hasOwn`, `Array.isArray`: a constructor's
                            // own data property, which the IC declines.
                            if let Some(v) = self.ctor_own_data_read(o, &key) {
                                self.set(base, dst, v);
                                ip += 1;
                                continue;
                            }
                        }
                        // Name the enclosing function when a property read
                        // fails. "Cannot read properties of undefined (reading
                        // 'x')" with an empty Error.stack gives an embedder no
                        // way to find the site; the function name usually does,
                        // and it is built only on the throw path.
                        let r = match self.get_prop(o, &key) {
                            Ok(v) => v,
                            Err(Thrown(msg)) => {
                                let f = self.func(func_id as usize);
                                let name: &str = if f.name.is_empty() {
                                    "<anonymous>"
                                } else {
                                    &f.name
                                };
                                Err(Thrown(format!("{msg} (in {name})")))?
                            }
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SetProp {
                        obj,
                        name,
                        val,
                        strict: site_strict,
                    } => {
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        // The cache's way-0 own-data hit, inline and first (a
                        // private name fills no way: `ic_set_prop`); anything
                        // else is the full `ic_set_prop` call below.
                        #[cfg(not(feature = "safe-sandbox"))]
                        if self.ic_set_prop_own_hit(func_id, ip, o, v) {
                            ip += 1;
                            continue;
                        }
                        let key = strs[name as usize].as_str();
                        // Private stores (field-init emission, compound and update
                        // writes) go to the side table or the accessors. One
                        // byte-compare on the hot path.
                        if matches!(key.as_bytes().first(), Some(b'#')) && self.private_set_slow(func_id, ip, name, o, key, v)? {
                            ip += 1;
                            continue;
                        }
                        // A class value's own writable static data property
                        // (`Entity.count++`), or a function's, which the
                        // cache declines. Neither is ever an ordinary
                        // object's, whose adds (`this.x = x` in a
                        // constructor) paid both lanes' key tests first.
                        if o.is_heap()
                            && !matches!(self.heap.get(o.heap_index()), HeapObj::Object(_))
                            && (self.class_static_data_write(o, &key, v) || self.fn_own_data_write(o, &key, v))
                        {
                            ip += 1;
                            continue;
                        }
                        // ── interpreter property IC ── an existing own data
                        // slot writes directly; a cached class/own SETTER
                        // frame-calls on THIS dispatch loop with the value as
                        // its argument and ret_dst = RET_DISCARD.
                        match self.ic_set_prop(func_id, ip, o, &key, v) {
                            SetAct::Done => {
                                // The hardened WASM profile has no trace rows to
                                // preserve. When a proven plain own-data store is
                                // immediately read back through the same receiver
                                // and key, the read is exactly the RHS we just
                                // committed. Forward it without another property
                                // probe or dispatch. Exotic receivers, accessors,
                                // proxies and failed/slow stores never return Done.
                                //
                                // This remains two logical bytecodes. Enter the
                                // skipped GetProp through the ordinary meter hook
                                // before publishing its destination, so a budget
                                // ending between the pair observes the completed
                                // store but not the read, exactly as before.
                                #[cfg(feature = "meter-only")]
                                if let Some(
                                    next @ Instr::GetProp {
                                        dst,
                                        obj: get_obj,
                                        name: get_name,
                                    },
                                ) = code.get(ip + 1)
                                {
                                    let same_key = name == *get_name
                                        || self.func(func_id as usize).string_constants
                                            [*get_name as usize]
                                            .as_str()
                                            == key;
                                    if *get_obj == obj && same_key {
                                        if self.instr_rec.is_some() {
                                            if let Err(msg) =
                                                self.instrument_step(base, ip + 1, next)
                                            {
                                                let top = self.frames.len() - 1;
                                                self.frames[top].ip = ip + 1;
                                                return Err(Thrown(msg.to_string()));
                                            }
                                        }
                                        self.set(base, *dst, v);
                                        ip += 2;
                                        continue;
                                    }
                                }
                                ip += 1;
                                continue;
                            }
                            SetAct::Setter {
                                fid,
                                closure,
                                setter,
                            } => {
                                self.setup_call(
                                    fid,
                                    closure,
                                    o,
                                    base,
                                    val,
                                    1,
                                    RET_DISCARD,
                                    ip + 1,
                                    setter,
                                )?;
                                break;
                            }
                            SetAct::None => {}
                        }
                        let strict = self.func(func_id as usize).is_strict || site_strict;
                        self.set_prop(o, &key, v, strict)?;
                        ip += 1;
                    }
                    Instr::SetPrivate { obj, name, val } => {
                        // PrivateSet (user `this.#x = v`): the site's way-0 field
                        // hit (`IcEntry::Private`, the only way such a site
                        // fills), else `private_set_strict`.
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        #[cfg(not(feature = "safe-sandbox"))]
                        if self.ic_set_prop_own_hit(func_id, ip, o, v) {
                            ip += 1;
                            continue;
                        }
                        let key = strs[name as usize].as_str();
                        self.private_set_strict(func_id, ip, name, o, key, v)?;
                        ip += 1;
                    }
                    Instr::SetLiteralProto { obj, val } => {
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        if o.is_heap() && (self.is_object_value(v) || v == Value::NULL) {
                            let _ = self.ordinary_set_prototype_of(o, v)?;
                        }
                        ip += 1;
                    }
                    Instr::InitDataProp { obj, name, val } => {
                        // CreateDataProperty on a fresh object-literal object: a plain
                        // own w/e/c data property, ignoring the prototype chain.
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        let key: &'p str = self
                            .func(func_id as usize)
                            .string_constants
                            .get(name as usize)
                            .map(String::as_str)
                            .ok_or_else(|| {
                                Thrown("InternalError: invalid static property name".to_string())
                            })?;
                        if o.is_heap() {
                            let oi = o.heap_index();
                            if matches!(self.heap.get(oi), HeapObj::Object(_)) {
                                // A value expression can suspend/re-enter after
                                // NewObject and let this still-rooted literal be
                                // promoted before the property is committed.
                                // Publish the exact holder -> value edge before
                                // the direct ObjMap mutation, just like set_prop.
                                self.heap.write_barrier_val(oi, v);
                                let HeapObj::Object(m) = self.heap.get_mut(oi) else {
                                    unreachable!("object kind changed across write barrier")
                                };
                                m.define(key, v, crate::heap::PropAttr::data());
                            } else {
                                let k = key.to_string();
                                self.set_prop(o, &k, v, false)?;
                            }
                        }
                        ip += 1;
                    }
                    // The compiler proved this key is new (see `AppendDataProp`):
                    // append without `define`'s existence probe.
                    Instr::AppendDataProp { obj, name, val } => {
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        let key: &'p str = self
                            .func(func_id as usize)
                            .string_constants
                            .get(name as usize)
                            .map(String::as_str)
                            .ok_or_else(|| {
                                Thrown("InternalError: invalid static property name".to_string())
                            })?;
                        if o.is_heap() {
                            let oi = o.heap_index();
                            if matches!(self.heap.get(oi), HeapObj::Object(_)) {
                                self.heap.write_barrier_val(oi, v);
                                let HeapObj::Object(m) = self.heap.get_mut(oi) else {
                                    unreachable!("object kind changed across write barrier")
                                };
                                m.push_static_data(key, v);
                            } else {
                                let k = key.to_string();
                                self.set_prop(o, &k, v, false)?;
                            }
                        }
                        ip += 1;
                    }
                    Instr::DeleteProp {
                        dst,
                        obj,
                        name,
                        strict,
                    } => {
                        let o = self.get(base, obj);
                        // `delete obj.x` does ToObject(base) â€” null/undefined throw a
                        // TypeError (RequireObjectCoercible); other primitives box and
                        // delete returns true.
                        self.require_object_coercible(o)?;
                        let key =
                            self.func(func_id as usize).string_constants[name as usize].as_str();
                        let r = self.delete_property(o, &key)?;
                        // Strict mode: a delete that evaluates to false throws.
                        if strict && r == Value::bool(false) {
                            return Err(Thrown(format!(
                                "TypeError: Cannot delete property '{key}'"
                            )));
                        }
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::DeleteIndex {
                        dst,
                        obj,
                        key,
                        strict,
                    } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let ks = self.to_property_key(k)?; // ToPropertyKey (symbol â†’ prop_key, object â†’ ToString)
                                                           // `delete obj[k]` does ToObject(base) after ToPropertyKey â€”
                                                           // null/undefined throw a TypeError (other primitives box and
                                                           // delete returns true).
                        self.require_object_coercible(o)?;
                        let r = self.delete_property(o, &ks)?;
                        if strict && r == Value::bool(false) {
                            return Err(Thrown(format!(
                                "TypeError: Cannot delete property '{ks}'"
                            )));
                        }
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::DeleteIndexConcat {
                        dst,
                        obj,
                        name,
                        key,
                        strict,
                    } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let r = self.delete_index_concat(o, name, k, strict, func_id)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }

                    Instr::TailCall {
                        callee,
                        arg_base,
                        argc,
                    } => {
                        // Proper tail call: REUSE this frame for a PLAIN
                        // function/closure callee (constant stack). Any other
                        // callee (native/bound/class/generator/async/primitive)
                        // falls through to the Call+Return emitted after.
                        let callee_v = self.get(base, callee);
                        // Args live above `base` — copy them out BEFORE the
                        // window is rebuilt in place: into a stack buffer for
                        // the usual few (a `Vec` here was an allocator round
                        // trip per tail call), a `Vec` past that.
                        const INLINE_ARGS: usize = 8;
                        let reused = if (argc as usize) <= INLINE_ARGS {
                            let mut buf = [Value::UNDEFINED; INLINE_ARGS];
                            for (i, slot) in buf[..argc as usize].iter_mut().enumerate() {
                                *slot = self.get(base, arg_base + i as u16);
                            }
                            self.try_tail_reuse(base, callee_v, Value::UNDEFINED, &buf[..argc as usize])?
                        } else {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            self.try_tail_reuse(base, callee_v, Value::UNDEFINED, &argv)?
                        };
                        if reused {
                            break; // re-snapshot the frame coordinates
                        }
                        ip += 1;
                    }
                    Instr::TailCallWithThis {
                        callee,
                        this_v,
                        arg_base,
                        argc,
                    } => {
                        // Tail-position with-resolved call: frame reuse with
                        // `this` = the with-object; non-plain callees fall
                        // through to the CallWithThis emitted after.
                        let callee_v = self.get(base, callee);
                        let this_val = self.get(base, this_v);
                        let argv: Vec<Value> =
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                        if self.try_tail_reuse(base, callee_v, this_val, &argv)? {
                            break; // re-snapshot the frame coordinates
                        }
                        ip += 1;
                    }
                    Instr::CallWithThis {
                        dst,
                        callee,
                        this_v,
                        arg_base,
                        argc,
                        name,
                    } => {
                        // A `with`-resolved identifier call: `this` = the
                        // with-object. A plain user function pushes a frame in
                        // THIS dispatch loop (deep with-call recursion must be
                        // a catchable RangeError, never native-stack overflow);
                        // everything else (natives, bound, proxies, generators)
                        // routes through call_value.
                        let callee_v = self.get(base, callee);
                        let this_val = self.get(base, this_v);
                        // The spec-order member-call lowering captures the
                        // callable before evaluating its literal arguments.
                        // Recover the exact array copy-length loop reduction at
                        // this call only when the captured Value is still the
                        // pristine slice/concat intrinsic. The recognizer also
                        // proves the complete GetProp/argument/loop window.
                        // The call's own method name rules out every other call
                        // first: the recognizer re-read this instruction and the
                        // one three back, a chain of dependent loads on every
                        // split call (6% of a `dv.getUint32(o, le)` loop). Only
                        // the reduction, an optimization, depends on the filter.
                        let copy_method = if name == crate::bytecode::NO_NAME
                            || matches!(strs[name as usize].as_str(), "slice" | "concat")
                        {
                            self.captured_array_copy_method(func_id, ip)
                        } else {
                            None
                        };
                        if let Some(method) = copy_method {
                            if let Some(exit) = self.try_array_copy_len_reduce(
                                func_id,
                                ip,
                                base,
                                this_val,
                                method,
                                Some(callee_v),
                            ) {
                                ip = exit;
                                continue;
                            }
                        }
                        let plain = if callee_v.is_heap() {
                            match self.heap.get(callee_v.heap_index()) {
                                HeapObj::Func(id) => Some((*id, NO_CLOSURE)),
                                HeapObj::Closure { func, .. } => {
                                    Some((*func, callee_v.heap_index()))
                                }
                                _ => None,
                            }
                        } else {
                            None
                        };
                        if let Some((fid, closure)) = plain {
                            let p = self.func(fid as usize);
                            if !p.is_generator && !p.is_async {
                                self.setup_call(
                                    fid,
                                    closure,
                                    this_val,
                                    base,
                                    arg_base,
                                    argc,
                                    dst,
                                    ip + 1,
                                    callee_v,
                                )?;
                                break;
                            }
                        } else if callee_v.is_heap()
                            && matches!(self.heap.get(callee_v.heap_index()),
                                HeapObj::Native(id) if *id == crate::vm::native::FN_CALL)
                        {
                            // `f.call(t, ...)` captured in spec order: the
                            // callee IS %Function.prototype.call% and the
                            // receiver a plain function, so the call is `f`
                            // with `this` = `t`. It enters as a frame of this
                            // loop, like `f(...)`, rather than through
                            // `call_value` and the nested run loop the native
                            // starts for it (deep `.call` recursion was
                            // native-stack depth, not frames).
                            let (t, at, n) = if argc == 0 {
                                (Value::UNDEFINED, arg_base, 0)
                            } else {
                                (self.get(base, arg_base), arg_base + 1, argc - 1)
                            };
                            if let Some((fid, closure)) = self.plain_fn_entry(this_val) {
                                self.setup_call(fid, closure, t, base, at, n, dst, ip + 1, this_val)?;
                                break;
                            }
                            // A native receiver, as `call_value` would run it
                            // with no other realm in existence.
                            if self.realm_global_objs.is_empty() && this_val.is_heap() {
                                if let HeapObj::Native(id) = self.heap.get(this_val.heap_index()) {
                                    let id = *id;
                                    let r = self.with_argv(base, at, n, |vm, argv| vm.call_native(id, t, argv))?;
                                    self.set(base, dst, r);
                                    ip += 1;
                                    continue;
                                }
                            }
                        } else if callee_v.is_heap()
                            && matches!(self.heap.get(callee_v.heap_index()),
                                HeapObj::Native(id) if *id == crate::vm::native::FN_APPLY)
                            && super::builtins::apply_fast_enabled()
                        {
                            // `f.apply(t, list)` captured in spec order (`f` a
                            // closure variable, so its Get came first): the
                            // callee IS %Function.prototype.apply%, so on a
                            // plain function of this realm whose list reads
                            // without running code (`interp_apply_list`) the
                            // call is `f`'s frame here, as in `CallMethod`.
                            if let Some((fid, closure)) = self.plain_fn_entry(this_val) {
                                if self.realm_global_objs.is_empty() || self.get_function_realm(this_val) == 0 {
                                    let t = if argc == 0 { Value::UNDEFINED } else { self.get(base, arg_base) };
                                    let list = if argc < 2 { Value::UNDEFINED } else { self.get(base, arg_base + 1) };
                                    if let Some(args) = self.interp_apply_list(list) {
                                        self.setup_call_slice(fid, closure, t, &args, dst, ip + 1, this_val)?;
                                        break;
                                    }
                                }
                            }
                        }
                        // A captured Map or Set method on a receiver of its
                        // kind (`set.has(keys[i])`): the native IS the callee,
                        // so the call is the native run with that `this`, as
                        // `call_value` runs one with no other realm in
                        // existence. The captured lane below proved it the live
                        // prototype intrinsic and then dispatched by name,
                        // which proved it again: three proofs per call with the
                        // read's.
                        if callee_v.is_heap() && this_val.is_heap() && self.realm_global_objs.is_empty() {
                            if let HeapObj::Native(id) = self.heap.get(callee_v.heap_index()) {
                                let id = *id;
                                let kind_ok = match crate::vm::native::proto_method(id) {
                                    Some((_, 3, _)) => matches!(self.heap.get(this_val.heap_index()), HeapObj::Set(_)),
                                    Some((_, 4, _)) => matches!(self.heap.get(this_val.heap_index()), HeapObj::Map { .. }),
                                    _ => false,
                                };
                                if kind_ok {
                                    let r = self.with_argv(base, arg_base, argc, |vm, argv| vm.call_native(id, this_val, argv))?;
                                    self.set(base, dst, r);
                                    ip += 1;
                                    continue;
                                }
                            }
                        }
                        // B289: a captured boot intrinsic (`arr.push(i % 13)`,
                        // `s.charCodeAt(starts[i])` — a member call whose
                        // argument was not order-transparent, so the strict
                        // default lowering captured the callee first) takes the
                        // same builtin lanes as the fused `CallMethod` once the
                        // captured Value is proven identical to the live
                        // prototype intrinsic; any other captured Value falls
                        // through and is invoked exactly as captured.
                        if name != crate::bytecode::NO_NAME {
                            if let Some(result) = self.captured_intrinsic_lane(
                                func_id, base, this_val, callee_v, name, arg_base, argc,
                            )? {
                                self.set(base, dst, result);
                                ip += 1;
                                continue;
                            }
                        }
                        // A plain native callee (`Object.hasOwn(o, k)`, captured
                        // in spec order) runs as `call_value` would run it with
                        // no other realm and no realm function in existence:
                        // every earlier arm of its cascade is for another kind
                        // of callee.
                        if self.realm_global_objs.is_empty() && self.realm_fns.is_empty() && callee_v.is_heap() {
                            if let HeapObj::Native(id) = self.heap.get(callee_v.heap_index()) {
                                let id = *id;
                                let r = self.with_argv(base, arg_base, argc, |vm, argv| {
                                    vm.call_native(id, this_val, argv)
                                })?;
                                self.set(base, dst, r);
                                ip += 1;
                                continue;
                            }
                        }
                        // `with_argv`: a stack buffer for the common arity. A
                        // split member call with a native callee (`arr.push(x)`
                        // whose argument was not order-transparent) lands here
                        // once per call; a heap `Vec` per call was a measurable
                        // share of the interpreter's split-form tax.
                        let r = self.with_argv(base, arg_base, argc, |vm, argv| {
                            vm.call_value(callee_v, this_val, argv)
                        })?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Call {
                        dst,
                        callee,
                        arg_base,
                        argc,
                    } => {
                        let callee_v = self.get(base, callee);
                        // ── interpreter call IC ── a repeat callee (identity +
                        // heap-version guarded) resolved to a plain user
                        // function skips the Proxy/native/bound/ctor probes
                        // and the generator/async flag loads.
                        if let Some(call) = self.ic_call(func_id, ip, callee_v) {
                            #[cfg(all(feature = "meter-only", not(feature = "jit")))]
                            if argc == 1 && call.fib_global != crate::vm::ic::IC_NO_FIB_GLOBAL {
                                let arg = self.get(base, arg_base);
                                if let Some(result) =
                                    self.try_metered_numeric_fib(call.fib_global, callee_v, arg)
                                {
                                    self.set(base, dst, result);
                                    ip += 1;
                                    continue;
                                }
                            }
                            // The entry carries the callee's frame facts: the
                            // push reads nothing of its `FuncProto`.
                            #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
                            self.setup_call_lean_desc(
                                call.fid,
                                call.closure,
                                call.desc,
                                base,
                                arg_base,
                                argc,
                                dst,
                                ip + 1,
                                callee_v,
                            )?;
                            #[cfg(any(feature = "meter-only", feature = "safe-sandbox"))]
                            self.setup_call_lean(
                                call.fid,
                                call.closure,
                                Value::UNDEFINED,
                                base,
                                arg_base,
                                argc,
                                dst,
                                ip + 1,
                                callee_v,
                            )?;
                            break;
                        }
                        // A callable Proxy: route through call_value (apply trap).
                        if callee_v.is_heap()
                            && matches!(self.heap.get(callee_v.heap_index()), HeapObj::Proxy { .. })
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(callee_v, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        // A native resolve/reject function (from `new Promise`).
                        if callee_v.is_heap() {
                            if let HeapObj::BoundResolver {
                                promise,
                                is_reject,
                                pair,
                            } = self.heap.get(callee_v.heap_index())
                            {
                                let (p, isr, pid) = (*promise, *is_reject, *pair);
                                let arg = if argc >= 1 {
                                    self.get(base, arg_base)
                                } else {
                                    Value::UNDEFINED
                                };
                                // [[AlreadyResolved]]: only the pair's FIRST call acts.
                                if self.resolver_pair_fire(pid) {
                                    if isr {
                                        self.reject(p, arg);
                                    } else {
                                        self.resolve(p, arg);
                                    }
                                }
                                self.set(base, dst, Value::UNDEFINED);
                                ip += 1;
                                continue;
                            }
                            // A loose for-of step through a bound array-iterator
                            // `next` reuses its result object (`bound_step_call`).
                            if argc == 0 {
                                if let Some(r) = self.bound_step_call(func_id, ip, base, dst, callee_v) {
                                    let r = r?;
                                    self.set(base, dst, r);
                                    ip += 1;
                                    continue;
                                }
                            }
                            // A bound or native function: run via call_value (fixes
                            // `this`/prepends bound args, or dispatches the builtin).
                            // A CombinatorResolver (a Promise.all/race/â€¦ resolve/reject
                            // element) is callable too: a userland thenable invokes it
                            // directly as `onFulfilled(v)` â€” route it through call_value
                            // so it performs its combinator step instead of throwing
                            // "not a function". %Function.prototype% is also a callable.
                            if matches!(
                                self.heap.get(callee_v.heap_index()),
                                HeapObj::Bound { .. }
                                    | HeapObj::Wrapped { .. }
                                    | HeapObj::Native(_)
                                    | HeapObj::NativeClosure { .. }
                                    | HeapObj::CombinatorResolver { .. }
                            ) || (self.fn_proto != 0 && callee_v.heap_index() == self.fn_proto)
                            {
                                let argv: Vec<Value> =
                                    (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                                let r = self.call_value(callee_v, Value::UNDEFINED, &argv)?;
                                self.set(base, dst, r);
                                ip += 1;
                                continue;
                            }
                        }
                        // A built-in constructor object invoked as a function
                        // (e.g. an Intl service ctor without `new`), or an
                        // [[IsHTMLDDA]] exotic called directly (â†’ undefined).
                        if callee_v.is_heap()
                            && (matches!(self.heap.get(callee_v.heap_index()), HeapObj::Object(m) if m.is_ctor)
                                || self.is_htmldda_index(callee_v.heap_index()))
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(callee_v, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        // Name the CALLING function when a plain `f()` callee is
                        // not callable. `Error.prototype.stack` is empty, so
                        // without this a host embedder gets "null is not a
                        // function" with nothing to locate it by. The enclosing
                        // name is cheap and is only ever built on the throw path.
                        let (fid, closure) = match self.resolve_callable_realm(callee_v) {
                            Ok(v) => v,
                            Err(Thrown(msg)) => {
                                let f = self.func(func_id as usize);
                                let name: &str = if f.name.is_empty() {
                                    "<anonymous>"
                                } else {
                                    &f.name
                                };
                                // Minified code names everything `t`/`e`, so the
                                // enclosing function's string literals are what
                                // actually identify it. Opt-in: they are long.
                                let strings = if std::env::var_os("ZIPP_TRACE_CALLS").is_some() {
                                    let sample: Vec<&str> = f
                                        .string_constants
                                        .iter()
                                        .map(|s| s.as_str())
                                        .filter(|s| s.len() > 3)
                                        .take(8)
                                        .collect();
                                    format!(" strings={sample:?}")
                                } else {
                                    String::new()
                                };
                                Err(Thrown(format!("{msg} (in {name}){strings}")))?
                            }
                        };
                        // An `async function*` returns an AsyncGenerator (checked
                        // before the plain-generator/async cases since it is both).
                        if self.func(fid as usize).is_generator && self.func(fid as usize).is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            self.pending_gen_callee = callee_v;
                            let ag =
                                self.alloc_async_generator(fid, closure, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        // A generator function returns a Generator object, unrun.
                        if self.func(fid as usize).is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            self.pending_gen_callee = callee_v;
                            let g = self.alloc_generator(fid, closure, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        // An async function runs to its first `await` then returns
                        // its result Promise.
                        if self.func(fid as usize).is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            self.pending_gen_callee = callee_v;
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
                            callee_v,
                        )?;
                        break;
                    }

                    Instr::CallMethod {
                        dst,
                        obj,
                        name,
                        arg_base,
                        argc,
                    } => {
                        let recv = self.get(base, obj);
                        // `program` outlives the VM, so borrow the method name
                        // with the program's lifetime (NOT self's) â€” avoids
                        // cloning the name string on every method call (a heap
                        // alloc per `a.push(i)` / `a.map(cb)` etc.).
                        // `func()` returns `&'p`, so the interned name key outlives
                        // any `&mut self` below â€” and resolves eval functions too.
                        let key: &'p str = &strs[name as usize];
                        // A method of an ordinary object or class instance: the
                        // method cache first. None of the receiver-kind leaves
                        // below can serve such a receiver (they want a string,
                        // a DataView, a generator, an array, a Map or a
                        // function), and each paid a key compare or a heap read
                        // before the cache on every `o.m()`.
                        let object_recv = recv.is_heap()
                            && !is_private_key(key)
                            && matches!(self.heap.get(recv.heap_index()), HeapObj::Object(_));
                        if object_recv {
                            if let Some((fid, closure, callee)) = self.ic_call_method(func_id, ip, recv, key) {
                                self.setup_call_lean_this(
                                    fid, closure, recv, true, base, arg_base, argc, dst, ip + 1, callee,
                                )?;
                                break;
                            }
                            // An own native method (`Reflect.get(t, k)`).
                            if let Some(id) = self.ic_call_native(func_id, ip, recv, key) {
                                let r = self.with_argv(base, arg_base, argc, |vm, argv| vm.call_native(id, recv, argv))?;
                                self.set(base, dst, r);
                                ip += 1;
                                continue;
                            }
                        }
                        // `charCodeAt` dominates the parser/tokenizer-shaped
                        // interpreter workload. Its exact flat-string + Int
                        // leaf bypasses the method IC, argument stack buffer,
                        // generic receiver dispatcher, no-op flatten and
                        // ToInteger conversion. Every guard is read-only and a
                        // miss resumes this historical path unchanged.
                        if argc == 1 && key == "charCodeAt" {
                            let arg = self.get(base, arg_base);
                            if let Some(result) = self.interp_char_code_at_fast(recv, arg) {
                                self.set(base, dst, result);
                                ip += 1;
                                continue;
                            }
                        }
                        // Any other method of a primitive string: the builtin
                        // dispatcher's string arm, reached before the probes
                        // below, none of which a string receiver can match.
                        if recv.is_heap()
                            && matches!(self.heap.get(recv.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. })
                        {
                            if let Some(result) =
                                self.interp_string_method_fast(recv.heap_index(), key, base, arg_base, argc)?
                            {
                                self.set(base, dst, result);
                                ip += 1;
                                continue;
                            }
                        }
                        // Hot fast path: `arr.push(x)` — the most common
                        // per-element array idiom — and `arr.pop()`, its
                        // stack partner. Append or remove directly, skipping
                        // the probes below (none serves an array) and the
                        // try_builtin_method → dispatch_builtin_method →
                        // array_method layering, then answer the new length
                        // or the element. A FROZEN array (or one whose `length`
                        // was made non-writable) must throw, so the lanes
                        // decline it to array_method's guard. The side tables
                        // are empty for an ordinary array.
                        if recv.is_heap() && matches!(self.heap.get(recv.heap_index()), HeapObj::Array(_)) {
                            if argc == 1 && super::builtins::is_push_name(key) {
                                let v = self.get(base, arg_base);
                                if let Some(len) = self.interp_array_push_fast(recv, v) {
                                    self.set(base, dst, len);
                                    ip += 1;
                                    continue;
                                }
                            } else if argc == 0 && super::builtins::is_pop_name(key) {
                                if let Some(v) = self.interp_array_pop_fast(recv) {
                                    self.set(base, dst, v);
                                    ip += 1;
                                    continue;
                                }
                            }
                        }
                        // Pristine non-BigInt DataView getters are a sufficiently
                        // hot interpreter primitive to bypass the generic
                        // receiver-kind dispatcher and argument stack buffer.
                        // The leaf repeats every live method/prototype/own-shadow
                        // guard and declines before observable work on any miss.
                        if recv.is_heap()
                            && matches!(self.heap.get(recv.heap_index()), HeapObj::DataView { .. })
                        {
                            let position = if argc == 0 {
                                Value::UNDEFINED
                            } else {
                                self.get(base, arg_base)
                            };
                            let little_endian = if argc < 2 {
                                Value::UNDEFINED
                            } else {
                                self.get(base, arg_base + 1)
                            };
                            if let Some(result) = self.dataview_interp_get_fast(
                                recv.heap_index(),
                                key,
                                position,
                                little_endian,
                            )? {
                                self.set(base, dst, result);
                                ip += 1;
                                continue;
                            }
                            // The setters, likewise (`set*(pos, value, le)`).
                            if argc >= 2 && key.as_bytes().first() == Some(&b's') {
                                let little_endian = if argc < 3 {
                                    Value::UNDEFINED
                                } else {
                                    self.get(base, arg_base + 2)
                                };
                                let value = self.get(base, arg_base + 1);
                                if self
                                    .dataview_interp_set_fast(
                                        recv.heap_index(),
                                        key,
                                        position,
                                        value,
                                        little_endian,
                                    )?
                                    .is_some()
                                {
                                    self.set(base, dst, Value::UNDEFINED);
                                    ip += 1;
                                    continue;
                                }
                            }
                            // The BigInt accessors (`getBigInt64(pos, le)`,
                            // `setBigUint64(pos, v, le)`, ...), likewise.
                            if key.len() > 6 && key.as_bytes()[3..6] == *b"Big" {
                                let a1 = if argc < 2 {
                                    Value::UNDEFINED
                                } else {
                                    self.get(base, arg_base + 1)
                                };
                                let a2 = if argc < 3 {
                                    Value::UNDEFINED
                                } else {
                                    self.get(base, arg_base + 2)
                                };
                                if let Some(result) =
                                    self.dataview_interp_big_fast(recv.heap_index(), key, position, a1, a2)?
                                {
                                    self.set(base, dst, result);
                                    ip += 1;
                                    continue;
                                }
                            }
                        }
                        // `g.next(v)` on a generator whose `next` is the
                        // intrinsic: its resume, as the native would run it.
                        // A generator has no shape for the property cache, so
                        // the member walk to %GeneratorPrototype% and the
                        // native dispatcher were most of a step's cost. Main
                        // realm only: a child realm's `next` would make its
                        // result object in that realm.
                        if key == "next"
                            && self.realm_global_objs.is_empty()
                            && recv.is_heap()
                            && matches!(self.heap.get(recv.heap_index()), HeapObj::Generator { .. })
                            && self.gen_method_pristine(recv.heap_index(), "next")
                        {
                            let sent = if argc == 0 {
                                Value::UNDEFINED
                            } else {
                                self.get(base, arg_base)
                            };
                            super::builtins::builtin_stats_count(self, recv, "next");
                            // Run the step as a frame of this loop; its yield or
                            // return completes this call with its result object.
                            if self.gen_enter_inline(
                                recv.heap_index(),
                                ip,
                                dst,
                                super::async_runtime::GEN_STEP_OBJECT,
                                sent,
                                false,
                            ) {
                                break;
                            }
                            if let Some(result) = self.generator_method(recv.heap_index(), "next", &[sent])? {
                                self.set(base, dst, result);
                                ip += 1;
                                continue;
                            }
                        }
                        // An exact allocation-only loop of
                        // `sum += array.slice(...).length` or
                        // `sum += array.concat([literal...]).length` can be
                        // collapsed after proving the live method/species/
                        // spreadability protocol is pristine. A failed proof is
                        // read-only and falls through to the unchanged call path.
                        if matches!(key, "slice" | "concat") {
                            if let Some(exit) =
                                self.try_array_copy_len_reduce(func_id, ip, base, recv, key, None)
                            {
                                ip = exit;
                                continue;
                            }
                        }
                        // PrivateMethodCall brand check (`obj.#m()`): the receiver must
                        // carry the declaring class's brand, else a TypeError. Gated on
                        // is_private_key so ordinary calls (push/map/…) only pay a
                        // leading-'#' test; textual fallback when no brand resolvable.
                        let mut private_callee: Option<Value> = None;
                        if is_private_key(key) {
                            let ctx = self.frames.last().map_or(Value::UNDEFINED, |f| f.callee);
                            let site = super::values::private_site(func_id as u32, name as u32);
                            private_callee = self.private_method_callee(ctx, recv, key, Some(site))?;
                        }
                        // `map.get(k)` / `map.set(k, v)` / `map.has(k)` on a
                        // Map receiver: the receiver-kind dispatcher's Map
                        // arm, reached directly. The method IC never holds a
                        // Map receiver (its methods are natives), so every
                        // such call otherwise pays a miss probe, the argument
                        // stack buffer and the generic receiver walk before
                        // the same `map_method` arm. The leaf runs the SAME
                        // intrinsic proof that arm runs (`collection_method_is_
                        // intrinsic`: pristine %Map.prototype% slot, no own
                        // shadow) and declines to the unchanged path otherwise.
                        // The Python frontend's runtime keeps globals,
                        // attribute caches and instance dicts in Maps.
                        if private_callee.is_none() && (argc == 1 || argc == 2) {
                            if let Some(result) =
                                self.interp_map_method_fast(recv, key, base, arg_base, argc)?
                            {
                                self.set(base, dst, result);
                                ip += 1;
                                continue;
                            }
                        }
                        // `f.call(t, ...args)` on a plain function while a Get
                        // of `call` finds the pristine %Function.prototype.call%:
                        // that is the call of `f` with `this` = `t`, so `f`
                        // enters as a frame of this loop, as `f(...)` would,
                        // rather than through the builtin dispatcher's
                        // `call_value` and the nested run loop it starts.
                        // A native (`hasOwn.call(o, k)`) runs as the same call
                        // `call_value` would make of it.
                        if private_callee.is_none() && key == "call" {
                            let (this_v, args_at, n) = if argc == 0 {
                                (Value::UNDEFINED, arg_base, 0)
                            } else {
                                (self.get(base, arg_base), arg_base + 1, argc - 1)
                            };
                            if let Some((fid, closure)) = self.fn_call_plain_target(recv) {
                                self.setup_call(fid, closure, this_v, base, args_at, n, dst, ip + 1, recv)?;
                                break;
                            }
                            if let Some(id) = self.fn_call_native_target(recv) {
                                if let Some(it) = self.interp_array_values_call(id, this_v) {
                                    self.set(base, dst, it);
                                    ip += 1;
                                    continue;
                                }
                                let r = self.with_argv(base, args_at, n, |vm, argv| vm.call_native(id, this_v, argv))?;
                                self.set(base, dst, r);
                                ip += 1;
                                continue;
                            }
                        }
                        // `f.bind(t, ...args)` while a Get of `bind` finds the
                        // pristine %Function.prototype.bind% (`it.next.bind(it)`).
                        if private_callee.is_none() && key == "bind" {
                            if let Some(b) = self.interp_fn_bind_fast(recv, base, arg_base, argc) {
                                self.set(base, dst, b);
                                ip += 1;
                                continue;
                            }
                        }
                        // `f.apply(t, list)` on a plain function while a Get of
                        // `apply` finds the pristine %Function.prototype.apply%
                        // and the list reads without running code (`arguments`
                        // forwarding, a dense array): `f`'s frame in this loop,
                        // as `f.call(t, ...)` enters, instead of the native
                        // dispatcher's list walk and a nested run loop.
                        if private_callee.is_none() && key == "apply" {
                            if let Some((fid, closure)) = self.fn_apply_plain_target(recv) {
                                let this_v = if argc == 0 { Value::UNDEFINED } else { self.get(base, arg_base) };
                                let list = if argc < 2 { Value::UNDEFINED } else { self.get(base, arg_base + 1) };
                                if let Some(args) = self.interp_apply_list(list) {
                                    self.setup_call_slice(fid, closure, this_v, &args, dst, ip + 1, recv)?;
                                    break;
                                }
                            }
                        }
                        // ── interpreter method-call IC ── a monomorphic /
                        // low-polymorphic `obj.method()` resolves through the
                        // per-site cache (validated; see vm/ic.rs), skipping
                        // the builtin probe, the class/proto chain walk, and
                        // resolve_callable — straight to the frame push.
                        if private_callee.is_none() && !object_recv {
                            if let Some((fid, closure, callee)) =
                                self.ic_call_method(func_id, ip, recv, key)
                            {
                                self.setup_call_lean(
                                    fid,
                                    closure,
                                    recv,
                                    base,
                                    arg_base,
                                    argc,
                                    dst,
                                    ip + 1,
                                    callee,
                                )?;
                                break;
                            }
                        }
                        // A class value's static method or a function's own
                        // method property (`Probe.make(s)`, `util.fmt(x)`):
                        // the plain function that data property holds, entered
                        // as a frame of this loop with the receiver as `this`,
                        // as the general Get-then-call would call it.
                        if private_callee.is_none() && recv.is_heap() {
                            if let Some(f) = self
                                .class_static_data_read(recv, key)
                                .or_else(|| self.fn_own_data_read(recv, key))
                            {
                                if let Some((fid, closure)) = self.plain_fn_entry(f) {
                                    self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1, f)?;
                                    break;
                                }
                            }
                        }
                        // Builtin methods (array/string) execute inline and
                        // produce a result without pushing a frame.
                        if let Some(result) =
                            self.try_builtin_method(recv, key, base, arg_base, argc)?
                        {
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        // Otherwise the property must resolve to a function; call it
                        // with `this = recv`.
                        let prop = match private_callee {
                            Some(f) => f,
                            // Name the enclosing function: `x.foo()` where `x`
                            // is undefined is the most common runtime failure in
                            // page code, and the bare message says nothing about
                            // where it happened.
                            None => match self.get_prop(recv, key) {
                                Ok(v) => v,
                                Err(Thrown(msg)) => {
                                    let f = self.func(func_id as usize);
                                    let name: &str = if f.name.is_empty() {
                                        "<anonymous>"
                                    } else {
                                        &f.name
                                    };
                                    Err(Thrown(format!("{msg} (in {name})")))?
                                }
                            },
                        };
                        // A built-in constructor object used as a method value
                        // (e.g. `Intl.NumberFormat(...)` without `new`): route via
                        // call_value, which handles construct-without-new / throws.
                        if prop.is_heap()
                            && matches!(self.heap.get(prop.heap_index()), HeapObj::Object(m) if m.is_ctor)
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(prop, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        // A built-in constructor object used as a computed method
                        // value (e.g. `Intl["NumberFormat"](...)` without `new`).
                        if prop.is_heap()
                            && matches!(self.heap.get(prop.heap_index()), HeapObj::Object(m) if m.is_ctor)
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(prop, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        // A native, bound or PROXY method value (e.g. inherited from a
                        // prototype) is invoked via call_value with this = recv.
                        // `resolve_callable` below only knows Func/Closure, so a
                        // Proxy over a function reached it as "is not a function".
                        if prop.is_heap()
                            && (matches!(
                                self.heap.get(prop.heap_index()),
                                HeapObj::Native(_)
                                    | HeapObj::NativeClosure { .. }
                                    | HeapObj::Bound { .. }
                                    | HeapObj::BoundResolver { .. }
                                    | HeapObj::Proxy { .. }
                            ) || (self.fn_proto != 0 && prop.heap_index() == self.fn_proto))
                        {
                            let r = self.with_argv(base, arg_base, argc, |vm, argv| {
                                vm.call_value(prop, recv, argv)
                            })?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        let (fid, closure) = self.resolve_callable_named(prop, key)?;
                        // An `async function*` method returns an AsyncGenerator.
                        if self.func(fid as usize).is_generator && self.func(fid as usize).is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            self.pending_gen_callee = prop;
                            let ag = self.alloc_async_generator(fid, closure, recv, &argv)?;
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        // A generator method returns a Generator object, unrun.
                        if self.func(fid as usize).is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            self.pending_gen_callee = prop;
                            let g = self.alloc_generator(fid, closure, recv, &argv)?;
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        // An async method runs to its first `await` then returns
                        // its result Promise.
                        if self.func(fid as usize).is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            self.pending_gen_callee = prop;
                            let p = self.alloc_async(fid, closure, recv, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
                        // Pass the resolved method VALUE as the callee (so LoadCallee and
                        // object-method `super` â€” which reads [[HomeObject]] keyed by the
                        // executing function value â€” find it, even for a no-upvalue method).
                        self.setup_call(
                            fid,
                            closure,
                            recv,
                            base,
                            arg_base,
                            argc,
                            dst,
                            ip + 1,
                            prop,
                        )?;
                        break;
                    }

                    Instr::Throw { src } => {
                        let v = self.get(base, src);
                        // The message is only read when the throw leaves this
                        // run loop uncaught; a handler in a frame it unwinds
                        // through catches it first, so it is not built then.
                        let msg = if self.handler_within(stop_depth) {
                            String::new()
                        } else {
                            self.throw_message(v)
                        };
                        // Persist ip so the (unused) frame state is coherent,
                        // then signal unwinding via pending_throw + Err.
                        let top = self.frames.len() - 1;
                        self.frames[top].ip = ip;
                        self.pending_throw = Some(v);
                        return Err(Thrown(msg));
                    }
                    Instr::PushHandler {
                        catch_target,
                        catch_reg,
                    } => {
                        let top = self.frames.len() - 1;
                        let hs = &mut self.frames[top].handlers;
                        // A recycled buffer for the frame's first push.
                        if hs.capacity() == 0 {
                            *hs = super::handler_pool::take(&mut self.handler_pool);
                        }
                        hs.push(Handler::Catch {
                            target: catch_target,
                            reg: catch_reg,
                        });
                        ip += 1;
                    }
                    Instr::PopHandler => {
                        let top = self.frames.len() - 1;
                        self.frames[top].handlers.pop();
                        ip += 1;
                    }
                    Instr::PushFinally {
                        target,
                        kind_reg,
                        val_reg,
                    } => {
                        let top = self.frames.len() - 1;
                        let hs = &mut self.frames[top].handlers;
                        if hs.capacity() == 0 {
                            *hs = super::handler_pool::take(&mut self.handler_pool);
                        }
                        hs.push(Handler::Finally {
                            target,
                            kind_reg,
                            val_reg,
                        });
                        ip += 1;
                    }
                    Instr::PopFinally => {
                        let top = self.frames.len() - 1;
                        self.frames[top].handlers.pop();
                        ip += 1;
                    }
                    Instr::EndFinally { kind_reg, val_reg } => {
                        // Resume the completion deposited when this finally was
                        // entered. The low 2 bits are the kind: 1 = return (re-leave
                        // through any outer finally, else return), 2 = throw
                        // (re-raise), 3 = break/continue jump (resume the unwind
                        // toward the jump target, `floor` in the upper bits), else
                        // 0 = normal.
                        let raw = self.regs[base + kind_reg as usize].as_int();
                        match raw & 3 {
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
                            3 => {
                                let jump_target =
                                    self.regs[base + val_reg as usize].as_int() as u32;
                                let floor = (raw >> 2) as usize;
                                let from = ip;
                                match self.route_jump_through_finally(jump_target, floor) {
                                    Some(target) => ip = target as usize,
                                    None => ip = jump_target as usize,
                                }
                                // A `continue` resumed through a finally re-enters
                                // its loop without the loop's `Jump` back-edge.
                                if ip < from {
                                    self.maybe_gc();
                                }
                            }
                            _ => {
                                ip += 1;
                            }
                        }
                    }
                    Instr::JumpFinally { target, floor } => {
                        // A `break`/`continue` exiting one or more `try` blocks:
                        // run each intervening `finally` first, popping any
                        // intervening `catch`, then land at `target`.
                        let from = ip;
                        match self.route_jump_through_finally(target, floor as usize) {
                            Some(t) => ip = t as usize,
                            None => ip = target as usize,
                        }
                        // `continue` out of a try lands back in the loop without
                        // executing its `Jump` back-edge: poll like one.
                        if ip < from {
                            self.maybe_gc();
                        }
                    }
                    Instr::TemplateGetCached { dst, site } => {
                        let v = self
                            .template_cache
                            .get(&(func_id, site))
                            .copied()
                            .unwrap_or(Value::UNDEFINED);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::TemplateSetCached { site, src } => {
                        let v = self.get(base, src);
                        self.template_cache.insert((func_id, site), v);
                        ip += 1;
                    }
                    Instr::GetIterator { dst, src } => {
                        let s = self.get(base, src);
                        let it = self.get_iterator(s)?;
                        self.set(base, dst, it);
                        ip += 1;
                    }
                    Instr::GetIteratorObj { dst, src } => {
                        let s = self.get(base, src);
                        let it = self.get_iterator_direct(s)?;
                        self.set(base, dst, it);
                        ip += 1;
                    }
                    Instr::IterToArray { dst, src, count } => {
                        let s = self.get(base, src);
                        // A LIVE-mapped arguments source: refresh its dense
                        // slots first (the array fast path reads them directly).
                        if s.is_heap() {
                            self.args_sync_dense(s.heap_index());
                        }
                        let a = self.iter_to_array(s, count)?;
                        self.set(base, dst, a);
                        ip += 1;
                    }
                    Instr::Return { src } => {
                        let v = self.regs[base + src as usize];
                        // A handler-free return cannot have a pending `finally`.
                        // Avoid the out-of-line handler walk on this overwhelmingly
                        // common path; a non-empty stack keeps the exact historical
                        // route, including discarding catch-only handlers.
                        if !self.frames[frame_idx].handlers.is_empty() {
                            if let Some(target) = self.route_through_finally(1, v) {
                                ip = target as usize;
                                continue;
                            }
                        }
                        if self.pop_frame_fast(v, stop_depth) {
                            return Ok(v);
                        }
                        break;
                    }
                    Instr::ReturnUndefined => {
                        if !self.frames[frame_idx].handlers.is_empty() {
                            if let Some(target) = self.route_through_finally(1, Value::UNDEFINED) {
                                ip = target as usize;
                                continue;
                            }
                        }
                        if self.pop_frame_fast(Value::UNDEFINED, stop_depth) {
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
                        // A step `gen_enter_inline` runs in this loop: park, and
                        // continue the consumer.
                        if self.frames[frame_idx].gen != u32::MAX {
                            self.gen_yield_inline(v, ip);
                            break;
                        }
                        // Capture this frame's `try` handlers so a SYNC generator can
                        // park them and `gen.throw(e)`/`gen.return(v)` resume into the
                        // body's try/catch/finally. (drive_async_gen ignores this.)
                        let f = self.frames.pop().unwrap();
                        self.pending_yield_eval_scope = f.eval_scope;
                        self.pending_yield_handlers = f.handlers;
                        self.pending_yield = Some((v, ip));
                        return Ok(v);
                    }
                    Instr::RequireObject { val } => {
                        let v = self.get(base, val);
                        if !self.is_object_value(v) {
                            return Err(Thrown(
                                "TypeError: iterator result is not an object".into(),
                            ));
                        }
                        ip += 1;
                    }
                    Instr::GenStart => {
                        // Body-entry marker reached during the eager call-time run of
                        // a generator's parameter prologue. Suspend exactly like a
                        // valueless yield: pop the frame, leave the window live for
                        // `alloc_generator` to park, and record this ip so the first
                        // `.next()` resumes just past it (the resume path delivers no
                        // sent value here because this is not a `Yield`).
                        let f = self.frames.pop().unwrap();
                        self.pending_yield_eval_scope = f.eval_scope;
                        self.pending_yield_handlers = f.handlers; // empty at GenStart
                        self.pending_yield = Some((Value::UNDEFINED, ip));
                        return Ok(Value::UNDEFINED);
                    }
                    Instr::Await { dst, val } => {
                        // Suspend the async activation: pop the frame ENTRY but
                        // leave its register window live at the top of `self.regs`
                        // for `drive_async` to park into the heap AsyncState. Unlike
                        // a generator yield, we CAPTURE the frame's `try` handlers
                        // (carried in `pending_await`) so they can be restored on
                        // resume â€” letting `try { await p } catch (e)` see a
                        // rejection thrown back in at the await point. The async
                        // frame is always the top (and the run_loop stop frame) at
                        // an await, so popping returns to `drive_async`.
                        let v = self.get(base, val);
                        // Await's PromiseResolve(%Promise%, v): a NATIVE promise
                        // performs the OBSERVABLE Get(v, "constructor") HERE —
                        // ordinary dispatch context, where a getter (or its
                        // throw, via `?` while the frame and its handlers are
                        // still live) behaves like any user call at the await
                        // site. Pass-through only when it IS the realm
                        // %Promise%; otherwise a fresh promise ADOPTS v (the
                        // spec's NewPromiseCapability route, same extra jobs).
                        let v = if v.is_heap()
                            && matches!(self.heap.get(v.heap_index()), HeapObj::Promise { .. })
                        {
                            // PLAIN promise (intrinsic proto, no own
                            // `constructor`, pristine prototype `constructor`):
                            // the Get is unobservable and yields the realm
                            // %Promise% — pass through with no Get at all.
                            if self.promise_plain_unobservable(v.heap_index()) {
                                v
                            } else {
                                let c = self.get_prop(v, "constructor")?;
                                let intrinsic =
                                    self.global_by_name("Promise").unwrap_or(Value::UNDEFINED);
                                if c == intrinsic {
                                    v
                                } else {
                                    let p = self.alloc_promise();
                                    self.resolve_fresh_promise(p, v);
                                    Value::heap(p)
                                }
                            }
                        } else {
                            v
                        };
                        // An already-settled await in an activation resumed by
                        // its own job, with nothing else queued: the resume job
                        // would run next, so continue here (async_runtime.rs,
                        // `await_continue_in_place`).
                        if let Some(r) = self.await_continue_in_place(frame_idx, v) {
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        let f = self.frames.pop().unwrap();
                        self.pending_await = Some((v, ip, f.handlers));
                        return Ok(v);
                    }
                    Instr::IterClose { iter } => {
                        let it = self.get(base, iter);
                        self.iterator_close(it)?;
                        ip += 1;
                    }
                    Instr::IterCloseQuiet { iter } => {
                        // Error-context close: a throwing/non-object return() is
                        // ignored so the original abrupt completion is preserved.
                        let it = self.get(base, iter);
                        let _ = self.iterator_close(it);
                        ip += 1;
                    }
                    Instr::IterCloseFinally { iter, kind_reg } => {
                        // The for-of close handler; see the opcode's doc comment for
                        // why each completion kind closes (or does not).
                        let it = self.get(base, iter);
                        match self.regs[base + kind_reg as usize].as_int() & 3 {
                            1 => self.iterator_close(it)?,
                            2 => {
                                let _ = self.iterator_close(it);
                            }
                            _ => {}
                        }
                        ip += 1;
                    }
                    Instr::IterPrime { dst, iter } => {
                        // Cache a USER-OBJECT iterator's `next` once for the
                        // loop (the iterator record's [[NextMethod]]); built-in
                        // fast iterables perform no observable get.
                        let it = self.get(base, iter);
                        // A generator's is read only when it is not the
                        // intrinsic (a replaced `next`, which the loop then
                        // calls); left undefined, `IterNext` steps the body.
                        let nv = if it.is_heap()
                            && (matches!(
                                self.heap.get(it.heap_index()),
                                HeapObj::Object(_)
                                    | HeapObj::Proxy { .. }
                                    | HeapObj::Iterator { .. }
                                    | HeapObj::IterHelper { .. }
                                    | HeapObj::Intl { .. }
                            ) || (matches!(self.heap.get(it.heap_index()), HeapObj::Generator { .. })
                                && !self.gen_method_pristine(it.heap_index(), "next")))
                        {
                            self.get_prop(it, "next")?
                        } else {
                            Value::UNDEFINED
                        };
                        self.set(base, dst, nv);
                        ip += 1;
                    }
                    Instr::IterNext {
                        value_dst,
                        done_dst,
                        iter,
                        idx,
                        next,
                    } => {
                        let it = self.get(base, iter);
                        if !it.is_heap() {
                            return Err(Thrown(format!(
                                "TypeError: {} is not iterable",
                                self.display(it)
                            )));
                        }
                        // ── fast path: a plain dense Array ──
                        // By far the most common `for-of` subject, and the generic
                        // cascade below costs it SIX separate `heap.get` probes (the
                        // generator test, the iterator-object test, the tombstone
                        // scan, `flatten`, the string-step test, the length test)
                        // and then a full `get_index`. One probe and a direct
                        // element read instead: `for (v of a)` measured 113ms
                        // against 16ms for the equivalent counted loop over the
                        // same array, where node makes the two forms equal.
                        //
                        // Falls through to the generic path for anything the direct
                        // read cannot answer identically: a HOLE (its value comes
                        // from the prototype chain), a sparse array with a virtual
                        // length, or an array carrying side-table properties.
                        if self.array_js_len.is_empty()
                            || !self.array_js_len.contains_key(&it.heap_index())
                        {
                            if !self.array_elements_overlaid(it.heap_index()) {
                                let cur = array_index(self.get(base, idx)).unwrap_or(0);
                                let hit = match self.heap.get(it.heap_index()) {
                                    HeapObj::Array(items) => match items.get(cur) {
                                        Some(v) if !v.is_hole() => Some(Some(*v)),
                                        Some(_) => None,    // hole → generic path
                                        None => Some(None), // exhausted
                                    },
                                    _ => None,
                                };
                                match hit {
                                    Some(Some(v)) => {
                                        self.set(base, value_dst, v);
                                        self.set(base, done_dst, Value::bool(false));
                                        self.set(base, idx, Value::int((cur + 1) as i32));
                                        ip += 1;
                                        continue;
                                    }
                                    Some(None) => {
                                        self.set(base, done_dst, Value::bool(true));
                                        ip += 1;
                                        continue;
                                    }
                                    None => {}
                                }
                            }
                        }
                        // A generator is driven by `.next()`; the cursor is unused.
                        // The step is taken without its `{value, done}` result
                        // object, which nothing could see: built, read back and
                        // dropped, it was a third of a generator-driven loop.
                        // A `yield*` step still hands over the inner result
                        // object itself, which is read below as before.
                        // A primed loop decided at `IterPrime` (its record's
                        // [[NextMethod]] is read once: undefined there means the
                        // intrinsic); an unprimed step asks now.
                        if matches!(self.heap.get(it.heap_index()), HeapObj::Generator { .. })
                            && if next != u16::MAX {
                                self.get(base, next).is_undefined()
                            } else {
                                self.gen_method_pristine(it.heap_index(), "next")
                            }
                        {
                            // Run the step as a frame of this loop; its yield or
                            // return completes this instruction.
                            if self.gen_enter_inline(it.heap_index(), ip, value_dst, done_dst, Value::UNDEFINED, false) {
                                break;
                            }
                            let res = match self.generator_next_step(it.heap_index())? {
                                Some(super::async_runtime::GenStep::Yield(v)) => {
                                    self.set(base, value_dst, v);
                                    self.set(base, done_dst, Value::FALSE);
                                    ip += 1;
                                    continue;
                                }
                                Some(super::async_runtime::GenStep::Done(v)) => {
                                    self.set(base, value_dst, v);
                                    self.set(base, done_dst, Value::TRUE);
                                    ip += 1;
                                    continue;
                                }
                                Some(super::async_runtime::GenStep::Raw(r)) => r,
                                None => Value::UNDEFINED,
                            };
                            // `iter_result` builds `{value, done}` in that order
                            // and nothing else, so when the result is exactly
                            // that shape its slots can be read directly instead
                            // of through two full `get_prop`s. Verified against
                            // the map rather than assumed, because `yield*`
                            // returns the INNER result verbatim and that may be
                            // any user object.
                            if let Some((val, done)) = self.iter_result_unwrap(res) {
                                self.set(base, value_dst, val);
                                self.set(base, done_dst, done);
                                ip += 1;
                                continue;
                            }
                            let done = self.get_prop(res, "done")?;
                            let val = self.get_prop(res, "value")?;
                            self.set(base, value_dst, val);
                            self.set(base, done_dst, done);
                            ip += 1;
                            continue;
                        }
                        // A user iterator object (`@@iterator` already resolved by
                        // GetIterator): pull the next result via `.next()`. Lazy â€”
                        // a `break` simply stops calling it. The cached prologue
                        // [[NextMethod]] (IterPrime) is used when present; the
                        // uncached (destructuring) sites re-fetch per step.
                        // `HeapObj::Intl` is in the list for %SegmentIterator%:
                        // it is an ordinary object with a `next` method, and
                        // without it `for (v of segmenter.segment(s))` fell
                        // through to the positional walk below and reported the
                        // iterator itself as "not iterable".
                        // A generator lands here when its `next` was replaced
                        // (the arm above runs it directly only while it is the
                        // intrinsic).
                        if matches!(
                            self.heap.get(it.heap_index()),
                            HeapObj::Object(_)
                                | HeapObj::Proxy { .. }
                                | HeapObj::Iterator { .. }
                                | HeapObj::IterHelper { .. }
                                | HeapObj::Intl { .. }
                                | HeapObj::Generator { .. }
                        ) {
                            let next = if next != u16::MAX {
                                self.get(base, next)
                            } else {
                                self.get_prop(it, "next")?
                            };
                            if self.is_callable(next) {
                                // A %RegExpStringIterator% stepped by the PRISTINE
                                // intrinsic `next`: run the step inline. The
                                // {value, done} result object the intrinsic would
                                // build is engine-internal and its `done`/`value`
                                // Gets are unobservable — skip both.
                                if next.is_heap()
                                    && matches!(self.heap.get(next.heap_index()),
                                                HeapObj::Native(n) if *n == crate::vm::native::ITER_NEXT)
                                {
                                    if let Some(step) =
                                        self.regexp_string_iter_step(it.heap_index())
                                    {
                                        let (val, done) = step?;
                                        self.set(base, value_dst, val);
                                        self.set(base, done_dst, Value::bool(done));
                                        ip += 1;
                                        continue;
                                    }
                                    // Same argument, same intrinsic `next`: a
                                    // live %ArrayIteratorPrototype% iterator
                                    // (`a.values()`, `a.keys()`, `a.entries()`,
                                    // and every iterator-helper chain over an
                                    // array) is stepped inline, and the
                                    // {value, done} object it would have built
                                    // — 90ns of a 143ns step — is never made.
                                    if let Some(step) = self.array_iter_step(it.heap_index()) {
                                        let (val, done) = step?;
                                        self.set(base, value_dst, val);
                                        self.set(base, done_dst, Value::bool(done));
                                        ip += 1;
                                        continue;
                                    }
                                    // Map/Set (`map.keys()`, `set.values()`,
                                    // `map.entries()`) and snapshot iterators.
                                    // `None` leaves the TypedArray-backed case,
                                    // whose per-step bounds check can throw, on
                                    // the general path below.
                                    if let Some((val, done)) =
                                        self.collection_iter_step(it.heap_index())?
                                    {
                                        self.set(base, value_dst, val);
                                        self.set(base, done_dst, Value::bool(done));
                                        ip += 1;
                                        continue;
                                    }
                                }
                                let res = self.call_value(next, it, &[])?;
                                // IteratorNext step 3: a non-Object result is a TypeError.
                                if !self.is_object_value(res) {
                                    return Err(Thrown(
                                        "TypeError: iterator result is not an object".into(),
                                    ));
                                }
                                // IteratorStep reads only `done`; IteratorValue (the
                                // `value` Get — observable via a getter) happens ONLY
                                // when the iterator is not done.
                                let done = self.get_prop(res, "done")?;
                                let val = if self.truthy(done) {
                                    Value::UNDEFINED
                                } else {
                                    self.get_prop(res, "value")?
                                };
                                self.set(base, value_dst, val);
                                self.set(base, done_dst, done);
                                ip += 1;
                                continue;
                            }
                        }
                        // Array/Set element, string char, or Map [k,v] at the cursor.
                        let mut cursor = array_index(self.get(base, idx)).unwrap_or(0);
                        // A Set's / Map's tombstoned (deleted) slots are skipped.
                        while match self.heap.get(it.heap_index()) {
                            HeapObj::Set(items) => cursor < items.len() && items[cursor].is_hole(),
                            HeapObj::Map { keys, .. } => {
                                cursor < keys.len() && keys[cursor].is_hole()
                            }
                            _ => false,
                        } {
                            cursor += 1;
                        }
                        // A STRING iterates by CODE POINT (one step per scalar; an
                        // astral char is ONE step) while the cursor advances in
                        // UTF-16 units (matching `.length`). Flatten a rope first
                        // (no-op tag check for everything else).
                        self.heap.flatten(it.heap_index());
                        let str_step = match self.heap.get(it.heap_index()) {
                            HeapObj::Str(s) => Some(s.cp_step(cursor)),
                            _ => None,
                        };
                        if let Some(step) = str_step {
                            match step {
                                Some((cp, next)) => {
                                    // `cp` may be a lone surrogate — a real 1-unit
                                    // surrogate string, not U+FFFD.
                                    let val = self.str_from_cp(cp);
                                    self.set(base, value_dst, val);
                                    self.set(base, done_dst, Value::bool(false));
                                    self.set(base, idx, Value::int(next as i32));
                                }
                                None => self.set(base, done_dst, Value::bool(true)),
                            }
                            ip += 1;
                            continue;
                        }
                        // `coll`: a Map/Set step reads its entry positionally
                        // (`collection_entry_at`); everything else is an ordinary index.
                        let (len, coll) = match self.heap.get(it.heap_index()) {
                            // The JS length: past the dense store for a
                            // virtual array (the fast path above declined it).
                            HeapObj::Array(_) => (self.js_array_len(it.heap_index()), false),
                            HeapObj::Set(items) => (items.len(), true),
                            HeapObj::Str(s) => (s.units(), false),
                            HeapObj::Cons { len, .. } => (*len, false),
                            HeapObj::Map { keys, .. } => (keys.len(), true),
                            // The LIVE length each step: a tracking view follows its
                            // resizable buffer (shrink ends early, grow yields more);
                            // a detached/out-of-bounds view mid-iteration throws.
                            HeapObj::TypedArray { .. } => {
                                match self.ta_effective_len(it.heap_index()) {
                                    Some(n) => (n, false),
                                    None => {
                                        return Err(Thrown(
                                            "TypeError: TypedArray iterator: the viewed buffer is detached or out of bounds".into(),
                                        ))
                                    }
                                }
                            }
                            _ => {
                                return Err(Thrown(format!(
                                    "TypeError: {} is not iterable",
                                    self.display(it)
                                )));
                            }
                        };
                        if cursor < len {
                            let val = if coll {
                                self.collection_entry_at(it, cursor)
                            } else {
                                self.get_index(it, Value::int(cursor as i32))?
                            };
                            self.set(base, value_dst, val);
                            self.set(base, done_dst, Value::bool(false));
                            self.set(base, idx, len_value(cursor + 1));
                        } else {
                            self.set(base, done_dst, Value::bool(true));
                        }
                        ip += 1;
                    }
                    // Instructions no measured workload executes run in `dispatch_cold`,
                    // out of line, so this loop stays small (see `vm/dispatch_cold.rs`).
                    _ => match self.dispatch_cold(
                        instr, ip, frame_idx, func_id, base, cur_closure, running, code, strs, stop_depth,
                    )? {
                        super::dispatch_cold::ColdFlow::Next(next) => ip = next,
                        super::dispatch_cold::ColdFlow::Frame => break,
                        super::dispatch_cold::ColdFlow::Done(v) => return Ok(v),
                    },
                }
            }
        }
    }

    /// `FuseStrSwitch` at `ip`: the `n` string-literal cases from `ip` in one
    /// lookup of the discriminant's text, leaving `ldst` and `dst` as the
    /// cases would (the matched or else the last case's literal, and whether
    /// one matched). Only a string can be `===` a string literal, so any
    /// other value matches none. Answers the next ip.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    pub(crate) fn str_switch(
        &mut self,
        running: &'p crate::bytecode::FuncProto,
        func_id: u32,
        base: usize,
        ip: usize,
        ldst: u16,
        a: u16,
        dst: u16,
        n: u16,
    ) -> usize {
        let v = self.get(base, a);
        let table = running.fused.str_switch(running, ip).and_then(|sw| {
            let found = if v.is_heap() {
                let idx = v.heap_index();
                self.heap.flatten(idx);
                match self.heap.get(idx) {
                    HeapObj::Str(s) => sw.find(s.as_bytes()),
                    _ => Ok(None),
                }
            } else {
                Ok(None)
            };
            found.ok().map(|k| (sw, k))
        });
        let Some((sw, found)) = table else {
            // No table: the first case alone, as `FuseConstEqJump` runs it.
            let (Instr::LoadConst { idx, .. }, Instr::JumpIfTrue { target, .. }) =
                (running.code[ip], running.code[ip + 2])
            else {
                unreachable!("FuseStrSwitch stands for a string case");
            };
            let c = self.resolve_const_slot(func_id, idx);
            self.set(base, ldst, c);
            let r = self.strict_eq(base, a, ldst);
            self.set(base, dst, Value::bool(r));
            return if r { target as usize } else { ip + 3 };
        };
        let case = found.unwrap_or(n as usize - 1);
        let (idx, target) = sw.cases[case];
        let c = self.resolve_const_slot(func_id, idx);
        self.set(base, ldst, c);
        self.set(base, dst, Value::bool(found.is_some()));
        if found.is_some() {
            target as usize
        } else {
            sw.after as usize
        }
    }

    /// Proper-tail-call frame reuse: when `callee_v` is a PLAIN (non-generator,
    /// non-async) function or closure, rebuild the CURRENT frame's register
    /// window in place for it (constant stack for tail recursion) and return
    /// `true` — the dispatch arm must then `break` to re-snapshot the frame
    /// coordinates. Any other callee returns `false` (the caller falls through
    /// to its ordinary call path). `argv` must already be copied OUT of the
    /// register window (it is truncated here). Shared by `TailCall` and the
    /// rebound-`eval` fall-through of a tail-position `DirectEval`.
    pub(crate) fn try_tail_reuse(
        &mut self,
        base: usize,
        callee_v: Value,
        this_arg: Value,
        argv: &[Value],
    ) -> Result<bool, Thrown> {
        let plain = if callee_v.is_heap() {
            match self.heap.get(callee_v.heap_index()) {
                HeapObj::Func(id) => Some((*id, NO_CLOSURE)),
                HeapObj::Closure { func, .. } => Some((*func, callee_v.heap_index())),
                _ => None,
            }
        } else {
            None
        };
        let Some((fid, closure)) = plain else {
            return Ok(false);
        };
        // A constructing frame's `this` is the instance its pop returns.
        if self.frames.last().is_some_and(|f| f.construct_this) {
            return Ok(false);
        }
        // Tail reuse never grows `frames`, so runaway tail recursion (strict
        // `return f()` forever) is invisible to MAX_FRAMES — bound the streak
        // of pop-free reuses instead so it throws the SAME catchable
        // RangeError node does on this shape, rather than hanging.
        self.tail_reuse_streak += 1;
        if self.tail_reuse_streak > MAX_TAIL_REUSE_STREAK {
            self.tail_reuse_streak = 0;
            return Err(Thrown(
                "RangeError: Maximum call stack size exceeded".into(),
            ));
        }
        let (callee_regs, callee_params, rest_reg, arguments_reg, p_strict, p_lex, simple) = {
            let p = self.func(fid as usize);
            if p.is_generator || p.is_async {
                return Ok(false);
            }
            (
                (p.reg_count as usize).max(1),
                p.param_count as usize,
                p.rest_reg,
                p.arguments_reg,
                p.is_strict,
                p.lexical_this,
                p.simple_params,
            )
        };
        // OrdinaryCallBindThis on `this_arg` (undefined for a plain tail call,
        // the with-object for a with-resolved one): arrow lexical this /
        // sloppy global substitution / sloppy primitive boxing.
        let this_val = if p_lex && closure != NO_CLOSURE {
            self.rebind_arrow_this(fid, closure, this_arg)
        } else if !p_strict && this_arg.is_nullish() && self.global_this != 0 {
            // The CALLEE realm's global (createRealm child fns).
            Value::heap(self.callee_this_global(callee_v))
        } else if !p_strict && !self.is_object_value(this_arg) && self.global_this != 0 {
            let b = self.to_object(this_arg)?;
            self.realm_retag_boxed(callee_v, b);
            b
        } else {
            this_arg
        };
        self.regs.truncate(base);
        if self.regs_would_overflow(base + callee_regs) {
            return Err(Thrown(
                "RangeError: Maximum call stack size exceeded".into(),
            ));
        }
        self.regs.resize(base + callee_regs, Value::UNDEFINED);
        self.regs[base] = this_val;
        let n = argv.len().min(callee_params);
        for (i, &a) in argv.iter().take(n).enumerate() {
            self.regs[base + 1 + i] = a;
        }
        if let Some(rreg) = rest_reg {
            let extra: Vec<Value> = argv.get(callee_params..).unwrap_or(&[]).to_vec();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[base + rreg as usize] = arr;
        }
        let mut args_obj = u32::MAX;
        if let Some(areg) = arguments_reg {
            let mapinfo =
                (!p_strict && simple).then(|| (self.frames.len() - 1, base, callee_params));
            let arr = self.build_arguments_object(argv.to_vec(), callee_v, p_strict, mapinfo);
            self.regs[base + areg as usize] = arr;
            if mapinfo.is_some() {
                args_obj = arr.heap_index();
            }
        }
        let fr = self.frames.last_mut().unwrap();
        fr.func = fid;
        fr.closure = closure;
        fr.ip = 0;
        fr.handlers.clear();
        fr.callee = callee_v;
        fr.new_target = Value::UNDEFINED;
        fr.eval_scope = u32::MAX;
        fr.super_done = false;
        fr.args_obj = args_obj;
        // `argv` was copied OUT before the window was truncated, so this
        // activation has no caller argument window to replay — the legacy
        // `f.arguments` accessor falls back to the bound formals.
        fr.arg_win = u32::MAX;
        fr.argc = 0;
        fr.is_eval = false;
        Ok(true)
    }

    /// Bundle the win64 heap-helper addresses the JIT codegen needs (region path
    /// and Tier C whole-function path). All entries are fixed `extern "win64" fn`
    /// addresses — independent of `self` — so this is a pure constructor.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_math_imul_guard(&self) -> Option<crate::codegen::MathIntrinsicGuard> {
        // The generated Math.imul path can replace its per-call Rust identity
        // helper with local comparisons. In addition to the captured reference
        // checks used by the ordinary emitted MathOp, typed lanes may replace the
        // source GetProp itself.  They therefore also carry the receiver's exact
        // data slot and generation, and re-read the live slot before doing any
        // arithmetic.  An accessor, deletion, relocation or in-place overwrite
        // all fail closed to the unchanged bytecode/call path.
        if std::env::var_os("ZIPP_NO_DIRECT_MATH_GUARD").is_some() {
            return None;
        }
        self.builtin_globals
            .get("Math")
            .copied()
            .and_then(|receiver_idx| {
                let receiver = Value::heap(receiver_idx);
                let (callee, receiver_vals, receiver_slot) = match self.heap.get(receiver_idx) {
                    HeapObj::Object(map) => {
                        let slot = map.pos("imul")?;
                        if map.attr_at(slot).accessor {
                            return None;
                        }
                        (map.val_at(slot), map.vals_ptr() as u64, slot as u32)
                    }
                    _ => return None,
                };
                let native_id = native::MATH_METHOD_BASE + crate::bytecode::MathFn::Imul as u16;
                if !self.static_builtin_is_intrinsic("Math", native_id, callee, receiver) {
                    return None;
                }
                // Bake every other Math.* intrinsic the same way, so the
                // emitted per-op guard is three compares rather than a helper
                // call (`emit_math_identity_guard`). An op whose property is
                // missing, an accessor, replaced or foreign-realm stays `0`
                // and keeps the helper-call guard.
                use crate::bytecode::MATH_FN_COUNT;
                debug_assert_eq!(native::MATH_METHODS.len(), MATH_FN_COUNT);
                let mut op_callee_bits = [0u64; MATH_FN_COUNT];
                let mut op_callee_ver = [0u32; MATH_FN_COUNT];
                let mut op_slot = [u32::MAX; MATH_FN_COUNT];
                if let HeapObj::Object(map) = self.heap.get(receiver_idx) {
                    for (i, &(name, _, _)) in native::MATH_METHODS.iter().enumerate() {
                        let Some(slot) = map.pos(name) else { continue };
                        if map.attr_at(slot).accessor {
                            continue;
                        }
                        let v = map.val_at(slot);
                        let id = native::MATH_METHOD_BASE + i as u16;
                        if self.static_builtin_is_intrinsic("Math", id, v, receiver) {
                            op_callee_bits[i] = v.bits();
                            op_callee_ver[i] = self.heap.version_of(v.heap_index());
                            op_slot[i] = slot as u32;
                        }
                    }
                }
                Some(crate::codegen::MathIntrinsicGuard {
                    receiver_bits: receiver.bits(),
                    receiver_ver: self.heap.version_of(receiver_idx),
                    receiver_vals,
                    receiver_slot,
                    callee_bits: callee.bits(),
                    callee_ver: self.heap.version_of(callee.heap_index()),
                    op_callee_bits,
                    op_callee_ver,
                    op_slot,
                })
            })
    }

    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_heap_helper_addrs(&self) -> crate::codegen::HeapHelperAddrs {
        let math_imul_guard = self.jit_math_imul_guard();
        crate::codegen::HeapHelperAddrs {
            get_prop_miss: jit_get_prop_miss as *const () as usize,
            set_prop_miss: jit_set_prop_miss as *const () as usize,
            versions_base: jit_heap_versions_base as *const () as usize,
            ic_base: jit_ic_base as *const () as usize,
            get_index: jit_get_index as *const () as usize,
            set_index: jit_set_index as *const () as usize,
            array_push: jit_array_push as *const () as usize,
            char_code_at: jit_char_code_at as *const () as usize,
            concat: jit_concat as *const () as usize,
            str_append: jit_str_append as *const () as usize,
            str_append_index: crate::vm::coerce::jit_str_append_index_ascii as *const () as usize,
            call_method_ic: jit_call_method_ic as *const () as usize,
            call_method_computed_dense: crate::vm::helpers_misc::jit_call_method_computed_dense
                as *const () as usize,
            regexp_call_direct: crate::vm::helpers_misc::jit_regexp_call_direct as *const () as usize,
            string_regexp_call_direct: crate::vm::helpers_misc::jit_string_regexp_call_direct
                as *const () as usize,
            has_own_call: crate::vm::helpers_misc::jit_has_own_call as *const () as usize,
            call_ic: jit_call_ic as *const () as usize,
            cross_call: crate::vm::helpers_misc::jit_cross_call as *const () as usize,
            cross_call_same_proto2: crate::vm::helpers_misc::jit_cross_call_same_proto2 as *const () as usize,
            quick_len: crate::vm::helpers_misc::jit_quick_len as *const () as usize,
            cross3_enter: crate::vm::helpers_misc::jit_cross3_enter as *const () as usize,
            window_close: crate::vm::helpers_misc::jit_window_close as *const () as usize,
            cross3_unroot: crate::vm::helpers_misc::jit_cross3_unroot as *const () as usize,
            cross3_finish: crate::vm::helpers_misc::jit_cross3_finish as *const () as usize,
            // B191's baseline tables hold only heap Values, so a real entry is
            // never 0; an absent one (pre-boot, a stripped prototype) is.
            push_intrinsic_bits: self.arr_proto_baseline.get("push").copied().unwrap_or(0),
            coll_boot_bits: self.coll_boot_bits(),
            coll_lookup_direct: crate::vm::helpers_misc::jit_coll_lookup_direct as *const () as usize,
            coll_mutate_direct: crate::vm::helpers_misc::jit_coll_mutate_direct as *const () as usize,
            char_code_at_intrinsic_bits: self
                .str_proto_baseline
                .get("charCodeAt")
                .copied()
                .unwrap_or(0),
            get_prop_slow: jit_get_prop_slow as *const () as usize,
            set_prop_slow: jit_set_prop_slow as *const () as usize,
            get_prop_acc: crate::vm::helpers_misc::jit_get_prop_acc as *const () as usize,
            set_prop_acc: crate::vm::helpers_misc::jit_set_prop_acc as *const () as usize,
            strict_eq: jit_strict_eq as *const () as usize,
            truthy: jit_truthy as *const () as usize,
            ta_snapshot: jit_ta_snapshot as *const () as usize,
            ta_clamp_store: jit_ta_clamp_store as *const () as usize,
            dv_get: jit_dv_get as *const () as usize,
            math_unary: jit_math_unary as *const () as usize,
            math_two: jit_math_two as *const () as usize,
            math_imul_guard,
            inline_store_lane_ok: self.heap.inline_store_lane_ok(),
            cell_get: jit_cell_get as *const () as usize,
            str_index_of: crate::vm::helpers_misc::jit_str_index_of as *const () as usize,
            str_substring: crate::vm::helpers_misc::jit_str_substring as *const () as usize,
            coll_lookup: crate::vm::helpers_misc::jit_coll_lookup as *const () as usize,
            coll_mutate: crate::vm::helpers_misc::jit_coll_mutate as *const () as usize,
            str_upper_case: crate::vm::helpers_misc::jit_str_upper_case as *const () as usize,
            cell_set: jit_cell_set as *const () as usize,
            upval_set: jit_upval_set as *const () as usize,
            tierc_upval_get: crate::vm::helpers_misc::jit_tierc_upval_get as *const () as usize,
            tierc_upval_set: crate::vm::helpers_misc::jit_tierc_upval_set as *const () as usize,
            tierc_upval_inc_i32: crate::vm::helpers_misc::jit_tierc_upval_inc_i32 as *const () as usize,
            get_index_concat: jit_get_index_concat as *const () as usize,
            upval_get: jit_upval_get as *const () as usize,
            forin_live: jit_forin_live as *const () as usize,
            iter_next: crate::vm::helpers_misc::jit_iter_next as *const () as usize,
            regexp_scalar_get_iterator: crate::vm::helpers_misc::jit_regexp_scalar_get_iterator
                as *const () as usize,
            regexp_scalar_iter_prime: crate::vm::helpers_misc::jit_regexp_scalar_iter_prime
                as *const () as usize,
            regexp_scalar_step: crate::vm::helpers_misc::jit_regexp_scalar_step as *const () as usize,
            regexp_scalar_capture_num: crate::vm::helpers_misc::jit_regexp_scalar_capture_num
                as *const () as usize,
            regexp_scalar_flush: crate::vm::helpers_misc::jit_regexp_scalar_flush as *const () as usize,
            regexp_scalar_exec: crate::vm::helpers_misc::jit_regexp_scalar_exec as *const () as usize,
            regexp_scalar_exec_flush: crate::vm::helpers_misc::jit_regexp_scalar_exec_flush
                as *const () as usize,
            push_finally: crate::vm::helpers_misc::jit_push_finally as *const () as usize,
            pop_finally: crate::vm::helpers_misc::jit_pop_finally as *const () as usize,
            to_num: crate::vm::helpers_misc::jit_to_num as *const () as usize,
            has_property: jit_has_property as *const () as usize,
            regs_fits: jit_regs_fits as *const () as usize,
            typeof_str: jit_typeof as *const () as usize,
            typeof_is: crate::vm::helpers_misc::jit_typeof_is as *const () as usize,
            typeof_same: crate::vm::helpers_misc::jit_typeof_same as *const () as usize,
            static_fn: crate::vm::helpers_misc::jit_static_fn as *const () as usize,
            to_concat_key: crate::vm::helpers_misc::jit_to_concat_key as *const () as usize,
            set_index_concat: crate::vm::helpers_misc::jit_set_index_concat as *const () as usize,
            set_index_concat_pure: crate::vm::helpers_misc::jit_set_index_concat_pure as *const () as usize,
            delete_index: crate::vm::helpers_misc::jit_delete_index as *const () as usize,
            delete_index_concat: crate::vm::helpers_misc::jit_delete_index_concat as *const () as usize,
            is_array: jit_is_array as *const () as usize,
            len_of: jit_len_of as *const () as usize,
            forin_keys: jit_forin_keys as *const () as usize,
        }
    }

    /// Run the guarded ARM64 whole-function baseline over this frame window.
    #[cfg(all(feature = "jit", target_arch = "aarch64"))]
    pub(crate) fn try_run_jit(&mut self, func_id: u32, base: usize) -> Option<(Value, u32)> {
        let jitfn = self.jit.get(func_id)? as *const crate::codegen::JitFn;
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // SAFETY: the accepted ARM64 subset makes no calls or allocations, so
        // the register Vec cannot move while its raw window is live.
        let (bits, bail) = unsafe { (*jitfn).run(regs_ptr, vm_ptr) };
        self.jit.note_bail(func_id, bail);
        Some((Value::from_bits(bits), bail))
    }

    /// If `func_id` has compiled native code, run it over the register window
    /// at `base` and return `(result_bits_as_Value, bail_ip)`. `None` if there
    /// is no compiled code for this function.
    ///
    /// The native code reads/writes `self.regs[base..]` directly via a raw
    /// pointer taken here and used ONLY for the duration of the call â€” nothing
    /// in between can resize `self.regs` (the JIT subset issues no calls/allocs).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn try_run_jit(&mut self, func_id: u32, base: usize) -> Option<(Value, u32)> {
        // ── global-route revalidation ── the compile-time `globals_ok` scan below
        // proved every global this body touches directly was a live, plain, writable
        // slot. A `delete` can take a slot back to UNINITIALIZED and a
        // `defineProperty(globalThis, …)` can put a real descriptor behind one, after
        // which the emitted `mov rax, [r12 + idx*8]` is wrong (it hands back the
        // sentinel instead of throwing, or bypasses a setter). Nothing re-checked.
        //
        // Zero-cost while no program has done either: `global_route_epoch` is 0 and
        // this is one compare. Falling back interprets THIS call and leaves the
        // compiled code installed, so a re-created binding heals on its own.
        if self.global_route_epoch != 0 && !self.jit_globals_still_routable(func_id) {
            return None;
        }
        let jitfn = self.jit.get(func_id)? as *const crate::codegen::JitFn;
        // ── self-binding revalidation ── a Tier A body containing a self-call emits a
        // direct `call` to its own entry in place of `LoadGlobal(self_slot) + Call`,
        // with NO callee guard. Rebind the name and the interpreter's inner calls
        // re-resolve to the new function while native code keeps calling itself.
        //
        // Checked here rather than inline at each recursive call because declining the
        // OUTER entry is enough: the whole activation then interprets, and the
        // interpreter re-resolves every inner call the way the spec says. The common
        // case — a name nobody rebinds — is one load and one compare per outer call,
        // not per recursion, so `fib`'s hot path is untouched.
        //
        // SAFETY: `jitfn` points into `self.jit.compiled`, which this function already
        // relies on being stable for the duration (see the note below).
        if let Some((slot, expect)) = unsafe { (*jitfn).self_binding() } {
            if self.globals.get(slot as usize).map(|v| v.bits()) != Some(expect) {
                return None;
            }
        }
        // An ordinary frame call can compile its CALLER before this callee's
        // IC becomes monomorphic, so it reaches Tier C through `try_run_jit`
        // rather than the native cross-call helper. Consume the same exact,
        // unmetered Markdown plan here after the shared global-route checks.
        // A failed runtime guard is pure and falls through to native ip 0.
        // The frame is active here, so `current_realm_id` names the renderer's
        // realm. The reducer currently proves main-realm String/helper images;
        // child-realm bodies execute the ordinary Tier-C path.
        if self.current_realm_id().is_none() {
            if let Some((_, _, _, Some(plan))) = self.jit.cross_entry(func_id) {
                let input = self.get(base, 1);
                let reduced = {
                    let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Jit);
                    self.markdown_inline_reduce(plan, input)
                };
                if let Some(bits) = reduced {
                    return Some((Value::from_bits(bits), crate::codegen::NO_BAIL));
                }
            }
        }
        // SAFETY: `jitfn` points into self.jit.compiled (stable for the call).
        // `regs_ptr` is valid for the frame's reg_count slots. A self-call op
        // routes through `jit_self_call` (passed the `vm` pointer below) which
        // may resize self.regs for the recursive frame â€” but it RESTORES regs to
        // this length before returning, and the native code re-reads its window
        // base from the callee-saved register only relative to `regs_ptr`, which
        // stays valid because jit_self_call uses a SEPARATE save/restore of the
        // regs Vec around the recursion (see its safety note).
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // Tier C may access captures even when entered through a real frame.
        // Install every activation's exact roots so the same native body also
        // works for frame-free cross-calls.
        // A full root stack declines before metering/native effects and leaves
        // this real Frame to execute in the interpreter.
        let (active_closure, active_callee) = self
            .frames
            .last()
            .filter(|f| f.func == func_id && f.base == base)
            .map(|f| {
                (
                    f.closure,
                    if f.callee.is_heap() {
                        f.callee.heap_index()
                    } else {
                        NO_CLOSURE
                    },
                )
            })
            .unwrap_or((NO_CLOSURE, NO_CLOSURE));
        let upvals_raw = self.resolve_upvals_raw(active_closure);
        let activation_token =
            self.enter_tierc_activation(active_closure, active_callee, false, upvals_raw)?;
        // Lend the native tier a bounded slice of the step budget, which its
        // compiled body charges directly. Saved and restored around the run
        // because native code re-enters Rust through its call helpers, and that
        // Rust can enter native again.
        // An enclosing native run's unspent loan is parked in the budget for
        // this one (`meter_park`) and handed back after.
        #[cfg(feature = "instrument")]
        let outer_steps = self.meter_park();
        #[cfg(feature = "instrument")]
        let _ = self.meter_lend();
        let (bits, bail) = unsafe { (*jitfn).run(regs_ptr, vm_ptr) };
        self.leave_tierc_activation(activation_token);
        #[cfg(feature = "instrument")]
        {
            self.meter_return();
            self.meter_unpark(outer_steps);
        }
        Some((Value::from_bits(bits), bail))
    }

    /// Pre-intern `func_id`'s multi-char string constants (for Tier C's whole-
    /// function mem path to embed their bits as immediates), returning a
    /// `const-index → interned bits` map. Each interned Value is rooted for the
    /// VM's life in `jit_const_strings` (compiled code isn't GC-traced). Mirrors
    /// the OSR region's const_strs build (over the whole function body here).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_build_const_strs(&mut self, func_id: u32) -> rustc_hash::FxHashMap<u32, u64> {
        let pending: Vec<u32> = {
            let proto = self.func(func_id as usize);
            proto
                .code
                .iter()
                .filter_map(|ins| match *ins {
                    Instr::LoadConst { idx, .. } => {
                        let c = proto.constants[idx as usize];
                        if c.is_heap() && (c.heap_index() & STRING_CONST_BIT) != 0 {
                            Some(idx)
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .collect()
        };
        let mut const_strs: rustc_hash::FxHashMap<u32, u64> = rustc_hash::FxHashMap::default();
        for idx in pending {
            if const_strs.contains_key(&idx) {
                continue;
            }
            let c = self.func(func_id as usize).constants[idx as usize];
            let v = self.resolve_const(func_id, c);
            self.jit_const_strings.push(v); // GC root
            const_strs.insert(idx, v.bits());
        }
        const_strs
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
    pub(crate) fn try_run_osr(
        &mut self,
        func_id: u32,
        entry_ip: u32,
        base: usize,
    ) -> Option<usize> {
        // Global-route revalidation, as in `try_run_jit`. Scoped to the whole
        // FUNCTION rather than the region's `[header, back-edge)` slice: declining a
        // region because a global elsewhere in the body went backwards is
        // conservative in the safe direction, it shares the per-func memo, and this
        // path is only reachable at all once some program has deleted a global.
        if self.global_route_epoch != 0 && !self.jit_globals_still_routable(func_id) {
            return None;
        }
        // Copy the native entry pointer and the SROA plan OUT of the region
        // before running: a region's call helper can re-enter the interpreter,
        // which may compile NEW regions (rehashing `jit.regions` — any &Region
        // would dangle) or even evict THIS region (parked in `Jit::retired`,
        // so the mmap'd code stays alive for the in-flight run).
        let (entry, field_plan, local_sroa, bounds, is_mem) = {
            let region = self.jit.get_region(func_id, entry_ip)?;
            (
                region.entry(),
                region.field_plan().cloned(),
                region.local_sroa_plan().cloned(),
                region.bounds(),
                region.is_mem(),
            )
        };

        // â”€â”€ pre-run sync â”€â”€ load the promoted object's fields into the scratch
        // pool globals the native code reads as ordinary globals.
        if let Some(ref p) = field_plan {
            let obj = self.globals[p.obj_global as usize];
            // â”€â”€ SROA shape guard â”€â”€ the native body reads/writes scratch pool slots,
            // bypassing any getter/setter and the property's writability. If the
            // global was reassigned, or its object's shape changed since compile
            // (a key add/remove/redefine, freeze, or setPrototypeOf bumps the
            // version, any of which could turn a promoted data field into an
            // accessor / non-writable / inherited slot), the bypass would diverge.
            // Fall back to the interpreter for this entry (the region stays; a
            // later entry with the original shape can still use it). See
            // memory: sroa-accessor-miscompile.
            if !obj.is_heap()
                || obj.heap_index() != p.obj_idx
                || self.heap.version_of(p.obj_idx) != p.obj_version
            {
                self.jit.note_region_entry_refused(func_id, entry_ip);
                return None;
            }
            for &(name_idx, slot) in &p.fields {
                let key =
                    self.func(p.func_id as usize).string_constants[name_idx as usize].as_str();
                let v = self.get_prop(obj, &key).unwrap_or(Value::UNDEFINED);
                self.globals[slot as usize] = v;
            }
        }

        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // SAFETY: `entry` points into the region's mmap'd ExecutableBuffer
        // (stable even if `jit.regions` rehashes; eviction parks, never drops,
        // a possibly-running region); regs/globals never reallocate (pinned).
        // An enclosing native run's unspent loan is parked in the budget for
        // this one (`meter_park`) and handed back after.
        #[cfg(feature = "instrument")]
        let outer_steps = self.meter_park();
        #[cfg(feature = "instrument")]
        let _ = self.meter_lend();
        let resume = {
            // Tagged around the native call ONLY, so a region helper that
            // re-enters the interpreter (`run_loop` re-tags itself `Interp`)
            // does not have its nested time charged to compiled code. Split by
            // TIER: `jit-mem` is the memory-backed path, which B92 measured ~4x
            // slower than the register-homed one on an identical loop.
            let _prof = crate::vm::prof::enter(if is_mem {
                crate::vm::prof::Phase::JitMem
            } else {
                crate::vm::prof::Phase::Jit
            });
            unsafe {
                let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
                    std::mem::transmute(entry);
                let mut resume: u32 = crate::codegen::NO_BAIL;
                let _ = f(regs_ptr, &mut resume as *mut u32, vm_ptr);
                resume
            }
        };
        // Running out of lent steps is a normal, periodic exit, not a signal
        // about this region's quality — the interpreter tops the budget up and
        // the next back-edge comes straight back here. Counting it as a deopt
        // would evict every hot loop after 64 chunks.
        #[cfg(feature = "instrument")]
        let meter_exit = self.meter_exhausted();
        #[cfg(feature = "instrument")]
        {
            self.meter_return();
            self.meter_unpark(outer_steps);
        }
        #[cfg(not(feature = "instrument"))]
        let meter_exit = false;
        #[cfg(not(target_arch = "wasm32"))]
        if interp_stats::enabled() {
            interp_stats::bump_region(func_id, entry_ip, resume);
        }

        // A LOCAL-SROA native body ran over an index-preserving clone in which
        // fresh aggregates were scalar homes. A clean exit proved those refs
        // dead; an internal guard bail resumes ORIGINAL bytecode, so recreate
        // every allocation that logically preceded the resume ip first. The
        // planner keeps array inputs stable and object fields in otherwise-
        // unused frame registers, all of which remain ordinary GC roots.
        if let Some(ref p) = local_sroa {
            let internal = resume >= bounds.0 && resume <= bounds.1;
            if internal {
                // The concat-length sub-lane carries only its proved bounded
                // integer in `scratch`. Restore the exact original primitive
                // string before materializing fields or resuming bytecode. A
                // bail between the original LoadConst and Add also needs the
                // prefix register restored; later bails need any Add/GetProp
                // destination whose next original definition has not run yet.
                for c in &p.concat_lens {
                    let raw_prefix = self.func(func_id as usize).constants[c.prefix_const as usize];
                    if c.prefix_load_ip < resume && resume <= c.add_ip {
                        let prefix = self.resolve_const(func_id, raw_prefix);
                        self.set(base, c.prefix_reg, prefix);
                    } else if c.add_ip < resume {
                        let number = self.get(base, c.scratch);
                        let prefix = self.resolve_const(func_id, raw_prefix);
                        let string = match self.add_values(prefix, number) {
                            Ok(v) => v,
                            Err(t) => {
                                // Eligibility proves primitive string + Int, so
                                // this is an invariant fallback rather than a
                                // reachable coercion path. Preserve normal JIT
                                // throw plumbing instead of panicking if that
                                // invariant is ever widened incorrectly.
                                let _ = self.jit_thrown_to_sentinel(t);
                                Value::UNDEFINED
                            }
                        };
                        self.set(base, c.scratch, string);
                        if resume <= c.add_live_until {
                            self.set(base, c.add_dst, string);
                        }
                        if c.get_ip < resume && resume <= c.get_live_until {
                            self.set(base, c.get_dst, string);
                        }
                    }
                }

                enum PendingAlloc {
                    Object(usize),
                    Array(usize),
                    Finalized(usize),
                }
                let mut pending = Vec::new();
                for (i, o) in p.objects.iter().enumerate() {
                    if o.alloc_ip < resume {
                        pending.push((o.alloc_ip, PendingAlloc::Object(i)));
                    }
                }
                for (i, a) in p.arrays.iter().enumerate() {
                    if a.alloc_ip < resume {
                        pending.push((a.alloc_ip, PendingAlloc::Array(i)));
                    }
                }
                for (i, f) in p.finalized.iter().enumerate() {
                    if f.alloc_ip < resume {
                        pending.push((f.alloc_ip, PendingAlloc::Finalized(i)));
                    }
                }
                pending.sort_by_key(|(ip, _)| *ip);
                for (_, alloc) in pending {
                    match alloc {
                        PendingAlloc::Object(i) => {
                            let o = &p.objects[i];
                            let completed: Vec<(String, Value)> = o
                                .fields
                                .iter()
                                .filter(|f| f.append_ip < resume)
                                .map(|f| {
                                    (
                                        self.func(func_id as usize).string_constants
                                            [f.name as usize]
                                            .clone(),
                                        self.get(base, f.scratch),
                                    )
                                })
                                .collect();
                            let mut map = ObjMap::with_capacity(o.hint as usize);
                            for (name, value) in completed {
                                map.push_data(name, value);
                            }
                            let idx = self.heap.alloc(HeapObj::Object(Box::new(map)));
                            self.realm_born(idx, self.obj_proto);
                            self.set(base, o.dst, Value::heap(idx));
                        }
                        PendingAlloc::Array(i) => {
                            let a = &p.arrays[i];
                            let items: Vec<Value> = (0..a.argc)
                                .map(|j| self.get(base, a.arg_base + j))
                                .collect();
                            let idx = self.heap.alloc(HeapObj::Array(items));
                            self.realm_born(idx, self.arr_proto);
                            self.set(base, a.dst, Value::heap(idx));
                        }
                        PendingAlloc::Finalized(i) => {
                            // The planner proved the staged block unchanged
                            // since the (elided) finalize, and the concat-len
                            // pass above has already restored any virtual
                            // string into its slot, so this rebuilds the exact
                            // interpreter object. Plans are immutable per
                            // FuncProto; a missing/invalid plan is unreachable
                            // and fails closed to the pre-SROA legacy build.
                            let f = &p.finalized[i];
                            let plan = self
                                .func(func_id as usize)
                                .static_key_plans
                                .get(f.plan_idx as usize)
                                .cloned();
                            let vals: Vec<Value> = (0..f.count)
                                .map(|j| self.get(base, f.val_base + j))
                                .collect();
                            let map = match plan {
                                Some(plan)
                                    if plan.runtime_valid() && plan.len() == f.count as usize =>
                                {
                                    let shape = self.finalize_shape(func_id, f.plan_idx, &plan);
                                    ObjMap::finalized_from_plan(plan, vals, shape)
                                }
                                _ => {
                                    debug_assert!(false, "finalized SROA plan disappeared");
                                    ObjMap::with_capacity(f.count as usize)
                                }
                            };
                            let idx = self.heap.alloc(HeapObj::Object(Box::new(map)));
                            self.realm_born(idx, self.obj_proto);
                            self.set(base, f.dst, Value::heap(idx));
                        }
                    }
                }
            }
            // Scratch is dead on a clean exit and has been copied into any
            // materialized object on a bail. Clear it to avoid retaining the
            // last iteration's heap values until this activation returns.
            for &r in &p.scratch {
                self.set(base, r, Value::UNDEFINED);
            }
        }

        // â”€â”€ post-run sync â”€â”€ flush the pool globals back to the object's fields,
        // so the interpreter (which resumes on the ORIGINAL bytecode, reading the
        // object) sees consistent values. Runs on EVERY exit (clean or bail).
        if let Some(ref p) = field_plan {
            let obj = self.globals[p.obj_global as usize];
            for &(name_idx, slot) in &p.fields {
                let key =
                    self.func(p.func_id as usize).string_constants[name_idx as usize].as_str();
                let v = self.globals[slot as usize];
                let _ = self.set_prop(obj, &key, v, false);
            }
        }
        // Bookkeeping: a resume INSIDE the region is a deopt; evict if chronic.
        // A call helper may have flagged this exit as exempt (depth-cap deopt /
        // a throw the call legitimately produced) — legal recursion and caught
        // exceptions must not evict a hot region.
        let exempt = std::mem::take(&mut self.osr_deopt_exempt) || meter_exit;
        if !exempt {
            self.jit.note_region_resume(func_id, entry_ip, resume);
        }
        Some(resume as usize)
    }

    /// Pop the current frame. If this returns control to `stop_depth` (the
    /// frame the active `run_loop` was asked to run), report `true` so the loop
    /// returns `ret`. Otherwise deliver `ret` into the caller's `ret_dst` and
    /// report `false` to keep executing the caller.
    #[inline]
    pub(crate) fn pop_frame_with(&mut self, mut ret: Value, stop_depth: usize) -> bool {
        // A MAPPED arguments object that escapes this frame answers from its
        // dense store once the frame is gone: flush the live formal registers
        // into it BEFORE the window dies, or post-return reads see stale
        // entry values (staging/sm/Function/strict-arguments.js).
        if let Some(f) = self.frames.last() {
            if f.args_obj != u32::MAX {
                let aidx = f.args_obj;
                self.args_sync_dense(aidx);
            }
        }
        let mut finished = self.frames.pop().expect("frame underflow");
        // A flat `new`: an object result replaces the instance, anything else
        // returns it ([[Construct]] step 10-12 for an ordinary function). The
        // instance's size is what the constructor's next one reserves.
        if finished.construct_this {
            // Or a generator's step run in its consumer's loop
            // (`gen_enter_inline`), whose return completes the step.
            if finished.gen != u32::MAX {
                self.gen_return_inline(
                    finished.base,
                    finished.gen,
                    finished.ret_dst,
                    finished.argc,
                    finished.super_done,
                    ret,
                );
                return false;
            }
            let instance = self.regs[finished.base];
            if instance.is_heap() {
                self.learn_ctor_field_hint(finished.func, instance);
            }
            if !self.is_object_value(ret) {
                ret = instance;
            }
        }
        // Its handler buffer's storage, for another frame's first push.
        if finished.handlers.capacity() != 0 {
            super::handler_pool::give(&mut self.handler_pool, std::mem::take(&mut finished.handlers));
        }
        // A real pop proves the tail-reuse streak terminated — reset its budget.
        self.tail_reuse_streak = 0;
        // Shrink the register file back to the caller's window top.
        self.regs.truncate(finished.base);
        if self.frames.len() == stop_depth {
            return true;
        }
        // RET_DISCARD marks a frame whose return value is dropped (an
        // IC-pushed setter activation — assignment doesn't observe it).
        if finished.ret_dst != RET_DISCARD {
            let caller_base = self.frames.last().unwrap().base;
            self.regs[caller_base + finished.ret_dst as usize] = ret;
        }
        false
    }

    /// [`Self::pop_frame_with`] inline, for the frame nearly every return
    /// pops: no `arguments` object to flush, not a flat `new`, no handler
    /// buffer to give back. Any other frame takes the full routine. Out of
    /// line, the pop's call and its frame setup were a third of its cost on
    /// a recursive call.
    #[inline(always)]
    pub(crate) fn pop_frame_fast(&mut self, ret: Value, stop_depth: usize) -> bool {
        let n = self.frames.len();
        let f = &self.frames[n - 1];
        if f.args_obj != u32::MAX || f.construct_this || f.handlers.capacity() != 0 || !lean_calls_enabled() {
            return self.pop_frame_with(ret, stop_depth);
        }
        let (fbase, ret_dst) = (f.base, f.ret_dst);
        self.frames.truncate(n - 1);
        self.tail_reuse_streak = 0;
        self.regs.truncate(fbase);
        if n - 1 == stop_depth {
            return true;
        }
        if ret_dst != RET_DISCARD {
            let caller_base = self.frames[n - 2].base;
            self.regs[caller_base + ret_dst as usize] = ret;
        }
        false
    }

    /// The intrinsic of a `StaticFn` instruction (`Object.assign`,
    /// `Object.fromEntries`, `Array.of`, `String.fromCharCode`, the
    /// `Number.is*` tests, the `Promise` statics, `Object.defineProperty` and
    /// its siblings) applied to `args`, once the instruction's callee and
    /// receiver are proven the intrinsic (`static_fn_is_intrinsic`). Shared by
    /// the interpreter's arm and compiled code (`jit_static_fn_ic`). Inlined
    /// into both: the interpreter loop kept this body inline before it was
    /// shared, and calling it out of line moved the loop's code enough to
    /// cost every interpreted Python call about 5%.
    #[inline(always)]
    pub(crate) fn static_fn_exec(
        &mut self,
        op: crate::bytecode::StaticFn,
        args: Vec<Value>,
    ) -> Result<Value, Thrown> {
        use crate::bytecode::StaticFn as S;
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        Ok(match op {
            S::ArrayOf => Value::heap(self.heap.alloc(HeapObj::Array(args))),
            S::NumberIsInteger => Value::bool(num_is_integer(a0)),
            S::NumberIsNaN => Value::bool(a0.is_double() && a0.as_f64().is_nan()),
            S::NumberIsFinite => Value::bool(num_is_finite(a0)),
            S::NumberIsSafeInteger => Value::bool(num_is_safe_integer(a0)),
            S::StringFromCharCode => {
                // ToUint16(ToNumber(v)) per arg, in argument order;
                // adjacent (high, low) surrogate halves combine into
                // the astral scalar â€” see `string_from_char_codes`.
                let js = self.string_from_char_codes(&args)?;
                Value::heap(self.heap.alloc_js(js))
            }
            S::ObjectAssign => self.object_assign(&args)?,
            S::ObjectFromEntries => self.object_from_entries(a0)?,
            S::PromiseResolve => {
                // Promise.resolve(p) of an existing Promise is identity
                // ONLY when Get(p, "constructor") is still %Promise%
                // (PromiseResolve step 2) — a changed constructor gets
                // a fresh adopting promise.
                if a0.is_heap()
                    && matches!(
                        self.heap.get(a0.heap_index()),
                        HeapObj::Promise { .. }
                    )
                    && self.get_prop(a0, "constructor")?
                        == self.promise_ctor_value()
                {
                    a0
                } else if let Some(p) =
                    self.try_alloc_fulfilled_primitive_promise(a0)
                {
                    Value::heap(p)
                } else {
                    let p = self.alloc_promise();
                    if a0.is_heap() {
                        self.resolve_fresh_promise(p, a0);
                    } else {
                        self.resolve(p, a0);
                    }
                    Value::heap(p)
                }
            }
            S::PromiseReject => {
                let p = self.alloc_promise();
                self.reject(p, a0);
                Value::heap(p)
            }
            S::PromiseAll => {
                let c = self.promise_ctor_value();
                self.promise_combine(crate::heap::CombKind::All, a0, c)?
            }
            S::PromiseAllSettled => {
                let c = self.promise_ctor_value();
                self.promise_combine(crate::heap::CombKind::AllSettled, a0, c)?
            }
            S::PromiseRace => {
                let c = self.promise_ctor_value();
                self.promise_combine(crate::heap::CombKind::Race, a0, c)?
            }
            S::PromiseAny => {
                let c = self.promise_ctor_value();
                self.promise_combine(crate::heap::CombKind::Any, a0, c)?
            }
            S::ObjectDefineProperty => {
                // Type(O) must be Object — checked BEFORE
                // ToPropertyKey(P), whose toString may run.
                if !self.is_object_value(a0) {
                    return Err(Thrown(
                        "TypeError: Object.defineProperty called on non-object"
                            .into(),
                    ));
                }
                let key = self.to_property_key(
                    args.get(1).copied().unwrap_or(Value::UNDEFINED),
                )?;
                let desc = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                self.object_define_property(a0, &key, desc)?;
                a0
            }
            S::ObjectDefineProperties => {
                if !self.is_object_value(a0) {
                    return Err(Thrown(
                        "TypeError: Object.defineProperties called on non-object"
                            .into(),
                    ));
                }
                let props = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                self.object_define_properties(a0, props)?;
                a0
            }
            S::PerformanceNow => {
                Value::num(crate::vm::clock::now_mono_ms() - self.start_mono_ms)
            }
            S::ObjectGetOwnPropertyDescriptor => {
                self.require_object_coercible(a0)?; // ToObject(O)
                let o = self.to_object(a0)?;
                let key = self.to_property_key(
                    args.get(1).copied().unwrap_or(Value::UNDEFINED),
                )?;
                self.defer_check(o, &key)?;
                self.ns_tdz_check(o, &key)?; // uninit export throws
                match self.proxy_gopd(o, &key)? {
                    Some(d) => d,
                    None => self.object_get_own_property_descriptor(o, &key),
                }
            }
            S::ObjectGetOwnPropertyNames => {
                self.require_object_coercible(a0)?; // ToObject(O)
                let o = self.to_object(a0)?;
                self.object_own_property_names(o)?
            }
            S::ObjectGetPrototypeOf => {
                self.require_object_coercible(a0)?; // ToObject(O)
                let o = self.to_object(a0)?;
                self.get_prototype_of_checked(o)?
            }
            S::ObjectCreate => {
                if a0 != Value::NULL && !self.is_object_value(a0) {
                    return Err(Thrown(
                        "TypeError: Object prototype may only be an Object or null"
                            .into(),
                    ));
                }
                let o = Value::heap(
                    self.heap.alloc(HeapObj::Object(Box::new(ObjMap::new()))),
                );
                if a0 != Value::UNDEFINED {
                    self.proto_of.insert(o.heap_index(), a0);
                }
                if let Some(props) = args.get(1).copied() {
                    if props != Value::UNDEFINED {
                        // `o` is a Rust local across the descriptor
                        // getters (as in the OBJ_CREATE native).
                        self.with_host_roots(&[o], |vm| {
                            vm.object_define_properties(o, props)
                        })?;
                    }
                }
                o
            }
        })
    }

    /// Render a thrown value for the UNCAUGHT-throw message (the `Outcome.error`
    /// string). An Error-like object (`{message,â€¦}` or one with a `.message`)
    /// prints `name: message`; otherwise the value's string form. Catchable
    /// throws bind the real `Value`, so this is only the top-level report.
    pub(crate) fn throw_message(&self, v: Value) -> String {
        if v.is_heap() {
            if let HeapObj::Object(_) = self.heap.get(v.heap_index()) {
                let idx = v.heap_index();
                // `name`: own/inherited "name", else the constructor's name — so a
                // nameless user error (e.g. the harness Test262Error, which sets only
                // `.message` but has constructor.name "Test262Error") reports
                // "Test262Error: …" rather than "Error: …", matching V8/Node (and
                // letting a negative-test type substring-match its stderr).
                let name = self
                    .read_data_prop(idx, "name")
                    .map(|n| self.display(n))
                    .or_else(|| {
                        self.read_data_prop(idx, "constructor")
                            .map(|c| self.callable_name(c))
                            .filter(|s| !s.is_empty())
                    });
                let msg = self.read_data_prop(idx, "message").map(|m| self.display(m));
                return match (name, msg) {
                    (Some(n), Some(m)) => format!("{n}: {m}"),
                    (Some(n), None) => n,
                    (None, Some(m)) => format!("Error: {m}"),
                    _ => self.display(v),
                };
            }
        }
        format!("Uncaught {}", self.display(v))
    }

    // â”€â”€ register access â”€â”€
    //
    // Unchecked: the compiler allocates `reg_count` registers per function and
    // never emits a register index â‰¥ `reg_count` (it tracks a `max_reg`
    // high-water mark), and every frame resizes `self.regs` to
    // `base + reg_count` on entry â€” so `base + r` is always in bounds. We index
    // `self.regs` freshly each call (no cached pointer), so a reallocation of
    // the register Vec by a re-entrant call/alloc is handled correctly. The
    // `debug_assert!` turns any compiler bug into a loud test failure in debug
    // builds while release elides the bounds check.
    #[inline(always)]
    pub(crate) fn get(&self, base: usize, r: u16) -> Value {
        // `base` starts an existing frame window in `regs`, whose allocation
        // is at most `isize::MAX` bytes, while a register index is only `u16`.
        // The sum therefore cannot overflow `usize`. Spell that invariant as a
        // wrapping add so overflow-checked hardened builds do not put a second
        // trap edge ahead of every register access; safe-sandbox's indexed
        // access below still performs the required bounds check.
        let index = base.wrapping_add(r as usize);
        debug_assert!(index < self.regs.len(), "reg read out of bounds");
        #[cfg(feature = "safe-sandbox")]
        return self.regs[index];
        #[cfg(not(feature = "safe-sandbox"))]
        return unsafe { *self.regs.get_unchecked(index) };
    }
    /// Whether the dispatch may run a function's superinstruction copy
    /// (`FusedCode`): not under `ZIPP_NO_INTERP_FUSE`, and not while a
    /// recorder meters or traces each instruction, which a superinstruction
    /// (one dispatch for two) would miscount.
    #[inline(always)]
    pub(crate) fn interp_fuse_ok(&self) -> bool {
        // A metered build's step counts and its exact-shape loop projections
        // (`try_metered_*`) are its original instructions'.
        if cfg!(feature = "meter-only") {
            return false;
        }
        #[cfg(feature = "instrument")]
        if self.instr_rec.is_some() {
            return false;
        }
        crate::bytecode::interp_fuse_enabled()
    }

    /// `Bitwise`'s result for two operands already read, for its
    /// superinstructions: two Int32s inline, Numbers that convert exactly,
    /// everything else (coercion, BigInt, the TypeErrors) through
    /// `numeric_binop`.
    #[inline(always)]
    pub(crate) fn fused_bitwise(&mut self, va: Value, vb: Value, op: crate::bytecode::BitwiseOp) -> Result<Value, Thrown> {
        use crate::bytecode::BitwiseOp as B;
        let ints = if va.is_int() && vb.is_int() {
            Some((va.as_int(), vb.as_int()))
        } else {
            number_int32(va).zip(number_int32(vb))
        };
        if let Some((x, y)) = ints {
            return Ok(match op {
                B::And => Value::int(x & y),
                B::Or => Value::int(x | y),
                B::Xor => Value::int(x ^ y),
                B::Shl => Value::int(x.wrapping_shl((y as u32) & 31)),
                B::Shr => Value::int(x >> ((y as u32) & 31)),
                B::Ushr => {
                    let u = (x as u32) >> ((y as u32) & 31);
                    if u <= i32::MAX as u32 {
                        Value::int(u as i32)
                    } else {
                        Value::num(u as f64)
                    }
                }
            });
        }
        let bop = match op {
            B::And => BigOp::And,
            B::Or => BigOp::Or,
            B::Xor => BigOp::Xor,
            B::Shl => BigOp::Shl,
            B::Shr => BigOp::Shr,
            B::Ushr => BigOp::Ushr,
        };
        self.numeric_binop(bop, va, vb)
    }

    /// `AddInt`'s result (see its arm), for its superinstructions: the
    /// Number paths inline, a Python `for` counter's small BigInt inline as
    /// in the arm (it is every iteration's), the rest out of line.
    #[inline(always)]
    pub(crate) fn add_int_value(&mut self, va: Value, imm: i32, upd: bool) -> Result<Value, Thrown> {
        if va.is_int() {
            return Ok(match va.as_int().checked_add(imm) {
                Some(v) => Value::int(v),
                None => Value::num(va.as_int() as f64 + imm as f64),
            });
        }
        if va.is_double() {
            return Ok(Value::num(va.as_f64() + imm as f64));
        }
        #[cfg(feature = "python")]
        if upd {
            if let Some(v) = va.small_bigint_val().and_then(|n| Value::small_bigint(n as i128 + imm as i128)) {
                return Ok(v);
            }
        }
        self.add_int_generic(va, imm, upd)
    }

    /// `AddInt`'s path for an operand that is not a Number, for its
    /// superinstruction (`Instr::FuseAddIntMove`): the arm's own code.
    #[cold]
    #[inline(never)]
    fn add_int_generic(&mut self, va: Value, imm: i32, upd: bool) -> Result<Value, Thrown> {
        let p = self.numeric_prim(va)?;
        if let Some(b) = self.bigint_val(p) {
            if upd {
                self.bigint_op(
                    BigOp::Add,
                    b,
                    crate::vm::bigint::BigVal::Small(imm as i128),
                )
            } else {
                Err(Thrown(crate::vm::bigint::BIGINT_MIX_ERR.into()))
            }
        } else {
            Ok(Value::num(self.to_number(p)? + imm as f64))
        }
    }

    #[inline(always)]
    pub(crate) fn set(&mut self, base: usize, r: u16, v: Value) {
        let index = base.wrapping_add(r as usize);
        debug_assert!(index < self.regs.len(), "reg write out of bounds");
        #[cfg(feature = "safe-sandbox")]
        {
            self.regs[index] = v;
        }
        #[cfg(not(feature = "safe-sandbox"))]
        unsafe {
            *self.regs.get_unchecked_mut(index) = v;
        }
    }

    // â”€â”€ call setup â”€â”€

    /// Resolve a value to a callable function id, or throw a TypeError.
    /// `MakeClosure` (`this_reg` `None`) / `MakeArrow` in activation
    /// `frame_idx`, whose window starts at `base`. Each upvalue's cell is
    /// resolved in that (defining) activation: a ParentLocal source reads the
    /// cell from a local register (the local was boxed via MakeCell); a
    /// ParentUpval source forwards one of the activation's own captured
    /// cells. An arrow also captures the effective `this` (register
    /// `this_reg`: usually reg 0, but the class value inside a static field
    /// initializer) and inherits the activation's `[[HomeObject]]` and
    /// `new.target` lexically. Both inherit a class body's private scope, any
    /// dynamic EvalScope and the realm. The interpreter arms and the loop
    /// region's helper share this.
    pub(crate) fn make_closure_in_frame(
        &mut self,
        frame_idx: usize,
        base: usize,
        func_id: u32,
        this_reg: Option<u16>,
    ) -> Value {
        let cur_closure = self.frames[frame_idx].closure;
        let sources = &self.func(func_id as usize).upvalues;
        let mut cells = Vec::with_capacity(sources.len());
        for src in sources {
            let cell = match *src {
                UpvalSource::ParentLocal(reg) => self.get(base, reg).heap_index(),
                UpvalSource::ParentUpval(idx) => self.closure_upvalue(cur_closure, idx),
            };
            cells.push(cell);
        }
        self.make_closure_with_cells(frame_idx, base, func_id, this_reg, cells)
    }

    /// [`Vm::make_closure_in_frame`] once the capture cells are resolved.
    pub(crate) fn make_closure_with_cells(
        &mut self,
        frame_idx: usize,
        base: usize,
        func_id: u32,
        this_reg: Option<u16>,
        cells: Vec<u32>,
    ) -> Value {
        let this_val = match this_reg {
            Some(r) => self.get(base, r),
            None => Value::UNDEFINED,
        };
        let v = Value::heap(self.heap.alloc_closure_settled(func_id, cells, this_val));
        let callee = self.frames[frame_idx].callee;
        if this_reg.is_some() {
            // An arrow inside an object method inherits that method's
            // [[HomeObject]] lexically (so `super.x` in the arrow resolves).
            if callee.is_heap() {
                if let Some(&home) = self.closure_home.get(&callee.heap_index()) {
                    self.record_closure_home(v.heap_index(), home);
                }
            }
            // ... and the defining activation's `new.target` (an arrow has
            // none of its own; `(_ => new.target)()` inside `new F()` must see
            // F). A nested arrow chains through the enclosing arrow's
            // captured value.
            let fnt = self.frames[frame_idx].new_target;
            let nt = if fnt != Value::UNDEFINED {
                fnt
            } else if callee.is_heap() {
                self.closure_new_target
                    .get(&callee.heap_index())
                    .copied()
                    .unwrap_or(Value::UNDEFINED)
            } else {
                Value::UNDEFINED
            };
            if nt != Value::UNDEFINED {
                self.record_closure_new_target(v.heap_index(), nt);
            }
        }
        // A function declared inside a class body keeps that body's private
        // scope.
        self.inherit_private_brands(callee, v.heap_index());
        if let Some(sc) = self.ensure_frame_eval_scope(frame_idx) {
            self.closure_eval_scope.insert(v.heap_index(), sc);
        }
        self.realm_tag_new(v.heap_index());
        v
    }

    /// `ArrayAppend { arr, val, spread }`: push `vv` onto the array literal
    /// `aidx` under construction, or spread it (the iterator protocol, or
    /// one of the inline paths while the iterator is unobservable). Shared by
    /// the interpreter arm and the loop region's helper.
    pub(crate) fn array_append_op(
        &mut self,
        aidx: u32,
        vv: Value,
        spread: bool,
    ) -> Result<(), Thrown> {
        // Nursery barrier (holder-grain): the literal under
        // construction is normally young, but a spread arm can
        // run user code (an iterator), whose safe points can
        // promote it mid-build — every later append is then an
        // old→young store. Old holder ⇒ one card per epoch.
        self.heap.write_barrier(aidx);
        if spread {
            // Spreading a LIVE-mapped arguments object: refresh
            // its dense slots from the formals first (the array
            // fast path below clones the dense store directly).
            if vv.is_heap() {
                self.args_sync_dense(vv.heap_index());
            }
            // Everything that is NOT one of the four inline fast
            // paths below (Array / Set / Str|Cons / Map) is drained
            // through the iterator protocol, which raises the
            // TypeError when the value has genuinely no
            // @@iterator. Naming the kinds explicitly instead left
            // `[...new Proxy([1,2],{})]`, `[...new String("ab")]`
            // and any object with a user-installed @@iterator
            // reporting "not iterable".
            // The inline paths stand in for the iterator only
            // while it is unobservable: the @@iterator read here
            // (GetIterator's one Get) is the kind's intrinsic and
            // its %XIteratorPrototype%.next is pristine. A
            // replaced @@iterator (own, subclass, patched
            // prototype) or a patched `next` spreads through the
            // protocol with the method already read.
            if vv.is_heap() && !self.builtin_iter_fast(vv, false) {
                let m = self.get_prop(vv, "@@iterator")?;
                if !self.builtin_iter_pristine(vv, m, false) {
                    let elems = self.iterate_with_method(vv, m)?;
                    if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                        dst_items.extend(elems);
                    }
                    return Ok(());
                }
            }
            // A string spreads by CODE POINT, exactly: a lone
            // surrogate is its own one-unit string (the lossy
            // `char` view made it U+FFFD).
            if let Some(elems) = self.string_iter_values(vv, usize::MAX) {
                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                    dst_items.extend(elems);
                }
                return Ok(());
            }
            // A TypedArray has no inline arm below: its element
            // count is the view's LIVE length (a length-tracking
            // view over a grown buffer yields the new elements)
            // and a detached or out-of-bounds view is the
            // iterator's TypeError, both of which the positional
            // walk behind `iterate_to_vec` already gets right.
            if vv.is_heap()
                && matches!(self.heap.get(vv.heap_index()), HeapObj::TypedArray { .. })
            {
                let elems = self.positional_iteration_elements(vv)?;
                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                    dst_items.extend(elems);
                }
                return Ok(());
            }
            // Materialize the spread source's elements (array/set â†’
            // elements; map â†’ [k,v] entries) WITHOUT holding a heap
            // borrow across the fresh allocations. A string never
            // reaches here: `string_iter_values` above already
            // spread it by EXACT code point.
            let mut map_pairs: Option<Vec<(Value, Value)>> = None;
            let mut array_src: Option<u32> = None;
            if vv.is_heap() {
                match self.heap.get(vv.heap_index()) {
                    // Deferred like the cases below: a hole in the
                    // source resolves through the prototype chain,
                    // which can run a getter, so it must not happen
                    // under this borrow.
                    HeapObj::Array(_) => array_src = Some(vv.heap_index()),
                    HeapObj::Set(items) => {
                        // Skip tombstoned (deleted) slots.
                        let elems: Vec<Value> = items
                            .iter()
                            .copied()
                            .filter(|v| !v.is_hole())
                            .collect();
                        if let HeapObj::Array(d) = self.heap.get_mut(aidx) {
                            d.extend(elems);
                        }
                    }
                    HeapObj::Map { keys, vals } => {
                        // Skip tombstoned (deleted) entries.
                        map_pairs = Some(
                            keys.iter()
                                .copied()
                                .zip(vals.iter().copied())
                                .filter(|(k, _)| !k.is_hole())
                                .collect(),
                        );
                    }
                    _ => {
                        return Err(Thrown(
                            "TypeError: spread value is not iterable".into(),
                        ));
                    }
                }
            } else {
                return Err(Thrown(
                    "TypeError: spread value is not iterable".into(),
                ));
            }
            if let Some(src_idx) = array_src {
                let elems = self.spread_array_elements(src_idx)?;
                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                    dst_items.extend(elems);
                }
            }
            if let Some(pairs) = map_pairs {
                let elems: Vec<Value> = pairs
                    .into_iter()
                    .map(|(k, v)| {
                        Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v])))
                    })
                    .collect();
                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                    dst_items.extend(elems);
                }
            }
        } else if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
            dst_items.push(vv);
        }
        Ok(())
    }

    /// The cell heap-index captured at upvalue slot `idx` of the closure heap
    /// object `closure`. Panics only on a miscompiled program (an UpvalGet in a
    /// frame with no closure, or an out-of-range slot), which the compiler must
    /// not emit.
    #[inline]
    pub(crate) fn closure_upvalue(&self, closure: u32, idx: u16) -> u32 {
        match self.heap.get(closure) {
            HeapObj::Closure { upvalues, .. } => upvalues[idx as usize],
            _ => panic!("UpvalGet/Set in a frame without a closure"),
        }
    }

    /// [`Self::closure_upvalue`] through `cache`, the running activation's
    /// upvalue list as (address, length), filled on its first upvalue access
    /// and reset by the dispatch loops at every frame change. A closure's list
    /// is fixed when `MakeClosure` makes it and its buffer stays where it is
    /// as the heap grows, and the running frame roots the closure, so the
    /// address holds for the whole activation. The access was a heap fetch
    /// and the list's header on every `UpvalGet` / `UpvalSet`: three
    /// dependent loads more than the cell itself.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline(always)]
    pub(crate) fn closure_upvalue_cached(&self, cache: &mut (*const u32, usize), closure: u32, idx: u16) -> u32 {
        if cache.0.is_null() {
            match self.heap.get(closure) {
                HeapObj::Closure { upvalues, .. } => *cache = (upvalues.as_ptr(), upvalues.len()),
                _ => panic!("UpvalGet/Set in a frame without a closure"),
            }
        }
        let i = idx as usize;
        assert!(i < cache.1, "upvalue index out of range");
        // SAFETY: `cache` was taken from this activation's closure, whose
        // list never changes and outlives the activation (see above), and
        // `i` is in its bounds.
        unsafe { *cache.0.add(i) }
    }

    /// The running activation's [[NewTarget]]. An ARROW frame has none of its own,
    /// so it falls back to the one its closure captured lexically at MakeArrow —
    /// without this, `new.target` reads `undefined` for every arrow.
    #[inline]
    pub(crate) fn frame_new_target(&self) -> Value {
        let (nt, callee) = match self.frames.last() {
            Some(f) => (f.new_target, f.callee),
            None => return Value::UNDEFINED,
        };
        if nt != Value::UNDEFINED || !callee.is_heap() {
            return nt;
        }
        self.closure_new_target
            .get(&callee.heap_index())
            .copied()
            .unwrap_or(nt)
    }

    /// The per-iteration for-in liveness re-check (EnumerateObjectProperties): is
    /// the snapshotted key `kv` still present on the receiver stored in `snapshot`?
    /// Shared by the interpreter `ForInLive` arm and the JIT `jit_forin_live`
    /// helper so both are byte-identical. A narrowly eligible ordinary Array can
    /// answer from the receiver/default-prototype versions captured by
    /// `ForInKeys`; a mismatch falls through to the exact old lookup below.
    /// NON-observable, so malformed internal state and opaque Proxy chains err on
    /// "live". No getter / Proxy trap fires and the call does not re-enter the
    /// dispatch loop, so it never runs a GC safe point (`key_of`'s transient
    /// `String` is a Rust alloc, not a VM-heap one).
    pub(crate) fn forin_live(&mut self, snapshot: Value, kv: Value) -> bool {
        // The ForInKeys Array is engine-private and cannot be replaced or
        // mutated by JavaScript. Copy the prefix values before any later heap
        // access so no borrow of its backing Vec crosses the ordinary path.
        let (
            o,
            receiver_version,
            array_proto,
            array_proto_version,
            object_proto,
            object_proto_version,
        ) = if snapshot.is_heap() {
            match self.heap.get(snapshot.heap_index()) {
                HeapObj::Array(items) if items.len() >= crate::bytecode::FORIN_SNAPSHOT_PREFIX => {
                    (items[0], items[1], items[2], items[3], items[4], items[5])
                }
                _ => return true,
            }
        } else {
            return true;
        };

        // A version hit proves that none of the three levels which supplied the
        // snapshot has lost a key or changed descriptors/prototype. The complete
        // structural licence was checked once while this immutable snapshot was
        // built; every operation which can revoke it bumps one of these versions.
        // Repeating HashMap/HeapObj shape probes here would merely replace the
        // old per-key property lookup with another per-key property lookup.
        // Adds/overwrites which leave versions stable cannot make a snapshotted
        // key disappear and are therefore harmless.
        if crate::codegen::forin_version_fast_enabled()
            && !receiver_version.is_undefined()
            && o.is_heap()
            && array_proto.is_heap()
            && object_proto.is_heap()
            && receiver_version == Value::int(self.heap.version_of(o.heap_index()) as i32)
            && array_proto_version
                == Value::int(self.heap.version_of(array_proto.heap_index()) as i32)
            && object_proto_version
                == Value::int(self.heap.version_of(object_proto.heap_index()) as i32)
        {
            return true;
        }

        // Own-hit fast probe, alloc-free: view the snapshotted key's bytes in
        // place (a flat ASCII/UTF-8 string — surrogate-bearing keys take the
        // generic path) and ask the receiver's own map directly. has_property
        // below re-derives the key as a fresh String per chain level.
        //
        // W19 (M4): THE ARRAY ARM. This probe used to match `HeapObj::Object`
        // ONLY, so an ARRAY receiver — the whole point of a `for (k in sparse)`
        // walk — fell through to `has_property` and then, on a miss, to
        // `key_of`, which allocates a fresh `String` for every key on every
        // iteration. Measured over sparse-array's 40,000-key `sp`: a count-only
        // for-in cost 250 ns/key against node's 170, while `Object.keys(sp)` on
        // the same receiver cost 67 ns/key against node's 97 — i.e. zipp BUILDS
        // the snapshot faster than node and loses it all in the liveness check.
        //
        // An array's own properties are its dense elements plus its `arr_props`
        // side table (sparse elements AND ordinary named keys both live there),
        // so both are probed, and both are byte views — no allocation:
        //   * a CANONICAL index key resolves against the dense slots, where a
        //     deleted element is a `HOLE` and correctly reads as absent;
        //   * every other key — and every canonical index the dense storage does
        //     not answer — takes one `pos` probe on the side table.
        // The canonicality test is load-bearing and is `canonical_index_str`'s,
        // byte-for-byte: `"01"`, `"1.0"`, `"-0"`, `" 1"` parse to a number but
        // are ORDINARY named properties that no element may answer. It is
        // spelled out here rather than called because `canonical_index_str`
        // round-trips through `i.to_string()`, and an allocation per key is the
        // cost this arm exists to remove.
        //
        // Answering `true` is the SAFE direction (it only keeps a key visited,
        // and this probe's whole job is to DROP keys deleted mid-loop), so the
        // arm is fail-closed: anything it cannot resolve falls through to the
        // unchanged generic path below. Arguments objects are excluded — their
        // `length`/mapped-index machinery is not this simple ownership question.
        let own_hit = o.is_heap()
            && kv.is_heap()
            && match self.heap.str_wtf8_cow(kv.heap_index()) {
                // ("@@…" is escaped as a key — `key_of` — so the generic path
                // below decides it.)
                Some(std::borrow::Cow::Borrowed(b)) if !b.starts_with(b"@@") => {
                    let oidx = o.heap_index();
                    match (std::str::from_utf8(b), self.heap.get(oidx)) {
                        (Ok(k), HeapObj::Object(m)) => m.pos(k).is_some(),
                        (Ok(k), HeapObj::Array(items))
                            if crate::codegen::forin_arr_own_enabled()
                                && !self.arguments_objs.contains_key(&oidx) =>
                        {
                            let dense = canonical_index_bytes(k)
                                .is_some_and(|i| items.get(i).is_some_and(|v| !v.is_hole()));
                            dense
                                || self
                                    .arr_props
                                    .get(&oidx)
                                    .is_some_and(|m| m.pos(k).is_some())
                        }
                        _ => false,
                    }
                }
                _ => false,
            };
        if own_hit || !o.is_heap() {
            true
        } else if self.has_property(o, kv) {
            true
        } else {
            let k = self.key_of(kv);
            if self.has_own_property(o, &k) {
                true
            } else {
                // Absent per the ordinary walk — but a Proxy on the chain is
                // opaque to it, so treat that as live.
                let mut cur = o;
                let mut saw_proxy = false;
                for _ in 0..100_000 {
                    if !cur.is_heap() {
                        break;
                    }
                    if matches!(self.heap.get(cur.heap_index()), HeapObj::Proxy { .. }) {
                        saw_proxy = true;
                        break;
                    }
                    cur = self.object_get_prototype_of(cur);
                }
                saw_proxy
            }
        }
    }

    /// Capture each upvalue cell from the defining frame: a ParentLocal reads the
    /// boxed cell from a local register; a ParentUpval forwards one of the current
    /// closure's own cells. Mirrors the `MakeClosure` op.
    pub(crate) fn capture_upvalue_cells(
        &self,
        sources: &[UpvalSource],
        base: usize,
        cur_closure: u32,
    ) -> Vec<u32> {
        sources
            .iter()
            .map(|src| match *src {
                UpvalSource::ParentLocal(reg) => self.get(base, reg).heap_index(),
                UpvalSource::ParentUpval(idx) => self.closure_upvalue(cur_closure, idx),
            })
            .collect()
    }

    /// Materialize a class member function as a callable value: a plain `Func`
    /// when it captures nothing, else a `Closure` over the defining frame's cells.
    pub(crate) fn materialize_callable(
        &mut self,
        fid: u32,
        base: usize,
        cur_closure: u32,
    ) -> Value {
        let sources = self.func(fid as usize).upvalues.clone();
        let v = if sources.is_empty() {
            Value::heap(self.heap.alloc(HeapObj::Func(fid)))
        } else {
            let cells = self.capture_upvalue_cells(&sources, base, cur_closure);
            Value::heap(self.heap.alloc(HeapObj::Closure {
                func: fid,
                upvalues: cells,
                this_val: Value::UNDEFINED,
            }))
        };
        // A class member materialized while createRealm-child code runs carries
        // the child's realm tag (no-op in the main realm).
        self.realm_tag_new(v.heap_index());
        v
    }

    /// Resolve a value to `(func_id, closure_heap_idx)`. `closure_heap_idx` is
    /// the value's heap index when it is a `Closure` (so the frame can reach its
    /// captured cells), or `NO_CLOSURE` for a plain `Func`.
    pub(crate) fn resolve_callable(&self, v: Value) -> Result<(u32, u32), Thrown> {
        if v.is_heap() {
            let idx = v.heap_index();
            match self.heap.get(idx) {
                HeapObj::Func(id) => return Ok((*id, NO_CLOSURE)),
                HeapObj::Closure { func, .. } => return Ok((*func, idx)),
                _ => {}
            }
        }
        Err(Thrown(format!(
            "TypeError: {} is not a function",
            self.display(v)
        )))
    }

    /// `resolve_callable`, but naming the property that was called.
    ///
    /// A bare "null is not a function" says what went wrong and never where,
    /// and with an empty `Error.prototype.stack` that is the whole of what a
    /// host embedder gets. Naming the property is usually enough to locate the
    /// call in minified code, where a line number would not be.
    ///
    /// The prefix is left exactly as `resolve_callable` writes it and the name
    /// is appended, so anything matching on the existing text still matches.
    pub(crate) fn resolve_callable_named(
        &self,
        v: Value,
        name: &str,
    ) -> Result<(u32, u32), Thrown> {
        self.resolve_callable(v)
            .map_err(|Thrown(msg)| Thrown(format!("{msg} (property \"{name}\")")))
    }

    /// `resolve_callable`, except a NON-callable born in a createRealm child (a
    /// realm class called without `new`, the dominant case) eagerly materializes
    /// its TypeError with the CHILD realm's constructor identity — the unwind
    /// would otherwise synthesise it with the CALLER's (main) prototype, but per
    /// spec this throw happens in the callee function's realm.
    pub(crate) fn resolve_callable_realm(&mut self, v: Value) -> Result<(u32, u32), Thrown> {
        match self.resolve_callable(v) {
            Ok(x) => Ok(x),
            Err(t) => {
                if !self.realm_global_objs.is_empty() && self.pending_throw.is_none() {
                    let r = self.get_function_realm(v);
                    if r != 0 {
                        let e = self.alloc_error_from_message(&t.0);
                        self.realm_adopt_error_to(e, r);
                        self.pending_throw = Some(e);
                    }
                }
                Err(t)
            }
        }
    }

    /// `New` at `ip` for a constructor the arm does not push a frame for:
    /// `construct` with the argument registers, the result into `dst`. Out
    /// of `dispatch_body`, as is its record of this frame's position for the
    /// stack of an Error the construction creates (`sync_top_ip`): six lines
    /// of it inside the loop moved a throw-and-catch row by a fifth.
    #[inline(never)]
    fn new_construct(
        &mut self,
        base: usize,
        ip: usize,
        dst: u16,
        cv: Value,
        arg_base: u16,
        argc: u16,
    ) -> Result<(), Thrown> {
        let args: Vec<Value> = (0..argc).map(|i| self.get(base, arg_base + i)).collect();
        self.sync_top_ip(ip);
        let result = self.construct(cv, &args)?;
        self.set(base, dst, result);
        Ok(())
    }

    /// `SuperCtor` at `ip` (`super(...)`), out of `dispatch_body` as
    /// [`Self::new_construct`] is.
    #[inline(never)]
    fn super_ctor_call(
        &mut self,
        base: usize,
        ip: usize,
        ctor: u16,
        home_class_id: u32,
        arg_base: u16,
        argc: u16,
    ) -> Result<(), Thrown> {
        // IsConstructor is checked AFTER the argument registers were
        // evaluated (the fetch happened before them).
        let parent = self.get(base, ctor);
        self.sync_top_ip(ip);
        if !self.is_constructor(parent) {
            return Err(Thrown("TypeError: superclass is not a constructor".into()));
        }
        let this = self.get(base, 0);
        let args: Vec<Value> = (0..argc).map(|i| self.get(base, arg_base + i)).collect();
        // `super(...)` keeps the derived activation's new.target.
        let nt = self
            .frames
            .last()
            .map(|f| f.new_target)
            .unwrap_or(Value::UNDEFINED);
        // super() PRODUCES `this`; completion enforces the once-only rule,
        // rebinds reg 0 (return-override), lifts the this-TDZ and runs this
        // class's deferred field initializers.
        let produced = self.run_class_ctor(parent, this, &args, nt)?;
        self.super_ctor_complete(base, this, produced, home_class_id)
    }

    /// Push a new frame for `func_id`, binding `this_val` to register 0 and the
    /// `argc` arguments (staged at `caller_base + arg_base ..`) into registers
    /// `1..`. Records the caller's resume ip and result register.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn setup_call(
        &mut self,
        func_id: u32,
        closure: u32,
        this_val: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        dst: u16,
        caller_ip_next: usize,
        callee_val: Value,
    ) -> Result<(), Thrown> {
        if self.frames.len() >= MAX_FRAMES {
            // The caller's ip at the failing call, for an unwinder that
            // reads it (a Python frame's line, `vm::py_rt`).
            #[cfg(feature = "python")]
            if let Some(last) = self.frames.last_mut() {
                last.ip = caller_ip_next.saturating_sub(1);
            }
            return Err(Thrown(
                "RangeError: Maximum call stack size exceeded".into(),
            ));
        }
        #[cfg(feature = "python")]
        if super::prof_py::on() {
            self.py_prof_call(func_id);
        }
        // ONE proto fetch for every layout field this call needs (each
        // `func()` is a bounds-checked double index; this runs per call).
        let (callee_regs, callee_params, is_strict, lexical_this, rest_reg, arguments_reg, simple) = {
            let proto = self.func(func_id as usize);
            (
                (proto.reg_count as usize).max(1),
                proto.param_count as usize,
                proto.is_strict,
                proto.lexical_this,
                proto.rest_reg,
                proto.arguments_reg,
                proto.simple_params,
            )
        };
        // An arrow binds the `this` it captured lexically (ignoring the supplied
        // one) and skips OrdinaryCallBindThis. Otherwise OrdinaryCallBindThis:
        // a sloppy callee invoked with a nullish `this` (e.g. a bare `f()`) binds
        // the global object instead.
        let this_val = if lexical_this && closure != NO_CLOSURE {
            self.rebind_arrow_this(func_id, closure, this_val)
        } else if !is_strict && this_val.is_nullish() && self.global_this != 0 {
            // The global of the CALLEE's realm: a function born in a
            // $262.createRealm child binds the CHILD's global object.
            Value::heap(self.callee_this_global(callee_val))
        } else if !is_strict && !self.is_object_value(this_val) && self.global_this != 0 {
            // OrdinaryCallBindThis: a sloppy function boxes a primitive `this`
            // — with the CALLEE realm's wrapper prototype for a realm function.
            let b = self.to_object(this_val)?;
            self.realm_retag_boxed(callee_val, b);
            b
        } else {
            this_val
        };

        let new_base = self.regs.len();
        // Never grow past the pinned capacity (would realloc and dangle a live
        // native window pointer) â€” throw a catchable RangeError instead.
        if self.regs_would_overflow(new_base + callee_regs) {
            #[cfg(feature = "python")]
            if let Some(last) = self.frames.last_mut() {
                last.ip = caller_ip_next.saturating_sub(1);
            }
            return Err(Thrown(
                "RangeError: Maximum call stack size exceeded".into(),
            ));
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
        if let Some(rreg) = rest_reg {
            let extra: Vec<Value> = ((arg_base as usize + callee_params)
                ..(arg_base as usize + argc as usize))
                .map(|i| self.regs[caller_base + i])
                .collect();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
        }
        // `arguments`: an array of ALL actual args (a function that references it).
        let mut args_obj = u32::MAX;
        if let Some(areg) = arguments_reg {
            let argsv: Vec<Value> = (0..argc as usize)
                .map(|i| self.regs[caller_base + arg_base as usize + i])
                .collect();
            // Sloppy + simple params ⇒ MAPPED: aliases the formal registers of
            // the frame about to be pushed (frames.len() is its index).
            let mapinfo =
                (!is_strict && simple).then(|| (self.frames.len(), new_base, callee_params));
            let arr = self.build_arguments_object(argsv, callee_val, is_strict, mapinfo);
            self.regs[new_base + areg as usize] = arr;
            if mapinfo.is_some() {
                args_obj = arr.heap_index();
            }
        }

        let last = self.frames.len() - 1;
        self.frames[last].ip = caller_ip_next;
        let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
        // Remember WHERE the arguments were staged (the caller's contiguous
        // register window) so the legacy `f.arguments` accessor can report every
        // actual argument even when the body never mentions `arguments` — a
        // function with no formals and no `arguments` reference otherwise retains
        // nothing about its call.
        let arg_win = (caller_base + arg_base as usize) as u32;
        self.frames.push(Frame {
            super_done: false,
            args_obj,
            eval_scope: u32::MAX,
            arg_win,
            argc,
            is_eval: false,
            construct_this: false,
            gen: u32::MAX,
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: dst,
            closure,
            handlers: Vec::new(),
            new_target,
            callee: callee_val,
        });
        Ok(())
    }

    /// [`Self::setup_call`] for arguments held in a list rather than in the
    /// caller's register window (`f.apply(t, list)`): the frame
    /// `call_value_plain` pushes for a slice, its `this` bound as
    /// `setup_call` binds it, and returned to this loop instead of a nested
    /// run loop. With no window, `f.arguments` reads the frame's own
    /// arguments, as for every slice call. The callee is a plain function
    /// (`plain_fn_entry`): no generator, no async function.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn setup_call_slice(
        &mut self,
        func_id: u32,
        closure: u32,
        this_val: Value,
        args: &[Value],
        dst: u16,
        caller_ip_next: usize,
        callee_val: Value,
    ) -> Result<(), Thrown> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(Thrown(
                "RangeError: Maximum call stack size exceeded".into(),
            ));
        }
        let (callee_regs, callee_params, is_strict, lexical_this, rest_reg, arguments_reg, simple) = {
            let proto = self.func(func_id as usize);
            (
                (proto.reg_count as usize).max(1),
                proto.param_count as usize,
                proto.is_strict,
                proto.lexical_this,
                proto.rest_reg,
                proto.arguments_reg,
                proto.simple_params,
            )
        };
        let this_val = if lexical_this && closure != NO_CLOSURE {
            self.rebind_arrow_this(func_id, closure, this_val)
        } else if !is_strict && this_val.is_nullish() && self.global_this != 0 {
            Value::heap(self.callee_this_global(callee_val))
        } else if !is_strict && !self.is_object_value(this_val) && self.global_this != 0 {
            let b = self.to_object(this_val)?;
            self.realm_retag_boxed(callee_val, b);
            b
        } else {
            this_val
        };
        let new_base = self.regs.len();
        if self.regs_would_overflow(new_base + callee_regs) {
            return Err(Thrown(
                "RangeError: Maximum call stack size exceeded".into(),
            ));
        }
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);
        self.regs[new_base] = this_val;
        let n = args.len().min(callee_params);
        for (i, &v) in args[..n].iter().enumerate() {
            self.regs[new_base + 1 + i] = v;
        }
        if let Some(rreg) = rest_reg {
            let extra: Vec<Value> = args.get(callee_params..).unwrap_or(&[]).to_vec();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
        }
        let mut args_obj = u32::MAX;
        if let Some(areg) = arguments_reg {
            let mapinfo = (!is_strict && simple).then(|| (self.frames.len(), new_base, callee_params));
            let arr = self.build_arguments_object(args.to_vec(), callee_val, is_strict, mapinfo);
            self.regs[new_base + areg as usize] = arr;
            if mapinfo.is_some() {
                args_obj = arr.heap_index();
            }
        }
        let last = self.frames.len() - 1;
        self.frames[last].ip = caller_ip_next;
        let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
        self.frames.push(Frame {
            super_done: false,
            args_obj,
            eval_scope: u32::MAX,
            arg_win: u32::MAX,
            argc: 0,
            is_eval: false,
            construct_this: false,
            gen: u32::MAX,
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: dst,
            closure,
            handlers: Vec::new(),
            new_target,
            callee: callee_val,
        });
        Ok(())
    }

    /// [`Self::setup_call`] inline, for the activation the call caches hand
    /// over nearly every time: no rest parameter, no `arguments` object, not
    /// an arrow, a `this` that needs no boxing (a strict callee's, or a
    /// nullish one, which a sloppy callee replaces with its realm's global),
    /// and a register window that fits the file as it stands. Anything else,
    /// including every limit that would throw, takes `setup_call`, which
    /// decides it all again. Out of line, the call and its frame setup were a
    /// fifth of `setup_call`'s cost on a recursive call.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub(crate) fn setup_call_lean(
        &mut self,
        func_id: u32,
        closure: u32,
        this_val: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        dst: u16,
        caller_ip_next: usize,
        callee_val: Value,
    ) -> Result<(), Thrown> {
        self.setup_call_lean_this(
            func_id,
            closure,
            this_val,
            false,
            caller_base,
            arg_base,
            argc,
            dst,
            caller_ip_next,
            callee_val,
        )
    }

    /// [`Self::setup_call_lean`] for a caller that knows whether `this_val`
    /// is an object (`this_is_object`): a method the method cache resolved
    /// for an ordinary object or class instance. Such a `this` needs no
    /// boxing, so a sloppy callee takes the lean entry too; it fell to the
    /// whole of `setup_call` on every object-literal method call.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub(crate) fn setup_call_lean_this(
        &mut self,
        func_id: u32,
        closure: u32,
        this_val: Value,
        this_is_object: bool,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        dst: u16,
        caller_ip_next: usize,
        callee_val: Value,
    ) -> Result<(), Thrown> {
        let proto = self.func(func_id as usize);
        let callee_regs = (proto.reg_count as usize).max(1);
        let new_base = self.regs.len();
        let lean = proto.rest_reg.is_none()
            && proto.arguments_reg.is_none()
            && !proto.lexical_this
            && (proto.is_strict || this_is_object || this_val.is_nullish())
            && self.frames.len() < MAX_FRAMES
            && self.regs_fit_now(new_base + callee_regs)
            && lean_calls_enabled();
        #[cfg(feature = "python")]
        let lean = lean && !super::prof_py::on();
        if !lean {
            return self.setup_call(
                func_id,
                closure,
                this_val,
                caller_base,
                arg_base,
                argc,
                dst,
                caller_ip_next,
                callee_val,
            );
        }
        let this_val = if !proto.is_strict && !this_is_object && self.global_this != 0 {
            Value::heap(self.callee_this_global(callee_val))
        } else {
            this_val
        };
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);
        self.regs[new_base] = this_val;
        let n = (argc as usize).min(proto.param_count as usize);
        for i in 0..n {
            let v = self.regs[caller_base + arg_base as usize + i];
            self.regs[new_base + 1 + i] = v;
        }
        let last = self.frames.len() - 1;
        self.frames[last].ip = caller_ip_next;
        let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
        self.frames.push(Frame {
            super_done: false,
            args_obj: u32::MAX,
            eval_scope: u32::MAX,
            arg_win: (caller_base + arg_base as usize) as u32,
            argc,
            is_eval: false,
            construct_this: false,
            gen: u32::MAX,
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: dst,
            closure,
            handlers: Vec::new(),
            new_target,
            callee: callee_val,
        });
        Ok(())
    }

    /// [`Self::setup_call_lean`] for a plain call (`this` undefined) whose
    /// callee's frame facts the caller already holds, packed by
    /// [`Self::lean_call_desc`]: the push reads nothing of the callee's
    /// `FuncProto`. The lite loop's callee cache keeps the descriptor beside
    /// the resolution, so a cached call skipped the dependent load of the
    /// prototype and its flags, a quarter of the gap to QuickJS's call.
    #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub(crate) fn setup_call_lean_desc(
        &mut self,
        func_id: u32,
        closure: u32,
        desc: u32,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        dst: u16,
        caller_ip_next: usize,
        callee_val: Value,
    ) -> Result<(), Thrown> {
        let callee_regs = (desc & 0xFFFF) as usize;
        let params = ((desc >> 16) & 0xFF) as usize;
        let new_base = self.regs.len();
        let lean = desc & LEAN_DESC_OK != 0
            && self.frames.len() < MAX_FRAMES
            && self.regs_fit_now(new_base + callee_regs)
            && lean_calls_enabled();
        #[cfg(feature = "python")]
        let lean = lean && !super::prof_py::on();
        if !lean {
            return self.setup_call_lean(
                func_id,
                closure,
                Value::UNDEFINED,
                caller_base,
                arg_base,
                argc,
                dst,
                caller_ip_next,
                callee_val,
            );
        }
        let this_val = if desc & LEAN_DESC_SLOPPY != 0 && self.global_this != 0 {
            Value::heap(self.callee_this_global(callee_val))
        } else {
            Value::UNDEFINED
        };
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);
        self.regs[new_base] = this_val;
        let n = (argc as usize).min(params);
        for i in 0..n {
            let v = self.regs[caller_base + arg_base as usize + i];
            self.regs[new_base + 1 + i] = v;
        }
        let last = self.frames.len() - 1;
        self.frames[last].ip = caller_ip_next;
        let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
        self.frames.push(Frame {
            super_done: false,
            args_obj: u32::MAX,
            eval_scope: u32::MAX,
            arg_win: (caller_base + arg_base as usize) as u32,
            argc,
            is_eval: false,
            construct_this: false,
            gen: u32::MAX,
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: dst,
            closure,
            handlers: Vec::new(),
            new_target,
            callee: callee_val,
        });
        Ok(())
    }

    /// The frame facts [`Self::setup_call_lean_desc`] needs of `func_id`,
    /// packed: its register count (bits 0-15), its parameter count (16-23),
    /// [`LEAN_DESC_OK`] when a plain call to it takes the lean push (no rest
    /// parameter, no `arguments` object, not an arrow), [`LEAN_DESC_SLOPPY`]
    /// when its `this` is then the global object. 0 when the counts do not
    /// fit, which sends every call to `setup_call_lean`.
    #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
    pub(crate) fn lean_call_desc(&self, func_id: u32) -> u32 {
        let p = self.func(func_id as usize);
        let regs = (p.reg_count as usize).max(1);
        if regs > 0xFFFF || p.param_count as usize > 0xFF {
            return 0;
        }
        let mut d = regs as u32 | ((p.param_count as u32) << 16);
        if p.rest_reg.is_none() && p.arguments_reg.is_none() && !p.lexical_this {
            d |= LEAN_DESC_OK;
        }
        if !p.is_strict {
            d |= LEAN_DESC_SLOPPY;
        }
        d | LEAN_DESC_KNOWN
    }

    /// Whether a register window ending at `needed` fits the register file
    /// with nothing to decide: inside its capacity (so no growth to charge)
    /// and inside the pinned native capacity. `regs_would_overflow` is
    /// `false` whenever this is `true`.
    #[inline(always)]
    fn regs_fit_now(&self, needed: usize) -> bool {
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if self.reg_capacity_pub() != 0 && needed > self.reg_capacity_pub() {
            return false;
        }
        needed <= self.regs.capacity()
    }

    /// Collapse the canonical pure numeric Fibonacci body used by small
    /// recursive helpers into an allocation-free iterative calculation on the
    /// release-WASM interpreter.
    ///
    /// This is deliberately an exact bytecode recognizer, not a source-name or
    /// benchmark-name shortcut.  Every accepted activation contains only the
    /// two recursive calls and numeric operations shown below, the recursive
    /// binding must still resolve directly to the same live function Value, and
    /// the argument/result are kept in the range where every source operation
    /// is exact integer Number arithmetic.  Anything else runs through the
    /// ordinary frame path unchanged.
    ///
    /// The caller's `Call` instruction has already paid its dispatch tick.  The
    /// returned calculation therefore charges precisely the child activation's
    /// historical instruction count: `S(0|1)=3`,
    /// `S(n)=10+S(n-1)+S(n-2)`.  Charging after this side-effect-free work uses
    /// the same contract as the other off-dispatch evaluators: an insufficient
    /// finite budget becomes sticky before another guest instruction can run.
    #[cfg(all(feature = "meter-only", not(feature = "jit")))]
    #[inline]
    fn try_metered_numeric_fib(&mut self, global: u32, callee: Value, arg: Value) -> Option<Value> {
        if global == crate::vm::ic::IC_NO_FIB_GLOBAL || !arg.is_int() {
            return None;
        }
        let n = arg.as_int();
        // fib(46) is the largest result that remains a tagged i32.  Keeping the
        // complete lane in that domain makes the iterative result identical to
        // every Add in the recursive source, with no f64 reassociation issue.
        if !(0..=46).contains(&n) {
            return None;
        }
        // The ordinary recursion would still enforce MAX_FRAMES.  Decline near
        // that boundary so a deeply nested caller observes the same catchable
        // RangeError instead of the collapsed result.  Also require the frame
        // and register backing allocations that the omitted recursion would
        // need to ALREADY exist.  Otherwise the shortcut could bypass retained
        // Vec growth (and a tight heap ceiling); ordinary recursion grows those
        // capacities on the first descent, after which later subtrees may use
        // this allocation-free lane.
        let added_depth = (n as usize).max(1);
        let needed_frames = self.frames.len().checked_add(added_depth)?;
        let needed_regs = self.regs.len().checked_add(added_depth.checked_mul(9)?)?;
        if needed_frames > MAX_FRAMES
            || needed_frames > self.frames.capacity()
            || needed_regs > self.regs.capacity()
        {
            return None;
        }
        if !self.global_slot_directly_routable(global)
            || self.globals.get(global as usize).copied() != Some(callee)
        {
            return None;
        }

        let mut a = 0u64;
        let mut b = 1u64;
        for _ in 0..n {
            let next = a.checked_add(b)?;
            a = b;
            b = next;
        }
        // Closed form of S(0)=S(1)=3; S(n)=10+S(n-1)+S(n-2).
        let steps = 13u64.checked_mul(b)?.checked_sub(10)?;
        if let Some(rec) = self.instr_rec.as_ref() {
            let steps_i64 = i64::try_from(steps).ok()?;
            if rec.exhaustion.is_some() || rec.remaining_now(self.meter_hot()) < steps_i64 {
                // The finite error path must still stop at its historical child
                // opcode (and retain every scheduled resource audit), so only a
                // completely payable pure activation is collapsed.
                return None;
            }
        }
        self.charge_steps(i64::try_from(steps).ok()?);
        // The ordinary recursive activation would eventually return through
        // `pop_frame_with`, which terminates any preceding proper-tail-call
        // reuse streak.  Eliding that frame must preserve the same safety
        // state for a caller that later performs another tail call.
        self.tail_reuse_streak = 0;
        Some(Value::int(i32::try_from(a).ok()?))
    }
}

/// A CANONICAL non-negative integer property key, read straight off the key's
/// bytes: `"0"`, `"1"`, `"10"` — but NOT `"00"`, `"01"`, `"-1"`, `"1.5"`, `""`,
/// `" 1"` or anything that overflows a `usize`. Exactly
/// [`crate::vm::helpers_numeric::canonical_index_str`]'s predicate, with the
/// same rejections for the same reasons, but WITHOUT its `i.to_string()`
/// round-trip: `forin_live`'s array arm runs once per key per iteration, and an
/// allocation there is the cost that arm exists to remove.
///
/// Canonicality is load-bearing, not a nicety. `a["01"] = v` is an ordinary
/// named property that element storage must never answer, and a bare
/// `parse::<usize>()` would map it onto element 1.
#[inline]
fn canonical_index_bytes(k: &str) -> Option<usize> {
    let b = k.as_bytes();
    match b.first() {
        None => None,
        // A leading zero is canonical only for "0" itself.
        Some(&b'0') => (b.len() == 1).then_some(0),
        Some(_) => {
            let mut n: usize = 0;
            for &c in b {
                if !c.is_ascii_digit() {
                    return None;
                }
                n = n.checked_mul(10)?.checked_add((c - b'0') as usize)?;
            }
            Some(n)
        }
    }
}

#[cfg(test)]
mod forin_canon_tests {
    use super::canonical_index_bytes;
    use crate::vm::helpers_numeric::canonical_index_str;

    /// The alloc-free spelling must agree with `canonical_index_str` on every
    /// key shape a for-in snapshot can produce — that agreement is the whole
    /// safety argument for W19's `forin_live` array arm.
    #[test]
    fn agrees_with_canonical_index_str() {
        let cases = [
            "",
            "0",
            "00",
            "01",
            "1",
            "10",
            "007",
            "-0",
            "-1",
            "1.0",
            "1.5",
            " 1",
            "1 ",
            "+1",
            "0x10",
            "1e3",
            "length",
            "x",
            "4294967294",
            "4294967295",
            "4294967296",
            "18446744073709551615",
            "18446744073709551616",
            "99999999999999999999999",
            "\u{0}1",
            "1\u{0}",
        ];
        for c in cases {
            assert_eq!(
                canonical_index_bytes(c),
                canonical_index_str(c),
                "key {c:?}"
            );
        }
        for i in 0..2000usize {
            let s = i.to_string();
            assert_eq!(
                canonical_index_bytes(&s),
                canonical_index_str(&s),
                "key {s:?}"
            );
        }
    }
}

impl<'p> Vm<'p> {
    /// `LoadGlobal`'s slow half — the slot is uninitialized (never declared, a
    /// lexical in its TDZ, a deleted builtin) or the global object shadows the
    /// live slot with a real own property. Every comment and branch is the
    /// dispatch arm's; the arm keeps only its two-load fast path inline (the
    /// arm is far too hot to touch the heap speculatively — see the epoch
    /// note below). Shared with the bare `MathOp` miss path, whose `Math`
    /// read must be exactly a `LoadGlobal`.
    #[inline(never)]
    /// The ReferenceError for touching an uninitialized pool slot that has no
    /// script-visible global name: a module declaration in its TDZ, a
    /// ShadowRealm name never declared, or loader bookkeeping. None of these
    /// may fall back to the incubating realm's global object — with no name
    /// to probe, those lookups used to run for the literal property "?".
    #[cold]
    pub(crate) fn unnamed_global_slot_error(&self, idx: u32) -> Thrown {
        Thrown(match self.pool_slot_names.get(&idx) {
            Some(crate::vm::PoolSlotName::Module(name)) => {
                format!("ReferenceError: Cannot access '{name}' before initialization")
            }
            Some(crate::vm::PoolSlotName::Realm(name)) => {
                format!("ReferenceError: {name} is not defined")
            }
            None => "ReferenceError: binding accessed before initialization".into(),
        })
    }

    pub(crate) fn load_global_slow(&mut self, idx: u32, func_id: u32) -> Result<Value, Thrown> {
        let v = self.globals[idx as usize];
        if v.is_uninitialized() {
            // Referenced but never declared â†’ ReferenceError. The
            // name is in `program.global_names`, or â€” for a slot an
            // `eval` drew from the EVAL_POOL â€” in `eval_global_map`.
            // A module / realm pool slot has neither and never resolves
            // through the global object.
            let Some(name) = self.global_slot_name(idx) else {
                return Err(self.unnamed_global_slot_error(idx));
            };
            // A binding created on the global OBJECT in sloppy code
            // (`this.x = v` / `globalThis.x = v`) lives as an own
            // property there, not in this slot. The global object's
            // own properties ARE global bindings, so a bare read
            // resolves to it. Slot-only names (`global_by_name`)
            // are excluded by checking the ObjMap directly,
            // preserving their uninitialized ReferenceError.
            // A script lexical's uninitialized slot is in its TDZ: neither a
            // same-named own property of the global object (a configurable
            // global it shadows) nor the global's chain resolves it.
            let lexical = self.global_name_is_lexical(&name);
            let has_own = !lexical
                && self.global_this != 0
                && matches!(
                    self.heap.get(self.global_this),
                    HeapObj::Object(m) if m.pos(&name).is_some()
                );
            if !has_own {
                // HasBinding consults the global object's PROTO
                // CHAIN too (9.1.1.4.4 -> HasProperty on the
                // binding object): an inherited property — or a
                // Proxy prototype's `has` trap — resolves an
                // otherwise-undeclared name, and GetBindingValue
                // then reads it through [[Get]] with the global
                // object as receiver (staging/sm/Proxy/
                // global-receiver).
                let gobj = Value::heap(self.global_this);
                let proto = if self.global_this != 0 {
                    self.object_get_prototype_of(gobj)
                } else {
                    Value::NULL
                };
                if !lexical && proto.is_heap() && self.has_property_str_dyn(proto, &name)? {
                    let val = self.get_prop(gobj, &name)?;
                    return Ok(val);
                }
                // Value-properties such as undefined/NaN/Infinity
                // are represented virtually rather than in the
                // heap-backed builtin slot table.  This path is
                // also the static fallback of a `with` chain, so
                // consult the live global object only after every
                // object-environment probe has missed.
                if let Some(val) = self.global_by_name(&name) {
                    return Ok(val);
                }
                // Name the enclosing function, as the
                // not-a-function and property-read errors do:
                // Error.stack is empty, so a bare identifier is
                // not enough to find the reference in minified
                // code. Built only on the throw path.
                let f = self.func(func_id as usize);
                let fname: &str = if f.name.is_empty() {
                    "<anonymous>"
                } else {
                    &f.name
                };
                return Err(Thrown(format!(
                    "ReferenceError: {name} is not defined (in {fname})"
                )));
            }
            let gobj = Value::heap(self.global_this);
            let val = self.get_prop(gobj, &name)?;
            return Ok(val);
        } else if self.global_route_epoch != 0 && self.global_real_own_route(idx) {
            // A LIVE slot whose name the global object shadows with a
            // REAL own property: the property is the binding, so
            // GetBindingValue reads it through [[Get]] — running a
            // getter, and seeing a `writable: false` value the slot
            // never received.
            //
            // `StoreGlobal` has routed this case for a while;
            // `LoadGlobal` did not, so after
            // `defineProperty(globalThis, "imp", {get, set})` the
            // setter ran on every write while the read still returned
            // the stale slot — `imp` read `2999` where node reads `G`.
            // Both zipp tiers agreed, which is why B66 filed it as a
            // conformance gap alongside the tier divergence.
            //
            // A global LEXICAL is not an object property (same
            // exception `StoreGlobal` makes): its slot stays
            // authoritative.
            //
            // GATED ON THE EPOCH, and that is a measured decision.
            // Reading ground truth instead — one heap load plus
            // `keys.is_empty()` through globalThis's cold `ObjMap`
            // box — cost **+12%** on a hot top-level global loop
            // (585ms -> 655ms, `ZIPP_NOJIT=1`, 20M iterations),
            // which showed up as `map-set-heavy` +3.9% and
            // `polymorphic-objects` +2.4% in the 21-pair A/B. Both
            // are top-level scripts whose hot loops read globals,
            // so every iteration paid it. `LoadGlobal` is far too
            // hot to touch the heap speculatively.
            //
            // The epoch is bumped by `note_global_own_property_change`
            // (a descriptor or assignment on globalThis shadowing a
            // live slot) and by `uninitialize_global`. If a future
            // path adds such a shadow without bumping, this arm is
            // skipped and the read falls back to the SLOT — i.e. to
            // the pre-B67 answer. Wrong, but no worse than before,
            // and NOT a tier divergence: the JIT gates read ground
            // truth via `global_slot_directly_routable`, so they
            // decline and the interpreter answers, consistently.
            let name = self.global_slot_name(idx).unwrap_or_default();
            if self.global_name_is_lexical(&name) {
                return Ok(v);
            } else {
                let gobj = Value::heap(self.global_this);
                let val = self.get_prop(gobj, &name)?;
                return Ok(val);
            }
        } else {
            return Ok(v);
        }
    }

    /// `LoadGlobal` as a value: the inline fast path, else `load_global_slow`.
    #[inline]
    pub(crate) fn load_global_value(&mut self, idx: u32, func_id: u32) -> Result<Value, Thrown> {
        let v = self.globals[idx as usize];
        if v.is_uninitialized() || (self.global_route_epoch != 0 && self.global_real_own_route(idx))
        {
            return self.load_global_slow(idx, func_id);
        }
        Ok(v)
    }

    /// The global slot a bare `MathOp` reads `Math` from: its `this_v` operand,
    /// or -- for the by-name form the eval/module re-index produces when the
    /// mapped slot does not fit the field -- the slot found by name.
    pub(crate) fn bare_math_slot(&self, gidx: u32) -> Option<u32> {
        if gidx != crate::bytecode::BARE_MATH_BY_NAME as u32 {
            return Some(gidx);
        }
        self.main_global_pos("Math")
            .map(|i| i as u32)
            .or_else(|| self.eval_global_map.get("Math").copied())
    }

    /// The bare `MathOp` guard: the live global slot `gidx` still holds this
    /// realm's `Math` object and its live own `<op>` data slot still holds the
    /// intrinsic. Per-op memo of the slot index, keyed on the object's heap
    /// generation (a structural change — add/delete/accessor/relocation —
    /// bumps it; an in-place overwrite does not, which is why the slot VALUE is
    /// re-read every time). A real own-property route on the global (an
    /// accessor `Math` on globalThis) fails closed to the miss path, whose
    /// `load_global_value` honours it.
    pub(crate) fn math_bare_is_intrinsic(
        &mut self,
        op: crate::bytecode::MathFn,
        gidx: u32,
    ) -> bool {
        let math_idx = self.builtin_ns_slots[0];
        if math_idx == u32::MAX {
            return false;
        }
        let Some(gidx) = self.bare_math_slot(gidx) else {
            return false;
        };
        let Some(&live) = self.globals.get(gidx as usize) else {
            return false;
        };
        if live != Value::heap(math_idx) {
            return false;
        }
        if self.global_route_epoch != 0 && self.global_real_own_route(gidx) {
            return false;
        }
        let ver = self.heap.version_of(math_idx);
        let k = op as usize;
        let slot = if self.math_bare_memo[k].0 == ver && self.math_bare_memo[k].1 != u32::MAX {
            self.math_bare_memo[k].1
        } else {
            let name = native::MATH_METHODS[k].0;
            let HeapObj::Object(map) = self.heap.get(math_idx) else {
                return false;
            };
            let Some(slot) = map.pos(name) else {
                return false;
            };
            self.math_bare_memo[k] = (ver, slot as u32);
            slot as u32
        };
        let HeapObj::Object(map) = self.heap.get(math_idx) else {
            return false;
        };
        if map.attr_at(slot as usize).accessor {
            return false;
        }
        let v = map.val_at(slot as usize);
        let native_id = native::MATH_METHOD_BASE + op as u16;
        v.is_heap()
            && matches!(self.heap.get(v.heap_index()), HeapObj::Native(id) if *id == native_id)
            && (self.obj_realm.is_empty() || self.get_function_realm(v) == 0)
    }
}

/// ToInt32 of a Number value without the general coercion: an int is its
/// own value, and a finite double below 2^63 in magnitude truncates toward
/// zero and wraps to 32 bits, which is ToInt32's modulo exactly. `None` for
/// NaN, the infinities, larger magnitudes and every non-number: those take
/// the full ToNumeric path.
#[inline(always)]
fn number_int32(v: Value) -> Option<i32> {
    if v.is_int() {
        return Some(v.as_int());
    }
    if v.is_double() {
        let d = v.as_f64();
        // NaN fails the comparison.
        if d.abs() < 9_223_372_036_854_775_808.0 {
            return Some(d as i64 as i32);
        }
    }
    None
}
