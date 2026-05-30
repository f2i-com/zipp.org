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

/// Sentinel `closure` value for a frame whose callee is a plain (capture-free)
/// function rather than a closure. Real heap indices are always `< u32::MAX`.
const NO_CLOSURE: u32 = u32::MAX;

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
}

pub struct Vm<'p> {
    program: &'p Program,
    heap: Heap,
    globals: Vec<Value>,
    /// One contiguous register file shared by all live frames; each frame owns
    /// the window `[base, base + reg_count)`.
    regs: Vec<Value>,
    frames: Vec<Frame>,
    /// Lines produced by `Print` (console.log), in order.
    pub output: Vec<String>,
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
        let globals = vec![Value::UNDEFINED; program.global_count as usize];
        let _ = &mut heap;
        Vm {
            program,
            heap,
            globals,
            regs: Vec::new(),
            frames: Vec::new(),
            output: Vec::new(),
        }
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
        self.regs.resize(top.reg_count as usize, Value::UNDEFINED);
        self.frames.push(Frame { func: 0, base, ip: 0, ret_dst: 0, closure: NO_CLOSURE });
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
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);
        self.regs[new_base] = this; // reg 0 = this
        let n = args.len().min(callee_params);
        for i in 0..n {
            self.regs[new_base + 1 + i] = args[i];
        }

        let stop_depth = self.frames.len();
        self.frames.push(Frame { func: func_id, base: new_base, ip: 0, ret_dst: 0, closure });
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

    /// The core dispatch loop. Drives frames until the frame that was current
    /// on entry returns — i.e. until `frames.len()` drops to `stop_depth`,
    /// whereupon that frame's return value is produced. `run()` passes 0 (drain
    /// everything); `call_value` passes the pre-call depth (run one nested call).
    fn run_loop(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
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

                    Instr::Jump { target } => {
                        ip = target as usize;
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

                    Instr::Print { arg_base, argc } => {
                        let mut parts = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            let v = self.get(base, arg_base + i);
                            parts.push(self.inspect(v));
                        }
                        self.output.push(parts.join(" "));
                        ip += 1;
                    }

                    Instr::MakeFunc { dst, func_id } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::Func(func_id)));
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
                        let key = self.program.functions[func_id as usize]
                            .string_constants[name as usize]
                            .clone();
                        // Builtin methods (array/string) execute inline and
                        // produce a result without pushing a frame.
                        if let Some(result) = self.try_builtin_method(recv, &key, base, arg_base, argc)? {
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        // Otherwise the property must resolve to a user function
                        // (a method on an object); call it with `this = recv`.
                        let prop = self.get_prop(recv, &key)?;
                        let (fid, closure) = self.resolve_callable(prop)?;
                        self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1)?;
                        break;
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

    // ── register access ──
    #[inline(always)]
    fn get(&self, base: usize, r: u16) -> Value {
        self.regs[base + r as usize]
    }
    #[inline(always)]
    fn set(&mut self, base: usize, r: u16, v: Value) {
        self.regs[base + r as usize] = v;
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
        self.frames.push(Frame { func: func_id, base: new_base, ip: 0, ret_dst: dst, closure });
        Ok(())
    }

    // ── property / index access ──

    fn get_index(&mut self, obj: Value, key: Value) -> Result<Value, Thrown> {
        if !obj.is_heap() {
            return Err(Thrown(format!(
                "TypeError: cannot read property of {}",
                self.display(obj)
            )));
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Array(items) => {
                if key.is_int() {
                    let i = key.as_int();
                    if i >= 0 && (i as usize) < items.len() {
                        return Ok(items[i as usize]);
                    }
                    return Ok(Value::UNDEFINED);
                }
                // Non-int key on an array: "length" or out of range → undefined.
                let k = self.display(key);
                if k == "length" {
                    return Ok(Value::int(items.len() as i32));
                }
                Ok(Value::UNDEFINED)
            }
            HeapObj::Object(map) => {
                let k = self.display(key);
                Ok(map.get(&k).unwrap_or(Value::UNDEFINED))
            }
            HeapObj::Str(s) => {
                if key.is_int() {
                    let i = key.as_int();
                    if i >= 0 {
                        if let Some(ch) = s.chars().nth(i as usize) {
                            let cs = ch.to_string();
                            return Ok(self.alloc_str(cs));
                        }
                    }
                    return Ok(Value::UNDEFINED);
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
                if key.is_int() {
                    let i = key.as_int();
                    if i >= 0 {
                        let i = i as usize;
                        if i >= items.len() {
                            items.resize(i + 1, Value::UNDEFINED);
                        }
                        items[i] = val;
                        return Ok(());
                    }
                }
                // Non-int / negative key falls back to nothing in this subset.
                Ok(())
            }
            HeapObj::Object(_) => {
                let k = self.display(key);
                if let HeapObj::Object(map) = self.heap.get_mut(idx) {
                    map.set(&k, val);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn get_prop(&mut self, obj: Value, key: &str) -> Result<Value, Thrown> {
        if !obj.is_heap() {
            // `"abc".length` and the like on primitive strings handled here too.
            return Ok(Value::UNDEFINED);
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Array(items) => {
                if key == "length" {
                    Ok(Value::int(items.len() as i32))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }
            HeapObj::Str(s) => {
                if key == "length" {
                    Ok(Value::int(s.chars().count() as i32))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }
            HeapObj::Object(map) => Ok(map.get(key).unwrap_or(Value::UNDEFINED)),
            _ => Ok(Value::UNDEFINED),
        }
    }

    fn set_prop(&mut self, obj: Value, key: &str, val: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: cannot set property of non-object".into()));
        }
        let idx = obj.heap_index();
        if let HeapObj::Object(map) = self.heap.get_mut(idx) {
            map.set(key, val);
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
        if !recv.is_heap() {
            return Ok(None);
        }
        let idx = recv.heap_index();
        let args: Vec<Value> = (0..argc)
            .map(|i| self.regs[base + arg_base as usize + i as usize])
            .collect();
        match self.heap.get(idx) {
            HeapObj::Array(_) => self.array_method(idx, name, &args),
            HeapObj::Str(_) => self.string_method(idx, name, &args),
            _ => Ok(None),
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
            "map" => {
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                let mut out = Vec::with_capacity(snapshot.len());
                for (i, v) in snapshot.iter().enumerate() {
                    let r = self.call_value(cb, Value::UNDEFINED, &[*v, Value::int(i as i32)])?;
                    out.push(r);
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "filter" => {
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                let mut out = Vec::new();
                for (i, v) in snapshot.iter().enumerate() {
                    let r = self.call_value(cb, Value::UNDEFINED, &[*v, Value::int(i as i32)])?;
                    if self.truthy(r) {
                        out.push(*v);
                    }
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "forEach" => {
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                for (i, v) in snapshot.iter().enumerate() {
                    self.call_value(cb, Value::UNDEFINED, &[*v, Value::int(i as i32)])?;
                }
                Ok(Some(Value::UNDEFINED))
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
                for (i, v) in iter {
                    acc = self.call_value(cb, Value::UNDEFINED, &[acc, *v, Value::int(i as i32)])?;
                }
                Ok(Some(acc))
            }
            "sort" => {
                let cmp = arg0;
                let mut snapshot = self.array_snapshot(idx);
                if cmp.is_heap() && self.heap.as_callable(cmp.heap_index()).is_some() {
                    // Comparator sort. insertion sort keeps it simple and stable
                    // and re-enters the VM for each comparison; fine for the
                    // corpus sizes. A faster merge sort is a later optimisation.
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

    /// Insertion sort driven by a JS comparator (`cmp(a,b) < 0` ⇒ a before b).
    fn comparator_sort(&mut self, items: &mut [Value], cmp: Value) -> Result<(), Thrown> {
        for i in 1..items.len() {
            let mut j = i;
            while j > 0 {
                let r = self.call_value(cmp, Value::UNDEFINED, &[items[j - 1], items[j]])?;
                if r.as_f64() > 0.0 {
                    items.swap(j - 1, j);
                    j -= 1;
                } else {
                    break;
                }
            }
        }
        Ok(())
    }

    fn string_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let s = match self.heap.get(idx) {
            HeapObj::Str(s) => s.clone(),
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
            if let (Some(sa), Some(sb)) =
                (self.heap.as_str(a.heap_index()), self.heap.as_str(b.heap_index()))
            {
                return sa == sb;
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
        // String concatenation if either side is a heap string.
        if va.is_heap() || vb.is_heap() {
            let sa = self.display(va);
            let sb = self.display(vb);
            let mut s = String::with_capacity(sa.len() + sb.len());
            s.push_str(&sa);
            s.push_str(&sb);
            return Ok(self.alloc_str(s));
        }
        Ok(Value::num(self.to_number(va)? + self.to_number(vb)?))
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
            if let (Some(sa), Some(sb)) =
                (self.heap.as_str(va.heap_index()), self.heap.as_str(vb.heap_index()))
            {
                return sa == sb;
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
        if let Some(s) = self.heap.as_str(v.heap_index()) {
            return !s.is_empty();
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
        if let Some(s) = self.heap.as_str(v.heap_index()) {
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
                HeapObj::Str(s) => s.clone(),
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
                HeapObj::Str(s) => return s.clone(), // top-level strings unquoted
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
            HeapObj::Str(s) => format!("'{s}'"),
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

/// Normalise a (possibly negative) slice index into `[0, len]`. Negative
/// indices count from the end; out-of-range clamps. Matches JS slice/substring.
fn norm_index(i: i32, len: i32) -> i32 {
    let v = if i < 0 { len + i } else { i };
    v.clamp(0, len)
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
    // Integer-valued doubles print without a decimal point (JS semantics).
    if n.fract() == 0.0 && n.abs() < 1e21 {
        return format!("{}", n as i64);
    }
    let mut s = format!("{n}");
    if s.contains('e') {
        // JS uses e+/e- exponent formatting; Rust already does e.g. 1e21.
        s = s.replace('e', "e+").replace("e+-", "e-");
    }
    s
}
