//! The bundled library compiles on first import (`Program::python_lazy`,
//! `vm::py_lazy`): what a program observes is what it observed when every
//! bundled module it named was compiled up front: imports, tracebacks
//! through library code, shadowing by project modules; plus the modules it
//! never names statically, which `importlib` can now reach as CPython's can.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_project;

fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn run(files: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    big_stack(move || {
        let mut compiled = compile_python_project("main", &files)?;
        let state = compiled.state_mut();
        state.set_limits(200_000_000, None);
        state.run_init()?;
        Ok(state.take_output())
    })
}

#[test]
fn a_library_module_compiles_when_first_imported() {
    let out = run(&[(
        "main",
        "print('before')\nimport pickle\nimport pickle as again\nprint(pickle is again, pickle.__name__)\n\
         def later():\n    import zipfile\n    return zipfile.__name__\nprint(later(), later())\n",
    )])
    .unwrap();
    assert_eq!(out, ["before", "True pickle", "zipfile zipfile"]);
}

#[test]
fn importlib_reaches_a_library_module_the_program_never_names() {
    let out = run(&[(
        "main",
        "import importlib\nm = importlib.import_module('argparse')\nprint(m.__name__, hasattr(m, 'ArgumentParser'))\n",
    )])
    .unwrap();
    assert_eq!(out, ["argparse True"]);
}

#[test]
fn tracebacks_through_library_code_name_its_file_and_line() {
    let out = run(&[(
        "main",
        "import torch\ntry:\n    torch.zeros(2, 3) @ torch.zeros(4, 5)\nexcept RuntimeError as e:\n    \
         import sys\n    print('caught')\ntorch.zeros(2, 3) @ torch.zeros(4, 5)\n",
    )]);
    let err = out.unwrap_err();
    assert!(err.contains("torch.py:"), "{err}");
    assert!(err.contains("File \"main.py\", line 7"), "{err}");
}

#[test]
fn a_project_package_shadows_the_library_one() {
    let out = run(&[
        ("main", "import torch\nprint(torch.VERSION)\nimport torch.nn as nn\nprint(nn.WHERE)\n"),
        ("torch", "VERSION = 'mine'\n"),
        ("torch.nn", "WHERE = 'project'\n"),
    ])
    .unwrap();
    assert_eq!(out, ["mine", "project"]);
}

#[test]
fn modules_share_one_library_module() {
    let out = run(&[
        ("main", "import helper\nimport pathlib\nprint(helper.pathlib is pathlib)\n"),
        ("helper", "import pathlib\n"),
    ])
    .unwrap();
    assert_eq!(out, ["True"]);
}

#[test]
fn every_library_module_compiles_and_imports() {
    // Each on its own, so one module's compile never hides another's.
    for name in [
        "zipp_gpu", "pickle", "zipfile", "pathlib", "argparse", "inspect", "pytest", "torch",
        "torch.nn", "torch.nn.functional", "torch.optim", "torch.utils.data", "torch.distributions",
        "torch.linalg", "torch.fft", "torch.sparse", "torch.ao.quantization", "torch.distributed",
        "torch.amp", "torch.special", "torch.autograd",
    ] {
        let src = format!("import {name}\nprint('{name}')\n");
        let out = run(&[("main", src.as_str())]).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(out, [name], "{name}");
    }
}

#[test]
fn tracebacks_name_project_and_library_files_alike() {
    let err = run(&[
        ("main", "import helper\nhelper.go()\n"),
        ("helper", "import pathlib\n\ndef go():\n    pathlib.Path('missing.txt').read_text()\n"),
    ])
    .unwrap_err();
    assert!(err.contains("File \"main.py\", line 2, in <module>"), "{err}");
    assert!(err.contains("File \"helper.py\", line 4, in go"), "{err}");
    assert!(err.contains("File \"pathlib.py\", line "), "{err}");
}

#[test]
fn a_library_module_compiled_for_one_project_serves_another() {
    // The per-process compile is keyed on what the module's code depends
    // on; a project defining a module the library imports gets its own.
    let a = run(&[("main", "import pickle\nprint(pickle.dumps is not None)\n")]).unwrap();
    let b = run(&[
        ("main", "import other\nimport pickle\nprint(pickle.loads(pickle.dumps([1, 'x'])))\n"),
        ("other", "X = 1\n"),
    ])
    .unwrap();
    // pickle imports struct: a project's own struct is the one it gets.
    let c = run(&[
        ("main", "import pickle\nprint(pickle.struct.PROJECT)\n"),
        ("struct", "PROJECT = 'mine'\n"),
    ])
    .unwrap();
    let d = run(&[("main", "import pickle\nprint(hasattr(pickle.struct, 'pack'))\n")]).unwrap();
    assert_eq!(a, ["True"]);
    assert_eq!(b, ["[1, 'x']"]);
    assert_eq!(c, ["mine"]);
    assert_eq!(d, ["True"]);
}
