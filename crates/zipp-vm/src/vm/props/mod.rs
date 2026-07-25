#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

// submodules (split out of the former monolithic props.rs)
mod proxy_ops;
mod enumerate;
mod descriptors;
mod define;
mod array_len;
mod member;
