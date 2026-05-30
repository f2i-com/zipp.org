//! ZIPP shared native runtime: the allocator (GC-backed), string/array helpers,
//! integer `pow`, and `print`. Used by **both** native backends — the Cranelift
//! JIT links it as an rlib and registers these as JIT symbols; the LLVM tier
//! links the staticlib into the clang-compiled exe and `declare`s them.
//!
//! All heap allocation goes through the conservative mark-sweep collector in
//! [`gc`]; the `#[no_mangle] extern "C"` entry points below are the ABI surface
//! the generated code calls. Output uses Rust's `stdout`/`Display` so `f64`
//! formatting is identical to the interpreter across every engine.

mod gc;

/// Re-exports for the JIT (it can't see the private `gc` module).
pub fn gc_set_stack_bottom(addr: usize) {
    gc::set_stack_bottom(addr);
}
#[allow(dead_code)]
pub fn gc_stats() -> (u64, usize) {
    gc::stats()
}
#[allow(dead_code)]
pub fn gc_set_threshold(t: usize) {
    gc::set_threshold(t);
}

// ───────────────────────── print ─────────────────────────

#[no_mangle]
pub extern "C" fn zipp_print_i64(x: i64) {
    println!("{x}");
}
#[no_mangle]
pub extern "C" fn zipp_print_f64(x: f64) {
    println!("{x}");
}
/// Unsigned 64-bit print (i32/u32/i64 widen to i64 and use `zipp_print_i64`;
/// only u64 needs an unsigned channel — its bits would print negative as i64).
#[no_mangle]
pub extern "C" fn zipp_print_u64(x: u64) {
    println!("{x}");
}
#[no_mangle]
pub extern "C" fn zipp_print_str(s: i64) {
    // SAFETY: s is a valid string block (make_str / leak_str_blob / a literal).
    println!("{}", String::from_utf8_lossy(unsafe { str_bytes(s) }));
}

// Markers the LLVM exe prints for the CLI to parse (the JIT doesn't use these —
// there the CLI prints the result itself). Routed through the same Rust stdout
// as `print` so program output and markers stay correctly ordered.
#[no_mangle]
pub extern "C" fn zipp_emit_result_i64(x: i64) {
    println!("__ZRESULT__:{x}");
}
#[no_mangle]
pub extern "C" fn zipp_emit_result_u64(x: u64) {
    println!("__ZRESULT__:{x}");
}
#[no_mangle]
pub extern "C" fn zipp_emit_result_f64(x: f64) {
    println!("__ZRESULT__:{x}");
}
#[no_mangle]
pub extern "C" fn zipp_emit_time_ms(ms: i64) {
    println!("__ZTIME_MS__:{ms}");
}

/// Set the GC stack bottom from the exe's `main` (the JIT uses
/// [`gc_set_stack_bottom`] directly).
#[no_mangle]
pub extern "C" fn zipp_set_stack_bottom(addr: i64) {
    gc::set_stack_bottom(addr as usize);
}

// ───────────────────────── structs / closures ─────────────────────────

/// Allocate a **struct/closure** block of `n` 8-byte slots, length-prefixed
/// (`[len | slot0 | … ]`), zero-initialized, from the GC heap. Returns the base
/// pointer as an i64. Used for structs and the `{code, env}` closure block —
/// fixed-shape records. (Arrays use the Vec-style header below.)
#[no_mangle]
pub extern "C" fn zipp_alloc(n: i64) -> i64 {
    if n < 0 {
        eprintln!("zipp: array length cannot be negative ({n})");
        std::process::abort();
    }
    let total = (n as usize) + 1; // 1 length slot + n element slots
    let p = gc::gc_alloc(total * 8) as *mut i64; // zeroed
    // SAFETY: p has `total` i64 slots; slot 0 holds the length.
    unsafe { *p = n };
    p as i64
}

// ───────────────────────── arrays (Vec-style) ─────────────────────────
//
// An array is a stable 3-word HEADER `[ +0 len | +8 cap | +16 data ]` that never
// moves (so the handle is stable under aliasing and a future `push` can realloc
// the data buffer without changing the array's identity), plus a SEPARATE data
// buffer of `cap` 8-byte element slots (f64 elements bit-reinterpreted; no length
// prefix — that lives in the header). Element address = `data + 8*idx` (NO +1).
// The GC follows the `data` pointer at +16 when it traces the header, so the
// buffer stays live as long as the header is reachable. `len()` reads +0 (same
// offset as a string's length, so the shared `Len` opcode covers both).

/// Allocate an array header + a zeroed data buffer for `n` elements
/// (`len == cap == n`). Returns the header pointer as an i64.
#[no_mangle]
pub extern "C" fn zipp_arr_new(n: i64) -> i64 {
    if n < 0 {
        eprintln!("zipp: array length cannot be negative ({n})");
        std::process::abort();
    }
    let cap = n as usize;
    // Data buffer first; `data` stays live in a local across the header alloc, so
    // a collection triggered by that alloc finds it on the stack (conservative
    // scan) and won't free it. gc_alloc zeroes and rounds size up to ≥ 8 bytes.
    let data = gc::gc_alloc(cap * 8) as i64;
    let hdr = gc::gc_alloc(3 * 8) as *mut i64;
    // SAFETY: hdr has 3 i64 slots; data is a valid buffer of `cap` slots.
    unsafe {
        *hdr = n; // len
        *hdr.add(1) = cap as i64; // cap
        *hdr.add(2) = data; // data ptr
    }
    hdr as i64
}

/// `[value; n]` — allocate an array then fill its `n` element slots with the raw
/// 8-byte payload `val`.
#[no_mangle]
pub extern "C" fn zipp_array_repeat(n: i64, val: i64) -> i64 {
    let hdr = zipp_arr_new(n) as *mut i64;
    // SAFETY: hdr is a fresh array header; its data buffer has `n` slots. No
    // allocation happens in the fill loop, so nothing can move/free it.
    unsafe {
        let data = *hdr.add(2) as *mut i64;
        for i in 0..n as usize {
            *data.add(i) = val;
        }
    }
    hdr as i64
}

/// `arr.push(val)` — append the raw 8-byte payload `val`, growing the data buffer
/// (cap 0→4, else ×2) if full. Returns the new length. The header is unchanged in
/// identity (only its len/cap/data words update), so aliases of the array see the
/// append. `val` is already in slot form (f64 bit-reinterpreted by the caller).
#[no_mangle]
pub extern "C" fn zipp_arr_push(hdr: i64, val: i64) -> i64 {
    let h = hdr as *mut i64;
    // SAFETY: `hdr` is a valid array header `[len|cap|data]`.
    unsafe {
        let len = *h as usize;
        let cap = *h.add(1) as usize;
        let mut data = *h.add(2) as *mut i64;
        if len == cap {
            let newcap = if cap == 0 { 4 } else { cap * 2 };
            // Allocate the new buffer FIRST. `hdr` is a live argument across this
            // call, so a collection it triggers marks the header → follows its
            // +16 pointer → keeps the OLD buffer alive while we copy from it.
            let newdata = gc::gc_alloc(newcap * 8) as *mut i64;
            std::ptr::copy_nonoverlapping(data, newdata, len);
            // Publish only after the copy (copy-then-publish): the header keeps
            // pointing at the old buffer until the new one is fully populated.
            *h.add(1) = newcap as i64;
            *h.add(2) = newdata as i64;
            data = newdata;
        }
        *data.add(len) = val;
        *h = (len + 1) as i64;
        (len + 1) as i64
    }
}

/// `arr.pop()` — remove and return the last element's raw 8-byte payload. Aborts
/// on an empty array (TS returns `undefined`; ZIPP arrays are total over a real
/// element type, so popping empty is a runtime error like an out-of-bounds read).
/// The capacity/buffer are kept (no shrink); only `len` decreases.
#[no_mangle]
pub extern "C" fn zipp_arr_pop(hdr: i64) -> i64 {
    let h = hdr as *mut i64;
    // SAFETY: `hdr` is a valid array header `[len|cap|data]`.
    unsafe {
        let len = *h as usize;
        if len == 0 {
            eprintln!("zipp: pop from an empty array");
            std::process::abort();
        }
        let data = *h.add(2) as *const i64;
        let v = *data.add(len - 1);
        *h = (len - 1) as i64;
        v
    }
}

#[no_mangle]
pub extern "C" fn zipp_oob(idx: i64, len: i64) {
    eprintln!("zipp: array index {idx} out of bounds (len {len})");
    std::process::abort();
}

// ───────────────────────── strings ─────────────────────────

/// Like [`make_str`] but for a byte range that may fall inside a multi-byte
/// UTF-8 character: invalid sequences become U+FFFD, exactly matching the
/// interpreter (whose `str_heap` holds Rust `String`s and uses
/// `String::from_utf8_lossy`). This keeps a sliced string byte-identical across
/// the interpreter and native tiers on *all* inputs, not just ASCII.
fn make_str_lossy(bytes: &[u8]) -> i64 {
    make_str(String::from_utf8_lossy(bytes).as_bytes())
}

/// Allocate an 8-aligned `[len|bytes]` string block from the GC heap.
fn make_str(bytes: &[u8]) -> i64 {
    let p = gc::gc_alloc(8 + bytes.len());
    // SAFETY: p is 8-aligned with room for the length slot + bytes.
    unsafe {
        *(p as *mut i64) = bytes.len() as i64;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(8), bytes.len());
    }
    p as i64
}

/// Bytes slice of a string block.
unsafe fn str_bytes<'a>(s: i64) -> &'a [u8] {
    let len = *(s as *const i64) as usize;
    std::slice::from_raw_parts((s as *const u8).add(8), len)
}

#[no_mangle]
pub extern "C" fn zipp_str_concat(a: i64, b: i64) -> i64 {
    // SAFETY: a, b are valid string blocks.
    let (sa, sb) = unsafe { (str_bytes(a), str_bytes(b)) };
    let mut bytes = Vec::with_capacity(sa.len() + sb.len());
    bytes.extend_from_slice(sa);
    bytes.extend_from_slice(sb);
    make_str(&bytes)
}

#[no_mangle]
pub extern "C" fn zipp_str_eq(a: i64, b: i64) -> i64 {
    // SAFETY: as above.
    (unsafe { str_bytes(a) == str_bytes(b) }) as i64
}

// String methods (v0: BYTE-level / UTF-8 byte offsets — consistent with `len`,
// which is already byte length. ASCII-exact vs TypeScript; on non-ASCII these
// operate on bytes, not UTF-16 code units). All take/return i64 (str ptrs are
// i64). Index args use TS clamping; out-of-range is total (sentinel), never a
// trap — except `repeat(negative)`, which aborts like `pow(negative)`.

/// TS index clamp: a negative index counts from the end, then clamp to `[0, len]`.
fn str_norm(idx: i64, len: usize) -> usize {
    let l = len as i64;
    (if idx < 0 { (l + idx).max(0) } else { idx.min(l) }) as usize
}

/// `s.charCodeAt(i)` — the raw UTF-8 byte (0-255) at byte offset `i`, or `-1` if
/// out of range (TS returns NaN; v0 contract is -1).
#[no_mangle]
pub extern "C" fn zipp_str_byte_at(s: i64, i: i64) -> i64 {
    let b = unsafe { str_bytes(s) };
    if i < 0 || i as usize >= b.len() {
        -1
    } else {
        b[i as usize] as i64
    }
}

/// `s.slice(start, end)` — a fresh string of `bytes[effStart..effEnd]` (TS
/// negative/clamp; `effEnd <= effStart` → empty).
#[no_mangle]
pub extern "C" fn zipp_str_slice(s: i64, start: i64, end: i64) -> i64 {
    let b = unsafe { str_bytes(s) };
    let a = str_norm(start, b.len());
    let e = str_norm(end, b.len());
    if e <= a {
        make_str(&[])
    } else {
        make_str_lossy(&b[a..e])
    }
}

/// `s.slice(start)` — `slice(start, len)`; a separate entry so the frontend never
/// has to materialize `len` (avoids re-evaluating the receiver).
#[no_mangle]
pub extern "C" fn zipp_str_slice_from(s: i64, start: i64) -> i64 {
    let len = unsafe { str_bytes(s) }.len() as i64;
    zipp_str_slice(s, start, len)
}

/// `s.indexOf(needle, from)` — lowest byte offset `>= clamp(from)` of `needle`,
/// else `-1`. Empty needle → `clamp(from)`.
#[no_mangle]
pub extern "C" fn zipp_str_index_of(s: i64, needle: i64, from: i64) -> i64 {
    let (b, n) = unsafe { (str_bytes(s), str_bytes(needle)) };
    let start = (from.max(0) as usize).min(b.len());
    if n.is_empty() {
        return start as i64;
    }
    if n.len() > b.len() {
        return -1;
    }
    for i in start..=(b.len() - n.len()) {
        if &b[i..i + n.len()] == n {
            return i as i64;
        }
    }
    -1
}

/// `s.lastIndexOf(needle)` — highest byte offset of `needle`, else `-1`. Empty
/// needle → `len`.
#[no_mangle]
pub extern "C" fn zipp_str_last_index_of(s: i64, needle: i64) -> i64 {
    let (b, n) = unsafe { (str_bytes(s), str_bytes(needle)) };
    if n.is_empty() {
        return b.len() as i64;
    }
    if n.len() > b.len() {
        return -1;
    }
    for i in (0..=(b.len() - n.len())).rev() {
        if &b[i..i + n.len()] == n {
            return i as i64;
        }
    }
    -1
}

/// `s.repeat(count)` — `count` copies. `count < 0` aborts (TS throws RangeError).
#[no_mangle]
pub extern "C" fn zipp_str_repeat(s: i64, count: i64) -> i64 {
    if count < 0 {
        eprintln!("zipp: repeat count must be >= 0 (got {count})");
        std::process::abort();
    }
    let b = unsafe { str_bytes(s) };
    let mut out = Vec::with_capacity(b.len() * count as usize);
    for _ in 0..count {
        out.extend_from_slice(b);
    }
    make_str(&out)
}

/// `s.endsWith(suffix)` → 0/1.
#[no_mangle]
pub extern "C" fn zipp_str_ends_with(s: i64, suffix: i64) -> i64 {
    let (b, n) = unsafe { (str_bytes(s), str_bytes(suffix)) };
    (n.len() <= b.len() && b[b.len() - n.len()..] == *n) as i64
}

/// `s.charAt(i)` — a 1-byte string of the byte at `i`, or `""` if out of range.
#[no_mangle]
pub extern "C" fn zipp_str_char_at(s: i64, i: i64) -> i64 {
    let b = unsafe { str_bytes(s) };
    if i < 0 || i as usize >= b.len() {
        make_str(&[])
    } else {
        make_str_lossy(&b[i as usize..i as usize + 1])
    }
}

/// Bake a string literal into a *leaked* `[len|bytes]` block (immortal — used by
/// the JIT, whose code embeds the address as a constant). Outside the GC heap.
pub fn leak_str_blob(s: &str) -> i64 {
    let bytes = s.as_bytes();
    let nslots = 1 + bytes.len().div_ceil(8);
    let mut v: Vec<i64> = vec![0; nslots];
    v[0] = bytes.len() as i64;
    // SAFETY: v has room for 8 + bytes.len() bytes after the length slot.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (v.as_mut_ptr() as *mut u8).add(8), bytes.len());
    }
    let p = v.as_ptr() as i64;
    std::mem::forget(v); // immortal by design
    p
}

// ───────────────────────── math ─────────────────────────

/// Integer `pow` (the one builtin with no single instruction). Exponent must be
/// ≥ 0; wrapping multiply (matches the interpreter).
#[no_mangle]
pub extern "C" fn zipp_ipow(base: i64, exp: i64) -> i64 {
    if exp < 0 {
        eprintln!("zipp: pow exponent must be >= 0 (got {exp})");
        std::process::abort();
    }
    base.wrapping_pow(exp as u32)
}
