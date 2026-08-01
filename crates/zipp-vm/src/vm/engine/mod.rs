#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, ReactionPair, Reactions,
};
use crate::value::Value;

/// Cached `ZIPP_CALLLOG` flag (an env read per call-site miss would dominate
/// the miss path — Windows scans the whole environment block per query).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn jit_call_log() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("ZIPP_CALLLOG").is_some())
}

// submodules (split out of the former monolithic engine.rs)
mod boot;
mod jit_plans;
mod jit_calls;
mod method_inline;
mod jit_frame;
mod run;
mod modules;
mod eval_prog;
