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

use std::process::Command;

fn main() {
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
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=PROFILE");

    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());

    // Dirty state, and — more useful for a benchmark — a digest of the actual
    // diff. Two dirty builds of DIFFERENT edits get different identities, which
    // is exactly what the stale-artifact problem needed.
    //
    // The digest covers `git diff HEAD` (tracked content) PLUS the porcelain
    // status WITH untracked names, so adding an untracked source file also
    // changes the identity even though its content is not in the diff.
    let (dirty, diff_digest) = match git(&["status", "--porcelain"]) {
        Some(status) if !status.is_empty() => {
            let diff = git(&["diff", "HEAD"]).unwrap_or_default();
            let mut material = diff.into_bytes();
            material.extend_from_slice(status.as_bytes());
            (true, short_digest(&material))
        }
        Some(_) => (false, String::new()),
        None => (false, "unknown".into()),
    };

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc_version = Command::new(&rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());

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
    emit(
        "ZIPP_BUILD_RUSTFLAGS",
        &std::env::var("RUSTFLAGS").unwrap_or_else(|_| String::new()),
    );
}

fn emit(key: &str, val: &str) {
    // Cargo treats each stdout line as a build-script directive. Values derived
    // from tools/environment must not be able to inject a second directive.
    let val: String = val
        .chars()
        .map(|c| {
            if matches!(c, '\r' | '\n' | '\0') {
                ' '
            } else {
                c
            }
        })
        .collect();
    println!("cargo:rustc-env={key}={val}");
}

/// A short hex digest of `bytes`. FNV-1a 64 — this only has to distinguish one
/// working tree from another in a build stamp, so a cryptographic hash would be
/// a dependency for no benefit.
fn short_digest(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}
