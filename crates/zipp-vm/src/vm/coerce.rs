#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;
use std::fmt::Write as _;

/// Largest combined length (UTF-16 units) that `a + b` builds as an immediate
/// FLAT string instead of a rope node. A small rope costs MORE than the copy:
/// the Cons allocation now plus a flatten allocation + objs write-back on
/// first content access (and Map/Set keys, property names, comparisons all
/// access content immediately) — the same reasoning as V8's ConsString
/// minimum length (13). `s += part` loops building LONG strings still get
/// O(1) rope appends (their combined length exceeds this almost immediately).
///
/// MEASURED 2026-07-26 — this was 24, which is far too eager to build ropes.
/// Real string-building code assembles a line from ~20 pieces and then READS it
/// (stores it, matches it, joins it), so the rope is flattened almost
/// immediately and every node allocated on the way was waste. Worse, a
/// `str + int` past the limit loses its fused fast path and pays TWO allocations
/// per part (a heap string for the number, then the Cons).
///
/// Sweeping the threshold against the real suite and an adversarial case:
///
/// ```text
/// units    markdown  regex-log-scan   many-2KB-strings (adversarial)
///    24       857ms          2610ms                       80ms
///   128       760ms          2210ms                       83ms
///   256       767ms          1965ms                       85ms
///   512       676ms          1980ms                      112ms
///  2048       613ms          1992ms                      646ms
/// ```
///
/// 256 takes essentially the whole regex-log-scan win (-25%) for a 6% cost on
/// the adversarial shape — building many strings up to the threshold one small
/// piece at a time, which is O(n²) copying and the reason this cannot simply be
/// raised without bound. Above 512 that term dominates and the curve inverts.
const SMALL_CONCAT_FLAT_UNITS: usize = 256;

/// Decimal ASCII form of an i32 written into a stack buffer: the digits live
/// in `buf[start..]` of the returned `(buf, start)`. No allocation — the
/// string⊕int concat fast path copies them straight into its result buffer.
#[inline]
pub(crate) fn fmt_i32_buf(n: i32) -> ([u8; 12], usize) {
    let mut buf = [0u8; 12];
    let mut i = buf.len();
    let mut m = (n as i64).unsigned_abs(); // i64: |i32::MIN| representable
    loop {
        i -= 1;
        buf[i] = b'0' + (m % 10) as u8;
        m /= 10;
        if m == 0 {
            break;
        }
    }
    if n < 0 {
        i -= 1;
        buf[i] = b'-';
    }
    (buf, i)
}

/// Default-on fast path for appending one of the heap's 128 permanently
/// interned ASCII-character strings directly into a proven-linear builder.
/// The switch is process-latched so the hot append pays one relaxed byte load,
/// while `ZIPP_NO_APPEND_ASCII_CHAR=1` restores the previous flat-string path
/// for same-binary A/B measurements.
#[inline]
fn ascii_char_append_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_APPEND_ASCII_CHAR").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Let the fused `acc += ascii_source[index]` helper create the first mutable
/// builder when `acc` is still the permanently interned empty/string literal.
/// Subsequent loop iterations retain the allocation-free in-place path. The
/// old first-iteration deopt is independently available for same-binary A/B.
#[inline]
fn str_append_index_first_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_STR_APPEND_INDEX_FIRST").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

/// Small proven-linear string builders overwhelmingly finish below this size
/// (IDs, tokens, short keys). Reserving it while constructing the first
/// mutable builder avoids the ordinary 8 -> 16 -> 32 backing-buffer growths;
/// it does not add a heap slot or change when the JS string itself is born.
const STR_APPEND_INDEX_FIRST_RESERVE: usize = 32;

/// Same-binary ablation for the bounded first-builder reserve. This is read
/// only on the first append (never on the per-character in-place hot path).
#[inline]
fn str_append_index_reserve_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_STR_APPEND_INDEX_RESERVE").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[inline]
fn markdown_push_escaped_ascii(out: &mut Vec<u8>, bytes: &[u8]) {
    for &b in bytes {
        match b {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            _ => out.push(b),
        }
    }
}

impl<'p> Vm<'p> {
    /// Preflight a loop that runs entirely inside one VM instruction. Such work
    /// is invisible to the ordinary bytecode step meter, so array-like lengths
    /// and similar guest-controlled counts need a separate safe-profile bound.
    pub(crate) fn preflight_native_iteration_work(&self, count: u64) -> Result<(), Thrown> {
        if count > MAX_NATIVE_ITERATION_WORK {
            Err(Thrown(
                "RangeError: native builtin iteration limit exceeded".into(),
            ))
        } else {
            Ok(())
        }
    }

    /// Sandboxed VMs retain the historical exact-capacity builder. The public
    /// heap ceiling intentionally counts HeapObj slots, not payload buffers;
    /// applying this reserve there would therefore add up to 31 uncharged
    /// bytes per live one-byte builder. Keeping the optimization out of every
    /// instrumented VM makes its incremental accounting undercount exactly 0.
    #[inline]
    fn str_append_index_reserve_allowed(&self) -> bool {
        if !str_append_index_reserve_enabled() {
            return false;
        }
        #[cfg(feature = "instrument")]
        if self.instr_rec.is_some() {
            return false;
        }
        true
    }

    /// Execute the exact [`crate::codegen::MarkdownInlinePlan`] over one flat
    /// ASCII primitive string. The source recogniser licenses the state
    /// machine; these live guards preserve every remaining observable lookup.
    /// Any mismatch is a side-effect-free decline to the ordinary Tier-C body.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn markdown_inline_reduce(
        &mut self,
        plan: crate::codegen::MarkdownInlinePlan,
        input: Value,
    ) -> Option<u64> {
        // `charCodeAt` and `substring` are invoked repeatedly by both exact
        // source bodies. Read the actual own data slots on every outer call:
        // accessorisation, deletion, replacement, and child-realm prototype
        // images must all execute the generic JavaScript path.
        if self.str_proto == 0 || self.active_realm_proto(self.str_proto) != self.str_proto {
            return None;
        }
        let methods_pristine = match self.heap.get(self.str_proto) {
            HeapObj::Object(map) => ["charCodeAt", "substring"].iter().all(|name| {
                map.pos(name).is_some_and(|slot| {
                    !map.attr_at(slot).accessor
                        && map.val_at(slot).is_heap()
                        && matches!(
                            self.heap.get(map.val_at(slot).heap_index()),
                            HeapObj::Native(id)
                                if native::proto_method(*id)
                                    .is_some_and(|(n, kind, _)| n == *name && kind == 1)
                        )
                })
            }),
            _ => false,
        };
        if !methods_pristine {
            return None;
        }

        // The render body performs a live global Get at each code/link span.
        // Accept only the exact no-capture helper function; rebinding it to a
        // proxy, closure, accessor-routed value or lookalike remains observable.
        let helper = *self.globals.get(plan.escape_html_global as usize)?;
        if !helper.is_heap() || self.get_function_realm(helper) != 0 {
            return None;
        }
        let helper_fid = match self.heap.get(helper.heap_index()) {
            HeapObj::Func(id) => *id,
            _ => return None,
        };
        if !crate::codegen::markdown_escape_html_proto(self.func(helper_fid as usize)) {
            return None;
        }

        let output = {
            let bytes = match input.is_heap().then(|| self.heap.get(input.heap_index())) {
                Some(HeapObj::Str(s)) if s.is_ascii() => s.as_bytes(),
                _ => return None,
            };
            let capacity = bytes.len().checked_add(bytes.len() / 2)?;
            let mut out = Vec::with_capacity(capacity);
            let mut i = 0usize;
            let mut bold = false;
            let mut ital = false;
            while i < bytes.len() {
                match bytes[i] {
                    b'*' => {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                            out.extend_from_slice(if bold { b"</strong>" } else { b"<strong>" });
                            bold = !bold;
                            i += 2;
                        } else {
                            out.extend_from_slice(if ital { b"</em>" } else { b"<em>" });
                            ital = !ital;
                            i += 1;
                        }
                    }
                    b'`' => {
                        let mut j = i + 1;
                        while j < bytes.len() && bytes[j] != b'`' {
                            j += 1;
                        }
                        out.extend_from_slice(b"<code>");
                        markdown_push_escaped_ascii(&mut out, &bytes[i + 1..j]);
                        out.extend_from_slice(b"</code>");
                        i = j + 1;
                    }
                    b'[' => {
                        let mut close_text = i + 1;
                        while close_text < bytes.len() && bytes[close_text] != b']' {
                            close_text += 1;
                        }
                        if close_text + 1 < bytes.len() && bytes[close_text + 1] == b'(' {
                            let mut close_url = close_text + 2;
                            while close_url < bytes.len() && bytes[close_url] != b')' {
                                close_url += 1;
                            }
                            out.extend_from_slice(b"<a href=\"");
                            out.extend_from_slice(&bytes[close_text + 2..close_url]);
                            out.extend_from_slice(b"\">");
                            markdown_push_escaped_ascii(&mut out, &bytes[i + 1..close_text]);
                            out.extend_from_slice(b"</a>");
                            i = close_url + 1;
                        } else {
                            out.push(b'[');
                            i += 1;
                        }
                    }
                    b'&' => {
                        out.extend_from_slice(b"&amp;");
                        i += 1;
                    }
                    b'<' => {
                        out.extend_from_slice(b"&lt;");
                        i += 1;
                    }
                    b'>' => {
                        out.extend_from_slice(b"&gt;");
                        i += 1;
                    }
                    b => {
                        out.push(b);
                        i += 1;
                    }
                }
            }
            out
        };
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                eprintln!("[jit] markdown-inline reducer accepted an ASCII input");
            }
        }
        Some(
            Value::heap(
                self.heap
                    .alloc(HeapObj::Str(crate::heap::JsStr::from_ascii(output))),
            )
            .bits(),
        )
    }
}

/// ECMAScript StringNumericLiteral over an already-decoded string. Keeping
/// this grammar in one allocation-free entry lets the RegExp scalar-result
/// path apply unary `+` directly to an immutable subject range without first
/// allocating the capture string. `Vm::to_number` delegates its ordinary
/// string arm here, so the two paths cannot drift.
pub(crate) fn string_to_number(s: &str) -> f64 {
    // StrWhiteSpace includes U+FEFF (BOM), which Rust's trim does not.
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
    if t.is_empty() {
        return 0.0;
    }
    // Non-decimal integer literals `0x…`/`0o…`/`0b…`
    // (StringNumericLiteral; no sign allowed). Fold digits into an f64 so
    // arbitrarily long literals don't overflow.
    let radix = match t.as_bytes() {
        [b'0', b'x' | b'X', ..] => Some((16u32, &t[2..])),
        [b'0', b'o' | b'O', ..] => Some((8, &t[2..])),
        [b'0', b'b' | b'B', ..] => Some((2, &t[2..])),
        _ => None,
    };
    if let Some((base, digits)) = radix {
        let mut acc = 0.0f64;
        for c in digits.chars() {
            match c.to_digit(base) {
                Some(d) => acc = acc * base as f64 + d as f64,
                None => return f64::NAN,
            }
        }
        return if digits.is_empty() { f64::NAN } else { acc };
    }
    // The only Infinity spellings a StringNumericLiteral accepts are these
    // exact capital-I forms.
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    // Rust's parser also accepts word forms JS rejects. A valid decimal or
    // scientific literal begins (after an optional sign) with a digit or `.`.
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    if body
        .as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic())
    {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// `ZIPP_ICSTATS=1` engagement census for `AddRightPair`.  `fast_str` and
/// `fast_int` are the bounded one-allocation ASCII arms classified by the
/// rightmost leaf; `fallback` ran the original inner Add followed by the outer
/// Add.  Off, the hot path pays one relaxed byte load, matching chainstats.
mod pairstats {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    static FAST_STR: AtomicU64 = AtomicU64::new(0);
    static FAST_INT: AtomicU64 = AtomicU64::new(0);
    static IN_PLACE: AtomicU64 = AtomicU64::new(0);
    static FALLBACK: AtomicU64 = AtomicU64::new(0);

    #[inline]
    fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_ICSTATS").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }

    #[inline]
    pub(super) fn fast_str() {
        if enabled() {
            FAST_STR.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(super) fn fast_int() {
        if enabled() {
            FAST_INT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(super) fn in_place() {
        if enabled() {
            IN_PLACE.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(super) fn fallback() {
        if enabled() {
            FALLBACK.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn dump() -> (u64, u64, u64, u64) {
        (
            FAST_STR.load(Ordering::Relaxed),
            FAST_INT.load(Ordering::Relaxed),
            IN_PLACE.load(Ordering::Relaxed),
            FALLBACK.load(Ordering::Relaxed),
        )
    }
}

pub(crate) fn concat_pair_stats() -> (u64, u64, u64, u64) {
    pairstats::dump()
}

/// `ZIPP_ICSTATS=1` engagement census for `Pad2Concat`. `zero` is a cached
/// `"0" + int(0..9)` result, `plain` a cached `"" + int(10..99)` result, and
/// `fallback` delegates to the exact ordinary `+` path. Off, each opcode pays
/// one relaxed byte load; the compiler rollback emits no opcode at all.
mod pad2stats {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    static ZERO: AtomicU64 = AtomicU64::new(0);
    static PLAIN: AtomicU64 = AtomicU64::new(0);
    static FALLBACK: AtomicU64 = AtomicU64::new(0);
    static COND_HIT: AtomicU64 = AtomicU64::new(0);
    static COND_SLOW: AtomicU64 = AtomicU64::new(0);

    #[inline]
    pub(super) fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_ICSTATS").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }

    #[inline]
    pub(super) fn zero() {
        if enabled() {
            ZERO.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(super) fn plain() {
        if enabled() {
            PLAIN.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(super) fn fallback() {
        if enabled() {
            FALLBACK.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(super) fn cond_hit() {
        if enabled() {
            COND_HIT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(super) fn cond_slow() {
        if enabled() {
            COND_SLOW.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn dump() -> (u64, u64, u64) {
        (
            ZERO.load(Ordering::Relaxed),
            PLAIN.load(Ordering::Relaxed),
            FALLBACK.load(Ordering::Relaxed),
        )
    }

    pub(super) fn cond_dump() -> (u64, u64) {
        (
            COND_HIT.load(Ordering::Relaxed),
            COND_SLOW.load(Ordering::Relaxed),
        )
    }
}

pub(crate) fn pad2_concat_stats() -> (u64, u64, u64) {
    pad2stats::dump()
}

pub(crate) fn pad2_conditional_stats() -> (u64, u64) {
    pad2stats::cond_dump()
}

/// Console inspection is a diagnostic surface, not a second serializer. Keep
/// it small and total even when the value graph is cyclic or deliberately much
/// larger than the configured output budget. The byte limit is per argument;
/// the recorder still applies the lower aggregate stdout/stderr limit to the
/// completed line.
const INSPECT_MAX_DEPTH: usize = 16;
const INSPECT_MAX_NODES: usize = 1_024;
const INSPECT_MAX_BYTES: usize = 64 * 1024;
const INSPECT_TRUNCATION_MARKER: &str = "...";

/// `display` is used by a number of read-only coercion and diagnostic paths.
/// It cannot be allowed to recursively allocate one `String` per array element:
/// an array containing many aliases to the same large string would amplify a
/// small guest heap into gigabytes of transient host allocations. Keep one
/// fallible output buffer, cap graph recursion/work, and detect active cycles.
const DISPLAY_MAX_DEPTH: usize = 64;
#[cfg(feature = "safe-sandbox")]
const DISPLAY_MAX_NODES: usize = 1 << 19;
#[cfg(not(feature = "safe-sandbox"))]
const DISPLAY_MAX_NODES: usize = 1 << 24;
const DISPLAY_LIMIT_MARKER: &str = "<string coercion exceeded sandbox limits>";

struct DisplayBuffer {
    out: String,
    active: Vec<u32>,
    nodes: usize,
    failed: bool,
}

impl DisplayBuffer {
    fn new() -> Self {
        Self {
            out: String::new(),
            active: Vec::new(),
            nodes: 0,
            failed: false,
        }
    }

    fn consume_node(&mut self) -> bool {
        if self.failed || self.nodes >= DISPLAY_MAX_NODES {
            self.failed = true;
            return false;
        }
        self.nodes += 1;
        true
    }

    fn push_str(&mut self, text: &str) -> bool {
        if self.failed {
            return false;
        }
        let Some(total) = self.out.len().checked_add(text.len()) else {
            self.failed = true;
            return false;
        };
        if total > MAX_STRING_BYTES {
            self.failed = true;
            return false;
        }
        // Grow geometrically but never *request* capacity beyond the sandbox's
        // output ceiling. Reserving each tiny comma exactly would repeatedly
        // copy the accumulated string and turn a bounded display into O(n^2).
        if total > self.out.capacity() {
            let desired = total
                .max(self.out.capacity().max(16).saturating_mul(2))
                .min(MAX_STRING_BYTES);
            if self
                .out
                .try_reserve_exact(desired.saturating_sub(self.out.len()))
                .is_err()
            {
                self.failed = true;
                return false;
            }
        }
        self.out.push_str(text);
        true
    }

    fn push_char(&mut self, ch: char) -> bool {
        let mut bytes = [0u8; 4];
        self.push_str(ch.encode_utf8(&mut bytes))
    }

    fn finish(self) -> Result<String, Thrown> {
        if self.failed {
            Err(Thrown("RangeError: Invalid string length".into()))
        } else {
            Ok(self.out)
        }
    }
}

impl std::fmt::Write for DisplayBuffer {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.push_str(text) {
            Ok(())
        } else {
            Err(std::fmt::Error)
        }
    }
}

struct InspectBuffer {
    out: String,
    nodes: usize,
    active: Vec<u32>,
    truncated: bool,
}

impl InspectBuffer {
    fn new() -> Self {
        Self {
            out: String::with_capacity(256),
            nodes: 0,
            active: Vec::with_capacity(INSPECT_MAX_DEPTH),
            truncated: false,
        }
    }

    /// Charge one value/rope node before inspecting it. Once this fails every
    /// later write is suppressed and `finish` appends one stable marker.
    fn consume_node(&mut self) -> bool {
        if self.truncated || self.nodes >= INSPECT_MAX_NODES {
            self.truncated = true;
            return false;
        }
        self.nodes += 1;
        true
    }

    fn push_str(&mut self, text: &str) -> bool {
        if self.truncated {
            return false;
        }
        // Always retain room for a marker if this write discovers truncation.
        let payload_limit = INSPECT_MAX_BYTES.saturating_sub(INSPECT_TRUNCATION_MARKER.len());
        let available = payload_limit.saturating_sub(self.out.len());
        if text.len() <= available {
            self.out.push_str(text);
            return true;
        }

        let mut end = available.min(text.len());
        while end != 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        self.out.push_str(&text[..end]);
        self.truncated = true;
        false
    }

    fn push_char(&mut self, ch: char) -> bool {
        let mut bytes = [0u8; 4];
        self.push_str(ch.encode_utf8(&mut bytes))
    }

    fn finish(mut self) -> String {
        if self.truncated {
            debug_assert!(self.out.len() + INSPECT_TRUNCATION_MARKER.len() <= INSPECT_MAX_BYTES);
            self.out.push_str(INSPECT_TRUNCATION_MARKER);
        }
        self.out
    }
}

impl std::fmt::Write for InspectBuffer {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.push_str(text) {
            Ok(())
        } else {
            Err(std::fmt::Error)
        }
    }
}

/// Native emitters inline the allocation-free hit unless counters are enabled;
/// an ICSTATS run deliberately routes hits through the shared helper so the
/// mechanism census remains exact without burdening production code.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn pad2_concat_stats_enabled() -> bool {
    pad2stats::enabled()
}

impl<'p> Vm<'p> {
    /// Clone an array's current elements out of the heap. Used before invoking
    /// callbacks so a heap reallocation during the call can't dangle a borrow.
    /// Read an array-like receiver's elements (`this.length` coerced via ToLength,
    /// then `this[0 .. length]`) into a Vec — backs the generic Array.prototype
    /// methods invoked via `.call(arrayLike, …)` on a non-array (object or string).
    pub(crate) fn array_like_read(&mut self, idx: u32) -> Result<Vec<Value>, Thrown> {
        let this = Value::heap(idx);
        if let HeapObj::Array(items) = self.heap.get(idx) {
            return Ok(items.clone());
        }
        // len = ToLength(Get(this, "length")): a throwing `length` getter, or a
        // Symbol / throwing-valueOf length, propagates (ReturnIfAbrupt) instead of
        // being read as 0, so a generic Array.prototype method surfaces the
        // TypeError before any element/predicate work. The full ToPrimitive path
        // also honours an array-like whose `length` is an object with valueOf.
        let lv = self.get_prop(this, "length")?;
        let len = self.to_number_coerce(lv)?;
        // ToLength: a positive `len` (including +Infinity / "Infinity" / a huge
        // finite) clamps to MAX_DENSE_ARRAY_LEN; NaN and ≤0 (incl. -Infinity) → 0.
        // `len as usize` saturates for +Infinity, so the `.min` bounds it.
        let len = if len > 0.0 {
            (len as usize).min(crate::vm::MAX_DENSE_ARRAY_LEN)
        } else {
            0
        };
        let mut out = Vec::with_capacity(len.min(4096));
        for i in 0..len {
            // An index getter that throws must propagate (ReturnIfAbrupt) — absent
            // properties already come back Ok(undefined) from get_index.
            out.push(self.get_index(this, Value::int(i as i32))?);
        }
        Ok(out)
    }

    pub(crate) fn array_snapshot(&self, idx: u32) -> Vec<Value> {
        match self.heap.get(idx) {
            // A hole reads as `undefined` for every snapshot consumer (join, slice,
            // concat, spread, sort, JSON, …) — the internal HOLE sentinel must never
            // leak to user code. Hole-SENSITIVE methods (the callback/search/find
            // family) take the live HasProperty+Get path instead, not this snapshot.
            HeapObj::Array(items) => items
                .iter()
                .map(|&v| if v.is_hole() { Value::UNDEFINED } else { v })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether a real array currently holds any HOLE (an absent element). Used to
    /// route the hole-sensitive callback/search methods off the dense-snapshot fast
    /// path onto the live HasProperty+Get protocol; a hole-free array keeps the fast
    /// path. O(n), but only consulted by methods that are already O(n) (and whose
    /// per-element JS callback dominates).
    pub(crate) fn array_has_holes(&self, idx: u32) -> bool {
        matches!(self.heap.get(idx), HeapObj::Array(items) if items.iter().any(|v| v.is_hole()))
    }

    /// IsArray(v) (ES 7.2.2): true for an Array exotic, recursing through Proxy
    /// targets (a revoked or non-array proxy is not an array). Used by
    /// `Array.isArray` and the `IsArray` op so `Array.isArray(new Proxy([], …))`
    /// is true. (The revoked-proxy-throws nuance is approximated as `false`.)
    /// IsArray(v) with the spec's revoked-proxy TypeError (used where the
    /// abrupt completion is observable: Array.isArray, the IsArray op,
    /// IsConcatSpreadable, ArraySpeciesCreate).
    pub(crate) fn value_is_array_throwing(&self, v: Value) -> Result<bool, Thrown> {
        if !v.is_heap() {
            return Ok(false);
        }
        // %Array.prototype% is itself an Array exotic object.
        if self.arr_proto != 0 && v.heap_index() == self.arr_proto {
            return Ok(true);
        }
        let mut idx = v.heap_index();
        for _ in 0..1000 {
            match self.heap.get(idx) {
                HeapObj::Array(_) => return Ok(!self.arguments_objs.contains_key(&idx)),
                HeapObj::Proxy {
                    target, revoked, ..
                } => {
                    if *revoked {
                        return Err(Thrown(
                            "TypeError: Cannot perform 'IsArray' on a proxy that has been revoked"
                                .into(),
                        ));
                    }
                    if !target.is_heap() {
                        return Ok(false);
                    }
                    idx = target.heap_index();
                }
                _ => return Ok(false),
            }
        }
        Ok(false)
    }

    pub(crate) fn value_is_array(&self, v: Value) -> bool {
        if !v.is_heap() {
            return false;
        }
        // %Array.prototype% is itself an Array exotic object.
        if self.arr_proto != 0 && v.heap_index() == self.arr_proto {
            return true;
        }
        let mut idx = v.heap_index();
        for _ in 0..1000 {
            match self.heap.get(idx) {
                // An `arguments` exotic is Array-backed internally but is an ordinary
                // object ([[ParameterMap]]), NOT an Array exotic — IsArray is false.
                HeapObj::Array(_) => return !self.arguments_objs.contains_key(&idx),
                HeapObj::Proxy {
                    target, revoked, ..
                } if !*revoked && target.is_heap() => {
                    idx = target.heap_index();
                }
                _ => return false,
            }
        }
        false
    }

    /// CreateListFromArrayLike(obj) (ES 7.3.18): the elements `0 .. ToLength(obj.length)`
    /// of an array-LIKE (any object), read via Get — so `Function.prototype.apply` /
    /// `Reflect.apply` accept `{length, 0, 1, …}`, not only real arrays. Throws if
    /// `obj` is not an object.
    pub(crate) fn create_list_from_array_like(&mut self, obj: Value) -> Result<Vec<Value>, Thrown> {
        if !self.is_object_value(obj) {
            return Err(Thrown(
                "TypeError: CreateListFromArrayLike called on a non-object".into(),
            ));
        }
        if let HeapObj::Array(items) = self.heap.get(obj.heap_index()) {
            // An arguments object takes the generic route below: its `length`
            // is an ordinary (mutable, even deletable) property and a LIVE-
            // mapped index must read the formal's register, not the snapshot.
            if !self.arguments_objs.contains_key(&obj.heap_index()) {
                self.preflight_native_iteration_work(items.len() as u64)?;
                let mut out = Vec::new();
                out.try_reserve_exact(items.len())
                    .map_err(|_| Thrown("RangeError: argument-list allocation failed".into()))?;
                out.extend_from_slice(items);
                return Ok(out); // dense fast path
            }
        }
        let len_v = self.get_prop(obj, "length")?;
        let len_u64 = self.to_integer_or_zero(len_v)?.clamp(0, (1i64 << 53) - 1) as u64;
        // Do not narrow ToLength before applying the safe-profile work cap:
        // on wasm32 an attacker-controlled 2^32 used to wrap to an empty list.
        self.preflight_native_iteration_work(len_u64)?;
        let len = usize::try_from(len_u64)
            .map_err(|_| Thrown("RangeError: argument list is too large".into()))?;
        let mut out = Vec::new();
        out.try_reserve_exact(len)
            .map_err(|_| Thrown("RangeError: argument-list allocation failed".into()))?;
        for i in 0..len {
            out.push(self.get_index(obj, Value::num(i as f64))?);
        }
        Ok(out)
    }

    /// IsConcatSpreadable(O) (ES 23.1.3.1.1): a `Symbol.isConcatSpreadable`
    /// ("@@isConcatSpreadable") flag overrides — when present it is ToBoolean'd;
    /// otherwise the value is spreadable iff it is an Array. Non-objects are never
    /// spreadable. Used by `Array.prototype.concat`.
    pub(crate) fn is_concat_spreadable(&mut self, v: Value) -> Result<bool, Thrown> {
        // Non-OBJECTS are never spreadable (step 1: If O is not an Object,
        // return false) — including heap-allocated string/symbol/bigint
        // PRIMITIVES, even when `String.prototype[@@isConcatSpreadable]` is set.
        if !self.is_object_value(v) {
            return Ok(false);
        }
        let flag = self.get_prop(v, "@@isConcatSpreadable")?;
        if flag != Value::UNDEFINED {
            return Ok(self.truthy(flag));
        }
        // Step 4 is the REAL IsArray: it pierces Proxy targets (a proxy over an
        // array IS spreadable) and throws on a revoked proxy.
        self.value_is_array_throwing(v)
    }

    /// Recursively flatten nested arrays up to `depth` levels (for `Array.flat`).
    /// Each nested array is cloned out before recursing (releases the heap borrow).
    pub(crate) fn flatten_array(&self, items: &[Value], depth: i32) -> Result<Vec<Value>, Thrown> {
        let mut work = 0u64;
        self.flatten_array_at(items, depth, 0, &mut work)
    }

    fn flatten_array_at(
        &self,
        items: &[Value],
        depth: i32,
        active_depth: u32,
        work: &mut u64,
    ) -> Result<Vec<Value>, Thrown> {
        *work = work
            .checked_add(items.len() as u64)
            .ok_or_else(|| Thrown("RangeError: native builtin iteration limit exceeded".into()))?;
        self.preflight_native_iteration_work(*work)?;
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
                Some(a) => {
                    let next_depth = active_depth.checked_add(1).ok_or_else(|| {
                        Thrown("RangeError: array flattening nesting limit exceeded".into())
                    })?;
                    #[cfg(feature = "safe-sandbox")]
                    if next_depth > 64 {
                        return Err(Thrown(
                            "RangeError: array flattening nesting limit exceeded".into(),
                        ));
                    }
                    let flattened = self.flatten_array_at(&a, depth - 1, next_depth, work)?;
                    if (out.len() as u64).saturating_add(flattened.len() as u64)
                        > MAX_NATIVE_ITERATION_WORK
                    {
                        return Err(Thrown(
                            "RangeError: native builtin iteration limit exceeded".into(),
                        ));
                    }
                    out.extend(flattened);
                }
                None => {
                    if out.len() as u64 >= MAX_NATIVE_ITERATION_WORK {
                        return Err(Thrown(
                            "RangeError: native builtin iteration limit exceeded".into(),
                        ));
                    }
                    out.push(*v);
                }
            }
        }
        Ok(out)
    }

    /// Strict equality between two raw values (no register indirection). Mirrors
    /// `strict_eq` but takes values directly, for builtin use.
    /// SameValueZero — Map/Set key & element equality. Like `===` but NaN equals
    /// NaN (so NaN is a usable key and all NaNs dedupe). +0/-0 are equal here too
    /// (matching `===`); the store side normalizes -0 → +0. Strings compare by
    /// value, objects by reference identity, and there is no type coercion.
    /// Whether `v` is a JS Object (Type(v) === Object): a heap value that is not a
    /// primitive string (`Str`/`Cons`). Used by Reflect, which throws on non-objects.
    pub(crate) fn is_object_value(&self, v: Value) -> bool {
        // Symbol and BigInt are heap-allocated but are primitives, not objects
        // (typeof is "symbol"/"bigint"), so they must not count here.
        v.is_heap()
            && !self.heap.is_str_like(v.heap_index())
            && !matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
            )
    }

    /// `ToObject(v)` — backs `Object(x)` / `new Object(x)`. Primitives box into
    /// the matching wrapper (string/number/boolean/symbol/bigint); null and
    /// undefined become a fresh ordinary object; an existing object (array,
    /// function, …) is returned unchanged. `Boxed.kind`: 0=String, 1=Number,
    /// 2=Boolean, 3=Symbol, 4=BigInt.
    /// RequireObjectCoercible: `null`/`undefined` cannot be converted to an
    /// object, so methods that ToObject their `this` throw a TypeError on them.
    pub(crate) fn require_object_coercible(&self, v: Value) -> Result<(), Thrown> {
        if v == Value::NULL || v == Value::UNDEFINED {
            return Err(Thrown(
                "TypeError: cannot convert null or undefined to an object".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn to_object(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_number() {
            return Ok(Value::heap(
                self.heap.alloc(HeapObj::Boxed { kind: 1, value: v }),
            ));
        }
        if v.is_bool() {
            return Ok(Value::heap(
                self.heap.alloc(HeapObj::Boxed { kind: 2, value: v }),
            ));
        }
        if !v.is_heap() {
            // null / undefined → a fresh ordinary object.
            return Ok(Value::heap(
                self.heap.alloc(HeapObj::Object(Box::new(ObjMap::new()))),
            ));
        }
        // A heap value: string/symbol/bigint primitives box; every real object
        // (Object/Array/Func/Map/Boxed/…) is already an object → unchanged.
        let kind = match self.heap.get(v.heap_index()) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => 0u8,
            HeapObj::Symbol { .. } => 3u8,
            HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => 4u8,
            _ => return Ok(v),
        };
        Ok(Value::heap(
            self.heap.alloc(HeapObj::Boxed { kind, value: v }),
        ))
    }

    /// `ToString(v)` as a Rust String, honouring a user `toString`/`valueOf` on an
    /// object (ToPrimitive with the string hint). Primitives and engine strings use
    /// `display`; a plain object with only the built-in (native) toString also falls
    /// back to `display` (which already yields "[object Object]" / the array join).
    /// `ToString(v)` as a string VALUE: IDENTITY (the same heap string — exact
    /// WTF-8 content, lone surrogates preserved) when `v` is already a string;
    /// otherwise the `to_js_string` coercion (observable, may throw),
    /// allocated. Use this wherever the coerced string becomes a VALUE the
    /// program can read back (ToStr op, String(x), error.message, …) — the
    /// `to_js_string` + `alloc_str` pair would silently lossy-copy a
    /// lone-surrogate string.
    pub(crate) fn to_str_value(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            return Ok(v);
        }
        let s = self.to_js_string(v)?;
        Ok(self.alloc_str(s))
    }

    pub(crate) fn to_js_string(&mut self, v: Value) -> Result<String, Thrown> {
        if !v.is_heap() || self.heap.is_str_like(v.heap_index()) {
            return self.display_checked(v);
        }
        // ToString of a Symbol is a TypeError (use `.toString()` / `String(sym)`
        // explicitly instead — but even `String(sym)` routes through the dedicated
        // path, not this coercion).
        if matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
            return Err(Thrown(
                "TypeError: Cannot convert a Symbol value to a string".into(),
            ));
        }
        // ToString(bigint) is BigIntToString - direct and unobservable (the
        // user-patchable BigInt.prototype.toString must NOT run for a
        // primitive BigInt; only a Boxed BigInt object takes the protocol).
        match self.heap.get(v.heap_index()) {
            HeapObj::BigInt(b) => return Ok(b.to_string()),
            HeapObj::BigIntBig(b) => return Ok(b.to_string()),
            _ => {}
        }
        // ToString(object) is ToPrimitive(input, "string") then a string coercion;
        // honour a `@@toPrimitive` hook before falling back to toString/valueOf.
        if let Some(p) = self.symbol_to_primitive(v, "string")? {
            return self.to_js_string(p);
        }
        for name in ["toString", "valueOf"] {
            let f = self.get_prop(v, name)?;
            // `is_callable` (not `as_callable`) so native methods count — notably
            // `Function.prototype.toString`, which yields the real source text.
            if self.is_callable(f) {
                let r = self.call_value(f, v, &[])?;
                // ANY primitive result completes OrdinaryToPrimitive — recurse so a
                // BigInt stringifies (via BigInt.prototype.toString) and a Symbol
                // throws (ToString of a Symbol is a TypeError), not just strings /
                // numbers. An object result means this method didn't yield a
                // primitive, so fall through to the next.
                if !self.is_object_value(r) {
                    return self.to_js_string(r);
                }
            }
        }
        // OrdinaryToPrimitive exhausted both methods without a primitive (each
        // returned an object, or neither was callable on a null-prototype object):
        // ToPrimitive throws rather than silently producing "[object Object]".
        Err(Thrown(
            "TypeError: Cannot convert object to primitive value".into(),
        ))
    }

    /// `ToPrimitive(v, hint String)` returning the primitive Value — which may be a
    /// Symbol (so `ToPropertyKey` can use it as a symbol key rather than throwing on
    /// a stringify). Honours `@@toPrimitive`, then OrdinaryToPrimitive (`toString`
    /// then `valueOf`). A non-object value is already primitive.
    pub(crate) fn to_primitive_string(&mut self, v: Value) -> Result<Value, Thrown> {
        if !self.is_object_value(v) {
            return Ok(v); // already a primitive (string / number / Symbol / …)
        }
        if let Some(p) = self.symbol_to_primitive(v, "string")? {
            return Ok(p);
        }
        for name in ["toString", "valueOf"] {
            let f = self.get_prop(v, name)?;
            if self.is_callable(f) {
                let r = self.call_value(f, v, &[])?;
                if !self.is_object_value(r) {
                    return Ok(r);
                }
            }
        }
        Err(Thrown(
            "TypeError: Cannot convert object to primitive value".into(),
        ))
    }

    /// Whether `v` has a `[[Construct]]` slot — i.e. `new v` / `Reflect.construct`
    /// is valid. Plain functions and classes qualify; native methods, bound values,
    /// and non-callables do not. (test262's `isConstructor` helper probes this via
    /// `Reflect.construct(fn, [], v)`, so getting it right matters across the suite.)
    pub(crate) fn is_constructor(&self, mut v: Value) -> bool {
        loop {
            if !v.is_heap() {
                return false;
            }
            return match self.heap.get(v.heap_index()) {
                // A class is always a constructor. A plain function/closure is too, but a
                // generator / async / arrow / concise-method function has no [[Construct]].
                HeapObj::Class(_) => true,
                HeapObj::Func(_) | HeapObj::Closure { .. } => {
                    match self.heap.as_callable(v.heap_index()) {
                        Some((fid, _)) => {
                            let fp = self.func(fid as usize);
                            !(fp.is_generator || fp.is_async || fp.non_constructable)
                        }
                        None => true,
                    }
                }
                // The built-in constructor globals (Object/Array/Map/…) are constructors.
                HeapObj::Object(m) => m.is_ctor,
                // A bound function exposes [[Construct]] iff its target does.
                HeapObj::Bound { target, .. } => {
                    v = *target;
                    continue;
                }
                // A non-revoked Proxy is a constructor iff its target is (the
                // `construct` trap is only callable when the target has [[Construct]]).
                HeapObj::Proxy {
                    target, revoked, ..
                } => {
                    if *revoked {
                        return false;
                    }
                    v = *target;
                    continue;
                }
                _ => false,
            };
        }
    }

    /// JS `SameValue` (Object.is): like SameValueZero but +0 and -0 are distinct.
    pub(crate) fn same_value(&self, a: Value, b: Value) -> bool {
        if a.is_number() && b.is_number() {
            let (x, y) = (a.as_f64(), b.as_f64());
            if x == 0.0 && y == 0.0 {
                return x.is_sign_negative() == y.is_sign_negative();
            }
            if x.is_nan() && y.is_nan() {
                return true;
            }
            return x == y;
        }
        self.same_value_zero(a, b)
    }

    // The single implementations live in vm/collections.rs (free functions
    // over &Heap) so the collection hash index can share them exactly — its
    // SameValueZero hash must never diverge from this equality.
    pub(crate) fn same_value_zero(&self, a: Value, b: Value) -> bool {
        super::collections::svz_eq(&self.heap, a, b)
    }

    pub(crate) fn values_strict_eq(&self, a: Value, b: Value) -> bool {
        super::collections::strict_eq(&self.heap, a, b)
    }

    /// JS loose equality `==` (the Abstract Equality Comparison). Same-type
    /// compares like `===`; cross-type coerces per spec: null == undefined;
    /// number vs string coerces the string to a number; boolean coerces to a
    /// number; an object vs a primitive coerces the object to its primitive
    /// (here: string coercion, since we have no valueOf). NaN is never equal.
    /// Whether `v` is an OBJECT for abstract-equality purposes — a heap value
    /// that is not a string/symbol/bigint primitive. (Boxed wrappers count as
    /// objects; they ToPrimitive to their wrapped value.)
    fn is_eq_object(&self, v: Value) -> bool {
        v.is_heap()
            && !matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Str(_)
                    | HeapObj::Cons { .. }
                    | HeapObj::Symbol { .. }
                    | HeapObj::BigInt(_)
                    | HeapObj::BigIntBig(_)
            )
    }

    pub(crate) fn loose_eq(&mut self, a: Value, b: Value) -> Result<bool, Thrown> {
        // An [[IsHTMLDDA]] exotic (`document.all`) loosely equals null/undefined.
        if a.is_heap() && b.is_nullish() {
            if self.is_htmldda_index(a.heap_index()) {
                return Ok(true);
            }
        }
        if b.is_heap() && a.is_nullish() {
            if self.is_htmldda_index(b.heap_index()) {
                return Ok(true);
            }
        }
        // Object vs primitive (`[1] == 1`, `{} == "[object Object]"`,
        // `Object('x') == 'x'`): ToPrimitive the object side, then retry. Two
        // objects fall through to reference equality; object vs null/undefined is
        // never ToPrimitive'd (handled by the nullish check below).
        let (a_obj, b_obj) = (self.is_eq_object(a), self.is_eq_object(b));
        if a_obj && !b_obj && !b.is_nullish() {
            let pa = self.to_primitive_default(a)?;
            return self.loose_eq(pa, b);
        }
        if b_obj && !a_obj && !a.is_nullish() {
            let pb = self.to_primitive_default(b)?;
            return self.loose_eq(a, pb);
        }
        // BigInt loose equality compares mathematical values across types
        // (`1n == 1`, `1n == "1"`, `1n == true`), so handle it before the generic
        // same-tag/heap shortcuts (two distinct 1n allocations aren't bit-equal).
        let (ab, bb) = (self.bigint_val(a), self.bigint_val(b));
        if ab.is_some() || bb.is_some() {
            return Ok(match (ab, bb) {
                (Some(x), Some(y)) => x == y,
                (Some(x), None) => self.bigint_loose_eq_other(&x, b),
                (None, Some(y)) => self.bigint_loose_eq_other(&y, a),
                _ => false,
            });
        }
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
        // A Symbol primitive is never loosely equal to a Number here (two Symbols
        // are both-heap and handled above; Symbol-vs-string is both-heap too). It
        // must NOT be ToNumber'd — that throws — so short-circuit to false. This
        // is reached when the object side ToPrimitive'd to a Symbol (e.g.
        // `0 == {[Symbol.toPrimitive]: () => Symbol()}`).
        let a_sym = a.is_heap() && matches!(self.heap.get(a.heap_index()), HeapObj::Symbol { .. });
        let b_sym = b.is_heap() && matches!(self.heap.get(b.heap_index()), HeapObj::Symbol { .. });
        if a_sym || b_sym {
            return Ok(false);
        }
        // number vs string: coerce string to number.
        // string vs object / number vs object: coerce via to_number (objects
        // become NaN here, matching `1 == {}` → false; `"[object Object]"`
        // string comparisons aren't reached because both-heap is handled above).
        let an = self.to_number(a)?;
        let bn = self.to_number(b)?;
        Ok(an == bn)
    }

    /// `BigInt x == <non-BigInt other>`: compare mathematical values. Number must
    /// be a finite integer; a string is parsed as a BigInt; boolean → 0/1; an
    /// object/symbol/null/undefined is never loosely equal to a BigInt here.
    pub(crate) fn bigint_loose_eq_other(&self, x: &BigVal, other: Value) -> bool {
        if other.is_bool() {
            return *x == BigVal::Small(if other.as_bool() { 1 } else { 0 });
        }
        if other.is_number() {
            // EXACT mathematical comparison — `x as f64` would round a large
            // i128/Big and wrongly equal a nearby Number.
            return x.cmp_f64(other.as_f64()) == Some(std::cmp::Ordering::Equal);
        }
        if other.is_heap() && self.heap.is_str_like(other.heap_index()) {
            if let Some(s) = self.heap.str_cow(other.heap_index()) {
                // StrWhiteSpace includes U+FEFF (BOM), which Rust's trim does not.
                let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
                if t.is_empty() {
                    return *x == BigVal::Small(0);
                }
                return parse_bigint_str(t).is_some_and(|y| y == *x);
            }
        }
        false
    }

    // ── arithmetic / coercion helpers ──

    #[inline]
    pub(crate) fn add(&mut self, base: usize, a: u16, b: u16) -> Result<Value, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        self.add_values(va, vb)
    }

    /// Preflight a mutation of a flat guest string. The optimized append paths
    /// bypass ordinary `add_values`, so they must independently enforce both
    /// safe-profile ceilings before touching the uniquely-owned accumulator.
    #[inline]
    fn inplace_string_growth_fits(&self, idx: u32, add_units: usize, add_bytes: usize) -> bool {
        match self.heap.get(idx) {
            HeapObj::Str(string) => {
                string
                    .units()
                    .checked_add(add_units)
                    .is_some_and(|units| units <= MAX_STRING_UNITS)
                    && string
                        .as_bytes()
                        .len()
                        .checked_add(add_bytes)
                        .is_some_and(|bytes| bytes <= MAX_STRING_BYTES)
            }
            _ => false,
        }
    }

    /// The `+` operator on two already-fetched Values (shared by the interpreter's
    /// `Add`/`StrConcat` and the JIT's `jit_concat` helper).
    pub(crate) fn add_values(&mut self, va: Value, vb: Value) -> Result<Value, Thrown> {
        // Fast path: int + int with overflow check.
        if va.is_int() && vb.is_int() {
            return Ok(match va.as_int().checked_add(vb.as_int()) {
                Some(v) => Value::int(v),
                None => Value::num(va.as_int() as f64 + vb.as_int() as f64),
            });
        }
        // Fast path: string + string (the hot concat shape, incl. jit_concat) —
        // strings pass ToPrimitive unchanged, so skipping it is unobservable.
        if va.is_heap()
            && vb.is_heap()
            && self.heap.is_str_like(va.heap_index())
            && self.heap.is_str_like(vb.heap_index())
        {
            let (li, ri) = (va.heap_index(), vb.heap_index());
            let llen = self.heap.str_units(li).unwrap_or(0);
            let rlen = self.heap.str_units(ri).unwrap_or(0);
            let total = llen
                .checked_add(rlen)
                .filter(|&n| n <= MAX_STRING_UNITS)
                .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
            // Never create a zero-growth rope. Repeated `large += ""` could
            // otherwise grow an arbitrarily deep object graph while its string
            // length stayed fixed, defeating every length-derived traversal cap.
            // Keep the established fresh-result invariant for JIT concat chains
            // by materialising a fresh flat string instead of aliasing a child.
            if llen == 0 || rlen == 0 {
                return Ok(Value::heap(self.heap.alloc_concat_flat(li, ri, total)));
            }
            // A SMALL result is built flat eagerly (one alloc, no flatten
            // later); only a large one pays for a rope node.
            if total <= SMALL_CONCAT_FLAT_UNITS {
                return Ok(Value::heap(self.heap.alloc_concat_flat(li, ri, total)));
            }
            return Ok(Value::heap(self.heap.alloc_cons(li, ri, total)));
        }
        // Fast path: string + int (the `"key_" + i` map-key shape; both sides are
        // primitives, so skipping ToPrimitive is unobservable). A SMALL result is
        // built flat in ONE allocation with the int's decimal form written
        // straight into the buffer — no intermediate heap string for the number.
        if vb.is_int() && va.is_heap() {
            // B212: the hot `"prefix" + i` shape repeats a small key set, and
            // JS strings have no observable identity — serve a version-guarded
            // memoized result before formatting anything. A hit skips the
            // format, the allocation, and the whole GC life of a fresh string.
            #[cfg(not(feature = "safe-sandbox"))]
            if let Some(idx) = self.heap.concat_memo_get(va.heap_index(), vb.as_int()) {
                return Ok(Value::heap(idx));
            }
            if let Some(lu) = self.heap.str_units(va.heap_index()) {
                let (buf, start) = fmt_i32_buf(vb.as_int());
                if lu + (buf.len() - start) <= SMALL_CONCAT_FLAT_UNITS {
                    if let Some(idx) = self
                        .heap
                        .alloc_concat_str_ascii(va.heap_index(), &buf[start..])
                    {
                        #[cfg(not(feature = "safe-sandbox"))]
                        self.heap.concat_memo_put(va.heap_index(), vb.as_int(), idx);
                        return Ok(Value::heap(idx));
                    }
                }
            }
        }
        if va.is_int() && vb.is_heap() {
            if let Some(ru) = self.heap.str_units(vb.heap_index()) {
                let (buf, start) = fmt_i32_buf(va.as_int());
                if (buf.len() - start) + ru <= SMALL_CONCAT_FLAT_UNITS {
                    if let Some(idx) = self
                        .heap
                        .alloc_concat_ascii_str(&buf[start..], vb.heap_index())
                    {
                        return Ok(Value::heap(idx));
                    }
                }
            }
        }
        // ToPrimitive(default hint) each operand IN ORDER — objects (including
        // boxed wrappers like `Object(1n)` / `new Number(5)`) run the
        // observable valueOf/toString/@@toPrimitive protocol; primitives pass
        // through unchanged.
        // GC INVARIANT: `pa` (possibly a fresh primitive from va's valueOf) is
        // held in a Rust local — unrooted — while vb's coercion runs user code.
        let _gc = self.gc_lock_guard();
        let pa = self.to_primitive_default(va)?;
        let pb = self.to_primitive_default(vb)?;
        let pa_str = pa.is_heap() && self.heap.is_str_like(pa.heap_index());
        let pb_str = pb.is_heap() && self.heap.is_str_like(pb.heap_index());
        if pa_str || pb_str {
            // String concatenation. ToString of a Symbol operand throws; a
            // BigInt stringifies (decimal, no "n") via to_str_idx.
            for v in [pa, pb] {
                if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
                    return Err(Thrown(
                        "TypeError: Cannot convert a Symbol value to a string".into(),
                    ));
                }
            }
            // Build a rope (cons-string) in O(1) — children point at existing
            // flat strings / ropes, so a `s += x` loop is O(n) overall.
            let li = self.to_str_idx(pa);
            let ri = self.to_str_idx(pb);
            let llen = self.heap.str_units(li).unwrap_or(0);
            let rlen = self.heap.str_units(ri).unwrap_or(0);
            let total = llen
                .checked_add(rlen)
                .filter(|&n| n <= MAX_STRING_UNITS)
                .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
            if llen == 0 || rlen == 0 {
                return Ok(Value::heap(self.heap.alloc_concat_flat(li, ri, total)));
            }
            // Small result → flat eagerly (see the string+string fast path).
            if total <= SMALL_CONCAT_FLAT_UNITS {
                return Ok(Value::heap(self.heap.alloc_concat_flat(li, ri, total)));
            }
            return Ok(Value::heap(self.heap.alloc_cons(li, ri, total)));
        }
        // Numeric `+`: both BigInt → BigInt addition; BigInt + non-BigInt →
        // the spec's mixing TypeError; otherwise Number addition (ToNumber of
        // a Symbol throws there).
        let (ab, bb) = (self.bigint_val(pa), self.bigint_val(pb));
        match (ab, bb) {
            (Some(x), Some(y)) => self.bigint_op(crate::vm::helpers_misc::BigOp::Add, x, y),
            (None, None) => Ok(Value::num(self.to_number(pa)? + self.to_number(pb)?)),
            _ => Err(Thrown(BIGINT_MIX_ERR.into())),
        }
    }

    /// Exact literal-prefix concatenation for `Pad2Concat`:
    /// `zero == true` is `"0" + value`, otherwise `"" + value`.
    ///
    /// The compiler removes only a side-effect-free string literal evaluation.
    /// A tagged Int in the branch-compatible range can therefore return the
    /// immutable `"00".."99"` prefix slot directly. Every other Value runs the
    /// ordinary `add_values` operator with the exact literal prefix, retaining
    /// ToPrimitive, Symbol/BigInt, throw and re-entry behaviour unchanged.
    pub(crate) fn pad2_concat(&mut self, value: Value, zero: bool) -> Result<Value, Thrown> {
        if value.is_int() {
            let n = value.as_int();
            let hit = if zero {
                (0..=9).contains(&n)
            } else {
                (10..=99).contains(&n)
            };
            if hit {
                if zero {
                    pad2stats::zero();
                } else {
                    pad2stats::plain();
                }
                return Ok(Value::heap(crate::heap::INTERN_PAD2_START + n as u32));
            }
        }
        pad2stats::fallback();
        let prefix = if zero {
            Value::heap(b'0' as u32)
        } else {
            Value::heap(crate::heap::INTERN_EMPTY)
        };
        self.add_values(prefix, value)
    }

    /// Exact `value < 10 ? "0" + value : "" + value` for a compiler-proven
    /// stable binding. Only tagged Ints 0..99 bypass the source operations.
    /// Every other Value performs the full number-hint relational conversion,
    /// then the selected ordinary literal-prefix Add (which independently
    /// performs its default-hint conversion). Keeping the original object in
    /// `value` is deliberate: mutation of its coercion methods by the first
    /// conversion is observed by the second.
    pub(crate) fn pad2_conditional(&mut self, value: Value) -> Result<Value, Thrown> {
        if value.is_int() {
            let n = value.as_int();
            if (0..=99).contains(&n) {
                pad2stats::cond_hit();
                if n < 10 {
                    pad2stats::zero();
                } else {
                    pad2stats::plain();
                }
                return Ok(Value::heap(crate::heap::INTERN_PAD2_START + n as u32));
            }
        }
        pad2stats::cond_slow();
        let zero = self.cmp_lt_values(value, Value::int(10), true)?;
        self.pad2_concat(value, zero)
    }

    /// Exact `a + (b + c)` for the compiler's identifier-`+=` right-pair
    /// lowering.  The fallback literally invokes the same two `add_values`
    /// operations in the same order as the historical bytecode: `b+c` first,
    /// then `a+inner`.  This preserves ToPrimitive order, throws, BigInt/Symbol
    /// rules and every user-code re-entry.
    ///
    /// The only shortcut is fully primitive and therefore unobservable:
    /// flat-ASCII `a` and `b`, plus a flat-ASCII String or Int `c`, whose total
    /// is within the engine's ordinary small-flat bound.  It copies the three
    /// pieces into one fresh flat string rather than allocating `b+c` and then
    /// allocating/copying the outer result.  The 256-unit bound retains the
    /// rope/O(n) policy for large builders.
    pub(crate) fn add_values_right_pair(
        &mut self,
        a: Value,
        b: Value,
        c: Value,
    ) -> Result<Value, Thrown> {
        if a.is_heap() && b.is_heap() {
            let (ab, bb) = match (self.heap.get(a.heap_index()), self.heap.get(b.heap_index())) {
                (HeapObj::Str(x), HeapObj::Str(y)) if x.is_ascii() && y.is_ascii() => {
                    (x.as_bytes(), y.as_bytes())
                }
                _ => (&[][..], &[][..]),
            };
            // Empty strings are legitimate operands, so eligibility is the
            // object-shape check, not `ab`/`bb` non-emptiness.
            let left_ok = matches!(
                (self.heap.get(a.heap_index()), self.heap.get(b.heap_index())),
                (HeapObj::Str(x), HeapObj::Str(y)) if x.is_ascii() && y.is_ascii()
            );
            if left_ok {
                if c.is_heap() {
                    if let HeapObj::Str(cs) = self.heap.get(c.heap_index()) {
                        if cs.is_ascii() {
                            let cb = cs.as_bytes();
                            let total = ab.len().saturating_add(bb.len()).saturating_add(cb.len());
                            if total <= SMALL_CONCAT_FLAT_UNITS {
                                let mut out = Vec::with_capacity(total);
                                out.extend_from_slice(ab);
                                out.extend_from_slice(bb);
                                out.extend_from_slice(cb);
                                pairstats::fast_str();
                                return Ok(Value::heap(
                                    self.heap
                                        .alloc(HeapObj::Str(crate::heap::JsStr::from_ascii(out))),
                                ));
                            }
                        }
                    }
                } else if c.is_int() {
                    let (digits, start) = fmt_i32_buf(c.as_int());
                    let cb = &digits[start..];
                    let total = ab.len().saturating_add(bb.len()).saturating_add(cb.len());
                    if total <= SMALL_CONCAT_FLAT_UNITS {
                        let mut out = Vec::with_capacity(total);
                        out.extend_from_slice(ab);
                        out.extend_from_slice(bb);
                        out.extend_from_slice(cb);
                        pairstats::fast_int();
                        return Ok(Value::heap(
                            self.heap
                                .alloc(HeapObj::Str(crate::heap::JsStr::from_ascii(out))),
                        ));
                    }
                }
            }
        }
        pairstats::fallback();
        let inner = self.add_values(b, c)?;
        self.add_values(a, inner)
    }

    /// Proven-linear accumulator sibling of [`Self::add_values_right_pair`].
    /// The compiler post-pass sets `in_place` only under the same global-buffer
    /// non-alias proof that licenses `StrAppendInPlace`.  This arm additionally
    /// requires three flat ASCII primitive pieces and refuses self-aliasing
    /// leaves before touching the accumulator.  A decline delegates to the
    /// fresh-result helper with no mutation, preserving exact pairwise fallback
    /// semantics.
    pub(crate) fn add_values_right_pair_inplace(
        &mut self,
        a: Value,
        b: Value,
        c: Value,
    ) -> Result<Value, Thrown> {
        let mutable = a.is_heap()
            && a.heap_index() > crate::heap::INTERN_PINNED_END
            && matches!(self.heap.get(a.heap_index()), HeapObj::Str(s) if s.is_ascii());
        let distinct = b.is_heap()
            && b.heap_index() != a.heap_index()
            && (!c.is_heap() || c.heap_index() != a.heap_index());
        if mutable && distinct {
            let b_ok = matches!(self.heap.get(b.heap_index()), HeapObj::Str(s) if s.is_ascii());
            let c_ok = c.is_int()
                || (c.is_heap()
                    && matches!(self.heap.get(c.heap_index()), HeapObj::Str(s) if s.is_ascii()));
            if b_ok && c_ok {
                let b_len = match self.heap.get(b.heap_index()) {
                    HeapObj::Str(string) => string.as_bytes().len(),
                    _ => 0,
                };
                let c_len = if c.is_int() {
                    let (digits, start) = fmt_i32_buf(c.as_int());
                    digits.len() - start
                } else {
                    match self.heap.get(c.heap_index()) {
                        HeapObj::Str(string) => string.as_bytes().len(),
                        _ => 0,
                    }
                };
                let Some(add_len) = b_len.checked_add(c_len) else {
                    return self.add_values_right_pair(a, b, c);
                };
                if !self.inplace_string_growth_fits(a.heap_index(), add_len, add_len) {
                    return self.add_values_right_pair(a, b, c);
                }
                let ai = a.heap_index();
                let taken = std::mem::replace(
                    self.heap.get_mut(ai),
                    HeapObj::Str(crate::heap::JsStr::from_ascii(Vec::new())),
                );
                let mut out = match taken {
                    HeapObj::Str(s) => s,
                    other => {
                        *self.heap.get_mut(ai) = other;
                        return self.add_values_right_pair(a, b, c);
                    }
                };
                let bb = match self.heap.get(b.heap_index()) {
                    HeapObj::Str(s) => s.as_bytes(),
                    _ => &[],
                };
                if c.is_int() {
                    let (digits, start) = fmt_i32_buf(c.as_int());
                    out.reserve_bytes(bb.len() + digits.len() - start);
                    out.push_wtf8(bb);
                    out.push_wtf8(&digits[start..]);
                } else {
                    let cb = match self.heap.get(c.heap_index()) {
                        HeapObj::Str(s) => s.as_bytes(),
                        _ => &[],
                    };
                    out.reserve_bytes(bb.len() + cb.len());
                    out.push_wtf8(bb);
                    out.push_wtf8(cb);
                }
                *self.heap.get_mut(ai) = HeapObj::Str(out);
                pairstats::in_place();
                return Ok(a);
            }
        }
        self.add_values_right_pair(a, b, c)
    }

    /// `acc + b` for one link of a W11 (B124) fused concat chain
    /// (`StrConcatChain`): result EQUALS `add_values(acc, b)` — the `+`
    /// operator — for every operand pair; the only difference is HOW a string
    /// result is built. The accumulator walks a state machine:
    ///
    /// * `acc` is a non-interned flat `Str` (the builder — always the fresh,
    ///   dead result of the previous link, per the emitter's licence on the
    ///   `StrConcatChain` variant) → `str_append_inplace` grows the buffer in
    ///   place. Its purity gate refuses any RHS needing user code or special
    ///   `+` rules (object/Symbol/BigInt — all heap non-strings) BEFORE any
    ///   mutation, and those fall through to the full `add_values`.
    /// * `acc` is a primitive (a numeric/BigInt prefix — `1+2+'x'`), a rope
    ///   (`Cons` — the chain went large, keep O(1) rope links), or interned →
    ///   plain `add_values`, which owns every coercion/TypeError/asymptotics
    ///   rule. Semantics identity is inherited, not re-implemented.
    ///
    /// Shared verbatim by the interpreter arm, `jit_concat_chain` (MEM
    /// region) and Tier C — interpreter-vs-JIT byte identity by construction.
    pub(crate) fn add_values_chain(&mut self, acc: Value, b: Value) -> Result<Value, Thrown> {
        if acc.is_heap()
            && acc.heap_index() > crate::heap::INTERN_PINNED_END
            && matches!(self.heap.get(acc.heap_index()), HeapObj::Str(_))
        {
            if let Some(r) = self.str_append_inplace(acc, b) {
                return Ok(r);
            }
        }
        self.add_values(acc, b)
    }

    /// `acc + val` as a string append that MUTATES `acc`'s buffer in place when
    /// `acc` is a uniquely-owned, non-interned flat string (`Str` at a user heap
    /// index). Otherwise — `acc` is the interned `""`/single-char (first append),
    /// a rope, or not a string — it allocates a FRESH non-interned flat string
    /// `display(acc) + display(val)` (never interned, so the NEXT append mutates
    /// it). Correctness rests on the emitter's linearity proof: the only reference
    /// to the mutated buffer is the accumulator itself, so the mutation is
    /// unobservable. Returns the (possibly unchanged) accumulator Value.
    pub(crate) fn str_append_inplace(&mut self, acc: Value, val: Value) -> Option<Value> {
        // ── purity gate, BEFORE any mutation ── this path materialises `val`
        // via `display`, which takes `&self` and therefore CANNOT run user
        // code. That is correct for primitives (numbers, bools, null,
        // undefined — no hooks exist) and for strings (no coercion at all),
        // and WRONG for every other heap value: an object's `+` coercion runs
        // ToPrimitive (a user `toString`/`valueOf`/`@@toPrimitive`, observable
        // side effects included), and a Symbol must THROW a TypeError.
        // `display` was quietly stringifying those as "[object Object]" /
        // "Symbol(x)" — a live wrong answer on any `s += obj` inside a
        // top-level string-accumulator loop. `None` ⇒ the caller runs the
        // full generic `+` (and the JIT helper deopts); returning it before
        // touching the accumulator is what keeps the fallback re-execution
        // clean.
        if val.is_heap() && !self.heap.is_str_like(val.heap_index()) {
            return None;
        }
        // B212: a frozen (memo-served) string is aliased by the memo and by
        // every consumer it was served to — never grow it in place; the
        // fresh-buffer fallback below is exactly right for it.
        let mutable = acc.is_heap()
            && acc.heap_index() > crate::heap::INTERN_PINNED_END
            && matches!(self.heap.get(acc.heap_index()), HeapObj::Str(s) if !s.frozen());
        // `Heap::new` permanently pins the single-ASCII-character strings at
        // slots 0..127, with slot == byte. String indexing returns those exact
        // handles, so the ordinary `out += s[i]` loop can append the byte
        // without taking the accumulator out of the heap, borrowing/copying
        // the RHS `JsStr`, or running the general WTF-8 seam machinery.
        //
        // A mutable accumulator is necessarily above INTERN_PINNED_END, hence
        // distinct from the RHS slot. Appending ASCII cannot create or repair a
        // surrogate seam, and `push_ascii` updates the cached UTF-16 length.
        if mutable && val.is_heap() {
            let vi = val.heap_index();
            if vi < crate::heap::INTERN_EMPTY && ascii_char_append_enabled() {
                debug_assert!(
                    matches!(self.heap.get(vi), HeapObj::Str(s) if s.as_bytes() == [vi as u8])
                );
                if !self.inplace_string_growth_fits(acc.heap_index(), 1, 1) {
                    return None;
                }
                if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                    js.push_ascii(vi as u8);
                    return Some(acc);
                }
            }
        }
        // Fast path: appending a single decimal digit (the `s += i%10` shape) —
        // no temporary allocation for the value's string form.
        if mutable && val.is_int() {
            let n = val.as_int();
            if (0..=9).contains(&n) {
                if !self.inplace_string_growth_fits(acc.heap_index(), 1, 1) {
                    return None;
                }
                if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                    js.push_ascii(b'0' + n as u8);
                    return Some(acc);
                }
            }
            // W11 (B124): any other int — write its exact decimal form (the
            // same `fmt_i32_buf` digits the `add_values` string+int fast path
            // produces) straight into the buffer. Int leaves dominate the
            // fused concat chains (`"#" + ri(9000) + …`), and the general
            // path below would allocate a temporary heap string per leaf.
            let (buf, start) = fmt_i32_buf(n);
            let digit_len = buf.len() - start;
            if !self.inplace_string_growth_fits(acc.heap_index(), digit_len, digit_len) {
                return None;
            }
            if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                js.push_wtf8(&buf[start..]);
                return Some(acc);
            }
        }
        // W11 (B124): flat-Str RHS at a DIFFERENT slot — append its bytes
        // directly instead of round-tripping through `str_wtf8_cow(..)
        // .into_owned()` (a Rust malloc + copy + free per link; string leaves
        // dominate the fused chains alongside ints). Split-borrow safely by
        // TAKING the accumulator's `JsStr` out of its slot for the append:
        // nothing in between allocates on the VM heap, so no GC safepoint can
        // observe the placeholder. Same-index RHS (`a += a` shapes reaching
        // the legacy `StrAppendInPlace` path) and ropes keep the general path
        // below — byte-identical results either way (`push_wtf8` is the same
        // append the cow path fed).
        if mutable && val.is_heap() && val.heap_index() != acc.heap_index() {
            let ai = acc.heap_index();
            let (add_units, add_bytes) = match self.heap.get(val.heap_index()) {
                HeapObj::Str(vs) => (vs.units(), vs.as_bytes().len()),
                // Rope RHS: take the general path below.
                _ => (0, 0),
            };
            if add_units != 0 || add_bytes != 0 {
                if !self.inplace_string_growth_fits(ai, add_units, add_bytes) {
                    return None;
                }
            }
            let slot = self.heap.get_mut(ai);
            if matches!(slot, HeapObj::Str(_)) {
                let taken = std::mem::replace(
                    slot,
                    HeapObj::Str(crate::heap::JsStr::from_wtf8(Vec::new())),
                );
                let mut js = match taken {
                    HeapObj::Str(js) => js,
                    _ => unreachable!("checked Str above"),
                };
                if let HeapObj::Str(vs) = self.heap.get(val.heap_index()) {
                    js.push_wtf8(vs.as_bytes());
                    *self.heap.get_mut(ai) = HeapObj::Str(js);
                    return Some(acc);
                }
                // Rope RHS: restore the accumulator and take the general path.
                *self.heap.get_mut(ai) = HeapObj::Str(js);
            }
        }
        // General: materialise `val`'s EXACT (WTF-8) string form (same coercion
        // as `+`) — `push_wtf8` canonicalizes a high+low surrogate seam.
        let ri = self.to_str_idx(val);
        let add: Vec<u8> = self
            .heap
            .str_wtf8_cow(ri)
            .map(|c| c.into_owned())
            .unwrap_or_default();
        let add_units = self.heap.str_units(ri).unwrap_or(0);
        if mutable {
            if !self.inplace_string_growth_fits(acc.heap_index(), add_units, add.len()) {
                return None;
            }
            if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                js.push_wtf8(&add); // updates the cached unit length + ascii flag
                return Some(acc);
            }
        }
        // Fresh buffer (first append / interned / rope acc): flatten acc + add into
        // a NON-interned `Str` (bypass `alloc_str`'s interning so it's mutable next).
        let li = self.to_str_idx(acc);
        let mut s: Vec<u8> = self
            .heap
            .str_wtf8_cow(li)
            .map(|c| c.into_owned())
            .unwrap_or_default();
        let total_units = self
            .heap
            .str_units(li)
            .unwrap_or(0)
            .checked_add(add_units)?;
        let total_bytes = s.len().checked_add(add.len())?;
        if total_units > MAX_STRING_UNITS || total_bytes > MAX_STRING_BYTES {
            return None;
        }
        crate::heap::wtf8_push(&mut s, &add);
        Some(Value::heap(
            self.heap
                .alloc(HeapObj::Str(crate::heap::JsStr::from_wtf8(s))),
        ))
    }

    /// Pure prefix for the fused `acc += obj[key]` opcode. A hit cannot invoke
    /// JavaScript: `obj` is a flat ASCII string, `key` is an in-range tagged
    /// integer, and `acc` is either the mutable flat builder licensed by the
    /// compiler's existing linearity proof or its interned string seed. The
    /// seed case allocates the first mutable builder; later iterations append
    /// in place. The indexed byte is copied before borrowing `acc` mutably, so
    /// even the defensive `obj == acc` case reads the pre-append string exactly.
    ///
    /// A miss performs no mutation. The interpreter then runs the historical
    /// GetIndex + StrAppendInPlace/Add sequence; native code deopts to that arm.
    #[inline]
    pub(crate) fn str_append_index_ascii_fast(
        &mut self,
        acc: Value,
        obj: Value,
        key: Value,
    ) -> Option<Value> {
        if !acc.is_heap() || !obj.is_heap() || !key.is_int() {
            return None;
        }
        let i = key.as_int();
        if i < 0 {
            return None;
        }
        let byte = match self.heap.get(obj.heap_index()) {
            HeapObj::Str(s) if s.is_ascii() => *s.as_bytes().get(i as usize)?,
            _ => return None,
        };
        if acc.heap_index() <= crate::heap::INTERN_PINNED_END {
            if !str_append_index_first_enabled()
                || !matches!(self.heap.get(acc.heap_index()), HeapObj::Str(_))
            {
                return None;
            }
            if self.str_append_index_reserve_allowed() {
                // The pinned prefix consists only of flat ASCII strings. The
                // indexed byte was copied above, before this second borrow, so
                // `obj == acc` observes the pre-append source exactly. Build the
                // ordinary non-interned flat result in one payload allocation,
                // but with bounded spare capacity for later licensed appends.
                let seed = match self.heap.get(acc.heap_index()) {
                    HeapObj::Str(s) => s.as_bytes(),
                    _ => return None,
                };
                let len = seed.len().checked_add(1)?;
                if len > MAX_STRING_UNITS || len > MAX_STRING_BYTES {
                    return None;
                }
                let mut bytes = Vec::with_capacity(len.max(STR_APPEND_INDEX_FIRST_RESERVE));
                bytes.extend_from_slice(seed);
                bytes.push(byte);
                return Some(Value::heap(
                    self.heap
                        .alloc(HeapObj::Str(crate::heap::JsStr::from_ascii(bytes))),
                ));
            }
            // The single-byte string is permanently interned at its byte value.
            // `str_append_inplace` copies the seed + byte into a non-interned
            // flat Str, exactly like the interpreter fallback, without coercion.
            return self.str_append_inplace(acc, Value::heap(byte as u32));
        }
        if !self.inplace_string_growth_fits(acc.heap_index(), 1, 1) {
            return None;
        }
        match self.heap.get_mut(acc.heap_index()) {
            HeapObj::Str(out) => {
                out.push_ascii(byte);
                Some(acc)
            }
            _ => None,
        }
    }

    /// Heap index of a string-like object representing `v`: `v`'s own index when
    /// it is already a string (flat or rope), else a freshly allocated flat
    /// string from `v`'s string coercion. Used to build rope children.
    pub(crate) fn to_str_idx(&mut self, v: Value) -> u32 {
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

    /// ToPrimitive(v, NUMBER) for the relational operators. IsLessThan (7.2.13
    /// step 1) passes hint `number`, unlike `+`/`==` which pass `default` — so a
    /// `@@toPrimitive` hook sees "number", and a Date compares by its timestamp
    /// (`valueOf` first) rather than by its `toString`. Primitives short-circuit:
    /// this is the hot path for every loop bound.
    #[inline]
    fn to_primitive_rel(&mut self, v: Value) -> Result<Value, Thrown> {
        if !v.is_heap()
            || matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Str(_)
                    | HeapObj::Cons { .. }
                    | HeapObj::BigInt(_)
                    | HeapObj::BigIntBig(_)
                    | HeapObj::Symbol { .. }
            )
        {
            return Ok(v);
        }
        self.to_primitive_number(v)
    }

    #[inline]
    pub(crate) fn cmp_lt(
        &mut self,
        base: usize,
        a: u16,
        b: u16,
        left_first: bool,
    ) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        self.cmp_lt_values(va, vb, left_first)
    }

    /// Raw-Value sibling of `cmp_lt`, used when a fused opcode carries one
    /// operand and the other is a fixed literal. Semantics and coercion order
    /// are exactly the register form above.
    #[inline]
    pub(crate) fn cmp_lt_values(
        &mut self,
        va: Value,
        vb: Value,
        left_first: bool,
    ) -> Result<bool, Thrown> {
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() < vb.as_int());
        }
        // Abstract relational comparison: ToPrimitive (number hint) both operands,
        // then compare two strings lexicographically, else numerically. The SOURCE
        // left operand must be coerced first: `<` passes (a,b) with left_first=true;
        // `>` passes the registers SWAPPED (b,a) with left_first=false, so the
        // original left operand `b` is ToPrimitive'd before `a` (spec LeftFirst).
        let (va, vb) = if left_first {
            let pa = self.to_primitive_rel(va)?;
            let pb = self.to_primitive_rel(vb)?;
            (pa, pb)
        } else {
            let pb = self.to_primitive_rel(vb)?;
            let pa = self.to_primitive_rel(va)?;
            (pa, pb)
        };
        if let Some(o) = self.str_relational(va, vb) {
            return Ok(o.is_lt());
        }
        if let Some(ord) = self.bigint_relational(va, vb)? {
            return Ok(matches!(ord, Some(std::cmp::Ordering::Less)));
        }
        Ok(self.to_number(va)? < self.to_number(vb)?)
    }
    #[inline]
    pub(crate) fn cmp_le(
        &mut self,
        base: usize,
        a: u16,
        b: u16,
        left_first: bool,
    ) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() <= vb.as_int());
        }
        // ToPrimitive the SOURCE left operand first (see cmp_lt). `>=` swaps the
        // registers (b,a) with left_first=false so `b` coerces before `a`.
        let (va, vb) = if left_first {
            let pa = self.to_primitive_rel(va)?;
            let pb = self.to_primitive_rel(vb)?;
            (pa, pb)
        } else {
            let pb = self.to_primitive_rel(vb)?;
            let pa = self.to_primitive_rel(va)?;
            (pa, pb)
        };
        if let Some(o) = self.str_relational(va, vb) {
            return Ok(o.is_le());
        }
        if let Some(ord) = self.bigint_relational(va, vb)? {
            return Ok(matches!(
                ord,
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
            ));
        }
        Ok(self.to_number(va)? <= self.to_number(vb)?)
    }

    /// Abstract relational comparison when at least one operand is a BigInt. A
    /// BigInt must be compared by its exact mathematical value — `to_number` would
    /// round it to an f64 (so `10000000000000000000n` and `9999999999999999999n`
    /// wrongly compare equal). Returns `Ok(None)` when neither operand is a BigInt
    /// (the caller falls back to numeric comparison), `Ok(Some(None))` when the
    /// pair is unordered (the other side is NaN), else `Ok(Some(Some(ordering)))`.
    fn bigint_relational(
        &self,
        va: Value,
        vb: Value,
    ) -> Result<Option<Option<std::cmp::Ordering>>, Thrown> {
        let ab = self.bigint_val(va);
        let bb = self.bigint_val(vb);
        if ab.is_none() && bb.is_none() {
            return Ok(None);
        }
        let ord = match (ab, bb) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            (Some(x), None) => self.cmp_bigint_other(&x, vb)?,
            (None, Some(y)) => self.cmp_bigint_other(&y, va)?.map(|o| o.reverse()),
            (None, None) => unreachable!(),
        };
        Ok(Some(ord))
    }

    /// Relational comparison of a BigInt `x` against a non-BigInt operand. A
    /// STRING is StringToBigInt'd (EXACT — `"9007199254740993"` is not rounded
    /// through f64; whitespace-only → 0n; a non-integer string → undefined, i.e.
    /// unordered so every relational result is false). A Number/Boolean compares
    /// by mathematical value.
    fn cmp_bigint_other(
        &self,
        x: &BigVal,
        other: Value,
    ) -> Result<Option<std::cmp::Ordering>, Thrown> {
        if other.is_heap() && self.heap.is_str_like(other.heap_index()) {
            if let Some(s) = self.heap.str_cow(other.heap_index()) {
                let y = if s.trim().is_empty() {
                    Some(BigVal::Small(0))
                } else {
                    parse_bigint_str(&s)
                };
                return Ok(y.map(|y| x.cmp(&y)));
            }
        }
        Ok(x.cmp_f64(self.to_number(other)?))
    }

    /// JS relational comparison of two STRING operands is lexicographic by UTF-16
    /// CODE UNIT — not numeric, and not by Unicode code point. Returns the
    /// `Ordering` when both are string-like, else `None` (the caller falls back to
    /// numeric comparison).
    pub(crate) fn str_relational(&self, va: Value, vb: Value) -> Option<std::cmp::Ordering> {
        if va.is_heap()
            && vb.is_heap()
            && self.heap.is_str_like(va.heap_index())
            && self.heap.is_str_like(vb.heap_index())
        {
            let sa = self.heap.str_wtf8_cow(va.heap_index())?;
            let sb = self.heap.str_wtf8_cow(vb.heap_index())?;
            let (a, b) = (sa.as_ref(), sb.as_ref());
            // Fast path: ASCII byte order == UTF-16 code-unit order (and
            // `is_ascii` is vectorised), so the hot common case stays a byte cmp.
            if a.is_ascii() && b.is_ascii() {
                return Some(a.cmp(b));
            }
            // Non-ASCII: an astral (>BMP) char is a UTF-16 surrogate pair
            // (0xD800–0xDBFF) that sorts BELOW the 0xE000–0xFFFF BMP range, so a
            // byte (code-point-order) cmp is wrong. Compare by UTF-16 code units
            // over the WTF-8 bytes — a LONE surrogate orders by its own unit
            // value, an astral scalar by its lead surrogate first (the
            // `wtf8_units_iter` decode), exactly the spec's unit order.
            return Some(crate::heap::wtf8_units_iter(a).cmp(crate::heap::wtf8_units_iter(b)));
        }
        None
    }

    pub(crate) fn strict_eq(&self, base: usize, a: u16, b: u16) -> bool {
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
            // BigInt === BigInt compares by value (1n === 1n), not heap identity.
            // Canonical form: a Small (i128) never equals a Big (beyond-i128).
            match (self.heap.get(ai), self.heap.get(bi)) {
                (HeapObj::BigInt(x), HeapObj::BigInt(y)) => return x == y,
                (HeapObj::BigIntBig(x), HeapObj::BigIntBig(y)) => return x == y,
                _ => {}
            }
        }
        false
    }

    #[inline]
    pub(crate) fn truthy(&self, v: Value) -> bool {
        if let Some(t) = v.truthy_primitive() {
            return t;
        }
        // Heap: empty string is falsy; 0n is falsy; everything else truthy.
        if let Some(empty) = self.heap.str_is_empty(v.heap_index()) {
            return !empty;
        }
        if let HeapObj::BigInt(n) = self.heap.get(v.heap_index()) {
            return *n != 0;
        }
        // An [[IsHTMLDDA]] exotic (`document.all`) is falsy.
        if self.is_htmldda_index(v.heap_index()) {
            return false;
        }
        true
    }

    pub(crate) fn to_number(&self, v: Value) -> Result<f64, Thrown> {
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
        // A boxed primitive coerces to its wrapped value's number (ToPrimitive).
        if let HeapObj::Boxed { value, .. } = self.heap.get(v.heap_index()) {
            return self.to_number(*value);
        }
        // ToNumber of a Symbol is a TypeError.
        if matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
            return Err(Thrown(
                "TypeError: Cannot convert a Symbol value to a number".into(),
            ));
        }
        // A BigInt's numeric value (for `Number(1n)` and relational comparison;
        // arithmetic mixing is rejected earlier by `numeric_binop`).
        match self.heap.get(v.heap_index()) {
            HeapObj::BigInt(n) => return Ok(*n as f64),
            HeapObj::BigIntBig(b) => {
                use num_traits::ToPrimitive;
                // Correctly rounded; beyond-f64 magnitudes become ±Infinity.
                return Ok(b.to_f64().unwrap_or(f64::NAN));
            }
            _ => {}
        }
        if let Some(s) = self.heap.str_cow(v.heap_index()) {
            return Ok(string_to_number(&s));
        }
        Ok(f64::NAN)
    }

    /// `ToNumber(v)` that honours a user `valueOf`/`toString` when `v` is an
    /// object (ToPrimitive with the number hint) — unlike the immutable
    /// `to_number`, which returns NaN for an un-handled object. Primitives and the
    /// already-handled heap types (Date/Boxed/Symbol/BigInt/String) defer straight
    /// to `to_number`; a plain object is reduced to a primitive first.
    /// `ToIntegerOrInfinity(v)` clamped to `i64` — ToNumber then NaN→0, truncate
    /// toward zero (±Infinity saturate to i64::MAX/MIN). Backs string index/
    /// position args (`charAt`/`charCodeAt`/`at`/…), which use ToInteger, not a
    /// plain number cast (so `"42".charAt(true)` is index 1, `"1"` is 1, etc).
    pub(crate) fn to_integer_or_zero(&mut self, v: Value) -> Result<i64, Thrown> {
        let n = self.to_number_coerce(v)?;
        if n.is_nan() {
            return Ok(0);
        }
        let t = n.trunc();
        Ok(if t >= i64::MAX as f64 {
            i64::MAX
        } else if t <= i64::MIN as f64 {
            i64::MIN
        } else {
            t as i64
        })
    }

    /// `ToIntegerOrInfinity(v)` like `to_integer_or_zero`, but via the STRICT
    /// ToNumber (a BigInt or Symbol argument is a TypeError, per spec) - for
    /// String.prototype position/count arguments.
    pub(crate) fn to_integer_strict(&mut self, v: Value) -> Result<i64, Thrown> {
        let n = self.to_number_strict(v)?;
        if n.is_nan() {
            return Ok(0);
        }
        let t = n.trunc();
        Ok(if t >= i64::MAX as f64 {
            i64::MAX
        } else if t <= i64::MIN as f64 {
            i64::MIN
        } else {
            t as i64
        })
    }

    /// ToIndex(v) (ES 7.1.22): ToIntegerOrInfinity, then a RangeError if the
    /// result is negative or exceeds 2^53-1. `undefined` → 0. Backs the
    /// byteOffset/length arguments of the TypedArray/DataView constructors, so
    /// `new Int8Array(buf, -1)` throws rather than silently clamping to 0 (a bare
    /// `as usize` cast saturates a negative float to 0).
    pub(crate) fn to_index(&mut self, v: Value) -> Result<usize, Thrown> {
        // ToIndex = ToIntegerOrInfinity(value) clamped to 0..=2^53-1. The integer
        // conversion goes through ToNumber, so a BigInt or Symbol is a TypeError
        // (not silently coerced); `undefined` is 0 and NaN truncates to 0.
        if v == Value::UNDEFINED {
            return Ok(0);
        }
        let num = self.to_number_strict(v)?;
        let n = if num.is_nan() { 0.0 } else { num.trunc() };
        if n < 0.0 || n > ((1i128 << 53) - 1) as f64 {
            return Err(Thrown("RangeError: index is out of range".into()));
        }
        Ok(n as usize)
    }

    pub(crate) fn to_number_coerce(&mut self, v: Value) -> Result<f64, Thrown> {
        if !v.is_heap() {
            return self.to_number(v);
        }
        if matches!(
            self.heap.get(v.heap_index()),
            HeapObj::Date(_)
                | HeapObj::Symbol { .. }
                | HeapObj::BigInt(_)
                | HeapObj::BigIntBig(_)
                | HeapObj::Str(_)
                | HeapObj::Cons { .. }
        ) {
            return self.to_number(v);
        }
        // A boxed wrapper (and any ordinary object) ToPrimitive(number) first, so an
        // overridden valueOf/toString fires; a plain wrapper still yields its [[xData]].
        let prim = self.to_primitive_number(v)?;
        self.to_number(prim)
    }

    /// ToNumber with FULL strictness — like `to_number_coerce` (ToPrimitive on an
    /// object, honouring valueOf/@@toPrimitive and propagating an abrupt), but a
    /// BigInt OR Symbol primitive is a TypeError. The shared `to_number` is
    /// deliberately lenient on BigInt (so `1n < 2` relational comparison works), so
    /// the String code-unit/code-point statics (`fromCharCode`/`fromCodePoint`) use
    /// this instead to reject BigInt/Symbol per spec.
    pub(crate) fn to_number_strict(&mut self, v: Value) -> Result<f64, Thrown> {
        // Objects (incl. boxed wrappers) ToPrimitive(number hint) first; an
        // already-primitive value passes through unchanged.
        let prim = if v.is_heap()
            && !matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Str(_)
                    | HeapObj::Cons { .. }
                    | HeapObj::BigInt(_)
                    | HeapObj::BigIntBig(_)
                    | HeapObj::Symbol { .. }
            ) {
            self.to_primitive_number(v)?
        } else {
            v
        };
        if prim.is_heap() {
            match self.heap.get(prim.heap_index()) {
                HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => {
                    return Err(Thrown(
                        "TypeError: Cannot convert a BigInt value to a number".into(),
                    ));
                }
                HeapObj::Symbol { .. } => {
                    return Err(Thrown(
                        "TypeError: Cannot convert a Symbol value to a number".into(),
                    ));
                }
                _ => {}
            }
        }
        self.to_number(prim)
    }

    /// ToPrimitive's `@@toPrimitive` hook (ES ToPrimitive step 2a-c): if `v` is an
    /// object with a callable `Symbol.toPrimitive` ("@@toPrimitive") method, invoke
    /// it with the hint ("number" / "string" / "default") and require a primitive
    /// result (else TypeError). Returns `None` when there is no such method, so the
    /// caller falls back to OrdinaryToPrimitive (valueOf/toString). Already-primitive
    /// heap values (str/bigint/symbol) and boxed wrappers are left to the caller.
    pub(crate) fn symbol_to_primitive(
        &mut self,
        v: Value,
        hint: &str,
    ) -> Result<Option<Value>, Thrown> {
        if !v.is_heap()
            || matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Str(_)
                    | HeapObj::Cons { .. }
                    | HeapObj::BigInt(_)
                    | HeapObj::BigIntBig(_)
                    | HeapObj::Symbol { .. }
            )
        {
            return Ok(None);
        }
        // A Boxed SYMBOL falls through to the real GetMethod lookup: it finds
        // Symbol.prototype[@@toPrimitive] (returning the wrapped symbol, so
        // ToString(Object(sym)) THROWS) — unless user code deleted/redefined
        // the hook, in which case ordinary toString/valueOf semantics apply.
        // Other Boxed kinds have no @@toPrimitive on their chains - no hook.
        if let HeapObj::Boxed { kind, .. } = self.heap.get(v.heap_index()) {
            if *kind != 3 {
                return Ok(None);
            }
        }
        // GetMethod(v, @@toPrimitive): undefined/null → no hook (None); a
        // present-but-not-callable @@toPrimitive is a TypeError, not a fallthrough.
        let f = self.get_prop(v, "@@toPrimitive")?;
        if f == Value::UNDEFINED || f == Value::NULL {
            return Ok(None);
        }
        if !self.is_callable(f) {
            return Err(Thrown(
                "TypeError: Symbol.toPrimitive is not a function".into(),
            ));
        }
        let hv = self.alloc_str(hint.to_string());
        let r = self.call_value(f, v, &[hv])?;
        let is_obj = r.is_heap()
            && !matches!(
                self.heap.get(r.heap_index()),
                HeapObj::Str(_)
                    | HeapObj::Cons { .. }
                    | HeapObj::BigInt(_)
                    | HeapObj::BigIntBig(_)
                    | HeapObj::Symbol { .. }
            );
        if is_obj {
            return Err(Thrown(
                "TypeError: Cannot convert object to primitive value".into(),
            ));
        }
        Ok(Some(r))
    }

    /// ToPrimitive(v, "number"): the `@@toPrimitive` hook, else OrdinaryToPrimitive
    /// (`valueOf` then `toString`, first primitive wins; TypeError if neither does).
    pub(crate) fn to_primitive_number(&mut self, v: Value) -> Result<Value, Thrown> {
        // A boxed primitive wrapper runs OrdinaryToPrimitive (valueOf/toString) like
        // any object: a PLAIN wrapper's built-in valueOf returns its [[xData]], but an
        // OVERRIDDEN valueOf/toString/@@toPrimitive must be honoured (`+new Number(1)`
        // with a custom valueOf).
        if let Some(p) = self.symbol_to_primitive(v, "number")? {
            return Ok(p);
        }
        for name in ["valueOf", "toString"] {
            let f = self.get_prop(v, name)?;
            if self.is_callable(f) {
                let r = self.call_value(f, v, &[])?;
                let is_primitive = !r.is_heap()
                    || matches!(
                        self.heap.get(r.heap_index()),
                        HeapObj::Str(_)
                            | HeapObj::Cons { .. }
                            | HeapObj::BigInt(_)
                            | HeapObj::BigIntBig(_)
                            | HeapObj::Symbol { .. }
                    );
                if is_primitive {
                    return Ok(r);
                }
            }
        }
        Err(Thrown(
            "TypeError: Cannot convert object to primitive value".into(),
        ))
    }

    /// ToPrimitive(v) with the DEFAULT hint (used by binary `+` and `==`): an
    /// already-primitive value passes through; a Date prefers `toString` (string
    /// hint), every other object prefers `valueOf` (number hint). The
    /// `Symbol.toPrimitive` hook is not consulted yet.
    pub(crate) fn to_primitive_default(&mut self, v: Value) -> Result<Value, Thrown> {
        if !v.is_heap() {
            return Ok(v);
        }
        if matches!(
            self.heap.get(v.heap_index()),
            HeapObj::Str(_)
                | HeapObj::Cons { .. }
                | HeapObj::BigInt(_)
                | HeapObj::BigIntBig(_)
                | HeapObj::Symbol { .. }
        ) {
            return Ok(v);
        }
        // A boxed primitive wrapper runs OrdinaryToPrimitive (valueOf/toString) — a
        // plain wrapper's built-in valueOf returns its [[xData]], but an overridden
        // valueOf/toString/@@toPrimitive is honoured (`new Number(1) + 0` with a
        // custom valueOf, `==` on a wrapper, …).
        if let Some(p) = self.symbol_to_primitive(v, "default")? {
            return Ok(p);
        }
        // OrdinaryToPrimitive(O, "default") IS OrdinaryToPrimitive(O, "number") —
        // there is no Date branch in 7.1.1. What makes `"" + new Date` stringify is
        // `Date.prototype[@@toPrimitive]` (21.4.4.45), which the hook lookup above
        // already found. A `["toString", "valueOf"]` order for Dates HERE is
        // therefore dead code while the hook is installed, and wrong once it is
        // not: `delete Date.prototype[Symbol.toPrimitive]; 0 + d` must be a NUMBER
        // (staging/sm/object/toPrimitive.js).
        for name in ["valueOf", "toString"] {
            let f = self.get_prop(v, name)?;
            if self.is_callable(f) {
                let r = self.call_value(f, v, &[])?;
                let is_primitive = !r.is_heap()
                    || matches!(
                        self.heap.get(r.heap_index()),
                        HeapObj::Str(_)
                            | HeapObj::Cons { .. }
                            | HeapObj::BigInt(_)
                            | HeapObj::BigIntBig(_)
                            | HeapObj::Symbol { .. }
                    );
                if is_primitive {
                    return Ok(r);
                }
            }
        }
        Err(Thrown(
            "TypeError: Cannot convert object to primitive value".into(),
        ))
    }

    /// OrdinaryToPrimitive(O, methodNames) (ES 7.1.1.1): try each method name in
    /// `order` — `["valueOf","toString"]` for hint "number", `["toString","valueOf"]`
    /// for hint "string" — and return the first primitive result; TypeError if none.
    /// Used by `Date.prototype[Symbol.toPrimitive]`.
    pub(crate) fn ordinary_to_primitive(
        &mut self,
        v: Value,
        order: [&str; 2],
    ) -> Result<Value, Thrown> {
        for name in order {
            let f = self.get_prop(v, name)?;
            if self.is_callable(f) {
                let r = self.call_value(f, v, &[])?;
                let is_primitive = !r.is_heap()
                    || matches!(
                        self.heap.get(r.heap_index()),
                        HeapObj::Str(_)
                            | HeapObj::Cons { .. }
                            | HeapObj::BigInt(_)
                            | HeapObj::BigIntBig(_)
                            | HeapObj::Symbol { .. }
                    );
                if is_primitive {
                    return Ok(r);
                }
            }
        }
        Err(Thrown(
            "TypeError: Cannot convert object to primitive value".into(),
        ))
    }

    /// String COERCION (`String(v)`, `'' + v`, property keys). Arrays join with
    /// commas; objects become `[object Object]` — JS `toString` semantics.
    pub(crate) fn display(&self, v: Value) -> String {
        self.display_checked(v)
            .unwrap_or_else(|_| DISPLAY_LIMIT_MARKER.to_owned())
    }

    /// Fallible form used by observable ToString paths. Keeping the failure
    /// typed lets those callers surface the configured string/depth/work limit
    /// as a RangeError instead of silently accepting a truncated property key.
    pub(crate) fn display_checked(&self, v: Value) -> Result<String, Thrown> {
        let mut out = DisplayBuffer::new();
        self.display_value_into(&mut out, v, 0);
        out.finish()
    }

    /// The special `String(symbol)` / Symbol.prototype.toString form. Ordinary
    /// ToString(Symbol) must throw, so this cannot use `to_js_string`; compose
    /// the already-string description with the same output and heap checks as
    /// every other guest-derived string builder.
    pub(crate) fn symbol_descriptive_string(&mut self, symbol: Value) -> Result<String, Thrown> {
        let desc = match symbol.is_heap().then(|| self.heap.get(symbol.heap_index())) {
            Some(HeapObj::Symbol { desc, .. }) => *desc,
            _ => Value::UNDEFINED,
        };
        let description = if desc == Value::UNDEFINED {
            None
        } else {
            Some(self.display_checked(desc)?)
        };
        let mut out = String::new();
        self.append_guest_string(&mut out, "Symbol(")?;
        if let Some(description) = description {
            self.append_guest_string(&mut out, &description)?;
        }
        self.append_guest_string(&mut out, ")")?;
        Ok(out)
    }

    fn display_string_into(&self, out: &mut DisplayBuffer, idx: u32) {
        // `str_cow` would first materialize a second full-size String. Traverse
        // ropes iteratively and write their leaves straight into the result.
        let mut stack = Vec::new();
        if stack.try_reserve_exact(16).is_err() {
            out.failed = true;
            return;
        }
        stack.push(idx);
        while let Some(part) = stack.pop() {
            if !out.consume_node() {
                return;
            }
            match self.heap.get(part) {
                HeapObj::Str(s) if s.is_wellformed() => {
                    if !out.push_str(s.as_str_wf()) {
                        return;
                    }
                }
                HeapObj::Str(s) => {
                    for cp in crate::heap::wtf8_code_points(s.as_bytes()) {
                        if !out.push_char(char::from_u32(cp).unwrap_or('\u{FFFD}')) {
                            return;
                        }
                    }
                }
                HeapObj::Cons { left, right, .. } => {
                    if stack.try_reserve(2).is_err() {
                        out.failed = true;
                        return;
                    }
                    stack.push(*right);
                    stack.push(*left);
                }
                _ => return,
            }
        }
    }

    fn display_error_into(&self, out: &mut DisplayBuffer, idx: u32, depth: usize) {
        let name_start = out.out.len();
        if let Some(name) = self.read_data_prop(idx, "name") {
            self.display_value_into(out, name, depth + 1);
        } else {
            out.push_str("Error");
        }
        if out.failed {
            return;
        }
        let name_end = out.out.len();
        if name_end != name_start {
            out.push_str(": ");
        }
        let message_start = out.out.len();
        if let Some(message) = self.read_data_prop(idx, "message") {
            self.display_value_into(out, message, depth + 1);
        }
        if !out.failed && out.out.len() == message_start {
            // No message: remove the speculative separator.
            out.out.truncate(name_end);
        }
    }

    fn display_value_into(&self, out: &mut DisplayBuffer, v: Value, depth: usize) {
        if !out.consume_node() {
            return;
        }
        if depth > DISPLAY_MAX_DEPTH {
            out.failed = true;
            return;
        }

        if v.is_int() {
            let _ = write!(out, "{}", v.as_int());
        } else if v.is_double() {
            out.push_str(&fmt_f64(v.as_f64()));
        } else if v.is_bool() {
            out.push_str(if v.as_bool() { "true" } else { "false" });
        } else if v.is_null() {
            out.push_str("null");
        } else if v.is_undefined() {
            out.push_str("undefined");
        } else if v.is_heap() {
            let idx = v.heap_index();
            // Cycles through arrays/proxies/boxed values stringify as an empty
            // join element, matching the observable self-referential Array case.
            if out.active.contains(&idx) {
                return;
            }
            if out.active.try_reserve(1).is_err() {
                out.failed = true;
                return;
            }
            out.active.push(idx);

            match self.heap.get(idx) {
                HeapObj::Proxy { target, .. } => self.display_value_into(out, *target, depth + 1),
                HeapObj::Temporal { kind: 0, fields } => {
                    let mut f = [0f64; 10];
                    for (i, slot) in f.iter_mut().enumerate() {
                        *slot = f64::from_bits(*fields.get(i).unwrap_or(&0) as u64);
                    }
                    out.push_str(&duration_to_string(&f));
                }
                HeapObj::Temporal { kind: 1, fields } => {
                    out.push_str(&iso_date_string(fields[0], fields[1], fields[2]));
                }
                HeapObj::Temporal { kind: 2, fields } => {
                    let mut f = [0i64; 6];
                    for (i, slot) in f.iter_mut().enumerate() {
                        *slot = *fields.get(i).unwrap_or(&0);
                    }
                    out.push_str(&time_string(&f));
                }
                HeapObj::Temporal { kind: 3, fields } => {
                    let g = |i: usize| *fields.get(i).unwrap_or(&0);
                    out.push_str(&iso_date_string(g(0), g(1), g(2)));
                    out.push_str("T");
                    out.push_str(&time_string(&[g(3), g(4), g(5), g(6), g(7), g(8)]));
                }
                HeapObj::Temporal { kind: 4, fields } => {
                    let ns = ((fields[0] as i128) << 64) | ((fields[1] as u64) as i128);
                    out.push_str(&instant_to_string(ns));
                }
                HeapObj::Temporal { kind: 5, fields } => {
                    out.push_str(&year_month_string(fields[0], fields[1]));
                }
                HeapObj::Temporal { kind: 6, fields } => {
                    let _ = write!(out, "{:02}-{:02}", fields[1], fields[2]);
                }
                HeapObj::Temporal { kind: 7, .. } => {
                    out.push_str(&self.zdt_to_string(idx));
                }
                HeapObj::Temporal { .. } => {
                    out.push_str("[object Temporal]");
                }
                HeapObj::Intl { .. } => {
                    out.push_str("[object Object]");
                }
                HeapObj::Str(_) | HeapObj::Cons { .. } => self.display_string_into(out, idx),
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Bound { .. }
                | HeapObj::Wrapped { .. }
                | HeapObj::Native(_)
                | HeapObj::NativeClosure { .. } => {
                    out.push_str("function");
                }
                HeapObj::Cell => {
                    let inner = self.heap.cell_get(idx);
                    self.display_value_into(out, inner, depth + 1);
                }
                HeapObj::EvalScope(_) => {
                    out.push_str("[object EvalScope]");
                }
                HeapObj::Array(items) => {
                    for (i, element) in items.iter().enumerate() {
                        if i != 0 && !out.push_str(",") {
                            break;
                        }
                        if !element.is_nullish() {
                            self.display_value_into(out, *element, depth + 1);
                        }
                        if out.failed {
                            break;
                        }
                    }
                }
                HeapObj::Object(_) => {
                    if self.is_error_instance(idx) {
                        self.display_error_into(out, idx, depth);
                    } else {
                        out.push_str("[object Object]");
                    }
                }
                HeapObj::Class(class) => {
                    out.push_str("class ");
                    out.push_str(&class.name);
                    out.push_str(" { }");
                }
                HeapObj::Map { .. } => {
                    out.push_str("[object Map]");
                }
                HeapObj::Set(_) => {
                    out.push_str("[object Set]");
                }
                HeapObj::WeakMap { .. } => {
                    out.push_str("[object WeakMap]");
                }
                HeapObj::WeakSet(_) => {
                    out.push_str("[object WeakSet]");
                }
                HeapObj::WeakRef(_) => {
                    out.push_str("[object WeakRef]");
                }
                HeapObj::FinalizationRegistry { .. } => {
                    out.push_str("[object FinalizationRegistry]");
                }
                HeapObj::Iterator { .. } => {
                    out.push_str("[object Array Iterator]");
                }
                HeapObj::IterHelper { .. } => {
                    out.push_str("[object Iterator Helper]");
                }
                HeapObj::Boxed { value, .. } => self.display_value_into(out, *value, depth + 1),
                HeapObj::Symbol { desc, .. } => {
                    out.push_str("Symbol(");
                    if *desc != Value::UNDEFINED {
                        self.display_value_into(out, *desc, depth + 1);
                    }
                    out.push_str(")");
                }
                HeapObj::BigInt(number) => {
                    let _ = write!(out, "{number}");
                }
                HeapObj::BigIntBig(number) => {
                    let _ = write!(out, "{number}");
                }
                HeapObj::RegExp { source, flags, .. } => {
                    out.push_str("/");
                    out.push_str(if source.is_empty() { "(?:)" } else { source });
                    let _ = write!(out, "/{flags}");
                }
                HeapObj::TypedArray { length, .. } => {
                    for i in 0..*length {
                        if !out.consume_node() || (i != 0 && !out.push_str(",")) {
                            break;
                        }
                        if !out.push_str(&self.ta_elem_string(idx, i)) {
                            break;
                        }
                    }
                }
                HeapObj::ArrayBuffer { .. } => {
                    out.push_str("[object ArrayBuffer]");
                }
                HeapObj::DataView { .. } => {
                    out.push_str("[object DataView]");
                }
                HeapObj::Generator { .. } => {
                    out.push_str("[object Generator]");
                }
                HeapObj::AsyncGenerator(_) => {
                    out.push_str("[object AsyncGenerator]");
                }
                HeapObj::Promise { .. } => {
                    out.push_str("[object Promise]");
                }
                HeapObj::BoundResolver { .. } => {
                    out.push_str("function");
                }
                HeapObj::AsyncState(_) => {
                    out.push_str("[object Promise]");
                }
                HeapObj::Combinator { .. } | HeapObj::CombinatorResolver { .. } => {
                    out.push_str("[object Object]");
                }
                HeapObj::Date(ms) => {
                    out.push_str(&date_to_string(*ms));
                }
            };
            let popped = out.active.pop();
            debug_assert_eq!(popped, Some(idx));
        } else {
            out.push_str("undefined");
        }
    }

    /// INSPECT (`console.log` rendering). Strings are quoted only when nested;
    /// arrays/objects use node's spaced bracket style (`[ 1, 2, 3 ]`,
    /// `{ a: 1 }`). Unlike a serializer this is deliberately bounded: hostile
    /// graphs must not turn one Print instruction into unbounded native work.
    /// Render a complete console line into one bounded buffer. A per-argument
    /// cap is insufficient: thousands of individually small arguments followed
    /// by `Vec<String>::join` can otherwise allocate far beyond the output
    /// meter before that meter sees the finished line.
    pub(crate) fn inspect_line<F>(&self, count: usize, mut value_at: F) -> String
    where
        F: FnMut(usize) -> Value,
    {
        let mut out = InspectBuffer::new();
        for i in 0..count {
            if i != 0 && !out.push_char(' ') {
                break;
            }
            self.inspect_value_into(&mut out, value_at(i), false, 0);
            if out.truncated {
                break;
            }
        }
        out.finish()
    }

    fn inspect_string_into(&self, out: &mut InspectBuffer, idx: u32) {
        // Traverse ropes iteratively. A skewed `s += piece` rope can be millions
        // of nodes deep, so flattening or recursive display here would defeat
        // the console limits before a byte reached the output recorder.
        let mut stack = Vec::with_capacity(16);
        stack.push(idx);
        while let Some(part) = stack.pop() {
            if !out.consume_node() {
                return;
            }
            match self.heap.get(part) {
                HeapObj::Str(s) if s.is_wellformed() => {
                    if !out.push_str(s.as_str_wf()) {
                        return;
                    }
                }
                HeapObj::Str(s) => {
                    for cp in crate::heap::wtf8_code_points(s.as_bytes()) {
                        let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                        if !out.push_char(ch) {
                            return;
                        }
                    }
                }
                HeapObj::Cons { left, right, .. } => {
                    // Pushing right first preserves left-to-right output.
                    stack.push(*right);
                    stack.push(*left);
                }
                _ => return,
            }
        }
    }

    /// Write a function label without allocating a copy of an
    /// attacker-controlled source name.
    fn inspect_function_label_into(&self, out: &mut InspectBuffer, fid: u32) {
        let name = &self.func(fid as usize).name;
        if name.is_empty() || name.starts_with('<') {
            out.push_str("[Function (anonymous)]");
        } else {
            out.push_str("[Function: ");
            out.push_str(name.rsplit('.').next().unwrap_or(name));
            out.push_str("]");
        }
    }

    fn inspect_value_into(&self, out: &mut InspectBuffer, v: Value, nested: bool, depth: usize) {
        if !out.consume_node() {
            return;
        }
        if depth >= INSPECT_MAX_DEPTH {
            out.push_str("[MaxDepth]");
            return;
        }
        if v.is_int() {
            let _ = write!(out, "{}", v.as_int());
            return;
        }
        if v.is_double() {
            out.push_str(&fmt_f64(v.as_f64()));
            return;
        }
        if v.is_bool() {
            out.push_str(if v.as_bool() { "true" } else { "false" });
            return;
        }
        if v.is_null() {
            out.push_str("null");
            return;
        }
        if v.is_undefined() || !v.is_heap() {
            out.push_str("undefined");
            return;
        }

        let idx = v.heap_index();
        if out.active.contains(&idx) {
            out.push_str("[Circular]");
            return;
        }
        out.active.push(idx);
        self.inspect_heap_into(out, idx, nested, depth);
        let popped = out.active.pop();
        debug_assert_eq!(popped, Some(idx));
    }

    fn inspect_heap_into(&self, out: &mut InspectBuffer, idx: u32, nested: bool, depth: usize) {
        match self.heap.get(idx) {
            HeapObj::Proxy { target, .. } => {
                self.inspect_value_into(out, *target, true, depth + 1);
            }
            HeapObj::Temporal { kind: 0, fields } => {
                // Duration fields store f64 BITS in the i64 slots.
                let mut f = [0f64; 10];
                for (i, slot) in f.iter_mut().enumerate() {
                    *slot = f64::from_bits(*fields.get(i).unwrap_or(&0) as u64);
                }
                let _ = write!(out, "Temporal.Duration <{}>", duration_to_string(&f));
            }
            HeapObj::Temporal { kind: 1, fields } => {
                let _ = write!(
                    out,
                    "Temporal.PlainDate <{}>",
                    iso_date_string(fields[0], fields[1], fields[2])
                );
            }
            HeapObj::Temporal { kind: 2, fields } => {
                let mut f = [0i64; 6];
                for (i, slot) in f.iter_mut().enumerate() {
                    *slot = *fields.get(i).unwrap_or(&0);
                }
                let _ = write!(out, "Temporal.PlainTime <{}>", time_string(&f));
            }
            HeapObj::Temporal { kind: 3, fields } => {
                let get = |i: usize| *fields.get(i).unwrap_or(&0);
                let _ = write!(
                    out,
                    "Temporal.PlainDateTime <{}T{}>",
                    iso_date_string(get(0), get(1), get(2)),
                    time_string(&[get(3), get(4), get(5), get(6), get(7), get(8)])
                );
            }
            HeapObj::Temporal { kind: 4, fields } => {
                let ns = ((fields[0] as i128) << 64) | ((fields[1] as u64) as i128);
                let _ = write!(out, "Temporal.Instant <{}>", instant_to_string(ns));
            }
            HeapObj::Temporal { kind: 5, fields } => {
                let _ = write!(
                    out,
                    "Temporal.PlainYearMonth <{}>",
                    year_month_string(fields[0], fields[1])
                );
            }
            HeapObj::Temporal { kind: 6, fields } => {
                let _ = write!(
                    out,
                    "Temporal.PlainMonthDay <{:02}-{:02}>",
                    fields[1], fields[2]
                );
            }
            HeapObj::Temporal { kind: 7, .. } => {
                let _ = write!(out, "Temporal.ZonedDateTime <{}>", self.zdt_to_string(idx));
            }
            HeapObj::Temporal { .. } => {
                out.push_str("[object Temporal]");
            }
            HeapObj::Intl { kind, .. } => {
                const NAMES: [&str; 10] = [
                    "NumberFormat",
                    "DateTimeFormat",
                    "Collator",
                    "PluralRules",
                    "ListFormat",
                    "RelativeTimeFormat",
                    "Segmenter",
                    "Locale",
                    "DisplayNames",
                    "DurationFormat",
                ];
                let name = NAMES.get(*kind as usize).copied().unwrap_or("?");
                let _ = write!(out, "Intl.{name} {{}}");
            }
            HeapObj::Str(_) | HeapObj::Cons { .. } => {
                if nested {
                    out.push_char('\'');
                }
                self.inspect_string_into(out, idx);
                if nested {
                    out.push_char('\'');
                }
            }
            HeapObj::Func(fid) => self.inspect_function_label_into(out, *fid),
            HeapObj::Closure { func, .. } => self.inspect_function_label_into(out, *func),
            HeapObj::Bound { .. } => {
                out.push_str("[Function: bound]");
            }
            HeapObj::Wrapped { name, .. } => {
                if name.is_empty() {
                    out.push_str("[Function (anonymous)]");
                } else {
                    out.push_str("[Function: ");
                    out.push_str(name);
                    out.push_char(']');
                }
            }
            HeapObj::Native(_) | HeapObj::NativeClosure { .. } => {
                out.push_str("[Function (native)]");
            }
            HeapObj::Cell => {
                let inner = self.heap.cell_get(idx);
                self.inspect_value_into(out, inner, true, depth + 1);
            }
            HeapObj::EvalScope(_) => {
                out.push_str("[object EvalScope]");
            }
            HeapObj::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push_str("[ ");
                for (i, value) in items.iter().enumerate() {
                    if i != 0 {
                        out.push_str(", ");
                    }
                    self.inspect_value_into(out, *value, true, depth + 1);
                    if out.truncated {
                        break;
                    }
                }
                out.push_str(" ]");
            }
            HeapObj::Object(map) => {
                // A class instance prints with its constructor name (`Pt { ... }`).
                if let Some(class_idx) = map.class {
                    if let HeapObj::Class(class) = self.heap.get(class_idx) {
                        out.push_str(&class.name);
                        out.push_char(' ');
                    }
                }
                if map.keys.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push_str("{ ");
                for (i, (key, value)) in map.keys.iter().zip(map.vals_slice().iter()).enumerate() {
                    if i != 0 {
                        out.push_str(", ");
                    }
                    out.push_str(key);
                    out.push_str(": ");
                    self.inspect_value_into(out, *value, true, depth + 1);
                    if out.truncated {
                        break;
                    }
                }
                out.push_str(" }");
            }
            HeapObj::Class(class) => {
                out.push_str("[class ");
                out.push_str(&class.name);
                out.push_char(']');
            }
            HeapObj::Map { keys, vals } => {
                let _ = write!(out, "Map({}) {{", keys.len());
                if keys.is_empty() {
                    out.push_char('}');
                    return;
                }
                out.push_char(' ');
                for (i, (key, value)) in keys.iter().zip(vals.iter()).enumerate() {
                    if i != 0 {
                        out.push_str(", ");
                    }
                    self.inspect_value_into(out, *key, true, depth + 1);
                    out.push_str(" => ");
                    self.inspect_value_into(out, *value, true, depth + 1);
                    if out.truncated {
                        break;
                    }
                }
                out.push_str(" }");
            }
            HeapObj::Set(items) => {
                let _ = write!(out, "Set({}) {{", items.len());
                if items.is_empty() {
                    out.push_char('}');
                    return;
                }
                out.push_char(' ');
                for (i, value) in items.iter().enumerate() {
                    if i != 0 {
                        out.push_str(", ");
                    }
                    self.inspect_value_into(out, *value, true, depth + 1);
                    if out.truncated {
                        break;
                    }
                }
                out.push_str(" }");
            }
            HeapObj::WeakMap { .. } => {
                out.push_str("WeakMap { <items unknown> }");
            }
            HeapObj::WeakSet(_) => {
                out.push_str("WeakSet { <items unknown> }");
            }
            HeapObj::WeakRef(_) => {
                out.push_str("WeakRef {}");
            }
            HeapObj::FinalizationRegistry { .. } => {
                out.push_str("FinalizationRegistry {}");
            }
            HeapObj::Iterator { .. } => {
                out.push_str("Object [Array Iterator] {}");
            }
            HeapObj::IterHelper { .. } => {
                out.push_str("Object [Iterator Helper] {}");
            }
            HeapObj::Boxed { kind, value } => {
                let prefix = match kind {
                    0 => "[String: ",
                    1 => "[Number: ",
                    _ => "[Boolean: ",
                };
                out.push_str(prefix);
                self.inspect_value_into(out, *value, true, depth + 1);
                out.push_char(']');
            }
            HeapObj::Symbol { desc, .. } => {
                out.push_str("Symbol(");
                if *desc != Value::UNDEFINED {
                    self.inspect_value_into(out, *desc, false, depth + 1);
                }
                out.push_char(')');
            }
            // console.log shows BigInt with the `n` suffix (1n), unlike ToString.
            HeapObj::BigInt(n) => {
                let _ = write!(out, "{n}n");
            }
            HeapObj::BigIntBig(big) => {
                let _ = write!(out, "{big}n");
            }
            HeapObj::RegExp { source, flags, .. } => {
                out.push_char('/');
                out.push_str(if source.is_empty() { "(?:)" } else { source });
                out.push_char('/');
                out.push_str(flags);
            }
            HeapObj::TypedArray { kind, length, .. } => {
                let name = native::TA_KINDS[*kind as usize].0;
                let len = *length;
                let _ = write!(out, "{name}({len}) [");
                if len == 0 {
                    out.push_char(']');
                    return;
                }
                out.push_char(' ');
                for i in 0..len {
                    if !out.consume_node() {
                        break;
                    }
                    if i != 0 {
                        out.push_str(", ");
                    }
                    let element = self.ta_elem_string(idx, i);
                    out.push_str(&element);
                    if out.truncated {
                        break;
                    }
                }
                out.push_str(" ]");
            }
            HeapObj::ArrayBuffer { data, .. } => {
                let _ = write!(out, "ArrayBuffer {{ byteLength: {} }}", data.len());
            }
            HeapObj::DataView { byte_length, .. } => {
                let _ = write!(out, "DataView {{ byteLength: {byte_length} }}");
            }
            HeapObj::Generator { .. } => {
                out.push_str("Object [Generator] {}");
            }
            HeapObj::AsyncGenerator(_) => {
                out.push_str("Object [AsyncGenerator] {}");
            }
            HeapObj::Promise { state, result, .. } => match state {
                crate::heap::PromiseState::Pending => {
                    out.push_str("Promise { <pending> }");
                }
                crate::heap::PromiseState::Fulfilled => {
                    out.push_str("Promise { ");
                    self.inspect_value_into(out, *result, true, depth + 1);
                    out.push_str(" }");
                }
                crate::heap::PromiseState::Rejected => {
                    out.push_str("Promise { <rejected> ");
                    self.inspect_value_into(out, *result, true, depth + 1);
                    out.push_str(" }");
                }
            },
            HeapObj::BoundResolver { .. } => {
                out.push_str("[Function (anonymous)]");
            }
            // Internal: never user-visible (an async call yields its Promise).
            HeapObj::AsyncState(_) => {
                out.push_str("Promise { <pending> }");
            }
            HeapObj::Combinator { .. } | HeapObj::CombinatorResolver { .. } => {
                out.push_str("[object Object]");
            }
            // node renders a Date in console.log as its ISO string (unquoted).
            HeapObj::Date(ms) => {
                if ms.is_nan() {
                    out.push_str("Invalid Date");
                } else {
                    let iso = date_to_iso(*ms);
                    out.push_str(&iso);
                }
            }
        }
    }

    /// Resolve a constant slot: most are plain Values; string constants are
    /// stored as a sentinel index into the function's `string_constants` and
    /// interned to a heap string on first use.
    /// A `wtf8_consts`-flagged slot holds the oxc lone-surrogate MARKER form —
    /// decoded here into a real WTF-8 string (this is where `'\uD800'` becomes
    /// a 1-unit lone-surrogate string).
    #[inline]
    pub(crate) fn resolve_const(&mut self, func_id: u32, v: Value) -> Value {
        // String constants are encoded as `Value::heap(STRING_CONST_BIT | i)`.
        if v.is_heap() && (v.heap_index() & STRING_CONST_BIT) != 0 {
            let si = (v.heap_index() & !STRING_CONST_BIT) as usize;
            let f = self.func(func_id as usize);
            if !f.wtf8_consts.is_empty() && f.wtf8_consts.binary_search(&(si as u32)).is_ok() {
                let bytes = crate::heap::decode_lone_surrogate_markers(&f.string_constants[si]);
                let js = crate::heap::JsStr::from_wtf8(bytes);
                return Value::heap(self.heap.alloc_js(js));
            }
            // `typeof` itself returns one of these permanently interned handles.
            // Reuse the same handle for an equal ordinary literal so the common
            // unfused `var t = typeof v; t === "number"` shape reaches the
            // pointer-equality fast path without allocating a second string.
            if let Some(code) = crate::bytecode::typeof_code(&f.string_constants[si]) {
                let interned = self.typeof_strs[code as usize];
                if !interned.is_undefined() {
                    return interned;
                }
            }
            let s = f.string_constants[si].clone();
            return self.alloc_str(s);
        }
        v
    }
}

/// Native prefix for [`Vm::str_append_index_ascii_fast`]. A miss is the normal
/// deopt sentinel: no state changed, so the interpreter can execute the fused
/// opcode's exact GetIndex + append fallback once.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_str_append_index_ascii(
    vm: *mut core::ffi::c_void,
    acc_bits: u64,
    obj_bits: u64,
    key_bits: u64,
) -> u64 {
    // SAFETY: native regions hold the VM exclusively while calling helpers.
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.str_append_index_ascii_fast(
        Value::from_bits(acc_bits),
        Value::from_bits(obj_bits),
        Value::from_bits(key_bits),
    ) {
        Some(v) => v.bits(),
        None => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Exact ordering of an i128 (a BigInt) against an f64, comparing mathematical
/// values without rounding the integer. `None` means unordered (the f64 is NaN).
pub(crate) fn cmp_i128_f64(x: i128, y: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if y.is_nan() {
        return None;
    }
    if y == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if y == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    // 2^127 — any finite f64 at or beyond this magnitude is outside i128's range.
    const TWO_127: f64 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0;
    if y >= TWO_127 {
        return Some(Ordering::Less);
    }
    if y < -TWO_127 {
        return Some(Ordering::Greater);
    }
    // y is finite and in (-2^127, 2^127): floor(y) is an integer-valued f64 that
    // converts to i128 exactly. Compare the integer parts, then break a tie by the
    // fractional part of y.
    let yf = y.floor();
    let yi = yf as i128;
    Some(match x.cmp(&yi) {
        Ordering::Equal if y > yf => Ordering::Less, // x == floor(y) < y
        ord => ord,
    })
}

#[cfg(test)]
mod display_safety_tests {
    use super::*;

    fn program(source: &str) -> Program {
        let ast = crate::front::parse_script(source).expect("source parses");
        crate::compile::compile_program(&ast, source).expect("source compiles")
    }

    #[test]
    fn cyclic_array_display_is_total_and_matches_join_semantics() {
        let program = program("var x = 0;");
        let mut vm = Vm::new(&program);
        vm.run().expect("program runs");

        let idx = vm.heap.alloc(HeapObj::Array(Vec::new()));
        let value = Value::heap(idx);
        match vm.heap.get_mut(idx) {
            HeapObj::Array(items) => items.extend([Value::int(1), value, Value::int(2)]),
            _ => unreachable!(),
        }
        assert_eq!(vm.display(value), "1,,2");
    }

    #[test]
    fn excessive_display_graph_depth_fails_without_native_recursion_overflow() {
        let program = program("var x = 0;");
        let mut vm = Vm::new(&program);
        vm.run().expect("program runs");

        let mut value = Value::int(1);
        for _ in 0..=DISPLAY_MAX_DEPTH {
            value = Value::heap(vm.heap.alloc(HeapObj::Array(vec![value])));
        }
        assert!(vm.display_checked(value).is_err());
        assert_eq!(vm.display(value), DISPLAY_LIMIT_MARKER);
    }

    #[cfg(feature = "safe-sandbox")]
    #[test]
    fn aliased_large_strings_cannot_amplify_transient_display_allocations() {
        let program = program("var x = 0;");
        let mut vm = Vm::new(&program);
        vm.run().expect("program runs");

        let large = Value::heap(vm.heap.alloc_str("x".repeat(MAX_STRING_BYTES / 2 + 1)));
        let array = Value::heap(vm.heap.alloc(HeapObj::Array(vec![large, large])));
        assert!(vm.display_checked(array).is_err());
        assert_eq!(vm.display(array), DISPLAY_LIMIT_MARKER);
    }
}

#[cfg(test)]
mod resolve_const_tests {
    use super::*;

    fn global_value(vm: &Vm<'_>, program: &Program, name: &str) -> Value {
        let slot = program
            .global_names
            .iter()
            .position(|candidate| candidate == name)
            .unwrap_or_else(|| panic!("missing global slot for {name}"));
        vm.globals[slot]
    }

    #[test]
    fn typeof_name_literals_reuse_the_permanent_handles() {
        let src = r#"
            var t0 = "number";
            var t1 = "string";
            var t2 = "boolean";
            var t3 = "undefined";
            var t4 = "object";
            var t5 = "function";
            var t6 = "symbol";
            var t7 = "bigint";
            var miss0 = "Number";
            var miss1 = "number ";
            var lone = "\uD800";
            var fromEval = "before";
            eval("fromEval = 'bigint'");
        "#;
        let ast = crate::front::parse_script(src).expect("source parses");
        let program = crate::compile::compile_program(&ast, src).expect("source compiles");
        assert!(
            !program.functions[0].wtf8_consts.is_empty(),
            "the lone-surrogate case must exercise the WTF-8 branch"
        );

        let mut vm = Vm::new(&program);
        vm.run().expect("program runs");

        for (i, name) in crate::bytecode::TYPEOF_NAMES.iter().enumerate() {
            let literal = global_value(&vm, &program, &format!("t{i}"));
            assert_eq!(literal, vm.typeof_strs[i], "literal {name:?}");
            assert_eq!(vm.display(literal), *name);
        }

        for (binding, text) in [("miss0", "Number"), ("miss1", "number ")] {
            let literal = global_value(&vm, &program, binding);
            assert!(
                vm.typeof_strs.iter().all(|&interned| literal != interned),
                "non-matching literal {text:?} reused a typeof handle"
            );
            assert_eq!(vm.display(literal), text);
        }

        let lone = global_value(&vm, &program, "lone");
        assert!(vm.typeof_strs.iter().all(|&interned| lone != interned));
        match vm.heap.get(lone.heap_index()) {
            HeapObj::Str(s) => assert_eq!(s.units(), 1),
            other => panic!("lone-surrogate literal resolved to {other:?}"),
        }

        // Direct eval functions live in `eval_funcs`, not `program.functions`;
        // this proves their constants take the same resolve_const path.
        assert_eq!(global_value(&vm, &program, "fromEval"), vm.typeof_strs[7]);
    }
}

#[cfg(test)]
mod add_values_fresh_index_tests {
    use super::*;

    /// The JIT chain arms' same-bits refetch elision rests on this: a `+`
    /// through `add_values` with a heap (string) LHS NEVER returns the LHS's
    /// own index — every string-producing arm allocates a fresh slot
    /// (`alloc_concat_flat`/`alloc_cons`/`alloc_concat_str_ascii`). A future
    /// identity fast path (e.g. `s + ""` returning `s`'s own bits) would
    /// silently break `jit_concat_chain_fast`'s contract; this pins it.
    #[test]
    fn add_values_never_returns_lhs_index() {
        let src = "var x = 0;";
        let ast = crate::front::parse_script(src).expect("source parses");
        let program = crate::compile::compile_program(&ast, src).expect("source compiles");
        let mut vm = Vm::new(&program);
        vm.run().expect("program runs");

        let lhs = Value::heap(
            vm.heap
                .alloc(HeapObj::Str(crate::heap::JsStr::new("acc".into()))),
        );
        let small_str = Value::heap(
            vm.heap
                .alloc(HeapObj::Str(crate::heap::JsStr::new("tail".into()))),
        );
        // Large RHS → the cons (rope) arm; small ones → the flat arms.
        let big_str = Value::heap(
            vm.heap
                .alloc(HeapObj::Str(crate::heap::JsStr::new("y".repeat(4096)))),
        );
        let empty = Value::heap(crate::heap::INTERN_EMPTY);
        let rhss = [
            small_str,
            big_str,
            empty,
            lhs, // self-concat
            Value::int(7),
            Value::int(-2147483648),
            Value::num(3.5),
            Value::TRUE,
            Value::NULL,
            Value::UNDEFINED,
        ];
        for rhs in rhss {
            let r = vm.add_values(lhs, rhs).expect("string + succeeds");
            assert!(r.is_heap(), "string + produced a non-string");
            assert_ne!(
                r.heap_index(),
                lhs.heap_index(),
                "add_values returned its heap LHS's own index — the chain \
                 fast helper's same-bits elision premise is broken"
            );
            // And symmetrically for a heap RHS: `x + s` never returns `s`.
            let r2 = vm.add_values(rhs, lhs).expect("+ succeeds");
            if r2.is_heap() {
                assert_ne!(r2.heap_index(), lhs.heap_index());
            }
        }
    }
}

#[cfg(test)]
mod str_append_ascii_char_tests {
    use super::*;

    fn vm() -> Vm<'static> {
        let src = "var x = 0;";
        let ast = crate::front::parse_script(src).expect("source parses");
        let program = Box::leak(Box::new(
            crate::compile::compile_program(&ast, src).expect("source compiles"),
        ));
        let mut vm = Vm::new(program);
        vm.run().expect("program runs");
        vm
    }

    fn mutable_str(vm: &mut Vm<'_>, s: &str) -> Value {
        Value::heap(
            vm.heap
                .alloc(HeapObj::Str(crate::heap::JsStr::new(s.into()))),
        )
    }

    #[test]
    fn every_interned_ascii_byte_appends_exactly_without_heap_allocation() {
        let mut vm = vm();
        let acc = mutable_str(&mut vm, "seed");
        let heap_len = vm.heap.len();
        let mut expected = b"seed".to_vec();

        for byte in 0u8..128 {
            let rhs = Value::heap(byte as u32);
            assert_eq!(vm.str_append_inplace(acc, rhs), Some(acc));
            assert_eq!(
                vm.heap.len(),
                heap_len,
                "byte {byte:#04x} allocated on the VM heap"
            );
            expected.push(byte);
        }

        match vm.heap.get(acc.heap_index()) {
            HeapObj::Str(s) => {
                assert_eq!(s.as_bytes(), expected);
                assert_eq!(s.units(), expected.len());
                assert!(s.is_ascii());
                assert!(s.is_wellformed());
            }
            other => panic!("ASCII append changed the builder kind: {other:?}"),
        }
    }

    #[test]
    fn nonascii_and_wtf8_metadata_and_seams_stay_exact() {
        let mut vm = vm();

        // An ASCII leaf on a non-ASCII flat builder uses the new arm. It must
        // leave the builder non-ASCII while preserving exact UTF-16 units.
        let unicode = mutable_str(&mut vm, "é☃");
        assert_eq!(
            vm.str_append_inplace(unicode, Value::heap(b'!' as u32)),
            Some(unicode)
        );
        match vm.heap.get(unicode.heap_index()) {
            HeapObj::Str(s) => {
                assert_eq!(s.as_bytes(), "é☃!".as_bytes());
                assert_eq!(s.units(), 3);
                assert!(!s.is_ascii());
                assert!(s.is_wellformed());
            }
            other => panic!("Unicode builder changed kind: {other:?}"),
        }

        // A lone surrogate remains lone when followed by ASCII; push_ascii
        // neither hides it nor perturbs its cached unit count.
        let lone = Value::heap(
            vm.heap
                .alloc(HeapObj::Str(crate::heap::JsStr::from_code_point(0xD83D))),
        );
        assert_eq!(
            vm.str_append_inplace(lone, Value::heap(b'A' as u32)),
            Some(lone)
        );
        match vm.heap.get(lone.heap_index()) {
            HeapObj::Str(s) => {
                assert_eq!(s.units(), 2);
                assert!(!s.is_ascii());
                assert!(!s.is_wellformed());
            }
            other => panic!("lone-surrogate builder changed kind: {other:?}"),
        }

        // A non-ASCII/lone-surrogate RHS is outside the new guard and keeps
        // using push_wtf8, including canonicalization of a surrogate seam.
        let hi = Value::heap(
            vm.heap
                .alloc(HeapObj::Str(crate::heap::JsStr::from_code_point(0xD83D))),
        );
        let lo = Value::heap(
            vm.heap
                .alloc(HeapObj::Str(crate::heap::JsStr::from_code_point(0xDE00))),
        );
        assert_eq!(vm.str_append_inplace(hi, lo), Some(hi));
        match vm.heap.get(hi.heap_index()) {
            HeapObj::Str(s) => {
                assert_eq!(s.as_bytes(), "😀".as_bytes());
                assert_eq!(s.units(), 2);
                assert!(!s.is_ascii());
                assert!(s.is_wellformed());
            }
            other => panic!("surrogate pair changed builder kind: {other:?}"),
        }
    }

    #[test]
    fn interned_shared_and_self_alias_accumulators_preserve_string_values() {
        let mut vm = vm();

        // Interned accumulators are immutable. The first append allocates a
        // fresh builder and leaves both pinned character slots untouched.
        let interned = Value::heap(b'x' as u32);
        let appended = vm
            .str_append_inplace(interned, Value::heap(b'y' as u32))
            .expect("primitive append");
        assert_ne!(appended, interned);
        assert_eq!(vm.display(interned), "x");
        assert_eq!(vm.display(Value::heap(b'y' as u32)), "y");
        assert_eq!(vm.display(appended), "xy");

        // Same-slot `a += a` deliberately misses the distinct-flat-string arm
        // and goes through the general owned-byte path; no split borrow occurs.
        let self_alias = mutable_str(&mut vm, "ab");
        assert_eq!(
            vm.str_append_inplace(self_alias, self_alias),
            Some(self_alias)
        );
        assert_eq!(vm.display(self_alias), "abab");

        // Compiler-level snapshots are the observable alias hazard. If its
        // linearity proof ever licenses mutation while `held`/`snap` are live,
        // either prefix below grows with the final builder.
        let out = crate::run(
            r#"
                "use strict";
                function render(input) {
                    var out = "seed", held = out, snap = "";
                    for (var i = 0; i < input.length; i++) {
                        out += input[i];
                        if (i === 10) snap = out;
                    }
                    return held + "|" + snap + "|" + out.length + "|" + out.slice(-4);
                }
                console.log(render("ab".repeat(25000)));
            "#,
        )
        .expect("alias fixture compiles");
        assert!(out.error.is_none(), "alias fixture error: {:?}", out.error);
        assert_eq!(out.output, vec!["seed|seedabababababa|50004|abab"]);
    }

    #[test]
    fn primitive_coercions_and_heap_fallbacks_do_not_mutate_before_decline() {
        let mut vm = vm();
        let acc = mutable_str(&mut vm, "v=");

        for (value, suffix) in [
            (Value::TRUE, "true"),
            (Value::NULL, "null"),
            (Value::UNDEFINED, "undefined"),
            (Value::num(3.5), "3.5"),
        ] {
            assert_eq!(vm.str_append_inplace(acc, value), Some(acc));
            assert!(vm.display(acc).ends_with(suffix));
        }
        let before = vm.display(acc);

        // Objects can run user coercion and Symbols must throw under ordinary
        // `+`; the append helper must decline before touching the builder.
        let object = Value::heap(vm.obj_proto);
        assert_eq!(vm.str_append_inplace(acc, object), None);
        assert_eq!(vm.display(acc), before);
        let generic = vm
            .add_values(acc, object)
            .expect("ordinary object coercion");
        assert_ne!(generic, acc);
        assert_eq!(vm.display(acc), before);

        let desc = Value::heap(crate::heap::INTERN_EMPTY);
        let symbol = vm.make_symbol(desc);
        assert_eq!(vm.str_append_inplace(acc, symbol), None);
        assert_eq!(vm.display(acc), before);
        assert!(
            vm.add_values(acc, symbol).is_err(),
            "ordinary string + Symbol must throw"
        );
        assert_eq!(vm.display(acc), before);
    }

    #[cfg(feature = "safe-sandbox")]
    #[test]
    fn optimized_append_paths_stop_at_the_safe_string_ceiling_without_mutation() {
        let mut vm = vm();

        for rhs in [Value::heap(b'x' as u32), Value::int(42), Value::TRUE] {
            let acc = mutable_str(&mut vm, &"a".repeat(MAX_STRING_UNITS));
            assert_eq!(vm.str_append_inplace(acc, rhs), None);
            assert_eq!(vm.heap.str_units(acc.heap_index()), Some(MAX_STRING_UNITS));
            assert!(vm.add_values(acc, rhs).is_err());
            assert_eq!(vm.heap.str_units(acc.heap_index()), Some(MAX_STRING_UNITS));
        }

        let acc = mutable_str(&mut vm, &"a".repeat(MAX_STRING_UNITS));
        let rhs = mutable_str(&mut vm, "tail");
        assert_eq!(vm.str_append_inplace(acc, rhs), None);
        assert_eq!(vm.heap.str_units(acc.heap_index()), Some(MAX_STRING_UNITS));

        let self_alias = mutable_str(&mut vm, &"a".repeat(MAX_STRING_UNITS));
        assert_eq!(vm.str_append_inplace(self_alias, self_alias), None);
        assert_eq!(
            vm.heap.str_units(self_alias.heap_index()),
            Some(MAX_STRING_UNITS)
        );
    }
}

#[cfg(test)]
mod str_append_index_ascii_tests {
    use super::*;

    fn vm() -> Vm<'static> {
        let src = "var x = 0;";
        let ast = crate::front::parse_script(src).expect("source parses");
        let program = Box::leak(Box::new(
            crate::compile::compile_program(&ast, src).expect("source compiles"),
        ));
        let mut vm = Vm::new(program);
        vm.run().expect("program runs");
        vm
    }

    fn flat(vm: &mut Vm<'_>, s: crate::heap::JsStr) -> Value {
        Value::heap(vm.heap.alloc(HeapObj::Str(s)))
    }

    #[test]
    fn copies_every_ascii_byte_without_heap_allocation() {
        let mut vm = vm();
        let source: String = (0u8..128).map(char::from).collect();
        let source = flat(&mut vm, crate::heap::JsStr::new(source));
        let out = flat(&mut vm, crate::heap::JsStr::new(String::new()));
        let heap_len = vm.heap.len();

        for i in 0..128 {
            assert_eq!(
                vm.str_append_index_ascii_fast(out, source, Value::int(i)),
                Some(out)
            );
        }
        assert_eq!(vm.heap.len(), heap_len);
        match vm.heap.get(out.heap_index()) {
            HeapObj::Str(s) => {
                assert_eq!(s.as_bytes(), &(0u8..128).collect::<Vec<_>>());
                assert_eq!(s.units(), 128);
                assert!(s.is_ascii());
            }
            other => panic!("builder changed kind: {other:?}"),
        }
    }

    #[cfg(feature = "safe-sandbox")]
    #[test]
    fn indexed_ascii_append_stops_at_the_safe_string_ceiling() {
        let mut vm = vm();
        let source = Value::heap(b'x' as u32);
        let out = flat(
            &mut vm,
            crate::heap::JsStr::new("a".repeat(MAX_STRING_UNITS)),
        );
        assert_eq!(
            vm.str_append_index_ascii_fast(out, source, Value::int(0)),
            None
        );
        assert_eq!(vm.heap.str_units(out.heap_index()), Some(MAX_STRING_UNITS));
    }

    #[test]
    fn same_slot_alias_reads_before_appending() {
        let mut vm = vm();
        let out = flat(&mut vm, crate::heap::JsStr::new("abc".into()));
        assert_eq!(
            vm.str_append_index_ascii_fast(out, out, Value::int(1)),
            Some(out)
        );
        assert_eq!(vm.display(out), "abcb");
    }

    #[test]
    fn every_miss_is_pristine_including_unicode_and_wtf8() {
        let mut vm = vm();
        let out = flat(&mut vm, crate::heap::JsStr::new("seed".into()));
        let unicode = flat(&mut vm, crate::heap::JsStr::new("A😀".into()));
        let lone = flat(&mut vm, crate::heap::JsStr::from_code_point(0xD800));

        for (acc, obj, key) in [
            (out, unicode, Value::int(0)),
            (out, lone, Value::int(0)),
            (out, out, Value::int(-1)),
            (out, out, Value::int(99)),
            (out, out, Value::num(1.5)),
        ] {
            assert_eq!(vm.str_append_index_ascii_fast(acc, obj, key), None);
            assert_eq!(vm.display(out), "seed");
        }
    }

    #[test]
    fn interned_seed_allocates_the_first_mutable_builder() {
        let mut vm = vm();
        let source = flat(&mut vm, crate::heap::JsStr::new("AZ".into()));
        let seed = Value::heap(crate::heap::INTERN_EMPTY);
        let heap_len = vm.heap.len();

        let out = vm
            .str_append_index_ascii_fast(seed, source, Value::int(1))
            .expect("interned seed takes the pure first-append path");
        assert_ne!(out, seed);
        assert_eq!(vm.heap.len(), heap_len + 1);
        assert_eq!(vm.display(seed), "");
        assert_eq!(vm.display(out), "Z");
        assert!(out.heap_index() > crate::heap::INTERN_PINNED_END);
        match vm.heap.get(out.heap_index()) {
            HeapObj::Str(s) => assert!(
                s.byte_capacity() >= STR_APPEND_INDEX_FIRST_RESERVE,
                "first builder did not receive the bounded reserve"
            ),
            other => panic!("first builder changed heap kind: {other:?}"),
        }
    }

    #[test]
    fn interned_seed_alias_reads_before_reserved_builder_allocation() {
        let mut vm = vm();
        let seed = Value::heap(crate::heap::INTERN_PAD2_START);
        assert_eq!(vm.display(seed), "00");
        let heap_len = vm.heap.len();

        let out = vm
            .str_append_index_ascii_fast(seed, seed, Value::int(0))
            .expect("pinned source/seed alias remains a pure hit");
        assert_ne!(out, seed);
        assert_eq!(vm.heap.len(), heap_len + 1);
        assert_eq!(vm.display(seed), "00");
        assert_eq!(vm.display(out), "000");
    }

    #[test]
    #[cfg(feature = "instrument")]
    fn instrumented_vm_declines_the_uncharged_payload_reserve() {
        let mut vm = vm();
        assert!(vm.str_append_index_reserve_allowed());
        vm.set_instrumentation(crate::vm::instrument::Recorder::new());
        assert!(!vm.str_append_index_reserve_allowed());
    }
}

#[cfg(test)]
mod inspect_bounds_tests {
    use super::{INSPECT_MAX_BYTES, INSPECT_TRUNCATION_MARKER};

    fn run_ok(src: &str) -> Vec<String> {
        let outcome = crate::run(src).expect("source compiles");
        assert!(
            outcome.error.is_none(),
            "unexpected runtime error: {:?}",
            outcome.error
        );
        outcome.output
    }

    #[test]
    fn ordinary_console_shapes_are_preserved() {
        let output = run_ok(
            r#"
            console.log("top");
            console.log([1, "x", true]);
            console.log({ a: 1, b: "x" });
            console.log(new Uint8Array([1, 2, 255]));
            "#,
        );
        assert_eq!(
            output,
            [
                "top",
                "[ 1, 'x', true ]",
                "{ a: 1, b: 'x' }",
                "Uint8Array(3) [ 1, 2, 255 ]",
            ]
        );
    }

    #[test]
    fn cycles_stop_but_shared_siblings_render_normally() {
        let output = run_ok(
            r#"
            const array = [];
            array[0] = array;
            console.log(array);

            const object = {};
            object.self = object;
            console.log(object);

            const shared = { v: 1 };
            console.log([shared, shared]);
            "#,
        );
        assert_eq!(
            output,
            [
                "[ [Circular] ]",
                "{ self: [Circular] }",
                "[ { v: 1 }, { v: 1 } ]",
            ]
        );
    }

    #[test]
    fn depth_and_node_limits_truncate_hostile_values() {
        let output = run_ok(
            r#"
            let deep = 0;
            for (let i = 0; i < 64; i++) deep = [deep];
            console.log(deep);
            console.log(new Uint8Array(5000));
            "#,
        );
        assert!(output[0].contains("[MaxDepth]"), "{}", output[0]);
        assert!(
            output[1].starts_with("Uint8Array(5000) [ 0, 0, "),
            "{}",
            output[1]
        );
        assert!(output[1].ends_with(INSPECT_TRUNCATION_MARKER));
        assert!(output[1].len() < INSPECT_MAX_BYTES);
    }

    #[test]
    fn byte_limit_keeps_utf8_valid_and_reserves_one_marker() {
        let output = run_ok(
            "console.log('\u{e9}'.repeat(70000));\n\
             console.log('x'.repeat(40000), 'y'.repeat(40000));\n\
             const indirect = print; indirect(...Array(5000).fill('z'));",
        );
        let unicode = &output[0];
        assert!(unicode.len() <= INSPECT_MAX_BYTES);
        assert!(unicode.ends_with(INSPECT_TRUNCATION_MARKER));
        assert!(unicode[..unicode.len() - INSPECT_TRUNCATION_MARKER.len()]
            .chars()
            .all(|ch| ch == '\u{e9}'));
        for line in &output[1..] {
            assert!(line.len() <= INSPECT_MAX_BYTES, "{}", line.len());
            assert!(line.ends_with(INSPECT_TRUNCATION_MARKER));
        }
    }
}

#[cfg(test)]
mod pad2_pinned_mutation_tests {
    use super::*;

    fn vm() -> Vm<'static> {
        // Leak the tiny test Program so the returned VM's borrow is valid for
        // the test lifetime; production ownership is unchanged.
        let src = "var x = 0;";
        let ast = crate::front::parse_script(src).expect("source parses");
        let program = Box::leak(Box::new(
            crate::compile::compile_program(&ast, src).expect("source compiles"),
        ));
        let mut vm = Vm::new(program);
        vm.run().expect("program runs");
        vm
    }

    #[test]
    fn pad2_slots_are_exact_and_all_inplace_paths_decline() {
        let mut vm = vm();
        for n in 0..100i32 {
            let v = vm.pad2_concat(Value::int(n), n < 10).expect("pad2 hit");
            assert_eq!(v.heap_index(), crate::heap::INTERN_PAD2_START + n as u32);
            assert_eq!(vm.display(v), format!("{n:02}"));
        }
        let plain_nine = vm.pad2_concat(Value::int(9), false).unwrap();
        let zero_ten = vm.pad2_concat(Value::int(10), true).unwrap();
        assert_eq!(vm.display(plain_nine), "9");
        assert_eq!(vm.display(zero_ten), "010");

        let cached = vm.pad2_concat(Value::int(7), true).unwrap();
        let x = Value::heap(b'x' as u32);
        let slash = Value::heap(b'/' as u32);

        // Direct StrAppendInPlace materialisation gate.
        let appended = vm.str_append_inplace(cached, x).expect("primitive append");
        assert_ne!(appended.heap_index(), cached.heap_index());
        assert_eq!(vm.display(appended), "07x");
        assert_eq!(vm.display(cached), "07");

        // Interpreter fused-chain gate.
        let chained = vm.add_values_chain(cached, x).expect("chain append");
        assert_ne!(chained.heap_index(), cached.heap_index());
        assert_eq!(vm.display(chained), "07x");
        assert_eq!(vm.display(cached), "07");

        // Proven-linear AddRightPair runtime guard (the compiler can never
        // license a cached slot, but the helper remains defensive).
        let paired = vm
            .add_values_right_pair_inplace(cached, slash, Value::int(1))
            .expect("right-pair append");
        assert_ne!(paired.heap_index(), cached.heap_index());
        assert_eq!(vm.display(paired), "07/1");
        assert_eq!(vm.display(cached), "07");

        // Native chain-fast gate is a separate direct mutation path.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        {
            let vm_ptr = (&mut vm as *mut Vm<'_>).cast::<core::ffi::c_void>();
            let bits = crate::vm::jit_concat_chain_fast(vm_ptr, cached.bits(), x.bits(), 0);
            let native = Value::from_bits(bits);
            assert_ne!(native.heap_index(), cached.heap_index());
            assert_eq!(vm.display(native), "07x");
            assert_eq!(vm.display(cached), "07");
        }
    }
}

#[cfg(test)]
mod cmp_i128_f64_tests {
    use super::cmp_i128_f64;
    use std::cmp::Ordering::*;

    #[test]
    fn exact_against_numbers() {
        assert_eq!(cmp_i128_f64(2, 1.5), Some(Greater));
        assert_eq!(cmp_i128_f64(1, 1.5), Some(Less));
        assert_eq!(cmp_i128_f64(5, 5.0), Some(Equal));
        assert_eq!(cmp_i128_f64(-2, -1.5), Some(Less));
        assert_eq!(cmp_i128_f64(-1, -1.5), Some(Greater));
        // Beyond 2^53 the f64 can't hold the integer exactly, but the compare must.
        assert_eq!(
            cmp_i128_f64(9_007_199_254_740_993, 9_007_199_254_740_992.0),
            Some(Greater)
        );
        assert_eq!(cmp_i128_f64(10_000_000_000_000_001, 1e16), Some(Greater));
        // NaN / infinities.
        assert_eq!(cmp_i128_f64(1, f64::NAN), None);
        assert_eq!(cmp_i128_f64(i128::MAX, f64::INFINITY), Some(Less));
        assert_eq!(cmp_i128_f64(i128::MIN, f64::NEG_INFINITY), Some(Greater));
        // A large in-range f64 (1e30 < 2^127) still compares exactly.
        assert_eq!(cmp_i128_f64(i128::MAX, 1e30), Some(Greater));
        assert_eq!(cmp_i128_f64(i128::MIN, -1e30), Some(Less));
        // Magnitudes beyond 2^127 saturate the comparison.
        assert_eq!(cmp_i128_f64(i128::MAX, 1e40), Some(Less));
        assert_eq!(cmp_i128_f64(i128::MIN, -1e40), Some(Greater));
    }
}
