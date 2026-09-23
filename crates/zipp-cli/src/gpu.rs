//! `zipp py` on the native GPU.
//!
//! A Python program is compiled exactly as without a GPU (unhosted): every
//! graph is evaluated where the program submits it, and its callback runs
//! before `submit` returns. The only difference a GPU makes is *where* that
//! evaluation happens: the program's `__zippHostCall` is served by
//! [`zipp_gpu::SyncBridge`], and zipp_gpu offers each request to it first
//! (`_zipp_gpu.native`). The bridge runs the request on gpu-lab's WebGPU
//! runtime and answers before the program continues; anything the GPU cannot
//! do, zipp_gpu evaluates on the CPU tensor kernels as it always has.
//!
//! The GPU starts on a program's first graph, not before, so a program that
//! never submits one pays nothing. `--no-gpu` or `ZIPP_GPU=0` leaves the
//! bridge out; `ZIPP_GPU_BACKEND` picks the backend and `ZIPP_GPU_LOG=1`
//! names the adapter when it starts.
use std::cell::RefCell;
use std::rc::Rc;

use zipp_vm::embed::ScriptState;

/// Serve `state`'s GPU requests from a lazily started native GPU, unless the
/// run opted out.
pub fn install(state: &mut ScriptState, enabled: bool) {
    if !enabled || zipp_gpu::disabled_by_env() {
        return;
    }
    let log = std::env::var_os("ZIPP_GPU_LOG").is_some();
    let bridge = Rc::new(RefCell::new(zipp_gpu::SyncBridge::new(
        zipp_gpu::GpuOptions::from_env(),
        log,
    )));
    state.set_host_call_ctx(Box::new(move |ctx, kind, args| {
        bridge.borrow_mut().call(ctx, kind, args)
    }));
}
