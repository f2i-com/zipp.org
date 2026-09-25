//! Compiled Python code kept on disk between processes: the runtime and the
//! library modules.
//!
//! Every Python program starts from the same runtime, about 0.7 MB of
//! JavaScript that took most of a hello-world run to compile, and a program
//! that imports a library module (torch: about 1 MB of Python) compiles it
//! on import. A host that runs one program per process (the `zipp` command)
//! opts in with [`set_dir`]: the first process compiles and stores the
//! bytecode (`bytecode_codec`), and later ones load it, in milliseconds
//! instead of tens of them.
//!
//! Entries live in a folder per executable (a hash of its path, size and
//! modification time, and the engine version), so a rebuilt or another
//! engine never reads them; the few most recently used executables' folders
//! are kept and older ones removed. An entry's name is a hash of what it was
//! compiled from — the source text, the script goal, every `ZIPP_*`
//! environment variable (several change what the compilers emit) and, for a
//! module, the program context its code depends on (the in-process memo's
//! key in `compile_lazy`). The file carries that key and a checksum of its
//! contents; anything that does not match (a torn write, a truncated file)
//! is ignored and the code compiled as if there were no cache.
//!
//! The folder is the user's own (like CPython's `__pycache__`): its entries
//! are trusted exactly as far as the executable they belong to.

use crate::bytecode::{FuncProto, Program};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

const MAGIC: &[u8; 8] = b"ZIPPPY\x01\x00";
/// Executables' folders kept besides the current one.
const KEEP: usize = 3;
/// The marker whose modification time says when an executable's folder was
/// last used (refreshed at most hourly).
const USED: &str = "used";

/// Keep compiled code under `dir` (created when first written), or not at
/// all (`None`, the default).
pub(crate) fn set_dir(dir: Option<PathBuf>) {
    *DIR.lock().unwrap_or_else(|e| e.into_inner()) = dir;
}

/// A 64-bit FNV-1a over 8-byte words (the tail bytewise): quick enough for
/// the 0.7 MB runtime text on every start. Not cryptographic; entries are
/// trusted as the executable is.
#[derive(Clone)]
struct Hash(u64);

impl Hash {
    fn new() -> Hash {
        Hash(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, bytes: &[u8]) {
        const P: u64 = 0x0000_0100_0000_01b3;
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.0 = (self.0 ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(P);
            self.0 ^= self.0 >> 29;
        }
        for &b in chunks.remainder() {
            self.0 = (self.0 ^ b as u64).wrapping_mul(P);
        }
        // The length, so field boundaries count.
        self.0 = (self.0 ^ bytes.len() as u64).wrapping_mul(P);
        self.0 ^= self.0 >> 31;
    }
}

/// Per process: the executable's folder and the hash of the settings every
/// key includes. `None` when there is no cache folder or no executable.
fn context() -> Option<&'static (PathBuf, Hash)> {
    static CONTEXT: OnceLock<Option<(PathBuf, Hash)>> = OnceLock::new();
    CONTEXT
        .get_or_init(|| {
            let base = DIR.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
            let exe = std::env::current_exe().ok()?;
            let meta = std::fs::metadata(&exe).ok()?;
            let modified = meta
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?;
            let mut id = Hash::new();
            id.bytes(env!("CARGO_PKG_VERSION").as_bytes());
            id.bytes(&crate::bytecode_codec::FORMAT.to_le_bytes());
            id.bytes(exe.as_os_str().as_encoded_bytes());
            id.bytes(&meta.len().to_le_bytes());
            id.bytes(&modified.as_nanos().to_le_bytes());
            id.bytes(&[cfg!(debug_assertions) as u8]);
            let mut settings = Hash::new();
            let mut vars: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
                .filter(|(name, _)| name.as_encoded_bytes().starts_with(b"ZIPP_"))
                .collect();
            vars.sort();
            for (name, value) in vars {
                settings.bytes(name.as_encoded_bytes());
                settings.bytes(value.as_encoded_bytes());
            }
            Some((base.join(format!("{:016x}", id.0)), settings))
        })
        .as_ref()
}

fn key(parts: &[&[u8]]) -> Option<(&'static Path, u64)> {
    let (dir, settings) = context()?;
    let mut h = settings.clone();
    h.bytes(&[crate::front::pure_script_goal() as u8]);
    for part in parts {
        h.bytes(part);
    }
    Some((dir, h.0))
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut h = Hash::new();
    h.bytes(bytes);
    h.0
}

/// The payload of the entry `name` of `dir`, if it is there, intact and
/// written for `key`.
fn load(dir: &Path, name: &str, key: u64) -> Option<Vec<u8>> {
    let mut bytes = std::fs::read(dir.join(name)).ok()?;
    let header = bytes.get(..32)?;
    if &header[..8] != MAGIC || u64::from_le_bytes(header[8..16].try_into().ok()?) != key {
        return None;
    }
    let len = u64::from_le_bytes(header[16..24].try_into().ok()?) as usize;
    let sum = u64::from_le_bytes(header[24..32].try_into().ok()?);
    let payload = bytes.get(32..)?;
    if payload.len() != len || checksum(payload) != sum {
        return None;
    }
    bytes.drain(..32);
    Some(bytes)
}

/// Write the entry `name` (whole or not at all). The first write of an
/// executable's folder also removes the oldest other executables' folders.
/// Failures only cost a later process a compile.
fn store(dir: &Path, name: &str, key: u64, payload: &[u8]) {
    let fresh = !dir.exists();
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if fresh {
        let _ = std::fs::write(dir.join(USED), b"");
    }
    let mut bytes = Vec::with_capacity(32 + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&key.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&checksum(payload).to_le_bytes());
    bytes.extend_from_slice(payload);
    let path = dir.join(name);
    let tmp = dir.join(format!("{name}.tmp{}", std::process::id()));
    if std::fs::write(&tmp, &bytes).is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    if fresh {
        prune(dir);
    }
}

/// Note that this executable's folder is in use (at most hourly).
fn touch(dir: &Path) {
    let marker = dir.join(USED);
    let stale = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age.as_secs() > 3600);
    if stale && dir.exists() {
        let _ = std::fs::write(marker, b"");
    }
}

/// Remove all but the [`KEEP`] most recently used other executables'
/// folders under the cache folder.
fn prune(current: &Path) {
    let Some(base) = current.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let mut others: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p != current && p.is_dir())
        .filter(|p| {
            // Only folders this cache made: sixteen hex digits.
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.len() == 16 && n.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .map(|p| {
            let used = std::fs::metadata(p.join(USED))
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (used, p)
        })
        .collect();
    others.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, old) in others.into_iter().skip(KEEP) {
        let _ = std::fs::remove_dir_all(old);
    }
}

/// The runtime compiled from `source`: loaded when this executable stored
/// it before, else compiled (and stored).
pub(crate) fn runtime(source: &str) -> Result<Program, String> {
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
pub(crate) fn load_module(context: &str, source: &str) -> Option<Vec<FuncProto>> {
    let (dir, key) = key(&[b"module", context.as_bytes(), source.as_bytes()])?;
    load(dir, &format!("module-{key:016x}.bin"), key)
        .and_then(|payload| crate::bytecode_codec::decode_functions(&payload).ok())
}

/// Store a library module's functions for [`load_module`].
pub(crate) fn store_module(context: &str, source: &str, functions: &[FuncProto]) {
    if let Some((dir, key)) = key(&[b"module", context.as_bytes(), source.as_bytes()]) {
        let payload = crate::bytecode_codec::encode_functions(functions);
        store(dir, &format!("module-{key:016x}.bin"), key, &payload);
    }
}

/// Whether compiled code is being kept (so callers compute a module's
/// context key even when the in-process memo is off).
pub(crate) fn enabled() -> bool {
    context().is_some()
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

    /// Truncated or altered bytes are an error (or, when an altered byte is
    /// still a valid encoding, a program), never a panic.
    #[test]
    fn code_cache_damaged_bytes_do_not_panic() {
        let source = "function f(a, b) { return a + b * 2; }\nvar o = { x: 1, y: [2, 3] }; f(1, 2);";
        let program = crate::compile_only(source, false).unwrap();
        let bytes = encode(&program, Some(source)).unwrap();
        for cut in 0..bytes.len() {
            assert!(decode(&bytes[..cut], Some(source)).is_err());
        }
        for i in 0..bytes.len() {
            for flip in [0x01u8, 0x80, 0xff] {
                let mut damaged = bytes.clone();
                damaged[i] ^= flip;
                let _ = decode(&damaged, Some(source));
            }
        }
    }

    /// An entry is read back only whole and for its own key.
    #[test]
    fn code_cache_entries_check_key_and_contents() {
        let dir = std::env::temp_dir().join(format!("zipp-code-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let entry = dir.join("0123456789abcdef");
        super::store(&entry, "e.bin", 7, b"payload");
        assert!(entry.join(super::USED).exists());
        assert_eq!(super::load(&entry, "e.bin", 7).as_deref(), Some(&b"payload"[..]));
        assert_eq!(super::load(&entry, "e.bin", 8), None);
        let path = entry.join("e.bin");
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(super::load(&entry, "e.bin", 7), None);
        std::fs::write(&path, &bytes[..20]).unwrap();
        assert_eq!(super::load(&entry, "e.bin", 7), None);
        // Other executables' folders beyond the most recent few are removed.
        for i in 0..5 {
            let other = dir.join(format!("{i:016x}"));
            super::store(&other, "e.bin", 1, b"x");
        }
        let left = std::fs::read_dir(&dir).unwrap().count();
        assert!(left <= super::KEEP + 1, "{left} folders left");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
