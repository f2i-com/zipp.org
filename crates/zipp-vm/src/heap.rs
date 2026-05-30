//! Minimal heap for the v1 engine: strings and function objects.
//!
//! Heap values are referenced by a `u32` index packed into a `Value`. The heap
//! is a plain `Vec`; v1 does not reclaim (programs are short-lived per `eval`).
//! A real GC slots in here later without touching the value representation.

/// A heap-allocated object.
#[derive(Clone, Debug)]
pub enum HeapObj {
    /// An interned-or-owned JS string.
    Str(String),
    /// A function: index into `Program::functions`.
    Func(u32),
}

#[derive(Default)]
pub struct Heap {
    objs: Vec<HeapObj>,
}

impl Heap {
    pub fn new() -> Heap {
        Heap { objs: Vec::new() }
    }

    #[inline]
    pub fn alloc(&mut self, obj: HeapObj) -> u32 {
        let idx = self.objs.len() as u32;
        self.objs.push(obj);
        idx
    }

    #[inline]
    pub fn get(&self, idx: u32) -> &HeapObj {
        &self.objs[idx as usize]
    }

    #[inline]
    pub fn alloc_str(&mut self, s: String) -> u32 {
        self.alloc(HeapObj::Str(s))
    }

    /// Borrow a string by heap index, or `None` if the object isn't a string.
    #[inline]
    pub fn as_str(&self, idx: u32) -> Option<&str> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Resolve a function-object index to its program function id.
    #[inline]
    pub fn as_func(&self, idx: u32) -> Option<u32> {
        match self.get(idx) {
            HeapObj::Func(id) => Some(*id),
            _ => None,
        }
    }
}
