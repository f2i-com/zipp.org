#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// Exact main-realm function identities used by RegExp protocol-collapse
/// proofs. Child realms intentionally allocate distinct Native objects with
/// the same ids, so checking `HeapObj::Native(id)` alone is unsound.
pub(crate) const REGEXP_PROTOCOL_INTRINSIC_COUNT: usize = 14;
const REGEXP_PROTOCOL_SPECIES: usize = 13;
const REGEXP_PROTOCOL_PROTO_SLOTS: [(&str, bool, u16); 13] = [
    ("exec", false, native::REGEXP_EXEC),
    ("flags", true, native::REGEXP_GET_FLAGS),
    ("hasIndices", true, native::REGEXP_GET_HASINDICES),
    ("global", true, native::REGEXP_GET_GLOBAL),
    ("ignoreCase", true, native::REGEXP_GET_IGNORECASE),
    ("multiline", true, native::REGEXP_GET_MULTILINE),
    ("dotAll", true, native::REGEXP_GET_DOTALL),
    ("unicode", true, native::REGEXP_GET_UNICODE),
    ("unicodeSets", true, native::REGEXP_GET_UNICODESETS),
    ("sticky", true, native::REGEXP_GET_STICKY),
    ("@@match", false, native::REGEXP_SYM_MATCH),
    ("@@matchAll", false, native::REGEXP_SYM_MATCHALL),
    ("@@replace", false, native::REGEXP_SYM_REPLACE),
];

#[inline]
const fn regexp_protocol_intrinsic_index(native_id: u16) -> Option<usize> {
    match native_id {
        native::REGEXP_EXEC => Some(0),
        native::REGEXP_GET_FLAGS => Some(1),
        native::REGEXP_GET_HASINDICES => Some(2),
        native::REGEXP_GET_GLOBAL => Some(3),
        native::REGEXP_GET_IGNORECASE => Some(4),
        native::REGEXP_GET_MULTILINE => Some(5),
        native::REGEXP_GET_DOTALL => Some(6),
        native::REGEXP_GET_UNICODE => Some(7),
        native::REGEXP_GET_UNICODESETS => Some(8),
        native::REGEXP_GET_STICKY => Some(9),
        native::REGEXP_SYM_MATCH => Some(10),
        native::REGEXP_SYM_MATCHALL => Some(11),
        native::REGEXP_SYM_REPLACE => Some(12),
        native::SPECIES_GET => Some(REGEXP_PROTOCOL_SPECIES),
        _ => None,
    }
}

/// Convert an already-ToLength-clamped integer to a host index without the
/// wasm32 `u64 as usize` wraparound. RegExp search indices beyond the largest
/// host-addressable string index are semantically just out of range, so
/// saturation is the useful representation here.
#[inline]
fn host_index_saturating(value: i64) -> usize {
    usize::try_from(value.max(0) as u64).unwrap_or(usize::MAX)
}

/// Native allocation retained by a completed match collection. The shared
/// named-group table belongs to the compiled RegExp program and is already
/// charged once by the heap audit; cloning its `Arc` allocates nothing. What
/// remains here is the outer backing allocation plus every owned capture Vec.
#[cfg(feature = "safe-sandbox")]
fn retained_match_collection_bytes(matches: &[regress::Match], outer_capacity: usize) -> usize {
    outer_capacity
        .saturating_mul(std::mem::size_of::<regress::Match>())
        .saturating_add(matches.iter().fold(0usize, |bytes, matched| {
            bytes.saturating_add(
                matched
                    .captures
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Option<std::ops::Range<usize>>>()),
            )
        }))
}

#[cfg(feature = "safe-sandbox")]
fn regex_reconcile_transient(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    prepaid: usize,
    actual: usize,
) -> Result<(), Thrown> {
    if actual > prepaid {
        vm.instrument_grow_regex_transient(reservation, actual - prepaid)
            .map_err(|m| Thrown(m.into()))?;
    } else if prepaid > actual {
        vm.instrument_shrink_regex_transient(reservation, prepaid - actual);
    }
    Ok(())
}

/// Charge a native Vec's requested backing before allocation, then reconcile
/// the provisional charge with the capacity Rust actually retained.
#[cfg(feature = "safe-sandbox")]
fn regex_try_reserve_exact<T>(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    values: &mut Vec<T>,
    additional: usize,
) -> Result<(), Thrown> {
    let target = values.len().saturating_add(additional);
    let requested_growth = target.saturating_sub(values.capacity());
    let requested_bytes = requested_growth.saturating_mul(std::mem::size_of::<T>());
    vm.instrument_grow_regex_transient(reservation, requested_bytes)
        .map_err(|m| Thrown(m.into()))?;
    let old_capacity = values.capacity();
    if values.try_reserve_exact(additional).is_err() {
        vm.instrument_shrink_regex_transient(reservation, requested_bytes);
        return Err(Thrown(vm.instrument_regex_memory_exhausted().into()));
    }
    let actual_bytes = values
        .capacity()
        .saturating_sub(old_capacity)
        .saturating_mul(std::mem::size_of::<T>());
    regex_reconcile_transient(vm, reservation, requested_bytes, actual_bytes)
}

/// Fallibly grow a repeatedly-appended Vec geometrically while charging the
/// allocator's retained capacity. Exact one-element growth turns left-deep
/// ropes and global replacement output into quadratic realloc/copy loops.
#[cfg(feature = "safe-sandbox")]
fn regex_try_reserve_geometric<T>(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    values: &mut Vec<T>,
    additional: usize,
    max_capacity: usize,
) -> Result<(), Thrown> {
    let required = values
        .len()
        .checked_add(additional)
        .filter(|required| *required <= max_capacity)
        .ok_or_else(|| Thrown(vm.instrument_regex_memory_exhausted().into()))?;
    if required <= values.capacity() {
        return Ok(());
    }
    let target = required
        .max(values.capacity().saturating_mul(2).max(4))
        .min(max_capacity);
    regex_try_reserve_exact(vm, reservation, values, target - values.len())
}

#[cfg(feature = "safe-sandbox")]
fn regex_try_reserve_string_exact(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    value: &mut String,
    additional: usize,
) -> Result<(), Thrown> {
    let target = value.len().saturating_add(additional);
    let requested = target.saturating_sub(value.capacity());
    vm.instrument_grow_regex_transient(reservation, requested)
        .map_err(|message| Thrown(message.into()))?;
    let old_capacity = value.capacity();
    if value.try_reserve_exact(additional).is_err() {
        vm.instrument_shrink_regex_transient(reservation, requested);
        return Err(Thrown(vm.instrument_regex_memory_exhausted().into()));
    }
    regex_reconcile_transient(
        vm,
        reservation,
        requested,
        value.capacity().saturating_sub(old_capacity),
    )
}

#[cfg(feature = "safe-sandbox")]
fn regex_try_reserve_string_geometric(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    value: &mut String,
    additional: usize,
) -> Result<(), Thrown> {
    let required = value
        .len()
        .checked_add(additional)
        .filter(|required| *required <= MAX_STRING_BYTES)
        .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
    if required <= value.capacity() {
        return Ok(());
    }
    let target = required
        .max(value.capacity().saturating_mul(2).max(4))
        .min(MAX_STRING_BYTES);
    regex_try_reserve_string_exact(vm, reservation, value, target - value.len())
}

#[cfg(feature = "safe-sandbox")]
fn regex_push_value(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    values: &mut Vec<Value>,
    value: Value,
) -> Result<(), Thrown> {
    if values.len() == values.capacity() {
        let grow_by = values.capacity().max(4);
        regex_try_reserve_exact(vm, reservation, values, grow_by)?;
    }
    values.push(value);
    Ok(())
}

#[cfg(feature = "safe-sandbox")]
fn regex_append_bytes(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    out: &mut Vec<u8>,
    bytes: &[u8],
) -> Result<(), Thrown> {
    if bytes.len() > MAX_STRING_BYTES.saturating_sub(out.len()) {
        return Err(Thrown("RangeError: Invalid string length".into()));
    }
    regex_try_reserve_geometric(vm, reservation, out, bytes.len(), MAX_STRING_BYTES)?;
    out.extend_from_slice(bytes);
    Ok(())
}

#[cfg(feature = "safe-sandbox")]
fn regex_append_wtf8(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    out: &mut Vec<u8>,
    bytes: &[u8],
) -> Result<(), Thrown> {
    if bytes.len() > MAX_STRING_BYTES.saturating_sub(out.len()) {
        return Err(Thrown("RangeError: Invalid string length".into()));
    }
    regex_try_reserve_geometric(vm, reservation, out, bytes.len(), MAX_STRING_BYTES)?;
    crate::heap::wtf8_push(out, bytes);
    Ok(())
}

#[cfg(feature = "safe-sandbox")]
fn regex_append_units(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    out: &mut Vec<u8>,
    units: &[u16],
) -> Result<(), Thrown> {
    let additional = units.len().saturating_mul(3);
    if additional > MAX_STRING_BYTES.saturating_sub(out.len()) {
        return Err(Thrown("RangeError: Invalid string length".into()));
    }
    regex_try_reserve_geometric(vm, reservation, out, additional, MAX_STRING_BYTES)?;
    push_units(out, units);
    Ok(())
}

#[cfg(feature = "safe-sandbox")]
fn regex_wtf8_to_heap(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    bytes: Vec<u8>,
) -> Value {
    let retained = bytes.capacity();
    let value = Value::heap(vm.heap.alloc_js(crate::heap::JsStr::from_wtf8(bytes)));
    vm.instrument_shrink_regex_transient(reservation, retained);
    value
}

#[cfg(feature = "safe-sandbox")]
fn regex_string_to_heap(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    string: String,
) -> Value {
    let retained = string.capacity();
    let value = vm.alloc_str(string);
    vm.instrument_shrink_regex_transient(reservation, retained);
    value
}

#[cfg(feature = "safe-sandbox")]
fn regex_values_to_heap(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    values: Vec<Value>,
) -> Value {
    let retained = values
        .capacity()
        .saturating_mul(std::mem::size_of::<Value>());
    let value = Value::heap(vm.heap.alloc(HeapObj::Array(values)));
    vm.instrument_shrink_regex_transient(reservation, retained);
    value
}

#[cfg(feature = "safe-sandbox")]
fn regex_units_value(vm: &mut Vm<'_>, units: &[u16]) -> Result<Value, Thrown> {
    let mut reservation = vm
        .instrument_reserve_regex_transient(0)
        .map_err(|message| Thrown(message.into()))?;
    let mut bytes = Vec::new();
    regex_append_units(vm, &mut reservation, &mut bytes, units)?;
    Ok(regex_wtf8_to_heap(vm, &mut reservation, bytes))
}

/// Copy a subject's exact UTF-16 view under the same transient-memory ceiling
/// as the RegExp executor.  The safe profile must not use `collect()` here:
/// that allocates before `MatchLimits` exists and can turn one compact WTF-8
/// heap string into an unmetered `Vec<u16>` (or an allocator abort).
///
/// The returned reservation intentionally lives beside the Vec.  Callers keep
/// both in scope until every match range and empty-match advance has finished,
/// so nested regex calls see the copy in their remaining headroom.
#[cfg(feature = "safe-sandbox")]
fn regex_subject_units(
    vm: &mut Vm<'_>,
    value: Value,
) -> Result<(Vec<u16>, super::instrument::RegexTransientReservation), Thrown> {
    let mut reservation = vm
        .instrument_reserve_regex_transient(0)
        .map_err(|message| Thrown(message.into()))?;
    if !value.is_heap() {
        return Ok((Vec::new(), reservation));
    }

    let index = value.heap_index();
    let expected_units = vm.heap.str_units(index).unwrap_or_default();
    let mut units = Vec::new();
    regex_try_reserve_exact(vm, &mut reservation, &mut units, expected_units)?;

    // Walk ropes without flattening them. Besides avoiding a second subject-
    // sized allocation, this ensures no infallible Heap::flatten buffer can be
    // created before the RegExp ceiling. The traversal stack is independently
    // precharged and fallibly grown under the same aggregate counter.
    let mut traversal_reservation = vm
        .instrument_reserve_regex_transient(0)
        .map_err(|message| Thrown(message.into()))?;
    let mut pending = Vec::new();
    regex_try_reserve_exact(vm, &mut traversal_reservation, &mut pending, 1)?;
    pending.push(index);

    // Push units only inside the pre-reserved logical length. Even if a
    // corrupted cached unit count disagreed with the decoder, this loop cannot
    // fall back to Vec's infallible growth path.
    let mut overflow = false;
    let mut malformed = false;
    while let Some(part) = pending.pop() {
        let children = match vm.heap.get(part) {
            HeapObj::Str(js) if js.is_ascii() => {
                for &byte in js.as_bytes() {
                    if units.len() == expected_units {
                        overflow = true;
                        break;
                    }
                    units.push(byte as u16);
                }
                None
            }
            HeapObj::Str(js) => {
                for unit in js.units_iter() {
                    if units.len() == expected_units {
                        overflow = true;
                        break;
                    }
                    units.push(unit);
                }
                None
            }
            HeapObj::Cons { left, right, .. } => Some((*left, *right)),
            _ => {
                malformed = true;
                None
            }
        };
        if overflow || malformed {
            break;
        }
        if let Some((left, right)) = children {
            regex_try_reserve_geometric(
                vm,
                &mut traversal_reservation,
                &mut pending,
                2,
                expected_units.max(2),
            )?;
            // LIFO: visit the left side first to preserve string order.
            pending.push(right);
            pending.push(left);
        }
    }
    drop(pending);
    drop(traversal_reservation);
    if overflow || malformed || units.len() != expected_units {
        return Err(Thrown(vm.instrument_regex_memory_exhausted().into()));
    }

    Ok((units, reservation))
}

/// Read one UTF-16 unit without flattening a rope or rebuilding its complete
/// unit buffer. Rope descent and the flat non-ASCII leaf scan are charged as
/// regex work, so repeated empty-match advancement cannot hide quadratic host
/// work from the safe profile's execution ceiling.
#[cfg(feature = "safe-sandbox")]
fn regex_value_unit_at(vm: &mut Vm<'_>, value: Value, index: usize) -> Result<Option<u16>, Thrown> {
    if !value.is_heap() {
        return Ok(None);
    }
    let mut part = value.heap_index();
    let Some(total_units) = vm.heap.str_units(part) else {
        return Ok(None);
    };
    if index >= total_units {
        vm.instrument_regex_usage(regress::MatchUsage {
            steps: 1,
            exhaustion: None,
        })
        .map_err(|message| Thrown(message.into()))?;
        return Ok(None);
    }

    let step_limit = vm.instrument_regex_limits().max_steps;
    let mut work = 0u64;
    let mut offset = index;
    loop {
        if work >= step_limit {
            vm.instrument_regex_usage(regress::MatchUsage {
                steps: work,
                exhaustion: Some(regress::MatchLimitError::Steps),
            })
            .map_err(|message| Thrown(message.into()))?;
            unreachable!("regex exhaustion must return above");
        }
        work += 1;

        enum Node {
            Leaf {
                value: Option<u16>,
                scan: u64,
            },
            Branch {
                left: u32,
                right: u32,
                left_units: usize,
            },
            Invalid,
        }
        let node = match vm.heap.get(part) {
            HeapObj::Str(js) => Node::Leaf {
                value: js.unit_at(offset),
                // ASCII lookup is indexed; a WTF-8 leaf's current `unit_at`
                // scans from its start, with `offset + 1` an upper bound.
                scan: if js.is_ascii() {
                    1
                } else {
                    u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1)
                },
            },
            HeapObj::Cons { left, right, .. } => Node::Branch {
                left: *left,
                right: *right,
                left_units: vm.heap.str_units(*left).unwrap_or_default(),
            },
            _ => Node::Invalid,
        };
        match node {
            Node::Leaf { value, scan } => {
                if scan > step_limit.saturating_sub(work) {
                    vm.instrument_regex_usage(regress::MatchUsage {
                        steps: work,
                        exhaustion: Some(regress::MatchLimitError::Steps),
                    })
                    .map_err(|message| Thrown(message.into()))?;
                    unreachable!("regex exhaustion must return above");
                }
                work += scan;
                vm.instrument_regex_usage(regress::MatchUsage {
                    steps: work,
                    exhaustion: None,
                })
                .map_err(|message| Thrown(message.into()))?;
                return Ok(value);
            }
            Node::Branch {
                left,
                right,
                left_units,
            } => {
                if offset < left_units {
                    part = left;
                } else {
                    offset -= left_units;
                    part = right;
                }
            }
            Node::Invalid => {
                vm.instrument_regex_usage(regress::MatchUsage {
                    steps: work,
                    exhaustion: None,
                })
                .map_err(|message| Thrown(message.into()))?;
                return Ok(None);
            }
        }
    }
}

/// ToString a capture once, then retain its lossy host representation under a
/// scoped charge. Three bytes per UTF-16 unit is a strict pre-allocation upper
/// bound (including lone-surrogate replacement); the charge is reduced to the
/// String's actual capacity immediately after construction.
#[cfg(feature = "safe-sandbox")]
fn regex_capture_primitive(vm: &mut Vm<'_>, value: Value) -> Result<Value, Thrown> {
    if vm.is_object_value(value) {
        vm.to_primitive_string(value)
    } else {
        Ok(value)
    }
}

#[cfg(feature = "safe-sandbox")]
fn regex_primitive_string_prepaid(vm: &Vm<'_>, value: Value) -> usize {
    if !value.is_heap() {
        return 64;
    }
    match vm.heap.get(value.heap_index()) {
        HeapObj::Str(_) | HeapObj::Cons { .. } => vm
            .heap
            .str_units(value.heap_index())
            .unwrap_or_default()
            .saturating_mul(3),
        HeapObj::BigInt(_) => 64,
        HeapObj::BigIntBig(integer) => (integer.bits() as usize)
            .saturating_mul(30_103)
            .saturating_div(100_000)
            .saturating_add(2),
        // A Symbol throws before allocation. Any object here would violate
        // regex_capture_primitive's postcondition; use the global string bound
        // as a fail-closed fallback rather than undercharging it.
        HeapObj::Symbol { .. } => 0,
        _ => MAX_STRING_BYTES,
    }
}

#[cfg(feature = "safe-sandbox")]
fn regex_owned_capture_string(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    value: Value,
) -> Result<String, Thrown> {
    let primitive = regex_capture_primitive(vm, value)?;
    if primitive.is_heap() && vm.heap.is_str_like(primitive.heap_index()) {
        let (units, _units_reservation) = regex_subject_units(vm, primitive)?;
        return regex_owned_utf16_lossy(vm, reservation, &units);
    }
    let prepaid = regex_primitive_string_prepaid(vm, primitive);
    vm.instrument_grow_regex_transient(reservation, prepaid)
        .map_err(|m| Thrown(m.into()))?;
    let owned = vm.to_js_string(primitive)?;
    regex_reconcile_transient(vm, reservation, prepaid, owned.capacity())?;
    Ok(owned)
}

#[cfg(feature = "safe-sandbox")]
fn regex_owned_wtf8_string(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    value: Value,
) -> Result<Vec<u8>, Thrown> {
    let primitive = regex_capture_primitive(vm, value)?;
    if primitive.is_heap() && vm.heap.is_str_like(primitive.heap_index()) {
        let (units, _units_reservation) = regex_subject_units(vm, primitive)?;
        let mut owned = Vec::new();
        regex_append_units(vm, reservation, &mut owned, &units)?;
        return Ok(owned);
    }
    let prepaid = regex_primitive_string_prepaid(vm, primitive);
    vm.instrument_grow_regex_transient(reservation, prepaid)
        .map_err(|message| Thrown(message.into()))?;
    let owned = vm.to_js_string(primitive)?.into_bytes();
    regex_reconcile_transient(vm, reservation, prepaid, owned.capacity())?;
    Ok(owned)
}

/// Produce the functional replacer's string Value without cloning an existing
/// string primitive. Non-string captures are coerced under a provisional
/// charge, moved into the heap once, and then handed off from transient to heap
/// accounting before the next capture is read.
#[cfg(feature = "safe-sandbox")]
fn regex_capture_value(vm: &mut Vm<'_>, value: Value) -> Result<Value, Thrown> {
    let primitive = regex_capture_primitive(vm, value)?;
    if primitive.is_heap() && vm.heap.is_str_like(primitive.heap_index()) {
        return Ok(primitive);
    }

    let prepaid = regex_primitive_string_prepaid(vm, primitive);
    let mut reservation = vm
        .instrument_reserve_regex_transient(prepaid)
        .map_err(|message| Thrown(message.into()))?;
    let owned = vm.to_js_string(primitive)?;
    regex_reconcile_transient(vm, &mut reservation, prepaid, owned.capacity())?;
    let retained = owned.capacity();
    let result = vm.alloc_str(owned);
    // The String buffer was either moved into HeapObj::Str (and is now visible
    // to heap_bytes) or discarded by the empty/single-ASCII intern fast path.
    vm.instrument_shrink_regex_transient(&mut reservation, retained);
    Ok(result)
}

#[cfg(feature = "safe-sandbox")]
fn regex_owned_utf16_lossy(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    units: &[u16],
) -> Result<String, Thrown> {
    let prepaid = units.len().saturating_mul(3);
    vm.instrument_grow_regex_transient(reservation, prepaid)
        .map_err(|m| Thrown(m.into()))?;
    let mut owned = String::new();
    if owned.try_reserve_exact(prepaid).is_err() {
        vm.instrument_shrink_regex_transient(reservation, prepaid);
        return Err(Thrown(vm.instrument_regex_memory_exhausted().into()));
    }
    regex_reconcile_transient(vm, reservation, prepaid, owned.capacity())?;
    for decoded in char::decode_utf16(units.iter().copied()) {
        owned.push(decoded.unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    Ok(owned)
}

#[cfg(feature = "safe-sandbox")]
fn regex_owned_str(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    value: &str,
) -> Result<String, Thrown> {
    let prepaid = value.len();
    vm.instrument_grow_regex_transient(reservation, prepaid)
        .map_err(|m| Thrown(m.into()))?;
    let mut owned = String::new();
    if owned.try_reserve_exact(prepaid).is_err() {
        vm.instrument_shrink_regex_transient(reservation, prepaid);
        return Err(Thrown(vm.instrument_regex_memory_exhausted().into()));
    }
    regex_reconcile_transient(vm, reservation, prepaid, owned.capacity())?;
    owned.push_str(value);
    Ok(owned)
}

#[cfg(feature = "safe-sandbox")]
fn regex_owned_flat_ascii(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    index: u32,
) -> Result<String, Thrown> {
    let prepaid = match vm.heap.get(index) {
        HeapObj::Str(value) if value.is_ascii() => value.as_bytes().len(),
        _ => 0,
    };
    vm.instrument_grow_regex_transient(reservation, prepaid)
        .map_err(|message| Thrown(message.into()))?;
    let mut owned = String::new();
    if owned.try_reserve_exact(prepaid).is_err() {
        vm.instrument_shrink_regex_transient(reservation, prepaid);
        return Err(Thrown(vm.instrument_regex_memory_exhausted().into()));
    }
    regex_reconcile_transient(vm, reservation, prepaid, owned.capacity())?;
    if let HeapObj::Str(value) = vm.heap.get(index) {
        owned.push_str(value.as_str_wf());
    }
    Ok(owned)
}

/// Build one deferred ASCII static under an aggregate reservation that already
/// includes `range.len()` bytes, then hand the actual Vec capacity to the VM
/// heap audit. This keeps the all-thirteen preflight atomic while allocation
/// itself remains fallible.
#[cfg(feature = "safe-sandbox")]
fn regex_ascii_slice_precharged(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    index: u32,
    range: std::ops::Range<usize>,
) -> Result<Value, Thrown> {
    let prepaid = range.end.saturating_sub(range.start);
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(prepaid).is_err() {
        return Err(Thrown(vm.instrument_regex_memory_exhausted().into()));
    }
    regex_reconcile_transient(vm, reservation, prepaid, bytes.capacity())?;
    if let HeapObj::Str(subject) = vm.heap.get(index) {
        bytes.extend_from_slice(&subject.as_bytes()[range]);
    }
    let retained = bytes.capacity();
    let value = Value::heap(vm.heap.alloc_js(crate::heap::JsStr::from_ascii(bytes)));
    vm.instrument_shrink_regex_transient(reservation, retained);
    Ok(value)
}

/// Materialize a slice of an already-owned ASCII subject under an aggregate
/// capture reservation. Empty and one-byte strings use the VM's permanent
/// intern slots and therefore need no native backing allocation.
#[cfg(feature = "safe-sandbox")]
fn regex_ascii_str_slice_precharged(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    subject: &str,
    range: std::ops::Range<usize>,
) -> Result<Value, Thrown> {
    let bytes = &subject.as_bytes()[range];
    match bytes {
        [] => return Ok(Value::heap(crate::heap::INTERN_EMPTY)),
        [byte] => return Ok(Value::heap(*byte as u32)),
        _ => {}
    }

    let prepaid = bytes.len();
    let mut owned = Vec::new();
    if owned.try_reserve_exact(prepaid).is_err() {
        return Err(Thrown(vm.instrument_regex_memory_exhausted().into()));
    }
    regex_reconcile_transient(vm, reservation, prepaid, owned.capacity())?;
    owned.extend_from_slice(bytes);
    let retained = owned.capacity();
    let value = Value::heap(vm.heap.alloc_js(crate::heap::JsStr::from_ascii(owned)));
    vm.instrument_shrink_regex_transient(reservation, retained);
    Ok(value)
}

/// UTF-16 counterpart of [`regex_ascii_str_slice_precharged`]. The aggregate
/// caller has already charged the same conservative three-bytes-per-unit
/// capacity used by the ordinary `units_value` builder; allocation itself is
/// fallible, and the charge is transferred to audited heap ownership before
/// any replacer callback can re-enter the VM.
#[cfg(feature = "safe-sandbox")]
fn regex_units_value_precharged(
    vm: &mut Vm<'_>,
    reservation: &mut super::instrument::RegexTransientReservation,
    units: &[u16],
) -> Result<Value, Thrown> {
    match units {
        [] => return Ok(Value::heap(crate::heap::INTERN_EMPTY)),
        [unit] if *unit < 0x80 => return Ok(Value::heap(*unit as u32)),
        _ => {}
    }

    let prepaid = units.len().saturating_mul(3);
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(prepaid).is_err() {
        return Err(Thrown(vm.instrument_regex_memory_exhausted().into()));
    }
    regex_reconcile_transient(vm, reservation, prepaid, bytes.capacity())?;
    push_units(&mut bytes, units);
    let retained = bytes.capacity();
    let value = Value::heap(vm.heap.alloc_js(crate::heap::JsStr::from_wtf8(bytes)));
    vm.instrument_shrink_regex_transient(reservation, retained);
    Ok(value)
}

#[cfg(feature = "safe-sandbox")]
fn ascii_slice_heap_bytes(range: std::ops::Range<usize>) -> usize {
    let len = range.end.saturating_sub(range.start);
    // Heap::alloc_js interns empty and single-ASCII-byte strings.
    if len <= 1 {
        0
    } else {
        len
    }
}

#[cfg(feature = "safe-sandbox")]
fn utf16_slice_heap_bytes(units: &[u16], range: std::ops::Range<usize>) -> usize {
    let slice = &units[range];
    if slice.is_empty() || (slice.len() == 1 && slice[0] < 0x80) {
        0
    } else {
        // units_value retains the Vec::with_capacity(3 * units) buffer.
        slice.len().saturating_mul(3)
    }
}

#[cfg(feature = "safe-sandbox")]
fn regexp_statics_materialization_bytes(
    matched: &regress::Match,
    mstart: usize,
    mend: usize,
    subject_units: usize,
    defer: bool,
    slice_bytes: impl Fn(std::ops::Range<usize>) -> usize,
) -> usize {
    if defer {
        return 0;
    }
    let mut bytes = slice_bytes(mstart..mend)
        .saturating_add(
            matched
                .captures
                .iter()
                .rev()
                .find_map(|capture| capture.clone())
                .map_or(0, &slice_bytes),
        )
        .saturating_add(slice_bytes(0..mstart))
        .saturating_add(slice_bytes(mend..subject_units));
    for capture in matched.captures.iter().take(9).flatten() {
        bytes = bytes.saturating_add(slice_bytes(capture.clone()));
    }
    bytes
}

/// Resident payload of the safe-profile ObjMap built for named captures.
/// Default data attributes normally stay in PropAttrs::AllData; charge a full
/// PropAttr column anyway so the runtime attr-elision rollback switch cannot
/// weaken the bound. Once the map crosses PROP_INDEX_THRESHOLD its safe lookup
/// index is a B-tree; ObjMap's heap audit deliberately charges 128 bytes per
/// entry plus its cloned key, and this preflight mirrors that accounting.
#[cfg(feature = "safe-sandbox")]
fn regexp_named_objmap_bytes(matched: &regress::Match) -> usize {
    let named_count = matched.named_groups().len();
    if named_count == 0 {
        return 0;
    }
    let name_bytes = matched
        .named_groups()
        .fold(0usize, |sum, (name, _)| sum.saturating_add(name.len()));
    let mut bytes = std::mem::size_of::<ObjMap>()
        .saturating_add(
            named_count.saturating_mul(
                std::mem::size_of::<String>()
                    .saturating_add(std::mem::size_of::<Value>())
                    .saturating_add(std::mem::size_of::<PropAttr>()),
            ),
        )
        .saturating_add(name_bytes);
    if named_count >= crate::heap::PROP_INDEX_THRESHOLD {
        bytes = bytes
            .saturating_add(std::mem::size_of::<std::collections::BTreeMap<String, u32>>())
            .saturating_add(named_count.saturating_mul(128))
            .saturating_add(name_bytes);
    }
    bytes
}

#[cfg(feature = "safe-sandbox")]
fn regexp_result_materialization_bytes(
    matched: &regress::Match,
    mstart: usize,
    mend: usize,
    has_indices: bool,
    slice_bytes: impl Fn(std::ops::Range<usize>) -> usize,
) -> usize {
    let captures = matched.captures.len();
    let mut bytes = slice_bytes(mstart..mend).saturating_add(
        matched
            .captures
            .iter()
            .flatten()
            .fold(0usize, |sum, range| {
                sum.saturating_add(slice_bytes(range.clone()))
            }),
    );

    // Result-array backing plus the temporary named-group table. Named string
    // values reuse their indexed capture Value in regexp_build_result below.
    bytes = bytes.saturating_add(
        captures
            .saturating_add(1)
            .saturating_mul(std::mem::size_of::<Value>()),
    );
    let named = matched.named_groups();
    let named_count = named.len();
    bytes = bytes.saturating_add(
        named_count.saturating_mul(std::mem::size_of::<(String, Option<std::ops::Range<usize>>)>()),
    );
    bytes = bytes.saturating_add(
        matched
            .named_groups()
            .fold(0usize, |sum, (name, _)| sum.saturating_add(name.len())),
    );
    bytes = bytes.saturating_add(regexp_named_objmap_bytes(matched));

    if has_indices {
        let participating = matched.captures.iter().flatten().count();
        let named_participating = matched.named_groups().filter(|(_, r)| r.is_some()).count();
        bytes = bytes
            .saturating_add(
                captures
                    .saturating_add(1)
                    .saturating_mul(std::mem::size_of::<Value>()),
            )
            .saturating_add(
                participating
                    .saturating_add(named_participating)
                    .saturating_add(1)
                    .saturating_mul(2)
                    .saturating_mul(std::mem::size_of::<Value>()),
            )
            .saturating_add(regexp_named_objmap_bytes(matched));
    }
    bytes
}

/// The prototype/constructor half of `regexp_matchall_fast_ok`, resolved to
/// SLOT INDICES (B68 item 2). The full gate re-found `flags`/`exec`/
/// `constructor`/`@@match` on the ~20-key %RegExp.prototype% plus `@@species`
/// on %RegExp% with hashed `pos()` scans on EVERY `matchAll()` call; the
/// slots cannot move without a version bump, so once resolved the warm
/// re-proof is version compares plus direct slot reads. (The five instance
/// probes stay uncached — in the pristine case they short-circuit behind a
/// single `arr_props` miss.)
///
/// Guarded exactly like [`super::async_runtime::PromisePristineSlots`]: the
/// heap's index-parallel `versions` array proves the slot indices still name
/// their keys (key add/delete, `defineProperty`, `Heap::replace` and GC slot
/// reuse all bump). What a version does NOT guard — a plain in-place
/// `vals[i] = v` data write bumps nothing (B67/B110) — is never trusted from
/// the cache: the accessor bit and the value identity at each slot are
/// re-read on every call, with each pinned native's own version standing in
/// for a `heap.get` (only `Heap::replace`/GC reuse can change a `Native`,
/// and both bump). On any mismatch the full gate re-runs and re-resolves:
/// conservative fallback, never a wrong answer.
#[derive(Clone, Copy)]
pub(crate) struct MatchallFastSlots {
    /// `heap.versions[regexp_proto]` / `heap.versions[regexp_ctor]` at fill.
    proto_version: u32,
    ctor_version: u32,
    /// `(slot, value heap index, value version)` for the pinned intrinsics:
    /// `flags` plus its eight component accessors / `exec` / `@@match` on the
    /// prototype, and `@@species` (accessor) on %RegExp%.
    flags: (u32, u32, u32),
    flag_accessors: [(u32, u32, u32); 8],
    exec: (u32, u32, u32),
    matchsym: (u32, u32, u32),
    species: (u32, u32, u32),
    /// `constructor`'s slot — its target is the `regexp_ctor` anchor itself,
    /// re-compared by identity per call, so nothing else needs pinning.
    ctor_slot: u32,
}

/// `ZIPP_NO_FASTOK_MEMO=1` makes `regexp_matchall_fast_ok_cached` run the
/// original nine-probe gate on every call, bypassing the slot memo entirely —
/// the rollback switch and one side of a one-binary A/B (`tools/bench.py
/// --ab-env`). Same idiom as `ZIPP_NO_PROMISE_SLOT_CACHE`.
#[inline]
fn fastok_memo_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_FASTOK_MEMO").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_MATCHALL_STEP=1` disables the fused %RegExpStringIterator% STEP
/// (B118): every step runs the full observable protocol re-proof again — the
/// rollback switch and one side of a one-binary A/B, same idiom as
/// `ZIPP_NO_FASTOK_MEMO`.
#[inline]
#[cfg(feature = "safe-sandbox")]
fn matchall_step_enabled() -> bool {
    // This fused path has no error channel for a host-resource failure. The
    // safe profile always falls through to the checked core executor.
    false
}

#[inline]
#[cfg(not(feature = "safe-sandbox"))]
fn matchall_step_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_MATCHALL_STEP").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_SLIM_EXEC=1` disables the B124 slim per-call exec: the fused
/// matchAll step goes back through the full `regexp_exec_impl_prebits`
/// protocol (duplicate lastIndex read + ToInteger, per-step flatten/is_ascii/
/// str_units heap.gets, per-step twin probe, result-array empty-match probe),
/// and the pristine exec's flag decode goes back to the four `contains`
/// scans. The rollback switch and one side of a one-binary A/B
/// (`tools/bench.py --ab-env`), same idiom as `ZIPP_NO_MATCHALL_STEP`.
#[inline]
fn slim_exec_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_SLIM_EXEC").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_TWIN_AT_CREATE=1` stops the fused matchAll creation arm from
/// building the SOURCE regex's `ascii_twin` up front: the first slim step's
/// cold arm builds it on the per-call MATCHER instead — which dies with the
/// iteration, so a source only ever used via `matchAll` re-pays the full
/// `ensure_regexp_ascii_twin` body (compile-cps vec builds, cache-key
/// materialisation, SipHash probe) on every creation. The rollback switch and
/// one side of a one-binary A/B, same idiom as `ZIPP_NO_SLIM_EXEC`.
#[inline]
pub(crate) fn twin_at_create_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_TWIN_AT_CREATE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_ITER_SUBJ_UNITS=1` makes the fused slim exec ignore the iterator
/// record's creation-time subject length ([`RegexpIterRec::subj_units`]) and
/// re-derive `subj.len()` inside the search loop, as before — the rollback
/// switch and one side of a one-binary A/B, same idiom as `ZIPP_NO_SLIM_EXEC`.
/// The batched matchAll path consumes the cached field unconditionally, so a
/// faithful A/B of the cached length requires `ZIPP_NO_MATCHALL_BATCH=1` too.
#[inline]
fn iter_subj_units_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_ITER_SUBJ_UNITS").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_MATCHALL_BATCH=1` disables the fused matchAll DRAIN (one
/// host-side scan serving up to [`MATCHALL_BATCH_CAP`] steps): every fused
/// step goes back to the one-shot B124 slim exec, re-paying the per-step
/// executor construction and scan-session setup. The rollback switch and one
/// side of a one-binary A/B, same idiom as `ZIPP_NO_SLIM_EXEC`.
#[inline]
fn matchall_batch_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_MATCHALL_BATCH").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Flag-bit layout of the `regexp_string_iters` record's `u8` (computed ONCE
/// at iterator creation): `global`/`fullUnicode` are what
/// CreateRegExpStringIterator captures per spec; the rest exist so the fused
/// step (B118) never re-derives them from the matcher's flags string.
/// `ITFB_FUSED` is only set by the pristine-clone creation arm, whose matcher
/// is ENGINE-INTERNAL (no user reference can ever exist), over a flat-ASCII
/// subject with a numeric `lastIndex`.
pub(crate) const ITFB_GLOBAL: u8 = 1 << 0;
pub(crate) const ITFB_UNICODE: u8 = 1 << 1;
pub(crate) const ITFB_FUSED: u8 = 1 << 2;
pub(crate) const ITFB_STICKY: u8 = 1 << 3;
pub(crate) const ITFB_INDICES: u8 = 1 << 4;

/// One not-yet-observable matchAll result. Offsets are byte indices into the
/// iterator record's immutable flat-ASCII subject; `u32::MAX` in both capture
/// cells denotes an unparticipating group. The exact plan admits at most four
/// captures, so this contains no heap Values and adds no GC tracing work.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
pub(crate) struct RegexpScalarMatch {
    mstart: u32,
    mend: u32,
    ncaps: u8,
    caps: [u32; 8],
}

/// One not-yet-observable result from the exact non-global exec region.  The
/// subject is an explicit VM root; all remaining fields are byte ranges into
/// that immutable flat-ASCII string.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
pub(crate) struct RegexpScalarExecPending {
    pub(crate) subject: Value,
    subj_units: u32,
    matched: RegexpScalarMatch,
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) enum RegexpScalarExecStep {
    Success([Value; 4]),
    Miss,
    Decline,
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) enum RegexpScalarStep {
    Success,
    Done,
    Decline,
}

/// One lazy %RegExpStringIterator%'s state — the value of
/// `Vm::regexp_string_iters`, keyed by the iterator's heap index. Its
/// `next()` drives RegExpExec (honouring a user `exec`) one match at a time,
/// rather than matchAll eagerly collecting every match up front.
pub(crate) struct RegexpIterRec {
    /// The matcher regexp's heap index — a SEPARATE object from the source
    /// regex, so the iteration advances its `lastIndex` independently.
    pub matcher: u32,
    /// The subject string.
    pub subject: Value,
    /// The subject's unit length, captured at creation. Strings are immutable
    /// and Cons→Str flattening preserves the unit count, so it can never go
    /// stale; it is load-bearing only on the `ITFB_FUSED` path (flat-ASCII
    /// subject, units == bytes), where the slim exec reads it instead of
    /// re-deriving `subj.len()` per search-loop pass. `usize` deliberately:
    /// a >4GB subject needs no `ITFB_FUSED` demotion (the eager >u32 statics
    /// arm keeps working).
    pub subj_units: usize,
    /// The `ITFB_*` flag bits above, computed ONCE at creation.
    pub fbits: u8,
    /// Done latch: set by a null result or the single match of a non-global
    /// regex; every later step answers `(undefined, true)`.
    pub done: bool,
    /// Range-only result held strictly inside an active exact scalar region.
    /// Every native exit/re-entry materialises and clears it.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) scalar_pending: Option<RegexpScalarMatch>,
}

/// Matches served per drain of the fused matchAll batch. The drain runs at
/// the FIRST step (never at `matchAll()` itself, which stays lazy), and the
/// cap bounds the wasted scan when a consumer breaks early.
const MATCHALL_BATCH_CAP: usize = 16;

/// One drained matchAll scan for a live `ITFB_FUSED` iterator — the value of
/// `Vm::matchall_batches`, keyed by the SAME iterator heap index as the
/// paired `regexp_string_iters` record and pruned alongside it. A PURE memo
/// of integers ("scanning this immutable flat-ASCII subject with this fixed
/// program from `expected_li` onward yields these ranges"), so GC never
/// traces it — the paired record roots the matcher and subject. Everything
/// OBSERVABLE stays per-step: `lastIndex` writes, Annex-B statics and the
/// result array are produced by `Vm::fused_publish` at each `next()`, never
/// at drain time.
pub(crate) struct MatchBatch {
    /// The matcher `lastIndex` the next unconsumed triple was drained at. Any
    /// divergence (a fallback round ran a user `exec` mid-iteration and moved
    /// the heap slot) invalidates the memo — the step re-drains from the live
    /// position.
    expected_li: u32,
    /// Next unconsumed triple.
    next: u16,
    /// Capture-group count; the triple stride is `2 + 2 * ncaps`.
    ncaps: u16,
    /// The drain hit the end of the subject: no match exists past the last
    /// triple, so consuming them all makes the NEXT step the done protocol
    /// (a consumed batch WITHOUT this bit re-drains instead).
    exhausted: bool,
    /// `[start, end, cap0.start, cap0.end, ..]` per match, stride
    /// `2 + 2 * ncaps`; `u32::MAX` = unparticipating capture.
    flat: Vec<u32>,
}

impl<'p> Vm<'p> {
    /// Capture the final setup-time main-realm RegExp protocol functions. Called
    /// once after the species accessor is installed; the resulting indices are
    /// permanent GC roots and remain the identity anchors after user mutation.
    pub(crate) fn capture_regexp_protocol_intrinsics(&mut self) {
        let mut exact = [0; REGEXP_PROTOCOL_INTRINSIC_COUNT];
        if let HeapObj::Object(proto) = self.heap.get(self.regexp_proto) {
            for &(name, accessor, native_id) in &REGEXP_PROTOCOL_PROTO_SLOTS {
                let Some(slot) = proto.pos(name) else {
                    continue;
                };
                let value = proto.val_at(slot);
                if proto.attr_at(slot).accessor == accessor
                    && value.is_heap()
                    && matches!(self.heap.get(value.heap_index()), HeapObj::Native(id) if *id == native_id)
                {
                    exact[regexp_protocol_intrinsic_index(native_id).unwrap()] = value.heap_index();
                }
            }
        }
        if let HeapObj::Object(ctor) = self.heap.get(self.regexp_ctor) {
            if let Some(slot) = ctor.pos("@@species") {
                let value = ctor.val_at(slot);
                if ctor.attr_at(slot).accessor
                    && value.is_heap()
                    && matches!(self.heap.get(value.heap_index()), HeapObj::Native(id) if *id == native::SPECIES_GET)
                {
                    exact[REGEXP_PROTOCOL_SPECIES] = value.heap_index();
                }
            }
        }
        self.regexp_protocol_intrinsics = exact;
    }

    #[inline]
    pub(crate) fn regexp_protocol_value_is_intrinsic(&self, value: Value, native_id: u16) -> bool {
        regexp_protocol_intrinsic_index(native_id).is_some_and(|index| {
            self.bare_builtin_is_intrinsic(self.regexp_protocol_intrinsics[index], value)
        })
    }

    #[inline]
    pub(crate) fn regexp_proto_slot_is_intrinsic(
        &self,
        proto: &ObjMap,
        name: &str,
        accessor: bool,
        native_id: u16,
    ) -> bool {
        proto.pos(name).is_some_and(|slot| {
            proto.attr_at(slot).accessor == accessor
                && self.regexp_protocol_value_is_intrinsic(proto.val_at(slot), native_id)
        })
    }

    /// `new Proxy(target, handler)` — both must be objects.
    pub(crate) fn make_proxy(&mut self, target: Value, handler: Value) -> Result<Value, Thrown> {
        if !self.is_object_value(target) || !self.is_object_value(handler) {
            return Err(Thrown(
                "TypeError: Cannot create proxy with a non-object as target or handler".into(),
            ));
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Proxy {
            target,
            handler,
            revoked: false,
        })))
    }

    pub(crate) fn proxy_parts(&self, idx: u32) -> Option<(Value, Value, bool)> {
        match self.heap.get(idx) {
            HeapObj::Proxy {
                target,
                handler,
                revoked,
            } => Some((*target, *handler, *revoked)),
            _ => None,
        }
    }

    /// Reconstruct a property KEY as a Value (a Symbol for an `@@`-encoded key,
    /// else a string) — so a Proxy trap / Reflect receives the real key.
    pub(crate) fn key_to_value(&mut self, key: &str) -> Value {
        if key.starts_with("@@") {
            if let Some(&sym) = self.symbol_keys.get(key) {
                return sym;
            }
        }
        self.alloc_str(key.to_string())
    }

    /// Look up a Proxy handler trap by name; `Ok(Some(fn))` if it's callable,
    /// `Ok(None)` to fall through to the target. A non-callable non-undefined trap
    /// is a TypeError. (`revoked` is checked by the caller.)
    pub(crate) fn proxy_trap(
        &mut self,
        handler: Value,
        name: &str,
    ) -> Result<Option<Value>, Thrown> {
        let t = self.get_prop(handler, name)?;
        if t.is_undefined() || t.is_null() {
            Ok(None)
        } else if self.is_callable(t) {
            Ok(Some(t))
        } else {
            Err(Thrown(format!(
                "TypeError: proxy handler's {name} trap is not a function"
            )))
        }
    }

    pub(crate) fn set_regexp_last_index(&mut self, idx: u32, n: usize) {
        if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(idx) {
            *last_index = Value::num(n as f64);
        }
    }

    /// Whether a RegExp's struct-backed `lastIndex` is writable. A
    /// `defineProperty` records the cleared flag in `arr_props` — but so does
    /// `Object.freeze(re)`, which runs DefinePropertyOrThrow over every own key
    /// and `lastIndex` is the only one a RegExp has. Because the slot lives in
    /// the struct rather than in the side table, freeze left no per-key entry
    /// behind and the flag read as writable: a frozen global regex silently
    /// advanced `lastIndex` instead of throwing.
    pub(crate) fn regexp_last_index_writable(&self, idx: u32) -> bool {
        self.arr_props.get(&idx).map_or(true, |m| {
            !m.frozen && m.pos("lastIndex").map_or(true, |i| m.attr_at(i).writable)
        })
    }

    /// True when `String.prototype.replace`'s internal regex fast path is
    /// UNOBSERVABLE for instance `re`: its [[Prototype]] is exactly
    /// %RegExp.prototype%, it has no own exec/flags/@@replace overrides, and
    /// the prototype's `exec` / `@@replace` are still the intrinsic natives.
    /// Anything else (a subclass instance, a patched prototype) must run the
    /// full observable @@replace protocol.
    pub(crate) fn regexp_replace_fast_ok(&self, re: u32) -> bool {
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self.arr_props.get(&re).is_some_and(|m| {
            m.pos("exec").is_some()
                || m.pos("flags").is_some()
                || m.pos("@@replace").is_some()
                || Self::FLAG_ACCESSORS
                    .iter()
                    .any(|(name, _)| m.pos(name).is_some())
        }) {
            return false;
        }
        match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => {
                let intrinsic =
                    |k: &str, id: u16| self.regexp_proto_slot_is_intrinsic(m, k, false, id);
                if !(intrinsic("exec", native::REGEXP_EXEC)
                    && intrinsic("@@replace", native::REGEXP_SYM_REPLACE))
                {
                    return false;
                }
            }
            _ => return false,
        }
        // The fast path starts matching at 0 and does not write `lastIndex`
        // back, which is only unobservable for a NON-sticky pattern whose
        // `lastIndex` is already 0. `@@replace` reads `lastIndex` for a sticky
        // regex and, when global, sets it to 0 before matching and leaves it
        // there — `"aaaa".replace(/a/g, "b")` must end with `lastIndex === 0`,
        // and a sticky `re` with `lastIndex === 5` must resume at 5.
        let (sticky, global, last_index) = match self.heap.get(re) {
            HeapObj::RegExp {
                flags, last_index, ..
            } => (flags.contains('y'), flags.contains('g'), *last_index),
            _ => return false,
        };
        if sticky {
            return false;
        }
        // `-0` compares equal to `+0`, but a GLOBAL @@replace performs
        // `Set(rx, "lastIndex", +0, true)` before matching.  Skipping that
        // write would leave an observably negative zero behind
        // (`Object.is(rx.lastIndex, -0)`).  A non-global replace never writes
        // lastIndex, so retaining -0 is correct there.
        if !last_index.is_number()
            || last_index.as_f64() != 0.0
            || (global && last_index.as_f64().is_sign_negative())
        {
            return false;
        }
        // `@@replace` step 8.b is `Set(rx, "lastIndex", 0, true)` for a GLOBAL
        // regex. The fast path skips it because `lastIndex` is already 0 — which
        // is unobservable only while the property is writable; on a frozen regex
        // that Set is a TypeError, and the fast path swallowed it.
        if global && !self.regexp_last_index_writable(re) {
            return false;
        }
        // `@@replace` reads `flags`; the intrinsic flags getter in turn reads
        // all eight flag accessors. Prove every live accessor's exact Native id
        // (not merely "some native": swapping e.g. the `global` and `unicode`
        // getters is observable). This is allocation-free, so a direct-call
        // decline remains a pure prefix.
        match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => {
                let accessor = |name: &str, want: u16| {
                    self.regexp_proto_slot_is_intrinsic(m, name, true, want)
                };
                accessor("flags", native::REGEXP_GET_FLAGS)
                    && Self::FLAG_ACCESSORS
                        .iter()
                        .all(|(name, want)| accessor(name, *want))
            }
            _ => false,
        }
    }

    /// UNOBSERVABLE to build `@@matchAll`'s matcher by direct clone for instance
    /// `re`: [[Prototype]] is exactly %RegExp.prototype%, no own
    /// `flags`/`constructor`/`lastIndex`-shadowing overrides, the prototype's
    /// `flags` accessor and `exec` are still intrinsic, `constructor` is still
    /// the %RegExp% intrinsic, and `RegExp[@@species]` is still the default
    /// accessor.
    ///
    /// The spec path is expensive because it is fully observable: Get(R,"flags")
    /// through the accessor, ToString it, Get(R,"constructor"), Get(C,"@@species"),
    /// then Construct(C, «R, flags») — which reparses/relooks-up the pattern —
    /// plus Get(R,"lastIndex"). That is ~6 property lookups, 3 string allocations
    /// and a full RegExp construction, measured at 1.2us per `matchAll` call
    /// before a single match is attempted (node: 47ns). When every one of those
    /// steps is guaranteed to return the intrinsic, the whole sequence is
    /// equivalent to cloning the compiled regex.
    pub(crate) fn regexp_matchall_fast_ok(&self, re: u32) -> bool {
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self.arr_props.get(&re).is_some_and(|m| {
            m.pos("flags").is_some()
                || m.pos("constructor").is_some()
                || m.pos("exec").is_some()
                || m.pos("@@matchAll").is_some()
                // `@@match` is observable from this path even though the clone
                // never matches with it: the spec builds the matcher via
                // Construct(C, «R, flags»), and the RegExp constructor's step 1
                // is IsRegExp(pattern) — a Get of `@@match`. Cloning skips the
                // construction and so skips that Get.
                || m.pos("@@match").is_some()
        }) {
            return false;
        }
        if self.regexp_ctor == 0 {
            return false;
        }
        if !self.regexp_pristine_flag_accessors_ok(re, Value::heap(re)) {
            return false;
        }
        // %RegExp.prototype%: `flags` still the intrinsic accessor, `exec` still
        // the intrinsic native, `constructor` still %RegExp%.
        let proto_ok = match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => {
                let exec_ok =
                    self.regexp_proto_slot_is_intrinsic(m, "exec", false, native::REGEXP_EXEC);
                let ctor_ok = m.pos("constructor").is_some_and(|i| {
                    !m.attr_at(i).accessor
                        && m.val_at(i).is_heap()
                        && m.val_at(i).heap_index() == self.regexp_ctor
                });
                // `@@match` still the intrinsic data property. Replacing it with
                // a GETTER makes the construction the fast path elides observable
                // (see the own-prop check above), and a plain replacement value
                // changes what IsRegExp answers inside the RegExp constructor.
                let match_ok = self.regexp_proto_slot_is_intrinsic(
                    m,
                    "@@match",
                    false,
                    native::REGEXP_SYM_MATCH,
                );
                exec_ok && ctor_ok && match_ok
            }
            _ => false,
        };
        if !proto_ok {
            return false;
        }
        // %RegExp%[@@species] still the default accessor (never replaced).
        match self.heap.get(self.regexp_ctor) {
            HeapObj::Object(m) => m.pos("@@species").is_some_and(|i| {
                m.attr_at(i).accessor
                    && self.regexp_protocol_value_is_intrinsic(m.val_at(i), native::SPECIES_GET)
            }),
            _ => false,
        }
    }

    /// `regexp_matchall_fast_ok` answered from the resolved slots when they
    /// are warm (version compares + slot reads — see [`MatchallFastSlots`]);
    /// a cold or invalidated memo re-runs the prototype/constructor half of
    /// the full gate once and re-resolves. The instance half (proto identity,
    /// own-shadow probes, the ctor anchor) is instance-specific and cheap, so
    /// it runs uncached per call, read-for-read the gate's opening.
    pub(crate) fn regexp_matchall_fast_ok_cached(&mut self, re: u32) -> bool {
        if !fastok_memo_enabled() {
            return self.regexp_matchall_fast_ok(re);
        }
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self.arr_props.get(&re).is_some_and(|m| {
            m.pos("flags").is_some()
                || m.pos("constructor").is_some()
                || m.pos("exec").is_some()
                || m.pos("@@matchAll").is_some()
                || m.pos("@@match").is_some()
        }) {
            return false;
        }
        if self.regexp_ctor == 0 {
            return false;
        }
        if self.matchall_fast_from_slots() {
            return true;
        }
        // Cold memo, or a guarded version moved: run the shared half of the
        // full gate and capture the slots for the next call. `None` (not
        // pristine) leaves every call on the full re-proof — exactly the
        // pre-memo behavior, and those calls take the observable protocol
        // anyway.
        let slots = self.matchall_fast_resolve_slots();
        self.matchall_fast_slots = slots;
        slots.is_some()
    }

    /// Answer the shared pristine question from the resolved slots: `true`
    /// only when every guard holds. Any mismatch — a moved version, a slot no
    /// longer naming its key, an in-place overwrite, a flipped accessor bit —
    /// declines to the full re-proof rather than reasoning about it (unlike
    /// the promise cache there is no fast `false` here: a DIFFERENT value in
    /// a slot could still be an equivalent intrinsic identity).
    #[inline]
    fn matchall_fast_from_slots(&self) -> bool {
        let Some(c) = self.matchall_fast_slots else {
            return false;
        };
        if self.heap.version_of(self.regexp_proto) != c.proto_version
            || self.heap.version_of(self.regexp_ctor) != c.ctor_version
        {
            return false;
        }
        // Belt-and-braces key checks as in the promise cache: the versions
        // say the layout is unchanged; verify the slots still name their keys
        // anyway, so an un-bumped structural change could only ever cost a
        // re-proof, never a wrong answer.
        let pinned = |m: &ObjMap, key: &str, accessor: bool, (slot, idx, ver): (u32, u32, u32)| {
            let s = slot as usize;
            m.keys.get(s).is_some_and(|k| k == key)
                && m.attr_at(s).accessor == accessor
                && m.val_at(s).is_heap()
                && m.val_at(s).heap_index() == idx
                // The same object, un-replaced since fill proved it the right
                // `Native` — no `heap.get` needed.
                && self.heap.version_of(idx) == ver
        };
        let m = match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => m,
            // Unreachable under a matching version (`Heap::replace` bumps).
            _ => return false,
        };
        if !(pinned(m, "flags", true, c.flags)
            && Self::FLAG_ACCESSORS
                .iter()
                .zip(c.flag_accessors.iter())
                .all(|((name, _), guard)| pinned(m, name, true, *guard))
            && pinned(m, "exec", false, c.exec)
            && pinned(m, "@@match", false, c.matchsym))
        {
            return false;
        }
        let cs = c.ctor_slot as usize;
        if m.keys.get(cs).map_or(true, |k| k != "constructor")
            || m.attr_at(cs).accessor
            || !m.val_at(cs).is_heap()
            || m.val_at(cs).heap_index() != self.regexp_ctor
        {
            return false;
        }
        match self.heap.get(self.regexp_ctor) {
            HeapObj::Object(mc) => pinned(mc, "@@species", true, c.species),
            _ => false,
        }
    }

    /// Run the shared (prototype/constructor) half of the full gate —
    /// read-for-read the same checks as `regexp_matchall_fast_ok` past its
    /// instance probes — and, when it holds, capture the slot indices plus
    /// the version of every object the proof read. `Some` is "pristine, and
    /// how to re-check it warm"; `None` is "not pristine".
    fn matchall_fast_resolve_slots(&self) -> Option<MatchallFastSlots> {
        let pin = |m: &ObjMap, key: &str, accessor: bool, id: u16| {
            let i = m.pos(key)?;
            let v = m.val_at(i);
            (m.attr_at(i).accessor == accessor && self.regexp_protocol_value_is_intrinsic(v, id))
                .then(|| {
                    (
                        i as u32,
                        v.heap_index(),
                        self.heap.version_of(v.heap_index()),
                    )
                })
        };
        let m = match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => m,
            _ => return None,
        };
        let flags = pin(m, "flags", true, native::REGEXP_GET_FLAGS)?;
        let mut flag_accessors = [(0, 0, 0); 8];
        for (out, &(name, native_id)) in flag_accessors.iter_mut().zip(Self::FLAG_ACCESSORS.iter())
        {
            *out = pin(m, name, true, native_id)?;
        }
        let exec = pin(m, "exec", false, native::REGEXP_EXEC)?;
        let matchsym = pin(m, "@@match", false, native::REGEXP_SYM_MATCH)?;
        let ctor_slot = m.pos("constructor").filter(|&i| {
            !m.attr_at(i).accessor
                && m.val_at(i).is_heap()
                && m.val_at(i).heap_index() == self.regexp_ctor
        })? as u32;
        let mc = match self.heap.get(self.regexp_ctor) {
            HeapObj::Object(mc) => mc,
            _ => return None,
        };
        let species = pin(mc, "@@species", true, native::SPECIES_GET)?;
        Some(MatchallFastSlots {
            proto_version: self.heap.version_of(self.regexp_proto),
            ctor_version: self.heap.version_of(self.regexp_ctor),
            flags,
            flag_accessors,
            exec,
            matchsym,
            species,
            ctor_slot,
        })
    }

    /// The heap index if `v` is a RegExp, else None.
    pub(crate) fn as_regexp(&self, v: Value) -> Option<u32> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::RegExp { .. }) {
            Some(v.heap_index())
        } else {
            None
        }
    }

    /// Coerce a `String.prototype.match`/`search` argument to a RegExp: a RegExp
    /// passes through; anything else becomes `new RegExp(arg)`.
    pub(crate) fn to_regexp_arg(&mut self, v: Value) -> Result<u32, Thrown> {
        if let Some(i) = self.as_regexp(v) {
            return Ok(i);
        }
        let p = if v.is_undefined() {
            self.alloc_str(String::new())
        } else {
            v
        };
        Ok(self.build_regexp(p, Value::UNDEFINED)?.heap_index())
    }

    /// Expand a `String.prototype.replace` string template against a match: `$&`
    /// (whole), `` $` ``/`$'` (pre/post), `$N`/`$NN` (group), `$<name>` (named), `$$`.
    pub(crate) fn expand_replacement(
        &self,
        tmpl: &str,
        whole: &str,
        groups: &[Option<String>],
        named: &[(String, Option<String>)],
        named_defined: bool,
        pre: &str,
        post: &str,
        limit: usize,
    ) -> Result<String, Thrown> {
        // `limit` caps the output in BYTES: a `$1`-heavy template applied to a
        // huge capture would otherwise build an unbounded string (hang / OOM —
        // staging/sm/String/replace-math.js). Same 2^28 bound as "repeat".
        let mut out = String::with_capacity(tmpl.len().min(limit));
        macro_rules! push {
            ($s:expr) => {{
                let s: &str = $s;
                if s.len() > limit - out.len() {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                out.push_str(s);
            }};
        }
        let bytes = tmpl.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && i + 1 < bytes.len() {
                let c = bytes[i + 1];
                match c {
                    b'$' => {
                        push!("$");
                        i += 2;
                    }
                    b'&' => {
                        push!(whole);
                        i += 2;
                    }
                    b'`' => {
                        push!(pre);
                        i += 2;
                    }
                    b'\'' => {
                        push!(post);
                        i += 2;
                    }
                    b'<' => {
                        // `$<name>` substitutes the named capture (or "" if absent)
                        // when named captures are present; otherwise (no groups
                        // object / namedCaptures undefined) "$<" is a literal.
                        if !named_defined {
                            push!("$");
                            i += 1;
                        } else if let Some(end) = tmpl[i + 2..].find('>') {
                            let name = &tmpl[i + 2..i + 2 + end];
                            if let Some((_, Some(g))) = named.iter().find(|(n, _)| n == name) {
                                push!(g);
                            }
                            i += 2 + end + 1;
                        } else {
                            push!("$");
                            i += 1;
                        }
                    }
                    b'0'..=b'9' => {
                        // One or two digits; prefer the two-digit group if valid.
                        let d1 = (c - b'0') as usize;
                        let two = if i + 2 < bytes.len() && bytes[i + 2].is_ascii_digit() {
                            Some(d1 * 10 + (bytes[i + 2] - b'0') as usize)
                        } else {
                            None
                        };
                        if let Some(n) = two.filter(|&n| n >= 1 && n <= groups.len()) {
                            if let Some(g) = &groups[n - 1] {
                                push!(g);
                            }
                            i += 3;
                        } else if d1 >= 1 && d1 <= groups.len() {
                            if let Some(g) = &groups[d1 - 1] {
                                push!(g);
                            }
                            i += 2;
                        } else {
                            push!("$");
                            i += 1;
                        }
                    }
                    _ => {
                        push!("$");
                        i += 1;
                    }
                }
            } else {
                // copy one UTF-8 char
                let ch = tmpl[i..].chars().next().unwrap();
                let mut b = [0u8; 4];
                push!(ch.encode_utf8(&mut b));
                i += ch.len_utf8();
            }
        }
        Ok(out)
    }

    /// Safe-profile `GetSubstitution`: identical parsing to
    /// `expand_replacement`, with every retained byte charged and every growth
    /// fallible before the caller appends the result to its aggregate output.
    #[cfg(feature = "safe-sandbox")]
    #[allow(clippy::too_many_arguments)]
    fn expand_replacement_safe(
        &mut self,
        reservation: &mut super::instrument::RegexTransientReservation,
        tmpl: &str,
        whole: &str,
        groups: &[Option<String>],
        named: &[(String, Option<String>)],
        named_defined: bool,
        pre: &str,
        post: &str,
        limit: usize,
    ) -> Result<String, Thrown> {
        let mut out = String::new();
        regex_try_reserve_string_exact(self, reservation, &mut out, tmpl.len().min(limit))?;
        macro_rules! push {
            ($s:expr) => {{
                let s: &str = $s;
                if s.len() > limit.saturating_sub(out.len()) {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                regex_try_reserve_string_geometric(self, reservation, &mut out, s.len())?;
                out.push_str(s);
            }};
        }
        let bytes = tmpl.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && i + 1 < bytes.len() {
                let c = bytes[i + 1];
                match c {
                    b'$' => {
                        push!("$");
                        i += 2;
                    }
                    b'&' => {
                        push!(whole);
                        i += 2;
                    }
                    b'`' => {
                        push!(pre);
                        i += 2;
                    }
                    b'\'' => {
                        push!(post);
                        i += 2;
                    }
                    b'<' => {
                        if !named_defined {
                            push!("$");
                            i += 1;
                        } else if let Some(end) = tmpl[i + 2..].find('>') {
                            let name = &tmpl[i + 2..i + 2 + end];
                            if let Some((_, Some(group))) = named.iter().find(|(n, _)| n == name) {
                                push!(group);
                            }
                            i += 2 + end + 1;
                        } else {
                            push!("$");
                            i += 1;
                        }
                    }
                    b'0'..=b'9' => {
                        let first = (c - b'0') as usize;
                        let two = if i + 2 < bytes.len() && bytes[i + 2].is_ascii_digit() {
                            Some(first * 10 + (bytes[i + 2] - b'0') as usize)
                        } else {
                            None
                        };
                        if let Some(n) = two.filter(|&n| n >= 1 && n <= groups.len()) {
                            if let Some(group) = &groups[n - 1] {
                                push!(group);
                            }
                            i += 3;
                        } else if first >= 1 && first <= groups.len() {
                            if let Some(group) = &groups[first - 1] {
                                push!(group);
                            }
                            i += 2;
                        } else {
                            push!("$");
                            i += 1;
                        }
                    }
                    _ => {
                        push!("$");
                        i += 1;
                    }
                }
            } else {
                let ch = tmpl[i..].chars().next().expect("template character exists");
                let mut encoded = [0u8; 4];
                push!(ch.encode_utf8(&mut encoded));
                i += ch.len_utf8();
            }
        }
        Ok(out)
    }

    /// RegExp instance property reads: `lastIndex`, `source` (empty → "(?:)"),
    /// `flags`, and the per-flag booleans; methods delegate to RegExp.prototype.
    /// EscapeRegExpPattern: render `source` so it round-trips between two `/`
    /// delimiters — escape a bare `/` and the line terminators, pass `\x` pairs
    /// through verbatim, and map the empty pattern to `(?:)`.
    pub(crate) fn escaped_source(&self, source: &str) -> Result<String, Thrown> {
        if source.is_empty() {
            return Ok("(?:)".to_string());
        }
        let checked_add = |total: usize, additional: usize| {
            total
                .checked_add(additional)
                .filter(|&n| n <= MAX_STRING_BYTES)
                .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))
        };
        let mut total = 0usize;
        let mut chars = source.chars().peekable();
        let mut in_class = false;
        while let Some(c) = chars.next() {
            let additional = if c == '\\' && chars.peek().is_some() {
                let next = chars.next().expect("peeked character exists");
                1 + match next {
                    '\n' | '\r' => 1,
                    '\u{2028}' | '\u{2029}' => 5,
                    other => other.len_utf8(),
                }
            } else {
                match c {
                    '[' => {
                        in_class = true;
                        1
                    }
                    ']' => {
                        in_class = false;
                        1
                    }
                    '/' if !in_class => 2,
                    '\n' | '\r' => 2,
                    '\u{2028}' | '\u{2029}' => 6,
                    _ => c.len_utf8(),
                }
            };
            total = checked_add(total, additional)?;
        }
        let mut out = String::new();
        out.try_reserve_exact(total)
            .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
        // A `/` inside a character class needs no escape — RegularExpressionClassChar
        // admits it literally — and escaping it there made `new RegExp("[/]").source`
        // report `[\/]`. An unescaped `[` opens the class and the next unescaped `]`
        // closes it (classes do not nest for this purpose: `/[[]/]/` really does end
        // its class at the first `]`).
        let mut in_class = false;
        let mut chars = source.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' && chars.peek().is_some() {
                // An escape pair passes through UNCHANGED — except that the
                // escaped character may itself be a raw LineTerminator, and
                // EscapeRegExpPattern's whole job is that `eval("/" + source +
                // "/")` re-parses. Emitting `\` + a literal LF produced an
                // unterminated regular expression.
                out.push('\\');
                match chars.next().expect("peeked character exists") {
                    '\n' => out.push_str("n"),
                    '\r' => out.push_str("r"),
                    '\u{2028}' => out.push_str("u2028"),
                    '\u{2029}' => out.push_str("u2029"),
                    other => out.push(other),
                }
                continue;
            }
            match c {
                '[' => {
                    in_class = true;
                    out.push(c);
                }
                ']' => {
                    in_class = false;
                    out.push(c);
                }
                '/' if !in_class => out.push_str("\\/"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\u{2028}' => out.push_str("\\u2028"),
                '\u{2029}' => out.push_str("\\u2029"),
                _ => out.push(c),
            }
        }
        debug_assert_eq!(out.len(), total);
        Ok(out)
    }

    /// WTF-8 twin of [`escaped_source`], for a pattern holding lone surrogates
    /// (`regexp_exact_source` side table): operates on code points over the
    /// exact bytes — same escapes (`/`, line terminators, `\x` pairs verbatim,
    /// empty → `(?:)`) — and returns WTF-8 bytes for the WTF-8 string
    /// constructor. A lone surrogate passes through as itself (the spec's
    /// EscapeRegExpPattern leaves it untouched), which `escaped_source` could
    /// never produce from its lossy `&str` view.
    pub(crate) fn escaped_source_wtf8(&self, bytes: &[u8]) -> Result<Vec<u8>, Thrown> {
        if bytes.is_empty() {
            return Ok(b"(?:)".to_vec());
        }
        let cp_len = |cp: u32| match cp {
            0..=0x7f => 1,
            0x80..=0x7ff => 2,
            0x800..=0xffff => 3,
            _ => 4,
        };
        let mut total = 0usize;
        let mut cps = crate::heap::wtf8_code_points(bytes).peekable();
        let mut in_class = false;
        while let Some(c) = cps.next() {
            let additional = if c == u32::from('\\') && cps.peek().is_some() {
                cp_len(c) + cp_len(cps.next().expect("peeked code point exists"))
            } else {
                match c {
                    0x5B => {
                        in_class = true;
                        1
                    }
                    0x5D => {
                        in_class = false;
                        1
                    }
                    0x2F if !in_class => 2,
                    0x0A | 0x0D => 2,
                    0x2028 | 0x2029 => 6,
                    _ => cp_len(c),
                }
            };
            total = total
                .checked_add(additional)
                .filter(|&n| n <= MAX_STRING_BYTES)
                .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
        }
        let mut out: Vec<u8> = Vec::new();
        out.try_reserve_exact(total)
            .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
        // Same character-class rule as `escaped_source`: `/` is literal inside `[…]`.
        let mut in_class = false;
        let mut cps = crate::heap::wtf8_code_points(bytes).peekable();
        while let Some(c) = cps.next() {
            if c == u32::from('\\') && cps.peek().is_some() {
                crate::heap::wtf8_push_cp(&mut out, c);
                crate::heap::wtf8_push_cp(&mut out, cps.next().expect("peeked code point exists"));
                continue;
            }
            match c {
                0x5B => {
                    in_class = true;
                    out.push(b'[');
                }
                0x5D => {
                    in_class = false;
                    out.push(b']');
                }
                0x2F if !in_class => out.extend_from_slice(b"\\/"),
                0x0A => out.extend_from_slice(b"\\n"),
                0x0D => out.extend_from_slice(b"\\r"),
                0x2028 => out.extend_from_slice(b"\\u2028"),
                0x2029 => out.extend_from_slice(b"\\u2029"),
                _ => crate::heap::wtf8_push_cp(&mut out, c),
            }
        }
        debug_assert_eq!(out.len(), total);
        Ok(out)
    }

    /// The `source` string Value for the RegExp at `idx` whose lossy escaped
    /// source is `src`: exact-WTF-8 when the side table has the pattern's
    /// exact bytes (lone surrogates round-trip), else the plain lossy string.
    pub(crate) fn regexp_source_value(&mut self, idx: u32, src: &str) -> Result<Value, Thrown> {
        if let Some(b) = self.regexp_exact_source.get(&idx) {
            self.preflight_native_iteration_work((b.len() as u64).saturating_mul(2))?;
            let esc = self.escaped_source_wtf8(b)?;
            self.preflight_guest_string_size(esc.len())?;
            let js = crate::heap::JsStr::from_wtf8(esc);
            return Ok(Value::heap(self.heap.alloc_js(js)));
        }
        self.preflight_native_iteration_work((src.len() as u64).saturating_mul(2))?;
        let s = self.escaped_source(src)?;
        self.preflight_guest_string_size(s.len())?;
        Ok(self.alloc_str(s))
    }

    /// RegExp.prototype[Symbol.search] core: reset lastIndex to 0, exec, restore
    /// lastIndex, return the match index or -1. Shared by String.prototype.search.
    pub(crate) fn regexp_search_impl(&mut self, rx: Value, input: Value) -> Result<Value, Thrown> {
        // @@search (22.2.6.12) is spec-generic over any Object `rx`: save lastIndex
        // (Get), zero it (Set) unless already 0, RegExpExec, restore it if exec
        // changed it — all via the observable get/set_prop protocol, honouring a
        // user lastIndex getter/setter and a custom `exec`.
        let prev = self.get_prop(rx, "lastIndex")?;
        let zero = Value::int(0);
        if !self.same_value(prev, zero) {
            self.set_prop(rx, "lastIndex", zero, true)?;
        }
        let result = self.regexp_exec_abstract(rx.heap_index(), input)?;
        let cur = self.get_prop(rx, "lastIndex")?;
        if !self.same_value(cur, prev) {
            self.set_prop(rx, "lastIndex", prev, true)?;
        }
        if result == Value::NULL {
            return Ok(Value::int(-1));
        }
        self.get_prop(result, "index")
    }

    /// RegExp.prototype[Symbol.replace] (ES 22.2.6.11) — the OBSERVABLE protocol:
    /// generic over any Object `rx`, honouring a user `exec`/`flags`/`lastIndex`,
    /// reading each result's `0`/`length`/`index`/group-N/`groups` via Get, and
    /// building the replacement from THOSE values. Reuses `regexp_exec_abstract` so a
    /// user `exec` governs the matches. All positions (`index`, lastIndex, slice
    /// bounds, the replacer's offset argument) are UTF-16 unit indices.
    pub(crate) fn regexp_symbol_replace(
        &mut self,
        rx: Value,
        string: Value,
        replace_value: Value,
    ) -> Result<Value, Thrown> {
        // ToString(string) — IDENTITY for a string value (exact WTF-8).
        let s_val = self.to_str_value(string)?;
        // Encode ONCE; every position below indexes this unit buffer.
        #[cfg(feature = "safe-sandbox")]
        let (u16s, _subject_units_reservation) = regex_subject_units(self, s_val)?;
        #[cfg(not(feature = "safe-sandbox"))]
        let u16s: Vec<u16> = self.value_units(s_val);
        let length_s = u16s.len();
        // `s_val` and `results` live in Rust locals across exec/replacer
        // re-entries — hold GC off for the whole protocol.
        let _gc = self.gc_lock_guard();
        let functional = self.is_callable(replace_value);
        #[cfg(feature = "safe-sandbox")]
        let mut replace_str_reservation = self
            .instrument_reserve_regex_transient(0)
            .map_err(|message| Thrown(message.into()))?;
        #[cfg(feature = "safe-sandbox")]
        let replace_str = if functional {
            String::new()
        } else {
            regex_owned_capture_string(self, &mut replace_str_reservation, replace_value)?
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let replace_str = if functional {
            String::new()
        } else {
            self.to_js_string(replace_value)?
        };
        // flags / global / fullUnicode are observable (Get, ToString).
        let flags_v = self.get_prop(rx, "flags")?;
        #[cfg(feature = "safe-sandbox")]
        let (global, full_unicode) = {
            let mut reservation = self
                .instrument_reserve_regex_transient(0)
                .map_err(|message| Thrown(message.into()))?;
            let flags = regex_owned_capture_string(self, &mut reservation, flags_v)?;
            let bits = (
                flags.contains('g'),
                flags.contains('u') || flags.contains('v'),
            );
            drop(flags);
            drop(reservation);
            bits
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let flags = self.to_js_string(flags_v)?;
        #[cfg(not(feature = "safe-sandbox"))]
        let global = flags.contains('g');
        // fullUnicode (`u`/`v`) selects code-point AdvanceStringIndex.
        #[cfg(not(feature = "safe-sandbox"))]
        let full_unicode = flags.contains('u') || flags.contains('v');
        if global {
            self.set_prop(rx, "lastIndex", Value::int(0), true)?;
        }
        // Collect all exec results through the exec protocol (honouring user `exec`).
        let mut results: Vec<Value> = Vec::new();
        #[cfg(feature = "safe-sandbox")]
        let mut results_reservation = self
            .instrument_reserve_regex_transient(0)
            .map_err(|m| Thrown(m.into()))?;
        let mut guard = 0u32;
        let mut native_work = 0u64;
        loop {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            native_work = native_work.saturating_add(1);
            self.preflight_native_iteration_work(native_work)?;
            let result = self.regexp_exec_abstract(rx.heap_index(), s_val)?;
            if result == Value::NULL {
                break;
            }
            #[cfg(feature = "safe-sandbox")]
            {
                if results.len() == results.capacity() {
                    let element_bytes = std::mem::size_of::<Value>().max(1);
                    let available_entries =
                        self.instrument_regex_limits().max_memory_bytes / element_bytes;
                    let grow_by = if results.capacity() == 0 {
                        4.min(available_entries)
                    } else {
                        results.capacity().min(available_entries)
                    };
                    if grow_by == 0 {
                        return Err(Thrown(self.instrument_regex_memory_exhausted().into()));
                    }
                    regex_try_reserve_exact(self, &mut results_reservation, &mut results, grow_by)?;
                }
            }
            #[cfg(not(feature = "safe-sandbox"))]
            if results.try_reserve(1).is_err() {
                return Err(Thrown(
                    "RangeError: RegExp replacement result allocation failed".into(),
                ));
            }
            results.push(result);
            if !global {
                break;
            }
            // An empty match advances lastIndex so the loop makes progress.
            let match0 = self.get_prop(result, "0")?;
            #[cfg(feature = "safe-sandbox")]
            let match_is_empty = {
                let value = self.to_str_value(match0)?;
                self.heap.str_is_empty(value.heap_index()) == Some(true)
            };
            #[cfg(not(feature = "safe-sandbox"))]
            let match_is_empty = self.to_js_string(match0)?.is_empty();
            if match_is_empty {
                let li_v = self.get_prop(rx, "lastIndex")?;
                // ToLength: clamp to 2^53-1 BEFORE the advance.
                let this_index = host_index_saturating(
                    self.to_integer_or_zero(li_v)?.clamp(0, (1i64 << 53) - 1),
                );
                let next = advance_string_index(&u16s, this_index, full_unicode);
                self.set_prop(rx, "lastIndex", Value::num(next as f64), true)?;
            }
        }
        // Build the accumulated result (WTF-8 — subject slices stay exact),
        // reading each match's fields via Get.
        let mut accumulated: Vec<u8> = Vec::new();
        #[cfg(feature = "safe-sandbox")]
        let mut accumulated_reservation = self
            .instrument_reserve_regex_transient(0)
            .map_err(|message| Thrown(message.into()))?;
        let mut next_pos: usize = 0;
        for result in results {
            native_work = native_work.saturating_add(1);
            self.preflight_native_iteration_work(native_work)?;
            let len_v = self.get_prop(result, "length")?;
            let n_captures_u64 = (self.to_integer_or_zero(len_v)?.max(0) as u64).saturating_sub(1);
            native_work = native_work.saturating_add(n_captures_u64);
            self.preflight_native_iteration_work(native_work)?;
            let n_captures = usize::try_from(n_captures_u64)
                .map_err(|_| Thrown("RangeError: RegExp capture list is too large".into()))?;
            let matched_v = self.get_prop(result, "0")?;
            // ToString(Get(result,"0")) — IDENTITY for a string value; its UNIT
            // length determines how far this match consumes the subject.
            let matched_val = self.to_str_value(matched_v)?;
            let match_len = self.heap.str_units(matched_val.heap_index()).unwrap_or(0);
            let pos_v = self.get_prop(result, "index")?;
            let position = self.to_integer_or_zero(pos_v)?.clamp(0, length_s as i64) as usize;
            // Replacement bytes (WTF-8): the functional path appends a returned
            // string's EXACT bytes; the template path expands over lossy views.
            #[cfg(feature = "safe-sandbox")]
            let mut replacement_reservation = self
                .instrument_reserve_regex_transient(0)
                .map_err(|message| Thrown(message.into()))?;
            let replacement: Vec<u8> = if functional {
                let argv_len = n_captures.checked_add(4).ok_or_else(|| {
                    Thrown("RangeError: RegExp replacement argument list is too large".into())
                })?;
                let mut argv: Vec<Value> = Vec::new();
                #[cfg(feature = "safe-sandbox")]
                let mut argv_reservation = self
                    .instrument_reserve_regex_transient(0)
                    .map_err(|m| Thrown(m.into()))?;
                #[cfg(feature = "safe-sandbox")]
                regex_try_reserve_exact(self, &mut argv_reservation, &mut argv, argv_len)?;
                #[cfg(not(feature = "safe-sandbox"))]
                argv.try_reserve_exact(argv_len).map_err(|_| {
                    Thrown("RangeError: RegExp replacement argument allocation failed".into())
                })?;
                argv.push(matched_val);
                for n in 1..=n_captures {
                    #[cfg(feature = "safe-sandbox")]
                    let cap_v = {
                        let mut key_buf = [0u8; 20];
                        self.get_prop(result, crate::heap::index_key(&mut key_buf, n))?
                    };
                    #[cfg(not(feature = "safe-sandbox"))]
                    let cap_v = self.get_prop(result, &n.to_string())?;
                    argv.push(if cap_v == Value::UNDEFINED {
                        Value::UNDEFINED
                    } else {
                        // Retain the one heap string produced by ToString. This
                        // avoids the old Rust String clone followed by a second
                        // clone into the heap for the replacer argv.
                        #[cfg(feature = "safe-sandbox")]
                        {
                            regex_capture_value(self, cap_v)?
                        }
                        #[cfg(not(feature = "safe-sandbox"))]
                        {
                            self.to_str_value(cap_v)?
                        }
                    });
                }
                let named_v = self.get_prop(result, "groups")?;
                let named_defined = named_v != Value::UNDEFINED;
                argv.push(Value::num(position as f64));
                argv.push(s_val);
                if named_defined {
                    argv.push(named_v);
                }
                let r = self.call_value(replace_value, Value::UNDEFINED, &argv)?;
                #[cfg(feature = "safe-sandbox")]
                let replacement = regex_owned_wtf8_string(self, &mut replacement_reservation, r)?;
                #[cfg(not(feature = "safe-sandbox"))]
                let replacement = {
                    let rv = self.to_str_value(r)?;
                    self.heap
                        .str_wtf8_cow(rv.heap_index())
                        .map(|c| c.into_owned())
                        .unwrap_or_default()
                };
                replacement
            } else {
                #[cfg(feature = "safe-sandbox")]
                let mut captures_reservation = self
                    .instrument_reserve_regex_transient(0)
                    .map_err(|m| Thrown(m.into()))?;
                let mut captures: Vec<Option<String>> = Vec::new();
                #[cfg(feature = "safe-sandbox")]
                regex_try_reserve_exact(
                    self,
                    &mut captures_reservation,
                    &mut captures,
                    n_captures,
                )?;
                #[cfg(not(feature = "safe-sandbox"))]
                captures.try_reserve_exact(n_captures).map_err(|_| {
                    Thrown("RangeError: RegExp capture-list allocation failed".into())
                })?;
                for n in 1..=n_captures {
                    #[cfg(feature = "safe-sandbox")]
                    let cap_v = {
                        let mut key_buf = [0u8; 20];
                        self.get_prop(result, crate::heap::index_key(&mut key_buf, n))?
                    };
                    #[cfg(not(feature = "safe-sandbox"))]
                    let cap_v = self.get_prop(result, &n.to_string())?;
                    captures.push(if cap_v == Value::UNDEFINED {
                        None
                    } else {
                        #[cfg(feature = "safe-sandbox")]
                        {
                            Some(regex_owned_capture_string(
                                self,
                                &mut captures_reservation,
                                cap_v,
                            )?)
                        }
                        #[cfg(not(feature = "safe-sandbox"))]
                        {
                            Some(self.to_js_string(cap_v)?)
                        }
                    });
                }
                let named_v = self.get_prop(result, "groups")?;
                let named_defined = named_v != Value::UNDEFINED;
                // GetSubstitution: read the named-capture group object's own props.
                // Step l.i.1 — when `groups` is not undefined it is ToObject'd, so a
                // primitive (e.g. a string `groups`) is boxed and its properties
                // (`$<length>` etc.) become readable; ToObject(null) throws.
                let named_list: Vec<(String, Option<String>)> = if named_defined {
                    // ToObject(namedCaptures): null throws a TypeError (the public
                    // Object(null) would return {}, but this is the internal op).
                    self.require_object_coercible(named_v)?;
                    let obj = self.to_object(named_v)?;
                    // GetSubstitution reads EXACTLY the template's `$<name>`
                    // groups via Get — through the PROTOTYPE chain, so an
                    // inherited group property resolves (groups-object-subclass)
                    // and a missing one substitutes the empty string.
                    let mut v: Vec<(String, Option<String>)> = Vec::new();
                    let mut rest = replace_str.as_str();
                    while let Some(p) = rest.find("$<") {
                        rest = &rest[p + 2..];
                        let Some(e) = rest.find('>') else { break };
                        let name_slice = &rest[..e];
                        rest = &rest[e + 1..];
                        if !v.iter().any(|(n, _)| n == name_slice) {
                            #[cfg(feature = "safe-sandbox")]
                            regex_try_reserve_geometric(
                                self,
                                &mut captures_reservation,
                                &mut v,
                                1,
                                usize::MAX,
                            )?;
                            #[cfg(feature = "safe-sandbox")]
                            let name =
                                regex_owned_str(self, &mut captures_reservation, name_slice)?;
                            #[cfg(not(feature = "safe-sandbox"))]
                            let name = name_slice.to_string();
                            let val = self.get_prop(obj, &name)?;
                            let sv = if val == Value::UNDEFINED {
                                None
                            } else {
                                #[cfg(feature = "safe-sandbox")]
                                {
                                    Some(regex_owned_capture_string(
                                        self,
                                        &mut captures_reservation,
                                        val,
                                    )?)
                                }
                                #[cfg(not(feature = "safe-sandbox"))]
                                {
                                    Some(self.to_js_string(val)?)
                                }
                            };
                            v.push((name, sv));
                        }
                    }
                    v
                } else {
                    Vec::new()
                };
                #[cfg(feature = "safe-sandbox")]
                let matched_lossy =
                    regex_owned_capture_string(self, &mut captures_reservation, matched_val)?;
                #[cfg(not(feature = "safe-sandbox"))]
                let matched_lossy = self
                    .heap
                    .str_cow(matched_val.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                #[cfg(feature = "safe-sandbox")]
                let pre =
                    regex_owned_utf16_lossy(self, &mut captures_reservation, &u16s[..position])?;
                #[cfg(not(feature = "safe-sandbox"))]
                let pre = String::from_utf16_lossy(&u16s[..position]);
                let post_start = (position + match_len).min(length_s);
                #[cfg(feature = "safe-sandbox")]
                let post =
                    regex_owned_utf16_lossy(self, &mut captures_reservation, &u16s[post_start..])?;
                #[cfg(not(feature = "safe-sandbox"))]
                let post = String::from_utf16_lossy(&u16s[post_start..]);
                #[cfg(feature = "safe-sandbox")]
                let expanded = self.expand_replacement_safe(
                    &mut replacement_reservation,
                    &replace_str,
                    &matched_lossy,
                    &captures,
                    &named_list,
                    named_defined,
                    &pre,
                    &post,
                    MAX_STRING_BYTES.saturating_sub(accumulated.len()),
                )?;
                #[cfg(not(feature = "safe-sandbox"))]
                let expanded = self.expand_replacement(
                    &replace_str,
                    &matched_lossy,
                    &captures,
                    &named_list,
                    named_defined,
                    &pre,
                    &post,
                    MAX_STRING_BYTES.saturating_sub(accumulated.len()),
                )?;
                expanded.into_bytes()
            };
            if position >= next_pos {
                #[cfg(feature = "safe-sandbox")]
                regex_append_units(
                    self,
                    &mut accumulated_reservation,
                    &mut accumulated,
                    &u16s[next_pos..position],
                )?;
                #[cfg(not(feature = "safe-sandbox"))]
                push_units(&mut accumulated, &u16s[next_pos..position]);
                #[cfg(feature = "safe-sandbox")]
                regex_append_wtf8(
                    self,
                    &mut accumulated_reservation,
                    &mut accumulated,
                    &replacement,
                )?;
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    if replacement.len() > MAX_STRING_BYTES.saturating_sub(accumulated.len()) {
                        return Err(Thrown("RangeError: Invalid string length".into()));
                    }
                    crate::heap::wtf8_push(&mut accumulated, &replacement);
                }
                next_pos = position + match_len;
            }
        }
        // The consumed Vec/IntoIter backing is gone. Its charge was needed
        // across every observable exec/replacer re-entry, but retaining that
        // charge while appending the final tail would reject valid output.
        #[cfg(feature = "safe-sandbox")]
        drop(results_reservation);
        if next_pos < length_s {
            #[cfg(feature = "safe-sandbox")]
            regex_append_units(
                self,
                &mut accumulated_reservation,
                &mut accumulated,
                &u16s[next_pos..],
            )?;
            #[cfg(not(feature = "safe-sandbox"))]
            push_units(&mut accumulated, &u16s[next_pos..]);
        }
        #[cfg(feature = "safe-sandbox")]
        return Ok(regex_wtf8_to_heap(
            self,
            &mut accumulated_reservation,
            accumulated,
        ));
        #[cfg(not(feature = "safe-sandbox"))]
        Ok(Value::heap(
            self.heap
                .alloc_js(crate::heap::JsStr::from_wtf8(accumulated)),
        ))
    }

    /// RegExpExec (ES 22.2.7.1): the exec PROTOCOL. When the regex has a callable
    /// own/inherited `exec` (honouring a user override), call it with the subject
    /// string and require an Object-or-null result; otherwise fall back to the
    /// builtin RegExpBuiltinExec. The `@@match`/`@@search` (non-global) cores route
    /// through this so a custom `re.exec` governs the result.
    pub(crate) fn regexp_exec_abstract(&mut self, re: u32, input: Value) -> Result<Value, Thrown> {
        // PLAIN regexp (a REAL RegExp whose intrinsic `exec` is reached
        // through %RegExp.prototype%): the Get(R,"exec") is unobservable and
        // the call dispatch is the intrinsic — run RegExpBuiltinExec
        // directly. (`re` may be any object here — the protocol is generic —
        // so the real-RegExp check guards the arr_props own-props model.)
        if matches!(self.heap.get(re), HeapObj::RegExp { .. }) && self.regexp_exec_fast_ok(re) {
            return self.regexp_exec(re, input);
        }
        let re_v = Value::heap(re);
        let exec = self.get_prop(re_v, "exec")?;
        if self.is_callable(exec) {
            // ToString(S) — IDENTITY for a string value (exact WTF-8; a lossy
            // copy would strip lone surrogates before exec ever sees them).
            let s = self.to_str_value(input)?;
            let r = self.call_value(exec, re_v, &[s])?;
            let is_object = r.is_heap()
                && !matches!(
                    self.heap.get(r.heap_index()),
                    HeapObj::Str(_)
                        | HeapObj::Cons { .. }
                        | HeapObj::Symbol { .. }
                        | HeapObj::BigInt(_)
                        | HeapObj::BigIntBig(_)
                );
            if r != Value::NULL && !is_object {
                return Err(Thrown(
                    "TypeError: RegExp exec method returned something other than an Object or null"
                        .into(),
                ));
            }
            return Ok(r);
        }
        self.regexp_exec(re, input)
    }

    /// RegExp.prototype[Symbol.match] core: a non-global regex returns the exec
    /// result (array or null); a global regex returns the array of matched
    /// substrings (or null) and resets lastIndex. Shared by String.match.
    pub(crate) fn regexp_match_impl(&mut self, re: u32, input: Value) -> Result<Value, Thrown> {
        // OBSERVABLE @@match (22.2.6.8), generic over any Object `rx`: read
        // ToString(Get(rx,"flags")); a non-global match is just RegExpExec; a global
        // match loops RegExpExec (honouring a user `exec`) collecting ToString(Get(
        // result,"0")), resets lastIndex first, and advances past an empty match.
        let rx = Value::heap(re);
        let flags_v = self.get_prop(rx, "flags")?;
        #[cfg(feature = "safe-sandbox")]
        let (global, full_unicode) = {
            let mut reservation = self
                .instrument_reserve_regex_transient(0)
                .map_err(|message| Thrown(message.into()))?;
            let flags = regex_owned_capture_string(self, &mut reservation, flags_v)?;
            let bits = (
                flags.contains('g'),
                flags.contains('u') || flags.contains('v'),
            );
            drop(flags);
            drop(reservation);
            bits
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let flags = self.to_js_string(flags_v)?;
        #[cfg(not(feature = "safe-sandbox"))]
        let global = flags.contains('g');
        if !global {
            return self.regexp_exec_abstract(re, input);
        }
        // fullUnicode (`u`/`v`) selects code-point AdvanceStringIndex.
        #[cfg(not(feature = "safe-sandbox"))]
        let full_unicode = flags.contains('u') || flags.contains('v');
        // ToString(string) — IDENTITY for a string value (exact WTF-8).
        let s_val = self.to_str_value(input)?;
        // Unit buffer for the empty-match AdvanceStringIndex step.
        #[cfg(feature = "safe-sandbox")]
        let (u16s, _subject_units_reservation) = regex_subject_units(self, s_val)?;
        #[cfg(not(feature = "safe-sandbox"))]
        let u16s: Vec<u16> = self.value_units(s_val);
        // `s_val`/`elems` live in Rust locals across exec re-entries.
        let _gc = self.gc_lock_guard();
        self.set_prop(rx, "lastIndex", Value::int(0), false)?;
        let mut elems: Vec<Value> = Vec::new();
        #[cfg(feature = "safe-sandbox")]
        let mut elems_reservation = self
            .instrument_reserve_regex_transient(0)
            .map_err(|message| Thrown(message.into()))?;
        let mut guard = 0u32;
        let mut native_work = 0u64;
        loop {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            native_work = native_work.saturating_add(1);
            self.preflight_native_iteration_work(native_work)?;
            let result = self.regexp_exec_abstract(re, s_val)?;
            if result == Value::NULL {
                break;
            }
            let m0 = self.get_prop(result, "0")?;
            // ToString(Get(result,"0")) — IDENTITY for a string value, so a
            // lone-surrogate match survives into the result array.
            let m0_val = self.to_str_value(m0)?;
            let is_empty = self.heap.str_units(m0_val.heap_index()) == Some(0);
            #[cfg(feature = "safe-sandbox")]
            regex_push_value(self, &mut elems_reservation, &mut elems, m0_val)?;
            #[cfg(not(feature = "safe-sandbox"))]
            {
                elems.try_reserve(1).map_err(|_| {
                    Thrown("RangeError: RegExp match-result allocation failed".into())
                })?;
                elems.push(m0_val);
            }
            if is_empty {
                let li_v = self.get_prop(rx, "lastIndex")?;
                // ToLength: clamp to 2^53-1 BEFORE the advance.
                let this_index = host_index_saturating(
                    self.to_integer_or_zero(li_v)?.clamp(0, (1i64 << 53) - 1),
                );
                let next = advance_string_index(&u16s, this_index, full_unicode);
                self.set_prop(rx, "lastIndex", Value::num(next as f64), true)?;
            }
        }
        if elems.is_empty() {
            return Ok(Value::NULL);
        }
        #[cfg(feature = "safe-sandbox")]
        return Ok(regex_values_to_heap(self, &mut elems_reservation, elems));
        #[cfg(not(feature = "safe-sandbox"))]
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(elems))))
    }

    /// RegExp.prototype[Symbol.split] core (simplified — no capture groups in the
    /// output yet). Shared by String.prototype.split for a regex separator.
    pub(crate) fn regexp_split_impl(
        &mut self,
        re: u32,
        input: Value,
        limit: Value,
    ) -> Result<Value, Thrown> {
        // OBSERVABLE @@split (22.2.6.14), generic over any Object `rx`:
        // SpeciesConstructor(rx, %RegExp%) builds a sticky (`y`) splitter, then a
        // loop calls RegExpExec (honouring a user `exec`) reading lastIndex/length/
        // captures via Get. Positions p/q/e are UTF-16 unit indices; the no-match
        // advance is spec AdvanceStringIndex (+2 over an astral pair in `u`/`v`).
        let rx = Value::heap(re);
        // ToString(string) — IDENTITY for a string value (exact WTF-8).
        let s_val = self.to_str_value(input)?;
        #[cfg(feature = "safe-sandbox")]
        let (u16s, _subject_units_reservation) = regex_subject_units(self, s_val)?;
        #[cfg(not(feature = "safe-sandbox"))]
        let u16s: Vec<u16> = self.value_units(s_val);
        let size = u16s.len();
        // `s_val`/`a` live in Rust locals across construct/exec re-entries.
        let _gc = self.gc_lock_guard();
        // SpeciesConstructor(rx, %RegExp%).
        let default_ctor = Value::heap(self.regexp_ctor);
        let c = {
            let ctor = self.get_prop(rx, "constructor")?;
            if ctor == Value::UNDEFINED {
                default_ctor
            } else if !self.is_object_value(ctor) {
                // SpeciesConstructor step 5: a defined-but-non-object constructor
                // (false / "string" / 86 / null) is a TypeError, before @@species.
                return Err(Thrown(
                    "TypeError: Symbol.split constructor property is not an object".into(),
                ));
            } else {
                let sp = self.get_prop(ctor, "@@species")?;
                if sp == Value::UNDEFINED || sp == Value::NULL {
                    default_ctor
                } else if self.is_constructor(sp) {
                    sp
                } else {
                    return Err(Thrown(
                        "TypeError: Symbol.split species constructor is not a constructor".into(),
                    ));
                }
            }
        };
        // flags (observable) + force the sticky `y` flag on the splitter copy.
        let flags_v = self.get_prop(rx, "flags")?;
        #[cfg(feature = "safe-sandbox")]
        let (unicode_matching, new_flags_v) = {
            let mut reservation = self
                .instrument_reserve_regex_transient(0)
                .map_err(|message| Thrown(message.into()))?;
            let mut flags = regex_owned_capture_string(self, &mut reservation, flags_v)?;
            let unicode_matching = flags.contains('u') || flags.contains('v');
            if !flags.contains('y') {
                regex_try_reserve_string_exact(self, &mut reservation, &mut flags, 1)?;
                flags.push('y');
            }
            let value = regex_string_to_heap(self, &mut reservation, flags);
            (unicode_matching, value)
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let flags = self.to_js_string(flags_v)?;
        // unicodeMatching (`u`/`v`) selects code-point AdvanceStringIndex.
        #[cfg(not(feature = "safe-sandbox"))]
        let unicode_matching = flags.contains('u') || flags.contains('v');
        #[cfg(not(feature = "safe-sandbox"))]
        let new_flags = if flags.contains('y') {
            flags
        } else {
            format!("{flags}y")
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let new_flags_v = self.alloc_str(new_flags);
        let splitter = self.construct(c, &[rx, new_flags_v])?;
        let lim: u64 = if limit == Value::UNDEFINED {
            u32::MAX as u64
        } else {
            to_uint32(self.to_number_coerce(limit)?) as u64
        };
        let mut a: Vec<Value> = Vec::new();
        #[cfg(feature = "safe-sandbox")]
        let mut array_reservation = self
            .instrument_reserve_regex_transient(0)
            .map_err(|message| Thrown(message.into()))?;
        if lim == 0 {
            #[cfg(feature = "safe-sandbox")]
            return Ok(regex_values_to_heap(self, &mut array_reservation, a));
            #[cfg(not(feature = "safe-sandbox"))]
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
        }
        // Empty input: one exec; if it matches, the result is empty, else [S].
        if size == 0 {
            let z = self.regexp_exec_abstract(splitter.heap_index(), s_val)?;
            if z == Value::NULL {
                #[cfg(feature = "safe-sandbox")]
                regex_push_value(self, &mut array_reservation, &mut a, s_val)?;
                #[cfg(not(feature = "safe-sandbox"))]
                a.push(s_val);
            }
            #[cfg(feature = "safe-sandbox")]
            return Ok(regex_values_to_heap(self, &mut array_reservation, a));
            #[cfg(not(feature = "safe-sandbox"))]
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
        }
        let mut p: usize = 0;
        let mut q: usize = 0;
        let mut guard = 0u32;
        let mut native_work = 0u64;
        while q < size {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            native_work = native_work.saturating_add(1);
            self.preflight_native_iteration_work(native_work)?;
            self.set_prop(splitter, "lastIndex", Value::num(q as f64), true)?;
            let z = self.regexp_exec_abstract(splitter.heap_index(), s_val)?;
            if z == Value::NULL {
                q = advance_string_index(&u16s, q, unicode_matching);
                continue;
            }
            // e = min(ToLength(Get(splitter,"lastIndex")), size).
            let li_v = self.get_prop(splitter, "lastIndex")?;
            let e = (self.to_integer_or_zero(li_v)?.max(0) as u64).min(size as u64) as usize;
            if e == p {
                q = advance_string_index(&u16s, q, unicode_matching);
                continue;
            }
            #[cfg(feature = "safe-sandbox")]
            let t = regex_units_value(self, &u16s[p..q])?;
            #[cfg(not(feature = "safe-sandbox"))]
            let t = self.units_value(&u16s[p..q]);
            #[cfg(feature = "safe-sandbox")]
            regex_push_value(self, &mut array_reservation, &mut a, t)?;
            #[cfg(not(feature = "safe-sandbox"))]
            a.push(t);
            if a.len() as u64 == lim {
                #[cfg(feature = "safe-sandbox")]
                return Ok(regex_values_to_heap(self, &mut array_reservation, a));
                #[cfg(not(feature = "safe-sandbox"))]
                return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
            }
            p = e;
            // Each capturing group (1..n) is emitted between the pieces.
            let zlen_v = self.get_prop(z, "length")?;
            let n_captures_u64 = (self.to_integer_or_zero(zlen_v)?.max(0) as u64).saturating_sub(1);
            // Only captures that fit before `limit` are observable; charge and
            // convert exactly that many so a huge declared length cannot wrap
            // on wasm32, while a small limit retains its early-return behavior.
            let n_captures_u64 = n_captures_u64.min(lim.saturating_sub(a.len() as u64));
            native_work = native_work.saturating_add(n_captures_u64);
            self.preflight_native_iteration_work(native_work)?;
            let n_captures = usize::try_from(n_captures_u64)
                .map_err(|_| Thrown("RangeError: RegExp capture list is too large".into()))?;
            for i in 1..=n_captures {
                #[cfg(feature = "safe-sandbox")]
                let cap = {
                    let mut key_buf = [0u8; 20];
                    self.get_prop(z, crate::heap::index_key(&mut key_buf, i))?
                };
                #[cfg(not(feature = "safe-sandbox"))]
                let cap = self.get_prop(z, &i.to_string())?;
                #[cfg(feature = "safe-sandbox")]
                regex_push_value(self, &mut array_reservation, &mut a, cap)?;
                #[cfg(not(feature = "safe-sandbox"))]
                a.push(cap);
                if a.len() as u64 == lim {
                    #[cfg(feature = "safe-sandbox")]
                    return Ok(regex_values_to_heap(self, &mut array_reservation, a));
                    #[cfg(not(feature = "safe-sandbox"))]
                    return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
                }
            }
            q = p;
        }
        #[cfg(feature = "safe-sandbox")]
        let tail = regex_units_value(self, &u16s[p..])?;
        #[cfg(not(feature = "safe-sandbox"))]
        let tail = self.units_value(&u16s[p..]);
        #[cfg(feature = "safe-sandbox")]
        regex_push_value(self, &mut array_reservation, &mut a, tail)?;
        #[cfg(not(feature = "safe-sandbox"))]
        a.push(tail);
        #[cfg(feature = "safe-sandbox")]
        return Ok(regex_values_to_heap(self, &mut array_reservation, a));
        #[cfg(not(feature = "safe-sandbox"))]
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))))
    }

    /// `RegExp.prototype.exec(input)`: returns the match-result Array (group 0 +
    /// captures, with `.index`/`.input`/`.groups` in the side table) or `null`.
    /// Advances `lastIndex` for a global/sticky regex.
    ///
    /// Matching runs over the subject's UTF-16 CODE UNITS (regress
    /// `find_from_utf16` for `u`/`v` regexes — code-point elements — and
    /// `find_from_ucs2` otherwise — each unit is an element, so `/./` matches
    /// one surrogate half). Every position regress reports (match range,
    /// capture ranges) is a unit index, identical to JS string indexing
    /// engine-wide: `lastIndex` seeds the search directly and `.index` /
    /// `indices` / `lastIndex` writes take the ranges verbatim.
    ///
    /// ASCII FAST PATH: an all-ASCII subject (the `JsStr::is_ascii` flag) is
    /// matched in place over its bytes with regress `find_from_ascii` — no
    /// per-exec `Vec<u16>` encode. Byte offsets == unit offsets for ASCII, so
    /// every reported range is a valid unit index verbatim. This is
    /// semantically identical to the UCS-2/UTF-16 run: regress folds pattern
    /// chars and closes bracket sets at COMPILE time (full Unicode folding),
    /// so a non-ASCII `CharICase` insn can never match an ASCII element on
    /// either backend, and runtime folding only ever compares two SUBJECT
    /// chars (backrefs) — both ASCII here, where ASCII and Unicode simple
    /// folding agree.
    pub(crate) fn regexp_exec(&mut self, re_idx: u32, input_v: Value) -> Result<Value, Thrown> {
        self.regexp_exec_impl(re_idx, input_v, true)
    }

    /// Read one of a pristine match-result Array's standard named properties
    /// without constructing its ordinary `ObjMap` representation.
    #[inline]
    pub(crate) fn regexp_result_prop(&self, idx: u32, key: &str) -> Option<Value> {
        // Reject unrelated Array names before touching the side table. This
        // helper sits on the generic Array named-read path, so `length` and
        // method reads must not acquire an extra indexed lookup merely because
        // some RegExp result exists elsewhere in the VM.
        let slot = match key {
            "index" => 0,
            "input" => 1,
            "groups" => 2,
            "indices" => 3,
            _ => return None,
        };
        let p = self.regexp_result_props.get(&idx)?;
        if slot == 3 && p.values[3] == Value::UNDEFINED {
            None
        } else {
            Some(p.values[slot])
        }
    }

    /// Convert a compact pristine match-result record into `arr_props` before
    /// an operation that can observe or change descriptors, key order,
    /// deletion, or integrity state.
    ///
    /// The defensive merge handles an element overlay installed by an internal
    /// Array path before materialisation. Standard result names were created
    /// first, so they are pushed first; a later explicit entry with the same key
    /// overwrites that slot while retaining its original insertion order.
    pub(crate) fn materialize_regexp_result_props(&mut self, idx: u32) {
        if self.regexp_result_props.is_empty() {
            return;
        }
        let Some(p) = self.regexp_result_props.remove(&idx) else {
            return;
        };
        rxstats::count_materialized();
        let old = self.arr_props.remove(&idx);
        let has_indices = p.values[3] != Value::UNDEFINED;
        let mut m = ObjMap::side_table_with_capacity(
            3 + has_indices as usize + old.as_ref().map_or(0, ObjMap::len),
        );
        m.push_data("index".to_string(), p.values[0]);
        m.push_data("input".to_string(), p.values[1]);
        m.push_data("groups".to_string(), p.values[2]);
        if has_indices {
            m.push_data("indices".to_string(), p.values[3]);
        }
        if let Some(old) = old {
            for (key, value, attr) in old.iter() {
                m.define(key, value, attr);
            }
            m.class = old.class;
            m.is_ctor = old.is_ctor;
            m.is_raw_json = old.is_raw_json;
            if old.frozen {
                m.freeze();
            } else if old.sealed {
                m.seal();
            } else {
                m.extensible = old.extensible;
            }
        }
        self.arr_props.insert(idx, m);
    }

    /// Promote only when an operation targets one of the compact properties.
    /// Unrelated additions can coexist in `arr_props`; the eventual full
    /// materialisation merges them after the earlier-created standard names.
    #[inline]
    pub(crate) fn materialize_regexp_result_prop_for_key(&mut self, idx: u32, key: &str) {
        if matches!(key, "index" | "input" | "groups" | "indices") {
            self.materialize_regexp_result_props(idx);
        }
    }

    /// `regexp_exec` with `build = false` for `RegExp.prototype.test`: the
    /// IDENTICAL protocol (lastIndex Get/ToLength + the stateful Sets, in spec
    /// order), but the unobservable match-result materialization (array +
    /// capture strings + groups/indices objects) is skipped — returns
    /// `Value::TRUE` instead of the array. `Value::NULL` still means no match.
    pub(crate) fn regexp_exec_impl(
        &mut self,
        re_idx: u32,
        input_v: Value,
        build: bool,
    ) -> Result<Value, Thrown> {
        self.regexp_exec_impl_prebits(re_idx, input_v, build, None)
    }

    /// `regexp_exec_impl` with the four flag-derived bits pre-decoded
    /// (`ITFB_*` layout). Callers passing `Some` must guarantee the bits still
    /// describe [[OriginalFlags]] at match time — which only holds when
    /// `lastIndex` is a plain number (no `valueOf` re-entry can `compile()`
    /// new flags between the ToLength and the flags read) and the regex's
    /// flags cannot have changed since the bits were captured. The fused
    /// matchAll step's matcher qualifies: it is engine-internal, so no user
    /// reference exists to `compile()` it.
    fn regexp_exec_impl_prebits(
        &mut self,
        re_idx: u32,
        input_v: Value,
        build: bool,
        prebits: Option<u8>,
    ) -> Result<Value, Thrown> {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::RegexExec);
        // ToString(string) — IDENTITY for a string value (exact WTF-8 content:
        // a lone-surrogate subject keeps its surrogate rather than decaying to
        // U+FFFD, so `/\uD800/` can match it).
        let input_val = self.to_str_value(input_v)?;
        // `input_val` + the result pieces below live in Rust locals across a
        // possible `lastIndex.valueOf` re-entry — hold GC off until we return.
        let _gc = self.gc_lock_guard();
        // Get(R,"lastIndex") — on a real RegExp this can never run user code:
        // `lastIndex` is a non-configurable own DATA property whose value's
        // source of truth is the heap slot (defineProperty writes the value
        // through; only attrs live in arr_props) — so read the slot directly.
        let li_v = match self.heap.get(re_idx) {
            HeapObj::RegExp { last_index, .. } => *last_index,
            _ => {
                return Err(Thrown(
                    "TypeError: RegExp.prototype.exec called on a non-RegExp".into(),
                ));
            }
        };
        // ToLength(Get(R,"lastIndex")) is RegExpBuiltinExec step 4 and it
        // PRECEDES the [[OriginalFlags]] / [[RegExpMatcher]] reads of steps
        // 5-11: a `lastIndex.valueOf` may call `R.compile(pattern, flags)`,
        // which replaces both, and the run must use what it left behind. The
        // flag bits used to be fused into the same heap.get as the slot, so a
        // recompile that added `g` never updated lastIndex and one that dropped
        // `y` still clobbered it.
        let li = host_index_saturating(self.to_integer_or_zero(li_v)?.clamp(0, (1i64 << 53) - 1));
        let (global, sticky, has_indices, unicode) = match prebits {
            // Pre-decoded at iterator creation (B118): `lastIndex` was a
            // number (checked by the caller), so no user code ran above and
            // the flags are what they were when the bits were captured.
            Some(b) => {
                debug_assert!(li_v.is_number());
                (
                    b & ITFB_GLOBAL != 0,
                    b & ITFB_STICKY != 0,
                    b & ITFB_INDICES != 0,
                    b & ITFB_UNICODE != 0,
                )
            }
            None => match self.heap.get(re_idx) {
                HeapObj::RegExp { flags, .. } => {
                    if slim_exec_enabled() {
                        // B124: one pass over the ≤8-byte flag string instead
                        // of four `contains` scans. Same heap.get, same spec
                        // position (AFTER ToLength — a `lastIndex.valueOf`
                        // may `compile()`, and this reads what it left).
                        let (mut g, mut y, mut d, mut u) = (false, false, false, false);
                        for b in flags.bytes() {
                            match b {
                                b'g' => g = true,
                                b'y' => y = true,
                                b'd' => d = true,
                                b'u' | b'v' => u = true,
                                _ => {}
                            }
                        }
                        (g, y, d, u)
                    } else {
                        (
                            flags.contains('g'),
                            flags.contains('y'),
                            flags.contains('d'),
                            flags.contains('u') || flags.contains('v'),
                        )
                    }
                }
                _ => {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.exec called on a non-RegExp".into(),
                    ));
                }
            },
        };
        let stateful = global || sticky;
        // Step 9: a non-global, non-sticky regex always searches from 0.
        let start = if stateful { li } else { 0 };
        // ASCII subjects match in place over the heap bytes (offsets == unit
        // indices); anything else encodes the subject ONCE per exec.
        // `lastIndex` is already a unit index engine-wide, so it is the
        // search start with no conversion either way.
        let s_idx = input_val.heap_index();
        // B124: ONE subject heap.get serves the flat-check, the ascii bit and
        // (for the ascii case, where units == bytes) the unit length, instead
        // of an unconditional `flatten` (its own get + tag check) plus a
        // second `str_units` get. `flatten` now runs only when the get
        // actually sees a rope — Cons→Str is irreversible, so the re-read
        // after it is a `Str` by construction. `ZIPP_NO_SLIM_EXEC=1` restores
        // the split reads; both compute identical values on every input.
        #[cfg(feature = "safe-sandbox")]
        let (is_ascii, ascii_units) = match self.heap.get(s_idx) {
            // Keep the allocation-free byte backend for an already-flat ASCII
            // subject. A rope is deliberately sent through the metered UTF-16
            // walker below instead of being flattened before its reservation.
            HeapObj::Str(js) => (js.is_ascii(), js.as_bytes().len()),
            _ => (false, 0),
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let (is_ascii, ascii_units) = if slim_exec_enabled() {
            match self.heap.get(s_idx) {
                HeapObj::Str(js) => (js.is_ascii(), js.as_bytes().len()),
                _ => {
                    self.heap.flatten(s_idx);
                    match self.heap.get(s_idx) {
                        HeapObj::Str(js) => (js.is_ascii(), js.as_bytes().len()),
                        _ => (false, 0),
                    }
                }
            }
        } else {
            self.heap.flatten(s_idx);
            (
                matches!(self.heap.get(s_idx), HeapObj::Str(js) if js.is_ascii()),
                0,
            )
        };
        #[cfg(feature = "safe-sandbox")]
        let (u16s, _subject_units_reservation) = if is_ascii {
            (Vec::new(), None)
        } else {
            let (units, reservation) = regex_subject_units(self, input_val)?;
            (units, Some(reservation))
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let u16s: Vec<u16> = if is_ascii {
            Vec::new()
        } else {
            self.value_units(input_val)
        };
        let subj_units = if is_ascii {
            if slim_exec_enabled() {
                ascii_units
            } else {
                self.heap.str_units(s_idx).unwrap_or(0)
            }
        } else {
            u16s.len()
        };
        let found = if start > subj_units {
            None
        } else if is_ascii {
            self.ensure_regexp_ascii_twin(re_idx);
            // Both the subject string and the regex/twin are shared borrows of
            // `self.heap` — they coexist. Prefer the byte-optimized twin; fall
            // back to the base program when the twin compile failed.
            let subj: &str = match self.heap.get(s_idx) {
                HeapObj::Str(js) => js.as_str_wf(),
                _ => "",
            };
            #[cfg(feature = "safe-sandbox")]
            {
                let limits = self.instrument_regex_limits();
                let (found, usage) = match self.heap.get(re_idx) {
                    HeapObj::RegExp {
                        ascii_twin: Some(Some(twin)),
                        ..
                    } => {
                        let mut matches = twin.find_from_ascii_with_limits(subj, start, limits);
                        let found = matches.next();
                        (found, matches.match_usage())
                    }
                    HeapObj::RegExp { regex, .. } => {
                        let mut matches = regex.find_from_ascii_with_limits(subj, start, limits);
                        let found = matches.next();
                        (found, matches.match_usage())
                    }
                    _ => (None, regress::MatchUsage::UNMETERED),
                };
                self.instrument_regex_usage(usage)
                    .map_err(|m| Thrown(m.into()))?;
                found
            }
            #[cfg(not(feature = "safe-sandbox"))]
            {
                match self.heap.get(re_idx) {
                    HeapObj::RegExp {
                        ascii_twin: Some(Some(twin)),
                        ..
                    } => twin.find_from_ascii(subj, start).next(),
                    HeapObj::RegExp { regex, .. } => regex.find_from_ascii(subj, start).next(),
                    _ => None,
                }
            }
        } else {
            #[cfg(feature = "safe-sandbox")]
            {
                let limits = self.instrument_regex_limits();
                let (found, usage) = match self.heap.get(re_idx) {
                    HeapObj::RegExp { regex, .. } => {
                        if unicode {
                            let mut matches =
                                regex.find_from_utf16_with_limits(&u16s, start, limits);
                            let found = matches.next();
                            (found, matches.match_usage())
                        } else {
                            let mut matches =
                                regex.find_from_ucs2_with_limits(&u16s, start, limits);
                            let found = matches.next();
                            (found, matches.match_usage())
                        }
                    }
                    _ => (None, regress::MatchUsage::UNMETERED),
                };
                self.instrument_regex_usage(usage)
                    .map_err(|m| Thrown(m.into()))?;
                found
            }
            #[cfg(not(feature = "safe-sandbox"))]
            {
                match self.heap.get(re_idx) {
                    HeapObj::RegExp { regex, .. } => {
                        if unicode {
                            regex.find_from_utf16(&u16s, start).next()
                        } else {
                            regex.find_from_ucs2(&u16s, start).next()
                        }
                    }
                    _ => None,
                }
            }
        };
        // Sticky: the match must begin exactly at the search start.
        let found = found.filter(|m| !(sticky && m.start() != start));
        let m = match found {
            Some(m) => m,
            None => {
                if stateful {
                    // RegExpBuiltinExec Set(R,"lastIndex",0,true): a non-writable
                    // lastIndex makes a failed global/sticky exec throw.
                    self.regexp_write_last_index(re_idx, Value::int(0))?;
                }
                return Ok(Value::NULL);
            }
        };
        let (mstart, mend) = (m.start(), m.end());
        if stateful {
            // RegExpBuiltinExec Set(R,"lastIndex",e,true) — spec step 15, BEFORE
            // the (unobservable) result construction; throws if non-writable.
            self.regexp_write_last_index(re_idx, Value::num(mend as f64))?;
        }
        // A unit-range slice of the subject: a byte slice of the heap string
        // for an ASCII subject, else a slice of the encoded unit buffer.
        let mk = |vm: &mut Self, r: std::ops::Range<usize>| -> Value {
            if is_ascii {
                vm.ascii_slice_value(s_idx, r)
            } else {
                vm.units_value(&u16s[r])
            }
        };
        // Annex B legacy RegExp statics — see `regexp_record_statics`. Only an
        // ASCII subject can defer: a non-ASCII slice reads the locally-decoded
        // `u16s` buffer, which does not outlive this call. The length bound
        // keeps the deferred record's `as u32` range casts from truncating
        // silently — unreachable in practice (a 4GB flat string), and a wrong
        // slice is exactly what it would produce.
        let defer = is_ascii && subj_units <= u32::MAX as usize;
        #[cfg(feature = "safe-sandbox")]
        let statics_bytes = if is_ascii {
            regexp_statics_materialization_bytes(
                &m,
                mstart,
                mend,
                subj_units,
                defer,
                ascii_slice_heap_bytes,
            )
        } else {
            regexp_statics_materialization_bytes(&m, mstart, mend, subj_units, defer, |range| {
                utf16_slice_heap_bytes(&u16s, range)
            })
        };
        #[cfg(feature = "safe-sandbox")]
        let statics_reservation = self
            .instrument_reserve_regex_transient(statics_bytes)
            .map_err(|message| Thrown(message.into()))?;
        self.regexp_record_statics(&m, input_val, s_idx, mstart, mend, subj_units, defer, &mk);
        // The materialized strings now belong to the audited VM heap. Release
        // their provisional native charge before reserving result storage so
        // the same bytes cannot consume heap headroom twice.
        #[cfg(feature = "safe-sandbox")]
        drop(statics_reservation);
        if !build {
            // `test`: nothing below is reachable, and with slot 1 deferred there is
            // no longer any string to build here at all.
            return Ok(Value::TRUE);
        }
        #[cfg(feature = "safe-sandbox")]
        let result_bytes = if is_ascii {
            regexp_result_materialization_bytes(
                &m,
                mstart,
                mend,
                has_indices,
                ascii_slice_heap_bytes,
            )
        } else {
            regexp_result_materialization_bytes(&m, mstart, mend, has_indices, |range| {
                utf16_slice_heap_bytes(&u16s, range)
            })
        };
        #[cfg(feature = "safe-sandbox")]
        let result_reservation = self
            .instrument_reserve_regex_transient(result_bytes)
            .map_err(|message| Thrown(message.into()))?;
        let result = self.regexp_build_result(&m, input_val, mstart, mend, has_indices, &mk);
        #[cfg(feature = "safe-sandbox")]
        drop(result_reservation);
        Ok(result)
    }

    /// Record the Annex B legacy RegExp statics (RegExp.input/$_, lastMatch/$&,
    /// lastParen/$+, leftContext/$`, rightContext/$', $1–$9) for successful
    /// match `m`: refreshed by EVERY successful RegExpBuiltinExec — `exec`,
    /// `test`, the fused matchAll step, and the String / RegExp methods that
    /// funnel through the builtin. Every subject slice goes through `mk`, so
    /// each caller monomorphizes its own slicer (see `regexp_build_result`).
    ///
    /// Slots 2..=13 (lastParen, leftContext, rightContext, $1..$9) are all
    /// SLICES OF THE SUBJECT, and `ascii_slice_value` copies: `as_bytes()[r]
    /// .to_vec()`, an `is_ascii` rescan in `from_wtf8`, and a heap slot. So the
    /// eager form copied leftContext + rightContext — together ~87% of the
    /// subject — on EVERY successful match, `test` included (the `!build`
    /// early-out sits between this and the result build), plus one slice per
    /// capture that the result array then sliced again. Virtually no program
    /// reads `RegExp.leftContext`.
    ///
    /// `defer` is the caller's proof that every slice can be re-derived later
    /// (an ASCII subject whose length fits the record's u32 ranges): root the
    /// subject and keep unit RANGES, and materialise all THIRTEEN on the first
    /// legacy-static getter read (see `Vm::regexp_last_materialise`). Only
    /// slot 0 stays eager — `input_val` is already a Value.
    ///
    /// Slot 1 (lastMatch) was eager on the stated grounds that the whole-match
    /// slice "is computed for the result array regardless". That holds for
    /// `exec` and NOT for `test`, which returns a boolean: every successful
    /// `.test()` was paying one `ascii_slice_value` — a malloc, a memcpy of
    /// the matched span, an `is_ascii` rescan of those same bytes, and a heap
    /// slot — for a string nothing ever read. On `regex-log-scan`'s anchored
    /// phase the match IS the whole ~112-byte line, ~90k times.
    ///
    /// MEASURED (tools/bench.py --ab-env against the same binary, 21 paired
    /// reps): ablating this block entirely was -8.65% on regex-log-scan
    /// [-8.86, -7.77], 2015ms -> 1844ms. That ablation is the ceiling this is
    /// aiming at, and it is reached whenever the statics go unread.
    #[inline]
    fn regexp_record_statics<F>(
        &mut self,
        m: &regress::Match,
        input_val: Value,
        s_idx: u32,
        mstart: usize,
        mend: usize,
        subj_units: usize,
        defer: bool,
        mk: &F,
    ) where
        F: Fn(&mut Self, std::ops::Range<usize>) -> Value,
    {
        if defer {
            // ranges[i] is slot 1+i: lastMatch, lastParen, leftContext,
            // rightContext, $1..$9.
            let mut ranges: [Option<(u32, u32)>; 13] = [None; 13];
            ranges[0] = Some((mstart as u32, mend as u32));
            // lastParen: the LAST participating capture, "" when none did.
            ranges[1] = m
                .captures
                .iter()
                .rev()
                .find_map(|c| c.clone())
                .map(|r| (r.start as u32, r.end as u32));
            ranges[2] = Some((0, mstart as u32));
            ranges[3] = Some((mend as u32, subj_units as u32));
            for i in 0..9 {
                ranges[4 + i] = m
                    .captures
                    .get(i)
                    .and_then(|c| c.clone())
                    .map(|r| (r.start as u32, r.end as u32));
            }
            // `regexp_last_lazy` being `Some` is what routes slots >= 1
            // through materialisation first, so the 13 tail slots are
            // placeholders the getter never returns — when the record is
            // already 14 wide only slot 0 needs storing (B118: the per-step
            // clear+resize wrote 14 slots per successful exec; any stale tail
            // value is overwritten by `regexp_last_materialise` before a
            // getter can see it, and is at worst a 13-value GC root).
            if self.regexp_last.len() == 14 {
                self.regexp_last[0] = input_val;
            } else {
                self.regexp_last.clear();
                self.regexp_last.push(input_val);
                self.regexp_last.resize(14, Value::UNDEFINED);
            }
            self.regexp_last_lazy = Some(RegexpLastLazy {
                subj: input_val,
                subj_idx: s_idx,
                ranges,
            });
        } else {
            // Cannot defer: slice all thirteen eagerly through `mk`.
            let empty = self.alloc_str(String::new());
            #[cfg(feature = "safe-sandbox")]
            {
                // Safe VMs preallocate this fixed record at construction and
                // audit its capacity as resident VM memory. Reuse it in place:
                // a fresh Vec here would allocate infallibly for every
                // non-ASCII success and discard that precharged backing.
                debug_assert_eq!(self.regexp_last.len(), 14);
                debug_assert!(self.regexp_last.capacity() >= 14);
                self.regexp_last.fill(empty);
                self.regexp_last[0] = input_val;
                self.regexp_last[1] = mk(self, mstart..mend);
                self.regexp_last[2] = match m.captures.iter().rev().find_map(|c| c.clone()) {
                    Some(r) => mk(self, r),
                    None => empty,
                };
                self.regexp_last[3] = mk(self, 0..mstart);
                self.regexp_last[4] = mk(self, mend..subj_units);
                for i in 0..9 {
                    self.regexp_last[5 + i] = match m.captures.get(i).and_then(|c| c.clone()) {
                        Some(r) => mk(self, r),
                        None => empty,
                    };
                }
                self.regexp_last_lazy = None;
            }
            #[cfg(not(feature = "safe-sandbox"))]
            {
                let mut rec = Vec::with_capacity(14);
                rec.push(input_val);
                let whole_units = mk(self, mstart..mend);
                rec.push(whole_units);
                rec.push(match m.captures.iter().rev().find_map(|c| c.clone()) {
                    Some(r) => mk(self, r),
                    None => empty,
                });
                rec.push(mk(self, 0..mstart));
                rec.push(mk(self, mend..subj_units));
                for i in 0..9 {
                    rec.push(match m.captures.get(i).and_then(|c| c.clone()) {
                        Some(r) => mk(self, r),
                        None => empty,
                    });
                }
                self.regexp_last = rec;
                self.regexp_last_lazy = None;
            }
        }
    }

    /// Build the RegExpBuiltinExec result array for successful match `m`:
    /// element 0 the whole-match slice, one element per capture, the compact
    /// `index`/`input`/`groups`/`indices` record, and (under `/d`) the
    /// `indices` array. Every subject slice goes through `mk` — generic so
    /// each caller's instantiation monomorphizes its slicer: the pristine
    /// exec's ascii-or-units closure, the fused slim exec's ASCII-only form
    /// (byte-identical outputs, since the pristine closure's ascii arm IS
    /// `ascii_slice_value`). Allocation order is load-bearing for heap-index
    /// assignment: whole + capture slices, groups object, result array,
    /// indices pair arrays, indices array.
    #[inline]
    fn regexp_build_result<F>(
        &mut self,
        m: &regress::Match,
        input_val: Value,
        mstart: usize,
        mend: usize,
        has_indices: bool,
        mk: &F,
    ) -> Value
    where
        F: Fn(&mut Self, std::ops::Range<usize>) -> Value,
    {
        let whole = mk(self, mstart..mend);
        let mut elems = Vec::with_capacity(1 + m.captures.len());
        elems.push(whole);
        for cap in &m.captures {
            let v = match cap {
                Some(r) => mk(self, r.clone()),
                None => Value::UNDEFINED,
            };
            elems.push(v);
        }
        let named: Vec<(String, Option<std::ops::Range<usize>>)> =
            m.named_groups().map(|(n, r)| (n.to_string(), r)).collect();
        let groups = if named.is_empty() {
            Value::UNDEFINED
        } else {
            let mut gm = ObjMap::with_capacity(named.len());
            for (name, r) in &named {
                let v = match r {
                    Some(r) => m
                        .captures
                        .iter()
                        .zip(elems.iter().skip(1))
                        .find_map(|(capture, value)| {
                            (capture.as_ref() == Some(r)).then_some(*value)
                        })
                        .expect("named capture range originates in the indexed capture list"),
                    None => Value::UNDEFINED,
                };
                gm.set(name, v);
            }
            let gidx = self.heap.alloc(HeapObj::Object(Box::new(gm)));
            // The groups object is OrdinaryObjectCreate(null) — no prototype.
            self.proto_of.insert(gidx, Value::NULL);
            Value::heap(gidx)
        };
        let arr_idx = self.alloc_array_current_realm(elems).heap_index();
        let index_v = Value::num(mstart as f64);
        // index/input/groups are real own data properties of the result array
        // (writable, enumerable, configurable) so reflection sees them.
        let attr = PropAttr {
            writable: true,
            enumerable: true,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        // `/d` (hasIndices): an `indices` array of [start,end] unit ranges for
        // the whole match + each capture group, with `.groups` for named groups.
        let indices_v = if has_indices {
            let mk = |vm: &mut Self, r: &std::ops::Range<usize>| -> Value {
                let s = Value::num(r.start as f64);
                let e = Value::num(r.end as f64);
                vm.alloc_array_current_realm(vec![s, e])
            };
            let mut idx_elems = Vec::with_capacity(m.captures.len().saturating_add(1));
            idx_elems.push(mk(self, &(mstart..mend)));
            for cap in &m.captures {
                idx_elems.push(match cap {
                    Some(r) => mk(self, r),
                    None => Value::UNDEFINED,
                });
            }
            let idx_groups = if named.is_empty() {
                Value::UNDEFINED
            } else {
                let mut gm = ObjMap::with_capacity(named.len());
                for (name, r) in &named {
                    let v = match r {
                        Some(r) => mk(self, r),
                        None => Value::UNDEFINED,
                    };
                    gm.set(name, v);
                }
                let gidx = self.heap.alloc(HeapObj::Object(Box::new(gm)));
                self.proto_of.insert(gidx, Value::NULL);
                Value::heap(gidx)
            };
            let indices_arr = self.alloc_array_current_realm(idx_elems).heap_index();
            self.arr_props
                .entry(indices_arr)
                .or_insert_with(ObjMap::new_side_table)
                .define("groups", idx_groups, attr);
            Value::heap(indices_arr)
        } else {
            Value::UNDEFINED
        };
        // `arr_idx` is a fresh heap slot. GC prunes both property tables against
        // the mark bits before a slot can be recycled, so neither can carry a
        // stale entry from the previous occupant.
        // Keep the pristine standard fields in one fixed record: this removes
        // the per-result ObjMap, three key-string allocations, and three Vec
        // allocations. Mutation/reflection materialises the exact ordinary
        // data properties lazily; direct reads and presence checks stay compact.
        debug_assert!(!self.arr_props.contains_key(&arr_idx));
        debug_assert!(!self.regexp_result_props.contains_key(&arr_idx));
        self.regexp_result_props.insert(
            arr_idx,
            RegexpResultProps {
                values: [index_v, input_val, groups, indices_v],
            },
        );
        rxstats::count_compact();
        if !match_variant_enabled() {
            // Off-switch: reproduce the eager representation (an ordinary
            // `ObjMap` in `arr_props`) so the compact form is A/B-able.
            self.materialize_regexp_result_props(arr_idx);
        }
        Value::heap(arr_idx)
    }

    /// The SLIM per-call exec for the fused matchAll step (B124): the same
    /// RegExpBuiltinExec `regexp_exec_impl_prebits` performs, minus every
    /// protocol step the `ITFB_FUSED` creation proof already paid for. What
    /// is elided, and why each elision is sound:
    ///
    ///  - ToString(subject): the iterator record's subject IS a string Value
    ///    (identity conversion — nothing to do).
    ///  - the lastIndex re-read + ToInteger: the caller just read the slot
    ///    for its `is_number` guard and passes the Value through; the inline
    ///    truncation below is exactly `to_integer_or_zero` on the numeric
    ///    domain (and only engine-written numbers ever reach this slot).
    ///  - the per-step flag decode: `fbits` was captured at creation and the
    ///    matcher is engine-internal — no user reference exists to
    ///    `compile()` new flags (the `Some(prebits)` soundness argument).
    ///  - flatten + `is_ascii` + `str_units` heap.gets: `ITFB_FUSED` encodes
    ///    "flat-ASCII subject", proven at creation and stable for the
    ///    record's life (strings are immutable, Cons→Str flattening is
    ///    irreversible, heap slots don't move); the unit length IS the byte
    ///    length of the one subject borrow the matcher needs anyway.
    ///  - the per-step `ensure_regexp_ascii_twin` probe: the twin field is
    ///    monotonic (only ever set, never cleared — clearing needs a user
    ///    `compile()`, impossible here), so the matcher heap.get the search
    ///    performs anyway doubles as the twin check; the build runs at most
    ///    once, cold, then every later step sees `Some`.
    ///  - `regexp_write_last_index`'s `arr_props` probe: the engine-internal
    ///    matcher can never gain a `lastIndex` attribute entry or a freeze
    ///    marker (both need a user reference), so the throwing form is
    ///    unreachable — the heap slot is written directly (debug-asserted).
    ///    The write-through itself is REQUIRED even though no user reads the
    ///    matcher: a mid-iteration `RegExp.prototype.exec` swap fails the
    ///    per-step memo and the fallback resumes from this heap slot.
    ///  - the caller's result-array empty-match probe: the search knew
    ///    `mstart == mend`; it is returned instead of re-derived from
    ///    element 0.
    ///
    /// The Annex-B statics deferral and the result build are VERBATIM the
    /// shared impl's — per-step statics stay observable (`RegExp.$1` after a
    /// matchAll iteration) and the result array is byte-identical. The
    /// `prof::enter` mark and the `gc_lock_guard` are kept (~2ns each; the
    /// guard's removal is a separate ablation — it is provably safe today
    /// but a landmine if a future edit re-enters the interpreter here).
    ///
    /// Nothing in here can throw (the two throwing steps of the full impl —
    /// ToLength on an object and the non-writable lastIndex Set — are the
    /// elided ones), so the return is a plain pair: `(NULL, None)` for no
    /// match, else the result array plus `Some(match end)` iff the match was
    /// EMPTY — the caller's AdvanceStringIndex trigger.
    fn regexp_exec_fused_slim(
        &mut self,
        re_idx: u32,
        input_val: Value,
        fbits: u8,
        subj_units: usize,
        li_v: Value,
    ) -> (Value, Option<usize>) {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::RegexExec);
        // The result pieces below live in Rust locals — hold GC off until we
        // return, exactly as the shared impl does.
        let _gc = self.gc_lock_guard();
        debug_assert!(li_v.is_number(), "the fused-step guard admits numbers only");
        // ToLength on an engine-written number: truncate toward zero, floor
        // at 0, cap 2^53-1 — `to_integer_or_zero` + clamp without the
        // observable-valueOf valve (unreachable for a number).
        let li = {
            let d = li_v.as_f64().trunc();
            let d = if d.is_nan() { 0.0 } else { d };
            d.max(0.0).min(((1u64 << 53) - 1) as f64) as usize
        };
        let global = fbits & ITFB_GLOBAL != 0;
        let sticky = fbits & ITFB_STICKY != 0;
        let has_indices = fbits & ITFB_INDICES != 0;
        let stateful = global || sticky;
        // Step 9: a non-global, non-sticky regex always searches from 0
        // (unreachable today — ITFB_FUSED implies `g` — but kept parallel).
        let start = if stateful { li } else { 0 };
        let s_idx = input_val.heap_index();
        // The record's creation-time unit length (== byte length: the subject
        // is flat ASCII and immutable) makes the rare past-the-end bail
        // decidable before the subject borrow; the shared no-match tail below
        // handles it. `ZIPP_NO_ITER_SUBJ_UNITS=1` ignores the cache and
        // re-derives `subj.len()` inside the loop, as before.
        let use_cached_units = iter_subj_units_enabled();
        // The matcher fetch the search needs anyway doubles as the twin
        // probe; `built_twin` bounds the cold build at one attempt so a
        // (impossible today) non-RegExp slot cannot loop.
        let mut built_twin = false;
        let found = if use_cached_units && start > subj_units {
            None
        } else {
            loop {
                let subj: &str = match self.heap.get(s_idx) {
                    HeapObj::Str(js) => {
                        debug_assert!(js.is_ascii(), "ITFB_FUSED encodes a flat-ASCII subject");
                        debug_assert_eq!(
                            subj_units,
                            js.as_bytes().len(),
                            "cached units must equal the flat-ASCII byte length"
                        );
                        js.as_str_wf()
                    }
                    _ => "",
                };
                if !use_cached_units && start > subj.len() {
                    break None;
                }
                match self.heap.get(re_idx) {
                    HeapObj::RegExp {
                        ascii_twin: Some(Some(twin)),
                        ..
                    } => {
                        break twin.find_from_ascii(subj, start).next();
                    }
                    // Twin compile failed once: the base program is byte-safe too.
                    HeapObj::RegExp {
                        ascii_twin: Some(None),
                        regex,
                        ..
                    } => {
                        break regex.find_from_ascii(subj, start).next();
                    }
                    HeapObj::RegExp {
                        ascii_twin: None, ..
                    } if !built_twin => {}
                    HeapObj::RegExp { regex, .. } => {
                        break regex.find_from_ascii(subj, start).next();
                    }
                    _ => break None,
                }
                // Cold, at most once per matcher: build (or record the failure
                // of) the byte-optimized twin, then re-enter with it in place —
                // `ascii_twin` is monotonic, so the next pass takes a `Some` arm.
                // The fused creation arm normally ensures the SOURCE's twin so
                // its clone arrives here as `Some`; this arm stays live for
                // `ZIPP_NO_TWIN_AT_CREATE=1` and for any matcher whose source
                // carried `None` at creation.
                built_twin = true;
                self.ensure_regexp_ascii_twin(re_idx);
            }
        };
        // Sticky: the match must begin exactly at the search start.
        let found = found.filter(|m| !(sticky && m.start() != start));
        let m = match found {
            Some(m) => m,
            None => {
                if stateful {
                    // Set(R,"lastIndex",0,true) — the direct form of
                    // `regexp_write_last_index`'s fast path (see the doc
                    // above for why the slow form is unreachable).
                    debug_assert!(self.arr_props.get(&re_idx).is_none());
                    if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(re_idx) {
                        *last_index = Value::int(0);
                    }
                }
                return (Value::NULL, None);
            }
        };
        let (mstart, mend) = (m.start(), m.end());
        if stateful {
            // Set(R,"lastIndex",e,true) — spec step 15, BEFORE the result
            // construction; direct write, same argument as above.
            debug_assert!(self.arr_props.get(&re_idx).is_none());
            if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(re_idx) {
                *last_index = Value::num(mend as f64);
            }
        }
        // Annex-B legacy statics + result build: the shared helpers with `mk`
        // monomorphized to the ASCII slicer — `is_ascii` is proven by
        // ITFB_FUSED, so this is the pristine instantiation byte-for-byte
        // (its closure's ascii arm IS `ascii_slice_value`) and the deferral
        // predicate reduces to the u32 length bound alone.
        let mka =
            |vm: &mut Self, r: std::ops::Range<usize>| -> Value { vm.ascii_slice_value(s_idx, r) };
        self.regexp_record_statics(
            &m,
            input_val,
            s_idx,
            mstart,
            mend,
            subj_units,
            subj_units <= u32::MAX as usize,
            &mka,
        );
        let arr = self.regexp_build_result(&m, input_val, mstart, mend, has_indices, &mka);
        (arr, (mstart == mend).then_some(mend))
    }

    /// Publish ONE drained matchAll triple as a full observable step: the
    /// stateful `lastIndex = matchEnd` write (spec step 15, BEFORE the result
    /// is handed over), the per-step Annex-B statics record, and the result
    /// array — the publish half of `regexp_exec_fused_slim`, driven from a
    /// [`MatchBatch`] triple instead of a live search. The triple is
    /// reassembled into a `regress::Match` so the shared helpers
    /// (`regexp_record_statics` / `regexp_build_result`) run VERBATIM with
    /// the same ASCII slicer instantiation — byte-identical output by
    /// construction. Only reachable for batch-eligible iterations (flat-ASCII
    /// subject, no named groups, no `/d`), so the assembled match's empty
    /// group-name table is exact.
    #[allow(clippy::too_many_arguments)]
    fn fused_publish(
        &mut self,
        re_idx: u32,
        s_idx: u32,
        input_val: Value,
        subj_units: usize,
        mstart: usize,
        mend: usize,
        caps: Vec<Option<std::ops::Range<usize>>>,
        fbits: u8,
    ) -> Value {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::RegexExec);
        // The result pieces below live in Rust locals — hold GC off until we
        // return, exactly as the slim exec does.
        let _gc = self.gc_lock_guard();
        let has_indices = fbits & ITFB_INDICES != 0;
        debug_assert!(!has_indices, "the batch gate excludes /d");
        if fbits & (ITFB_GLOBAL | ITFB_STICKY) != 0 {
            // Set(R,"lastIndex",e,true) — the direct write; see the slim
            // exec's doc for why the throwing form is unreachable on the
            // engine-internal matcher.
            debug_assert!(self.arr_props.get(&re_idx).is_none());
            if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(re_idx) {
                *last_index = Value::num(mend as f64);
            }
        }
        let m = regress::Match::from_scan_parts(mstart..mend, caps);
        let mka =
            |vm: &mut Self, r: std::ops::Range<usize>| -> Value { vm.ascii_slice_value(s_idx, r) };
        self.regexp_record_statics(
            &m,
            input_val,
            s_idx,
            mstart,
            mend,
            subj_units,
            subj_units <= u32::MAX as usize,
            &mka,
        );
        let out = self.regexp_build_result(&m, input_val, mstart, mend, has_indices, &mka);
        // Hand the capture Vec back to the scratch slot so the next publish
        // re-mallocs nothing (the caller took it from there).
        let mut caps = m.captures;
        caps.clear();
        self.matchall_caps_scratch = caps;
        out
    }

    /// Allocate the string for a byte-range slice of the (all-ASCII, flat)
    /// subject string at `s_idx` — for ASCII, byte offsets are unit offsets.
    /// Materialise the deferred Annex B legacy statics (slots 2..=13 — lastParen,
    /// leftContext, rightContext, `$1`..`$9`) into `regexp_last`, if a successful
    /// ASCII match left them as ranges.
    ///
    /// All twelve are done at once and the record cleared, so the cost is paid
    /// once per match no matter how many statics are read. A read of these is rare
    /// enough that splitting it per slot would only add a bitmask to the hot
    /// producer. `None` ranges are the empty string, exactly as the eager form
    /// pushed `empty`.
    ///
    /// Callers: `REGEXP_LEGACY_GET` for any slot >= 2. Slots 0/1 never defer.
    pub(crate) fn regexp_last_materialise(&mut self) -> Result<(), Thrown> {
        // COPY the record out and clear it only AFTER the slicing. `take()`ing it
        // up front would unroot `subj` for the duration — `ascii_slice_value`
        // allocates, and an allocation that trips `gc_requested` must not be able
        // to reach a collection while the only reference to the subject is a local.
        // `regexp_last[0]` usually roots it too, but not always: `RegExp.input = x`
        // overwrites slot 0 while the ranges still point at the old subject.
        let Some(lazy) = self.regexp_last_lazy.as_ref() else {
            return Ok(());
        };
        let subj_idx = lazy.subj_idx;
        let ranges = lazy.ranges;
        #[cfg(feature = "safe-sandbox")]
        let record_requested = 14usize
            .saturating_sub(self.regexp_last.capacity())
            .saturating_mul(std::mem::size_of::<Value>());
        #[cfg(feature = "safe-sandbox")]
        let slice_bytes = ranges.iter().flatten().fold(0usize, |total, (start, end)| {
            total.saturating_add((*end as usize).saturating_sub(*start as usize))
        });
        #[cfg(feature = "safe-sandbox")]
        let mut reservation = self
            .instrument_reserve_regex_transient(slice_bytes.saturating_add(record_requested))
            .map_err(|message| Thrown(message.into()))?;
        if self.regexp_last.len() < 14 {
            // A `RegExp.input = x` write with no prior match resizes to 14; this
            // only guards the impossible ordering rather than indexing blind.
            #[cfg(feature = "safe-sandbox")]
            let record_actual = {
                let old_capacity = self.regexp_last.capacity();
                if self
                    .regexp_last
                    .try_reserve_exact(14usize.saturating_sub(self.regexp_last.len()))
                    .is_err()
                {
                    return Err(Thrown(self.instrument_regex_memory_exhausted().into()));
                }
                let actual = self
                    .regexp_last
                    .capacity()
                    .saturating_sub(old_capacity)
                    .saturating_mul(std::mem::size_of::<Value>());
                regex_reconcile_transient(self, &mut reservation, record_requested, actual)?;
                actual
            };
            self.regexp_last.resize(14, Value::UNDEFINED);
            #[cfg(feature = "safe-sandbox")]
            self.instrument_shrink_regex_transient(&mut reservation, record_actual);
        }
        for (i, r) in ranges.iter().enumerate() {
            #[cfg(feature = "safe-sandbox")]
            let value = match *r {
                Some((s, e)) => regex_ascii_slice_precharged(
                    self,
                    &mut reservation,
                    subj_idx,
                    s as usize..e as usize,
                )?,
                None => self.alloc_str(String::new()),
            };
            #[cfg(not(feature = "safe-sandbox"))]
            let value = match *r {
                Some((s, e)) => self.ascii_slice_value(subj_idx, s as usize..e as usize),
                None => self.alloc_str(String::new()),
            };
            self.regexp_last[1 + i] = value;
        }
        self.regexp_last_lazy = None;
        Ok(())
    }

    pub(crate) fn ascii_slice_value(&mut self, s_idx: u32, r: std::ops::Range<usize>) -> Value {
        // W11 (B124): a slice of a KNOWN-ASCII subject is ascii by
        // construction — `from_ascii` skips `from_wtf8`'s linear rescan
        // (~1.8M slices/run on regex-log-scan). Non-ascii subjects keep the
        // full canonicalizing path.
        fn ascii_slice_fast() -> bool {
            use std::sync::atomic::{AtomicU8, Ordering};
            static STATE: AtomicU8 = AtomicU8::new(0);
            match STATE.load(Ordering::Relaxed) {
                1 => true,
                2 => false,
                _ => {
                    let on = std::env::var_os("ZIPP_NO_ASCII_SLICE_FAST").is_none();
                    STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
                    on
                }
            }
        }
        let (bytes, subject_ascii): (Vec<u8>, bool) = match self.heap.get(s_idx) {
            HeapObj::Str(js) => (
                js.as_bytes()[r].to_vec(),
                js.is_ascii() && ascii_slice_fast(),
            ),
            _ => (Vec::new(), false),
        };
        let js = if subject_ascii {
            crate::heap::JsStr::from_ascii(bytes)
        } else {
            crate::heap::JsStr::from_wtf8(bytes)
        };
        Value::heap(self.heap.alloc_js(js))
    }

    /// `Set(R, "lastIndex", v, true)` on a real RegExp. Fast path writes the
    /// heap slot directly when nothing can make the Set observable or fail:
    /// no arr_props entry for the object (so no attr override and no freeze
    /// marker) or one without a `lastIndex` key and not frozen. Otherwise the
    /// full set_prop runs (a non-writable lastIndex must throw).
    pub(crate) fn regexp_write_last_index(&mut self, re_idx: u32, v: Value) -> Result<(), Thrown> {
        let fast = match self.arr_props.get(&re_idx) {
            None => true,
            Some(m) => !m.frozen && m.pos("lastIndex").is_none(),
        };
        if fast {
            if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(re_idx) {
                *last_index = v;
            }
            Ok(())
        } else {
            self.set_prop(Value::heap(re_idx), "lastIndex", v, true)?;
            Ok(())
        }
    }

    /// True when the RegExpExec PROTOCOL's `Get(R, "exec")` is UNOBSERVABLE
    /// and yields the intrinsic for instance `re`: its [[Prototype]] is
    /// exactly %RegExp.prototype%, it has no own `exec`, and the prototype's
    /// `exec` is still the intrinsic native data property. The drivers
    /// (@@match/@@replace/@@split/matchAll/exec_abstract) may then call
    /// `regexp_exec` directly, skipping the Get + generic call dispatch.
    /// The eight per-flag accessor names, in the canonical order
    /// `get RegExp.prototype.flags` reads them.
    const FLAG_ACCESSORS: [(&'static str, u16); 8] = [
        ("hasIndices", native::REGEXP_GET_HASINDICES),
        ("global", native::REGEXP_GET_GLOBAL),
        ("ignoreCase", native::REGEXP_GET_IGNORECASE),
        ("multiline", native::REGEXP_GET_MULTILINE),
        ("dotAll", native::REGEXP_GET_DOTALL),
        ("unicode", native::REGEXP_GET_UNICODE),
        ("unicodeSets", native::REGEXP_GET_UNICODESETS),
        ("sticky", native::REGEXP_GET_STICKY),
    ];

    /// Canonical flag characters, index-parallel to [`Vm::FLAG_ACCESSORS`].
    const FLAG_CHARS: [char; 8] = ['d', 'g', 'i', 'm', 's', 'u', 'v', 'y'];

    /// `Some(flags)` when reading `receiver.flags` provably reduces to the internal
    /// flag string: `re` is a real RegExp, `receiver` IS that object (not a
    /// `Reflect.get` with a foreign receiver), its `[[Prototype]]` is
    /// %RegExp.prototype%, it shadows none of the eight flag names, and each of those
    /// eight on the prototype is still its intrinsic ACCESSOR.
    ///
    /// The internal string is stored **as the program wrote it**, NOT canonicalised —
    /// `new RegExp("a", "ig")` keeps `"ig"` — so the result is rebuilt in canonical
    /// `dgimsuvy` order by membership test, exactly as the eight reads would. Returning
    /// the raw field was the first version of this and it was a conformance regression
    /// (`"ig"` where node says `"gi"`); `tests/regexp_flags_fast_path.rs` diffs the two
    /// paths over all 192 legal flag combinations and both spellings of each, which is
    /// what caught it.
    ///
    /// Eight `contains` scans of a ≤8-byte string replace eight full property
    /// traversals, so the shortcut still stands.
    /// Allocation-free proof for the full observable `Get(rx, "flags")`
    /// sequence: exact receiver/prototype, no own shadows, the live `flags`
    /// getter, and all eight flag accessors still hold their exact natives.
    /// The stored internal flag string itself is intentionally not copied.
    pub(crate) fn regexp_pristine_flag_accessors_ok(&self, re: u32, receiver: Value) -> bool {
        if !receiver.is_heap() || receiver.heap_index() != re {
            return false;
        }
        if !matches!(self.heap.get(re), HeapObj::RegExp { .. }) {
            return false;
        }
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        // An own shadow of any flag name (or of `flags` itself) makes the reads
        // observable again.
        if let Some(m) = self.arr_props.get(&re) {
            if m.pos("flags").is_some()
                || Self::FLAG_ACCESSORS.iter().any(|(n, _)| m.pos(n).is_some())
            {
                return false;
            }
        }
        let proto = match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => m,
            _ => return false,
        };
        let flags_ok =
            self.regexp_proto_slot_is_intrinsic(proto, "flags", true, native::REGEXP_GET_FLAGS);
        flags_ok
            && Self::FLAG_ACCESSORS
                .iter()
                .all(|(name, want)| self.regexp_proto_slot_is_intrinsic(proto, name, true, *want))
    }

    pub(crate) fn regexp_pristine_flags(&self, re: u32, receiver: Value) -> Option<String> {
        if !self.regexp_pristine_flag_accessors_ok(re, receiver) {
            return None;
        }
        let flags = match self.heap.get(re) {
            HeapObj::RegExp { flags, .. } => flags,
            _ => return None,
        };
        let mut out = String::with_capacity(8);
        for (i, _) in Self::FLAG_ACCESSORS.iter().enumerate() {
            // Canonical `dgimsuvy` order, by membership — the stored string is in
            // SOURCE order, so it cannot be returned as-is.
            let ch = Self::FLAG_CHARS[i];
            if flags.as_bytes().contains(&(ch as u8)) {
                out.push(ch);
            }
        }
        Some(out)
    }

    /// True when `re.<name>` provably resolves to the intrinsic native `want`, so the
    /// receiver-kind builtin fast path may serve it inline instead of going through
    /// `get_prop` + `call_value`.
    ///
    /// This is the OVERRIDE-SAFE guard the other receiver-kind arms lack. B68 measured
    /// that `String.prototype.indexOf = f; "abc".indexOf("b")` still answers `1` in zipp
    /// against node's override, because those arms bind a builtin from its NAME alone;
    /// RegExp was correct only because it had no arm at all. So an arm may be added here
    /// ONLY behind a check that the name still reaches the intrinsic — all three ways it
    /// could stop doing so:
    ///
    /// * the instance's `[[Prototype]]` is no longer %RegExp.prototype% (a subclass, or
    ///   `setPrototypeOf`);
    /// * the instance has an OWN `name` shadowing the prototype;
    /// * %RegExp.prototype%'s `name` slot no longer holds the intrinsic native — it was
    ///   reassigned, deleted, or turned into an accessor.
    ///
    /// Deliberately NOT cached behind a version: B67 established that `ObjMap::set`
    /// bumps the heap version only when a key is ADDED (`if added`), so a plain
    /// `RegExp.prototype.test = f` overwrite would leave a version-keyed cache stale and
    /// silently reinstate the bug this guard exists to prevent. The uncached form is
    /// affordable — B68's ablation put the near-identical `regexp_exec_fast_ok` at ~7% of
    /// the call, while the generic path this skips is the bulk of it.
    pub(crate) fn regexp_method_is_intrinsic(&self, re: u32, name: &str, want: u16) -> bool {
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self
            .arr_props
            .get(&re)
            .is_some_and(|m| m.pos(name).is_some())
        {
            return false;
        }
        match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => self.regexp_proto_slot_is_intrinsic(m, name, false, want),
            _ => false,
        }
    }

    pub(crate) fn regexp_exec_fast_ok(&self, re: u32) -> bool {
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self
            .arr_props
            .get(&re)
            .is_some_and(|m| m.pos("exec").is_some())
        {
            return false;
        }
        match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => {
                self.regexp_proto_slot_is_intrinsic(m, "exec", false, native::REGEXP_EXEC)
            }
            _ => false,
        }
    }

    /// Serve a captured `RegExpMethod` call for `test` (`is_test = true`) or
    /// `exec` (`is_test = false`). `Ok(None)` is a PURE guard decline: the
    /// caller ordinary-calls the exact `callee` and receiver already captured
    /// before argument evaluation.
    ///
    /// The proof is deliberately the existing one, not a second approximation:
    /// Exact setup-time callee identity establishes which native was fetched;
    /// this intentionally does not re-read the outer property after arguments
    /// run. Intrinsic `test` also uses `regexp_exec_fast_ok`, because RegExpExec
    /// performs a second, separately observable Get of `exec`. Only after both
    /// proofs succeed may the native wrappers be collapsed.
    ///
    /// Callers gate this with [`regexp_call_direct_enabled`]. `from_jit` exists
    /// only to keep the mechanism counters non-vacuous across both entry paths.
    pub(crate) fn regexp_call_direct(
        &mut self,
        callee: Value,
        recv: Value,
        input: Value,
        is_test: bool,
        from_jit: bool,
    ) -> Result<Option<Value>, Thrown> {
        let op = if is_test {
            crate::bytecode::RegExpMethod::Test
        } else {
            crate::bytecode::RegExpMethod::Exec
        };
        if !recv.is_heap()
            || !matches!(self.heap.get(recv.heap_index()), HeapObj::RegExp { .. })
            || !self.captured_regexp_method_is_intrinsic(op, callee)
            // RegExp.prototype.test performs ToString(input) BEFORE its
            // observable RegExpExec Get of `exec`. An object coercion can patch
            // that slot, invalidating a proof taken here; fail closed to the
            // native's full protocol. Primitive ToString cannot run user code.
            || (is_test && self.is_object_value(input))
        {
            rxstats::count_call_direct_decline();
            return Ok(None);
        }
        let re = recv.heap_index();
        let name = if is_test { "test" } else { "exec" };
        if is_test && !self.regexp_exec_fast_ok(re) {
            rxstats::count_call_direct_decline();
            return Ok(None);
        }

        // Preserve ZIPP_BUILTINSTATS' promise that a builtin invocation is
        // counted even though this lane no longer reaches builtins.rs.
        super::builtins::builtin_stats_count(self, recv, name);
        rxstats::count_call_direct_hit(is_test, from_jit);
        let result = if is_test {
            let r = self.regexp_exec_impl(re, input, false)?;
            Value::bool(r != Value::NULL)
        } else {
            self.regexp_exec(re, input)?
        };
        Ok(Some(result))
    }

    /// Ensure the BYTE-OPTIMIZED twin compile (`HeapObj::RegExp::ascii_twin`)
    /// for the RegExp at `re_idx` exists — built once, lazily, from the SAME
    /// pattern characters and flags as the heap regex (mirrors
    /// `build_regexp`, incl. the exact-bytes lone-surrogate form). A failed
    /// compile is recorded as `Some(None)` so it isn't retried; callers fall
    /// back to `find_from_ascii` on the unoptimized program (also byte-safe).
    pub(crate) fn ensure_regexp_ascii_twin(&mut self, re_idx: u32) {
        let (source, flags) = match self.heap.get(re_idx) {
            // Already computed (twin or recorded failure): nothing to do.
            HeapObj::RegExp {
                ascii_twin: Some(_),
                ..
            } => return,
            HeapObj::RegExp { source, flags, .. } => (source.clone(), flags.clone()),
            _ => return,
        };
        let rflags: String = flags.chars().filter(|c| "imsuv".contains(*c)).collect();
        let unicode_mode = flags.contains('u') || flags.contains('v');
        let compile_cps: Vec<u32> = match (self.regexp_exact_source.get(&re_idx), unicode_mode) {
            (Some(b), true) => crate::heap::wtf8_code_points(b).collect(),
            (Some(b), false) => {
                nonunicode_pattern_chars(&crate::heap::wtf8_units_iter(b).collect::<Vec<u16>>())
            }
            (None, true) => source.chars().map(u32::from).collect(),
            (None, false) => nonunicode_pattern_chars(&source.encode_utf16().collect::<Vec<u16>>()),
        };
        // Through the byteopt half of the compile cache (species clones of
        // the same pattern share one twin too).
        let cache_key = self
            .regexp_exact_source
            .get(&re_idx)
            .is_none()
            // The cache key owns its text, so the shared source is materialised
            // here — once per twin build, never per match.
            .then(|| (source.to_string(), rflags.clone(), true));
        let twin: Option<std::sync::Arc<regress::Regex>> = match cache_key
            .as_ref()
            .and_then(|k| self.regex_compile_cache.get(k))
        {
            Some(rc) => Some(rc.clone()),
            None => {
                let compiled = regress::Regex::from_unicode_byteopt(
                    compile_cps.iter().copied(),
                    rflags.as_str(),
                )
                .ok()
                .and_then(|program| {
                    // The twin is an optional optimisation: a per-program cap
                    // decline falls back to the authoritative compile without
                    // changing RegExp semantics. A heap preflight failure still
                    // latches the recorder's terminal resource status.
                    self.preflight_regex_program(&program)
                        .is_ok()
                        .then(|| std::sync::Arc::new(program))
                });
                if let (Some(k), Some(rc)) = (cache_key, compiled.as_ref()) {
                    #[cfg(feature = "safe-sandbox")]
                    const REGEX_TWIN_CACHE_LIMIT: usize = 32;
                    #[cfg(not(feature = "safe-sandbox"))]
                    const REGEX_TWIN_CACHE_LIMIT: usize = 512;
                    if self.regex_compile_cache.len() >= REGEX_TWIN_CACHE_LIMIT {
                        self.regex_compile_cache.clear();
                    }
                    self.regex_compile_cache.insert(k, rc.clone());
                }
                compiled
            }
        };
        if let HeapObj::RegExp { ascii_twin, .. } = self.heap.get_mut(re_idx) {
            *ascii_twin = Some(twin);
        }
    }

    /// The string's UTF-16 code units — EXACT: an astral scalar yields its two
    /// halves and a lone surrogate its own 0xD800–0xDFFF value (which is what
    /// lets a `\uD800` pattern match a real lone-surrogate subject). `v` must
    /// be a string value (callers come through `to_str_value`).
    #[cfg(not(feature = "safe-sandbox"))]
    pub(crate) fn value_units(&mut self, v: Value) -> Vec<u16> {
        if !v.is_heap() {
            return Vec::new();
        }
        let idx = v.heap_index();
        self.heap.flatten(idx);
        match self.heap.get(idx) {
            HeapObj::Str(js) if js.is_ascii() => js.as_bytes().iter().map(|&b| b as u16).collect(),
            HeapObj::Str(js) => js.units_iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Allocate the string for a unit-slice of a subject — built as WTF-8 so
    /// lone surrogates round-trip exactly and a covered (high, low) pair
    /// recombines into its astral scalar (canonical form).
    pub(crate) fn units_value(&mut self, units: &[u16]) -> Value {
        let mut out: Vec<u8> = Vec::with_capacity(units.len() * 3);
        push_units(&mut out, units);
        Value::heap(self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out)))
    }

    /// One %RegExpStringIterator%.next step for the iterator at `it_idx`:
    /// `Some((value, done))` when it IS a lazy regexp-string iterator (else
    /// `None` — not one). Runs ONE RegExpExec (via the abstract protocol,
    /// honouring a user `exec`). A null result, or the single match of a
    /// non-global regex, latches done; a global empty match advances
    /// lastIndex (spec AdvanceStringIndex: +1 unit, +2 over an astral
    /// surrogate pair when the iterator's fullUnicode bit is set) so the
    /// next step makes progress. Shared by the ITER_NEXT native (which wraps
    /// the pair in a `{value, done}` object) and the `IterNext` for-of fast
    /// path (which consumes the pair directly — the result object an
    /// intrinsic `next` would build is engine-internal and its `done`/`value`
    /// Gets are unobservable).
    pub(crate) fn regexp_string_iter_step(
        &mut self,
        it_idx: u32,
    ) -> Option<Result<(Value, bool), Thrown>> {
        let &RegexpIterRec {
            matcher: regexp,
            subject: string,
            subj_units,
            fbits,
            done,
            ..
        } = self.regexp_string_iters.get(&it_idx)?;
        if fbits & ITFB_FUSED != 0 && !done && matchall_step_enabled() {
            if let Some(r) =
                self.regexp_string_iter_step_fused(it_idx, regexp, string, fbits, subj_units)
            {
                return Some(r);
            }
        }
        Some(self.regexp_string_iter_step_inner(it_idx, regexp, string, fbits, done))
    }

    /// Return one live own data method from the pristine
    /// %RegExpStringIteratorPrototype%, checking the iterator's exact embedded
    /// prototype and lack of an own-property overlay. The returned Value is
    /// read live, so a configurable method replacement/accessor is never
    /// hidden behind a cached version.
    pub(crate) fn regexp_string_iter_intrinsic_method(
        &self,
        it: Value,
        name: &str,
        native: u16,
    ) -> Option<Value> {
        if !it.is_heap() {
            return None;
        }
        let it_idx = it.heap_index();
        if !self.regexp_string_iters.contains_key(&it_idx)
            || (!self.arr_props.is_empty() && self.arr_props.contains_key(&it_idx))
        {
            return None;
        }
        let proto = match self.heap.get(it_idx) {
            HeapObj::Iterator { proto, .. } if *proto == self.regexp_string_iter_proto => *proto,
            _ => return None,
        };
        let p = match self.heap.get(proto) {
            HeapObj::Object(p) => p,
            _ => return None,
        };
        let slot = p.pos(name)?;
        if p.attr_at(slot).accessor {
            return None;
        }
        let v = p.val_at(slot);
        (v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Native(n) if *n == native))
            .then_some(v)
    }

    /// Pure guard for the source `GetIterator` at the exact scalar outer-loop
    /// IP. A miss has executed no getter/call and therefore resumes that same
    /// bytecode; a hit is identity because the live @@iterator is ITER_SELF.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn regexp_scalar_get_iterator_identity(&self, it: Value) -> Option<Value> {
        let rec = it
            .is_heap()
            .then(|| self.regexp_string_iters.get(&it.heap_index()))
            .flatten()?;
        if rec.fbits & ITFB_FUSED == 0 || rec.scalar_pending.is_some() {
            return None;
        }
        self.regexp_string_iter_intrinsic_method(it, "@@iterator", crate::vm::native::ITER_SELF)?;
        Some(it)
    }

    /// Pure guard for the following `IterPrime` Get(it,"next"). It returns
    /// the actual live data-property Value only when it is the pristine
    /// ITER_NEXT native; a replacement/accessor declines before observation.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn regexp_scalar_iter_prime(&self, it: Value) -> Option<Value> {
        let rec = it
            .is_heap()
            .then(|| self.regexp_string_iters.get(&it.heap_index()))
            .flatten()?;
        if rec.fbits & ITFB_FUSED == 0 || rec.scalar_pending.is_some() {
            return None;
        }
        self.regexp_string_iter_intrinsic_method(it, "next", crate::vm::native::ITER_NEXT)
    }

    /// Result-array-allocation-free matchAll step for the exact scalar outer
    /// region (Annex-B statics still publish their required strings).
    /// Every eligibility/protocol check precedes lastIndex/statics/pending/done
    /// mutation. A refill only installs a pure integer scan memo and is safe to
    /// retain across a decline.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn regexp_string_iter_step_scalar(&mut self, it_idx: u32) -> RegexpScalarStep {
        if !rx_scalar_matchall_enabled() {
            return RegexpScalarStep::Decline;
        }
        let (regexp, string, subj_units, fbits, done) = match self.regexp_string_iters.get(&it_idx)
        {
            Some(r) => (r.matcher, r.subject, r.subj_units, r.fbits, r.done),
            None => return RegexpScalarStep::Decline,
        };
        if done
            || fbits & (ITFB_GLOBAL | ITFB_FUSED) != (ITFB_GLOBAL | ITFB_FUSED)
            || fbits & (ITFB_UNICODE | ITFB_STICKY | ITFB_INDICES) != 0
            || subj_units >= u32::MAX as usize
            || !self.matchall_fast_from_slots()
        {
            return RegexpScalarStep::Decline;
        }
        let s_idx = if string.is_heap() {
            string.heap_index()
        } else {
            return RegexpScalarStep::Decline;
        };
        if !matches!(self.heap.get(s_idx), HeapObj::Str(js) if js.is_ascii()) {
            return RegexpScalarStep::Decline;
        }
        match self.heap.get(regexp) {
            HeapObj::RegExp { regex, .. } if !regex.has_named_groups() => {}
            _ => return RegexpScalarStep::Decline,
        }
        let li_v = match self.heap.get(regexp) {
            HeapObj::RegExp { last_index, .. } if last_index.is_number() => *last_index,
            _ => return RegexpScalarStep::Decline,
        };
        let li = {
            let d = li_v.as_f64().trunc();
            let d = if d.is_nan() { 0.0 } else { d };
            d.max(0.0).min(((1u64 << 53) - 1) as f64) as usize
        };
        if li > u32::MAX as usize {
            return RegexpScalarStep::Decline;
        }

        loop {
            let mut pending = None;
            let mut exhausted = false;
            if let Some(b) = self.matchall_batches.get_mut(&it_idx) {
                if b.expected_li as usize == li {
                    let stride = 2 + 2 * b.ncaps as usize;
                    let next = b.next as usize;
                    if next < b.flat.len() / stride {
                        if b.ncaps > 4 {
                            return RegexpScalarStep::Decline;
                        }
                        let base = next * stride;
                        let mut caps = [u32::MAX; 8];
                        for g in 0..b.ncaps as usize {
                            caps[2 * g] = b.flat[base + 2 + 2 * g];
                            caps[2 * g + 1] = b.flat[base + 3 + 2 * g];
                        }
                        let p = RegexpScalarMatch {
                            mstart: b.flat[base],
                            mend: b.flat[base + 1],
                            ncaps: b.ncaps as u8,
                            caps,
                        };
                        b.next += 1;
                        b.expected_li = p.mend + (p.mstart == p.mend) as u32;
                        pending = Some(p);
                    } else if b.exhausted {
                        exhausted = true;
                    }
                }
            }

            if let Some(p) = pending {
                // All guards have committed. Preserve the ordinary publish
                // order: lastIndex first, then Annex-B statics, then the
                // internally pending result; empty-match advancement is last.
                debug_assert!(self.arr_props.get(&regexp).is_none());
                if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(regexp) {
                    *last_index = Value::num(p.mend as f64);
                }
                let mut caps = std::mem::take(&mut self.matchall_caps_scratch);
                debug_assert!(caps.is_empty());
                caps.reserve(p.ncaps as usize);
                for g in 0..p.ncaps as usize {
                    let (s, e) = (p.caps[2 * g], p.caps[2 * g + 1]);
                    caps.push((s != u32::MAX).then(|| s as usize..e as usize));
                }
                let m = regress::Match::from_scan_parts(p.mstart as usize..p.mend as usize, caps);
                let mka = |vm: &mut Self, r: std::ops::Range<usize>| -> Value {
                    vm.ascii_slice_value(s_idx, r)
                };
                self.regexp_record_statics(
                    &m,
                    string,
                    s_idx,
                    p.mstart as usize,
                    p.mend as usize,
                    subj_units,
                    true,
                    &mka,
                );
                let mut caps = m.captures;
                caps.clear();
                self.matchall_caps_scratch = caps;
                let replaced = self
                    .regexp_string_iters
                    .get_mut(&it_idx)
                    .expect("active scalar matchAll iterator must remain registered")
                    .scalar_pending
                    .replace(p)
                    .is_some();
                if replaced {
                    rxstats::count_scalar_elided();
                }
                if p.mstart == p.mend {
                    self.set_regexp_last_index(regexp, p.mend as usize + 1);
                }
                rxstats::count_step_fused();
                rxstats::count_scalar_success();
                return RegexpScalarStep::Success;
            }

            if exhausted {
                if let Some(dead) = self.matchall_batches.remove(&it_idx) {
                    let mut flat = dead.flat;
                    flat.clear();
                    self.matchall_flat_scratch = flat;
                }
                debug_assert!(self.arr_props.get(&regexp).is_none());
                if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(regexp) {
                    *last_index = Value::int(0);
                }
                if let Some(r) = self.regexp_string_iters.get_mut(&it_idx) {
                    r.done = true;
                }
                rxstats::count_step_fused();
                return RegexpScalarStep::Done;
            }

            self.matchall_batches.remove(&it_idx);
            if !self.matchall_batch_refill(it_idx, regexp, s_idx, subj_units, li) {
                return RegexpScalarStep::Decline;
            }
        }
    }

    /// Apply the exact unary-`+` consumer to capture `capture` (1-based)
    /// directly from the pending subject range. No capture string/Array exists.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn regexp_scalar_capture_number(&self, it_idx: u32, capture: u32) -> Option<Value> {
        let r = self.regexp_string_iters.get(&it_idx)?;
        let p = r.scalar_pending?;
        if capture == 0 {
            return None;
        }
        if capture > p.ncaps as u32 {
            // Outside the result Array's own dense length. Array.prototype may
            // supply this index (including through an observable getter), so
            // only the interpreter can perform the Get.
            return None;
        }
        let g = capture as usize - 1;
        let (start, end) = (p.caps[2 * g], p.caps[2 * g + 1]);
        if start == u32::MAX {
            rxstats::count_scalar_capture_num();
            return Some(Value::num(f64::NAN));
        }
        if end < start || end as usize > r.subj_units {
            return None;
        }
        let s = match self.heap.get(r.subject.heap_index()) {
            HeapObj::Str(js) if js.is_ascii() => js.as_str_wf(),
            _ => return None,
        };
        let piece = s.get(start as usize..end as usize)?;
        rxstats::count_scalar_capture_num();
        Some(Value::num(super::coerce::string_to_number(piece)))
    }

    /// Consume the remaining OUTER iterations of the exact scalar matchAll
    /// region in one guarded pass. It eliminates every later
    /// `String#matchAll` lookup, species clone and iterator allocation.
    ///
    /// The complete remaining dense Array is preflighted before the first
    /// regex attempt. Matching itself mutates no JS-visible state; count/sum,
    /// the outer index, Annex-B statics and the final pending `km` are published
    /// only after every scan succeeds. Therefore every `None` is a pure prefix
    /// and the established scalar step can replay the current IterNext.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn regexp_dense_array_matchall_reduce(
        &mut self,
        it_idx: u32,
        callee: Value,
        result_global: u32,
        count_global: u32,
        sum_global: u32,
        capture: u32,
        i_global: u32,
        n_global: u32,
        lines_global: u32,
        re_global: u32,
    ) -> Option<RegexpScalarStep> {
        if !rx_dense_array_matchall_reduce_enabled()
            || !self.captured_regexp_method_is_intrinsic(
                crate::bytecode::RegExpMethod::MatchAll,
                callee,
            )
        {
            return None;
        }
        let globals = [
            result_global,
            count_global,
            sum_global,
            i_global,
            n_global,
            lines_global,
            re_global,
        ];
        if globals.iter().any(|&g| g as usize >= self.globals.len()) {
            return None;
        }
        // Admission already proves this, but it is the soundness boundary for
        // deferred publication; keep the runtime helper independently total.
        for (at, global) in globals.iter().enumerate() {
            if globals[at + 1..].contains(global) {
                return None;
            }
        }
        let (i_value, n_value, count_value, sum_value) = (
            self.globals[i_global as usize],
            self.globals[n_global as usize],
            self.globals[count_global as usize],
            self.globals[sum_global as usize],
        );
        if !i_value.is_int() || !n_value.is_int() || !count_value.is_int() || !sum_value.is_int() {
            return None;
        }
        let (i_raw, n_raw) = (i_value.as_int(), n_value.as_int());
        if i_raw < 0 || n_raw <= i_raw {
            return None;
        }
        let (i, n) = (i_raw as usize, n_raw as usize);

        let lines = self.globals[lines_global as usize];
        let source = self.globals[re_global as usize];
        if !lines.is_heap() || !source.is_heap() {
            return None;
        }
        let (lines_idx, source_idx) = (lines.heap_index(), source.heap_index());

        // These iterator facts were established by the just-completed direct
        // matchAll/GetIterator/IterPrime guards. Re-read them so this helper is
        // safe even if called from a future emitter with weaker assumptions.
        let (matcher_idx, current_subject, current_units, fbits, done, has_pending) =
            self.regexp_string_iters.get(&it_idx).map(|r| {
                (
                    r.matcher,
                    r.subject,
                    r.subj_units,
                    r.fbits,
                    r.done,
                    r.scalar_pending.is_some(),
                )
            })?;
        if done
            || has_pending
            || fbits & (ITFB_GLOBAL | ITFB_FUSED) != (ITFB_GLOBAL | ITFB_FUSED)
            || fbits & (ITFB_UNICODE | ITFB_STICKY | ITFB_INDICES) != 0
            || !current_subject.is_heap()
            || !self.matchall_fast_from_slots()
        {
            return None;
        }

        // The first iteration's observable Get may have been an accessor that
        // returned the intrinsic and then changed/deleted itself. The captured
        // identity proves the call already made; this live exact-data proof is
        // separately required before the reducer skips every later Get.
        if !self
            .captured_regexp_method_get_intrinsic(
                crate::bytecode::RegExpMethod::MatchAll,
                current_subject,
            )
            .is_some_and(|live| live.bits() == callee.bits())
        {
            return None;
        }

        // Re-prove the SOURCE instance, not merely the engine-private clone:
        // skipping later matchAll calls also skips their instance-specific own
        // property and prototype checks.
        if !self.regexp_matchall_fast_ok_cached(source_idx) {
            return None;
        }
        self.ensure_regexp_ascii_twin(source_idx);
        let (source_regex, scan_regex, source_flags, start) = match self.heap.get(source_idx) {
            HeapObj::RegExp {
                regex,
                flags,
                last_index,
                ascii_twin,
                ..
            } if last_index.is_number()
                && flags.contains('g')
                && !flags
                    .bytes()
                    .any(|b| matches!(b, b'd' | b'u' | b'v' | b'y')) =>
            {
                let d = last_index.as_f64().trunc();
                let d = if d.is_nan() { 0.0 } else { d };
                let start = d.max(0.0).min(((1u64 << 53) - 1) as f64) as usize;
                let scan = match ascii_twin {
                    Some(Some(twin)) => twin.clone(),
                    Some(None) => regex.clone(),
                    None => return None,
                };
                (regex.clone(), scan, flags.clone(), start)
            }
            _ => return None,
        };
        if start > u32::MAX as usize
            || capture == 0
            || capture as usize > scan_regex.capture_count()
            || scan_regex.capture_count() > 4
            || scan_regex.has_named_groups()
        {
            return None;
        }
        // The current iterator must really be the pristine clone of `source`
        // and still sit at its initial copied lastIndex (no step has committed).
        let matcher_ok = match self.heap.get(matcher_idx) {
            HeapObj::RegExp {
                regex,
                flags,
                last_index,
                ..
            } if last_index.is_number() => {
                let d = last_index.as_f64().trunc();
                let d = if d.is_nan() { 0.0 } else { d };
                std::sync::Arc::ptr_eq(regex, &source_regex)
                    && flags.as_ref() == source_flags.as_ref()
                    && d.max(0.0).min(((1u64 << 53) - 1) as f64) as usize == start
            }
            _ => false,
        };
        if !matcher_ok {
            return None;
        }

        // Full preflight: every `lines[k]` is an own dense element and already
        // a flat ASCII primitive string. No flattening, getter, Proxy trap or
        // prototype lookup can occur after this point. Strings and Array dense
        // elements cannot change because the admitted body contains no call or
        // write; the helper itself does not run user code.
        if self.array_elements_overlaid(lines_idx) || self.array_js_len.contains_key(&lines_idx) {
            return None;
        }
        let mut maximum_matches = 0usize;
        match self.heap.get(lines_idx) {
            HeapObj::Array(items)
                if n <= items.len() && items[i].bits() == current_subject.bits() =>
            {
                for value in &items[i..n] {
                    if value.is_hole() || !value.is_heap() {
                        return None;
                    }
                    match self.heap.get(value.heap_index()) {
                        HeapObj::Str(js)
                            if js.is_ascii() && js.as_bytes().len() < u32::MAX as usize =>
                        {
                            maximum_matches =
                                maximum_matches.saturating_add(js.as_bytes().len() + 1);
                        }
                        _ => return None,
                    }
                }
            }
            _ => return None,
        }
        if !matches!(
            self.heap.get(current_subject.heap_index()),
            HeapObj::Str(js) if js.is_ascii() && js.as_bytes().len() == current_units
        ) {
            return None;
        }
        let count_headroom = i32::MAX as i64 - count_value.as_int() as i64;
        if maximum_matches > count_headroom as usize {
            return None;
        }

        let mut count = count_value.as_int();
        let mut sum = sum_value.as_int();
        let mut matches = 0u64;
        let mut final_match: Option<(Value, usize, RegexpScalarMatch)> = None;
        for pos in i..n {
            let subject = match self.heap.get(lines_idx) {
                HeapObj::Array(items) => items[pos],
                _ => unreachable!("dense Array preflight is immutable inside the helper"),
            };
            let subject_idx = subject.heap_index();
            let text = match self.heap.get(subject_idx) {
                HeapObj::Str(js) => js.as_str_wf(),
                _ => unreachable!("flat-string preflight is immutable inside the helper"),
            };
            let exhausted = scan_regex.scan_ascii(text, start, usize::MAX, &mut |range, caps| {
                debug_assert!(caps.len() <= 4);
                let number = match caps.get(capture as usize - 1).and_then(|r| r.as_ref()) {
                    Some(r) => super::coerce::string_to_number(&text[r.clone()]),
                    None => f64::NAN,
                };
                count = count.checked_add(1).expect("preflighted count headroom");
                sum = crate::vm::helpers_num2::to_int32(sum as f64 + number);
                matches += 1;
                let mut packed = RegexpScalarMatch {
                    mstart: range.start as u32,
                    mend: range.end as u32,
                    ncaps: caps.len() as u8,
                    caps: [u32::MAX; 8],
                };
                for (group, cap) in caps.iter().enumerate() {
                    if let Some(cap) = cap {
                        packed.caps[2 * group] = cap.start as u32;
                        packed.caps[2 * group + 1] = cap.end as u32;
                    }
                }
                final_match = Some((subject, text.len(), packed));
            });
            debug_assert!(
                exhausted,
                "an unbounded drained scan must exhaust its subject"
            );
        }

        // Commit the pure reduction. Returning `done=true` sends generated
        // code to the ordinary outer tail, whose unchanged `i++` turns n-1
        // into n before the loop condition exits.
        self.globals[count_global as usize] = Value::int(count);
        self.globals[sum_global as usize] = Value::int(sum);
        self.globals[i_global as usize] = Value::int(n_raw - 1);

        if let Some((subject, subject_units, packed)) = final_match {
            // Only the final Annex-B record is observable: the exact admitted
            // body has no call, property access or static read between matches.
            let mut caps = std::mem::take(&mut self.matchall_caps_scratch);
            debug_assert!(caps.is_empty());
            caps.reserve(packed.ncaps as usize);
            for group in 0..packed.ncaps as usize {
                let (s, e) = (packed.caps[2 * group], packed.caps[2 * group + 1]);
                caps.push((s != u32::MAX).then(|| s as usize..e as usize));
            }
            let matched =
                regress::Match::from_scan_parts(packed.mstart as usize..packed.mend as usize, caps);
            let subject_idx = subject.heap_index();
            let mk = |vm: &mut Self, r: std::ops::Range<usize>| -> Value {
                vm.ascii_slice_value(subject_idx, r)
            };
            self.regexp_record_statics(
                &matched,
                subject,
                subject_idx,
                packed.mstart as usize,
                packed.mend as usize,
                subject_units,
                true,
                &mk,
            );
            let mut caps = matched.captures;
            caps.clear();
            self.matchall_caps_scratch = caps;

            let record = self
                .regexp_string_iters
                .get_mut(&it_idx)
                .expect("preflighted matchAll iterator must survive an allocation-free reduction");
            record.subject = subject;
            record.subj_units = subject_units;
            debug_assert!(record.scalar_pending.is_none());
            record.scalar_pending = Some(packed);
            record.done = true;
        } else if let Some(record) = self.regexp_string_iters.get_mut(&it_idx) {
            record.done = true;
        }
        // MatchAll's engine-private clone is exhausted and would reset to zero.
        if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(matcher_idx) {
            *last_index = Value::int(0);
        }
        if let Some(dead) = self.matchall_batches.remove(&it_idx) {
            let mut flat = dead.flat;
            flat.clear();
            self.matchall_flat_scratch = flat;
        }
        rxstats::count_scalar_array_reduce(matches, (n - i) as u64);
        Some(RegexpScalarStep::Done)
    }

    /// Materialise the one pending result into the exact skipped global
    /// binding. This intentionally does NOT record Annex-B statics again: the
    /// success step already published them once in source order.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn regexp_scalar_flush(
        &mut self,
        it_idx: u32,
        global: u32,
        slow_reentry: bool,
    ) -> bool {
        if global as usize >= self.globals.len() {
            return false;
        }
        let (p, string, subj_units) = match self.regexp_string_iters.get_mut(&it_idx) {
            Some(r) => match r.scalar_pending.take() {
                Some(p) => (p, r.subject, r.subj_units),
                None => return false,
            },
            None => return false,
        };
        let s_idx = string.heap_index();
        let mut caps = std::mem::take(&mut self.matchall_caps_scratch);
        debug_assert!(caps.is_empty());
        caps.reserve(p.ncaps as usize);
        for g in 0..p.ncaps as usize {
            let (s, e) = (p.caps[2 * g], p.caps[2 * g + 1]);
            caps.push((s != u32::MAX).then(|| s as usize..e as usize));
        }
        let m = regress::Match::from_scan_parts(p.mstart as usize..p.mend as usize, caps);
        let mka =
            |vm: &mut Self, r: std::ops::Range<usize>| -> Value { vm.ascii_slice_value(s_idx, r) };
        let _gc = self.gc_lock_guard();
        let out =
            self.regexp_build_result(&m, string, p.mstart as usize, p.mend as usize, false, &mka);
        let mut caps = m.captures;
        caps.clear();
        self.matchall_caps_scratch = caps;
        self.globals[global as usize] = out;
        rxstats::count_scalar_materialized(slow_reentry);
        let _ = subj_units; // retained in the record as the range bound/root proof
        true
    }

    /// Allocation-free `RegExp.prototype.exec` success for the exact MEM
    /// scalar region.  Every guard and the complete one-match scan precede
    /// Annex-B/pending mutation, so `Decline` is a pure prefix and the original
    /// RegExpMethod may be replayed exactly once by the interpreter.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn regexp_scalar_exec_step(
        &mut self,
        callee: Value,
        recv: Value,
        input: Value,
    ) -> RegexpScalarExecStep {
        if !rx_scalar_exec_enabled()
            || !self
                .captured_regexp_method_is_intrinsic(crate::bytecode::RegExpMethod::Exec, callee)
            || !recv.is_heap()
            || !input.is_heap()
        {
            return RegexpScalarExecStep::Decline;
        }
        let re_idx = recv.heap_index();
        if !self.regexp_method_is_intrinsic(re_idx, "exec", native::REGEXP_EXEC) {
            return RegexpScalarExecStep::Decline;
        }
        let (flags_ok, last_index_ok, matcher_ok, twin_ready) = match self.heap.get(re_idx) {
            HeapObj::RegExp {
                regex,
                flags,
                last_index,
                ascii_twin,
                ..
            } => (
                !flags
                    .bytes()
                    .any(|b| matches!(b, b'g' | b'y' | b'u' | b'v' | b'd')),
                last_index.is_number(),
                !regex.has_named_groups(),
                ascii_twin.is_some(),
            ),
            _ => return RegexpScalarExecStep::Decline,
        };
        if !flags_ok || !last_index_ok || !matcher_ok || !twin_ready {
            return RegexpScalarExecStep::Decline;
        }
        let s_idx = input.heap_index();
        let subj_units = match self.heap.get(s_idx) {
            HeapObj::Str(js) if js.is_ascii() && js.as_bytes().len() < u32::MAX as usize => {
                js.as_bytes().len()
            }
            _ => return RegexpScalarExecStep::Decline,
        };

        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::RegexExec);
        // The callback's ranges and the previous pending subject are held in
        // VM/Rust state until commit.  Collection already ran at the FFI safe
        // point; keep it suspended for this pure scan/publication sequence.
        let _gc = self.gc_lock_guard();
        let mut pending: Option<RegexpScalarMatch> = None;
        let mut malformed = false;
        {
            let subj = match self.heap.get(s_idx) {
                HeapObj::Str(js) => js.as_str_wf(),
                _ => return RegexpScalarExecStep::Decline,
            };
            let regex: &regress::Regex = match self.heap.get(re_idx) {
                HeapObj::RegExp {
                    ascii_twin: Some(Some(twin)),
                    ..
                } => twin,
                HeapObj::RegExp {
                    ascii_twin: Some(None),
                    regex,
                    ..
                } => regex,
                _ => return RegexpScalarExecStep::Decline,
            };
            let _ = regex.scan_ascii(subj, 0, 1, &mut |range, captures| {
                if captures.len() != 4 || range.start > range.end || range.end > subj_units {
                    malformed = true;
                    return;
                }
                let mut caps = [u32::MAX; 8];
                for (g, cap) in captures.iter().enumerate() {
                    if let Some(cap) = cap {
                        if cap.start > cap.end || cap.end > subj_units {
                            malformed = true;
                            return;
                        }
                        caps[2 * g] = cap.start as u32;
                        caps[2 * g + 1] = cap.end as u32;
                    }
                }
                pending = Some(RegexpScalarMatch {
                    mstart: range.start as u32,
                    mend: range.end as u32,
                    ncaps: 4,
                    caps,
                });
            });
        }
        if malformed {
            return RegexpScalarExecStep::Decline;
        }

        // From here onward this invocation is committed and must not be
        // replayed. Preserve the existing direct-call/builtin mechanism census.
        super::builtins::builtin_stats_count(self, recv, "exec");
        rxstats::count_call_direct_hit(false, true);
        let Some(p) = pending else {
            if self.regexp_scalar_exec_pending.take().is_some() {
                rxstats::count_scalar_exec_elided();
            }
            rxstats::count_scalar_exec_miss();
            return RegexpScalarExecStep::Miss;
        };

        // Unary plus over each own primitive capture is pure.  Compute all four
        // before publishing anything so an impossible range/parser failure can
        // still decline without visible state.
        let nums = {
            let subj = match self.heap.get(s_idx) {
                HeapObj::Str(js) => js.as_str_wf(),
                _ => return RegexpScalarExecStep::Decline,
            };
            let mut out = [Value::UNDEFINED; 4];
            for g in 0..4 {
                let (start, end) = (p.caps[2 * g], p.caps[2 * g + 1]);
                out[g] = if start == u32::MAX {
                    Value::num(f64::NAN)
                } else {
                    let Some(piece) = subj.get(start as usize..end as usize) else {
                        return RegexpScalarExecStep::Decline;
                    };
                    Value::num(super::coerce::string_to_number(piece))
                };
            }
            out
        };

        // Publish Annex-B statics exactly once, in RegExpBuiltinExec's source
        // position. Result construction and the global store remain pending.
        let mut caps = std::mem::take(&mut self.matchall_caps_scratch);
        debug_assert!(caps.is_empty());
        caps.reserve(4);
        for g in 0..4 {
            let (start, end) = (p.caps[2 * g], p.caps[2 * g + 1]);
            caps.push((start != u32::MAX).then(|| start as usize..end as usize));
        }
        let m = regress::Match::from_scan_parts(p.mstart as usize..p.mend as usize, caps);
        let mka =
            |vm: &mut Self, r: std::ops::Range<usize>| -> Value { vm.ascii_slice_value(s_idx, r) };
        self.regexp_record_statics(
            &m,
            input,
            s_idx,
            p.mstart as usize,
            p.mend as usize,
            subj_units,
            true,
            &mka,
        );
        let mut caps = m.captures;
        caps.clear();
        self.matchall_caps_scratch = caps;
        let replaced = self
            .regexp_scalar_exec_pending
            .replace(RegexpScalarExecPending {
                subject: input,
                subj_units: subj_units as u32,
                matched: p,
            })
            .is_some();
        if replaced {
            rxstats::count_scalar_exec_elided();
        }
        rxstats::count_scalar_exec_capture_nums(4);
        rxstats::count_scalar_exec_success();
        RegexpScalarExecStep::Success(nums)
    }

    /// Materialize the direct-exec result skipped by the exact region.  Annex-B
    /// statics were already published by the success helper and are not touched
    /// again. `reason == 1` denotes an observable slow-Add/re-entry exit.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn regexp_scalar_exec_flush(&mut self, global: u32, reason: u32) -> bool {
        if global as usize >= self.globals.len() {
            return false;
        }
        // Taking the record removes its GC root.  Lock first so allocations in
        // the canonical result builder cannot collect the local-only subject.
        let _gc = self.gc_lock_guard();
        let Some(pending) = self.regexp_scalar_exec_pending.take() else {
            return false;
        };
        let p = pending.matched;
        let s_idx = pending.subject.heap_index();
        let mut caps = std::mem::take(&mut self.matchall_caps_scratch);
        debug_assert!(caps.is_empty());
        caps.reserve(p.ncaps as usize);
        for g in 0..p.ncaps as usize {
            let (start, end) = (p.caps[2 * g], p.caps[2 * g + 1]);
            caps.push((start != u32::MAX).then(|| start as usize..end as usize));
        }
        let m = regress::Match::from_scan_parts(p.mstart as usize..p.mend as usize, caps);
        let mka =
            |vm: &mut Self, r: std::ops::Range<usize>| -> Value { vm.ascii_slice_value(s_idx, r) };
        let out = self.regexp_build_result(
            &m,
            pending.subject,
            p.mstart as usize,
            p.mend as usize,
            false,
            &mka,
        );
        let mut caps = m.captures;
        caps.clear();
        self.matchall_caps_scratch = caps;
        self.globals[global as usize] = out;
        debug_assert!(p.mend <= pending.subj_units);
        rxstats::count_scalar_exec_materialized(reason == 1);
        true
    }

    /// The fused pristine matchAll STEP (B118). Reaching here requires the
    /// `ITFB_FUSED` bit, which only the pristine-clone creation arm sets: the
    /// matcher is an ENGINE-INTERNAL clone no user reference exists to (so
    /// its own shape, prototype link, flags and `lastIndex` writability were
    /// proven once, at creation, and cannot change), and the subject is a
    /// flat-ASCII string (immutable, so the bit stays true).
    ///
    /// What CAN change mid-iteration is the shared %RegExp.prototype% — a
    /// replaced `exec` must be honoured per STEP. That is exactly the memo
    /// `matchall_fast_from_slots` version-guards (its `exec` pin re-reads the
    /// slot's value identity every call, which is what catches the
    /// no-version-bump in-place `RegExp.prototype.exec = f` write — B67).
    /// Any mismatch returns `None` and the caller runs the full observable
    /// step; a stale memo is refreshed by the next `matchAll()` call, never
    /// here (a per-step re-resolve would put the nine-probe gate back on the
    /// hot path for permanently-polluted programs).
    ///
    /// With the guards holding, the step is: one RegExpBuiltinExec with the
    /// flag bits pre-decoded from the iterator record (no per-step
    /// `flags.contains` scans, no exec-protocol re-derivation), the dense
    /// element-0 empty-match probe, and the +1 AdvanceStringIndex an ASCII
    /// subject admits (no surrogate pairs to skip).
    fn regexp_string_iter_step_fused(
        &mut self,
        it_idx: u32,
        regexp: u32,
        string: Value,
        fbits: u8,
        subj_units: usize,
    ) -> Option<Result<(Value, bool), Thrown>> {
        if !self.matchall_fast_from_slots() {
            rxstats::count_step_full();
            return None;
        }
        // The matcher's `lastIndex` only ever holds the numbers this path and
        // the builtin exec write, but the bit costs nothing to re-check and
        // turns "engine invariant" into "guard". The Value is EXTRACTED here
        // (B124): the slim entry takes it as a parameter instead of paying a
        // second heap.get + ToInteger for the identical slot.
        let li_v = match self.heap.get(regexp) {
            HeapObj::RegExp { last_index, .. } if last_index.is_number() => *last_index,
            _ => {
                rxstats::count_step_full();
                return None;
            }
        };
        if slim_exec_enabled() {
            // W12 batch: serve the step from a drained scan when one is live
            // (or drainable) for this iterator — one host-side scan per
            // subject instead of one per step. `None` = not batchable; the
            // one-shot slim call below runs unchanged.
            if matchall_batch_enabled() {
                if let Some(r) = self.regexp_string_iter_step_batched(
                    it_idx, regexp, string, fbits, subj_units, li_v,
                ) {
                    return Some(r);
                }
            }
            // B124 slim entry: one infallible call returns the result array
            // plus the empty-match fact the probe below re-derived from the
            // just-built array's element 0.
            let (r, empty_end) =
                self.regexp_exec_fused_slim(regexp, string, fbits, subj_units, li_v);
            rxstats::count_step_fused();
            if r == Value::NULL {
                if let Some(e) = self.regexp_string_iters.get_mut(&it_idx) {
                    e.done = true;
                }
                return Some(Ok((Value::UNDEFINED, true)));
            }
            // The old path probed the just-built array's element 0 for
            // emptiness; the fold to the returned flag is observation-free
            // ONLY because element 0 is exactly subject[mstart..mend] — the
            // assertion anchors that equivalence.
            debug_assert_eq!(
                empty_end.is_some(),
                match self.heap.get(r.heap_index()) {
                    HeapObj::Array(items) => matches!(
                        items.first(),
                        Some(v) if v.is_heap() && self.heap.str_units(v.heap_index()) == Some(0)
                    ),
                    _ => false,
                },
                "slim empty-match flag must agree with the element-0 probe"
            );
            if let Some(end) = empty_end {
                // `lastIndex` was just written by the exec (== the match
                // end); ASCII subject ⇒ the advance is exactly +1.
                self.set_regexp_last_index(regexp, end + 1);
            }
            return Some(Ok((r, false)));
        }
        // ZIPP_NO_SLIM_EXEC=1: the pre-B124 step, byte-for-byte.
        let r = match self.regexp_exec_impl_prebits(regexp, string, true, Some(fbits)) {
            Ok(r) => r,
            Err(t) => return Some(Err(t)),
        };
        rxstats::count_step_fused();
        if r == Value::NULL {
            if let Some(e) = self.regexp_string_iters.get_mut(&it_idx) {
                e.done = true;
            }
            return Some(Ok((Value::UNDEFINED, true)));
        }
        // Empty match ⇒ AdvanceStringIndex. Element 0 is the string the
        // builtin exec just built (pristine builder, dense store, no user
        // code ran since) — read it directly.
        let empty = match self.heap.get(r.heap_index()) {
            HeapObj::Array(items) => matches!(
                items.first(),
                Some(v) if v.is_heap() && self.heap.str_units(v.heap_index()) == Some(0)
            ),
            _ => false,
        };
        if empty {
            // `lastIndex` was just written by the exec above (a number, ==
            // the match end); ASCII subject ⇒ the advance is exactly +1.
            let cur = match self.heap.get(regexp) {
                HeapObj::RegExp { last_index, .. } => last_index.as_f64().max(0.0) as usize,
                _ => 0,
            };
            self.set_regexp_last_index(regexp, cur + 1);
        }
        Some(Ok((r, false)))
    }

    /// The fused matchAll step served from a DRAINED batch: one host-side
    /// scan per subject serves up to [`MATCHALL_BATCH_CAP`] steps, hoisting
    /// the per-step executor construction and (with rx-jit) the scan-session
    /// setup out of the step. The batch is a PURE memo, so soundness reduces
    /// to guarding the resume position: the caller already re-proved the
    /// per-step protocol (slot memo + numeric `lastIndex`), and the
    /// `expected_li` check catches every remaining divergence — a fallback
    /// round ran a user `exec` mid-iteration and moved the heap slot — by
    /// re-draining from the live position. Everything OBSERVABLE stays
    /// per-step (`fused_publish`): `RegExp.$1`/`lastMatch` read in the loop
    /// body see per-step values, and `lastIndex` is written per published
    /// step so a mid-iteration `RegExp.prototype.exec` swap resumes the full
    /// path from a coherent position.
    ///
    /// `None` = not batchable — sticky needs the per-step start filter, `/d`
    /// and named groups need publish machinery the triples don't carry, and
    /// an over-u32 subject doesn't fit the triple encoding (matching the
    /// statics-deferral bound) — the caller falls through to the one-shot
    /// slim exec unchanged.
    fn regexp_string_iter_step_batched(
        &mut self,
        it_idx: u32,
        regexp: u32,
        string: Value,
        fbits: u8,
        subj_units: usize,
        li_v: Value,
    ) -> Option<Result<(Value, bool), Thrown>> {
        if fbits & (ITFB_STICKY | ITFB_INDICES) != 0 || subj_units >= u32::MAX as usize {
            return None;
        }
        debug_assert!(li_v.is_number(), "the fused-step guard admits numbers only");
        debug_assert!(fbits & ITFB_GLOBAL != 0, "ITFB_FUSED implies g");
        // ToLength on the engine-written number — the slim exec's inline form.
        let li = {
            let d = li_v.as_f64().trunc();
            let d = if d.is_nan() { 0.0 } else { d };
            d.max(0.0).min(((1u64 << 53) - 1) as f64) as usize
        };
        if li > u32::MAX as usize {
            return None;
        }
        let s_idx = string.heap_index();
        // At most two passes: a refill installs a batch at exactly `li`, and
        // a capped drain always carries either a triple or the exhausted bit.
        loop {
            if let Some(b) = self.matchall_batches.get_mut(&it_idx) {
                if b.expected_li as usize == li {
                    let stride = 2 + 2 * b.ncaps as usize;
                    let next = b.next as usize;
                    if next < b.flat.len() / stride {
                        let base = next * stride;
                        let mstart = b.flat[base] as usize;
                        let mend = b.flat[base + 1] as usize;
                        let ncaps = b.ncaps as usize;
                        // The capture Vec cycles through the scratch slot
                        // (`fused_publish` returns it there cleared), so the
                        // steady-state publish re-mallocs nothing.
                        let mut caps = std::mem::take(&mut self.matchall_caps_scratch);
                        debug_assert!(caps.is_empty());
                        caps.reserve(ncaps);
                        for g in 0..ncaps {
                            let (s, e) = (b.flat[base + 2 + 2 * g], b.flat[base + 3 + 2 * g]);
                            caps.push((s != u32::MAX).then(|| s as usize..e as usize));
                        }
                        // Advance the memo BEFORE publishing (the publish half
                        // never reads the batch): the next triple was drained
                        // from the end, one past it for an empty match.
                        b.next += 1;
                        b.expected_li = mend as u32 + (mstart == mend) as u32;
                        rxstats::count_step_fused();
                        let r = self.fused_publish(
                            regexp, s_idx, string, subj_units, mstart, mend, caps, fbits,
                        );
                        if mstart == mend {
                            // `lastIndex` was just written by the publish (==
                            // the match end); ASCII subject ⇒ the advance is
                            // exactly +1.
                            self.set_regexp_last_index(regexp, mend + 1);
                        }
                        return Some(Ok((r, false)));
                    }
                    if b.exhausted {
                        rxstats::count_step_fused();
                        if let Some(dead) = self.matchall_batches.remove(&it_idx) {
                            // Hand the triple storage back to the scratch slot
                            // so the next drain re-mallocs nothing.
                            let mut flat = dead.flat;
                            flat.clear();
                            self.matchall_flat_scratch = flat;
                        }
                        // Set(R,"lastIndex",0,true) — the slim exec's no-match
                        // tail, direct form (same unreachability argument).
                        debug_assert!(self.arr_props.get(&regexp).is_none());
                        if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(regexp) {
                            *last_index = Value::int(0);
                        }
                        if let Some(e) = self.regexp_string_iters.get_mut(&it_idx) {
                            e.done = true;
                        }
                        return Some(Ok((Value::UNDEFINED, true)));
                    }
                    // A consumed batch with subject left: re-drain from the
                    // live position (== expected_li == li).
                }
                // else: resume-position divergence — drop the stale memo and
                // re-drain from the live position.
            }
            self.matchall_batches.remove(&it_idx);
            if !self.matchall_batch_refill(it_idx, regexp, s_idx, subj_units, li) {
                return None;
            }
        }
    }

    /// Drain up to [`MATCHALL_BATCH_CAP`] matches from `li` into a fresh
    /// batch record for `it_idx` — `false` when this iteration cannot be
    /// batched (named capture groups, or a non-RegExp matcher slot). The scan
    /// is `Regex::scan_ascii`: the identical attempt/advance sequence the
    /// one-shot steps would run, minus the per-step executor construction —
    /// RXSTATS attempt counts are invariant by construction.
    fn matchall_batch_refill(
        &mut self,
        it_idx: u32,
        regexp: u32,
        s_idx: u32,
        subj_units: usize,
        li: usize,
    ) -> bool {
        match self.heap.get(regexp) {
            HeapObj::RegExp { regex, .. } => {
                if regex.has_named_groups() {
                    return false;
                }
            }
            _ => return false,
        }
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::RegexExec);
        // Cold, at most once per matcher — the fused creation arm normally
        // pre-builds the SOURCE's twin, so the clone arrives here as `Some`;
        // this stays live for `ZIPP_NO_TWIN_AT_CREATE=1`.
        self.ensure_regexp_ascii_twin(regexp);
        // Cycles through the scratch slot (the done protocol returns a dead
        // batch's storage there cleared) — steady state re-mallocs nothing.
        let mut flat = std::mem::take(&mut self.matchall_flat_scratch);
        debug_assert!(flat.is_empty());
        let mut ncaps: u16 = 0;
        let exhausted = {
            let subj: &str = match self.heap.get(s_idx) {
                HeapObj::Str(js) => {
                    debug_assert!(js.is_ascii(), "ITFB_FUSED encodes a flat-ASCII subject");
                    debug_assert_eq!(
                        subj_units,
                        js.as_bytes().len(),
                        "cached units must equal the flat-ASCII byte length"
                    );
                    js.as_str_wf()
                }
                _ => "",
            };
            let re = match self.heap.get(regexp) {
                HeapObj::RegExp {
                    ascii_twin: Some(Some(twin)),
                    ..
                } => &**twin,
                // Twin compile failed once: the base program is byte-safe too.
                HeapObj::RegExp { regex, .. } => &**regex,
                _ => return false,
            };
            re.scan_ascii(subj, li, MATCHALL_BATCH_CAP, &mut |r, caps| {
                ncaps = caps.len() as u16;
                flat.push(r.start as u32);
                flat.push(r.end as u32);
                for c in caps {
                    match c {
                        Some(c) => {
                            flat.push(c.start as u32);
                            flat.push(c.end as u32);
                        }
                        None => {
                            flat.push(u32::MAX);
                            flat.push(u32::MAX);
                        }
                    }
                }
            })
        };
        debug_assert!(
            exhausted || !flat.is_empty(),
            "a capped drain always carries matches"
        );
        self.matchall_batches.insert(
            it_idx,
            MatchBatch {
                expected_li: li as u32,
                next: 0,
                ncaps,
                exhausted,
                flat,
            },
        );
        true
    }

    fn regexp_string_iter_step_inner(
        &mut self,
        it_idx: u32,
        regexp: u32,
        string: Value,
        fbits: u8,
        done: bool,
    ) -> Result<(Value, bool), Thrown> {
        let global = fbits & 1 != 0;
        let full_unicode = fbits & 2 != 0;
        let (value, ret_done, latch) = if done {
            (Value::UNDEFINED, true, true)
        } else {
            // Captured BEFORE the exec: `regexp_exec_fast_ok` proves the result
            // array is the one RegExpBuiltinExec builds, so element 0 can be read
            // from the dense store below. A user `exec` can return anything, and
            // could also install one between iterations, so it is re-checked
            // every step rather than cached on the iterator.
            let pristine_exec = matches!(self.heap.get(regexp), HeapObj::RegExp { .. })
                && self.regexp_exec_fast_ok(regexp);
            // `regexp_exec_abstract` opens by re-proving exactly
            // `pristine_exec` to pick the builtin — when it already holds,
            // call the builtin directly instead of proving it twice.
            let r = if pristine_exec {
                self.regexp_exec(regexp, string)?
            } else {
                self.regexp_exec_abstract(regexp, string)?
            };
            if r == Value::NULL {
                (Value::UNDEFINED, true, true)
            } else if !global {
                (r, false, true)
            } else {
                // Was the match EMPTY? Only then does lastIndex need advancing.
                //
                // The generic `get_index` path must still account for arguments
                // mapping, element overlays, holes, and a custom prototype. When
                // we built the array ourselves (real RegExp, pristine `exec`, so
                // no user code could have replaced element 0 with a getter), we
                // can read it directly from dense store; otherwise full Get/ToString.
                let empty = if pristine_exec {
                    match self.heap.get(r.heap_index()) {
                        HeapObj::Array(items) => {
                            let m0 = items.first().copied().unwrap_or(Value::UNDEFINED);
                            m0.is_heap() && self.heap.str_units(m0.heap_index()) == Some(0)
                        }
                        _ => false,
                    }
                } else {
                    let m0 = self.get_index(r, Value::int(0))?;
                    // ToString(Get(match,"0")) — identity for a string value;
                    // only a custom exec result can need coercion here.
                    let m0v = self.to_str_value(m0)?;
                    self.heap.str_units(m0v.heap_index()) == Some(0)
                };
                if empty {
                    let cur_v = self.get_prop(Value::heap(regexp), "lastIndex")?;
                    // ToLength(Get(R,"lastIndex")) — a throwing
                    // lastIndex.valueOf must propagate, not be swallowed; the
                    // 2^53-1 clamp applies BEFORE the advance.
                    let cur = host_index_saturating(
                        self.to_integer_or_zero(cur_v)?.clamp(0, (1i64 << 53) - 1),
                    );
                    #[cfg(feature = "safe-sandbox")]
                    let next = self.advance_index_on_value(string, cur, full_unicode)?;
                    #[cfg(not(feature = "safe-sandbox"))]
                    let next = self.advance_index_on_value(string, cur, full_unicode);
                    self.set_regexp_last_index(regexp, next);
                }
                (r, false, false)
            }
        };
        if latch != done {
            if let Some(e) = self.regexp_string_iters.get_mut(&it_idx) {
                e.done = latch;
            }
        }
        Ok((value, ret_done))
    }

    /// AdvanceStringIndex reading the units from heap string `s` (for the lazy
    /// matchAll driver, which doesn't keep an encoded unit buffer around).
    #[cfg(not(feature = "safe-sandbox"))]
    pub(crate) fn advance_index_on_value(
        &mut self,
        s: Value,
        index: usize,
        unicode: bool,
    ) -> usize {
        if unicode && s.is_heap() {
            self.heap.flatten(s.heap_index());
            if let HeapObj::Str(js) = self.heap.get(s.heap_index()) {
                if let (Some(hi), Some(lo)) =
                    (js.unit_at(index), js.unit_at(index.saturating_add(1)))
                {
                    if (0xD800..=0xDBFF).contains(&hi) && (0xDC00..=0xDFFF).contains(&lo) {
                        return index.saturating_add(2);
                    }
                }
            }
        }
        index.saturating_add(1)
    }

    /// Safe-profile AdvanceStringIndex without flattening a rope or rebuilding
    /// its entire UTF-16 view for every empty global match.
    #[cfg(feature = "safe-sandbox")]
    pub(crate) fn advance_index_on_value(
        &mut self,
        s: Value,
        index: usize,
        unicode: bool,
    ) -> Result<usize, Thrown> {
        if unicode && s.is_heap() {
            if let Some(hi) = regex_value_unit_at(self, s, index)? {
                if (0xD800..=0xDBFF).contains(&hi)
                    && regex_value_unit_at(self, s, index.saturating_add(1))?
                        .is_some_and(|lo| (0xDC00..=0xDFFF).contains(&lo))
                {
                    return Ok(index.saturating_add(2));
                }
            }
        }
        Ok(index.saturating_add(1))
    }

    /// Regex-backed `String.prototype.replace`/`replaceAll`. `repl` is a function
    /// (called `(match, ...groups, offset, input)`) or a template string (`$&`/`$N`/…).
    /// `s_idx` is the receiver string's heap index. All positions are UTF-16
    /// unit indices; the output is assembled as WTF-8 so the subject's lone
    /// surrogates (and a functional replacer's) round-trip exactly.
    pub(crate) fn regex_replace(
        &mut self,
        s_idx: u32,
        re: u32,
        repl: Value,
        global: bool,
    ) -> Result<Value, Thrown> {
        // ASCII subject: match in place over the bytes (offsets == unit
        // indices), no Vec<u16> encode — see `regexp_exec` for why the ASCII
        // backend is semantically identical here.
        #[cfg(not(feature = "safe-sandbox"))]
        self.heap.flatten(s_idx);
        if matches!(self.heap.get(s_idx), HeapObj::Str(js) if js.is_ascii()) {
            return self.regex_replace_ascii(s_idx, re, repl, global);
        }
        // Encode the subject ONCE; every regress range below indexes into it.
        #[cfg(feature = "safe-sandbox")]
        let (u16s, _subject_units_reservation) = regex_subject_units(self, Value::heap(s_idx))?;
        #[cfg(not(feature = "safe-sandbox"))]
        let u16s: Vec<u16> = self.value_units(Value::heap(s_idx));
        #[cfg(feature = "safe-sandbox")]
        let matches: Vec<regress::Match> = {
            let (limits, output_bytes) = self.instrument_regex_collection_limits();
            let max_items = if global { usize::MAX } else { 1 };
            let (collected, mut usage) = match self.heap.get(re) {
                HeapObj::RegExp { regex, flags, .. } => {
                    let unicode = flags.contains('u') || flags.contains('v');
                    if unicode {
                        let mut iter = regex.find_from_utf16_with_limits(&u16s, 0, limits);
                        let collected = iter.try_collect_with_memory_limit(max_items, output_bytes);
                        (collected, iter.match_usage())
                    } else {
                        let mut iter = regex.find_from_ucs2_with_limits(&u16s, 0, limits);
                        let collected = iter.try_collect_with_memory_limit(max_items, output_bytes);
                        (collected, iter.match_usage())
                    }
                }
                _ => (Ok(Vec::new()), regress::MatchUsage::UNMETERED),
            };
            if let Err(error) = &collected {
                usage.exhaustion.get_or_insert(*error);
            }
            self.instrument_regex_usage(usage)
                .map_err(|m| Thrown(m.into()))?;
            match collected {
                Ok(matches) => matches,
                Err(_) => unreachable!("regex exhaustion must return above"),
            }
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let matches: Vec<regress::Match> = match self.heap.get(re) {
            HeapObj::RegExp { regex, flags, .. } => {
                let unicode = flags.contains('u') || flags.contains('v');
                match (unicode, global) {
                    (true, true) => regex.find_from_utf16(&u16s, 0).collect(),
                    (true, false) => regex.find_from_utf16(&u16s, 0).next().into_iter().collect(),
                    (false, true) => regex.find_from_ucs2(&u16s, 0).collect(),
                    (false, false) => regex.find_from_ucs2(&u16s, 0).next().into_iter().collect(),
                }
            }
            _ => Vec::new(),
        };
        #[cfg(feature = "safe-sandbox")]
        let _matches_reservation = self
            .instrument_reserve_regex_transient(retained_match_collection_bytes(
                &matches,
                matches.capacity(),
            ))
            .map_err(|m| Thrown(m.into()))?;
        // IsCallable(replaceValue) — the full predicate, not just a compiled
        // Func/Closure: a bound function, a native, a class, or a Proxy of any
        // of them is a functional replacer too, and testing only the two
        // compiled shapes ToString'd it into a literal template instead.
        let callable = self.is_callable(repl);
        #[cfg(feature = "safe-sandbox")]
        let mut repl_str_reservation = self
            .instrument_reserve_regex_transient(0)
            .map_err(|message| Thrown(message.into()))?;
        #[cfg(feature = "safe-sandbox")]
        let repl_str = if callable {
            String::new()
        } else {
            regex_owned_capture_string(self, &mut repl_str_reservation, repl)?
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let repl_str = if callable {
            String::new()
        } else {
            self.to_js_string(repl)?
        };
        // No match ⇒ the result is the subject unchanged (T0.4): return it as-is,
        // after the observable `ToString(replaceValue)` above, skipping the full
        // subject copy/rebuild. Strings are immutable, so the same heap value is
        // observably identical to a fresh copy.
        if matches.is_empty() {
            return Ok(Value::heap(s_idx));
        }
        // Intrinsic RegExpExec refreshes the Annex-B RegExp statics for every
        // successful match.  This internal matcher collects the same results
        // without building exec arrays, so publish the final successful match
        // now.  No user code can run between the collected matches on this
        // path; consequently the only observable state is exactly the final
        // record that the ordinary exec loop would leave behind.  Keep this
        // after replacement ToString: @@replace coerces a non-callable
        // replacement before it starts its exec loop, so a throwing coercion
        // must not update the statics.
        if let Some(m) = matches.last() {
            let (mstart, mend) = (m.start(), m.end());
            let mk =
                |vm: &mut Self, r: std::ops::Range<usize>| -> Value { vm.units_value(&u16s[r]) };
            #[cfg(feature = "safe-sandbox")]
            let statics_reservation = self
                .instrument_reserve_regex_transient(regexp_statics_materialization_bytes(
                    m,
                    mstart,
                    mend,
                    u16s.len(),
                    false,
                    |range| utf16_slice_heap_bytes(&u16s, range),
                ))
                .map_err(|message| Thrown(message.into()))?;
            self.regexp_record_statics(
                m,
                Value::heap(s_idx),
                s_idx,
                mstart,
                mend,
                u16s.len(),
                false,
                &mk,
            );
            #[cfg(feature = "safe-sandbox")]
            drop(statics_reservation);
        }
        let mut out: Vec<u8> = Vec::new();
        #[cfg(feature = "safe-sandbox")]
        let mut out_reservation = self
            .instrument_reserve_regex_transient(0)
            .map_err(|message| Thrown(message.into()))?;
        let mut last = 0usize;
        for m in &matches {
            let (st, en) = (m.start(), m.end());
            if st < last {
                continue;
            }
            #[cfg(feature = "safe-sandbox")]
            regex_append_units(self, &mut out_reservation, &mut out, &u16s[last..st])?;
            #[cfg(not(feature = "safe-sandbox"))]
            push_units(&mut out, &u16s[last..st]);
            if callable {
                #[cfg(feature = "safe-sandbox")]
                let capture_heap_bytes = utf16_slice_heap_bytes(&u16s, m.range())
                    .saturating_add(m.captures.iter().flatten().fold(0usize, |sum, range| {
                        sum.saturating_add(utf16_slice_heap_bytes(&u16s, range.clone()))
                    }))
                    .saturating_add(regexp_named_objmap_bytes(m));
                #[cfg(feature = "safe-sandbox")]
                let mut capture_heap_reservation = self
                    .instrument_reserve_regex_transient(capture_heap_bytes)
                    .map_err(|message| Thrown(message.into()))?;
                #[cfg(feature = "safe-sandbox")]
                let mut argv_reservation = self
                    .instrument_reserve_regex_transient(0)
                    .map_err(|message| Thrown(message.into()))?;
                #[cfg(feature = "safe-sandbox")]
                let whole = regex_units_value_precharged(
                    self,
                    &mut capture_heap_reservation,
                    &u16s[m.range()],
                )?;
                #[cfg(not(feature = "safe-sandbox"))]
                let whole = self.units_value(&u16s[m.range()]);
                let mut argv = Vec::new();
                let argv_len = m
                    .captures
                    .len()
                    .saturating_add(3)
                    .saturating_add(usize::from(m.named_groups().next().is_some()));
                #[cfg(feature = "safe-sandbox")]
                regex_try_reserve_exact(self, &mut argv_reservation, &mut argv, argv_len)?;
                #[cfg(not(feature = "safe-sandbox"))]
                argv.try_reserve_exact(argv_len).map_err(|_| {
                    Thrown("RangeError: RegExp replacement argument allocation failed".into())
                })?;
                argv.push(whole);
                for cap in &m.captures {
                    argv.push(match cap {
                        Some(r) => {
                            #[cfg(feature = "safe-sandbox")]
                            {
                                regex_units_value_precharged(
                                    self,
                                    &mut capture_heap_reservation,
                                    &u16s[r.clone()],
                                )?
                            }
                            #[cfg(not(feature = "safe-sandbox"))]
                            {
                                self.units_value(&u16s[r.clone()])
                            }
                        }
                        None => Value::UNDEFINED,
                    });
                }
                argv.push(Value::num(st as f64));
                argv.push(Value::heap(s_idx));
                // RegExp.prototype[@@replace] step 14.k.iv: when the regex has named
                // capture groups, a `groups` object (OrdinaryObjectCreate(null)) is
                // the FINAL replacer argument. (Mirrors the exec/array path above.)
                if m.named_groups().next().is_some() {
                    let mut gm = ObjMap::with_capacity(m.named_groups().len());
                    for (name, r) in m.named_groups() {
                        let v = match r {
                            Some(r) => m
                                .captures
                                .iter()
                                .zip(argv.iter().skip(1))
                                .find_map(|(capture, value)| {
                                    (capture.as_ref() == Some(&r)).then_some(*value)
                                })
                                .expect(
                                    "named capture range originates in the indexed capture list",
                                ),
                            None => Value::UNDEFINED,
                        };
                        gm.set(name, v);
                    }
                    let gidx = self.heap.alloc(HeapObj::Object(Box::new(gm)));
                    self.proto_of.insert(gidx, Value::NULL);
                    argv.push(Value::heap(gidx));
                }
                // Capture buffers and group names have moved into the VM heap;
                // heap_bytes now owns that charge. Keep only the argv backing
                // reserved across the observable replacer callback.
                #[cfg(feature = "safe-sandbox")]
                drop(capture_heap_reservation);
                let r = self.call_value(repl, Value::UNDEFINED, &argv)?;
                // ToString(result) — exact bytes (a returned lone-surrogate
                // string keeps its surrogate; `wtf8_push` canonicalizes the seam).
                #[cfg(feature = "safe-sandbox")]
                let bytes = regex_owned_wtf8_string(self, &mut argv_reservation, r)?;
                #[cfg(not(feature = "safe-sandbox"))]
                let bytes = {
                    let rv = self.to_str_value(r)?;
                    self.heap
                        .str_wtf8_cow(rv.heap_index())
                        .map(|c| c.into_owned())
                        .unwrap_or_default()
                };
                #[cfg(feature = "safe-sandbox")]
                regex_append_wtf8(self, &mut out_reservation, &mut out, &bytes)?;
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    if bytes.len() > MAX_STRING_BYTES.saturating_sub(out.len()) {
                        return Err(Thrown("RangeError: Invalid string length".into()));
                    }
                    crate::heap::wtf8_push(&mut out, &bytes);
                }
            } else {
                // GetSubstitution over LOSSY views (the template + captures come
                // through ToString); positions stay unit-exact either way.
                #[cfg(feature = "safe-sandbox")]
                let mut substitution_reservation = self
                    .instrument_reserve_regex_transient(0)
                    .map_err(|message| Thrown(message.into()))?;
                #[cfg(feature = "safe-sandbox")]
                let whole =
                    regex_owned_utf16_lossy(self, &mut substitution_reservation, &u16s[m.range()])?;
                #[cfg(not(feature = "safe-sandbox"))]
                let whole = String::from_utf16_lossy(&u16s[m.range()]);
                let mut groups: Vec<Option<String>> = Vec::new();
                #[cfg(feature = "safe-sandbox")]
                regex_try_reserve_exact(
                    self,
                    &mut substitution_reservation,
                    &mut groups,
                    m.captures.len(),
                )?;
                #[cfg(not(feature = "safe-sandbox"))]
                groups.try_reserve_exact(m.captures.len()).map_err(|_| {
                    Thrown("RangeError: RegExp capture-list allocation failed".into())
                })?;
                for capture in &m.captures {
                    groups.push(match capture {
                        Some(range) => {
                            #[cfg(feature = "safe-sandbox")]
                            {
                                Some(regex_owned_utf16_lossy(
                                    self,
                                    &mut substitution_reservation,
                                    &u16s[range.clone()],
                                )?)
                            }
                            #[cfg(not(feature = "safe-sandbox"))]
                            {
                                Some(String::from_utf16_lossy(&u16s[range.clone()]))
                            }
                        }
                        None => None,
                    });
                }
                let mut named: Vec<(String, Option<String>)> = Vec::new();
                for (name, range) in m.named_groups() {
                    #[cfg(feature = "safe-sandbox")]
                    regex_try_reserve_geometric(
                        self,
                        &mut substitution_reservation,
                        &mut named,
                        1,
                        usize::MAX,
                    )?;
                    #[cfg(feature = "safe-sandbox")]
                    let owned_name = regex_owned_str(self, &mut substitution_reservation, name)?;
                    #[cfg(not(feature = "safe-sandbox"))]
                    let owned_name = name.to_string();
                    let value = match range {
                        Some(range) => {
                            #[cfg(feature = "safe-sandbox")]
                            {
                                Some(regex_owned_utf16_lossy(
                                    self,
                                    &mut substitution_reservation,
                                    &u16s[range],
                                )?)
                            }
                            #[cfg(not(feature = "safe-sandbox"))]
                            {
                                Some(String::from_utf16_lossy(&u16s[range]))
                            }
                        }
                        None => None,
                    };
                    named.push((owned_name, value));
                }
                #[cfg(feature = "safe-sandbox")]
                let pre =
                    regex_owned_utf16_lossy(self, &mut substitution_reservation, &u16s[..st])?;
                #[cfg(not(feature = "safe-sandbox"))]
                let pre = String::from_utf16_lossy(&u16s[..st]);
                #[cfg(feature = "safe-sandbox")]
                let post =
                    regex_owned_utf16_lossy(self, &mut substitution_reservation, &u16s[en..])?;
                #[cfg(not(feature = "safe-sandbox"))]
                let post = String::from_utf16_lossy(&u16s[en..]);
                #[cfg(feature = "safe-sandbox")]
                let rep = self.expand_replacement_safe(
                    &mut substitution_reservation,
                    &repl_str,
                    &whole,
                    &groups,
                    &named,
                    !named.is_empty(),
                    &pre,
                    &post,
                    MAX_STRING_BYTES.saturating_sub(out.len()),
                )?;
                #[cfg(not(feature = "safe-sandbox"))]
                let rep = self.expand_replacement(
                    &repl_str,
                    &whole,
                    &groups,
                    &named,
                    !named.is_empty(),
                    &pre,
                    &post,
                    MAX_STRING_BYTES.saturating_sub(out.len()),
                )?;
                #[cfg(feature = "safe-sandbox")]
                regex_append_wtf8(self, &mut out_reservation, &mut out, rep.as_bytes())?;
                #[cfg(not(feature = "safe-sandbox"))]
                crate::heap::wtf8_push(&mut out, rep.as_bytes());
            }
            last = en;
        }
        #[cfg(feature = "safe-sandbox")]
        regex_append_units(self, &mut out_reservation, &mut out, &u16s[last..])?;
        #[cfg(not(feature = "safe-sandbox"))]
        push_units(&mut out, &u16s[last..]);
        #[cfg(feature = "safe-sandbox")]
        return Ok(regex_wtf8_to_heap(self, &mut out_reservation, out));
        #[cfg(not(feature = "safe-sandbox"))]
        Ok(Value::heap(
            self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out)),
        ))
    }

    /// `regex_replace` for an all-ASCII subject: regress `find_from_ascii`
    /// over the heap bytes in place (byte offsets == unit offsets), output
    /// assembled from byte slices. Functional replacements still append their
    /// EXACT WTF-8 bytes (a replacer may return lone surrogates), so the
    /// output buffer stays WTF-8.
    fn regex_replace_ascii(
        &mut self,
        s_idx: u32,
        re: u32,
        repl: Value,
        global: bool,
    ) -> Result<Value, Thrown> {
        self.ensure_regexp_ascii_twin(re);
        #[cfg(feature = "safe-sandbox")]
        let matches: Vec<regress::Match> = {
            let (limits, output_bytes) = self.instrument_regex_collection_limits();
            let max_items = if global { usize::MAX } else { 1 };
            let subj: &str = match self.heap.get(s_idx) {
                HeapObj::Str(js) => js.as_str_wf(),
                _ => "",
            };
            let regex: Option<&regress::Regex> = match self.heap.get(re) {
                HeapObj::RegExp {
                    ascii_twin: Some(Some(twin)),
                    ..
                } => Some(twin),
                HeapObj::RegExp { regex, .. } => Some(regex),
                _ => None,
            };
            let (collected, mut usage) = match regex {
                Some(regex) => {
                    let mut iter = regex.find_from_ascii_with_limits(subj, 0, limits);
                    let collected = iter.try_collect_with_memory_limit(max_items, output_bytes);
                    (collected, iter.match_usage())
                }
                None => (Ok(Vec::new()), regress::MatchUsage::UNMETERED),
            };
            if let Err(error) = &collected {
                usage.exhaustion.get_or_insert(*error);
            }
            self.instrument_regex_usage(usage)
                .map_err(|m| Thrown(m.into()))?;
            match collected {
                Ok(matches) => matches,
                Err(_) => unreachable!("regex exhaustion must return above"),
            }
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let matches: Vec<regress::Match> = {
            let subj: &str = match self.heap.get(s_idx) {
                HeapObj::Str(js) => js.as_str_wf(),
                _ => "",
            };
            let regex: Option<&regress::Regex> = match self.heap.get(re) {
                HeapObj::RegExp {
                    ascii_twin: Some(Some(twin)),
                    ..
                } => Some(twin),
                HeapObj::RegExp { regex, .. } => Some(regex),
                _ => None,
            };
            match regex {
                Some(regex) => {
                    if global {
                        regex.find_from_ascii(subj, 0).collect()
                    } else {
                        regex.find_from_ascii(subj, 0).next().into_iter().collect()
                    }
                }
                None => Vec::new(),
            }
        };
        #[cfg(feature = "safe-sandbox")]
        let _matches_reservation = self
            .instrument_reserve_regex_transient(retained_match_collection_bytes(
                &matches,
                matches.capacity(),
            ))
            .map_err(|m| Thrown(m.into()))?;
        // IsCallable(replaceValue) — the full predicate, not just a compiled
        // Func/Closure: a bound function, a native, a class, or a Proxy of any
        // of them is a functional replacer too, and testing only the two
        // compiled shapes ToString'd it into a literal template instead.
        let callable = self.is_callable(repl);
        #[cfg(feature = "safe-sandbox")]
        let mut repl_str_reservation = self
            .instrument_reserve_regex_transient(0)
            .map_err(|message| Thrown(message.into()))?;
        #[cfg(feature = "safe-sandbox")]
        let repl_str = if callable {
            String::new()
        } else {
            regex_owned_capture_string(self, &mut repl_str_reservation, repl)?
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let repl_str = if callable {
            String::new()
        } else {
            self.to_js_string(repl)?
        };
        // No match ⇒ the result is the subject unchanged (T0.4): return it as-is,
        // after the observable `ToString(replaceValue)`, skipping the subject
        // memcpy + rebuild. (~46% of the regex bench's section-3 lines have no
        // `//` and hit this.)
        if matches.is_empty() {
            return Ok(Value::heap(s_idx));
        }
        // Own the subject (one memcpy) so the heap allocs below can't
        // invalidate the borrow; ASCII ⇒ valid UTF-8, sliceable as &str.
        #[cfg(feature = "safe-sandbox")]
        let mut subject_reservation = self
            .instrument_reserve_regex_transient(0)
            .map_err(|message| Thrown(message.into()))?;
        #[cfg(feature = "safe-sandbox")]
        let subject = regex_owned_flat_ascii(self, &mut subject_reservation, s_idx)?;
        #[cfg(not(feature = "safe-sandbox"))]
        let subject: String = match self.heap.get(s_idx) {
            HeapObj::Str(js) => js.as_str_wf().to_string(),
            _ => String::new(),
        };
        // See the UTF-16 arm above.  An ASCII subject can retain the same lazy
        // range record used by RegExpBuiltinExec, avoiding thirteen eager
        // subject slices unless a legacy static is actually read.
        if let Some(m) = matches.last() {
            let (mstart, mend) = (m.start(), m.end());
            let mk = |vm: &mut Self, r: std::ops::Range<usize>| -> Value {
                vm.ascii_slice_value(s_idx, r)
            };
            self.regexp_record_statics(
                m,
                Value::heap(s_idx),
                s_idx,
                mstart,
                mend,
                subject.len(),
                subject.len() <= u32::MAX as usize,
                &mk,
            );
        }
        #[cfg(feature = "safe-sandbox")]
        let mut out: Vec<u8> = Vec::new();
        #[cfg(feature = "safe-sandbox")]
        let mut out_reservation = self
            .instrument_reserve_regex_transient(0)
            .map_err(|message| Thrown(message.into()))?;
        #[cfg(feature = "safe-sandbox")]
        regex_try_reserve_exact(
            self,
            &mut out_reservation,
            &mut out,
            subject.len().saturating_add(16),
        )?;
        #[cfg(not(feature = "safe-sandbox"))]
        let mut out: Vec<u8> = Vec::with_capacity(subject.len() + 16);
        let mut last = 0usize;
        for m in &matches {
            let (st, en) = (m.start(), m.end());
            if st < last {
                continue;
            }
            #[cfg(feature = "safe-sandbox")]
            regex_append_bytes(
                self,
                &mut out_reservation,
                &mut out,
                subject[last..st].as_bytes(),
            )?;
            #[cfg(not(feature = "safe-sandbox"))]
            out.extend_from_slice(subject[last..st].as_bytes());
            if callable {
                #[cfg(feature = "safe-sandbox")]
                let capture_heap_bytes = ascii_slice_heap_bytes(m.range())
                    .saturating_add(m.captures.iter().flatten().fold(0usize, |sum, range| {
                        sum.saturating_add(ascii_slice_heap_bytes(range.clone()))
                    }))
                    .saturating_add(regexp_named_objmap_bytes(m));
                #[cfg(feature = "safe-sandbox")]
                let mut capture_heap_reservation = self
                    .instrument_reserve_regex_transient(capture_heap_bytes)
                    .map_err(|message| Thrown(message.into()))?;
                #[cfg(feature = "safe-sandbox")]
                let mut argv_reservation = self
                    .instrument_reserve_regex_transient(0)
                    .map_err(|message| Thrown(message.into()))?;
                #[cfg(feature = "safe-sandbox")]
                let whole = regex_ascii_str_slice_precharged(
                    self,
                    &mut capture_heap_reservation,
                    &subject,
                    m.range(),
                )?;
                #[cfg(not(feature = "safe-sandbox"))]
                let whole = self.alloc_str(subject[m.range()].to_string());
                let mut argv = Vec::new();
                let argv_len = m
                    .captures
                    .len()
                    .saturating_add(3)
                    .saturating_add(usize::from(m.named_groups().next().is_some()));
                #[cfg(feature = "safe-sandbox")]
                regex_try_reserve_exact(self, &mut argv_reservation, &mut argv, argv_len)?;
                #[cfg(not(feature = "safe-sandbox"))]
                argv.try_reserve_exact(argv_len).map_err(|_| {
                    Thrown("RangeError: RegExp replacement argument allocation failed".into())
                })?;
                argv.push(whole);
                for cap in &m.captures {
                    argv.push(match cap {
                        Some(r) => {
                            #[cfg(feature = "safe-sandbox")]
                            {
                                regex_ascii_str_slice_precharged(
                                    self,
                                    &mut capture_heap_reservation,
                                    &subject,
                                    r.clone(),
                                )?
                            }
                            #[cfg(not(feature = "safe-sandbox"))]
                            {
                                self.alloc_str(subject[r.clone()].to_string())
                            }
                        }
                        None => Value::UNDEFINED,
                    });
                }
                argv.push(Value::num(st as f64));
                argv.push(Value::heap(s_idx));
                // RegExp.prototype[@@replace] step 14.k.iv: a `groups` object
                // (OrdinaryObjectCreate(null)) as the FINAL replacer argument
                // when the regex has named capture groups.
                if m.named_groups().next().is_some() {
                    let mut gm = ObjMap::with_capacity(m.named_groups().len());
                    for (name, r) in m.named_groups() {
                        let v = match r {
                            Some(r) => m
                                .captures
                                .iter()
                                .zip(argv.iter().skip(1))
                                .find_map(|(capture, value)| {
                                    (capture.as_ref() == Some(&r)).then_some(*value)
                                })
                                .expect(
                                    "named capture range originates in the indexed capture list",
                                ),
                            None => Value::UNDEFINED,
                        };
                        gm.set(name, v);
                    }
                    let gidx = self.heap.alloc(HeapObj::Object(Box::new(gm)));
                    self.proto_of.insert(gidx, Value::NULL);
                    argv.push(Value::heap(gidx));
                }
                #[cfg(feature = "safe-sandbox")]
                drop(capture_heap_reservation);
                let r = self.call_value(repl, Value::UNDEFINED, &argv)?;
                // ToString(result) — exact bytes (a returned lone-surrogate
                // string keeps its surrogate; `wtf8_push` canonicalizes the seam).
                #[cfg(feature = "safe-sandbox")]
                let bytes = regex_owned_wtf8_string(self, &mut argv_reservation, r)?;
                #[cfg(not(feature = "safe-sandbox"))]
                let bytes = {
                    let rv = self.to_str_value(r)?;
                    self.heap
                        .str_wtf8_cow(rv.heap_index())
                        .map(|c| c.into_owned())
                        .unwrap_or_default()
                };
                #[cfg(feature = "safe-sandbox")]
                regex_append_wtf8(self, &mut out_reservation, &mut out, &bytes)?;
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    if bytes.len() > MAX_STRING_BYTES.saturating_sub(out.len()) {
                        return Err(Thrown("RangeError: Invalid string length".into()));
                    }
                    crate::heap::wtf8_push(&mut out, &bytes);
                }
            } else {
                // GetSubstitution directly over &str slices of the subject.
                #[cfg(feature = "safe-sandbox")]
                let mut substitution_reservation = self
                    .instrument_reserve_regex_transient(0)
                    .map_err(|message| Thrown(message.into()))?;
                let mut groups: Vec<Option<String>> = Vec::new();
                #[cfg(feature = "safe-sandbox")]
                regex_try_reserve_exact(
                    self,
                    &mut substitution_reservation,
                    &mut groups,
                    m.captures.len(),
                )?;
                #[cfg(not(feature = "safe-sandbox"))]
                groups.try_reserve_exact(m.captures.len()).map_err(|_| {
                    Thrown("RangeError: RegExp capture-list allocation failed".into())
                })?;
                for capture in &m.captures {
                    groups.push(match capture {
                        Some(range) => {
                            #[cfg(feature = "safe-sandbox")]
                            {
                                Some(regex_owned_str(
                                    self,
                                    &mut substitution_reservation,
                                    &subject[range.clone()],
                                )?)
                            }
                            #[cfg(not(feature = "safe-sandbox"))]
                            {
                                Some(subject[range.clone()].to_string())
                            }
                        }
                        None => None,
                    });
                }
                let mut named: Vec<(String, Option<String>)> = Vec::new();
                for (name, range) in m.named_groups() {
                    #[cfg(feature = "safe-sandbox")]
                    regex_try_reserve_geometric(
                        self,
                        &mut substitution_reservation,
                        &mut named,
                        1,
                        usize::MAX,
                    )?;
                    #[cfg(feature = "safe-sandbox")]
                    let owned_name = regex_owned_str(self, &mut substitution_reservation, name)?;
                    #[cfg(not(feature = "safe-sandbox"))]
                    let owned_name = name.to_string();
                    let value = match range {
                        Some(range) => {
                            #[cfg(feature = "safe-sandbox")]
                            {
                                Some(regex_owned_str(
                                    self,
                                    &mut substitution_reservation,
                                    &subject[range],
                                )?)
                            }
                            #[cfg(not(feature = "safe-sandbox"))]
                            {
                                Some(subject[range].to_string())
                            }
                        }
                        None => None,
                    };
                    named.push((owned_name, value));
                }
                #[cfg(feature = "safe-sandbox")]
                let rep = self.expand_replacement_safe(
                    &mut substitution_reservation,
                    &repl_str,
                    &subject[m.range()],
                    &groups,
                    &named,
                    !named.is_empty(),
                    &subject[..st],
                    &subject[en..],
                    MAX_STRING_BYTES.saturating_sub(out.len()),
                )?;
                #[cfg(not(feature = "safe-sandbox"))]
                let rep = self.expand_replacement(
                    &repl_str,
                    &subject[m.range()],
                    &groups,
                    &named,
                    !named.is_empty(),
                    &subject[..st],
                    &subject[en..],
                    MAX_STRING_BYTES.saturating_sub(out.len()),
                )?;
                #[cfg(feature = "safe-sandbox")]
                regex_append_wtf8(self, &mut out_reservation, &mut out, rep.as_bytes())?;
                #[cfg(not(feature = "safe-sandbox"))]
                crate::heap::wtf8_push(&mut out, rep.as_bytes());
            }
            last = en;
        }
        #[cfg(feature = "safe-sandbox")]
        regex_append_bytes(
            self,
            &mut out_reservation,
            &mut out,
            subject[last..].as_bytes(),
        )?;
        #[cfg(not(feature = "safe-sandbox"))]
        out.extend_from_slice(subject[last..].as_bytes());
        #[cfg(feature = "safe-sandbox")]
        return Ok(regex_wtf8_to_heap(self, &mut out_reservation, out));
        #[cfg(not(feature = "safe-sandbox"))]
        Ok(Value::heap(
            self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out)),
        ))
    }

    // ── TypedArrays / ArrayBuffer / DataView ──
}

/// The pattern characters fed to the regress parser for a NON-`u`/`v` regex,
/// from the pattern's UTF-16 units. The spec grammar reads such a pattern per
/// CODE UNIT — an astral literal is its two surrogate halves, each its own
/// pattern character (so it matches over UCS-2 subject units) — EXCEPT inside
/// RegExpIdentifierName (a group name `(?<name>…)` / `\k<name>`), where
/// surrogate pairs recombine into code points. Tracks character-class nesting
/// so a literal `(?<` / `\k<` inside `[...]` stays plain units.
pub(crate) fn nonunicode_pattern_chars(units: &[u16]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(units.len());
    let mut i = 0usize;
    let mut in_class = false;
    let mut in_name = false;
    while i < units.len() {
        let u = units[i] as u32;
        if in_name {
            if u == '>' as u32 {
                in_name = false;
                out.push(u);
                i += 1;
            } else if (0xD800..=0xDBFF).contains(&units[i])
                && i + 1 < units.len()
                && (0xDC00..=0xDFFF).contains(&units[i + 1])
            {
                let (hi, lo) = (units[i] as u32, units[i + 1] as u32);
                out.push(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
                i += 2;
            } else {
                out.push(u);
                i += 1;
            }
            continue;
        }
        match u {
            // An escape: copy `\` + the next unit verbatim (so `\[`/`\]` can't
            // flip the class state), EXCEPT `\k<` outside a class, which opens
            // a group-name reference.
            0x5C => {
                out.push(u);
                if i + 1 < units.len() {
                    let n = units[i + 1] as u32;
                    out.push(n);
                    if !in_class
                        && n == 'k' as u32
                        && i + 2 < units.len()
                        && units[i + 2] == '<' as u16
                    {
                        out.push('<' as u32);
                        in_name = true;
                        i += 3;
                        continue;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            0x5B if !in_class => {
                in_class = true;
                out.push(u);
                i += 1;
            }
            0x5D if in_class => {
                in_class = false;
                out.push(u);
                i += 1;
            }
            // `(?<` not followed by `=`/`!` (lookbehinds) opens a group name.
            0x28 if !in_class
                && i + 2 < units.len()
                && units[i + 1] == '?' as u16
                && units[i + 2] == '<' as u16
                && units
                    .get(i + 3)
                    .map_or(true, |&n| n != '=' as u16 && n != '!' as u16) =>
            {
                out.extend_from_slice(&['(' as u32, '?' as u32, '<' as u32]);
                in_name = true;
                i += 3;
            }
            _ => {
                out.push(u);
                i += 1;
            }
        }
    }
    out
}

/// Append `units` onto WTF-8 buffer `out` — exact (`wtf8_push_cp`
/// canonicalizes an adjacent (high, low) pair back into its astral scalar).
pub(crate) fn push_units(out: &mut Vec<u8>, units: &[u16]) {
    for &u in units {
        crate::heap::wtf8_push_cp(out, u as u32);
    }
}

/// AdvanceStringIndex (ES 22.2.7.3): +1 code UNIT, or +2 when `unicode`
/// (the `u`/`v` flags) and `index` sits on a high surrogate directly followed
/// by a low surrogate (one astral code point).
pub(crate) fn advance_string_index(units: &[u16], index: usize, unicode: bool) -> usize {
    if unicode
        && index.saturating_add(1) < units.len()
        && (0xD800..=0xDBFF).contains(&units[index])
        && (0xDC00..=0xDFFF).contains(&units[index + 1])
    {
        index.saturating_add(2)
    } else {
        index.saturating_add(1)
    }
}

/// `ZIPP_NO_MATCH_VARIANT=1` restores the eager `arr_props` `ObjMap` for every
/// match result: the compact record is built and then immediately materialised,
/// so the old representation (and its cost) is reproduced exactly. Exists so
/// the compact form is A/B-able and bisectable on one binary, same as
/// `ZIPP_NO_ENUM_HOIST`.
#[inline]
pub(crate) fn match_variant_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_MATCH_VARIANT").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Exact outer-region scalar matchAll package. The dependent fused-step,
/// slim-exec and batch rungs are part of the proof: disabling any one restores
/// the ordinary result-array path rather than a partially enabled protocol.
#[inline]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn rx_scalar_matchall_enabled() -> bool {
    if cfg!(feature = "safe-sandbox") {
        return false;
    }
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = (std::env::var_os("ZIPP_NO_RX_SCALAR_MATCHALL").is_none()
                && std::env::var_os("ZIPP_NO_MATCHALL_PRISTINE").is_none()
                && std::env::var_os("ZIPP_NO_FASTOK_MEMO").is_none()
                && matchall_step_enabled()
                && slim_exec_enabled()
                && matchall_batch_enabled()
                && match_variant_enabled()
                && string_regexp_call_direct_enabled()
                && std::env::var_os("ZIPP_NO_ITER_REGION").is_none()
                && std::env::var_os("ZIPP_NO_TONUM_STR").is_none()) as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Whole-outer-loop dense-string-array reducer for the exact scalar matchAll
/// plan. `ZIPP_NO_RX_ARRAY_MATCHALL_REDUCE=1` restores the ordinary scalar
/// iterator path on the same binary.
#[inline]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn rx_dense_array_matchall_reduce_enabled() -> bool {
    if cfg!(feature = "safe-sandbox") {
        return false;
    }
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_RX_ARRAY_MATCHALL_REDUCE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Exact MEM-only non-global exec scalarization.  Every dependent switch is
/// part of the isolation contract: turning off the direct call, canonical
/// string-number grammar, compact result representation, or slim ASCII exec
/// restores the pre-scalar native byte stream rather than a partial package.
#[inline]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn rx_scalar_exec_enabled() -> bool {
    if cfg!(feature = "safe-sandbox") {
        return false;
    }
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = (std::env::var_os("ZIPP_NO_RX_SCALAR_EXEC").is_none()
                && regexp_call_direct_enabled()
                && slim_exec_enabled()
                && match_variant_enabled()
                && std::env::var_os("ZIPP_NO_TONUM_STR").is_none()) as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_RX_CALL_DIRECT=1` restores the pre-direct `CallMethod` route on one
/// binary. Codegen reads the same cached latch while compiling a region, so the
/// off mode emits the old generic call site rather than paying a failed helper
/// probe on every iteration.
#[inline]
pub(crate) fn regexp_call_direct_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_RX_CALL_DIRECT").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_RX_STRING_CALL_DIRECT=1` restores the generic primitive-string
/// `CallMethod` route for `matchAll(RegExp)` and `replace(RegExp, string)` on
/// one binary.  Codegen consults the same process latch, so off mode emits no
/// helper probe and all five mechanism counters remain zero.
#[inline]
pub(crate) fn string_regexp_call_direct_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_RX_STRING_CALL_DIRECT").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_RXSTATS=1` — how many match results were CONSTRUCTED in the compact
/// record vs how many were later MATERIALISED into an ordinary `arr_props`
/// `ObjMap` (by mutation/reflection — or by `ZIPP_NO_MATCH_VARIANT=1`, which
/// materialises every one at construction). A workload that only reads
/// `m[i]`/`m.index`/`m.input`/`m.groups` should show near-zero materialisations.
/// Off, this costs one relaxed atomic load per event.
pub(crate) mod rxstats {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    static COMPACT: AtomicU64 = AtomicU64::new(0);
    static MATERIALIZED: AtomicU64 = AtomicU64::new(0);
    static STEP_FUSED: AtomicU64 = AtomicU64::new(0);
    static STEP_FULL: AtomicU64 = AtomicU64::new(0);
    static CALL_INTERP_TEST: AtomicU64 = AtomicU64::new(0);
    static CALL_INTERP_EXEC: AtomicU64 = AtomicU64::new(0);
    static CALL_JIT_TEST: AtomicU64 = AtomicU64::new(0);
    static CALL_JIT_EXEC: AtomicU64 = AtomicU64::new(0);
    static CALL_DECLINE: AtomicU64 = AtomicU64::new(0);
    static STRING_CALL_INTERP_MATCHALL: AtomicU64 = AtomicU64::new(0);
    static STRING_CALL_INTERP_REPLACE: AtomicU64 = AtomicU64::new(0);
    static STRING_CALL_JIT_MATCHALL: AtomicU64 = AtomicU64::new(0);
    static STRING_CALL_JIT_REPLACE: AtomicU64 = AtomicU64::new(0);
    static STRING_CALL_DECLINE: AtomicU64 = AtomicU64::new(0);
    static SCALAR_SUCCESS: AtomicU64 = AtomicU64::new(0);
    static SCALAR_CAPTURE_NUM: AtomicU64 = AtomicU64::new(0);
    static SCALAR_MATERIALIZED: AtomicU64 = AtomicU64::new(0);
    static SCALAR_ELIDED: AtomicU64 = AtomicU64::new(0);
    static SCALAR_GUARD_DECLINE: AtomicU64 = AtomicU64::new(0);
    static SCALAR_SLOW_FLUSH: AtomicU64 = AtomicU64::new(0);
    static SCALAR_EXEC_SUCCESS: AtomicU64 = AtomicU64::new(0);
    static SCALAR_EXEC_MISS: AtomicU64 = AtomicU64::new(0);
    static SCALAR_EXEC_CAPTURE_NUM: AtomicU64 = AtomicU64::new(0);
    static SCALAR_EXEC_MATERIALIZED: AtomicU64 = AtomicU64::new(0);
    static SCALAR_EXEC_ELIDED: AtomicU64 = AtomicU64::new(0);
    static SCALAR_EXEC_GUARD_DECLINE: AtomicU64 = AtomicU64::new(0);
    static SCALAR_EXEC_PIN_DECLINE: AtomicU64 = AtomicU64::new(0);
    static SCALAR_EXEC_SLOW_FLUSH: AtomicU64 = AtomicU64::new(0);

    #[inline]
    pub(crate) fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_RXSTATS").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }

    #[inline]
    pub(crate) fn count_compact() {
        if enabled() {
            COMPACT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(crate) fn count_materialized() {
        if enabled() {
            MATERIALIZED.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A %RegExpStringIterator% step served by the fused pristine path
    /// (B118): flag bits from the iterator record, no per-step protocol
    /// re-proof beyond the version-guarded slot memo.
    #[inline]
    pub(crate) fn count_step_fused() {
        if enabled() {
            STEP_FUSED.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A fused-ELIGIBLE step that fell back to the full observable protocol
    /// (memo cold/invalidated, or a guard declined).
    #[inline]
    pub(crate) fn count_step_full() {
        if enabled() {
            STEP_FULL.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A guarded direct `RegExp.prototype.test` / `exec` CallMethod actually
    /// served. Split by interpreter/JIT so a hot test cannot pass solely on its
    /// warmup calls.
    #[inline]
    pub(crate) fn count_call_direct_hit(is_test: bool, from_jit: bool) {
        if enabled() {
            match (from_jit, is_test) {
                (false, true) => &CALL_INTERP_TEST,
                (false, false) => &CALL_INTERP_EXEC,
                (true, true) => &CALL_JIT_TEST,
                (true, false) => &CALL_JIT_EXEC,
            }
            .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A direct-call probe that declined before any observable operation.
    #[inline]
    pub(crate) fn count_call_direct_decline() {
        if enabled() {
            CALL_DECLINE.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A guarded primitive-string `matchAll` / regex `replace` direct call was
    /// served.  Split by interpreter/JIT to make hot generated-code coverage
    /// independently non-vacuous.
    #[inline]
    pub(crate) fn count_string_call_direct_hit(is_replace: bool, from_jit: bool) {
        if enabled() {
            match (from_jit, is_replace) {
                (false, false) => &STRING_CALL_INTERP_MATCHALL,
                (false, true) => &STRING_CALL_INTERP_REPLACE,
                (true, false) => &STRING_CALL_JIT_MATCHALL,
                (true, true) => &STRING_CALL_JIT_REPLACE,
            }
            .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A string/RegExp direct-call probe that declined before any observable
    /// operation.
    #[inline]
    pub(crate) fn count_string_call_direct_decline() {
        if enabled() {
            STRING_CALL_DECLINE.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_success() {
        if enabled() {
            SCALAR_SUCCESS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_capture_num() {
        if enabled() {
            SCALAR_CAPTURE_NUM.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_materialized(slow_reentry: bool) {
        if enabled() {
            SCALAR_MATERIALIZED.fetch_add(1, Ordering::Relaxed);
            if slow_reentry {
                SCALAR_SLOW_FLUSH.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_elided() {
        if enabled() {
            SCALAR_ELIDED.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Account for the observable operations represented by one successful
    /// outer-array reduction without putting an atomic/branch in its match loop.
    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_array_reduce(matches: u64, subjects: u64) {
        if enabled() {
            SCALAR_SUCCESS.fetch_add(matches, Ordering::Relaxed);
            SCALAR_CAPTURE_NUM.fetch_add(matches, Ordering::Relaxed);
            let elided = matches.saturating_sub((matches != 0) as u64);
            SCALAR_ELIDED.fetch_add(elided, Ordering::Relaxed);
            // Each global iterator would expose one step per match plus its
            // terminal done step. Preserve the diagnostic accounting.
            STEP_FUSED.fetch_add(matches.saturating_add(subjects), Ordering::Relaxed);
        }
    }

    /// A scalar helper guard that declined before any observable operation.
    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_guard_decline() {
        if enabled() {
            SCALAR_GUARD_DECLINE.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_exec_success() {
        if enabled() {
            SCALAR_EXEC_SUCCESS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_exec_miss() {
        if enabled() {
            SCALAR_EXEC_MISS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_exec_capture_nums(n: u64) {
        if enabled() {
            SCALAR_EXEC_CAPTURE_NUM.fetch_add(n, Ordering::Relaxed);
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_exec_materialized(slow_reentry: bool) {
        if enabled() {
            SCALAR_EXEC_MATERIALIZED.fetch_add(1, Ordering::Relaxed);
            if slow_reentry {
                SCALAR_EXEC_SLOW_FLUSH.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_exec_elided() {
        if enabled() {
            SCALAR_EXEC_ELIDED.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_exec_guard_decline() {
        if enabled() {
            SCALAR_EXEC_GUARD_DECLINE.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn count_scalar_exec_pin_decline() {
        if enabled() {
            SCALAR_EXEC_PIN_DECLINE.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `(compact_constructions, materialized, steps_fused, steps_fallback)`.
    pub fn dump() -> (u64, u64, u64, u64) {
        (
            COMPACT.load(Ordering::Relaxed),
            MATERIALIZED.load(Ordering::Relaxed),
            STEP_FUSED.load(Ordering::Relaxed),
            STEP_FULL.load(Ordering::Relaxed),
        )
    }

    /// `(interp_test, interp_exec, jit_test, jit_exec, guard_declines)`.
    pub fn dump_call_direct() -> (u64, u64, u64, u64, u64) {
        (
            CALL_INTERP_TEST.load(Ordering::Relaxed),
            CALL_INTERP_EXEC.load(Ordering::Relaxed),
            CALL_JIT_TEST.load(Ordering::Relaxed),
            CALL_JIT_EXEC.load(Ordering::Relaxed),
            CALL_DECLINE.load(Ordering::Relaxed),
        )
    }

    /// `(interp_matchall, interp_replace, jit_matchall, jit_replace,
    /// guard_declines)`.
    pub fn dump_string_call_direct() -> (u64, u64, u64, u64, u64) {
        (
            STRING_CALL_INTERP_MATCHALL.load(Ordering::Relaxed),
            STRING_CALL_INTERP_REPLACE.load(Ordering::Relaxed),
            STRING_CALL_JIT_MATCHALL.load(Ordering::Relaxed),
            STRING_CALL_JIT_REPLACE.load(Ordering::Relaxed),
            STRING_CALL_DECLINE.load(Ordering::Relaxed),
        )
    }

    /// `(successes, direct_capture_numbers, materialized, elided,
    /// guard_declines, slow_reentry_flushes)` for the exact matchAll scalar
    /// outer-region package.
    pub fn dump_scalar_matchall() -> (u64, u64, u64, u64, u64, u64) {
        (
            SCALAR_SUCCESS.load(Ordering::Relaxed),
            SCALAR_CAPTURE_NUM.load(Ordering::Relaxed),
            SCALAR_MATERIALIZED.load(Ordering::Relaxed),
            SCALAR_ELIDED.load(Ordering::Relaxed),
            SCALAR_GUARD_DECLINE.load(Ordering::Relaxed),
            SCALAR_SLOW_FLUSH.load(Ordering::Relaxed),
        )
    }

    /// `(successes, semantic_misses, direct_capture_numbers, materialized,
    /// elided, guard_declines, input_pin_declines, slow_reentry_flushes)` for
    /// the exact non-global exec scalar package.
    pub fn dump_scalar_exec() -> (u64, u64, u64, u64, u64, u64, u64, u64) {
        (
            SCALAR_EXEC_SUCCESS.load(Ordering::Relaxed),
            SCALAR_EXEC_MISS.load(Ordering::Relaxed),
            SCALAR_EXEC_CAPTURE_NUM.load(Ordering::Relaxed),
            SCALAR_EXEC_MATERIALIZED.load(Ordering::Relaxed),
            SCALAR_EXEC_ELIDED.load(Ordering::Relaxed),
            SCALAR_EXEC_GUARD_DECLINE.load(Ordering::Relaxed),
            SCALAR_EXEC_PIN_DECLINE.load(Ordering::Relaxed),
            SCALAR_EXEC_SLOW_FLUSH.load(Ordering::Relaxed),
        )
    }
}

/// Assemble a RegExp `flags` string in the canonical order `dgimsuvy`,
/// regardless of the order the flags were supplied in.
pub(crate) fn canonical_flags(flags: &str) -> String {
    let mut out = String::new();
    for ch in ['d', 'g', 'i', 'm', 's', 'u', 'v', 'y'] {
        if flags.contains(ch) {
            out.push(ch);
        }
    }
    out
}
