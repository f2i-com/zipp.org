//! Capture build identity so `zipp --version` can name the exact source that
//! produced the binary.
//!
//! This exists because of a measurement failure, not for cosmetics. A benchmark
//! artifact records the executable's SHA-256, but with no way to ask a binary
//! what it was built from there is nothing to cross-check it against: an A/B run
//! whose two sides were the SAME build reported a clean pass, and the only tell
//! was a ratio that failed to move. See `PERF_ROADMAP.md` B59/B60/B61.
//!
//! Everything here degrades to `"unknown"` rather than failing the build — a
//! source tarball with no `.git`, or a machine without `git` on PATH, must still
//! compile.

use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const PGO_BUILD_CONTRACT: &str = "zipp-pgo-build-v2;cargo=build --locked --release --target=x86_64-pc-windows-msvc --package=zipp-cli --bin=zipp --no-default-features;profile=opt-level=3,lto=fat,codegen-units=1,panic=abort,incremental=false,debug=false,strip=none,debug-assertions=false,overflow-checks=false;pe-stack=reserve-268435456,commit-4096;rustflags=target-cpu=x86-64,linker-flavor=lld-link,profile-use=<verified-profile>;linker=selected-rustc-rust-lld;cc-rs=target-specific-selected-msvc-cl+lib;source=private-readonly-clean-head-snapshot-v1;cargo-config=controlled-cwd+no-home-config;target-dir=fresh;sdk=validated-environment-paths-not-byte-manifested;env=allowlist-v2";
const PGO_BUILD_ENV_POLICY: &str = "zipp-pgo-build-env-allowlist-v2";

// The CLI runs the engine on the Windows main thread. Reserve the same large,
// lazily committed stack that the old worker thread requested so guest-driven
// native re-entry reaches the VM's catchable depth guards instead of the OS
// guard page. This is a PE-header policy, not a request to commit 256 MiB.
pub(crate) const WINDOWS_STACK_RESERVE_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const WINDOWS_STACK_COMMIT_BYTES: u64 = 4 * 1024;

pub(crate) fn windows_stack_link_arg(target: &str) -> Option<String> {
    if target.ends_with("-windows-msvc") {
        Some(format!(
            "/STACK:{WINDOWS_STACK_RESERVE_BYTES},{WINDOWS_STACK_COMMIT_BYTES}"
        ))
    } else if target.ends_with("-windows-gnu") || target.ends_with("-windows-gnullvm") {
        // The GNU and gnullvm targets both drive their linker through a
        // GNU-style compiler frontend, so pass the PE reserve option through
        // with `-Wl`. Their PE default commit is one 4 KiB page. The Windows
        // integration test reads both fields back from the linked binary,
        // independent of linker flavour.
        Some(format!("-Wl,--stack,{WINDOWS_STACK_RESERVE_BYTES}"))
    } else {
        None
    }
}

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("-windows-") {
        let link_arg = windows_stack_link_arg(&target)
            .unwrap_or_else(|| panic!("unsupported Windows linker target: {target}"));
        // Scope the setting to the shipped CLI binary: build-script helpers,
        // unit tests and unrelated workspace binaries keep their normal stack.
        println!("cargo:rustc-link-arg-bin=zipp={link_arg}");
    }

    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (!s.is_empty()).then_some(s)
    };

    // Rerun triggers. HEAD/index alone are NOT enough and getting this wrong
    // defeats the whole point: with only those, editing `crates/zipp-vm/src` did
    // not re-run this script, so a rebuilt binary reported the PREVIOUS tree's
    // identity — two genuinely different builds claiming the same source, which
    // is the exact failure the stamp exists to catch (verified, then fixed).
    //
    // So watch the whole source tree. Cargo accepts a DIRECTORY here and treats
    // any change beneath it as a trigger, which is both shorter and harder to get
    // wrong than enumerating files — the first attempt used `git ls-files`, which
    // without a pathspec lists only files under the build script's own package,
    // so it watched `zipp-cli` alone and the staleness survived. Cargo ignores a
    // `rerun-if-changed` path it cannot match, so that mistake was silent.
    //
    // Paths are relative to this package's root (cargo's CWD for a build script),
    // hence `../..` for the workspace root.
    println!("cargo:rerun-if-changed=build.rs");
    for p in [
        "../../.git/HEAD",
        "../../.git/index",
        "../../crates",
        "../../tools",
        "../../Cargo.toml",
        "../../Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={p}");
    }
    for key in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "PROFILE",
        "ZIPP_PGO_PROFILE_SHA256",
        "ZIPP_PGO_TRAINING_RECIPE_SHA256",
        "ZIPP_PGO_BUILD_RECIPE_SHA256",
        "ZIPP_PGO_BUILD_CONTRACT",
        "ZIPP_PGO_BUILD_ENV_POLICY",
        "ZIPP_PGO_BUILD_ENV_SHA256",
        "ZIPP_PGO_SOURCE_SNAPSHOT_SHA256",
        "ZIPP_PGO_CARGO_IDENTITY",
        "ZIPP_PGO_CARGO_PATH",
        "ZIPP_PGO_CARGO_SHA256",
        "ZIPP_PGO_RUSTC_SHA256",
        "ZIPP_PGO_LINKER_PATH",
        "ZIPP_PGO_LINKER_IDENTITY",
        "ZIPP_PGO_LINKER_SHA256",
        "ZIPP_PGO_MSVC_CL_PATH",
        "ZIPP_PGO_MSVC_CL_IDENTITY",
        "ZIPP_PGO_MSVC_CL_SHA256",
        "ZIPP_PGO_MSVC_LIB_PATH",
        "ZIPP_PGO_MSVC_LIB_IDENTITY",
        "ZIPP_PGO_MSVC_LIB_SHA256",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }

    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());

    // Dirty state, and — more useful for a benchmark — a digest of the exact
    // dirty source. Tracked changes are path-sorted binary diffs; every
    // non-ignored untracked entry contributes its path, kind, and file bytes (or
    // the stored target bytes for a symlink). Length-delimited SHA-256 frames
    // make concatenation unambiguous and disclose none of that source material.
    // If Git or any listed entry cannot be read, retain the dirty bit but mark
    // the digest unknown instead of publishing a partial identity.
    let (dirty, diff_digest) = dirty_source_identity(Path::new("../.."));

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc_version = command_identity(&rustc, &["-vV"]);

    emit("ZIPP_BUILD_COMMIT", &commit);
    emit("ZIPP_BUILD_DIRTY", if dirty { "true" } else { "false" });
    emit("ZIPP_BUILD_DIFF_DIGEST", &diff_digest);
    emit("ZIPP_BUILD_RUSTC", &rustc_version);
    emit(
        "ZIPP_BUILD_TARGET",
        &std::env::var("TARGET").unwrap_or_else(|_| "unknown".into()),
    );
    emit(
        "ZIPP_BUILD_PROFILE",
        &std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into()),
    );
    emit(
        "ZIPP_BUILD_OPT_LEVEL",
        &std::env::var("OPT_LEVEL").unwrap_or_else(|_| "unknown".into()),
    );
    // CARGO_CFG_FEATURE is the cfg-visible feature set of THIS crate.
    emit(
        "ZIPP_BUILD_FEATURES",
        &std::env::var("CARGO_CFG_FEATURE").unwrap_or_else(|_| String::new()),
    );
    let (rustflags_source, raw_rustflags) = raw_rustflags();
    let rustflags = raw_rustflags
        .iter()
        .map(|flag| sanitized_rustflag(flag))
        .collect::<Vec<_>>()
        .join(" ");
    emit("ZIPP_BUILD_RUSTFLAGS_SOURCE", rustflags_source);
    emit("ZIPP_BUILD_RUSTFLAGS", &rustflags);
    emit(
        "ZIPP_BUILD_PGO_PROFILE_SHA256",
        &verified_pgo_profile_sha256(&raw_rustflags),
    );
    emit(
        "ZIPP_BUILD_PGO_TRAINING_RECIPE_SHA256",
        &validated_sha256_env("ZIPP_PGO_TRAINING_RECIPE_SHA256"),
    );
    emit(
        "ZIPP_BUILD_PGO_BUILD_RECIPE_SHA256",
        &validated_sha256_env("ZIPP_PGO_BUILD_RECIPE_SHA256"),
    );
    emit(
        "ZIPP_BUILD_PGO_BUILD_CONTRACT",
        &validated_exact_env("ZIPP_PGO_BUILD_CONTRACT", PGO_BUILD_CONTRACT),
    );
    emit(
        "ZIPP_BUILD_PGO_BUILD_ENV_POLICY",
        &validated_exact_env("ZIPP_PGO_BUILD_ENV_POLICY", PGO_BUILD_ENV_POLICY),
    );
    emit(
        "ZIPP_BUILD_PGO_BUILD_ENV_SHA256",
        &validated_sha256_env("ZIPP_PGO_BUILD_ENV_SHA256"),
    );
    emit(
        "ZIPP_BUILD_PGO_SOURCE_SNAPSHOT_SHA256",
        &validated_sha256_env("ZIPP_PGO_SOURCE_SNAPSHOT_SHA256"),
    );
    emit(
        "ZIPP_BUILD_PGO_CARGO_IDENTITY",
        &validated_text_env("ZIPP_PGO_CARGO_IDENTITY", 2048),
    );
    let cargo_path = std::env::var("ZIPP_PGO_CARGO_PATH").unwrap_or_default();
    let cargo_sha256 = if cargo_path.is_empty() {
        String::new()
    } else {
        verified_tool_sha256("ZIPP_PGO_CARGO_SHA256", Path::new(&cargo_path))
    };
    emit("ZIPP_BUILD_PGO_CARGO_SHA256", &cargo_sha256);
    emit(
        "ZIPP_BUILD_PGO_RUSTC_SHA256",
        &verified_tool_sha256("ZIPP_PGO_RUSTC_SHA256", Path::new(&rustc)),
    );
    emit(
        "ZIPP_BUILD_PGO_LINKER_IDENTITY",
        &validated_text_env("ZIPP_PGO_LINKER_IDENTITY", 512),
    );
    let linker_path = std::env::var("ZIPP_PGO_LINKER_PATH").unwrap_or_default();
    let linker_sha256 = if linker_path.is_empty() {
        String::new()
    } else {
        verified_tool_sha256("ZIPP_PGO_LINKER_SHA256", Path::new(&linker_path))
    };
    emit("ZIPP_BUILD_PGO_LINKER_SHA256", &linker_sha256);
    emit(
        "ZIPP_BUILD_PGO_MSVC_CL_IDENTITY",
        &validated_text_env("ZIPP_PGO_MSVC_CL_IDENTITY", 512),
    );
    let msvc_cl_path = std::env::var("ZIPP_PGO_MSVC_CL_PATH").unwrap_or_default();
    let msvc_cl_sha256 = if msvc_cl_path.is_empty() {
        String::new()
    } else {
        verified_tool_sha256("ZIPP_PGO_MSVC_CL_SHA256", Path::new(&msvc_cl_path))
    };
    emit("ZIPP_BUILD_PGO_MSVC_CL_SHA256", &msvc_cl_sha256);
    emit(
        "ZIPP_BUILD_PGO_MSVC_LIB_IDENTITY",
        &validated_text_env("ZIPP_PGO_MSVC_LIB_IDENTITY", 512),
    );
    let msvc_lib_path = std::env::var("ZIPP_PGO_MSVC_LIB_PATH").unwrap_or_default();
    let msvc_lib_sha256 = if msvc_lib_path.is_empty() {
        String::new()
    } else {
        verified_tool_sha256("ZIPP_PGO_MSVC_LIB_SHA256", Path::new(&msvc_lib_path))
    };
    emit("ZIPP_BUILD_PGO_MSVC_LIB_SHA256", &msvc_lib_sha256);
}

fn command_identity(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|output| !output.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Return raw Git stdout, preserving empty output and NUL-delimited paths.
fn git_output<I, S>(repo_root: &Path, args: I) -> Option<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .current_dir(repo_root)
        // These read-only identity probes never need to refresh the index.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// Compute the dirty-tree identity. A successful clean probe deliberately
/// preserves the historical `(false, "")` result used by `zipp --version`.
pub(crate) fn dirty_source_identity(repo_root: &Path) -> (bool, String) {
    let Some(status) = git_output(
        repo_root,
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    ) else {
        // An unverifiable tree must never be labelled clean: the benchmark
        // harness treats `dirty=false` plus the expected commit as publishable
        // evidence. Fail closed when Git is unavailable, the directory is not
        // a repository, or the status probe otherwise fails.
        return (true, "unknown".into());
    };
    if status.is_empty() {
        return (false, String::new());
    }

    let digest = collect_dirty_source(repo_root).unwrap_or_else(|| "unknown".into());
    (true, digest)
}

fn collect_dirty_source(repo_root: &Path) -> Option<String> {
    let tracked_paths = nul_records(git_output(
        repo_root,
        [
            "--literal-pathspecs",
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--ignore-submodules=none",
            "HEAD",
            "--",
        ],
    )?)?;
    let mut tracked = Vec::with_capacity(tracked_paths.len());
    for path in tracked_paths {
        let path_text = std::str::from_utf8(&path).ok()?;
        let mut args = [
            "--literal-pathspecs",
            "diff",
            "--binary",
            "--full-index",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--ignore-submodules=none",
            "HEAD",
            "--",
        ]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
        args.push(OsString::from(path_text));
        let diff = git_output(repo_root, &args)?;
        tracked.push((path, diff));
    }

    let untracked = nul_records(git_output(
        repo_root,
        [
            "--literal-pathspecs",
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
        ],
    )?)?;
    hash_dirty_material(repo_root, tracked, untracked)
}

/// Hash already collected Git material. Kept separate so failure behavior and
/// framing can be regression-tested without racing a live repository scan.
pub(crate) fn hash_dirty_material(
    repo_root: &Path,
    mut tracked: Vec<(Vec<u8>, Vec<u8>)>,
    mut untracked: Vec<Vec<u8>>,
) -> Option<String> {
    tracked.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    untracked.sort_unstable();

    let mut digest = Sha256::new();
    update_frame(&mut digest, b"schema", b"zipp-dirty-source-v2");
    update_frame(
        &mut digest,
        b"tracked-count",
        &(tracked.len() as u64).to_be_bytes(),
    );
    for (path, diff) in tracked {
        update_frame(&mut digest, b"tracked-path", &path);
        update_frame(&mut digest, b"tracked-diff", &diff);
    }
    update_frame(
        &mut digest,
        b"untracked-count",
        &(untracked.len() as u64).to_be_bytes(),
    );
    for path in untracked {
        hash_untracked_entry(&mut digest, repo_root, &path)?;
    }
    Some(format!("{:x}", digest.finalize()))
}

fn nul_records(mut bytes: Vec<u8>) -> Option<Vec<Vec<u8>>> {
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    if bytes.pop() != Some(0) {
        return None;
    }
    let records = bytes
        .split(|byte| *byte == 0)
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    (!records.iter().any(Vec::is_empty)).then_some(records)
}

fn hash_untracked_entry(
    digest: &mut Sha256,
    repo_root: &Path,
    relative_bytes: &[u8],
) -> Option<()> {
    let relative = Path::new(std::str::from_utf8(relative_bytes).ok()?);
    if relative.is_absolute()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let path = repo_root.join(relative);
    let metadata = std::fs::symlink_metadata(&path).ok()?;

    update_frame(digest, b"untracked-path", relative_bytes);
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path).ok()?;
        let target = os_str_bytes(target.as_os_str())?;
        update_frame(digest, b"untracked-kind", b"symlink");
        update_frame(digest, b"untracked-content", &target);
        return Some(());
    }
    if !metadata.is_file() {
        // Never open devices, pipes, or other special files during a build.
        return None;
    }

    let mut file = std::fs::File::open(path).ok()?;
    let opened = file.metadata().ok()?;
    if !opened.is_file() {
        return None;
    }
    update_frame(digest, b"untracked-kind", b"file");
    update_frame_header(digest, b"untracked-content", opened.len());
    let mut read_total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        read_total = read_total.checked_add(read as u64)?;
        if read_total > opened.len() {
            return None;
        }
        digest.update(&buffer[..read]);
    }
    let final_metadata = file.metadata().ok()?;
    (read_total == opened.len() && final_metadata.len() == opened.len()).then_some(())
}

fn update_frame(digest: &mut Sha256, label: &[u8], bytes: &[u8]) {
    update_frame_header(digest, label, bytes.len() as u64);
    digest.update(bytes);
}

fn update_frame_header(digest: &mut Sha256, label: &[u8], content_len: u64) {
    digest.update((label.len() as u64).to_be_bytes());
    digest.update(label);
    digest.update(content_len.to_be_bytes());
}

#[cfg(unix)]
fn os_str_bytes(value: &OsStr) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Some(value.as_bytes().to_vec())
}

#[cfg(windows)]
fn os_str_bytes(value: &OsStr) -> Option<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;
    Some(
        value
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )
}

#[cfg(not(any(unix, windows)))]
fn os_str_bytes(value: &OsStr) -> Option<Vec<u8>> {
    Some(value.to_str()?.as_bytes().to_vec())
}

/// Cargo removes `RUSTFLAGS` from build-script environments and passes the
/// flags it actually gives rustc as unit-separator-delimited
/// `CARGO_ENCODED_RUSTFLAGS`. Prefer that authoritative value, retaining the
/// old variable only as a compatibility fallback for non-Cargo invocations.
fn raw_rustflags() -> (&'static str, Vec<String>) {
    if let Ok(encoded) = std::env::var("CARGO_ENCODED_RUSTFLAGS") {
        if !encoded.is_empty() {
            let flags = encoded
                .split('\x1f')
                .filter(|flag| !flag.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            return ("CARGO_ENCODED_RUSTFLAGS", flags);
        }
    }
    if let Ok(flags) = std::env::var("RUSTFLAGS") {
        if !flags.is_empty() {
            return (
                "RUSTFLAGS",
                flags.split_whitespace().map(str::to_owned).collect(),
            );
        }
    }
    ("none", Vec::new())
}

/// Keep build provenance useful without publishing workstation paths. PGO
/// paths are intentionally replaced wholesale: the adjacent profile hash names
/// the bytes that matter, while the absolute path says only who built them.
fn sanitized_rustflag(flag: &str) -> String {
    for prefix in [
        "-Cprofile-use=",
        "profile-use=",
        "-Cprofile-generate=",
        "profile-generate=",
    ] {
        if flag.starts_with(prefix) {
            return format!("{prefix}<redacted-path>");
        }
    }

    let mut sanitized = flag.to_string();
    if let Some(workspace) = std::env::var_os("CARGO_MANIFEST_DIR")
        .as_deref()
        .map(Path::new)
        .and_then(Path::parent)
        .and_then(Path::parent)
    {
        sanitized = replace_path_spellings(sanitized, workspace, "<workspace>");
    }
    for key in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(key).as_deref().map(Path::new) {
            sanitized = replace_path_spellings(sanitized, home, "<home>");
        }
    }
    sanitized
}

fn replace_path_spellings(mut value: String, path: &Path, replacement: &str) -> String {
    let native = path.to_string_lossy();
    if native.len() > 3 {
        value = value.replace(native.as_ref(), replacement);
        let forward = native.replace('\\', "/");
        if forward != native {
            value = value.replace(&forward, replacement);
        }
    }
    value
}

/// PGO hashes cross a shell/build-script boundary. Accept only the expected
/// digest syntax so a stray path, token, or malformed value cannot be embedded
/// in a published binary's identity output.
fn validated_sha256_env(key: &str) -> String {
    let Ok(value) = std::env::var(key) else {
        return String::new();
    };
    if value.is_empty() {
        return String::new();
    }
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return value.to_ascii_lowercase();
    }
    "invalid".into()
}

fn validated_exact_env(key: &str, expected: &str) -> String {
    match std::env::var(key) {
        Err(_) => String::new(),
        Ok(value) if value.is_empty() => String::new(),
        Ok(value) if value == expected => value,
        Ok(_) => "invalid".into(),
    }
}

fn validated_text_env(key: &str, max_len: usize) -> String {
    let Ok(value) = std::env::var(key) else {
        return String::new();
    };
    if value.is_empty() {
        return String::new();
    }
    if value.len() <= max_len && value.chars().all(|character| !character.is_control()) {
        value
    } else {
        "invalid".into()
    }
}

fn verified_tool_sha256(key: &str, path: &Path) -> String {
    let declared = validated_sha256_env(key);
    if declared.is_empty() || declared == "invalid" {
        return declared;
    }
    match sha256_file(path) {
        Some(actual) if actual == declared => actual,
        _ => "invalid".into(),
    }
}

/// Bind the embedded PGO profile identity to the bytes rustc will actually
/// consume. A stale or fabricated environment digest becomes `invalid`, which
/// the publication harness rejects.
fn verified_pgo_profile_sha256(rustflags: &[String]) -> String {
    let declared = validated_sha256_env("ZIPP_PGO_PROFILE_SHA256");
    let Ok(profile_path) = pgo_profile_path(rustflags) else {
        return "invalid".into();
    };
    let Some(profile_path) = profile_path else {
        return if declared.is_empty() {
            String::new()
        } else {
            "invalid".into()
        };
    };
    let rerun_path: String = profile_path
        .to_string_lossy()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    println!("cargo:rerun-if-changed={rerun_path}");
    let Some(actual) = sha256_file(&profile_path) else {
        return "invalid".into();
    };
    if declared == actual {
        actual
    } else {
        "invalid".into()
    }
}

/// Parse both accepted rustc codegen spellings: `-Cprofile-use=PATH` and
/// `-C profile-use=PATH`. Multiple profile inputs are ambiguous and rejected.
fn pgo_profile_path(rustflags: &[String]) -> Result<Option<PathBuf>, ()> {
    let mut found: Option<PathBuf> = None;
    let mut index = 0;
    while index < rustflags.len() {
        let flag = &rustflags[index];
        let mut consumed_next = false;
        let value = if let Some(value) = flag.strip_prefix("-Cprofile-use=") {
            Some(value)
        } else if flag == "-C" {
            let value = rustflags
                .get(index + 1)
                .and_then(|next| next.strip_prefix("profile-use="));
            consumed_next = value.is_some();
            value
        } else {
            None
        };
        if let Some(value) = value {
            if value.is_empty() || found.is_some() {
                return Err(());
            }
            found = Some(PathBuf::from(value));
        }
        if consumed_next {
            index += 1;
        }
        index += 1;
    }
    Ok(found)
}

fn sha256_file(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Some(format!("{:x}", digest.finalize()))
}

fn emit(key: &str, val: &str) {
    // Cargo treats each stdout line as a build-script directive. Values derived
    // from tools/environment must not be able to inject a second directive.
    let val: String = val
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    println!("cargo:rustc-env={key}={val}");
}
