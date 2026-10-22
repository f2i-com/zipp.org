//! The Python frontend's entries in the code cache (`crate::code_cache`):
//! the runtime every program starts from, and library modules compiled for
//! installation.

use crate::bytecode::{FuncProto, Program};
use crate::code_cache::{key, load, store, touch};
pub(super) use crate::code_cache::{enabled, set_dir};

/// The runtime compiled from `source`: loaded when this executable stored
/// it before, else compiled (and stored).
pub(super) fn runtime(source: &str) -> Result<Program, String> {
    let Some((dir, key)) = key(&[b"runtime", source.as_bytes()]) else {
        return crate::compile_only(source, false);
    };
    touch(dir);
    let name = format!("runtime-{key:016x}.bin");
    if let Some(program) =
        load(dir, &name, key).and_then(|p| crate::bytecode_codec::decode(&p, Some(source)).ok())
    {
        return Ok(program);
    }
    let program = crate::compile_only(source, false)?;
    if let Some(payload) = crate::bytecode_codec::encode(&program, Some(source)) {
        store(dir, &name, key, &payload);
    }
    Ok(program)
}

/// A library module's functions compiled for installation, if this
/// executable stored them for the same `context` (the in-process memo key of
/// `compile_lazy`) and `source`.
pub(super) fn load_module(context: &str, source: &str) -> Option<Vec<FuncProto>> {
    let (dir, key) = key(&[b"module", context.as_bytes(), source.as_bytes()])?;
    load(dir, &format!("module-{key:016x}.bin"), key)
        .and_then(|payload| crate::bytecode_codec::decode_functions(&payload).ok())
}

/// Store a library module's functions for [`load_module`].
pub(super) fn store_module(context: &str, source: &str, functions: &[FuncProto]) {
    if let Some((dir, key)) = key(&[b"module", context.as_bytes(), source.as_bytes()]) {
        let payload = crate::bytecode_codec::encode_functions(functions);
        store(dir, &format!("module-{key:016x}.bin"), key, &payload);
    }
}

#[cfg(test)]
mod cache_check {
    use crate::bytecode_codec::{decode, decode_functions, encode, encode_functions};

    /// The runtime (JavaScript: most instruction kinds) comes back exactly.
    #[test]
    fn code_cache_runtime_round_trips() {
        let source = format!("{}\n{}", super::super::RUNTIME_BASE, super::super::RUNTIME_ENTRY);
        let program = crate::compile_only(&source, false).unwrap();
        let bytes = encode(&program, Some(&source)).unwrap();
        let back = decode(&bytes, Some(&source)).unwrap();
        assert!(format!("{back:?}") == format!("{program:?}"), "the runtime changed in the round trip");
        // Without the source, function texts are stored whole.
        let whole = encode(&program, None).unwrap();
        assert!(format!("{:?}", decode(&whole, None).unwrap()) == format!("{program:?}"));
        // Another source cannot stand in for the original.
        assert!(decode(&bytes, Some("")).is_err());
    }

    /// Every library module (the Python emitter's instruction kinds) comes
    /// back exactly.
    #[test]
    fn code_cache_library_modules_round_trip() {
        let program = super::super::compile("import sys\n").unwrap();
        let lazy = program.python_lazy.as_ref().unwrap();
        for k in 0..lazy.modules.len() {
            let functions = super::super::compile_lazy(lazy, k, 1000).unwrap();
            let back = decode_functions(&encode_functions(&functions)).unwrap();
            assert!(
                format!("{back:?}") == format!("{functions:?}"),
                "{} changed in the round trip",
                lazy.modules[k].name
            );
        }
    }
}
