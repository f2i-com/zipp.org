//! zipp_torch.wasm: the torch package archive and the native tensor kernels
//! for ZIPP's WebAssembly `python` artifact.
//!
//! The kernels are zipp-vm's own (`vm/py_tensor/*`, included by path, not
//! copied): the engine sends each call here as bytes (`wire`) and this
//! module runs it over those bytes exactly as the engine runs it over its
//! heap in the `all` artifact. Exports (no imports):
//!
//! - `zipp_package_ptr()` / `zipp_package_len()`: the archive, in memory.
//! - `zipp_alloc(len)` / `zipp_free(ptr, len)`: buffers the loader fills.
//! - `zipp_kernel(ptr, len)`: run the request at `ptr` (freed here) and
//!   answer a buffer holding `[len: u32 LE][response]` (the caller frees it
//!   with `zipp_free(out, len + 4)`).
#![allow(clippy::missing_safety_doc)]

#[allow(dead_code, unused_imports)]
#[path = "../../../zipp-vm/src/vm/py_tensor/args.rs"]
mod args;
#[allow(dead_code)]
#[path = "../../../zipp-vm/src/vm/py_tensor/fft.rs"]
mod fft;
#[allow(dead_code)]
#[path = "../../../zipp-vm/src/vm/py_tensor/jsmath.rs"]
mod jsmath;
#[allow(dead_code)]
#[path = "../../../zipp-vm/src/vm/py_tensor/kernels.rs"]
mod kernels;
#[allow(dead_code)]
#[path = "../../../zipp-vm/src/vm/py_tensor/linalg.rs"]
mod linalg;
#[allow(dead_code)]
#[path = "../../../zipp-vm/src/vm/py_tensor/quant.rs"]
mod quant;
#[allow(dead_code)]
#[path = "../../../zipp-vm/src/vm/py_tensor/sparse.rs"]
mod sparse;
#[allow(dead_code)]
#[path = "../../../zipp-vm/src/vm/py_tensor/wire.rs"]
mod wire;

static PACKAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/torch.zpkg"));

impl kernels::Host for wire::WireHost {
    fn bytes(&self, buffer: u32) -> Option<&[u8]> {
        wire::WireHost::bytes(self, buffer)
    }
    fn bytes_mut(&mut self, buffer: u32) -> Option<&mut [u8]> {
        wire::WireHost::bytes_mut(self, buffer)
    }
    fn admits(&self, cost: u64, transient: usize) -> bool {
        self.budget.admits(cost, transient)
    }
    fn charge(&mut self, cost: u64) {
        self.charged = self.charged.saturating_add(cost);
    }
    // The engine's WebAssembly meter polls no abort flag either
    // (`Vm::native_kernel_interrupted`): the host ends a run by terminating
    // the Worker.
    fn interrupted(&self) -> bool {
        false
    }
}

#[no_mangle]
pub extern "C" fn zipp_package_ptr() -> *const u8 {
    PACKAGE.as_ptr()
}

#[no_mangle]
pub extern "C" fn zipp_package_len() -> usize {
    PACKAGE.len()
}

#[no_mangle]
pub extern "C" fn zipp_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn zipp_free(ptr: *mut u8, len: usize) {
    drop(Vec::from_raw_parts(ptr, 0, len.max(1)));
}

#[no_mangle]
pub unsafe extern "C" fn zipp_kernel(ptr: *mut u8, len: usize) -> *mut u8 {
    let request = std::slice::from_raw_parts(ptr, len);
    let response = match wire::decode_request(request) {
        Some(mut host) => {
            let op = host.op;
            let args = std::mem::take(&mut host.args);
            let outcome = kernels::run(&mut host, op, &args);
            host.response(outcome)
        }
        // A request this module cannot read: declined, without charge.
        None => vec![0; 13],
    };
    zipp_free(ptr, len);
    let out = zipp_alloc(response.len() + 4);
    std::ptr::copy_nonoverlapping((response.len() as u32).to_le_bytes().as_ptr(), out, 4);
    std::ptr::copy_nonoverlapping(response.as_ptr(), out.add(4), response.len());
    out
}
