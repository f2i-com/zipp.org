//! Python packages a host adds at run time (`zipp_vm::python_packages`): the
//! archive checks, a package's modules compiling on first import next to the
//! bundled library, its runtime JavaScript joining the Python runtime, and,
//! in an engine without the torch package (`python-no-torch`), what
//! `import torch` says until the host adds it.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_project;
use zipp_vm::python_packages;

#[path = "../build/sha256.rs"]
mod sha256;

fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn run(src: &str) -> Result<Vec<String>, String> {
    let files = vec![("main".to_string(), src.to_string())];
    big_stack(move || {
        let mut compiled = compile_python_project("main", &files)?;
        let state = compiled.state_mut();
        state.set_limits(200_000_000, None);
        state.run_init()?;
        Ok(state.take_output())
    })
}

/// An archive in `zipp-python-package 1` format.
fn archive(name: &str, abi: &str, runtime: &[(&str, &str)], modules: &[(&str, &str, &str)]) -> Vec<u8> {
    let mut manifest = format!("{}\nname {name}\nversion 1.0\nengine-abi {abi}\nkernels 0\n", python_packages::PACKAGE_FORMAT);
    let mut blob = Vec::new();
    let add = |text: &str, blob: &mut Vec<u8>| {
        let at = blob.len();
        blob.extend_from_slice(text.as_bytes());
        (at, text.len(), sha256::hex(&sha256::sha256(text.as_bytes())))
    };
    for (file, text) in runtime {
        let (o, l, h) = add(text, &mut blob);
        manifest.push_str(&format!("runtime {file} {o} {l} {h}\n"));
    }
    for (module, file, text) in modules {
        let (o, l, h) = add(text, &mut blob);
        manifest.push_str(&format!("module {module} {file} {o} {l} {h} -\n"));
    }
    manifest.push_str("end\n");
    let mut out = manifest.into_bytes();
    out.extend_from_slice(&blob);
    out
}

#[test]
fn a_package_adds_modules_and_runtime_code() {
    let abi = python_packages::engine_abi();
    let runtime = "(function (R) { const rt = R.__rt; rt.defineModule(\"_greeting\", (g) => { g.set(\"text\", \"hello from the runtime\"); }); })(__zipp_py);";
    let good = archive(
        "greet",
        abi,
        &[("greet.js", runtime)],
        &[("greet", "greet.py", "import _greeting\n\ndef hello():\n    return _greeting.text\n"), ("greet.extra", "greet_extra.py", "VALUE = 42\n")],
    );
    // Refused: another ABI, a tampered file, a module the engine has.
    let wrong = archive("greet", "0000000000000000", &[], &[]);
    assert!(python_packages::install(&wrong).unwrap_err().contains("engine ABI 0000000000000000"));
    let mut tampered = good.clone();
    let last = tampered.len() - 2;
    tampered[last] ^= 1;
    assert!(python_packages::install(&tampered).unwrap_err().contains("SHA-256 does not match"));
    let clash = archive("clash", abi, &[], &[("pickle", "pickle.py", "X = 1\n")]);
    assert!(python_packages::install(&clash).unwrap_err().contains("module `pickle`"));
    assert!(python_packages::install(b"garbage").is_err());
    assert!(!python_packages::installed_packages().iter().any(|(n, _)| n == "greet"));

    let info = python_packages::install(&good).expect("install");
    assert_eq!((info.name.as_str(), info.modules, info.kernels), ("greet", 2, false));
    // The same archive again: accepted, nothing changes. Another one under
    // the same name: refused.
    python_packages::install(&good).expect("install again");
    let other = archive("greet", abi, &[], &[("greet", "greet.py", "X = 2\n")]);
    assert!(python_packages::install(&other).unwrap_err().contains("different package `greet`"));

    let out = run("import greet\nfrom greet import extra\nprint(greet.hello(), extra.VALUE)\n").unwrap();
    assert_eq!(out, ["hello from the runtime 42"]);
    // Tracebacks name the package's files.
    let err = run("import greet\ngreet.hello(1)\n").unwrap_err();
    assert!(err.contains("File \"main.py\", line 2"), "{err}");
}

#[cfg(feature = "python-no-torch")]
#[test]
fn without_the_torch_package_import_torch_says_how_to_add_it() {
    assert!(!python_packages::torch_built_in());
    let err = run("import torch\n").unwrap_err();
    assert!(err.contains("ModuleNotFoundError: No module named 'torch'"), "{err}");
    assert!(err.contains("addPythonPackage"), "{err}");
    let out = run(
        "try:\n    from torch import nn\nexcept ImportError as e:\n    print(type(e).__name__, e.name)\n\
         import zipfile, _zipp_crc\nprint(_zipp_crc.crc32(b'hello'))\n\
         try:\n    import _zipp_tensor\nexcept ModuleNotFoundError:\n    print('no _zipp_tensor')\n",
    )
    .unwrap();
    assert_eq!(out, ["ModuleNotFoundError torch", "907060870", "no _zipp_tensor"]);
}

#[cfg(not(feature = "python-no-torch"))]
#[test]
fn the_built_in_torch_package_refuses_another() {
    assert!(python_packages::torch_built_in());
    let abi = python_packages::engine_abi();
    let torch = archive("torch", abi, &[], &[("torch", "torch.py", "X = 1\n")]);
    assert!(python_packages::install(&torch).unwrap_err().contains("module `torch`"));
    let out = run("import _zipp_crc, _zipp_tensor\nprint(_zipp_crc.crc32(b'hello'), _zipp_tensor.crc32(b'hello'))\n").unwrap();
    assert_eq!(out, ["907060870 907060870"]);
}
