#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// Where a frame-free call's arguments come from (`Vm::call_compiled_windowed`):
/// a slice a builtin holds, or `argc` registers of the caller's window from
/// absolute register `at` (a compiled `new`, which reads them in place).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
pub(crate) enum WinArgs<'a> {
    Slice(&'a [Value]),
    Regs { at: usize, argc: u16 },
}

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

/// Let ordinary `eval` / `new Function` functions (every script an embedder
/// runs through `eval_in_context`) use the function and region JITs too.
/// Their protos are leaked at stable addresses and installed after the remap,
/// exactly as a module's are; what differs is sloppy code and direct-eval
/// scopes, which the JITs already meet in main-program code: a closure with
/// an EvalScope stamp is refused by `closure_eval_scope`, and the dynamic
/// name ops a direct eval compiles to are not admitted. Off switch for
/// parity testing.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn jit_eval_functions_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("ZIPP_NO_EVAL_JIT").is_none())
}

// submodules (split out of the former monolithic engine.rs)
mod boot;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) mod ctor_store;
mod eval_prog;
mod jit_calls;
mod jit_frame;
mod jit_plans;
mod method_inline;
mod modules;
#[cfg(feature = "python")]
mod py_jit;
#[cfg(all(feature = "python", feature = "jit", target_arch = "x86_64"))]
mod py_jit_attr;
mod run;
