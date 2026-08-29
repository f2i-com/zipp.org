#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// Cached `ZIPP_CALLLOG` flag (an env read per call-site miss would dominate
/// the miss path — Windows scans the whole environment block per query).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn jit_call_log() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("ZIPP_CALLLOG").is_some())
}

/// Keep a megamorphic plain-call site inside a compiled memory region after its
/// small identity IC saturates. The live callee is still resolved on every call
/// and only a real `Func`/`Closure` is accepted; natives, proxies, bound
/// functions and every other exotic retain the existing interpreter fallback.
/// The switch makes the mechanism independently measurable on hostile corpora.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn jit_poly_call_fallback_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("ZIPP_NO_POLY_CALL_FALLBACK").is_none())
}

/// Let loader-installed ES-module functions use the ordinary function and
/// region JITs. Module `FuncProto`s share the runtime-function table with
/// `eval`/`new Function`, but unlike those dynamic-script functions every
/// module range is recorded by the loader and its leaked protos remain stable
/// for the VM's lifetime. Keep an independent off switch for parity testing.
///
/// Cached because the eligibility predicate runs at every module-function
/// entry and hot loop backedge; querying the environment there is measurable,
/// especially on Windows.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn jit_module_functions_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("ZIPP_NO_MODULE_JIT").is_none())
}

// submodules (split out of the former monolithic engine.rs)
mod boot;
mod eval_prog;
mod jit_calls;
mod jit_frame;
mod jit_plans;
mod method_inline;
mod modules;
mod run;
