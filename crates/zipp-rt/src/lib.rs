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

// ───────────────────────── arrays / structs ─────────────────────────

/// Allocate an array (or struct) block of `n` 8-byte slots, length-prefixed,
/// zero-initialized, from the GC heap. Returns the base pointer as an i64.
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

/// `[value; n]` — allocate then fill. `val` is the raw 8-byte payload.
#[no_mangle]
pub extern "C" fn zipp_array_repeat(n: i64, val: i64) -> i64 {
    let base = zipp_alloc(n) as *mut i64;
    // SAFETY: zipp_alloc gave us n+1 valid slots; fill the n element slots.
    unsafe {
        for i in 0..n {
            *base.add(1 + i as usize) = val;
        }
    }
    base as i64
}

#[no_mangle]
pub extern "C" fn zipp_oob(idx: i64, len: i64) {
    eprintln!("zipp: array index {idx} out of bounds (len {len})");
    std::process::abort();
}

// ───────────────────────── strings ─────────────────────────

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
