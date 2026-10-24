//! Compiled code kept on disk between processes: large JavaScript scripts,
//! and Python's runtime and library modules (`lang::python::frontend`'s
//! entries).
//!
//! A JavaScript application is often one large bundle (TypeScript's compiler
//! is 10 MB, half a second to parse and compile), every Python program
//! starts from the same runtime, about 0.7 MB of JavaScript that took most of
//! a hello-world run to compile, and a program that imports a library module
//! (torch: about 1 MB of Python) compiles it on import. A host that runs one
//! program per process (the `zipp` command) opts in with [`set_dir`]: the
//! first process compiles and stores the bytecode (`bytecode_codec`), and
//! later ones load it, in milliseconds instead of tens or hundreds of them.
//!
//! Entries live in a folder per executable (a hash of its path, size and
//! modification time, and the engine version), so a rebuilt or another
//! engine never reads them; the few most recently used executables' folders
//! are kept and older ones removed. An entry's name is a hash of what it was
//! compiled from — the source text, the script goal, every `ZIPP_*`
//! environment variable (several change what the compilers emit; the few
//! that only observe a run, [`OBSERVERS`], are left out) and, for a Python
//! module, the program context its code depends on (the in-process memo's
//! key in `compile_lazy`). The file carries that key and a checksum of its
//! contents; anything that does not match (a torn write, a truncated file)
//! is ignored and the code compiled as if there were no cache. A folder
//! keeps [`FOLDER_BYTES`] of entries, those used most recently.
//!
//! The folder is the user's own (like CPython's `__pycache__`): its entries
//! are trusted exactly as far as the executable they belong to.

use crate::bytecode::Program;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

const MAGIC: &[u8; 8] = b"ZIPPPY\x01\x00";
/// Executables' folders kept besides the current one.
const KEEP: usize = 3;
/// The marker whose modification time says when an executable's folder was
/// last used (refreshed at most hourly).
const USED: &str = "used";
/// Ownership marker for namespaces created by this version. Older namespaces
/// have an empty `used` file and are recognized by their cache entry headers.
const USED_CONTENT: &[u8] = b"zipp-code-cache-v1\n";
/// Bytes of entries kept in one executable's folder. A combination of
/// settings needs the Python runtime (0.76 MB) and each library module a
/// program imports (the bundled library has about fifty), and a large
/// script two thirds to four thirds of its size (TypeScript's 10 MB: 6.8 MB;
/// Prettier's 2.3 MB: 3.0 MB). Without a bound, a caller that varies a
/// `ZIPP_*` value from run to run left the runtime behind again for every
/// run.
const FOLDER_BYTES: u64 = 64 * 1024 * 1024;
/// How long an entry's modification time, which says when it was last used,
/// may go without being brought up to date by a load; and how old a write
/// left unfinished (its process ended first) is before it is removed.
const REFRESH_SECS: u64 = 3600;
/// `ZIPP_*` variables that observe a run without changing what the
/// compilers emit, so they are not part of an entry's key: a profiled or
/// logged run starts from the same compiled code as a plain one, and a dump
/// path that differs every run does not miss the cache every run.
const OBSERVERS: &[&str] = &[
    "ZIPP_CACHE_DIR",
    "ZIPP_CENSUS",
    "ZIPP_JITLOG",
    "ZIPP_PROF",
    "ZIPP_PROF_PC",
    "ZIPP_PROF_PC_DUMP",
];

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
                .filter(|(name, _)| !OBSERVERS.iter().any(|o| name == std::ffi::OsStr::new(o)))
                .collect();
            crate::cold_sort::sort(&mut vars);
            for (name, value) in vars {
                settings.bytes(name.as_encoded_bytes());
                settings.bytes(value.as_encoded_bytes());
            }
            Some((base.join(format!("{:016x}", id.0)), settings))
        })
        .as_ref()
}

/// The executable's folder and the key of an entry compiled from `parts`,
/// when compiled code is kept.
pub(crate) fn key(parts: &[&[u8]]) -> Option<(&'static Path, u64)> {
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
pub(crate) fn load(dir: &Path, name: &str, key: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    // A caller may point ZIPP_CACHE_DIR at a folder with unrelated hex-named
    // children. Never deserialize a cache entry from one of those children.
    cache_namespace_owned(dir)?;
    let path = dir.join(name);
    plain_metadata(&path, false)?;
    let mut file = std::fs::File::open(&path).ok()?;
    let metadata = file.metadata().ok()?;
    if metadata.len() > FOLDER_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes).ok()?;
    drop(file);
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
    // An entry's modification time is when it was last used, which is what
    // `trim` keeps entries by: the runtime is written once and loaded by
    // every run, and must not be the first to go. Brought up to date at most
    // hourly, so a run seldom writes.
    let stale = metadata
        .modified()
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_none_or(|age| age.as_secs() > REFRESH_SECS);
    if stale {
        if let Ok(file) = std::fs::File::options().write(true).open(&path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
    }
    bytes.drain(..32);
    Some(bytes)
}

/// Write the entry `name` (whole or not at all). The first write of an
/// executable's folder also removes the oldest other executables' folders.
/// Failures only cost a later process a compile.
pub(crate) fn store(dir: &Path, name: &str, key: u64, payload: &[u8]) {
    let fresh = if std::fs::symlink_metadata(dir).is_ok() {
        // Never adopt an existing, unidentified directory and then trim its
        // unrelated `.bin` files. Old empty-marker namespaces are accepted by
        // `cache_namespace_owned` when their entries match our format.
        if cache_namespace_owned(dir).is_none() {
            return;
        }
        false
    } else {
        let Some(parent) = dir.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() || std::fs::create_dir(dir).is_err() {
            return;
        }
        if std::fs::write(dir.join(USED), USED_CONTENT).is_err() {
            return;
        }
        true
    };
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
    } else {
        trim(dir, FOLDER_BYTES);
    }
}

/// Keep the most recently used entries of `dir` that fit in `limit` bytes,
/// and remove writes left unfinished over [`REFRESH_SECS`] ago (a process
/// ended while it wrote one).
fn trim(dir: &Path, limit: u64) {
    if cache_namespace_owned(dir).is_none() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut used: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        let Some(metadata) = plain_metadata(&path, false) else {
            continue;
        };
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if cache_entry_key(name).is_some() {
            used.push((modified, metadata.len(), path));
        } else if cache_temp_name(name)
            && modified
                .elapsed()
                .is_ok_and(|age| age.as_secs() > REFRESH_SECS)
        {
            let _ = std::fs::remove_file(path);
        }
    }
    if used.iter().map(|entry| entry.1).sum::<u64>() <= limit {
        return;
    }
    crate::cold_sort::sort_by(&mut used, |a, b| b.0.cmp(&a.0));
    let mut kept = 0u64;
    for (_, size, path) in used {
        kept += size;
        if kept > limit {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Note that this executable's folder is in use (at most hourly).
pub(crate) fn touch(dir: &Path) {
    let marker = dir.join(USED);
    let stale = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age.as_secs() > 3600);
    if stale && cache_namespace_owned(dir).is_some() {
        let _ = std::fs::write(marker, USED_CONTENT);
    }
}

/// A namespace may be pruned only if all of its contents are recognizable
/// cache files. The caller can choose `ZIPP_CACHE_DIR`, so a hex directory name
/// alone is not evidence that the directory belongs to us. Reject symlinks
/// (including Windows junctions/reparse points) before recursive deletion.
fn plain_metadata(path: &Path, directory: bool) -> Option<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let kind = metadata.file_type();
    if if directory {
        !kind.is_dir()
    } else {
        !kind.is_file()
    } {
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return None;
        }
    }
    Some(metadata)
}

fn cache_entry_key(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".bin")?;
    let key = ["script-", "runtime-", "module-"]
        .into_iter()
        .find_map(|prefix| stem.strip_prefix(prefix))?;
    if key.len() != 16 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(key, 16).ok()
}

fn cache_temp_name(name: &str) -> bool {
    let Some((entry, pid)) = name.split_once(".tmp") else {
        return false;
    };
    cache_entry_key(entry).is_some() && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CacheMarker {
    Current,
    Legacy,
}

fn cache_marker(path: &Path) -> Option<(CacheMarker, std::time::SystemTime)> {
    use std::io::Read;

    plain_metadata(path, true)?;
    let marker = path.join(USED);
    let metadata = plain_metadata(&marker, false)?;
    let mut contents = Vec::new();
    std::fs::File::open(&marker)
        .ok()?
        .take(USED_CONTENT.len() as u64 + 1)
        .read_to_end(&mut contents)
        .ok()?;
    let kind = if contents == USED_CONTENT {
        CacheMarker::Current
    } else if contents.is_empty() {
        CacheMarker::Legacy
    } else {
        return None;
    };
    Some((kind, metadata.modified().unwrap_or(std::time::UNIX_EPOCH)))
}

/// A current marker makes normal cache reads and writes constant-time. Legacy
/// empty markers get one strict directory check before being upgraded.
fn cache_namespace_owned(path: &Path) -> Option<()> {
    let (kind, _) = cache_marker(path)?;
    if kind == CacheMarker::Legacy {
        cache_namespace_used(path)?;
        let _ = std::fs::write(path.join(USED), USED_CONTENT);
    }
    Some(())
}

/// Full validation is reserved for recursive pruning: every child must be a
/// cache file, never a symlink, junction, unknown file, or subdirectory.
fn cache_namespace_used(path: &Path) -> Option<std::time::SystemTime> {
    use std::io::Read;

    let (_, used) = cache_marker(path)?;

    let mut found_entry = false;
    for entry in std::fs::read_dir(path).ok()? {
        let entry = entry.ok()?;
        if entry.file_name() == USED {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_str()?;
        let entry_metadata = plain_metadata(&entry.path(), false)?;
        if cache_temp_name(name) {
            continue;
        }
        let key = cache_entry_key(name)?;
        if entry_metadata.len() < 32 {
            return None;
        }
        let mut header = [0_u8; 32];
        std::fs::File::open(entry.path())
            .ok()?
            .read_exact(&mut header)
            .ok()?;
        if &header[..8] != MAGIC
            || u64::from_le_bytes(header[8..16].try_into().ok()?) != key
            || u64::from_le_bytes(header[16..24].try_into().ok()?) != entry_metadata.len() - 32
        {
            return None;
        }
        found_entry = true;
    }
    found_entry.then_some(used)
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
        .filter(|p| p != current)
        .filter(|p| {
            // Only folders this cache made: sixteen hex digits.
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.len() == 16 && n.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .filter_map(|p| cache_namespace_used(&p).map(|used| (used, p)))
        .collect();
    crate::cold_sort::sort_by(&mut others, |a, b| b.0.cmp(&a.0));
    for (_, old) in others.into_iter().skip(KEEP) {
        if cache_namespace_used(&old).is_some() {
            let _ = std::fs::remove_dir_all(old);
        }
    }
}

/// Whether compiled code is being kept (so callers compute a module's
/// context key even when the in-process memo is off).
#[cfg(feature = "python")]
pub(crate) fn enabled() -> bool {
    context().is_some()
}

/// Scripts shorter than this are compiled every run. Parsing and compiling a
/// script of this size take about 3 ms (Preact's server renderer, 33 KB:
/// 2.8 ms, against 0.2 ms to decode its entry); the largest benchmark row,
/// 10 KB, takes 0.5 ms, little more than opening and checking an entry
/// would, and a cache of every small script edited and run would be mostly
/// entries never read again.
pub(crate) const SCRIPT_MIN_BYTES: usize = 32 * 1024;

/// Where the compiled program of one JavaScript script is kept (the
/// `zipp` command's script, `crate::run_file_then_exit`).
pub(crate) struct ScriptEntry {
    dir: &'static Path,
    name: String,
    key: u64,
}

/// The entry for the script `source`, when compiled code is kept and the
/// script is large enough to gain from it.
pub(crate) fn script(source: &str) -> Option<ScriptEntry> {
    if source.len() < SCRIPT_MIN_BYTES {
        return None;
    }
    let (dir, key) = key(&[b"script", source.as_bytes()])?;
    touch(dir);
    Some(ScriptEntry {
        dir,
        name: format!("script-{key:016x}.bin"),
        key,
    })
}

impl ScriptEntry {
    /// The program compiled from `source`, if this executable stored it.
    pub(crate) fn load(&self, source: &str) -> Option<Program> {
        load(self.dir, &self.name, self.key)
            .and_then(|payload| crate::bytecode_codec::decode(&payload, Some(source)).ok())
    }

    /// Store `program`, compiled from `source`, for later processes.
    pub(crate) fn store(&self, source: &str, program: &Program) {
        if let Some(payload) = crate::bytecode_codec::encode(program, Some(source)) {
            store(self.dir, &self.name, self.key, &payload);
        }
    }
}

#[cfg(test)]
mod cache_check {
    use crate::bytecode_codec::{decode, encode};

    /// A script with functions, classes, methods and accessors comes back
    /// exactly, every text a range of one shared copy of the source.
    #[test]
    fn code_cache_script_round_trips_with_one_source_copy() {
        let source = "function outer(a) { function inner(b) { return a + b; } return inner; }\n\
                      class K { m() { return 1; } get g() { return 2; } static s() { return 3; } }\n\
                      var f = (x) => x * 2, o = { method() { return f(1); } };\n\
                      outer(1)(2) + new K().m() + K.s() + o.method();";
        let program = crate::compile_only(source, false).unwrap();
        let bytes = encode(&program, Some(source)).unwrap();
        let back = decode(&bytes, Some(source)).unwrap();
        assert!(format!("{back:?}") == format!("{program:?}"), "the script changed in the round trip");
        let copies: std::collections::HashSet<*const u8> = back
            .functions
            .iter()
            .map(|f| &f.source)
            .chain(back.classes.iter().map(|c| &c.source))
            .filter(|text| !text.is_empty())
            .map(|text| text.shared().0)
            .collect();
        assert_eq!(copies.len(), 1, "texts decoded into {} copies", copies.len());
        // Without the source, texts are stored whole.
        let whole = encode(&program, None).unwrap();
        assert!(format!("{:?}", decode(&whole, None).unwrap()) == format!("{program:?}"));
        // Another source cannot stand in for the original.
        assert!(decode(&bytes, Some("")).is_err());
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
        let name = "runtime-0000000000000007.bin";
        super::store(&entry, name, 7, b"payload");
        assert!(entry.join(super::USED).exists());
        assert_eq!(
            super::load(&entry, name, 7).as_deref(),
            Some(&b"payload"[..])
        );
        assert_eq!(super::load(&entry, name, 8), None);
        let path = entry.join(name);
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(super::load(&entry, name, 7), None);
        std::fs::write(&path, &bytes[..20]).unwrap();
        assert_eq!(super::load(&entry, name, 7), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A caller-selected cache root may contain unrelated hex-named folders.
    /// Legacy namespaces with empty markers are still pruned when their files
    /// have recognizable cache names and headers.
    #[test]
    fn code_cache_prunes_only_identifiable_namespaces() {
        let dir = std::env::temp_dir().join(format!(
            "zipp-code-cache-prune-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();

        let foreign = dir.join("cccccccccccccccc");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("important.txt"), b"keep").unwrap();
        super::store(&foreign, "runtime-0000000000000009.bin", 9, b"cache");
        super::touch(&foreign);
        assert!(!foreign.join(super::USED).exists());
        let deceptive = dir.join("dddddddddddddddd");
        std::fs::create_dir(&deceptive).unwrap();
        std::fs::write(deceptive.join(super::USED), b"").unwrap();
        std::fs::write(deceptive.join("important.txt"), b"keep").unwrap();
        super::store(&deceptive, "runtime-0000000000000009.bin", 9, b"cache");
        assert!(!deceptive.join("runtime-0000000000000009.bin").exists());

        let mut old = Vec::new();
        for i in 0..5_u64 {
            let path = dir.join(format!("{i:016x}"));
            super::store(&path, &format!("runtime-{i:016x}.bin"), i, b"old");
            std::fs::write(path.join(super::USED), b"").unwrap();
            std::fs::File::options()
                .write(true)
                .open(path.join(super::USED))
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + i))
                .unwrap();
            old.push(path);
        }

        let current = dir.join("eeeeeeeeeeeeeeee");
        super::store(&current, "script-0000000000000009.bin", 9, b"new");
        std::fs::copy(
            current.join("script-0000000000000009.bin"),
            foreign.join("script-0000000000000009.bin"),
        )
        .unwrap();
        assert_eq!(super::load(&foreign, "script-0000000000000009.bin", 9), None);

        let interrupted = dir.join("aaaaaaaaaaaaaaaa");
        std::fs::create_dir(&interrupted).unwrap();
        std::fs::write(interrupted.join(super::USED), super::USED_CONTENT).unwrap();
        std::fs::write(interrupted.join("runtime-000000000000000a.bin.tmp42"), b"partial")
            .unwrap();
        super::store(&interrupted, "runtime-000000000000000a.bin", 10, b"recovered");
        assert_eq!(
            super::load(&interrupted, "runtime-000000000000000a.bin", 10).as_deref(),
            Some(&b"recovered"[..])
        );
        assert_eq!(
            std::fs::read(foreign.join("important.txt")).unwrap(),
            b"keep"
        );
        assert_eq!(
            std::fs::read(deceptive.join("important.txt")).unwrap(),
            b"keep"
        );
        assert!(current.join(super::USED).exists());
        assert_eq!(old.iter().filter(|path| path.exists()).count(), super::KEEP);
        assert!(!old[0].exists() && !old[1].exists());

        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&current, dir.join("ffffffffffffffff")).is_ok();
        #[cfg(windows)]
        let linked =
            std::os::windows::fs::symlink_dir(&current, dir.join("ffffffffffffffff")).is_ok();
        #[cfg(not(any(unix, windows)))]
        let linked = false;
        if linked {
            assert!(super::cache_namespace_used(&dir.join("ffffffffffffffff")).is_none());
            super::prune(&current);
            assert!(dir.join("ffffffffffffffff").exists());
            assert!(current.exists());
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A folder keeps a bounded size of entries: those used most recently,
    /// where loading an entry is using it. A write a process left unfinished
    /// long ago is removed; a recent one may still be in progress.
    #[test]
    fn code_cache_folder_keeps_what_was_used_last() {
        let dir = std::env::temp_dir().join(format!("zipp-code-cache-trim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let entry = dir.join("0123456789abcdef");
        let name = |i: u64| format!("runtime-{i:016x}.bin");
        let payload = [7u8; 68];
        // Ten entries of 100 bytes, written a day apart long ago, the first
        // of them the oldest.
        let long_ago = |i: u64| std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + 86_400 * i);
        for i in 0..10u64 {
            super::store(&entry, &name(i), i, &payload);
            let file = std::fs::File::options().write(true).open(entry.join(name(i))).unwrap();
            file.set_modified(long_ago(i)).unwrap();
        }
        let present = |i: u64| entry.join(name(i)).exists();
        let (old_tmp, new_tmp) = (
            entry.join("script-0000000000000001.bin.tmp41"),
            entry.join("script-0000000000000002.bin.tmp42"),
        );
        std::fs::write(&old_tmp, b"torn").unwrap();
        std::fs::File::options().write(true).open(&old_tmp).unwrap().set_modified(long_ago(0)).unwrap();
        std::fs::write(&new_tmp, b"writing").unwrap();
        // Everything fits: no entry goes, but the abandoned write does.
        super::trim(&entry, 1000);
        assert!((0..10).all(present));
        assert!(!old_tmp.exists() && new_tmp.exists());
        // The oldest entry is loaded, as the runtime is by every run: that
        // is its use, so it outlives entries written after it.
        assert_eq!(super::load(&entry, &name(0), 0).as_deref(), Some(&payload[..]));
        super::trim(&entry, 450);
        let kept: Vec<u64> = (0..10).filter(|&i| present(i)).collect();
        assert_eq!(kept, [0, 7, 8, 9]);
        assert!(entry.join(super::USED).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
