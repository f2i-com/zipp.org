//! Recycled `Frame::handlers` buffers.
//!
//! A frame's handler stack is a `Vec` that starts empty (no allocation) and
//! allocates on its first `try` push, which the frame's return then frees:
//! an allocator round trip per call of any function that sets a handler (a
//! Python function always does: its traceback guard). A popped frame's buffer
//! goes here instead, emptied, and the next first push takes it back. Only the
//! buffer's storage is reused; its contents are always cleared first, so what a
//! frame sees is exactly an empty stack, as before.

use crate::heap::Handler;
use std::cell::RefCell;

/// Buffers kept at most, and the largest capacity kept.
const POOL_MAX: usize = 64;
const CAP_MAX: usize = 16;

thread_local! {
    static POOL: RefCell<Vec<Vec<Handler>>> = const { RefCell::new(Vec::new()) };
}

/// An empty handler buffer, recycled when one is available. Out of line:
/// the dispatch loop's arms stay as small as they were.
#[inline(never)]
pub(crate) fn take() -> Vec<Handler> {
    POOL.with(|p| p.borrow_mut().pop()).unwrap_or_default()
}

/// Keep `v`'s storage for a later [`take`] (one that grew large is
/// simply dropped). The caller passes only a buffer that allocated.
#[inline(never)]
pub(crate) fn give(mut v: Vec<Handler>) {
    if v.capacity() > CAP_MAX {
        return;
    }
    v.clear();
    POOL.with(|p| {
        let mut p = p.borrow_mut();
        if p.len() < POOL_MAX {
            p.push(v);
        }
    });
}
