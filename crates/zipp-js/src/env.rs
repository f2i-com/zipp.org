//! Lexical environments (scope chains).
//!
//! A `Scope` is a frame of variable bindings plus a link to its parent; closures
//! capture an `Rc<RefCell<Scope>>` so they keep their defining environment alive
//! and observe later mutations (capture by reference, like JS).

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

use crate::value::JsValue;

/// FxHash — rustc's fast non-cryptographic hash. The default `HashMap` uses
/// SipHash (DoS-resistant but slow); variable names are short, trusted, and
/// looked up on every identifier access, so a cheap hash is a big interpreter
/// win. Byte-wise variant (simple + correct; keys are short).
#[derive(Default)]
pub struct FxHasher {
    hash: u64,
}

const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.hash;
        for &b in bytes {
            h = (h.rotate_left(5) ^ (b as u64)).wrapping_mul(K);
        }
        self.hash = h;
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

/// A variable map keyed by name, using the fast FxHash hasher.
pub type VarMap = HashMap<String, JsValue, BuildHasherDefault<FxHasher>>;

pub struct Scope {
    pub vars: VarMap,
    pub parent: Option<Rc<RefCell<Scope>>>,
}

impl Scope {
    pub fn global() -> Rc<RefCell<Scope>> {
        Rc::new(RefCell::new(Scope { vars: VarMap::default(), parent: None }))
    }

    pub fn child(parent: &Rc<RefCell<Scope>>) -> Rc<RefCell<Scope>> {
        Rc::new(RefCell::new(Scope { vars: VarMap::default(), parent: Some(parent.clone()) }))
    }

    /// Declare (or redeclare) a binding in this frame.
    pub fn declare(&mut self, name: &str, v: JsValue) {
        self.vars.insert(name.to_string(), v);
    }
}

/// Look up `name`, walking up the scope chain. `None` if undeclared.
pub fn get(scope: &Rc<RefCell<Scope>>, name: &str) -> Option<JsValue> {
    let mut cur = scope.clone();
    loop {
        // One borrow per frame: read the var (returning if found) and grab the
        // parent link in the same borrow, so we clone the parent `Rc` at most
        // once per level (not twice).
        let next = {
            let b = cur.borrow();
            if let Some(v) = b.vars.get(name) {
                return Some(v.clone());
            }
            b.parent.clone()
        };
        match next {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

/// Assign to the nearest existing binding named `name`. Returns `false` if no
/// such binding exists (the caller then creates an implicit global, per sloppy
/// mode).
pub fn set(scope: &Rc<RefCell<Scope>>, name: &str, v: JsValue) -> bool {
    let mut cur = scope.clone();
    loop {
        {
            let mut b = cur.borrow_mut();
            if let Some(slot) = b.vars.get_mut(name) {
                *slot = v;
                return true;
            }
        }
        let next = cur.borrow().parent.clone();
        match next {
            Some(p) => cur = p,
            None => return false,
        }
    }
}
