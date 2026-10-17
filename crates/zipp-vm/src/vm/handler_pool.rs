//! Recycled `Frame::handlers` buffers.
//!
//! A frame's handler stack is a `Vec` that starts empty (no allocation) and
//! allocates on its first `try` push, which the frame's return then frees:
//! an allocator round trip per call of any function that sets a handler (a
//! Python function always does: its traceback guard). A popped frame's buffer
//! goes to the VM's pool (`Vm::handler_pool`) instead, emptied, and the next
//! first push takes it back. Only the buffer's storage is reused; its contents
//! are always cleared first, so what a frame sees is exactly an empty stack,
//! as before. The pool is the VM's own field, not a thread-local: a `try` in
//! a hot function paid the thread-local lookup and its `RefCell` borrow twice
//! per call, a sixth of a loop calling three functions with `finally`
//! blocks.

use crate::heap::Handler;

/// Buffers kept at most, and the largest capacity kept.
const POOL_MAX: usize = 64;
const CAP_MAX: usize = 16;

/// An empty handler buffer from `pool`, recycled when one is available.
#[inline]
pub(crate) fn take(pool: &mut Vec<Vec<Handler>>) -> Vec<Handler> {
    pool.pop().unwrap_or_default()
}

/// Keep `v`'s storage in `pool` for a later [`take`] (one that grew large is
/// simply dropped). The caller passes only a buffer that allocated.
#[inline]
pub(crate) fn give(pool: &mut Vec<Vec<Handler>>, mut v: Vec<Handler>) {
    if v.capacity() > CAP_MAX {
        return;
    }
    v.clear();
    if pool.len() < POOL_MAX {
        pool.push(v);
    }
}
