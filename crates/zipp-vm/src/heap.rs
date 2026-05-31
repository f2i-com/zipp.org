//! Heap object storage.
//!
//! Heap values are referenced by a `u32` index packed into a [`crate::value::Value`].
//! Reference semantics fall out naturally: copying a `Value` copies the index,
//! so `let b = a` makes `a` and `b` alias the same heap slot, and a mutation
//! through either is visible through both — exactly JS object/array semantics.
//!
//! v1 does not reclaim memory (programs are short-lived per `eval`); a real GC
//! slots in here later without touching the value representation. Objects use a
//! simple insertion-ordered property list, which preserves JS string-key
//! enumeration order and is correct (if not yet fast — shapes/inline-caches are
//! a later tier).

use crate::value::Value;

/// A JS object: insertion-ordered string-keyed properties.
#[derive(Clone, Debug, Default)]
pub struct ObjMap {
    pub keys: Vec<String>,
    pub vals: Vec<Value>,
}

impl ObjMap {
    pub fn new() -> ObjMap {
        ObjMap { keys: Vec::new(), vals: Vec::new() }
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.keys.iter().position(|k| k == key).map(|i| self.vals[i])
    }

    /// Set `key = val`. Returns `true` if a NEW key was appended (which may have
    /// reallocated `vals`), `false` if an existing slot was overwritten. The JIT
    /// inline cache uses this to bump the object's version on a key-add (an
    /// existing key's slot never moves — keys are append-only, no delete).
    pub fn set(&mut self, key: &str, val: Value) -> bool {
        if let Some(i) = self.keys.iter().position(|k| k == key) {
            self.vals[i] = val;
            false
        } else {
            self.keys.push(key.to_string());
            self.vals.push(val);
            true
        }
    }

    pub fn has(&self, key: &str) -> bool {
        self.keys.iter().any(|k| k == key)
    }
}

/// A heap-allocated object.
#[derive(Clone, Debug)]
pub enum HeapObj {
    /// An owned JS string.
    Str(String),
    /// A plain function: index into `Program::functions`. No captured state.
    Func(u32),
    /// A closure: a function id plus captured upvalue cells (indices of `Cell`
    /// heap objects). Captured variables are boxed into cells so mutation is
    /// shared between the closure and its defining scope.
    Closure { func: u32, upvalues: Vec<u32> },
    /// A boxed mutable variable cell (an upvalue's storage).
    Cell(Value),
    /// A dense array.
    Array(Vec<Value>),
    /// A plain object.
    Object(ObjMap),
}

#[derive(Default)]
pub struct Heap {
    objs: Vec<HeapObj>,
    /// Per-object version, parallel to `objs` (one `u32` per heap object). Bumped
    /// whenever an object gains a NEW key (which may reallocate its `vals`). The
    /// JIT inline cache reads this (by heap index) to validate a cached
    /// `vals`-pointer: a matching version proves `vals` hasn't reallocated since
    /// the cache was filled. Allocated in lockstep with `objs` so indices align.
    versions: Vec<u32>,
}

impl Heap {
    pub fn new() -> Heap {
        Heap { objs: Vec::new(), versions: Vec::new() }
    }

    #[inline]
    pub fn alloc(&mut self, obj: HeapObj) -> u32 {
        let idx = self.objs.len() as u32;
        self.objs.push(obj);
        self.versions.push(0);
        idx
    }

    /// Bump object `idx`'s version (call after a key-add reallocates its `vals`).
    #[inline]
    pub fn bump_version(&mut self, idx: u32) {
        self.versions[idx as usize] = self.versions[idx as usize].wrapping_add(1);
    }

    /// Base pointer of the parallel version array (for the JIT inline cache). The
    /// array does not reallocate during a native region run (a region never
    /// allocates a heap object), so this stays valid for the run.
    #[inline]
    pub fn versions_ptr(&self) -> *const u32 {
        self.versions.as_ptr()
    }

    /// Current version of object `idx` (for filling an inline-cache entry).
    #[inline]
    pub fn version_of(&self, idx: u32) -> u32 {
        self.versions[idx as usize]
    }

    #[inline]
    pub fn get(&self, idx: u32) -> &HeapObj {
        &self.objs[idx as usize]
    }

    #[inline]
    pub fn get_mut(&mut self, idx: u32) -> &mut HeapObj {
        &mut self.objs[idx as usize]
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

    /// Resolve a callable (plain function or closure) to its function id and
    /// upvalue list. Returns `None` for non-callables.
    #[inline]
    pub fn as_callable(&self, idx: u32) -> Option<(u32, &[u32])> {
        match self.get(idx) {
            HeapObj::Func(id) => Some((*id, &[])),
            HeapObj::Closure { func, upvalues } => Some((*func, upvalues.as_slice())),
            _ => None,
        }
    }

    #[inline]
    pub fn cell_get(&self, idx: u32) -> Value {
        match self.get(idx) {
            HeapObj::Cell(v) => *v,
            _ => Value::UNDEFINED,
        }
    }

    #[inline]
    pub fn cell_set(&mut self, idx: u32, v: Value) {
        if let HeapObj::Cell(slot) = self.get_mut(idx) {
            *slot = v;
        }
    }
}
