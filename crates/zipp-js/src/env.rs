//! Lexical environments (scope chains).
//!
//! A `Scope` is a frame of variable bindings plus a link to its parent; closures
//! capture an `Rc<RefCell<Scope>>` so they keep their defining environment alive
//! and observe later mutations (capture by reference, like JS).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::value::JsValue;

pub struct Scope {
    pub vars: HashMap<String, JsValue>,
    pub parent: Option<Rc<RefCell<Scope>>>,
}

impl Scope {
    pub fn global() -> Rc<RefCell<Scope>> {
        Rc::new(RefCell::new(Scope { vars: HashMap::new(), parent: None }))
    }

    pub fn child(parent: &Rc<RefCell<Scope>>) -> Rc<RefCell<Scope>> {
        Rc::new(RefCell::new(Scope { vars: HashMap::new(), parent: Some(parent.clone()) }))
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
        if let Some(v) = cur.borrow().vars.get(name) {
            return Some(v.clone());
        }
        let parent = cur.borrow().parent.clone();
        match parent {
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
        if cur.borrow().vars.contains_key(name) {
            cur.borrow_mut().vars.insert(name.to_string(), v);
            return true;
        }
        let parent = cur.borrow().parent.clone();
        match parent {
            Some(p) => cur = p,
            None => return false,
        }
    }
}
