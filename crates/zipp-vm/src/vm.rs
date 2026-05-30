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

use crate::bytecode::{Instr, Program};
use crate::heap::{Heap, HeapObj};
use crate::value::Value;

/// Hard cap on simultaneous JS frames. Throws a catchable RangeError rather
/// than growing unbounded. 100k is far beyond any non-pathological recursion
/// and the flat register file makes each frame cheap.
const MAX_FRAMES: usize = 100_000;

/// One activation record.
struct Frame {
    func: u32,
    /// Base index into `regs` of this frame's register window.
    base: usize,
    /// Instruction pointer within the function's code.
    ip: usize,
    /// Register in the *caller's* window that receives this call's result.
    ret_dst: u16,
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
        self.frames.push(Frame { func: 0, base, ip: 0, ret_dst: 0 });
        self.dispatch()
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

    /// The core dispatch loop. One loop drives all frames.
    fn dispatch(&mut self) -> Result<Value, Thrown> {
        loop {
            // Snapshot the current frame's coordinates. `ip` is advanced as a
            // local and written back only on frame transitions / loops.
            let frame_idx = self.frames.len() - 1;
            let func_id = self.frames[frame_idx].func;
            let base = self.frames[frame_idx].base;
            let mut ip = self.frames[frame_idx].ip;
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
                            parts.push(self.display(v));
                        }
                        self.output.push(parts.join(" "));
                        ip += 1;
                    }

                    Instr::Call { dst, callee, arg_base, argc } => {
                        let callee_v = self.get(base, callee);
                        if !callee_v.is_heap() {
                            return Err(Thrown(format!(
                                "TypeError: {} is not a function",
                                self.display(callee_v)
                            )));
                        }
                        let func_id_to_call = match self.heap.as_func(callee_v.heap_index()) {
                            Some(id) => id,
                            None => {
                                return Err(Thrown(format!(
                                    "TypeError: {} is not a function",
                                    self.display(callee_v)
                                )))
                            }
                        };
                        if self.frames.len() >= MAX_FRAMES {
                            return Err(Thrown(
                                "RangeError: Maximum call stack size exceeded".into(),
                            ));
                        }
                        let callee_proto = &self.program.functions[func_id_to_call as usize];
                        let callee_regs = callee_proto.reg_count as usize;
                        let callee_params = callee_proto.param_count as usize;

                        // The new frame's window starts at the current top of
                        // the register file.
                        let new_base = self.regs.len();
                        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);

                        // Copy arguments into the callee's parameter registers
                        // (params occupy registers 0..param_count). Missing
                        // args default to undefined; extra args are dropped.
                        let n = (argc as usize).min(callee_params);
                        for i in 0..n {
                            let v = self.regs[base + arg_base as usize + i];
                            self.regs[new_base + i] = v;
                        }

                        // Write back the caller's ip (points past the Call) so
                        // we resume correctly on return.
                        let last = self.frames.len() - 1;
                        self.frames[last].ip = ip + 1;
                        self.frames.push(Frame {
                            func: func_id_to_call,
                            base: new_base,
                            ip: 0,
                            ret_dst: dst,
                        });
                        break; // re-enter outer loop with the new frame
                    }

                    Instr::Return { src } => {
                        let v = self.regs[base + src as usize];
                        if self.pop_frame_with(v) {
                            return Ok(v);
                        }
                        break;
                    }
                    Instr::ReturnUndefined => {
                        if self.pop_frame_with(Value::UNDEFINED) {
                            return Ok(Value::UNDEFINED);
                        }
                        break;
                    }
                }
            }
        }
    }

    /// Pop the current frame, delivering `ret` into the caller's `ret_dst`.
    /// Returns `true` if this was the top-level frame (program done).
    #[inline]
    fn pop_frame_with(&mut self, ret: Value) -> bool {
        let finished = self.frames.pop().expect("frame underflow");
        // Shrink the register file back to the caller's window top.
        self.regs.truncate(finished.base);
        if let Some(caller) = self.frames.last() {
            let caller_base = caller.base;
            let dst = finished.ret_dst as usize;
            self.regs[caller_base + dst] = ret;
            false
        } else {
            true
        }
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

    /// Render a value the way `console.log` / string-concat does.
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
                HeapObj::Func(_) => "function".into(),
            }
        } else {
            "undefined".into()
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
