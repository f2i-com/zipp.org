//! Python packages a host adds to a Python engine: `addPythonPackage`.
//!
//! The WebAssembly `python` artifact ships without the torch package; the
//! host adds it with zipp_torch.wasm, whose loader (`zipp_torch.js`) reads
//! the package archive out of that module and hands it here together with an
//! object whose `zippTorchKernel(request)` runs a tensor kernel call in the
//! module (request bytes in, response bytes out; see zipp-vm's
//! `vm::py_tensor::wire`). That method is the one host function the engine
//! calls for kernels. The archive is checked by the engine
//! (`zipp_vm::python_packages::install`: format, engine ABI, SHA-256 of
//! every file) before anything is registered; a package is trusted code once
//! added (its runtime JavaScript runs with the Python runtime's own reach),
//! so a host adds only packages it would load as engine code, pinned by hash.
use std::cell::RefCell;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[wasm_bindgen]
extern "C" {
    /// A package's kernel entry, as its loader hands it over.
    #[derive(Clone)]
    type PackageKernels;
    #[wasm_bindgen(method, catch, js_name = zippTorchKernel)]
    fn zipp_torch_kernel(this: &PackageKernels, request: &[u8]) -> Result<Option<Vec<u8>>, JsValue>;
}

thread_local! {
    static KERNELS: RefCell<Option<PackageKernels>> = const { RefCell::new(None) };
}

/// The engine's kernel bridge: the call to the package's module. A throw or
/// a missing answer declines the kernel (the runtime runs its own loop).
fn bridge(request: &[u8]) -> Option<Vec<u8>> {
    let kernels = KERNELS.with(|k| k.borrow().clone())?;
    kernels.zipp_torch_kernel(request).ok().flatten()
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Add a Python package (an archive in `zipp-python-package 1` format) to
/// every Python engine this module creates from now on, with `kernels`
/// (an object with `zippTorchKernel(request)`) when the package brings
/// tensor kernels. Returns `{name, version, modules, kernels}` as JSON.
/// Adding the same archive again does nothing; anything the engine refuses
/// throws with the reason and registers nothing.
#[wasm_bindgen(js_name = addPythonPackage)]
pub fn add_python_package(archive: &[u8], kernels: JsValue) -> Result<String, JsValue> {
    let info = zipp_vm::python_packages::install(archive).map_err(|e| JsValue::from_str(&e))?;
    if info.kernels && !kernels.is_undefined() && !kernels.is_null() {
        KERNELS.with(|k| *k.borrow_mut() = Some(kernels.unchecked_into()));
        zipp_vm::python_packages::set_kernel_bridge(bridge);
    }
    Ok(format!(
        "{{\"name\":{},\"version\":{},\"modules\":{},\"kernels\":{}}}",
        json_string(&info.name),
        json_string(&info.version),
        info.modules,
        info.kernels
    ))
}

/// This engine's Python packages as JSON: the package ABI a package must be
/// built against, whether torch is built in, and what has been added.
#[wasm_bindgen(js_name = pythonPackages)]
pub fn python_packages() -> String {
    let installed: Vec<String> = zipp_vm::python_packages::installed_packages()
        .iter()
        .map(|(name, version)| format!("{{\"name\":{},\"version\":{}}}", json_string(name), json_string(version)))
        .collect();
    format!(
        "{{\"engineAbi\":{},\"torchBuiltIn\":{},\"installed\":[{}]}}",
        json_string(zipp_vm::python_packages::engine_abi()),
        zipp_vm::python_packages::torch_built_in(),
        installed.join(",")
    )
}
