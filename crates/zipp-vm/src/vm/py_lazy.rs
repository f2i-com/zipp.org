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
use super::*;

impl<'p> Vm<'p> {
    pub(super) fn py_compile_module(&mut self, name: Value) -> Result<Value, Thrown> {
        let program: &'p crate::bytecode::Program = self.program;
        let Some(lazy) = program.python_lazy.as_deref() else {
            return Err(Thrown("ImportError: no library modules to compile".into()));
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
            self.eval_funcs.push(f);
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
}
