//! Storage for the code a VM installs at run time — the functions and classes
//! `eval`, `new Function` and loaded modules compile — owned beside the
//! `Program` so it is freed with the VM.
//!
//! Each installed `FuncProto` / `ClassDef` must keep its address for the VM's
//! whole life: the function table hands out `&'p` references and the JIT holds
//! raw pointers into them. They used to be `Box::leak`ed for that, so a state
//! an embedder dropped left all of its evaluated code behind
//! (f2i-com/zipp.org#53: a host that evaluates a 243 KB runtime into each
//! page's state grew by about 1.2 MB a page).
//!
//! The arena is a chain of boxed nodes linked through `OnceCell`, which takes
//! its value through a shared reference, so a VM holding `&'p EvalArena`
//! appends with safe code and gets `&'p` references back. Nodes never move
//! (each lives in its own `Box`) and are never removed until the arena drops,
//! which it does iteratively, so a long chain cannot overflow the stack.

use std::cell::OnceCell;

use crate::bytecode::{ClassDef, FuncProto};

/// One installed value and the link to the next.
pub(crate) struct Node<T> {
    pub(crate) value: T,
    next: OnceCell<Box<Node<T>>>,
}

/// An append-only chain of nodes.
pub(crate) struct Chain<T> {
    head: OnceCell<Box<Node<T>>>,
}

impl<T> Default for Chain<T> {
    fn default() -> Self {
        Chain {
            head: OnceCell::new(),
        }
    }
}

impl<T> Chain<T> {
    /// Append `value` after `tail` (`None`: the chain is empty as far as the
    /// caller knows) and return its node. A stale `tail` only costs a walk to
    /// the real end.
    pub(crate) fn push<'a>(&'a self, tail: Option<&'a Node<T>>, value: T) -> &'a Node<T> {
        let mut slot = match tail {
            Some(node) => &node.next,
            None => &self.head,
        };
        while let Some(node) = slot.get() {
            slot = &node.next;
        }
        if slot.set(Box::new(Node {
            value,
            next: OnceCell::new(),
        }))
        .is_err()
        {
            unreachable!("the walk above ended on an empty link");
        }
        slot.get().expect("just set")
    }
}

impl<T> Drop for Chain<T> {
    fn drop(&mut self) {
        let mut next = self.head.take();
        while let Some(mut node) = next {
            next = node.next.take();
        }
    }
}

/// The functions and classes a VM installs at run time.
#[derive(Default)]
pub struct EvalArena {
    pub(crate) funcs: Chain<FuncProto>,
    pub(crate) classes: Chain<ClassDef>,
}

#[cfg(test)]
mod tests {
    use super::Chain;

    #[test]
    fn references_stay_valid_and_a_long_chain_drops_without_recursion() {
        let chain: Chain<Vec<u32>> = Chain::default();
        let mut tail = None;
        let mut refs = Vec::new();
        for i in 0..200_000u32 {
            let node = chain.push(tail, vec![i, i + 1]);
            refs.push(&node.value);
            tail = Some(node);
        }
        assert_eq!(refs[0], &vec![0, 1]);
        assert_eq!(refs[199_999], &vec![199_999, 200_000]);
        // A stale tail still appends at the end.
        let late = chain.push(None, vec![7]);
        assert_eq!(late.value, vec![7]);
        drop(refs);
        drop(chain);
    }
}
