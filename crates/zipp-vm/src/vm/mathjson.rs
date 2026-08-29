#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// `ZIPP_NO_JSON_LEAF_FAST=1` restores the old JSON.stringify leaf emission:
/// the `into_owned` copy per string leaf, the fresh `String` per number
/// (`fmt_f64`), and the cloned-key + `pos()` re-lookup object walk. Kept so
/// each change is A/B-able and bisectable on one binary.
#[inline]
fn json_leaf_fast_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_JSON_LEAF_FAST").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Default-on compact `JSON.stringify(value)` fast path for a graph made only
/// of plain data objects, dense Arrays and JSON primitive leaves. The entire
/// output is private until the walk succeeds, so an exotic node can decline
/// after an arbitrary prefix without exposing work; the ordinary serializer
/// then restarts and observes getters, proxies, `toJSON`, holes and errors.
/// `ZIPP_NO_JSON_PLAIN_FAST=1` restores the general recursive serializer.
#[inline]
fn json_plain_fast_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_JSON_PLAIN_FAST").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

const JSON_STRING_LIMIT_ERROR: &str = "RangeError: Invalid string length";

/// Pretty serialization retains one indentation string per recursive level;
/// bounding depth in the hardened profile keeps that live set quadratic only
/// in a small constant. The default profile retains its existing semantics.
#[cfg(feature = "safe-sandbox")]
const MAX_JSON_NESTING_DEPTH: usize = 64;

/// `JSON.stringify` snapshots object keys before it invokes getters, `toJSON`,
/// or a replacer. Those copies are deliberately outside the guest heap so a
/// mutation cannot invalidate them, but that also means the ordinary heap
/// audit cannot see them. Bound the complete key-snapshot work for one
/// stringify operation (including the retained array-replacer PropertyList)
/// well below the 128 MiB VM ceiling and the 256 MiB WASM backstop.
#[cfg(feature = "safe-sandbox")]
const MAX_JSON_KEY_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
#[cfg(not(feature = "safe-sandbox"))]
const MAX_JSON_KEY_SNAPSHOT_BYTES: usize = usize::MAX;

const JSON_KEY_SNAPSHOT_LIMIT_ERROR: &str =
    "RangeError: JSON key snapshot exceeds the sandbox memory limit";

/// Conservative retained bytes per occupied `HashMap<String, usize>` bucket.
/// `capacity()` includes the table's spare buckets; the extra two words cover
/// control bytes, hashes/alignment and allocator rounding in addition to the
/// `(String, usize)` payload.
const JSON_PROPERTY_MAP_BUCKET_BYTES: usize =
    std::mem::size_of::<(String, usize)>() + 2 * std::mem::size_of::<usize>();

/// Cumulative private key-copy work for one stringify. Cumulative accounting
/// is intentionally stricter than live-only accounting: sibling objects cannot
/// repeatedly allocate and free large unmetered snapshots inside one native VM
/// instruction. Interior mutability lets recursive frames share the budget
/// without keeping a mutable borrow across guest callbacks.
#[derive(Default)]
struct JsonKeySnapshotBudget {
    used: std::cell::Cell<usize>,
}

/// Recursive JSON parsing is intentionally left compatible with the ordinary
/// engine profile, but the hardened profile must fail before a valid, deeply
/// nested document can exhaust the native (or WebAssembly) call stack.
#[inline]
fn json_check_parse_depth(depth: usize) -> Result<(), Thrown> {
    #[cfg(feature = "safe-sandbox")]
    if depth >= MAX_JSON_NESTING_DEPTH {
        return Err(Thrown(
            "RangeError: JSON parse nesting depth exceeds the sandbox limit".into(),
        ));
    }
    #[cfg(not(feature = "safe-sandbox"))]
    let _ = depth;
    Ok(())
}

/// Private output-buffer failure. Both an arithmetic/engine-size overflow and
/// `try_reserve_exact` failure become the same catchable JavaScript RangeError;
/// callers must not expose allocator details to guest code.
#[derive(Clone, Copy)]
struct JsonOutputError;

#[inline]
fn json_reserve_bounded(out: &mut String, additional: usize) -> Result<(), JsonOutputError> {
    let target = out
        .len()
        .checked_add(additional)
        .filter(|&len| len <= MAX_STRING_BYTES)
        .ok_or(JsonOutputError)?;
    if target > out.capacity() {
        // Grow geometrically so a delimiter-heavy document does not turn exact
        // fallible reservation into quadratic copying. The requested capacity
        // is itself capped; `try_reserve_exact` prevents `String::push` from
        // taking an infallible allocation path.
        let doubled = out.capacity().checked_mul(2).unwrap_or(MAX_STRING_BYTES);
        let desired = target.max(doubled.max(16)).min(MAX_STRING_BYTES);
        // `try_reserve_exact` takes bytes beyond the current *length*, not
        // beyond the current capacity. Passing `target - capacity` can be a
        // no-op and leave the following push free to invoke an infallible,
        // potentially aborting allocation.
        out.try_reserve_exact(desired - out.len())
            .map_err(|_| JsonOutputError)?;
    }
    Ok(())
}

#[inline]
fn json_push_str_bounded(out: &mut String, text: &str) -> Result<(), JsonOutputError> {
    json_reserve_bounded(out, text.len())?;
    out.push_str(text);
    Ok(())
}

#[inline]
fn json_push_char_bounded(out: &mut String, ch: char) -> Result<(), JsonOutputError> {
    json_reserve_bounded(out, ch.len_utf8())?;
    out.push(ch);
    Ok(())
}

#[inline]
fn json_quoted_code_point_len(cp: u32) -> usize {
    match cp {
        0x22 | 0x5C | 0x08 | 0x09 | 0x0A | 0x0C | 0x0D => 2,
        c if c < 0x20 || (0xD800..=0xDFFF).contains(&c) => 6,
        c => char::from_u32(c).unwrap_or('\u{FFFD}').len_utf8(),
    }
}

/// B234: does every byte stand for itself in the output?
///
/// True only for PRINTABLE ASCII with no `"` and no `\\`. Each such byte is a
/// whole code point (so no decode is needed to find the boundaries), is not
/// escaped (so it contributes one output byte), and cannot be part of a
/// multi-byte or lone-surrogate sequence (so nothing later in the buffer can
/// reinterpret it). The quoted length is then exactly `len + 2` — the two
/// delimiters — and the exact loop below has nothing to add.
///
/// Deliberately NOT extended to multi-byte UTF-8: `wtf8_decode` answers
/// malformed input with U+FFFD, whose encoded length differs from the bytes
/// it consumed, so a byte scan could disagree with the writer for input this
/// predicate cannot rule out. ASCII has no such case.
#[inline]
fn json_quote_len_is_plain(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|&c| (0x20..0x80).contains(&c) && c != b'"' && c != b'\\')
}

fn json_quoted_str_len(text: &str) -> Result<usize, JsonOutputError> {
    if json_quote_fastlen_enabled() && json_quote_len_is_plain(text.as_bytes()) {
        return text.len().checked_add(2).ok_or(JsonOutputError);
    }
    text.chars().try_fold(2usize, |len, ch| {
        len.checked_add(json_quoted_code_point_len(ch as u32))
            .ok_or(JsonOutputError)
    })
}

fn json_quoted_wtf8_len(bytes: &[u8]) -> Result<usize, JsonOutputError> {
    if json_quote_fastlen_enabled() && json_quote_len_is_plain(bytes) {
        return bytes.len().checked_add(2).ok_or(JsonOutputError);
    }
    crate::heap::wtf8_code_points(bytes).try_fold(2usize, |len, cp| {
        len.checked_add(json_quoted_code_point_len(cp))
            .ok_or(JsonOutputError)
    })
}

/// B234 latch: `ZIPP_NO_JSON_QUOTE_FASTLEN=1` sizes every quoted string by
/// decoding it, as before.
fn json_quote_fastlen_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_JSON_QUOTE_FASTLEN").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

fn json_quote_bounded(out: &mut String, text: &str) -> Result<(), JsonOutputError> {
    json_reserve_bounded(out, json_quoted_str_len(text)?)?;
    // `json_quote_into`'s own reserve is now a no-op: the exact escaped size is
    // already available, including the surrounding quotes.
    json_quote_into(out, text);
    Ok(())
}

fn json_quote_wtf8_bounded(out: &mut String, bytes: &[u8]) -> Result<(), JsonOutputError> {
    json_reserve_bounded(out, json_quoted_wtf8_len(bytes)?)?;
    json_quote_wtf8_into(out, bytes);
    Ok(())
}

fn json_number_bounded(out: &mut String, number: f64) -> Result<(), JsonOutputError> {
    // All finite IEEE-754 numbers fit comfortably in 32 output bytes. Keep the
    // allocation-free hot path until the buffer is within that distance of the
    // ceiling; only the boundary case formats into a tiny temporary so a valid
    // short number is not rejected merely because the conservative headroom is
    // larger than its actual representation.
    const NUMBER_HEADROOM: usize = 32;
    let remaining = MAX_STRING_BYTES
        .checked_sub(out.len())
        .ok_or(JsonOutputError)?;
    if remaining >= NUMBER_HEADROOM {
        json_reserve_bounded(out, NUMBER_HEADROOM)?;
        let mark = out.len();
        fmt_f64_into(out, number);
        debug_assert!(out.len() - mark <= NUMBER_HEADROOM);
        if out.len() > MAX_STRING_BYTES {
            out.truncate(mark);
            return Err(JsonOutputError);
        }
        return Ok(());
    }

    let mut encoded = String::new();
    encoded
        .try_reserve_exact(NUMBER_HEADROOM)
        .map_err(|_| JsonOutputError)?;
    fmt_f64_into(&mut encoded, number);
    json_push_str_bounded(out, &encoded)
}

fn json_repeat_bounded(text: &str, count: usize) -> Result<String, JsonOutputError> {
    let bytes = text
        .len()
        .checked_mul(count)
        .filter(|&len| len <= MAX_STRING_BYTES)
        .ok_or(JsonOutputError)?;
    let mut repeated = String::new();
    repeated
        .try_reserve_exact(bytes)
        .map_err(|_| JsonOutputError)?;
    for _ in 0..count {
        repeated.push_str(text);
    }
    Ok(repeated)
}

/// Fallible `spec_key_order` used by stringify snapshots. A first pass gives
/// exact integer/rest counts, avoiding geometric infallible growth on hostile
/// objects with many keys.
fn json_spec_key_order_bounded(keys: &[String]) -> Result<(Vec<usize>, usize), JsonOutputError> {
    let integer_count = keys
        .iter()
        .filter(|key| canonical_u32_key(key).is_some())
        .count();
    let mut integers = Vec::new();
    integers
        .try_reserve_exact(integer_count)
        .map_err(|_| JsonOutputError)?;
    let mut rest = Vec::new();
    rest.try_reserve_exact(keys.len().saturating_sub(integer_count))
        .map_err(|_| JsonOutputError)?;
    for (index, key) in keys.iter().enumerate() {
        match canonical_u32_key(key) {
            Some(number) => integers.push((number, index)),
            None => rest.push(index),
        }
    }
    if integers.is_empty() {
        let allocated = rest
            .capacity()
            .checked_mul(std::mem::size_of::<usize>())
            .ok_or(JsonOutputError)?;
        return Ok((rest, allocated));
    }
    integers.sort_unstable_by_key(|&(number, _)| number);
    let mut ordered = Vec::new();
    ordered
        .try_reserve_exact(keys.len())
        .map_err(|_| JsonOutputError)?;
    let allocated = integers
        .capacity()
        .checked_mul(std::mem::size_of::<(u32, usize)>())
        .and_then(|bytes| {
            rest.capacity()
                .checked_mul(std::mem::size_of::<usize>())
                .and_then(|rest_bytes| bytes.checked_add(rest_bytes))
        })
        .and_then(|bytes| {
            ordered
                .capacity()
                .checked_mul(std::mem::size_of::<usize>())
                .and_then(|ordered_bytes| bytes.checked_add(ordered_bytes))
        })
        .ok_or(JsonOutputError)?;
    ordered.extend(integers.into_iter().map(|(_, index)| index));
    ordered.extend(rest);
    Ok((ordered, allocated))
}

impl<'p> Vm<'p> {
    fn json_output_error(&mut self) -> Thrown {
        // A metered sandbox treats exhaustion as terminal even if guest code
        // catches this immediate RangeError. Unmetered/ordinary engines retain
        // the normal catchable maximum-string-length failure.
        #[cfg(feature = "instrument")]
        if let Err(message) = self.instrument_preflight_heap_growth(usize::MAX) {
            return Thrown(message.into());
        }
        Thrown(JSON_STRING_LIMIT_ERROR.into())
    }

    #[inline]
    fn json_key_snapshot_limit_error(&self) -> Thrown {
        Thrown(JSON_KEY_SNAPSHOT_LIMIT_ERROR.into())
    }

    fn json_key_snapshot_allocation_error(&mut self) -> Thrown {
        // An allocator failure is terminal for an instrumented sandbox even if
        // guest code catches the immediate RangeError. The fixed 8 MiB policy
        // limit itself remains an ordinary catchable operation limit.
        #[cfg(feature = "instrument")]
        if let Err(message) = self.instrument_preflight_heap_growth(usize::MAX) {
            return Thrown(message.into());
        }
        self.json_key_snapshot_limit_error()
    }

    /// Check and then account private serializer key storage. `output_capacity`
    /// is included in the VM heap preflight because that output buffer is also
    /// private Rust memory until the final guest string is allocated.
    fn json_charge_key_snapshot(
        &mut self,
        budget: &JsonKeySnapshotBudget,
        additional: usize,
        output_capacity: usize,
    ) -> Result<(), Thrown> {
        let total = budget
            .used
            .get()
            .checked_add(additional)
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        if total > MAX_JSON_KEY_SNAPSHOT_BYTES {
            return Err(self.json_key_snapshot_limit_error());
        }
        let projected = total
            .checked_add(output_capacity)
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        #[cfg(feature = "instrument")]
        if self.instr_rec.is_some() {
            self.instrument_preflight_heap_growth(projected)
                .map_err(|message| Thrown(message.into()))?;
        }
        #[cfg(not(feature = "instrument"))]
        let _ = projected;
        budget.used.set(total);
        Ok(())
    }

    /// Preflight a fallible reserve before requesting it. The actual capacity
    /// returned by the allocator is charged separately after the reserve.
    fn json_preflight_key_snapshot(
        &mut self,
        budget: &JsonKeySnapshotBudget,
        additional: usize,
        output_capacity: usize,
    ) -> Result<(), Thrown> {
        let total = budget
            .used
            .get()
            .checked_add(additional)
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        if total > MAX_JSON_KEY_SNAPSHOT_BYTES {
            return Err(self.json_key_snapshot_limit_error());
        }
        let projected = total
            .checked_add(output_capacity)
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        #[cfg(feature = "instrument")]
        if self.instr_rec.is_some() {
            self.instrument_preflight_heap_growth(projected)
                .map_err(|message| Thrown(message.into()))?;
        }
        #[cfg(not(feature = "instrument"))]
        let _ = projected;
        Ok(())
    }

    fn json_commit_output(
        &mut self,
        _out: &String,
        _prior_capacity: usize,
        appended: Result<(), JsonOutputError>,
    ) -> Result<(), Thrown> {
        if appended.is_err() {
            return Err(self.json_output_error());
        }
        // The serializer buffer is not in `Heap` until `alloc_str` takes it.
        // Charge its whole retained capacity as projected growth, rather than
        // only the last append, so repeated aliases cannot evade a small meter.
        #[cfg(feature = "instrument")]
        if _out.capacity() != _prior_capacity && self.instr_rec.is_some() {
            self.instrument_preflight_heap_growth(_out.capacity())
                .map_err(|message| Thrown(message.into()))?;
        }
        Ok(())
    }

    fn json_push_str_output(&mut self, out: &mut String, text: &str) -> Result<(), Thrown> {
        let prior_capacity = out.capacity();
        let appended = json_push_str_bounded(out, text);
        self.json_commit_output(out, prior_capacity, appended)
    }

    fn json_push_char_output(&mut self, out: &mut String, ch: char) -> Result<(), Thrown> {
        let prior_capacity = out.capacity();
        let appended = json_push_char_bounded(out, ch);
        self.json_commit_output(out, prior_capacity, appended)
    }

    fn json_quote_output(&mut self, out: &mut String, text: &str) -> Result<(), Thrown> {
        let prior_capacity = out.capacity();
        let appended = json_quote_bounded(out, text);
        self.json_commit_output(out, prior_capacity, appended)
    }

    fn json_quote_heap_string_output(
        &mut self,
        out: &mut String,
        index: u32,
    ) -> Result<(), Thrown> {
        let prior_capacity = out.capacity();
        let appended = if json_leaf_fast_enabled() {
            let bytes = self.heap.str_wtf8_cow(index).unwrap();
            json_quote_wtf8_bounded(out, &bytes)
        } else {
            let bytes = self.heap.str_wtf8_cow(index).unwrap().into_owned();
            json_quote_wtf8_bounded(out, &bytes)
        };
        self.json_commit_output(out, prior_capacity, appended)
    }

    fn json_push_number_output(&mut self, out: &mut String, number: f64) -> Result<(), Thrown> {
        let prior_capacity = out.capacity();
        let appended = if number.is_finite() {
            if json_leaf_fast_enabled() {
                json_number_bounded(out, number)
            } else {
                json_push_str_bounded(out, &fmt_f64(number))
            }
        } else {
            json_push_str_bounded(out, "null")
        };
        self.json_commit_output(out, prior_capacity, appended)
    }

    fn json_push_line_pad_output(&mut self, out: &mut String, pad: &str) -> Result<(), Thrown> {
        let prior_capacity = out.capacity();
        let appended =
            json_push_char_bounded(out, '\n').and_then(|()| json_push_str_bounded(out, pad));
        self.json_commit_output(out, prior_capacity, appended)
    }

    fn json_push_entry_prefix_output(
        &mut self,
        out: &mut String,
        any: bool,
        pretty: bool,
        pad: &str,
        key: &str,
        separator: &str,
    ) -> Result<(), Thrown> {
        let prior_capacity = out.capacity();
        let appended = (|| {
            if any {
                json_push_char_bounded(out, ',')?;
            }
            if pretty {
                json_push_char_bounded(out, '\n')?;
                json_push_str_bounded(out, pad)?;
            }
            json_quote_bounded(out, key)?;
            json_push_str_bounded(out, separator)
        })();
        self.json_commit_output(out, prior_capacity, appended)
    }

    #[allow(clippy::too_many_arguments)]
    fn json_push_slot_entry_output(
        &mut self,
        out: &mut String,
        object_index: u32,
        slot: usize,
        any: bool,
        pretty: bool,
        pad: &str,
        separator: &str,
    ) -> Result<Value, Thrown> {
        let prior_capacity = out.capacity();
        let (value, appended) = match self.heap.get(object_index) {
            HeapObj::Object(map) => {
                let appended = (|| {
                    if any {
                        json_push_char_bounded(out, ',')?;
                    }
                    if pretty {
                        json_push_char_bounded(out, '\n')?;
                        json_push_str_bounded(out, pad)?;
                    }
                    json_quote_bounded(out, &map.keys[slot])?;
                    json_push_str_bounded(out, separator)
                })();
                (map.val_at(slot), appended)
            }
            // Unreachable — no user code has run since the slot plan was made.
            _ => (Value::UNDEFINED, Ok(())),
        };
        self.json_commit_output(out, prior_capacity, appended)?;
        Ok(value)
    }

    /// Serialize the closed plain-data subset without paying the general
    /// serializer's per-node `toJSON` lookup, key snapshot, version probe and
    /// generic Array length/index dispatch. `None` is a side-effect-free
    /// decline, never the JavaScript `undefined` result.
    pub(crate) fn json_plain_stringify(&mut self, root: Value) -> Option<String> {
        if !json_plain_fast_enabled() || self.current_realm_id().is_some() {
            return None;
        }

        // Plain objects/arrays/strings still perform a live `toJSON` lookup in
        // the general serializer. Only accept the default main-realm chains
        // while none of their links can answer that lookup. An explicit proto
        // entry means user code changed the chain, even if today's end happens
        // not to contain `toJSON`, so leave it to the observable generic walk.
        if self.obj_proto == 0
            || self.arr_proto == 0
            || self.str_proto == 0
            || self.proto_of.contains_key(&self.obj_proto)
            || self.proto_of.contains_key(&self.arr_proto)
            || self.proto_of.contains_key(&self.str_proto)
        {
            return None;
        }
        let no_own_tojson = |vm: &Self, idx: u32| match vm.heap.get(idx) {
            HeapObj::Object(map) => map.pos("toJSON").is_none(),
            _ => vm
                .arr_props
                .get(&idx)
                .is_none_or(|map| map.pos("toJSON").is_none()),
        };
        if !no_own_tojson(self, self.obj_proto)
            || !no_own_tojson(self, self.arr_proto)
            || !no_own_tojson(self, self.str_proto)
        {
            return None;
        }

        let mut out = String::new();
        out.try_reserve_exact(1024.min(MAX_STRING_BYTES)).ok()?;
        let mut active = Vec::new();
        active.try_reserve_exact(16).ok()?;
        if !self.json_plain_value_into(root, 0, &mut active, &mut out) {
            return None;
        }
        // The fast walk is side-effect free, but recursively borrows the heap;
        // charge its completed, size-bounded buffer once that borrow has ended.
        // A failure makes the ordinary serializer rerun and surface the sticky
        // typed resource error through its first checked append.
        #[cfg(feature = "instrument")]
        if self.instr_rec.is_some()
            && self
                .instrument_preflight_heap_growth(out.capacity())
                .is_err()
        {
            return None;
        }
        Some(out)
    }

    /// Recursive emitter for [`Self::json_plain_stringify`]. Depth is capped
    /// below the ordinary engine stack limit so the iterative-looking fast
    /// path cannot hide the generic serializer's stack overflow behaviour.
    fn json_plain_value_into(
        &self,
        value: Value,
        depth: usize,
        active: &mut Vec<u32>,
        out: &mut String,
    ) -> bool {
        #[cfg(feature = "safe-sandbox")]
        const MAX_DEPTH: usize = MAX_JSON_NESTING_DEPTH;
        #[cfg(not(feature = "safe-sandbox"))]
        const MAX_DEPTH: usize = 256;
        if value.is_null() {
            return json_push_str_bounded(out, "null").is_ok();
        }
        if value.is_bool() {
            return json_push_str_bounded(out, if value.as_bool() { "true" } else { "false" })
                .is_ok();
        }
        if value.is_number() {
            let n = value.as_f64();
            if n.is_finite() {
                return json_number_bounded(out, n).is_ok();
            } else {
                return json_push_str_bounded(out, "null").is_ok();
            }
        }
        if !value.is_heap() {
            return false;
        }
        let idx = value.heap_index();
        match self.heap.get(idx) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => {
                let Some(bytes) = self.heap.str_wtf8_cow(idx) else {
                    return false;
                };
                json_quote_wtf8_bounded(out, &bytes).is_ok()
            }
            HeapObj::Array(items) => {
                if depth >= MAX_DEPTH
                    || active.contains(&idx)
                    || self.proto_of.contains_key(&idx)
                    || self.arguments_objs.contains_key(&idx)
                    || self.arr_props.contains_key(&idx)
                    || self.array_js_len.contains_key(&idx)
                    || items.iter().any(|item| item.is_hole())
                {
                    return false;
                }
                active.push(idx);
                if json_push_char_bounded(out, '[').is_err() {
                    active.pop();
                    return false;
                }
                for (i, &item) in items.iter().enumerate() {
                    if i != 0 && json_push_char_bounded(out, ',').is_err() {
                        active.pop();
                        return false;
                    }
                    if !self.json_plain_value_into(item, depth + 1, active, out) {
                        active.pop();
                        return false;
                    }
                }
                if json_push_char_bounded(out, ']').is_err() {
                    active.pop();
                    return false;
                }
                active.pop();
                true
            }
            HeapObj::Object(map) => {
                if depth >= MAX_DEPTH
                    || active.contains(&idx)
                    || idx == self.global_this
                    // `%Array.prototype%` is internally an Object map, but
                    // IsArray is true and the ordinary serializer emits it as
                    // a length-zero Array (`[]`), ignoring named properties.
                    || idx == self.arr_proto
                    || self.proto_of.contains_key(&idx)
                    || map.class.is_some()
                    || map.is_ctor
                    || map.is_raw_json
                    || map.pos("toJSON").is_some()
                    || self.module_namespaces.contains_key(&idx)
                    || self.deferred_ns_state.contains_key(&idx)
                {
                    return false;
                }
                active.push(idx);
                if json_push_char_bounded(out, '{').is_err() {
                    active.pop();
                    return false;
                }
                let mut any = false;
                for i in 0..map.keys.len() {
                    let key = &map.keys[i];
                    if is_hidden_key(key) || !map.attr_at(i).enumerable {
                        continue;
                    }
                    // Integer keys need spec reordering; accessors can run user
                    // code. Both are clean declines before any visible result.
                    if map.attr_at(i).accessor || canonical_index_str(key).is_some() {
                        active.pop();
                        return false;
                    }
                    if any && json_push_char_bounded(out, ',').is_err() {
                        active.pop();
                        return false;
                    }
                    if json_quote_bounded(out, key).is_err()
                        || json_push_char_bounded(out, ':').is_err()
                    {
                        active.pop();
                        return false;
                    }
                    if !self.json_plain_value_into(map.val_at(i), depth + 1, active, out) {
                        active.pop();
                        return false;
                    }
                    any = true;
                }
                if json_push_char_bounded(out, '}').is_err() {
                    active.pop();
                    return false;
                }
                active.pop();
                true
            }
            _ => false,
        }
    }

    /// Guarded whole-tree execution for the exact Tier-C [`JsonWalkPlan`].
    ///
    /// The bytecode shape has no observable work except numeric global updates
    /// and reads from the visited tree.  We therefore accumulate privately,
    /// validate the *entire* graph, and commit only after traversal succeeds.
    /// Any getter/proxy/custom prototype/sparse element/cycle/unsupported leaf
    /// returns `None` with zero visible effects, so the ordinary native body can
    /// execute from instruction 0. Aliases are intentionally revisited: that is
    /// what the recursive JavaScript function does.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn json_walk_reduce(
        &mut self,
        plan: crate::codegen::JsonWalkPlan,
        callee: Value,
        root: Value,
    ) -> Option<u64> {
        // The three typeof comparison constants are heap-resolved after the JIT
        // plan was built. Validate their live contents here rather than trusting
        // source text or constant-pool positions.
        let nc = Value::from_bits(plan.number_bits);
        let sc = Value::from_bits(plan.string_bits);
        let bc = Value::from_bits(plan.boolean_bits);
        let const_is = |vm: &Self, v: Value, want: &str| {
            v.is_heap()
                && vm
                    .heap
                    .str_cow(v.heap_index())
                    .is_some_and(|s| s.as_ref() == want)
        };
        if !const_is(self, nc, "number")
            || !const_is(self, sc, "string")
            || !const_is(self, bc, "boolean")
            || self.globals.get(plan.self_global as usize).copied()? != callee
        {
            return None;
        }

        let slots = [
            plan.nodes,
            plan.nulls,
            plan.sum2x,
            plan.strings,
            plan.string_len,
            plan.bools,
        ];
        let mut acc = [0.0; 6];
        for (i, &g) in slots.iter().enumerate() {
            let v = *self.globals.get(g as usize)?;
            if !v.is_number() {
                return None; // would run ToNumeric / `+` coercion in JS
            }
            acc[i] = v.as_f64();
        }

        // Every ordinary object in the admitted graph inherits from the default
        // Object prototype. `for-in` would also visit inherited enumerable keys;
        // admit the direct own-slot walk only while that whole default tail is
        // the usual terminal, barren object.
        let object_proto_barren = self.obj_proto != 0
            && !self.proto_of.contains_key(&self.obj_proto)
            && matches!(self.heap.get(self.obj_proto), HeapObj::Object(m)
                if m.keys.iter().enumerate().all(|(i, k)|
                    is_hidden_key(k) || !m.attr_at(i).enumerable));

        // `(value, exit, depth)` implements DFS without Rust recursion. Exit markers
        // keep only the current ancestry in `active`, detecting cycles while
        // allowing shared subtrees to be counted once per incoming edge. Very
        // deep inputs deliberately decline: completing them without JS frames
        // could otherwise hide the recursive body's observable RangeError.
        const MAX_REDUCED_DEPTH: usize = 256;
        // The ordinary recursive body consumes one VM frame per tree level.
        // Leave enough headroom for the deepest tree this reducer admits; when
        // a caller has already filled the stack, completing iteratively here
        // would incorrectly suppress the ordinary path's RangeError.
        if self.frames.len().saturating_add(MAX_REDUCED_DEPTH) >= MAX_FRAMES {
            return None;
        }
        let mut work: Vec<(Value, bool, usize)> = vec![(root, false, 0)];
        let mut active = rustc_hash::FxHashSet::<u32>::default();
        while let Some((v, exit, depth)) = work.pop() {
            if exit {
                active.remove(&v.heap_index());
                continue;
            }
            if depth >= MAX_REDUCED_DEPTH {
                return None;
            }
            // `nodes++` precedes every branch in the JavaScript body.
            acc[0] += 1.0;
            if v.is_null() {
                acc[1] += 1.0;
                continue;
            }
            if v.is_number() {
                // Keep the exact left-to-right f64 operation grouping:
                // `numSum2x = numSum2x + (v * 2)`.
                acc[2] += v.as_f64() * 2.0;
                continue;
            }
            if v.is_bool() {
                acc[5] += 1.0;
                continue;
            }
            if !v.is_heap() {
                return None;
            }
            let idx = v.heap_index();
            if let Some(n) = self.heap.str_units(idx) {
                acc[3] += 1.0;
                acc[4] += n as f64;
                continue;
            }
            if !active.insert(idx) {
                return None;
            }
            work.push((v, true, depth));
            match self.heap.get(idx) {
                HeapObj::Array(items) => {
                    // The source loop reads the live `length` then every index.
                    // Dense, non-overlaid, ordinary Arrays make those reads pure
                    // and identical to the backing vector. Named own properties
                    // are irrelevant because the array branch never for-ins.
                    if self.proto_of.contains_key(&idx)
                        || self.arguments_objs.contains_key(&idx)
                        || self.array_elements_overlaid(idx)
                        || self.array_js_len.contains_key(&idx)
                        || items.iter().any(|v| v.is_hole())
                    {
                        return None;
                    }
                    for &child in items.iter().rev() {
                        work.push((child, false, depth + 1));
                    }
                }
                HeapObj::Object(map) => {
                    if !object_proto_barren
                        || idx == self.global_this
                        || self.proto_of.contains_key(&idx)
                        || map.class.is_some()
                        || map.is_raw_json
                        || self.module_namespaces.contains_key(&idx)
                        || self.deferred_ns_state.contains_key(&idx)
                    {
                        return None;
                    }
                    // With no integer-index key, insertion order is exactly
                    // for-in's own-key order. Rejecting that rare shape avoids a
                    // per-object sort/allocation and preserves floating-add order.
                    for i in (0..map.keys.len()).rev() {
                        let key = &map.keys[i];
                        if is_hidden_key(key) || !map.attr_at(i).enumerable {
                            continue;
                        }
                        if map.attr_at(i).accessor || canonical_index_str(key).is_some() {
                            return None;
                        }
                        work.push((map.val_at(i), false, depth + 1));
                    }
                }
                _ => return None,
            }
        }

        for (i, &g) in slots.iter().enumerate() {
            // This replaces the Tier-C body's ordinary bytecode store. Do not
            // bump `global_gens`: generated StoreGlobalResolved does not either.
            self.globals[g as usize] = Value::num(acc[i]);
        }
        Some(Value::UNDEFINED.bits())
    }

    /// Evaluate a `Math.<fn>` call over `argc` argument registers (coerced to
    /// numbers). Mirrors JS semantics where they differ from Rust's f64 methods:
    /// `round` is half-up (so −2.5 → −2, not −3); `sign` preserves ±0 and maps
    /// NaN→NaN; `min`/`max` are NaN-sticky (any NaN arg ⇒ NaN).
    pub(crate) fn eval_math(
        &mut self,
        op: crate::bytecode::MathFn,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Result<f64, Thrown> {
        // Snapshot the argument registers FIRST (a ToNumber coercion below may run a
        // user valueOf that re-enters the VM and pushes registers), then delegate to
        // the shared value-form evaluator, which ToNumber-coerces each argument.
        //
        // The snapshot used to be a `Vec`, allocated and freed on EVERY fused
        // `Math.*` -- and four of the thirteen benchmark rows call `Math.imul`
        // once per element in their mixing functions.
        self.with_argv(base, arg_base, argc, |vm, args| vm.eval_math_args(op, args))
    }

    /// `Math.<op>` reduced to a single f64 result (used by the `MathSpread`
    /// fallback for an unusual non-variadic spread like `Math.abs(...arr)`).
    pub(crate) fn eval_math_one(&self, op: crate::bytecode::MathFn, x: f64) -> f64 {
        math_unary(op, x)
    }

    /// Evaluate a Math method over an argument SLICE (the value-form `Math.abs`
    /// invoked as a native), mirroring `eval_math`'s register-based variant.
    pub(crate) fn eval_math_args(
        &mut self,
        op: crate::bytecode::MathFn,
        args: &[Value],
    ) -> Result<f64, Thrown> {
        use crate::bytecode::MathFn as M;
        let at = |args: &[Value], i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        Ok(match op {
            M::Min | M::Max | M::Hypot => {
                // ToNumber EVERY argument (observable valueOf/toString, left-to-right)
                // before reducing.
                let mut nums = Vec::with_capacity(args.len());
                for &v in args {
                    nums.push(self.to_number_coerce(v)?);
                }
                let mut acc = match op {
                    M::Min => f64::INFINITY,
                    M::Max => f64::NEG_INFINITY,
                    _ => 0.0,
                };
                let mut hypot_inf = false;
                for v in nums {
                    acc = match op {
                        // f64 min/max treat -0 and +0 as equal; spec orders -0 < +0,
                        // so tie-break on the sign (Min prefers -0, Max prefers +0).
                        M::Min => {
                            if v.is_nan() || acc.is_nan() {
                                f64::NAN
                            } else if v == acc {
                                if v.is_sign_negative() {
                                    v
                                } else {
                                    acc
                                }
                            } else {
                                acc.min(v)
                            }
                        }
                        M::Max => {
                            if v.is_nan() || acc.is_nan() {
                                f64::NAN
                            } else if v == acc {
                                if v.is_sign_positive() {
                                    v
                                } else {
                                    acc
                                }
                            } else {
                                acc.max(v)
                            }
                        }
                        _ => {
                            // Math.hypot: a ±Infinity argument forces +Infinity even
                            // when another argument is NaN (spec step 3).
                            if v.is_infinite() {
                                hypot_inf = true;
                            }
                            acc + v * v
                        }
                    };
                }
                if matches!(op, M::Hypot) {
                    if hypot_inf {
                        f64::INFINITY
                    } else {
                        acc.sqrt()
                    }
                } else {
                    acc
                }
            }
            // The two-arg ops coerce arg0 then arg1 (ToNumber, left-to-right).
            M::Pow => {
                let a = self.to_number_coerce(at(args, 0))?;
                let b = self.to_number_coerce(at(args, 1))?;
                // Spec: base of magnitude 1 with a NaN/±Infinity exponent is NaN
                // (C/Rust powf returns 1 for these — a deliberate deviation).
                if (a == 1.0 || a == -1.0) && (b.is_nan() || b.is_infinite()) {
                    f64::NAN
                } else {
                    a.powf(b)
                }
            }
            M::Atan2 => {
                let a = self.to_number_coerce(at(args, 0))?;
                let b = self.to_number_coerce(at(args, 1))?;
                a.atan2(b)
            }
            M::Imul => {
                let a = self.to_number_coerce(at(args, 0))?;
                let b = self.to_number_coerce(at(args, 1))?;
                (to_uint32(a).wrapping_mul(to_uint32(b)) as i32) as f64
            }
            _ => {
                let x = self.to_number_coerce(at(args, 0))?;
                math_unary(op, x)
            }
        })
    }

    /// The per-level indent string for `JSON.stringify`'s `space` argument: a
    /// number → that many spaces (clamped 0..10); a string → its first 10 chars;
    /// anything else → empty (compact output).
    /// JSON.stringify `space` coercion (spec sec-json.stringify step 5): a Number
    /// wrapper object is read as ToNumber(space) and a String wrapper as
    /// ToString(space) — both honouring an overridden `valueOf`/`toString` (so
    /// `new Number(1)` with `valueOf:()=>3` indents by 3, and a throwing `valueOf`
    /// propagates). Everything else passes through unchanged to `json_indent`.
    pub(crate) fn json_coerce_space(&mut self, space: Value) -> Result<Value, Thrown> {
        if !space.is_heap() {
            return Ok(space);
        }
        match self.heap.get(space.heap_index()) {
            HeapObj::Boxed { kind: 1, .. } => {
                // ToPrimitive(space, number) honouring overrides, then ToNumber.
                let prim = if let Some(p) = self.symbol_to_primitive(space, "number")? {
                    p
                } else {
                    let mut found = None;
                    for name in ["valueOf", "toString"] {
                        let f = self.get_prop(space, name)?;
                        if self.is_callable(f) {
                            let r = self.call_value(f, space, &[])?;
                            if !self.is_object_value(r) {
                                found = Some(r);
                                break;
                            }
                        }
                    }
                    found.ok_or_else(|| {
                        Thrown("TypeError: Cannot convert object to primitive value".into())
                    })?
                };
                Ok(Value::num(self.to_number(prim)?))
            }
            HeapObj::Boxed { kind: 0, .. } => {
                let s = self.to_js_string(space)?;
                Ok(self.alloc_str(s))
            }
            _ => Ok(space),
        }
    }

    pub(crate) fn json_indent(&self, space: Value) -> String {
        if space.is_number() {
            let n = space.as_f64();
            let n = if n.is_finite() && n > 0.0 {
                (n as usize).min(10)
            } else {
                0
            };
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

    /// Resolve `JSON.stringify`'s second argument into either a function
    /// replacer or a property allowlist. A callable is the function form; an
    /// Array is a PropertyList — its String / Number (and boxed String/Number)
    /// entries become allowed keys, ToString-coerced and deduplicated in order.
    pub(crate) fn json_resolve_replacer(
        &mut self,
        replacer: Value,
    ) -> Result<(Value, Option<Vec<String>>), Thrown> {
        if self.is_callable(replacer) {
            return Ok((replacer, None));
        }
        // IsArray on a revoked Proxy is a TypeError (value_is_array approximates it
        // as false, so check explicitly before the PropertyList branch).
        if replacer.is_heap() {
            if let HeapObj::Proxy { revoked: true, .. } = self.heap.get(replacer.heap_index()) {
                return Err(Thrown(
                    "TypeError: Cannot perform IsArray on a revoked Proxy".into(),
                ));
            }
        }
        // An array (or Proxy-wrapping-array) replacer is a PropertyList: read its
        // length + each element via REAL [[Get]] (a revoked/throwing proxy throws),
        // keeping only string / number / (String|Number)-object items, deduped, in
        // order. A non-array object replacer is ignored (no filter).
        if replacer.is_heap() && self.value_is_array(replacer) {
            let lenv = self.get_prop(replacer, "length")?;
            let lenf = self.to_number_coerce(lenv)?;
            let len: u64 = if lenf.is_nan() || lenf <= 0.0 {
                0
            } else {
                lenf.min(9007199254740991.0) as u64
            };
            // This loop is one native VM instruction. A sparse/virtual length
            // must therefore be bounded explicitly; bytecode step metering
            // cannot interrupt its billions of absent-index probes.
            #[cfg(feature = "safe-sandbox")]
            if len > MAX_EAGER_ITER_RESULT as u64 {
                return Err(Thrown(
                    "RangeError: JSON replacer array exceeds the sandbox iteration limit".into(),
                ));
            }

            // Record each first-seen key's insertion position. Moving the map's
            // owned strings into a Vec afterwards preserves PropertyList order
            // without the old quadratic `Vec::contains` scan or a second clone
            // of every unique key. This storage is private Rust memory rather
            // than a HeapObj, so explicitly account its bucket and String
            // capacities and use fallible growth throughout.
            let snapshot_budget = JsonKeySnapshotBudget::default();
            let mut unique: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            let mut charged_map_capacity = 0usize;
            let mut k: u64 = 0;
            while k < len {
                let it = self.get_index(replacer, Value::num(k as f64))?;
                let item = if it.is_number() {
                    Some(self.to_js_string(it)?)
                } else if it.is_heap() {
                    match self.heap.get(it.heap_index()) {
                        HeapObj::Str(_) | HeapObj::Cons { .. } => {
                            self.heap.str_cow(it.heap_index()).map(|s| s.into_owned())
                        }
                        HeapObj::Boxed { kind, .. } if *kind == 0 || *kind == 1 => {
                            Some(self.to_js_string(it)?)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(s) = item {
                    // Count every temporary conversion, including duplicates
                    // that are dropped. Otherwise `[huge, huge, ...]` can
                    // allocate and free enormous cumulative storage inside one
                    // native instruction while retaining only one list entry.
                    self.json_charge_key_snapshot(&snapshot_budget, s.capacity(), 0)?;
                    if !unique.contains_key(&s) {
                        // HashMap growth can roughly double the bucket count.
                        // Check that upper bound before asking the allocator,
                        // then charge the actual capacity it returned.
                        let current_capacity = unique.capacity();
                        let predicted_capacity = if unique.len() < current_capacity {
                            current_capacity
                        } else {
                            current_capacity.max(2).saturating_mul(2)
                        };
                        let predicted_table_growth = predicted_capacity
                            .saturating_sub(charged_map_capacity)
                            .checked_mul(JSON_PROPERTY_MAP_BUCKET_BYTES)
                            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
                        self.json_preflight_key_snapshot(
                            &snapshot_budget,
                            predicted_table_growth,
                            0,
                        )?;

                        unique
                            .try_reserve(1)
                            .map_err(|_| self.json_key_snapshot_allocation_error())?;
                        let actual_capacity = unique.capacity();
                        if actual_capacity > charged_map_capacity {
                            let table_growth = (actual_capacity - charged_map_capacity)
                                .checked_mul(JSON_PROPERTY_MAP_BUCKET_BYTES)
                                .ok_or_else(|| self.json_key_snapshot_limit_error())?;
                            self.json_charge_key_snapshot(&snapshot_budget, table_growth, 0)?;
                            charged_map_capacity = actual_capacity;
                        }
                        let next = unique.len();
                        unique.insert(s, next);
                    }
                }
                k += 1;
            }
            let requested_list_bytes = unique
                .len()
                .checked_mul(std::mem::size_of::<String>())
                .ok_or_else(|| self.json_key_snapshot_limit_error())?;
            self.json_preflight_key_snapshot(&snapshot_budget, requested_list_bytes, 0)?;
            let mut list = Vec::new();
            list.try_reserve_exact(unique.len())
                .map_err(|_| self.json_key_snapshot_allocation_error())?;
            let list_bytes = list
                .capacity()
                .checked_mul(std::mem::size_of::<String>())
                .ok_or_else(|| self.json_key_snapshot_limit_error())?;
            self.json_charge_key_snapshot(&snapshot_budget, list_bytes, 0)?;
            list.resize_with(unique.len(), String::new);
            for (key, position) in unique {
                list[position] = key;
            }
            return Ok((Value::UNDEFINED, Some(list)));
        }
        Ok((Value::UNDEFINED, None))
    }

    /// Serialize `v` to JSON (`None` ⇒ omit: undefined / function). `indent` is
    /// the per-level pad (empty ⇒ compact); `depth` is the current nesting.
    /// `holder` is the object/array `key` lives on (the `this` for a function
    /// `replacer`); `replacer` is a callable or undefined; `allowlist`, when
    /// `Some`, restricts which object keys are emitted (the array-replacer form).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn json_value(
        &mut self,
        holder: Value,
        key: &str,
        v: Value,
        indent: &str,
        depth: usize,
        visited: &mut Vec<u32>,
        replacer: Value,
        allowlist: Option<&[String]>,
    ) -> Result<Option<String>, Thrown> {
        let mut out = String::new();
        let snapshot_budget = JsonKeySnapshotBudget::default();
        if let Some(list) = allowlist {
            let vector_bytes = list
                .len()
                .checked_mul(std::mem::size_of::<String>())
                .ok_or_else(|| self.json_key_snapshot_limit_error())?;
            let retained_bytes = list
                .iter()
                .try_fold(vector_bytes, |total, key| total.checked_add(key.capacity()));
            self.json_charge_key_snapshot(
                &snapshot_budget,
                retained_bytes.ok_or_else(|| self.json_key_snapshot_limit_error())?,
                0,
            )?;
        }
        if self.json_value_into_with_budget(
            holder,
            key,
            v,
            indent,
            depth,
            visited,
            replacer,
            allowlist,
            &mut out,
            &snapshot_budget,
        )? {
            Ok(Some(out))
        } else {
            Ok(None)
        }
    }

    /// Does either default prototype (Object.prototype / Array.prototype) carry a
    /// callable `toJSON`? Cached on the two protos' shape VERSIONS, so any mutation
    /// that adds/removes `toJSON` there bumps a version and auto-invalidates the
    /// cache (no manual invalidation). Used by `json_value_into_with_budget` to skip the
    /// per-value `toJSON` probe for plain objects/arrays. `false` ⇒ provably safe
    /// to skip the probe for a plain value with no own `toJSON`.
    fn json_default_protos_have_tojson(&mut self) -> bool {
        let ov = self.heap.version_of(self.obj_proto);
        let av = self.heap.version_of(self.arr_proto);
        if let Some((co, ca, r)) = self.json_default_tj {
            if co == ov && ca == av {
                return r;
            }
        }
        let has = |vm: &mut Self, proto: u32| -> bool {
            if proto == 0 {
                return false;
            }
            let tj = vm
                .get_prop(Value::heap(proto), "toJSON")
                .unwrap_or(Value::UNDEFINED);
            vm.is_callable(tj)
        };
        let r = has(self, self.obj_proto) || has(self, self.arr_proto);
        self.json_default_tj = Some((ov, av, r));
        r
    }

    /// Is `idx` a PLAIN object/array whose only possible `toJSON` would be on a
    /// default prototype? (no custom proto, not a class instance / raw-json, no
    /// own `toJSON`, no `arr_props` overlay for arrays). When true AND
    /// `!json_default_protos_have_tojson()`, `get_prop(v,"toJSON")` is provably
    /// `undefined` and the serializer skips the chain-walking probe entirely.
    fn json_plain_no_own_tojson(&self, idx: u32) -> bool {
        if self.proto_of.contains_key(&idx) {
            return false;
        }
        match self.heap.get(idx) {
            HeapObj::Object(map) => {
                map.class.is_none() && !map.is_raw_json && map.pos("toJSON").is_none()
            }
            HeapObj::Array(_) => !self.arr_props.contains_key(&idx),
            _ => false,
        }
    }

    /// The same key set as `json_object_keys_fast`, but as map SLOTS — no key
    /// clone, no later `pos()` re-lookup. The second element is true when every
    /// selected slot is a PRIMITIVE data property (no accessor, no heap value):
    /// serializing those (with no replacer) runs NO user code, so the map
    /// provably cannot mutate mid-walk and the slots stay exact for the whole
    /// loop. Otherwise the caller must snapshot the key texts before the first
    /// recursion, exactly like the cloning path.
    fn json_object_slots_fast(
        &mut self,
        idx: u32,
        snapshot_budget: &JsonKeySnapshotBudget,
        output_capacity: usize,
    ) -> Result<Option<(Vec<usize>, bool)>, Thrown> {
        if idx == self.global_this
            || self.module_namespaces.contains_key(&idx)
            || self.deferred_ns_state.contains_key(&idx)
        {
            return Ok(None);
        }
        let key_count = match self.heap.get(idx) {
            HeapObj::Object(map) => map.keys.len(),
            _ => return Ok(None),
        };
        // Peak working storage is the integer/rest partition plus the returned
        // order vector: at most `(u32, usize) + usize` per source key.
        let order_working_bytes = key_count
            .checked_mul(std::mem::size_of::<(u32, usize)>() + std::mem::size_of::<usize>())
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        self.json_preflight_key_snapshot(snapshot_budget, order_working_bytes, output_capacity)?;
        let ordered = match self.heap.get(idx) {
            HeapObj::Object(map) => json_spec_key_order_bounded(&map.keys),
            _ => return Ok(None),
        };
        let (mut slots, allocated_bytes) =
            ordered.map_err(|_| self.json_key_snapshot_allocation_error())?;
        self.json_charge_key_snapshot(snapshot_budget, allocated_bytes, output_capacity)?;
        let all_prim = match self.heap.get(idx) {
            HeapObj::Object(map) => {
                slots.retain(|&i| map.attr_at(i).enumerable && !is_hidden_key(&map.keys[i]));
                slots
                    .iter()
                    .all(|&i| !map.attr_at(i).accessor && !map.val_at(i).is_heap())
            }
            _ => return Ok(None),
        };
        Ok(Some((slots, all_prim)))
    }

    fn json_clone_object_keys(
        &mut self,
        idx: u32,
        slots: &[usize],
        snapshot_budget: &JsonKeySnapshotBudget,
        output_capacity: usize,
    ) -> Result<Vec<String>, Thrown> {
        let requested = slots
            .len()
            .checked_mul(std::mem::size_of::<String>())
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        self.json_preflight_key_snapshot(snapshot_budget, requested, output_capacity)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(slots.len())
            .map_err(|_| self.json_key_snapshot_allocation_error())?;
        let vector_bytes = keys
            .capacity()
            .checked_mul(std::mem::size_of::<String>())
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        self.json_charge_key_snapshot(snapshot_budget, vector_bytes, output_capacity)?;

        for &slot in slots {
            let key_len = match self.heap.get(idx) {
                HeapObj::Object(map) => map.keys[slot].len(),
                _ => 0,
            };
            self.json_preflight_key_snapshot(snapshot_budget, key_len, output_capacity)?;
            let mut key = String::new();
            key.try_reserve_exact(key_len)
                .map_err(|_| self.json_key_snapshot_allocation_error())?;
            self.json_charge_key_snapshot(snapshot_budget, key.capacity(), output_capacity)?;
            if let HeapObj::Object(map) = self.heap.get(idx) {
                key.push_str(&map.keys[slot]);
            }
            keys.push(key);
        }
        Ok(keys)
    }

    fn json_clone_object_slot_keys(
        &mut self,
        idx: u32,
        slots: &[usize],
        snapshot_budget: &JsonKeySnapshotBudget,
        output_capacity: usize,
    ) -> Result<Vec<(usize, String)>, Thrown> {
        let requested = slots
            .len()
            .checked_mul(std::mem::size_of::<(usize, String)>())
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        self.json_preflight_key_snapshot(snapshot_budget, requested, output_capacity)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(slots.len())
            .map_err(|_| self.json_key_snapshot_allocation_error())?;
        let vector_bytes = keys
            .capacity()
            .checked_mul(std::mem::size_of::<(usize, String)>())
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        self.json_charge_key_snapshot(snapshot_budget, vector_bytes, output_capacity)?;

        for &slot in slots {
            let key_len = match self.heap.get(idx) {
                HeapObj::Object(map) => map.keys[slot].len(),
                _ => 0,
            };
            self.json_preflight_key_snapshot(snapshot_budget, key_len, output_capacity)?;
            let mut key = String::new();
            key.try_reserve_exact(key_len)
                .map_err(|_| self.json_key_snapshot_allocation_error())?;
            self.json_charge_key_snapshot(snapshot_budget, key.capacity(), output_capacity)?;
            if let HeapObj::Object(map) = self.heap.get(idx) {
                key.push_str(&map.keys[slot]);
            }
            keys.push((slot, key));
        }
        Ok(keys)
    }

    fn json_clone_heap_key_values(
        &mut self,
        key_array: Value,
        snapshot_budget: &JsonKeySnapshotBudget,
        output_capacity: usize,
    ) -> Result<Vec<String>, Thrown> {
        let value_count = match self.heap.get(key_array.heap_index()) {
            HeapObj::Array(items) => items.len(),
            _ => 0,
        };
        // `object_enum_own` already accounts the heap Array and its Values. Read
        // one copied Value at a time so converting a key never requires a second
        // unmetered clone of that whole array.
        let requested = value_count
            .checked_mul(std::mem::size_of::<String>())
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        self.json_preflight_key_snapshot(snapshot_budget, requested, output_capacity)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(value_count)
            .map_err(|_| self.json_key_snapshot_allocation_error())?;
        let vector_bytes = keys
            .capacity()
            .checked_mul(std::mem::size_of::<String>())
            .ok_or_else(|| self.json_key_snapshot_limit_error())?;
        self.json_charge_key_snapshot(snapshot_budget, vector_bytes, output_capacity)?;
        for position in 0..value_count {
            let value = match self.heap.get(key_array.heap_index()) {
                HeapObj::Array(items) => items[position],
                _ => Value::UNDEFINED,
            };
            let key = self.to_js_string(value)?;
            self.json_charge_key_snapshot(snapshot_budget, key.capacity(), output_capacity)?;
            keys.push(key);
        }
        Ok(keys)
    }

    /// SerializeJSONProperty, appending straight into a single shared output
    /// buffer instead of building a per-node `String`/`Vec<String>` tree and
    /// joining at every level (the V8 approach). Returns `true` if a value was
    /// written, `false` if the property is OMITTED (undefined / function / symbol
    /// / a replacer-undefined) — for an object the caller rolls the buffer back to
    /// the entry start; for an array the caller writes `null`. Byte-for-byte
    /// identical to the old `Vec<String>` + `wrap_json` path (same escaping,
    /// indent layout, key order, toJSON/replacer/allowlist/cycle/raw-JSON rules).
    #[allow(clippy::too_many_arguments)]
    fn json_value_into_with_budget(
        &mut self,
        holder: Value,
        key: &str,
        v: Value,
        indent: &str,
        depth: usize,
        visited: &mut Vec<u32>,
        replacer: Value,
        allowlist: Option<&[String]>,
        out: &mut String,
        snapshot_budget: &JsonKeySnapshotBudget,
    ) -> Result<bool, Thrown> {
        // SerializeJSONProperty: a value with a callable `toJSON` is replaced by
        // `value.toJSON(key)` before serialization (Date, user objects, …).
        // FAST PATH (T0.1): a plain object/array with no own `toJSON` whose
        // default prototypes carry no `toJSON` provably has no callable `toJSON`,
        // so skip the per-value `get_prop(v,"toJSON")` prototype-chain walk
        // (~900k walks on the json bench). The version-keyed cache stays correct
        // if user code mutates a default prototype mid-serialization.
        let v = if v.is_heap() {
            let idx = v.heap_index();
            if self.json_plain_no_own_tojson(idx) && !self.json_default_protos_have_tojson() {
                v
            } else {
                let tj = self.get_prop(v, "toJSON")?;
                if self.is_callable(tj) {
                    let kv = self.alloc_str(key.to_string());
                    self.call_value(tj, v, &[kv])?
                } else {
                    v
                }
            }
        } else {
            v
        };
        // A function `replacer` is applied after `toJSON`: replacer(key, value)
        // with `this` = the holder. Its result is what gets serialized.
        let v = if self.is_callable(replacer) {
            let kv = self.alloc_str(key.to_string());
            self.call_value(replacer, holder, &[kv, v])?
        } else {
            v
        };
        if v.is_undefined() {
            return Ok(false);
        }
        if v.is_null() {
            self.json_push_str_output(out, "null")?;
            return Ok(true);
        }
        if v.is_bool() {
            self.json_push_str_output(out, if v.as_bool() { "true" } else { "false" })?;
            return Ok(true);
        }
        if v.is_number() {
            self.json_push_number_output(out, v.as_f64())?;
            return Ok(true);
        }
        if !v.is_heap() {
            return Ok(false);
        }
        let idx = v.heap_index();
        // Leaf / primitive-wrapper cases (no recursion into properties).
        match self.heap.get(idx) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => {
                // EXACT bytes: a lone surrogate must emit its \udXXX escape
                // (well-formed JSON.stringify), not a U+FFFD substitution.
                // A flat string's Cow is Borrowed — quote straight from it
                // (`out` is a separate buffer, so the heap borrow is free);
                // only a rope materializes.
                self.json_quote_heap_string_output(out, idx)?;
                return Ok(true);
            }
            HeapObj::Func(_)
            | HeapObj::Closure { .. }
            | HeapObj::Bound { .. }
            | HeapObj::Native(_)
            | HeapObj::NativeClosure { .. }
            | HeapObj::Symbol { .. } => return Ok(false),
            HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => {
                return Err(Thrown(
                    "TypeError: Do not know how to serialize a BigInt".into(),
                ))
            }
            // A boxed primitive serializes as ToString / ToNumber / its boolean —
            // observably invoking the wrapper's toString/valueOf (which may throw).
            HeapObj::Boxed { kind: 0, .. } => {
                let s = self.to_js_string(v)?;
                self.json_quote_output(out, &s)?;
                return Ok(true);
            }
            HeapObj::Boxed { kind: 1, .. } => {
                // ToNumber(wrapper): ToPrimitive(number) so an overridden
                // valueOf/@@toPrimitive fires (to_number_coerce reads [[NumberData]]).
                let prim = self.to_primitive_number(v)?;
                let n = self.to_number(prim)?;
                self.json_push_number_output(out, n)?;
                return Ok(true);
            }
            HeapObj::Boxed { kind: 2, value } => {
                let b = self.truthy(*value);
                self.json_push_str_output(out, if b { "true" } else { "false" })?;
                return Ok(true);
            }
            // A boxed BigInt (Object(0n)) throws like a primitive BigInt; a boxed
            // Symbol falls through to SerializeJSONObject ("{}").
            HeapObj::Boxed { value, .. } => {
                if value.is_heap()
                    && matches!(
                        self.heap.get(value.heap_index()),
                        HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
                    )
                {
                    return Err(Thrown(
                        "TypeError: Do not know how to serialize a BigInt".into(),
                    ));
                }
            }
            HeapObj::Object(map) if map.is_raw_json => {
                // [[IsRawJSON]]: emit the stored "rawJSON" text verbatim.
                let raw_val = map.get("rawJSON").unwrap_or(Value::UNDEFINED);
                let s = self
                    .heap
                    .str_cow(raw_val.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                self.json_push_str_output(out, &s)?;
                return Ok(true);
            }
            _ => {}
        }
        // SerializeJSONArray / SerializeJSONObject. Both read properties via REAL
        // [[Get]] (so getters / Proxy traps fire and abrupt completions propagate),
        // and detect cycles via `visited`. The PropertyList allowlist is GLOBAL — it
        // filters object keys at EVERY nesting level, including objects inside arrays.
        if visited.contains(&idx) {
            return Err(Thrown(
                "TypeError: Converting circular structure to JSON".into(),
            ));
        }
        #[cfg(feature = "safe-sandbox")]
        if depth >= MAX_JSON_NESTING_DEPTH {
            return Err(Thrown(
                "RangeError: JSON nesting depth exceeds the sandbox limit".into(),
            ));
        }
        visited.push(idx);
        let pad = if indent.is_empty() {
            String::new()
        } else {
            json_repeat_bounded(indent, depth + 1).map_err(|_| self.json_output_error())?
        };
        let pad_close = if indent.is_empty() {
            String::new()
        } else {
            json_repeat_bounded(indent, depth).map_err(|_| self.json_output_error())?
        };
        if self.value_is_array(v) {
            // len = ToLength(Get(val, "length"))
            let lenv = self.get_prop(v, "length")?;
            let lenf = self.to_number_coerce(lenv)?;
            let len: u64 = if lenf.is_nan() || lenf <= 0.0 {
                0
            } else {
                lenf.min(9007199254740991.0) as u64
            };
            // Like replacer-list construction, SerializeJSONArray runs inside
            // one native instruction. Cap virtual/sparse lengths before the
            // uninterruptible index loop; the output-size bound is too late to
            // help an array whose elements stringify to tiny values.
            #[cfg(feature = "safe-sandbox")]
            if len > MAX_EAGER_ITER_RESULT as u64 {
                visited.pop();
                return Err(Thrown(
                    "RangeError: JSON array exceeds the sandbox iteration limit".into(),
                ));
            }
            self.json_push_char_output(out, '[')?;
            let mut i: u64 = 0;
            while i < len {
                if i > 0 {
                    self.json_push_char_output(out, ',')?;
                }
                if !indent.is_empty() {
                    self.json_push_line_pad_output(out, &pad)?;
                }
                // FAST PATH (T0.1): for a dense in-range element of an array with
                // no `arr_props` overlay, read `items[i]` directly — skipping the
                // `json_get` generic-index dispatch (string-key coercion + chain
                // resolution) per element. Any overlay / virtual-length / OOB falls
                // back to `json_get` (which handles holes/proto exactly).
                let direct = if !self.array_elements_overlaid(idx) {
                    match self.heap.get(idx) {
                        // A present (non-hole) dense element. A HOLE falls back to
                        // `json_get` so the prototype chain is walked exactly.
                        HeapObj::Array(a) => match a.get(i as usize) {
                            Some(e) if !e.is_hole() => Some(*e),
                            _ => None,
                        },
                        _ => None,
                    }
                } else {
                    None
                };
                let e = match direct {
                    Some(e) => e,
                    None => match self.json_get(v, &i.to_string()) {
                        Ok(e) => e,
                        Err(e) => {
                            visited.pop();
                            return Err(e);
                        }
                    },
                };
                // The element key is only observable if the element has a callable
                // `toJSON` / there is a replacer — defer the `i.to_string()` alloc
                // to that case (a heap element); primitives pass an empty key.
                let ks: String = if e.is_heap() || self.is_callable(replacer) {
                    i.to_string()
                } else {
                    String::new()
                };
                // An omitted array element serializes as `null` (NOT skipped).
                let wrote = match self.json_value_into_with_budget(
                    v,
                    &ks,
                    e,
                    indent,
                    depth + 1,
                    visited,
                    replacer,
                    allowlist,
                    out,
                    snapshot_budget,
                ) {
                    Ok(w) => w,
                    Err(e) => {
                        visited.pop();
                        return Err(e);
                    }
                };
                if !wrote {
                    self.json_push_str_output(out, "null")?;
                }
                i += 1;
            }
            if len > 0 && !indent.is_empty() {
                self.json_push_line_pad_output(out, &pad_close)?;
            }
            self.json_push_char_output(out, ']')?;
        } else {
            // Keep the large object-walk local set out of the recursive
            // `json_value_into` frame. This closure is a separate Rust body;
            // without it, debug/Windows builds reserve object-path temporaries
            // at every level of a deeply nested array and can exhaust the
            // native stack before the default serializer's established depth.
            return (|| -> Result<bool, Thrown> {
                // FAST PATH (leaf emission): with no allowlist and no function
                // replacer, walk a plain object's keys as map SLOTS instead of
                // cloned Strings re-found by `pos()` each iteration. Two tiers:
                // every value a primitive data property ⇒ nothing in the loop can
                // run user code, so the map provably never mutates and keys are
                // quoted straight from the borrow (no clone, no re-lookup, no
                // version check); otherwise the key texts are snapshotted upfront
                // (same clones as the old path — a toJSON/getter may delete them
                // out from under the walk) and only the `pos()` re-lookup is
                // elided, guarded by the map version (a delete shifts slots and
                // bumps it). `ZIPP_NO_JSON_LEAF_FAST=1` restores the cloning walk
                // below.
                let slot_plan = if json_leaf_fast_enabled()
                    && allowlist.is_none()
                    && !self.is_callable(replacer)
                {
                    self.json_object_slots_fast(idx, snapshot_budget, out.capacity())?
                } else {
                    None
                };
                if let Some((slots, all_prim)) = slot_plan {
                    let sep = if indent.is_empty() { ":" } else { ": " };
                    self.json_push_char_output(out, '{')?;
                    let mut any = false;
                    if all_prim {
                        for &slot in &slots {
                            // Tentatively write `[,]\n pad "key"sep`, then the
                            // value; an OMITTED value (undefined) rolls the buffer
                            // back to before this entry — same as the cloning path.
                            let mark = out.len();
                            let val = self.json_push_slot_entry_output(
                                out,
                                idx,
                                slot,
                                any,
                                !indent.is_empty(),
                                &pad,
                                sep,
                            )?;
                            // The key is unobservable for a primitive value with
                            // no replacer (no toJSON probe, no replacer call) —
                            // pass "" like the array path does for primitives.
                            let wrote = match self.json_value_into_with_budget(
                                v,
                                "",
                                val,
                                indent,
                                depth + 1,
                                visited,
                                replacer,
                                allowlist,
                                out,
                                snapshot_budget,
                            ) {
                                Ok(w) => w,
                                Err(e) => {
                                    visited.pop();
                                    return Err(e);
                                }
                            };
                            if wrote {
                                any = true;
                            } else {
                                out.truncate(mark);
                            }
                        }
                    } else {
                        let v0 = self.heap.version_of(idx);
                        let keys = self.json_clone_object_slot_keys(
                            idx,
                            &slots,
                            snapshot_budget,
                            out.capacity(),
                        )?;
                        for (slot, k) in &keys {
                            // Value read at SERIALIZATION time (so a prior key's
                            // toJSON that mutated this one is observed). While the
                            // version is unchanged the snapshot slot IS `pos(&k)`;
                            // after a bump, re-find the key exactly as the cloning
                            // path always did (accessor / deleted ⇒ `json_get`).
                            let direct = if self.heap.version_of(idx) == v0 {
                                match self.heap.get(idx) {
                                    HeapObj::Object(m) if !m.attr_at(*slot).accessor => {
                                        Some(m.val_at(*slot))
                                    }
                                    _ => None,
                                }
                            } else {
                                match self.heap.get(idx) {
                                    HeapObj::Object(m) => match m.pos(k) {
                                        Some(i) if !m.attr_at(i).accessor => Some(m.val_at(i)),
                                        _ => None,
                                    },
                                    _ => None,
                                }
                            };
                            let val = match direct {
                                Some(val) => val,
                                None => match self.json_get(v, k) {
                                    Ok(val) => val,
                                    Err(e) => {
                                        visited.pop();
                                        return Err(e);
                                    }
                                },
                            };
                            let mark = out.len();
                            self.json_push_entry_prefix_output(
                                out,
                                any,
                                !indent.is_empty(),
                                &pad,
                                k,
                                sep,
                            )?;
                            let wrote = match self.json_value_into_with_budget(
                                v,
                                k,
                                val,
                                indent,
                                depth + 1,
                                visited,
                                replacer,
                                allowlist,
                                out,
                                snapshot_budget,
                            ) {
                                Ok(w) => w,
                                Err(e) => {
                                    visited.pop();
                                    return Err(e);
                                }
                            };
                            if wrote {
                                any = true;
                            } else {
                                out.truncate(mark);
                            }
                        }
                    }
                    if any && !indent.is_empty() {
                        self.json_push_line_pad_output(out, &pad_close)?;
                    }
                    self.json_push_char_output(out, '}')?;
                    visited.pop();
                    return Ok(true);
                }
                // EnumerableOwnPropertyNames(val) — or the PropertyList, when given.
                // FAST PATH (T0.5/T0.6): a plain object (not global / namespace) yields
                // its enumerable own string keys as a `Vec<String>` straight from the
                // ObjMap (no heap-Array, no `display()`), and below its DATA values are
                // read directly from the map — eliding `object_enum_own`'s array alloc
                // + per-key display + the per-key `json_get` dispatch. `use_fast` gates
                // both the keys and the value reads together.
                let fast_key_slots = if allowlist.is_none() {
                    self.json_object_slots_fast(idx, snapshot_budget, out.capacity())?
                } else {
                    None
                };
                let use_fast = fast_key_slots.is_some();
                let owned_keys: Vec<String>;
                let keys: &[String] = match (allowlist, fast_key_slots) {
                    (Some(a), _) => a,
                    (None, Some((slots, _))) => {
                        owned_keys = self.json_clone_object_keys(
                            idx,
                            &slots,
                            snapshot_budget,
                            out.capacity(),
                        )?;
                        &owned_keys
                    }
                    (None, None) => {
                        let kv = match self.object_enum_own(v, crate::vm::EnumWhat::Keys) {
                            Ok(kv) => kv,
                            Err(e) => {
                                visited.pop();
                                return Err(e);
                            }
                        };
                        owned_keys =
                            self.json_clone_heap_key_values(kv, snapshot_budget, out.capacity())?;
                        &owned_keys
                    }
                };
                let sep = if indent.is_empty() { ":" } else { ": " };
                self.json_push_char_output(out, '{')?;
                let mut any = false;
                for k in keys {
                    // Value read at SERIALIZATION time (so a prior key's toJSON that
                    // mutated this one is observed). Fast path: a non-accessor own data
                    // slot reads `vals[slot]` directly; an accessor / a key deleted
                    // during recursion / anything else falls back to `json_get` (runs
                    // the getter, walks the prototype, deleted⇒undefined⇒omitted).
                    let direct = if use_fast {
                        match self.heap.get(idx) {
                            HeapObj::Object(m) => match m.pos(&k) {
                                Some(i) if !m.attr_at(i).accessor => Some(m.val_at(i)),
                                _ => None,
                            },
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let val = match direct {
                        Some(val) => val,
                        None => match self.json_get(v, k) {
                            Ok(val) => val,
                            Err(e) => {
                                visited.pop();
                                return Err(e);
                            }
                        },
                    };
                    // Tentatively write `[,]\n pad "key"sep`, then the value; if the
                    // value is OMITTED, roll the buffer back to before this entry (so
                    // an undefined-valued property leaves no trace, incl. its comma).
                    let mark = out.len();
                    self.json_push_entry_prefix_output(out, any, !indent.is_empty(), &pad, k, sep)?;
                    let wrote = match self.json_value_into_with_budget(
                        v,
                        k,
                        val,
                        indent,
                        depth + 1,
                        visited,
                        replacer,
                        allowlist,
                        out,
                        snapshot_budget,
                    ) {
                        Ok(w) => w,
                        Err(e) => {
                            visited.pop();
                            return Err(e);
                        }
                    };
                    if wrote {
                        any = true;
                    } else {
                        out.truncate(mark);
                    }
                }
                if any && !indent.is_empty() {
                    self.json_push_line_pad_output(out, &pad_close)?;
                }
                self.json_push_char_output(out, '}')?;
                visited.pop();
                Ok(true)
            })();
        }
        visited.pop();
        Ok(true)
    }

    /// Parse a JSON string into a Value, or throw SyntaxError. Recursive-descent
    /// over the byte string (structure tokens are ASCII; string content is
    /// flushed as UTF-8 slices). Allocates heap objects/arrays/strings.
    pub(crate) fn json_parse(&mut self, src: &[u8]) -> Result<Value, Thrown> {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::JsonParse);
        // W9 static pretenure (NURSERY_DESIGN.md §4): the parsed tree is the
        // measured pretenure case (B119's oracle: json-large's 48% old-trace
        // share with ~zero old→young stores), so the whole builder allocates
        // OLD. No user code runs inside this scope (the reviver path,
        // `internalize_json`, is deliberately OUTSIDE it), so no GC-visible
        // young value can be created and missed here. Manual begin/end pair —
        // the error path must unwind the depth too.
        self.heap.pretenure_begin();
        let r = self.json_parse_scoped(src);
        self.heap.pretenure_end();
        r
    }

    fn json_parse_scoped(&mut self, src: &[u8]) -> Result<Value, Thrown> {
        let mut i = 0;
        json_skip_ws(src, &mut i);
        let v = self.json_parse_value(src, &mut i, 0)?;
        json_skip_ws(src, &mut i);
        if i != src.len() {
            return Err(Thrown(
                "SyntaxError: Unexpected non-whitespace character after JSON".into(),
            ));
        }
        Ok(v)
    }

    fn json_parse_value(
        &mut self,
        src: &[u8],
        i: &mut usize,
        depth: usize,
    ) -> Result<Value, Thrown> {
        let b = src;
        match b.get(*i).copied() {
            Some(b'{') => self.json_parse_object(src, i, depth),
            Some(b'[') => self.json_parse_array(src, i, depth),
            Some(b'"') => {
                let js = json_parse_string(src, i)?;
                Ok(Value::heap(self.heap.alloc_js(js)))
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

    fn json_parse_array(
        &mut self,
        src: &[u8],
        i: &mut usize,
        depth: usize,
    ) -> Result<Value, Thrown> {
        json_check_parse_depth(depth)?;
        let b = src;
        *i += 1; // '['
        let mut items = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) == Some(&b']') {
            *i += 1;
            return Ok(self.alloc_array_current_realm(items));
        }
        loop {
            json_skip_ws(b, i);
            let v = self.json_parse_value(src, i, depth + 1)?;
            items.push(v);
            json_skip_ws(b, i);
            match b.get(*i) {
                Some(b',') => *i += 1,
                Some(b']') => {
                    *i += 1;
                    break;
                }
                _ => {
                    return Err(Thrown(
                        "SyntaxError: Expected ',' or ']' in JSON array".into(),
                    ))
                }
            }
        }
        Ok(self.alloc_array_current_realm(items))
    }

    fn json_parse_object(
        &mut self,
        src: &[u8],
        i: &mut usize,
        depth: usize,
    ) -> Result<Value, Thrown> {
        json_check_parse_depth(depth)?;
        let b = src;
        *i += 1; // '{'
        let mut pairs: Vec<(String, Value)> = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) != Some(&b'}') {
            loop {
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b'"') {
                    return Err(Thrown(
                        "SyntaxError: Expected property name string in JSON".into(),
                    ));
                }
                // B233: an escape-free member name IS its source bytes, so
                // read it straight into the one `String` the map will own.
                // The general parser reaches that same string through a
                // `Vec<u8>` and a `JsStr` first — three allocations per key
                // where one will do, and object keys are where JSON parsing
                // spends its allocator time (~440,000 of them on the
                // json-large corpus). Anything needing decoding still goes
                // through the parser, which is where the escape,
                // lone-surrogate and error rules live.
                let key = match crate::vm::helpers_json::json_plain_key_enabled()
                    .then(|| crate::vm::helpers_json::json_scan_plain_key(b, i))
                    .flatten()
                {
                    Some(name) => name.to_string(),
                    None => json_parse_string(src, i)?.to_lossy_string(),
                };
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return Err(Thrown("SyntaxError: Expected ':' in JSON object".into()));
                }
                *i += 1;
                json_skip_ws(b, i);
                let val = self.json_parse_value(src, i, depth + 1)?;
                pairs.push((key, val));
                json_skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b'}') => break,
                    _ => {
                        return Err(Thrown(
                            "SyntaxError: Expected ',' or '}' in JSON object".into(),
                        ))
                    }
                }
            }
        }
        *i += 1; // '}'
                 // `set_owned`, not `set(&k, …)`: the parser already allocated each key
                 // (`to_lossy_string` above), and `set` cloned a SECOND copy on first
                 // insertion only to drop the first. `with_capacity` then sizes the three
                 // parallel vectors once instead of growing them log n times — `pairs.len()`
                 // is exact for a duplicate-free object and a harmless over-reserve otherwise.
        let mut map = crate::heap::ObjMap::with_capacity(pairs.len());
        for (k, v) in pairs {
            map.set_owned(k, v);
        }
        Ok(self.alloc_object_current_realm(map))
    }

    /// `[[Get]](holder, key)` for the reviver walk: a canonical array index goes
    /// through `get_index` (so an absent element reads up the prototype chain), any
    /// other key through the named `[[Get]]`. Both observe getters / Proxy traps.
    fn json_get(&mut self, holder: Value, key: &str) -> Result<Value, Thrown> {
        if let Ok(i) = key.parse::<u32>() {
            if i.to_string() == *key {
                return self.get_index(holder, Value::num(i as f64));
            }
        }
        self.get_prop(holder, key)
    }

    /// CreateDataProperty(target, key, value): `target.[[DefineOwnProperty]]` with a
    /// fresh `{value, writable, enumerable, configurable}` data descriptor. A Proxy's
    /// defineProperty trap may throw (propagated); an ordinary object that REJECTS the
    /// define (e.g. a non-configurable existing prop) just returns false — no throw.
    fn json_create_data(&mut self, target: Value, key: &str, value: Value) -> Result<(), Thrown> {
        let is_proxy =
            target.is_heap() && matches!(self.heap.get(target.heap_index()), HeapObj::Proxy { .. });
        let mut m = crate::heap::ObjMap::new();
        m.set("value", value);
        m.set("writable", Value::TRUE);
        m.set("enumerable", Value::TRUE);
        m.set("configurable", Value::TRUE);
        let desc = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))));
        let r = self.object_define_property(target, key, desc);
        if is_proxy {
            r
        } else {
            let _ = r; // ordinary [[DefineOwnProperty]] never throws; a reject is false
            Ok(())
        }
    }

    /// InternalizeJSONProperty: walk the parsed tree bottom-up, replacing each
    /// `holder[key]` with `reviver.call(holder, key, value, context)`. Children
    /// are revived before their parent; a child revived to `undefined` is
    /// deleted. `src` is this value's parse-source node (ES2025
    /// json-parse-with-source): a primitive's `context` carries its raw source
    /// text, an array/object's `context` is an empty object.
    pub(crate) fn internalize_json(
        &mut self,
        holder: Value,
        key: &str,
        reviver: Value,
        src: Option<&JsonSrc>,
    ) -> Result<Value, Thrown> {
        let mut active = Vec::new();
        #[cfg(feature = "safe-sandbox")]
        active
            .try_reserve_exact(MAX_JSON_NESTING_DEPTH)
            .map_err(|_| {
                Thrown("RangeError: JSON reviver nesting depth exceeds the sandbox limit".into())
            })?;
        self.internalize_json_at(holder, key, reviver, src, 0, &mut active)
    }

    fn internalize_json_at(
        &mut self,
        holder: Value,
        key: &str,
        reviver: Value,
        src: Option<&JsonSrc>,
        depth: usize,
        active: &mut Vec<u32>,
    ) -> Result<Value, Thrown> {
        // 1. val = ? Get(holder, name)  — a real [[Get]] (getters / Proxy / the
        // prototype chain are all observed, e.g. a deleted element reads its inherited
        // value).
        let val = self.json_get(holder, key)?;
        // json-parse-with-source correspondence (proposal InternalizeJSONProperty
        // step 3): the parse node applies only while the CURRENT value still
        // SameValue-matches the value it produced. A reviver that forward-modified
        // this holder entry invalidates the snapshot — its `context` carries no
        // `source` and its children no longer correspond.
        let src = src.filter(|s| self.same_value(s.snapshot(), val));
        // 2. If Type(val) is Object: recurse into its elements / enumerable props
        // using REAL object operations so a reviver that mutates the holder (changing
        // length, replacing a value with a Proxy, making a prop non-configurable, …)
        // is observed and any abrupt completion propagates.
        if val.is_heap() && self.is_object_value(val) {
            #[cfg(feature = "safe-sandbox")]
            {
                if depth >= MAX_JSON_NESTING_DEPTH || active.contains(&val.heap_index()) {
                    return Err(Thrown(
                        "RangeError: JSON reviver nesting depth exceeds the sandbox limit".into(),
                    ));
                }
                active.push(val.heap_index());
            }

            // Keep this active-path entry across child getters, Proxy traps, and
            // child reviver calls. A prior child may replace a later child with
            // an ancestor, even though the originally parsed tree was acyclic.
            let walk = (|| -> Result<(), Thrown> {
                if self.value_is_array(val) {
                    // 2.b.ii  len = ? ToLength(? Get(val, "length"))
                    let lenv = self.get_prop(val, "length")?;
                    let lenf = self.to_number_coerce(lenv)?;
                    let len: u64 = if lenf.is_nan() || lenf <= 0.0 {
                        0
                    } else {
                        lenf.min(9007199254740991.0) as u64
                    };
                    #[cfg(feature = "safe-sandbox")]
                    if len > MAX_EAGER_ITER_RESULT as u64 {
                        return Err(Thrown(
                            "RangeError: JSON reviver array exceeds the sandbox iteration limit"
                                .into(),
                        ));
                    }
                    let mut i: u64 = 0;
                    while i < len {
                        let k = i.to_string();
                        // Source tracking only applies to the ORIGINAL parsed element at
                        // this position; the snapshot check at the child's own entry
                        // drops a reviver-replaced value's source.
                        let child = match src {
                            Some(JsonSrc::Arr(v, _)) => v.get(i as usize),
                            _ => None,
                        };
                        let nv =
                            self.internalize_json_at(val, &k, reviver, child, depth + 1, active)?;
                        if nv.is_undefined() {
                            self.delete_property(val, &k)?; // ? val.[[Delete]](ToString(I))
                        } else {
                            self.json_create_data(val, &k, nv)?; // ? CreateDataProperty
                        }
                        i += 1;
                    }
                } else {
                    // 2.c  keys = ? EnumerableOwnPropertyNames(val, key)  — proxy-aware
                    // (the ownKeys trap may throw), in integer-then-insertion order.
                    let keys_v = self.object_enum_own(val, crate::vm::EnumWhat::Keys)?;
                    let keys: Vec<String> = match self.heap.get(keys_v.heap_index()) {
                        HeapObj::Array(a) => a.iter().map(|&k| self.display(k)).collect(),
                        _ => Vec::new(),
                    };
                    for k in keys {
                        let child = match src {
                            Some(JsonSrc::Obj(pairs, _)) => pairs.get(&k),
                            _ => None,
                        };
                        let nv =
                            self.internalize_json_at(val, &k, reviver, child, depth + 1, active)?;
                        if nv.is_undefined() {
                            self.delete_property(val, &k)?;
                        } else {
                            self.json_create_data(val, &k, nv)?;
                        }
                    }
                }
                Ok(())
            })();

            #[cfg(feature = "safe-sandbox")]
            active.pop();
            walk?;
        }
        let context = self.make_json_context(src);
        let kv = self.alloc_str(key.to_string());
        self.call_value(reviver, holder, &[kv, val, context])
    }

    /// The reviver `context`: a plain object that, for a primitive parse node,
    /// carries a `"source"` data property holding the value's raw JSON text.
    /// An array/object node yields an empty context.
    fn make_json_context(&mut self, src: Option<&JsonSrc>) -> Value {
        let ctx = self.alloc_object_current_realm(crate::heap::ObjMap::new());
        if let Some(JsonSrc::Prim(s, _)) = src {
            let sv = self.alloc_str(s.clone());
            if let HeapObj::Object(m) = self.heap.get_mut(ctx.heap_index()) {
                m.set("source", sv);
            }
        }
        ctx
    }

    /// Like [`json_parse`], but also returns a parallel source tree recording the
    /// raw JSON text of every value (for the parse-with-source reviver context).
    pub(crate) fn json_parse_with_src(&mut self, src: &[u8]) -> Result<(Value, JsonSrc), Thrown> {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::JsonParse);
        // W9 static pretenure — same scope as `json_parse`; the reviver runs
        // later, outside this call, so its results stay young.
        self.heap.pretenure_begin();
        let r = self.json_parse_with_src_scoped(src);
        self.heap.pretenure_end();
        r
    }

    fn json_parse_with_src_scoped(&mut self, src: &[u8]) -> Result<(Value, JsonSrc), Thrown> {
        let mut i = 0;
        json_skip_ws(src, &mut i);
        let r = self.json_parse_value_src(src, &mut i, 0)?;
        json_skip_ws(src, &mut i);
        if i != src.len() {
            return Err(Thrown(
                "SyntaxError: Unexpected non-whitespace character after JSON".into(),
            ));
        }
        Ok(r)
    }

    fn json_parse_value_src(
        &mut self,
        src: &[u8],
        i: &mut usize,
        depth: usize,
    ) -> Result<(Value, JsonSrc), Thrown> {
        let b = src;
        match b.get(*i).copied() {
            Some(b'{') => self.json_parse_object_src(src, i, depth),
            Some(b'[') => self.json_parse_array_src(src, i, depth),
            _ => {
                // A primitive (string/number/true/false/null): record its exact span.
                let start = *i;
                let v = self.json_parse_value(src, i, depth)?;
                // `context.source` is a Rust String — LOSSY if the span holds
                // a raw lone surrogate (documented limit; escapes round-trip).
                Ok((
                    v,
                    JsonSrc::Prim(crate::heap::wtf8_to_lossy_string(&src[start..*i]), v),
                ))
            }
        }
    }

    fn json_parse_array_src(
        &mut self,
        src: &[u8],
        i: &mut usize,
        depth: usize,
    ) -> Result<(Value, JsonSrc), Thrown> {
        json_check_parse_depth(depth)?;
        let b = src;
        *i += 1; // '['
        let mut items = Vec::new();
        let mut srcs = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) != Some(&b']') {
            loop {
                json_skip_ws(b, i);
                let (v, s) = self.json_parse_value_src(src, i, depth + 1)?;
                items.push(v);
                srcs.push(s);
                json_skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b']') => break,
                    _ => {
                        return Err(Thrown(
                            "SyntaxError: Expected ',' or ']' in JSON array".into(),
                        ))
                    }
                }
            }
        }
        *i += 1; // ']'
        let av = self.alloc_array_current_realm(items);
        Ok((av, JsonSrc::Arr(srcs, av)))
    }

    fn json_parse_object_src(
        &mut self,
        src: &[u8],
        i: &mut usize,
        depth: usize,
    ) -> Result<(Value, JsonSrc), Thrown> {
        json_check_parse_depth(depth)?;
        let b = src;
        *i += 1; // '{'
        let mut pairs: Vec<(String, Value)> = Vec::new();
        // Source correspondence is queried by property name during the
        // reviver walk. A Vec both made duplicate replacement and every later
        // lookup linear, turning a flat object with unique attacker-chosen keys
        // into quadratic work inside JSON.parse. RandomState's keyed hash table
        // keeps those operations expected-linear without trusting guest keys.
        let mut srcs: std::collections::HashMap<String, JsonSrc> = std::collections::HashMap::new();
        json_skip_ws(b, i);
        if b.get(*i) != Some(&b'}') {
            loop {
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b'"') {
                    return Err(Thrown(
                        "SyntaxError: Expected property name string in JSON".into(),
                    ));
                }
                // B233: same escape-free member-name read as the ordinary
                // object path — see the comment there.
                let key = match crate::vm::helpers_json::json_plain_key_enabled()
                    .then(|| crate::vm::helpers_json::json_scan_plain_key(b, i))
                    .flatten()
                {
                    Some(name) => name.to_string(),
                    None => json_parse_string(src, i)?.to_lossy_string(),
                };
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return Err(Thrown("SyntaxError: Expected ':' in JSON object".into()));
                }
                *i += 1;
                json_skip_ws(b, i);
                let (val, s) = self.json_parse_value_src(src, i, depth + 1)?;
                pairs.push((key.clone(), val));
                // A DUPLICATE key OVERWRITES, exactly as the object build below
                // does (`map.set`): the LAST member is what the property ends up
                // holding, so that is the parse node `context.source` must report.
                // Appending instead made the lookup find the FIRST member, whose
                // snapshot no longer matched the property's value — so the
                // correspondence check dropped `source` altogether
                // (staging/sm/JSON/parse-with-source.js line 76,
                // `{ "b": 2, "b": 1, "b": 4 }`).
                if !srcs.contains_key(&key) {
                    srcs.try_reserve(1).map_err(|_| {
                        Thrown("RangeError: JSON parse source-map allocation failed".into())
                    })?;
                }
                srcs.insert(key, s);
                json_skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b'}') => break,
                    _ => {
                        return Err(Thrown(
                            "SyntaxError: Expected ',' or '}' in JSON object".into(),
                        ))
                    }
                }
            }
        }
        *i += 1; // '}'
                 // As in `json_parse_object`. The `key.clone()` above stays: this variant
                 // maintains a PARALLEL source tree that needs the key too, so one of the two
                 // must own a copy. `set_owned` still removes the third allocation — the one
                 // `set` made inside the map.
        let mut map = crate::heap::ObjMap::with_capacity(pairs.len());
        for (k, v) in pairs {
            map.set_owned(k, v);
        }
        let ov = self.alloc_object_current_realm(map);
        Ok((ov, JsonSrc::Obj(srcs, ov)))
    }
}

/// A parallel tree to a parsed JSON value recording each node's raw source text
/// AND the value the node produced (its snapshot), for the ES2025
/// parse-with-source reviver `context.source`. The snapshot drives the spec's
/// SameValue correspondence check: a holder entry the reviver forward-modified
/// no longer matches its parse node, so its `context` loses `source` and its
/// children stop corresponding. Snapshot `Value`s are held across reviver
/// callbacks — safe because the whole walk runs under a `gc_lock_guard`.
pub(crate) enum JsonSrc {
    /// A primitive leaf — the exact JSON text that produced it (e.g. `"1.1"`).
    Prim(String, Value),
    Arr(Vec<JsonSrc>, Value),
    Obj(std::collections::HashMap<String, JsonSrc>, Value),
}

impl JsonSrc {
    /// The value this parse node produced at parse time.
    pub(crate) fn snapshot(&self) -> Value {
        match self {
            JsonSrc::Prim(_, v) | JsonSrc::Arr(_, v) | JsonSrc::Obj(_, v) => *v,
        }
    }
}

#[cfg(all(test, feature = "safe-sandbox"))]
mod stringify_limit_tests {
    use crate::embed;

    // A tiny retained graph whose serialized expansion is about 10 MiB:
    // every level aliases the preceding value ten times, so constructing it
    // costs only four short arrays and one 1 KiB string.
    const ALIAS_TREE: &str = r#"
        let v = "x".repeat(1024);
        for (let depth = 0; depth < 4; depth++) {
            v = [v, v, v, v, v, v, v, v, v, v];
        }
    "#;

    fn assert_catchable_range_error(call: &str) {
        let source = format!(
            r#"
                {ALIAS_TREE}
                let caught = "none";
                try {{ {call}; }} catch (error) {{
                    caught = (error instanceof RangeError) + ":" + error.name + ":" + error.message;
                }}
                console.log(caught);
            "#
        );
        let outcome = crate::run(&source).expect("script compiles");
        assert_eq!(outcome.error, None, "the RangeError must be catchable");
        assert_eq!(
            outcome.output,
            vec!["true:RangeError:Invalid string length"]
        );
    }

    #[test]
    fn plain_fast_stringify_bounds_alias_expansion() {
        assert_catchable_range_error("JSON.stringify(v)");
    }

    #[test]
    fn general_stringify_bounds_alias_expansion() {
        assert_catchable_range_error("JSON.stringify(v, function (_key, value) { return value; })");
    }

    #[test]
    fn metered_stringify_exhaustion_is_sticky() {
        let source = format!(
            r#"
                {ALIAS_TREE}
                try {{
                    JSON.stringify(v, function (_key, value) {{ return value; }});
                }} catch (error) {{}}
            "#
        );
        let mut state = embed::compile_script(&source).expect("script compiles");
        state.set_limits(u64::MAX, None);
        let baseline = state.heap_bytes();
        state.set_heap_limit(baseline + 128 * 1024);

        let error = state
            .run_init()
            .expect_err("a guest catch must not clear resource exhaustion");
        assert!(error.contains("memory budget"), "unexpected error: {error}");
        assert!(
            state
                .resource_limit_error()
                .is_some_and(|message| message.contains("memory budget")),
            "the typed resource status must remain sticky"
        );
    }

    #[test]
    fn pretty_stringify_caps_recursive_indentation() {
        let outcome = crate::run(
            r#"
                let value = 0;
                for (let depth = 0; depth < 300; depth++) value = [value];
                let caught = "none";
                try { JSON.stringify(value, null, 10); } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the depth RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:JSON nesting depth exceeds the sandbox limit"]
        );
    }

    #[test]
    fn replacer_array_caps_virtual_length_inside_native_call() {
        let outcome = crate::run(
            r#"
                const replacer = new Proxy([], {
                    get(target, key) {
                        return key === "length" ? 1e12 : target[key];
                    }
                });
                let caught = "none";
                try { JSON.stringify({}, replacer); } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the length RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:JSON replacer array exceeds the sandbox iteration limit"]
        );
    }

    #[test]
    fn replacer_property_list_caps_aggregate_key_storage() {
        let outcome = crate::run(
            r#"
                const pad = "x".repeat(1024);
                const replacer = [];
                for (let i = 0; i < 8192; i++) replacer.push(pad + i);
                let caught = "none";
                try { JSON.stringify({}, replacer); } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.name + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the policy RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:RangeError:JSON key snapshot exceeds the sandbox memory limit"]
        );
    }

    #[test]
    fn replacer_property_list_caps_duplicate_conversion_churn() {
        let outcome = crate::run(
            r#"
                const key = "x".repeat(262144);
                const replacer = Array(40).fill(key);
                let caught = "none";
                try { JSON.stringify({}, replacer); } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.name + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the policy RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:RangeError:JSON key snapshot exceeds the sandbox memory limit"]
        );
    }

    #[test]
    fn object_key_snapshot_caps_aggregate_cloned_capacities() {
        let outcome = crate::run(
            r#"
                const pad = "x".repeat(1024);
                const value = {};
                for (let i = 0; i < 8192; i++) value[pad + i] = i;
                let caught = "none";
                try {
                    JSON.stringify(value, function (_key, item) { return item; });
                } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.name + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the policy RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:RangeError:JSON key snapshot exceeds the sandbox memory limit"]
        );
    }

    #[test]
    fn stringify_array_caps_virtual_length_inside_native_call() {
        let outcome = crate::run(
            r#"
                const value = new Proxy([], {
                    get(target, key) {
                        return key === "length" ? 1e12 : target[key];
                    }
                });
                let caught = "none";
                try { JSON.stringify(value); } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the length RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:JSON array exceeds the sandbox iteration limit"]
        );
    }

    #[test]
    fn replacer_deduplication_preserves_first_seen_order() {
        let outcome = crate::run(
            r#"
                const value = { a: 1, b: 2, c: 3 };
                console.log(JSON.stringify(value, ["b", "a", "b", "c", "a"]));
            "#,
        )
        .expect("script compiles");
        assert_eq!(outcome.error, None);
        assert_eq!(outcome.output, vec![r#"{"b":2,"a":1,"c":3}"#]);
    }

    fn assert_parse_depth_limited(call: &str) {
        let source = format!(
            r#"
                const text = "[".repeat(65) + "0" + "]".repeat(65);
                let caught = "none";
                try {{ {call}; }} catch (error) {{
                    caught = (error instanceof RangeError) + ":" + error.name + ":" + error.message;
                }}
                console.log(caught);
            "#
        );
        let outcome = crate::run(&source).expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the depth RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:RangeError:JSON parse nesting depth exceeds the sandbox limit"]
        );
    }

    #[test]
    fn parse_caps_recursive_descent() {
        assert_parse_depth_limited("JSON.parse(text)");
    }

    #[test]
    fn parse_with_source_caps_recursive_descent() {
        assert_parse_depth_limited("JSON.parse(text, function (_key, value) { return value; })");
    }

    #[test]
    fn reviver_mutation_cannot_create_an_unbounded_cycle() {
        let outcome = crate::run(
            r#"
                let caught = "none";
                try {
                    JSON.parse("[0,0]", function (key, value) {
                        if (key === "0") this[1] = this;
                        return value;
                    });
                } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the cycle RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:JSON reviver nesting depth exceeds the sandbox limit"]
        );
    }

    #[test]
    fn reviver_mutation_cannot_create_an_unbounded_deep_graph() {
        let outcome = crate::run(
            r#"
                let caught = "none";
                try {
                    JSON.parse("[0,0]", function (key, value) {
                        if (key === "0") {
                            let deep = 0;
                            for (let i = 0; i < 80; i++) deep = { next: deep };
                            this[1] = deep;
                        }
                        return value;
                    });
                } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the depth RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:JSON reviver nesting depth exceeds the sandbox limit"]
        );
    }

    #[test]
    fn reviver_proxy_getter_cannot_reintroduce_an_ancestor() {
        let outcome = crate::run(
            r#"
                let caught = "none";
                try {
                    JSON.parse("[0,0]", function (key, value) {
                        if (key === "0") {
                            const root = this;
                            this[1] = new Proxy({ child: 0 }, {
                                get(target, name) {
                                    return name === "child" ? root : target[name];
                                }
                            });
                        }
                        return value;
                    });
                } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the cycle RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:JSON reviver nesting depth exceeds the sandbox limit"]
        );
    }

    #[test]
    fn reviver_proxy_cannot_claim_an_unbounded_array_length() {
        let outcome = crate::run(
            r#"
                let caught = "none";
                try {
                    JSON.parse("[0,0]", function (key, value) {
                        if (key === "0") {
                            this[1] = new Proxy([], {
                                get(target, name) {
                                    return name === "length" ? 1e12 : target[name];
                                }
                            });
                        }
                        return value;
                    });
                } catch (error) {
                    caught = (error instanceof RangeError) + ":" + error.message;
                }
                console.log(caught);
            "#,
        )
        .expect("script compiles");
        assert_eq!(
            outcome.error, None,
            "the length RangeError must be catchable"
        );
        assert_eq!(
            outcome.output,
            vec!["true:JSON reviver array exceeds the sandbox iteration limit"]
        );
    }
}
