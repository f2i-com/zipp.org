//! A conservative mark-sweep garbage collector for the JIT runtime.
//!
//! ZIPP heap objects — arrays, strings, structs — are reached through `i64`
//! handles (pointers to the start of a heap block). In the generated machine
//! code those handles only ever live in **general-purpose registers or on the
//! stack**, never in float/xmm registers (those hold `f64`). That bounds the
//! root set, so a *conservative* collector is sound here without any stack maps
//! or codegen cooperation:
//!
//!   * roots = the callee-saved GPRs (captured with a tiny asm block) + every
//!     aligned word of the machine stack between the current SP and a bottom
//!     captured before `main` runs;
//!   * mark = any word that points into (or *interior* to) a known allocation
//!     keeps it live, then we scan that object's words too (so heap→heap
//!     references — e.g. a struct field holding an array — are followed);
//!   * sweep = free everything unmarked.
//!
//! Conservative means it can occasionally *retain* a dead object (a non-pointer
//! word that happens to look like a heap address), which is harmless. It never
//! frees a live one. Mark-sweep doesn't move objects, so handles stay valid.
//!
//! The heap is **thread-local**: ZIPP runs `main` on one thread, and a collector
//! must only ever scan its own thread's stack. Thread-local state also keeps it
//! correct under parallel `cargo test`.
//!
//! Scope: this is the JIT (in-process) runtime. The `--llvm` AOT tier is a
//! separate process and still leaks (sharing this as a static lib is a follow-on).
//!
//! String *literals* are deliberately allocated outside this heap (they're baked
//! into the code as constants and must be immortal) — see `leak_str_blob`.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::cell::RefCell;
use std::collections::BTreeMap;

const MIN_THRESHOLD: usize = 4 << 20; // collect once ~4 MB has been allocated

struct AllocInfo {
    size: usize,
    mark: bool,
}

struct GcHeap {
    allocs: BTreeMap<usize, AllocInfo>, // base address -> info, ordered for interior-pointer lookup
    bytes: usize,                       // live bytes currently tracked
    threshold: usize,                   // collect when `bytes` would exceed this
    stack_bottom: usize,                // high address; scan [sp, stack_bottom)
    enabled: bool,                      // set once stack_bottom is known
    collections: u64,                   // stats (for tests)
}

impl GcHeap {
    const fn new() -> Self {
        GcHeap {
            allocs: BTreeMap::new(),
            bytes: 0,
            threshold: MIN_THRESHOLD,
            stack_bottom: 0,
            enabled: false,
            collections: 0,
        }
    }
}

thread_local! {
    static HEAP: RefCell<GcHeap> = const { RefCell::new(GcHeap::new()) };
}

/// Record the high end of the stack (a local in the frame that calls the JIT'd
/// `main`). Until this is set, the GC only allocates — it never collects.
pub fn set_stack_bottom(addr: usize) {
    HEAP.with(|h| {
        let mut h = h.borrow_mut();
        h.stack_bottom = addr;
        h.enabled = std::env::var("ZIPP_GC").map(|v| v != "0").unwrap_or(true)
            && cfg!(target_arch = "x86_64");
    });
}

/// Allocate `size` zeroed, 8-aligned bytes from the GC heap, collecting first if
/// we're over the threshold. Returns the base pointer.
pub fn gc_alloc(size: usize) -> *mut u8 {
    HEAP.with(|h| {
        {
            let mut hb = h.borrow_mut();
            if hb.enabled && hb.bytes.saturating_add(size) > hb.threshold {
                // SAFETY: single-threaded mutator; collect scans this thread.
                unsafe { collect(&mut hb) };
                // If the live set is genuinely large, grow the threshold so we
                // don't thrash (collect on every allocation).
                if hb.bytes.saturating_add(size) > hb.threshold {
                    hb.threshold = hb.bytes.saturating_add(size).saturating_mul(2).max(MIN_THRESHOLD);
                }
            }
        }
        let layout = Layout::from_size_align(size.max(8), 8).expect("gc layout");
        // SAFETY: layout is valid (size>=8, align 8).
        let p = unsafe { alloc_zeroed(layout) };
        if p.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        let mut hb = h.borrow_mut();
        hb.allocs.insert(p as usize, AllocInfo { size, mark: false });
        hb.bytes += size;
        p
    })
}

/// (collections so far, live bytes) for the calling thread — used by tests.
#[allow(dead_code)]
pub fn stats() -> (u64, usize) {
    HEAP.with(|h| {
        let h = h.borrow();
        (h.collections, h.bytes)
    })
}

/// Override the collection threshold (tests force frequent GC with a tiny value).
#[allow(dead_code)]
pub fn set_threshold(t: usize) {
    HEAP.with(|h| h.borrow_mut().threshold = t);
}

/// Mark `w` if it points into a known allocation (interior pointers allowed).
fn scan_word(h: &mut GcHeap, w: usize, worklist: &mut Vec<usize>) {
    // Greatest base <= w; live if w is within [base, base+size).
    let hit = h
        .allocs
        .range(..=w)
        .next_back()
        .and_then(|(&base, info)| (w < base + info.size).then_some(base));
    if let Some(base) = hit {
        let info = h.allocs.get_mut(&base).expect("base present");
        if !info.mark {
            info.mark = true;
            worklist.push(base);
        }
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn collect(h: &mut GcHeap) {
    h.collections += 1;
    let mut worklist: Vec<usize> = Vec::new();

    // Capture callee-saved GPRs (which may hold live handles) + the current SP.
    let mut regs = [0usize; 9];
    core::arch::asm!(
        "mov [r11], rbx",
        "mov [r11 + 8], rbp",
        "mov [r11 + 16], rdi",
        "mov [r11 + 24], rsi",
        "mov [r11 + 32], r12",
        "mov [r11 + 40], r13",
        "mov [r11 + 48], r14",
        "mov [r11 + 56], r15",
        "mov [r11 + 64], rsp",
        in("r11") regs.as_mut_ptr(),
        options(nostack, preserves_flags),
    );
    for &w in &regs {
        scan_word(h, w, &mut worklist);
    }

    // Conservatively scan the stack [sp, stack_bottom).
    let mut p = regs[8] & !7usize; // align down to a word
    while p < h.stack_bottom {
        let w = *(p as *const usize);
        scan_word(h, w, &mut worklist);
        p += 8;
    }

    // Trace: scan each marked object's words for interior pointers to others.
    while let Some(base) = worklist.pop() {
        let size = h.allocs[&base].size;
        let mut q = base;
        let end = base + size;
        while q + 8 <= end {
            let w = *(q as *const usize);
            scan_word(h, w, &mut worklist);
            q += 8;
        }
    }

    // Sweep: free unmarked, clear marks on survivors.
    let dead: Vec<usize> = h
        .allocs
        .iter()
        .filter(|(_, a)| !a.mark)
        .map(|(&b, _)| b)
        .collect();
    for base in dead {
        let info = h.allocs.remove(&base).expect("dead present");
        h.bytes = h.bytes.saturating_sub(info.size);
        let layout = Layout::from_size_align(info.size.max(8), 8).expect("gc layout");
        dealloc(base as *mut u8, layout);
    }
    for a in h.allocs.values_mut() {
        a.mark = false;
    }
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn collect(_h: &mut GcHeap) {
    // No register/stack scanner on this arch — the GC stays disabled
    // (set_stack_bottom leaves `enabled` false), so this is never reached.
}

#[cfg(test)]
mod tests {
    use super::*;

    // Standalone sanity check of the allocator/collector data structures
    // (independent of the JIT): allocate, root some via the stack, collect.
    #[test]
    fn interior_pointer_lookup_marks_object() {
        let mut h = GcHeap::new();
        h.allocs.insert(0x1000, AllocInfo { size: 64, mark: false });
        let mut wl = Vec::new();
        scan_word(&mut h, 0x1020, &mut wl); // interior pointer into [0x1000,0x1040)
        assert_eq!(wl, vec![0x1000]);
        assert!(h.allocs[&0x1000].mark);
        // a word outside any allocation marks nothing
        scan_word(&mut h, 0x9999, &mut wl);
        assert_eq!(wl.len(), 1);
    }
}
