//! Python packages a host adds to the engine at run time: the torch package
//! (zipp_torch.wasm) for the WebAssembly `python` artifact, which ships
//! without it.
//!
//! A package is an archive of Python modules (compiled on first import, like
//! the bundled library, see `frontend::python`), runtime JavaScript compiled
//! into the Python runtime ahead of `entry.js` (trusted code with the
//! runtime's own reach, like the bundled runtime), and optionally the native
//! tensor kernels, which run in the package's own WebAssembly module: the
//! engine sends each kernel call there through one host function
//! ([`set_kernel_bridge`], see `vm::py_tensor::wire`).
//!
//! Packages are process-wide and can only be added: a program compiled after
//! [`install`] sees them, one compiled before is unaffected. The archive is
//! checked before anything is registered: its format, the engine ABI it was
//! built against ([`engine_abi`], a hash of what a package depends on in the
//! engine, the kernel wire format included), and the SHA-256 of every file.
//!
//! Archive layout: the line `zipp-python-package 1`, then a manifest of
//! `key value...` lines ended by `end`, then the files' bytes. Manifest keys:
//! `name`, `version`, `engine-abi`, `kernels <1 when the package brings them, else 0>`,
//! `runtime <file> <offset> <length> <sha256>` (runtime JavaScript, in
//! order) and `module <name> <file> <offset> <length> <sha256> <imports>`
//! (a Python module and its import statements, `-` for none, in the form
//! `frontend::python::Bundled::imports` takes). Offsets count from the first
//! byte after `end\n`.
use std::sync::Mutex;

#[path = "../build/sha256.rs"]
mod sha256;

/// The archive's first line.
pub const PACKAGE_FORMAT: &str = "zipp-python-package 1";

/// The engine ABI a package must have been built against (see
/// `build/abi.rs`).
pub fn engine_abi() -> &'static str {
    include_str!(concat!(env!("OUT_DIR"), "/package_abi.txt"))
}

/// Whether this engine has the torch package built in.
pub fn torch_built_in() -> bool {
    cfg!(not(feature = "python-no-torch"))
}

/// The library modules built into the engine, one name per line.
fn built_in_modules() -> impl Iterator<Item = &'static str> {
    let base = include_str!(concat!(env!("OUT_DIR"), "/library_base.txt"));
    #[cfg(not(feature = "python-no-torch"))]
    let torch = include_str!(concat!(env!("OUT_DIR"), "/library_torch.txt"));
    #[cfg(feature = "python-no-torch")]
    let torch = "";
    base.lines().chain(torch.lines())
}

/// A module an installed package provides.
pub(crate) struct PackageModule {
    pub name: &'static str,
    pub source: &'static str,
    pub imports: &'static str,
}

/// An installed package.
pub(crate) struct Installed {
    pub name: String,
    pub version: String,
    pub modules: Vec<PackageModule>,
    pub runtime: Vec<&'static str>,
    /// The archive, to recognise a second install of the same package.
    digest: [u8; 32],
}

struct Registry {
    packages: Vec<&'static Installed>,
    generation: u64,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    packages: Vec::new(),
    generation: 0,
});

/// The installed packages and a generation that changes with each install.
pub(crate) fn installed() -> (u64, Vec<&'static Installed>) {
    let r = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    (r.generation, r.packages.clone())
}

/// What [`install`] registered.
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub modules: usize,
    pub kernels: bool,
}

/// The names and versions of the installed packages.
pub fn installed_packages() -> Vec<(String, String)> {
    installed().1.iter().map(|p| (p.name.clone(), p.version.clone())).collect()
}

/// Check and register a package archive. Installing the same archive again
/// does nothing; a different archive for an installed name, a module name
/// the engine or another package already has, or any failed check is
/// refused with the reason, and nothing is registered.
pub fn install(archive: &[u8]) -> Result<PackageInfo, String> {
    let digest = sha256::sha256(archive);
    let fail = |why: String| Err(format!("addPythonPackage: {why}"));
    let text_end = match find(archive, b"\nend\n") {
        Some(at) => at + 5,
        None => return fail("not a Python package archive (no manifest)".into()),
    };
    let Ok(manifest) = std::str::from_utf8(&archive[..text_end]) else {
        return fail("the manifest is not UTF-8".into());
    };
    let blob = &archive[text_end..];
    let mut lines = manifest.lines();
    if lines.next() != Some(PACKAGE_FORMAT) {
        return fail(format!("not a `{PACKAGE_FORMAT}` archive"));
    }
    let (mut name, mut version, mut abi, mut kernels) = (None, String::new(), None, None);
    let mut runtime = Vec::new();
    let mut modules = Vec::new();
    let file = |file: &str, offset: &str, len: &str, hash: &str| -> Result<&'static str, String> {
        let (Ok(offset), Ok(len)) = (offset.parse::<usize>(), len.parse::<usize>()) else {
            return Err(format!("{file}: bad offset or length"));
        };
        let bytes = offset
            .checked_add(len)
            .and_then(|end| blob.get(offset..end))
            .ok_or_else(|| format!("{file}: outside the archive"))?;
        if sha256::hex(&sha256::sha256(bytes)) != hash {
            return Err(format!("{file}: SHA-256 does not match the manifest"));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| format!("{file}: not UTF-8"))?;
        Ok(Box::leak(text.to_owned().into_boxed_str()))
    };
    for line in lines {
        let fields: Vec<&str> = line.split(' ').collect();
        match fields[..] {
            ["name", n] => name = Some(n.to_owned()),
            ["version", v] => version = v.to_owned(),
            ["engine-abi", a] => abi = Some(a.to_owned()),
            ["kernels", k] => kernels = k.parse::<u32>().ok(),
            ["runtime", f, o, l, h] => runtime.push((f, o, l, h)),
            ["module", n, f, o, l, h, imports] => modules.push((n, f, o, l, h, imports)),
            ["end"] | [""] => {}
            _ => return fail(format!("unknown manifest line {line:?}")),
        }
    }
    let Some(name) = name else {
        return fail("the manifest names no package".into());
    };
    if abi.as_deref() != Some(engine_abi()) {
        return fail(format!(
            "package `{name}` was built for engine ABI {}, this engine is {}: use the package built with this engine",
            abi.as_deref().unwrap_or("(none)"),
            engine_abi()
        ));
    }
    let kernels = kernels.unwrap_or(0);
    if kernels != 0 && torch_built_in() {
        return fail(format!("package `{name}` brings tensor kernels, and this engine has them built in"));
    }
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(have) = reg.packages.iter().find(|p| p.name == name) {
        return if have.digest == digest {
            Ok(PackageInfo {
                name,
                version: have.version.clone(),
                modules: have.modules.len(),
                kernels: kernels != 0,
            })
        } else {
            fail(format!("a different package `{name}` is installed already"))
        };
    }
    for (n, ..) in &modules {
        let taken = built_in_modules().any(|b| b == *n) || reg.packages.iter().any(|p| p.modules.iter().any(|m| m.name == *n));
        if taken {
            return fail(format!("module `{n}` of package `{name}` is provided by the engine or another package already"));
        }
    }
    let mut chunks = Vec::with_capacity(runtime.len());
    for (f, o, l, h) in runtime {
        chunks.push(file(f, o, l, h).map_err(|e| format!("addPythonPackage: {e}"))?);
    }
    let mut mods = Vec::with_capacity(modules.len());
    for (n, f, o, l, h, imports) in modules {
        let source = file(f, o, l, h).map_err(|e| format!("addPythonPackage: {e}"))?;
        let imports = if imports == "-" { "" } else { imports };
        mods.push(PackageModule {
            name: Box::leak(n.to_owned().into_boxed_str()),
            source,
            imports: Box::leak(imports.to_owned().into_boxed_str()),
        });
    }
    let info = PackageInfo {
        name: name.clone(),
        version: version.clone(),
        modules: mods.len(),
        kernels: kernels != 0,
    };
    reg.packages.push(Box::leak(Box::new(Installed {
        name,
        version,
        modules: mods,
        runtime: chunks,
        digest,
    })));
    reg.generation += 1;
    Ok(info)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The host function that runs a tensor kernel call in the torch package's
/// module: request bytes in, response bytes out (`None`: the kernel
/// declined without running; the runtime runs its own loop).
pub type KernelBridge = fn(&[u8]) -> Option<Vec<u8>>;

static BRIDGE: Mutex<Option<KernelBridge>> = Mutex::new(None);

/// Route tensor kernel calls to `bridge` (an engine without the kernels
/// built in; see `vm::py_tensor::wire`).
pub fn set_kernel_bridge(bridge: KernelBridge) {
    *BRIDGE.lock().unwrap_or_else(|e| e.into_inner()) = Some(bridge);
}

#[allow(dead_code)]
pub(crate) fn kernel_bridge() -> Option<KernelBridge> {
    *BRIDGE.lock().unwrap_or_else(|e| e.into_inner())
}
