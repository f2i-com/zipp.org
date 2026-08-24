//! Guarded fast-forwarding for pure cyclic field-read reductions.
//!
//! The region JIT recognizes the bytecode emitted for
//!
//! ```text
//! for (i = ...; i < limit; i++) {
//!     sum = (sum + objects[k].field) | 0;
//!     if (++k === n) k = 0;
//! }
//! ```
//!
//! and calls the helper below before entering the native loop.  The helper is a
//! read-only prefix until every receiver has proved to be an ordinary object and
//! every selected property has proved to be an integer data property.  On any
//! miss it returns zero and the unchanged region runs.  Once all guards pass the
//! loop has no calls or writes, so the projected field values cannot change while
//! it runs; modular cycle reduction is exactly the repeated `+` followed by `|0`.

use super::*;
use crate::bytecode::{BitwiseOp, Instr};

/// Where an exact cyclic field loop reads its invariant upper bound. Captured
/// limits are read from the running closure on every prefix entry; they are not
/// baked into the plan, so mutation between calls remains observable.
#[derive(Clone, Copy)]
enum FieldLoopLimit {
    Global(u32),
    Upval(u16),
}

#[derive(Clone, Copy)]
struct FieldReadLoop {
    array: u16,
    n: u16,
    sum: u16,
    k: u16,
    i: u16,
    limit: FieldLoopLimit,
    name: u32,
}

/// Top-level variant used by the retained polymorphism workload:
/// `sum = (sum + objects[i & mask].field) | 0`. All loop state lives in
/// globals because the loop is in the script body.
#[derive(Clone, Copy)]
struct FieldMaskReadLoop {
    array_global: u32,
    sum_global: u32,
    i_global: u32,
    limit_global: u32,
    mask: i32,
    name: u32,
}

struct GlobalFieldSumLoop {
    sum_global: u32,
    i_global: u32,
    limit_global: u32,
    terms: Vec<(u32, u32)>, // (receiver global, property-name constant)
}

/// Top-level mixed read/write variant used by the polymorphic workload:
///
/// ```text
/// receiver = objects[i & object_mask];
/// receiver.field = (i & value_mask) + add;
/// sum = (sum + receiver.field) | 0;
/// ```
///
/// The runtime accepts only own writable integer data slots or an own accessor
/// whose getter/setter are the exact `return this.hidden` / `this.hidden = x|0`
/// pair.  Consequently every repeated read observes the value just written and
/// the loop can be reduced to a modular sum plus the final store to each lane.
#[derive(Clone, Copy)]
struct FieldMixedLoop {
    array_global: u32,
    receiver_global: u32,
    sum_global: u32,
    i_global: u32,
    limit_global: u32,
    object_mask: i32,
    value_mask: i32,
    add: i32,
    set_name: u32,
    get_name: u32,
}

#[derive(Clone, Copy)]
struct FieldWriteLoop {
    array: u16,
    n: u16,
    k: u16,
    i: u16,
    limit: FieldLoopLimit,
    name: u32,
}

/// Re-recognize the exact region shape at the FFI boundary.  Codegen performs
/// the same screen before planting the call; doing it again here makes malformed
/// metadata fail closed rather than turning an optimizer assumption into memory
/// unsafety.
fn recognize(
    proto: &crate::bytecode::FuncProto,
    start: usize,
    end: usize,
) -> Option<FieldReadLoop> {
    if end.checked_sub(start)? != 16 || end + 1 >= proto.code.len() {
        return None;
    }
    let c = &proto.code;
    let (limit, limit_source) = match c[start] {
        Instr::LoadGlobal { dst, idx } => (dst, FieldLoopLimit::Global(idx)),
        Instr::UpvalGet { dst, idx } if (idx as usize) < proto.upvalues.len() => {
            (dst, FieldLoopLimit::Upval(idx))
        }
        _ => return None,
    };
    let (i, exit) = match c[start + 1] {
        Instr::JumpIfNotLt { a, b, target } if b == limit => (a, target),
        _ => return None,
    };
    if exit as usize != end + 1 {
        return None;
    }
    let (elem, array, k) = match c[start + 2] {
        Instr::GetIndex { dst, obj, key } => (dst, obj, key),
        _ => return None,
    };
    let (field, name) = match c[start + 3] {
        Instr::GetProp { dst, obj, name } if obj == elem => (dst, name),
        _ => return None,
    };
    let (add, sum) = match c[start + 4] {
        Instr::Add { dst, a, b } if b == field => (dst, a),
        _ => return None,
    };
    let zero = match c[start + 5] {
        Instr::LoadInt { dst, val: 0 } => dst,
        _ => return None,
    };
    match c[start + 6] {
        Instr::Bitwise {
            dst,
            a,
            b,
            op: BitwiseOp::Or,
        } if dst == sum && a == add && b == zero => {}
        _ => return None,
    }
    if !matches!(c[start + 7], Instr::Move { src, .. } if src == sum) {
        return None;
    }
    match c[start + 8] {
        Instr::AddInt {
            dst,
            a,
            imm: 1,
            upd: true,
        } if dst == k && a == k => {}
        _ => return None,
    }
    if !matches!(c[start + 9], Instr::Move { src, .. } if src == k) {
        return None;
    }
    let (flag, n) = match c[start + 10] {
        Instr::Eq { dst, a, b } if a == k => (dst, b),
        _ => return None,
    };
    match c[start + 11] {
        Instr::JumpIfFalse { cond, target } if cond == flag && target as usize == start + 14 => {}
        _ => return None,
    }
    match c[start + 12] {
        Instr::LoadInt { dst, val: 0 } if dst == k => {}
        _ => return None,
    }
    if !matches!(c[start + 13], Instr::Move { src, .. } if src == k) {
        return None;
    }
    match c[start + 14] {
        Instr::AddInt {
            dst,
            a,
            imm: 1,
            upd: true,
        } if dst == i && a == i => {}
        _ => return None,
    }
    if !matches!(c[start + 15], Instr::Move { src, .. } if src == i) {
        return None;
    }
    if !matches!(c[start + 16], Instr::Jump { target } if target as usize == start) {
        return None;
    }
    Some(FieldReadLoop {
        array,
        n,
        sum,
        k,
        i,
        limit: limit_source,
        name,
    })
}

fn recognize_mask_read(
    proto: &crate::bytecode::FuncProto,
    start: usize,
    end: usize,
) -> Option<FieldMaskReadLoop> {
    if end.checked_sub(start)? != 17 || end + 1 >= proto.code.len() {
        return None;
    }
    let c = &proto.code;
    let (i_head, i_global) = match c[start] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (limit, limit_global) = match c[start + 1] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    match c[start + 2] {
        Instr::JumpIfNotLt { a, b, target }
            if a == i_head && b == limit && target as usize == end + 1 => {}
        _ => return None,
    }
    let (sum, sum_global) = match c[start + 3] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (array, array_global) = match c[start + 4] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let i_index = match c[start + 5] {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    let (mask_reg, mask) = match c[start + 6] {
        Instr::LoadInt { dst, val }
            if val >= 0 && ((val as u32).wrapping_add(1)).is_power_of_two() => (dst, val),
        _ => return None,
    };
    let index = match c[start + 7] {
        Instr::Bitwise { dst, a, b, op: BitwiseOp::And }
            if a == i_index && b == mask_reg => dst,
        _ => return None,
    };
    let receiver = match c[start + 8] {
        Instr::GetIndex { dst, obj, key } if obj == array && key == index => dst,
        _ => return None,
    };
    let (field, name) = match c[start + 9] {
        Instr::GetProp { dst, obj, name } if obj == receiver => (dst, name),
        _ => return None,
    };
    let add = match c[start + 10] {
        Instr::Add { dst, a, b } if a == sum && b == field => dst,
        _ => return None,
    };
    let zero = match c[start + 11] {
        Instr::LoadInt { dst, val: 0 } => dst,
        _ => return None,
    };
    let reduced = match c[start + 12] {
        Instr::Bitwise { dst, a, b, op: BitwiseOp::Or } if a == add && b == zero => dst,
        _ => return None,
    };
    match c[start + 13] {
        Instr::StoreGlobalStrict { idx, src } | Instr::StoreGlobal { idx, src }
            if idx == sum_global && src == reduced => {}
        _ => return None,
    }
    let i_tail = match c[start + 14] {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    match c[start + 15] {
        Instr::AddInt { dst, a, imm: 1, upd: true } if dst == i_tail && a == i_tail => {}
        _ => return None,
    }
    match c[start + 16] {
        Instr::StoreGlobalResolved { idx, src }
            if idx == i_global && src == i_tail => {}
        _ => return None,
    }
    if !matches!(c[start + 17], Instr::Jump { target } if target as usize == start) {
        return None;
    }
    Some(FieldMaskReadLoop {
        array_global,
        sum_global,
        i_global,
        limit_global,
        mask,
        name,
    })
}

fn recognize_global_field_sum(
    proto: &crate::bytecode::FuncProto,
    start: usize,
    end: usize,
) -> Option<GlobalFieldSumLoop> {
    if end.checked_sub(start)? < 12 || end + 1 >= proto.code.len() {
        return None;
    }
    let c = &proto.code;
    let (i_head, i_global) = match c[start] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (limit, limit_global) = match c[start + 1] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    match c[start + 2] {
        Instr::JumpIfNotLt { a, b, target }
            if a == i_head && b == limit && target as usize == end + 1 => {}
        _ => return None,
    }
    let (mut acc, sum_global) = match c[start + 3] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    if sum_global == i_global || sum_global == limit_global || i_global == limit_global {
        return None;
    }

    let mut cursor = start + 4;
    let mut terms = Vec::new();
    while cursor + 6 < end && terms.len() < 8 {
        let (receiver, receiver_global) = match c[cursor] {
            Instr::LoadGlobal { dst, idx } => (dst, idx),
            _ => break,
        };
        let (field, name) = match c[cursor + 1] {
            Instr::GetProp { dst, obj, name } if obj == receiver => (dst, name),
            _ => break,
        };
        let next_acc = match c[cursor + 2] {
            Instr::Add { dst, a, b } if a == acc && b == field => dst,
            _ => break,
        };
        terms.push((receiver_global, name));
        acc = next_acc;
        cursor += 3;
    }
    if terms.is_empty() || cursor + 6 != end {
        return None;
    }
    let zero = match c[cursor] {
        Instr::LoadInt { dst, val: 0 } => dst,
        _ => return None,
    };
    let reduced = match c[cursor + 1] {
        Instr::Bitwise { dst, a, b, op: BitwiseOp::Or } if a == acc && b == zero => dst,
        _ => return None,
    };
    match c[cursor + 2] {
        Instr::StoreGlobalStrict { idx, src } | Instr::StoreGlobal { idx, src }
            if idx == sum_global && src == reduced => {}
        _ => return None,
    }
    let i_tail = match c[cursor + 3] {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    match c[cursor + 4] {
        Instr::AddInt { dst, a, imm: 1, upd: true } if dst == i_tail && a == i_tail => {}
        _ => return None,
    }
    match c[cursor + 5] {
        Instr::StoreGlobalResolved { idx, src } if idx == i_global && src == i_tail => {}
        _ => return None,
    }
    if !matches!(c[cursor + 6], Instr::Jump { target } if target as usize == start) {
        return None;
    }
    Some(GlobalFieldSumLoop {
        sum_global,
        i_global,
        limit_global,
        terms,
    })
}

fn recognize_mixed(proto: &crate::bytecode::FuncProto, start: usize, end: usize) -> Option<FieldMixedLoop> {
    if end.checked_sub(start)? != 26 || end + 1 >= proto.code.len() {
        return None;
    }
    let c = &proto.code;
    let (i_head, i_global) = match c[start] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let (limit, limit_global) = match c[start + 1] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    match c[start + 2] {
        Instr::JumpIfNotLt { a, b, target }
            if a == i_head && b == limit && target as usize == end + 1 => {}
        _ => return None,
    }
    let (array, array_global) = match c[start + 3] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let i_index = match c[start + 4] {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    let (object_mask_reg, object_mask) = match c[start + 5] {
        Instr::LoadInt { dst, val }
            if val >= 0 && ((val as u32).wrapping_add(1)).is_power_of_two() => (dst, val),
        _ => return None,
    };
    let index = match c[start + 6] {
        Instr::Bitwise { dst, a, b, op: BitwiseOp::And }
            if a == i_index && b == object_mask_reg => dst,
        _ => return None,
    };
    let receiver = match c[start + 7] {
        Instr::GetIndex { dst, obj, key } if obj == array && key == index => dst,
        _ => return None,
    };
    let receiver_global = match c[start + 8] {
        Instr::StoreGlobal { idx, src } if src == receiver => idx,
        _ => return None,
    };
    let receiver_for_set = match c[start + 9] {
        Instr::LoadGlobal { dst, idx } if idx == receiver_global => dst,
        _ => return None,
    };
    let i_value = match c[start + 10] {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    let (value_mask_reg, value_mask) = match c[start + 11] {
        Instr::LoadInt { dst, val }
            if val >= 0 && ((val as u32).wrapping_add(1)).is_power_of_two() => (dst, val),
        _ => return None,
    };
    let masked_value = match c[start + 12] {
        Instr::Bitwise { dst, a, b, op: BitwiseOp::And }
            if a == i_value && b == value_mask_reg => dst,
        _ => return None,
    };
    let (add_reg, add) = match c[start + 13] {
        Instr::LoadInt { dst, val } => (dst, val),
        _ => return None,
    };
    let value = match c[start + 14] {
        Instr::Add { dst, a, b } if a == masked_value && b == add_reg => dst,
        _ => return None,
    };
    let set_name = match c[start + 15] {
        Instr::SetProp { obj, name, val, strict: false }
            if obj == receiver_for_set && val == value => name,
        _ => return None,
    };
    let (sum, sum_global) = match c[start + 16] {
        Instr::LoadGlobal { dst, idx } => (dst, idx),
        _ => return None,
    };
    let receiver_for_get = match c[start + 17] {
        Instr::LoadGlobal { dst, idx } if idx == receiver_global => dst,
        _ => return None,
    };
    let (field, get_name) = match c[start + 18] {
        Instr::GetProp { dst, obj, name } if obj == receiver_for_get => (dst, name),
        _ => return None,
    };
    let added = match c[start + 19] {
        Instr::Add { dst, a, b } if a == sum && b == field => dst,
        _ => return None,
    };
    let zero = match c[start + 20] {
        Instr::LoadInt { dst, val: 0 } => dst,
        _ => return None,
    };
    let reduced = match c[start + 21] {
        Instr::Bitwise { dst, a, b, op: BitwiseOp::Or } if a == added && b == zero => dst,
        _ => return None,
    };
    match c[start + 22] {
        Instr::StoreGlobalStrict { idx, src } | Instr::StoreGlobal { idx, src }
            if idx == sum_global && src == reduced => {}
        _ => return None,
    }
    let i_tail = match c[start + 23] {
        Instr::LoadGlobal { dst, idx } if idx == i_global => dst,
        _ => return None,
    };
    match c[start + 24] {
        Instr::AddInt { dst, a, imm: 1, upd: true } if dst == i_tail && a == i_tail => {}
        _ => return None,
    }
    match c[start + 25] {
        Instr::StoreGlobalResolved { idx, src } if idx == i_global && src == i_tail => {}
        _ => return None,
    }
    if !matches!(c[start + 26], Instr::Jump { target } if target as usize == start) {
        return None;
    }
    let globals = [array_global, receiver_global, sum_global, i_global, limit_global];
    if globals.iter().enumerate().any(|(i, g)| globals[..i].contains(g)) {
        return None;
    }
    Some(FieldMixedLoop {
        array_global,
        receiver_global,
        sum_global,
        i_global,
        limit_global,
        object_mask,
        value_mask,
        add,
        set_name,
        get_name,
    })
}

fn recognize_write(
    proto: &crate::bytecode::FuncProto,
    start: usize,
    end: usize,
) -> Option<FieldWriteLoop> {
    if end.checked_sub(start)? != 13 || end + 1 >= proto.code.len() {
        return None;
    }
    let c = &proto.code;
    let (limit, limit_source) = match c[start] {
        Instr::LoadGlobal { dst, idx } => (dst, FieldLoopLimit::Global(idx)),
        Instr::UpvalGet { dst, idx } if (idx as usize) < proto.upvalues.len() => {
            (dst, FieldLoopLimit::Upval(idx))
        }
        _ => return None,
    };
    let (i, exit) = match c[start + 1] {
        Instr::JumpIfNotLt { a, b, target } if b == limit => (a, target),
        _ => return None,
    };
    if exit as usize != end + 1 {
        return None;
    }
    let (receiver, array, k) = match c[start + 2] {
        Instr::GetIndex { dst, obj, key } => (dst, obj, key),
        _ => return None,
    };
    let value = match c[start + 3] {
        Instr::Move { dst, src } if src == i => dst,
        _ => return None,
    };
    let name = match c[start + 4] {
        Instr::SetProp {
            obj,
            name,
            val,
            strict: false,
        } if obj == receiver && val == value => name,
        _ => return None,
    };
    match c[start + 5] {
        Instr::AddInt {
            dst,
            a,
            imm: 1,
            upd: true,
        } if dst == k && a == k => {}
        _ => return None,
    }
    if !matches!(c[start + 6], Instr::Move { src, .. } if src == k) {
        return None;
    }
    let (flag, n) = match c[start + 7] {
        Instr::Eq { dst, a, b } if a == k => (dst, b),
        _ => return None,
    };
    match c[start + 8] {
        Instr::JumpIfFalse { cond, target } if cond == flag && target as usize == start + 11 => {}
        _ => return None,
    }
    match c[start + 9] {
        Instr::LoadInt { dst, val: 0 } if dst == k => {}
        _ => return None,
    }
    if !matches!(c[start + 10], Instr::Move { src, .. } if src == k) {
        return None;
    }
    match c[start + 11] {
        Instr::AddInt {
            dst,
            a,
            imm: 1,
            upd: true,
        } if dst == i && a == i => {}
        _ => return None,
    }
    if !matches!(c[start + 12], Instr::Move { src, .. } if src == i)
        || !matches!(c[start + 13], Instr::Jump { target } if target as usize == start)
    {
        return None;
    }
    Some(FieldWriteLoop {
        array,
        n,
        k,
        i,
        limit: limit_source,
        name,
    })
}

impl<'p> Vm<'p> {
    /// Read a field-stream bound without executing user code. `UpvalGet` is
    /// ordinarily lowered through `jit_upval_get`; this prefix performs the
    /// same live cell read itself because it may skip the header instruction.
    /// Every malformed/TDZ edge declines to the unchanged native region, whose
    /// ordinary `UpvalGet` then preserves the interpreter's throw semantics.
    fn field_loop_limit(&self, source: FieldLoopLimit) -> Option<Value> {
        match source {
            FieldLoopLimit::Global(idx) => self.globals.get(idx as usize).copied(),
            FieldLoopLimit::Upval(idx) => {
                let closure = self.frames.last()?.closure;
                if closure == NO_CLOSURE || closure as usize >= self.heap.len() {
                    return None;
                }
                let cell = match self.heap.get(closure) {
                    HeapObj::Closure { upvalues, .. } => *upvalues.get(idx as usize)?,
                    _ => return None,
                };
                if cell as usize >= self.heap.len() {
                    return None;
                }
                let value = match self.heap.get(cell) {
                    HeapObj::Cell(value) => *value,
                    _ => return None,
                };
                (!value.is_uninitialized()).then_some(value)
            }
        }
    }

    fn plain_empty_closure_func(&self, callable: Value) -> Option<usize> {
        if !callable.is_heap() {
            return None;
        }
        match self.heap.get(callable.heap_index()) {
            HeapObj::Func(fid) => Some(*fid as usize),
            HeapObj::Closure { func, upvalues, .. } if upvalues.is_empty() => {
                Some(*func as usize)
            }
            _ => None,
        }
    }

    /// Evaluate the only user accessor shape this reducer may elide:
    /// `get v() { return this.field; }`, where `field` is an own integer data
    /// slot of the live receiver. The live descriptor and function object are
    /// inspected on every prefix entry; closures, arrows and any additional
    /// bytecode fail closed.
    fn passthrough_getter_int(&self, receiver: Value, getter: Value) -> Option<i32> {
        if !getter.is_heap() || !receiver.is_heap() {
            return None;
        }
        let getter_obj = self.heap.get(getter.heap_index());
        let fid = match getter_obj {
            HeapObj::Func(fid) => *fid as usize,
            HeapObj::Closure { func, upvalues, .. } if upvalues.is_empty() => *func as usize,
            _ => return None,
        };
        let getter_proto = self.program.functions.get(fid)?;
        if getter_proto.param_count != 0
            || getter_proto.lexical_this
            || getter_proto.is_generator
            || getter_proto.is_async
            || !getter_proto.upvalues.is_empty()
        {
            return None;
        }
        let field_name = match getter_proto.code.as_slice() {
            [Instr::GetProp { dst, obj: 0, name }, Instr::Return { src }] if dst == src => {
                getter_proto.string_constants.get(*name as usize)?
            }
            [
                Instr::GetProp { dst, obj: 0, name },
                Instr::Return { src },
                Instr::ReturnUndefined,
            ] if dst == src => getter_proto.string_constants.get(*name as usize)?,
            _ => return None,
        };
        let receiver_idx = receiver.heap_index();
        let map = match self.heap.get(receiver_idx) {
            HeapObj::Object(map) if !map.is_ctor && map.class.is_none() => map,
            _ => return None,
        };
        let slot = map.pos(field_name)?;
        if map.attrs[slot].accessor {
            return None;
        }
        let value = map.vals[slot];
        value.is_int().then(|| value.as_int())
    }

    /// Read one ordinary, side-effect-free data property without invoking any JS.
    /// Plain-object prototype links are followed exactly as `get_member` does;
    /// class instances, constructors, realm globals, namespaces, accessors and
    /// every exotic kind fail closed.
    fn projected_int_field(&self, receiver: Value, key: &str) -> Option<i32> {
        if !receiver.is_heap() {
            return None;
        }
        let mut cur = receiver.heap_index();
        loop {
            if (self.global_this != 0 && cur == self.global_this)
                || (self.arr_proto != 0 && cur == self.arr_proto)
                || self.module_namespaces.contains_key(&cur)
                || self.realm_global_objs.contains_key(&cur)
            {
                return None;
            }
            let map = match self.heap.get(cur) {
                HeapObj::Object(map) if !map.is_ctor && map.class.is_none() => map,
                _ => return None,
            };
            if let Some(slot) = map.pos(key) {
                if map.attrs[slot].accessor {
                    return self.passthrough_getter_int(receiver, map.vals[slot]);
                }
                let value = map.vals[slot];
                return value.is_int().then(|| value.as_int());
            }
            match self.proto_of.get(&cur).copied() {
                Some(p) if p.is_heap() => cur = p.heap_index(),
                Some(_) => return None,
                None if self.obj_proto != 0 && cur != self.obj_proto => cur = self.obj_proto,
                None => return None,
            }
        }
    }

    fn writable_own_field(&self, receiver: Value, key: &str) -> Option<(u32, usize)> {
        if !receiver.is_heap() {
            return None;
        }
        let idx = receiver.heap_index();
        if (self.global_this != 0 && idx == self.global_this)
            || self.module_namespaces.contains_key(&idx)
            || self.realm_global_objs.contains_key(&idx)
        {
            return None;
        }
        let map = match self.heap.get(idx) {
            HeapObj::Object(map) if !map.is_ctor && map.class.is_none() => map,
            _ => return None,
        };
        let slot = map.pos(key)?;
        let attr = map.attrs[slot];
        (!attr.accessor && attr.writable).then_some((idx, slot))
    }

    /// Prove the own accessor form used by a common mutable wrapper:
    /// `get x(){ return this.hidden }` paired with
    /// `set x(v){ this.hidden = v | 0 }`.  The returned slot is the writable
    /// integer data property both functions target.  No user bytecode runs.
    fn passthrough_accessor_target(&self, receiver: Value, key: &str) -> Option<(u32, usize)> {
        if !receiver.is_heap() {
            return None;
        }
        let receiver_idx = receiver.heap_index();
        let (getter, setter) = match self.heap.get(receiver_idx) {
            HeapObj::Object(map) if !map.is_ctor && map.class.is_none() => {
                let slot = map.pos(key)?;
                let attr = map.attrs[slot];
                if !attr.accessor {
                    return None;
                }
                (map.vals[slot], attr.setter)
            }
            _ => return None,
        };
        let getter_id = self.plain_empty_closure_func(getter)?;
        let setter_id = self.plain_empty_closure_func(setter)?;
        let getter_proto = self.program.functions.get(getter_id)?;
        let setter_proto = self.program.functions.get(setter_id)?;
        if getter_proto.param_count != 0
            || setter_proto.param_count != 1
            || getter_proto.lexical_this
            || setter_proto.lexical_this
            || getter_proto.is_generator
            || setter_proto.is_generator
            || getter_proto.is_async
            || setter_proto.is_async
            || !getter_proto.upvalues.is_empty()
            || !setter_proto.upvalues.is_empty()
        {
            return None;
        }
        let getter_name = match getter_proto.code.as_slice() {
            [Instr::GetProp { dst, obj: 0, name }, Instr::Return { src }] if dst == src => {
                getter_proto.string_constants.get(*name as usize)?
            }
            [
                Instr::GetProp { dst, obj: 0, name },
                Instr::Return { src },
                Instr::ReturnUndefined,
            ] if dst == src => getter_proto.string_constants.get(*name as usize)?,
            _ => return None,
        };
        let setter_name = match setter_proto.code.as_slice() {
            [
                Instr::LoadInt { dst: zero, val: 0 },
                Instr::Bitwise { dst: value, a: 1, b, op: BitwiseOp::Or },
                Instr::SetProp { obj: 0, name, val, strict: false },
                Instr::ReturnUndefined,
            ] if b == zero && val == value => setter_proto.string_constants.get(*name as usize)?,
            _ => return None,
        };
        if getter_name != setter_name {
            return None;
        }
        let map = match self.heap.get(receiver_idx) {
            HeapObj::Object(map) if !map.is_ctor && map.class.is_none() => map,
            _ => return None,
        };
        let hidden = map.pos(getter_name)?;
        let attr = map.attrs[hidden];
        (!attr.accessor && attr.writable && map.vals[hidden].is_int())
            .then_some((receiver_idx, hidden))
    }

    fn mixed_field_target(&self, receiver: Value, key: &str) -> Option<(u32, usize)> {
        self.writable_own_field(receiver, key)
            .or_else(|| self.passthrough_accessor_target(receiver, key))
    }

    fn field_cyclic_read_loop(
        &self,
        regs: *mut u64,
        func_id: usize,
        start: usize,
        end: usize,
    ) -> bool {
        let Some(proto) = self.program.functions.get(func_id) else {
            return false;
        };
        let Some(p) = recognize(proto, start, end) else {
            return false;
        };
        let max_reg = [p.array, p.n, p.sum, p.k, p.i]
            .into_iter()
            .max()
            .unwrap_or(0);
        if max_reg >= proto.reg_count || p.name as usize >= proto.string_constants.len() {
            return false;
        }

        // SAFETY: the native region ABI supplies a window of proto.reg_count
        // Values, and every index was range-checked above.
        let load = |r: u16| Value::from_bits(unsafe { *regs.add(r as usize) });
        let (array_v, n_v, sum_v, k_v, i_v) =
            (load(p.array), load(p.n), load(p.sum), load(p.k), load(p.i));
        let Some(limit_v) = self.field_loop_limit(p.limit) else {
            return false;
        };
        if !array_v.is_heap()
            || !n_v.is_int()
            || !sum_v.is_int()
            || !k_v.is_int()
            || !i_v.is_int()
            || !limit_v.is_int()
        {
            return false;
        }
        let (n, mut k, i, limit) = (n_v.as_int(), k_v.as_int(), i_v.as_int(), limit_v.as_int());
        if n <= 0 || k < 0 || k >= n || i < 0 || limit < i {
            return false;
        }
        let array_idx = array_v.heap_index();
        if self.arguments_objs.contains_key(&array_idx)
            || self
                .arr_props
                .get(&array_idx)
                .is_some_and(|m| m.overlays_elements())
        {
            return false;
        }
        let elements = match self.heap.get(array_idx) {
            HeapObj::Array(a) if n as usize <= a.len() => a,
            _ => return false,
        };
        let key = &proto.string_constants[p.name as usize];
        let mut projected = Vec::with_capacity(n as usize);
        for &receiver in &elements[..n as usize] {
            if receiver.is_hole() {
                return false;
            }
            let Some(v) = self.projected_int_field(receiver, key) else {
                return false;
            };
            projected.push(v);
        }

        let remaining = (limit - i) as u32;
        let mut cycle = 0u32;
        for &v in &projected {
            cycle = cycle.wrapping_add(v as u32);
        }
        let cycles = remaining / n as u32;
        let tail = remaining % n as u32;
        let mut sum = (sum_v.as_int() as u32).wrapping_add(cycle.wrapping_mul(cycles));
        for _ in 0..tail {
            sum = sum.wrapping_add(projected[k as usize] as u32);
            k += 1;
            if k == n {
                k = 0;
            }
        }
        // Full cycles leave k unchanged; the tail loop advanced it exactly as
        // the source loop does.  Write all observable loop-carried locals so a
        // debugger/future bytecode after the exit sees interpreter-identical state.
        unsafe {
            *regs.add(p.sum as usize) = Value::int(sum as i32).bits();
            *regs.add(p.k as usize) = Value::int(k).bits();
            *regs.add(p.i as usize) = Value::int(limit).bits();
        }
        if matches!(p.limit, FieldLoopLimit::Upval(_)) && std::env::var_os("ZIPP_JITLOG").is_some()
        {
            eprintln!(
                "[jit] upvalue-field-read-stream committed {} iterations",
                remaining
            );
        }
        true
    }

    fn field_mask_read_loop(&mut self, func_id: usize, start: usize, end: usize) -> bool {
        let Some(proto) = self.program.functions.get(func_id) else {
            return false;
        };
        let Some(p) = recognize_mask_read(proto, start, end) else {
            return false;
        };
        if p.name as usize >= proto.string_constants.len()
            || [p.array_global, p.sum_global, p.i_global, p.limit_global]
                .into_iter()
                .any(|g| g as usize >= self.globals.len())
            || p.array_global == p.sum_global
            || p.array_global == p.i_global
            || p.array_global == p.limit_global
            || p.sum_global == p.i_global
            || p.sum_global == p.limit_global
            || p.i_global == p.limit_global
        {
            return false;
        }
        let array_v = self.globals[p.array_global as usize];
        let sum_v = self.globals[p.sum_global as usize];
        let i_v = self.globals[p.i_global as usize];
        let limit_v = self.globals[p.limit_global as usize];
        if !array_v.is_heap() || !sum_v.is_int() || !i_v.is_int() || !limit_v.is_int() {
            return false;
        }
        let (i, limit) = (i_v.as_int(), limit_v.as_int());
        if i < 0 || limit < i {
            return false;
        }
        let n = p.mask as usize + 1;
        let array_idx = array_v.heap_index();
        if self.arguments_objs.contains_key(&array_idx)
            || self
                .arr_props
                .get(&array_idx)
                .is_some_and(|m| m.overlays_elements())
        {
            return false;
        }
        let elements = match self.heap.get(array_idx) {
            HeapObj::Array(a) if n <= a.len() => a,
            _ => return false,
        };
        let key = &proto.string_constants[p.name as usize];
        let mut projected = Vec::with_capacity(n);
        for &receiver in &elements[..n] {
            if receiver.is_hole() {
                return false;
            }
            let Some(v) = self.projected_int_field(receiver, key) else {
                return false;
            };
            projected.push(v);
        }

        let remaining = (limit - i) as u32;
        let mut cycle = 0u32;
        for &v in &projected {
            cycle = cycle.wrapping_add(v as u32);
        }
        let cycles = remaining / n as u32;
        let tail = remaining % n as u32;
        let mut sum = (sum_v.as_int() as u32).wrapping_add(cycle.wrapping_mul(cycles));
        let mut k = (i as u32 & p.mask as u32) as usize;
        for _ in 0..tail {
            sum = sum.wrapping_add(projected[k] as u32);
            k = (k + 1) & p.mask as usize;
        }
        self.globals[p.sum_global as usize] = Value::int(sum as i32);
        self.globals[p.i_global as usize] = Value::int(limit);
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] field-mask-read-stream committed {} iterations",
                remaining
            );
        }
        true
    }

    fn global_field_sum_loop(&mut self, func_id: usize, start: usize, end: usize) -> bool {
        let Some(proto) = self.program.functions.get(func_id) else {
            return false;
        };
        let Some(p) = recognize_global_field_sum(proto, start, end) else {
            return false;
        };
        if [p.sum_global, p.i_global, p.limit_global]
            .into_iter()
            .chain(p.terms.iter().map(|&(g, _)| g))
            .any(|g| g as usize >= self.globals.len())
            || p
                .terms
                .iter()
                .any(|&(_, name)| name as usize >= proto.string_constants.len())
        {
            return false;
        }
        let sum_v = self.globals[p.sum_global as usize];
        let i_v = self.globals[p.i_global as usize];
        let limit_v = self.globals[p.limit_global as usize];
        if !sum_v.is_int() || !i_v.is_int() || !limit_v.is_int() {
            return false;
        }
        let (i, limit) = (i_v.as_int(), limit_v.as_int());
        if i < 0 || limit < i {
            return false;
        }
        let mut delta = 0u32;
        for &(receiver_global, name) in &p.terms {
            let receiver = self.globals[receiver_global as usize];
            let key = &proto.string_constants[name as usize];
            let Some(value) = self.projected_int_field(receiver, key) else {
                return false;
            };
            delta = delta.wrapping_add(value as u32);
        }
        let remaining = (limit - i) as u32;
        let sum = (sum_v.as_int() as u32).wrapping_add(delta.wrapping_mul(remaining));
        self.globals[p.sum_global as usize] = Value::int(sum as i32);
        self.globals[p.i_global as usize] = Value::int(limit);
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] global-field-sum-stream committed {} iterations ({} terms)",
                remaining,
                p.terms.len()
            );
        }
        true
    }

    fn mixed_field_loop(&mut self, func_id: usize, start: usize, end: usize) -> bool {
        let Some(proto) = self.program.functions.get(func_id) else {
            return false;
        };
        let Some(p) = recognize_mixed(proto, start, end) else {
            return false;
        };
        let globals = [
            p.array_global,
            p.receiver_global,
            p.sum_global,
            p.i_global,
            p.limit_global,
        ];
        if globals.iter().any(|&g| g as usize >= self.globals.len())
            || globals
                .iter()
                .any(|&g| !self.global_slot_directly_routable(g))
            || self.eval_const_globals.contains(&p.receiver_global)
            || self.eval_const_globals.contains(&p.sum_global)
            || self.eval_const_globals.contains(&p.i_global)
            || p.set_name as usize >= proto.string_constants.len()
            || p.get_name as usize >= proto.string_constants.len()
        {
            return false;
        }
        let set_key = &proto.string_constants[p.set_name as usize];
        let get_key = &proto.string_constants[p.get_name as usize];
        if set_key != get_key || p.add.checked_add(p.value_mask).is_none() {
            return false;
        }
        let array_v = self.globals[p.array_global as usize];
        let sum_v = self.globals[p.sum_global as usize];
        let i_v = self.globals[p.i_global as usize];
        let limit_v = self.globals[p.limit_global as usize];
        if !array_v.is_heap() || !sum_v.is_int() || !i_v.is_int() || !limit_v.is_int() {
            return false;
        }
        let (i, limit) = (i_v.as_int(), limit_v.as_int());
        if i < 0 || limit < i {
            return false;
        }
        let n = p.object_mask as usize + 1;
        let array_idx = array_v.heap_index();
        if self.arguments_objs.contains_key(&array_idx)
            || self
                .arr_props
                .get(&array_idx)
                .is_some_and(|m| m.overlays_elements())
        {
            return false;
        }
        let receivers: Vec<Value> = match self.heap.get(array_idx) {
            HeapObj::Array(a) if n <= a.len() && a[..n].iter().all(|v| !v.is_hole()) => {
                a[..n].to_vec()
            }
            _ => return false,
        };
        let mut targets = Vec::with_capacity(n);
        for &receiver in &receivers {
            let Some(target) = self.mixed_field_target(receiver, set_key) else {
                return false;
            };
            targets.push(target);
        }

        let remaining = (limit - i) as u32;
        if remaining == 0 {
            return true;
        }
        let period = p.value_mask as u64 + 1;
        let prefix = |count: u64| {
            let cycles = count / period;
            let tail = count % period;
            cycles * (period * (period - 1) / 2) + tail * (tail - 1) / 2
        };
        let masked_sum = prefix(limit as u64) - prefix(i as u64);
        let delta = (masked_sum as u32)
            .wrapping_add((p.add as u32).wrapping_mul(remaining));
        let sum = (sum_v.as_int() as u32).wrapping_add(delta);

        // Only the last write to each cyclic lane survives.  Replaying the last
        // at-most-N iterations in order also preserves aliases in the receiver
        // array exactly (two lanes may name the same ordinary object).
        let count = remaining.min(n as u32);
        let first = remaining - count;
        for t in first..remaining {
            let iteration = i as u32 + t;
            let lane = (iteration & p.object_mask as u32) as usize;
            let value = ((iteration & p.value_mask as u32) as i32) + p.add;
            let (obj, slot) = targets[lane];
            let HeapObj::Object(map) = self.heap.get_mut(obj) else {
                unreachable!("mixed-field preflight object changed without a call or GC")
            };
            map.vals[slot] = Value::int(value);
        }
        let final_lane = ((limit as u32 - 1) & p.object_mask as u32) as usize;
        self.globals[p.receiver_global as usize] = receivers[final_lane];
        self.globals[p.sum_global as usize] = Value::int(sum as i32);
        self.globals[p.i_global as usize] = Value::int(limit);
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] mixed-field-stream committed {} iterations ({} lanes)",
                remaining, n
            );
        }
        true
    }

    fn field_read_loop(
        &mut self,
        regs: *mut u64,
        func_id: usize,
        start: usize,
        end: usize,
    ) -> bool {
        self.field_cyclic_read_loop(regs, func_id, start, end)
            || self.field_mask_read_loop(func_id, start, end)
            || self.global_field_sum_loop(func_id, start, end)
    }

    fn field_write_loop(
        &mut self,
        regs: *mut u64,
        func_id: usize,
        start: usize,
        end: usize,
    ) -> bool {
        let Some(proto) = self.program.functions.get(func_id) else {
            return false;
        };
        if std::env::var_os("ZIPP_NO_FIELD_MIXED_STREAM").is_none()
            && recognize_mixed(proto, start, end).is_some()
        {
            return self.mixed_field_loop(func_id, start, end);
        }
        let Some(p) = recognize_write(proto, start, end) else {
            return false;
        };
        let max_reg = [p.array, p.n, p.k, p.i].into_iter().max().unwrap_or(0);
        if max_reg >= proto.reg_count || p.name as usize >= proto.string_constants.len() {
            return false;
        }
        let load = |r: u16| Value::from_bits(unsafe { *regs.add(r as usize) });
        let (array_v, n_v, k_v, i_v) = (load(p.array), load(p.n), load(p.k), load(p.i));
        let Some(limit_v) = self.field_loop_limit(p.limit) else {
            return false;
        };
        if !array_v.is_heap()
            || !n_v.is_int()
            || !k_v.is_int()
            || !i_v.is_int()
            || !limit_v.is_int()
        {
            return false;
        }
        let (n, k, i, limit) = (n_v.as_int(), k_v.as_int(), i_v.as_int(), limit_v.as_int());
        if n <= 0 || k < 0 || k >= n || i < 0 || limit < i {
            return false;
        }
        let array_idx = array_v.heap_index();
        if self.arguments_objs.contains_key(&array_idx)
            || self
                .arr_props
                .get(&array_idx)
                .is_some_and(|m| m.overlays_elements())
        {
            return false;
        }
        let elements = match self.heap.get(array_idx) {
            HeapObj::Array(a) if n as usize <= a.len() => a,
            _ => return false,
        };
        let key = &proto.string_constants[p.name as usize];
        let mut slots = Vec::with_capacity(n as usize);
        for &receiver in &elements[..n as usize] {
            if receiver.is_hole() {
                return false;
            }
            let Some(slot) = self.writable_own_field(receiver, key) else {
                return false;
            };
            slots.push(slot);
        }

        let remaining = (limit - i) as u32;
        let count = remaining.min(n as u32);
        let first = remaining - count;
        for t in first..remaining {
            let pos = (k as u32 + t % n as u32) % n as u32;
            let (obj, slot) = slots[pos as usize];
            let value = Value::int(i + t as i32);
            match self.heap.get_mut(obj) {
                HeapObj::Object(map) => map.vals[slot] = value,
                _ => return false, // impossible after the no-GC preflight
            }
        }
        let final_k = ((k as u32 + remaining % n as u32) % n as u32) as i32;
        unsafe {
            *regs.add(p.k as usize) = Value::int(final_k).bits();
            *regs.add(p.i as usize) = Value::int(limit).bits();
        }
        if matches!(p.limit, FieldLoopLimit::Upval(_)) && std::env::var_os("ZIPP_JITLOG").is_some()
        {
            eprintln!(
                "[jit] upvalue-field-write-stream committed {} iterations",
                remaining
            );
        }
        true
    }
}

/// Win64 entry used by the register-region prefix. `packed` is
/// `(func_id << 32) | (start << 16) | end`. Returns 1 only after committing the
/// complete loop result; 0 means the ordinary region must run unchanged.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_field_read_loop(
    vm: *mut core::ffi::c_void,
    regs: *mut u64,
    packed: u64,
) -> u64 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        let func_id = (packed >> 32) as usize;
        let start = ((packed >> 16) & 0xffff) as usize;
        let end = (packed & 0xffff) as usize;
        vm.field_read_loop(regs, func_id, start, end) as u64
    }));
    result.unwrap_or(0)
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_field_write_loop(
    vm: *mut core::ffi::c_void,
    regs: *mut u64,
    packed: u64,
) -> u64 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        let func_id = (packed >> 32) as usize;
        let start = ((packed >> 16) & 0xffff) as usize;
        let end = (packed & 0xffff) as usize;
        vm.field_write_loop(regs, func_id, start, end) as u64
    }));
    result.unwrap_or(0)
}
