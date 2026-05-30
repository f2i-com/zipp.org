//! Runtime helpers reachable from tier-2 emitted code.
//!
//! Each helper is `extern "win64"` so the emitter can call it
//! through the same ABI the rest of the x86-64 backend uses (rcx
//! = `vm_ptr`, rdx / r8 / r9 = args, rax = return, caller saves
//! shadow space).
//!
//! Helpers live here rather than in `vm/mod.rs` because they are
//! tier-2-specific: their semantics are narrower than the tier-0
//! generic arithmetic (no bytecode register window, no dispatch
//! stack — just a pure `(Value, Value) -> Value`).
//!
//! # i32 preservation
//!
//! The three arithmetic helpers share one invariant: `i32 op i32 →
//! i32` whenever the result fits in i32. This matches tier-0's
//! behaviour so that tier-2 promoted code in a mixed-tier program
//! doesn't silently widen integers to f64 and drop `Value::Integer`
//! matches at the Rust boundary. Overflow widens to f64, as does
//! any f64 operand. Non-numeric operands hit the VM slow path
//! (Add) or return NaN (Sub / Mul), matching JS semantics for those
//! conversions.

#![cfg(feature = "djit")]

use crate::runtime::value::Value;

/// Generic `+`: i32 fast path, f64 fallback, string/object slow
/// path via the tier-1 helper. Matches the decision tree tier-0's
/// inline Add opcode uses.
///
/// # Safety
///
/// `vm_raw` must point to a live [`crate::vm::VM`] **when either
/// operand is non-numeric** (we may reach the string-rope fallback
/// which derefs the VM). The numeric paths never touch the pointer.
pub unsafe extern "win64" fn tier2_add_generic_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let l = Value::from_bits(left_bits);
    let r = Value::from_bits(right_bits);
    if l.is_i32() && r.is_i32() {
        let lv = l.as_i32_unchecked();
        let rv = r.as_i32_unchecked();
        if let Some(sum) = lv.checked_add(rv) {
            return Value::from_i32(sum).bits();
        }
        // i32 overflow → widen.
        return Value::from_f64(lv as f64 + rv as f64).bits();
    }
    if l.is_number() && r.is_number() {
        return Value::from_f64(l.to_number() + r.to_number()).bits();
    }
    // Slow path: delegate to the tier-1 helper for string rope
    // append and other mixed-type cases. A live VM pointer is
    // required from here on.
    crate::vm::djit_add_generic_helper(vm_raw, left_bits, right_bits, 0)
}

/// Generic `-`: i32 fast path, f64 fallback. Non-numeric operands
/// produce `NaN` (matches JS semantics for `{} - 1` and friends).
///
/// # Safety
///
/// `vm_raw` is accepted only for ABI uniformity; the body never
/// derefs it.
pub unsafe extern "win64" fn tier2_sub_generic_helper(
    _vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let l = Value::from_bits(left_bits);
    let r = Value::from_bits(right_bits);
    if l.is_i32() && r.is_i32() {
        let lv = l.as_i32_unchecked();
        let rv = r.as_i32_unchecked();
        if let Some(diff) = lv.checked_sub(rv) {
            return Value::from_i32(diff).bits();
        }
        return Value::from_f64(lv as f64 - rv as f64).bits();
    }
    if l.is_number() && r.is_number() {
        return Value::from_f64(l.to_number() - r.to_number()).bits();
    }
    Value::from_f64(f64::NAN).bits()
}

/// Generic `*`: same shape as sub.
///
/// # Safety
///
/// See [`tier2_sub_generic_helper`].
pub unsafe extern "win64" fn tier2_mul_generic_helper(
    _vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let l = Value::from_bits(left_bits);
    let r = Value::from_bits(right_bits);
    if l.is_i32() && r.is_i32() {
        let lv = l.as_i32_unchecked();
        let rv = r.as_i32_unchecked();
        if let Some(prod) = lv.checked_mul(rv) {
            return Value::from_i32(prod).bits();
        }
        return Value::from_f64(lv as f64 * rv as f64).bits();
    }
    if l.is_number() && r.is_number() {
        return Value::from_f64(l.to_number() * r.to_number()).bits();
    }
    Value::from_f64(f64::NAN).bits()
}

/// Generic `/`: JS division semantics. i32 / i32 that divides evenly
/// and doesn't overflow stays i32; everything else widens to f64
/// (matching `5 / 2 === 2.5`, `1 / 0 === Infinity`, `0 / 0 === NaN`).
///
/// # Safety
///
/// `vm_raw` is unused; body never derefs.
pub unsafe extern "win64" fn tier2_div_generic_helper(
    _vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let l = Value::from_bits(left_bits);
    let r = Value::from_bits(right_bits);
    if l.is_i32() && r.is_i32() {
        let lv = l.as_i32_unchecked();
        let rv = r.as_i32_unchecked();
        // JS: 1 / 0 → Infinity, 0 / 0 → NaN. i32 zero divisor can't
        // stay in i32 land — fall straight to f64.
        if rv != 0 {
            // Integer-exact quotient stays i32; anything else widens.
            // Checked_div handles (INT_MIN, -1) overflow by returning
            // None so it widens to f64 too.
            if let Some(q) = lv.checked_div(rv) {
                if q * rv == lv {
                    return Value::from_i32(q).bits();
                }
            }
        }
        return Value::from_f64(lv as f64 / rv as f64).bits();
    }
    if l.is_number() && r.is_number() {
        return Value::from_f64(l.to_number() / r.to_number()).bits();
    }
    Value::from_f64(f64::NAN).bits()
}

/// Generic `%`: JS modulo. Matches f64 `%` semantics rather than
/// truncated-division modulo (same sign as dividend, NaN on
/// divide-by-zero, f64 fractional on non-integer operands).
///
/// # Safety
///
/// `vm_raw` is unused; body never derefs.
pub unsafe extern "win64" fn tier2_mod_generic_helper(
    _vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let l = Value::from_bits(left_bits);
    let r = Value::from_bits(right_bits);
    if l.is_i32() && r.is_i32() {
        let lv = l.as_i32_unchecked();
        let rv = r.as_i32_unchecked();
        if rv != 0 {
            // checked_rem avoids panic on (INT_MIN, -1). On success
            // the remainder fits in i32.
            if let Some(rem) = lv.checked_rem(rv) {
                return Value::from_i32(rem).bits();
            }
        }
        // Zero divisor or overflow → fall to f64.
        return Value::from_f64(lv as f64 % rv as f64).bits();
    }
    if l.is_number() && r.is_number() {
        return Value::from_f64(l.to_number() % r.to_number()).bits();
    }
    Value::from_f64(f64::NAN).bits()
}

/// Create a zero-capture closure on the heap. Matches the tier-0
/// interpreter's `MakeClosure` handling for `count == 0` — clones
/// the compiled-function constant at `const_idx` and puts the
/// resulting `Object::CompiledFunction` on the heap, returning a
/// NaN-boxed heap Value.
///
/// # Safety
///
/// `vm_raw` must point to a live [`crate::vm::VM`]. `const_idx` must
/// be a valid index into the VM's current constants table, and the
/// constant at that index must be a `CompiledFunction`. Both are
/// guaranteed by the translator's static check + the VM's own
/// constants-preconvert invariants.
pub unsafe extern "win64" fn tier2_make_closure_helper(
    vm_raw: *mut u8,
    const_idx: u64,
    _unused1: u64,
    _unused2: u64,
) -> u64 {
    let vm = &mut *(vm_raw as *mut crate::vm::VM);
    let const_idx = const_idx as usize;
    let func_obj = &*vm.constants_raw;
    let func = match &func_obj[const_idx] {
        crate::object::Object::CompiledFunction(f) => (**f).clone(),
        _ => {
            // Mismatched shape — translator shouldn't emit this, but
            // fail safely via deopt rather than UB.
            vm.deopt_pending = true;
            return Value::UNDEFINED.bits();
        }
    };
    // Zero-capture closure: captured_values stays empty (the clone
    // from the constants table already starts with an empty vec).
    let val = crate::runtime::value::obj_into_val(
        crate::object::Object::CompiledFunction(Box::new(func)),
        &mut vm.heap,
    );
    val.bits()
}

/// Soft-deopt helper: called by the tier-2 deopt trampoline when a
/// speculation guard fails (CheckI32 sees a non-i32, CheckedAddI32
/// overflows, an explicit `Deopt` terminator is reached).
///
/// The helper does the minimum possible: flip the VM's
/// `deopt_pending` flag and return a sentinel. The tier-2 frame
/// then unwinds via its normal epilogue. Once control reaches the
/// VM dispatch site, it notices the flag, blacklists the offending
/// tier-2 function, and retries the call through tier-1.
///
/// # Safety
///
/// `vm_raw` must point to a live [`crate::vm::VM`]. Called from
/// JIT-emitted code; the compiler contract guarantees validity.
pub unsafe extern "win64" fn tier2_deopt_helper(vm_raw: *mut u8) -> u64 {
    let vm = &mut *(vm_raw as *mut crate::vm::VM);
    vm.deopt_pending = true;
    // Return value is discarded by the dispatch site when
    // deopt_pending is set. Zero is as good as any.
    0
}

// ── Generic comparison helpers ────────────────────────────────────────
//
// Each helper returns the NaN-boxed `Value::TRUE` / `Value::FALSE`
// bit-pattern (not a raw 0/1) so the result is a well-formed Value
// whether it feeds downstream arithmetic, a conditional branch (which
// inspects bit 0 — matching TRUE's low bit), or a final `Return`.
// This mirrors tier-0's `store_cmp` which also stores
// `Value::TRUE` / `Value::FALSE` into the destination register.
//
// On error (ex. a comparison that would throw from the slow path), the
// helper flips `vm.deopt_pending` and returns FALSE. The dispatch-site
// handler at the VM catches the flag, blacklists the tier-2 function,
// and falls through to the interpreter — which re-runs the comparison
// with proper error propagation.

#[inline(always)]
fn bool_to_val_bits(b: bool) -> u64 {
    if b { Value::TRUE.bits() } else { Value::FALSE.bits() }
}

/// Strict equality (`===`).
///
/// # Safety
///
/// `vm_raw` must point to a live [`crate::vm::VM`].
pub unsafe extern "win64" fn tier2_eq_value_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let l = Value::from_bits(left_bits);
    let r = Value::from_bits(right_bits);
    // Bit-equal handles most cases including identical i32, identical
    // heap pointers, and identical f64s that aren't NaN.
    if l.bits() == r.bits() {
        // The lone tricky case: NaN === NaN is false in JS.
        if l.is_f64() && l.as_f64().is_nan() {
            return Value::FALSE.bits();
        }
        return Value::TRUE.bits();
    }
    // Different bits, both i32 → definitely different values.
    if Value::both_i32(l, r) {
        return Value::FALSE.bits();
    }
    // Different bits, both numeric → compare as f64 (handles i32↔f64).
    if l.is_number() && r.is_number() {
        return bool_to_val_bits(l.to_number() == r.to_number());
    }
    // Different bits, non-numeric → delegate to VM for structural
    // equality of heap objects / inline strings / etc.
    let vm = &mut *(vm_raw as *mut crate::vm::VM);
    bool_to_val_bits(vm.strict_equality_slow(l, r))
}

/// Strict inequality (`!==`). Bit-flips strict equality's result.
///
/// # Safety
///
/// See [`tier2_eq_value_helper`].
pub unsafe extern "win64" fn tier2_ne_value_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    // XOR with `1` in the low bit flips TRUE↔FALSE without disturbing
    // the upper 63 bits of the NaN box (both values share the same
    // tag prefix).
    tier2_eq_value_helper(vm_raw, left_bits, right_bits, 0) ^ 1
}

/// Loose equality (`==`). Applies the ECMAScript type-coercion rules
/// for mixed-type operands via the VM's slow path.
///
/// # Safety
///
/// `vm_raw` must point to a live [`crate::vm::VM`].
pub unsafe extern "win64" fn tier2_loose_eq_value_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let l = Value::from_bits(left_bits);
    let r = Value::from_bits(right_bits);
    // Same bit-exact and same-type-same-value shortcuts apply.
    if l.bits() == r.bits() {
        if l.is_f64() && l.as_f64().is_nan() {
            return Value::FALSE.bits();
        }
        return Value::TRUE.bits();
    }
    if Value::both_i32(l, r) {
        return Value::FALSE.bits();
    }
    if l.is_number() && r.is_number() {
        return bool_to_val_bits(l.to_number() == r.to_number());
    }
    // Mixed or non-numeric: delegate to VM which handles
    // string↔number, null↔undefined, object coercion, etc.
    let vm = &mut *(vm_raw as *mut crate::vm::VM);
    bool_to_val_bits(vm.equality_slow(l, r))
}

/// Less-than (`<`). Numeric operands compare directly; non-numeric
/// fall to the VM's slow path which handles string lex compare and
/// object coercion.
///
/// # Safety
///
/// `vm_raw` must point to a live [`crate::vm::VM`].
pub unsafe extern "win64" fn tier2_lt_value_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let l = Value::from_bits(left_bits);
    let r = Value::from_bits(right_bits);
    if Value::both_i32(l, r) {
        return bool_to_val_bits(l.as_i32_unchecked() < r.as_i32_unchecked());
    }
    if l.is_number() && r.is_number() {
        let lv = l.to_number();
        let rv = r.to_number();
        return bool_to_val_bits(lv < rv); // NaN < anything is false
    }
    let vm = &mut *(vm_raw as *mut crate::vm::VM);
    match vm.comparison_slow(crate::rcode::ROp::LessThan, l, r) {
        Ok(b) => bool_to_val_bits(b),
        Err(_) => {
            // Comparison threw; fall back to tier-0 via the deopt path
            // so the exception propagates correctly.
            vm.deopt_pending = true;
            Value::FALSE.bits()
        }
    }
}

/// Less-than-or-equal (`<=`).
///
/// # Safety
///
/// See [`tier2_lt_value_helper`].
pub unsafe extern "win64" fn tier2_le_value_helper(
    vm_raw: *mut u8,
    left_bits: u64,
    right_bits: u64,
    _unused: u64,
) -> u64 {
    let l = Value::from_bits(left_bits);
    let r = Value::from_bits(right_bits);
    if Value::both_i32(l, r) {
        return bool_to_val_bits(l.as_i32_unchecked() <= r.as_i32_unchecked());
    }
    if l.is_number() && r.is_number() {
        let lv = l.to_number();
        let rv = r.to_number();
        return bool_to_val_bits(lv <= rv);
    }
    let vm = &mut *(vm_raw as *mut crate::vm::VM);
    match vm.comparison_slow(crate::rcode::ROp::LessOrEqual, l, r) {
        Ok(b) => bool_to_val_bits(b),
        Err(_) => {
            vm.deopt_pending = true;
            Value::FALSE.bits()
        }
    }
}
