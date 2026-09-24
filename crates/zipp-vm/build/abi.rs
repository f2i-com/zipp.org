// The Python package ABI: what a package built outside the engine (the torch
// package, zipp_torch.wasm) depends on, hashed. A package's runtime code runs
// against the base runtime's object model and its kernels speak the engine's
// kernel wire format, so the hash covers exactly those sources, as the engine
// embeds them. zipp-vm's build.rs and the package build both call this on
// the same tree; the engine refuses a package whose `engine-abi` differs.
use std::path::Path;

#[allow(dead_code)]
pub const PACKAGE_FORMAT: &str = "zipp-python-package 1";

#[allow(dead_code)]
pub fn package_abi(vm_crate: &Path) -> String {
    let py = vm_crate.join("src/frontend/python/runtime");
    let mut all = Vec::new();
    all.extend_from_slice(PACKAGE_FORMAT.as_bytes());
    for file in ["core.js", "types.js", "builtins.js", "stdlib.js", "storage.js", "entry.js"] {
        let text = std::fs::read_to_string(py.join(file)).unwrap_or_else(|e| panic!("runtime/{file}: {e}"));
        all.extend_from_slice(file.as_bytes());
        // Line endings as checked out (CRLF on some Windows setups) must not
        // make two builds of one tree disagree.
        all.extend_from_slice(super::minify::strip_javascript(&text.replace("\r\n", "\n")).as_bytes());
    }
    for file in ["args.rs", "wire.rs"] {
        let text = std::fs::read_to_string(vm_crate.join("src/vm/py_tensor").join(file))
            .unwrap_or_else(|e| panic!("py_tensor/{file}: {e}"));
        all.extend_from_slice(file.as_bytes());
        all.extend_from_slice(text.replace("\r\n", "\n").as_bytes());
    }
    super::sha256::hex(&super::sha256::sha256(&all)[..8])
}
