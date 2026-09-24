// The torch package archive (`$OUT_DIR/torch.zpkg`, format in zipp-vm's
// `python_packages`): the `torch` rows of zipp-vm's lib/modules.txt and
// runtime/tensor.js, comment-stripped exactly as the engine strips what it
// embeds, each module with its import statements, every file with its
// SHA-256, and the engine ABI of the tree it was built from.
use std::fmt::Write;
use std::path::{Path, PathBuf};

#[path = "../../zipp-vm/build/minify.rs"]
mod minify;
#[path = "../../zipp-vm/build/pyimports.rs"]
mod pyimports;
#[path = "../../zipp-vm/build/sha256.rs"]
mod sha256;
#[path = "../../zipp-vm/build/abi.rs"]
mod abi;

fn main() {
    let vm = Path::new("../../zipp-vm");
    let lib = vm.join("src/frontend/python/lib");
    let runtime = vm.join("src/frontend/python/runtime");
    for p in [
        "build.rs",
        "../../zipp-vm/build",
        "../../zipp-vm/src/frontend/python/lib",
        "../../zipp-vm/src/frontend/python/runtime",
        "../../zipp-vm/src/vm/py_tensor",
    ] {
        println!("cargo:rerun-if-changed={p}");
    }
    let mut manifest = String::new();
    let mut blob: Vec<u8> = Vec::new();
    // Each file stored once (several modules share torch_alias.py).
    let mut stored: Vec<(String, usize, usize, String)> = Vec::new();
    let mut store = |file: &str, text: &str, blob: &mut Vec<u8>| -> (usize, usize, String) {
        if let Some((_, o, l, h)) = stored.iter().find(|s| s.0 == file) {
            return (*o, *l, h.clone());
        }
        let bytes = text.as_bytes();
        let entry = (blob.len(), bytes.len(), sha256::hex(&sha256::sha256(bytes)));
        blob.extend_from_slice(bytes);
        stored.push((file.to_owned(), entry.0, entry.1, entry.2.clone()));
        entry
    };
    writeln!(manifest, "{}", abi::PACKAGE_FORMAT).unwrap();
    writeln!(manifest, "name torch").unwrap();
    writeln!(manifest, "version {}", env!("CARGO_PKG_VERSION")).unwrap();
    writeln!(manifest, "engine-abi {}", abi::package_abi(vm)).unwrap();
    writeln!(manifest, "kernels 1").unwrap();
    let tensor = std::fs::read_to_string(runtime.join("tensor.js")).expect("runtime/tensor.js");
    let (o, l, h) = store("tensor.js", &minify::strip_javascript(&tensor), &mut blob);
    writeln!(manifest, "runtime tensor.js {o} {l} {h}").unwrap();
    let list = std::fs::read_to_string(lib.join("modules.txt")).expect("lib/modules.txt");
    for line in list.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [name, file, package] = f[..] else {
            panic!("lib/modules.txt: {line:?}");
        };
        if package != "torch" {
            continue;
        }
        let source = std::fs::read_to_string(lib.join(file)).unwrap_or_else(|e| panic!("lib/{file}: {e}"));
        let imports: Vec<String> = pyimports::imports(&source)
            .into_iter()
            .map(|stmt| match stmt {
                pyimports::ImportStmt::Plain(v) => format!("i|{}", v.join(",")),
                pyimports::ImportStmt::From(level, module, v) => {
                    format!("{level}|{}|{}", module.unwrap_or_default(), v.join(","))
                }
            })
            .collect();
        let imports = if imports.is_empty() { "-".to_owned() } else { imports.join(";") };
        let (o, l, h) = store(file, &minify::strip_python(&source), &mut blob);
        writeln!(manifest, "module {name} {file} {o} {l} {h} {imports}").unwrap();
    }
    manifest.push_str("end\n");
    let mut archive = manifest.into_bytes();
    archive.extend_from_slice(&blob);
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("torch.zpkg");
    std::fs::write(out, archive).unwrap();
}
