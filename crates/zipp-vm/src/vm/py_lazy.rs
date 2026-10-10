//! `__zipp_py_compile(name)`: a Python program's library modules compile on
//! first import (`Program::python_lazy`, filled by the Python frontend).
//!
//! The runtime registers every library module the program may import with
//! no code; the first `import` of one calls this native, which compiles the
//! module (`PyLazy::compile`, the frontend's own module emission), installs
//! its functions past every function already installed, the way dynamic code
//! is (`eval_funcs`), and answers the module's top-level code object, which
//! the runtime keeps and runs exactly as it runs an eagerly compiled module.
//! The functions are owned by the program (`PyLazyModule::installed`), so
//! they live as long as the program does and are freed with it. The native
//! is bound by slot only in the program the Python frontend built, like the
//! other `__zipp_py_*` natives.
//!
//! `__zipp_py_vfs_read(index)`: the bytes of a file the host gave the
//! program (`PyLazy::files`), asked for when the program first reads it.
//! The entry code names every path with its index and nothing else, so a
//! project's files are neither copied into the heap nor decoded until one
//! is opened.
use super::*;

impl<'p> Vm<'p> {
    pub(super) fn py_compile_module(&mut self, name: Value) -> Result<Value, Thrown> {
        let program: &'p crate::bytecode::Program = self.program;
        let Some(lazy) = program.python_lazy.as_deref() else {
            return Err(Thrown::from_static("ImportError: no library modules to compile"));
        };
        let name = self.to_js_string(name)?;
        let Some(k) = lazy.modules.iter().position(|m| m.name == name) else {
            return Err(Thrown(format!("ImportError: no library module named '{name}'")));
        };
        let module = &lazy.modules[k];
        let base = (self.main_func_count + self.eval_funcs.len()) as u32;
        let (first, functions) = match module.installed.get() {
            Some(installed) => installed,
            None => {
                let functions = (lazy.compile)(lazy, k, base).map_err(Thrown)?;
                if functions.is_empty() {
                    return Err(Thrown(format!("ImportError: {name}: no code")));
                }
                module.installed.get_or_init(|| (base, functions.into_boxed_slice()))
            }
        };
        // Installed once per program: the runtime keeps the code object.
        if *first != base {
            return Err(Thrown(format!("ImportError: {name} is already installed")));
        }
        for f in functions.iter() {
            self.install_func(f);
        }
        // Immutable code at stable addresses, exactly as a loader-installed
        // ES module's: its exact id range makes it eligible for the inline
        // caches and native tiers the program's own functions get. (No module
        // namespace: Python code has no `import.meta` or `import()`.)
        self.module_func_ranges.push((base, base + functions.len() as u32, u32::MAX));
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        self.jit.python_functions_installed(base as usize, functions);
        // The top level is the module's last function (`emit_module`).
        let top = base + functions.len() as u32 - 1;
        let code = Value::heap(self.heap.alloc(HeapObj::Func(top)));
        self.realm_tag_new(code.heap_index());
        Ok(code)
    }

    /// File `index` of the program's virtual filesystem as a fresh
    /// `Uint8Array`, or `undefined` when there is no such file or the host
    /// can no longer read it.
    pub(super) fn py_vfs_read(&mut self, index: Value) -> Result<Value, Thrown> {
        let program: &'p crate::bytecode::Program = self.program;
        let index = self.to_number(index)?;
        let file = program
            .python_lazy
            .as_deref()
            .filter(|_| index >= 0.0 && index.fract() == 0.0)
            .and_then(|lazy| lazy.files.get(index as usize));
        let read;
        let bytes: &[u8] = match file {
            Some(crate::bytecode::PyFile::Bytes(bytes)) => bytes,
            Some(crate::bytecode::PyFile::Deferred { read: source, .. }) => match source() {
                // A file that grew past the limit since the host listed it
                // is refused, as it would have been when listed.
                Some(bytes) if bytes.len() <= crate::bytecode::PY_VFS_FILE_LIMIT => {
                    read = bytes;
                    &read
                }
                _ => return Ok(Value::UNDEFINED),
            },
            None => return Ok(Value::UNDEFINED),
        };
        self.preflight_native_iteration_work(bytes.len() as u64)?;
        let buffer = self.alloc_array_buffer(bytes.len())?;
        if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(buffer) {
            data.copy_from_slice(bytes);
        }
        Ok(self.alloc_typed_array(buffer, 1, 0, bytes.len()))
    }
}
