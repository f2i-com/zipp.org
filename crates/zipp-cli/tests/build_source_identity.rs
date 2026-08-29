use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "zipp-build-source-identity-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create scratch repository");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(name), bytes).expect("write scratch file");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self
            .0
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("zipp-build-source-identity-"))
            && self.0.starts_with(std::env::temp_dir())
        {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn initialized_repo() -> Scratch {
    let scratch = Scratch::new();
    git(scratch.path(), &["init", "--quiet"]);
    git(scratch.path(), &["config", "user.name", "Zipp Test"]);
    git(
        scratch.path(),
        &["config", "user.email", "zipp-test@example.invalid"],
    );
    git(scratch.path(), &["config", "commit.gpgsign", "false"]);
    scratch.write("tracked.txt", b"base\n");
    git(scratch.path(), &["add", "tracked.txt"]);
    git(scratch.path(), &["commit", "--quiet", "-m", "base"]);
    scratch
}

fn assert_sha256(value: &str) {
    assert_eq!(value.len(), 64, "{value}");
    assert!(value.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[test]
fn clean_identity_is_preserved_and_all_dirty_content_changes_the_sha256() {
    let scratch = initialized_repo();
    assert_eq!(
        build_script::dirty_source_identity(scratch.path()),
        (false, String::new())
    );

    scratch.write("tracked.txt", b"first tracked edit\n");
    let (dirty, tracked_digest) = build_script::dirty_source_identity(scratch.path());
    assert!(dirty);
    assert_sha256(&tracked_digest);

    scratch.write("untracked.txt", b"untracked secret one\n");
    let (_, first_untracked_digest) = build_script::dirty_source_identity(scratch.path());
    assert_sha256(&first_untracked_digest);
    assert_ne!(tracked_digest, first_untracked_digest);

    // The path and byte count stay unchanged: the content itself must be bound.
    scratch.write("untracked.txt", b"untracked secret two\n");
    let (_, second_untracked_digest) = build_script::dirty_source_identity(scratch.path());
    assert_sha256(&second_untracked_digest);
    assert_ne!(first_untracked_digest, second_untracked_digest);
    assert!(!second_untracked_digest.contains("secret"));
}

#[test]
fn framing_is_order_independent_and_unreadable_entries_fail_closed() {
    let scratch = Scratch::new();
    scratch.write("a", b"alpha");
    scratch.write("b", b"bravo");

    let forward = build_script::hash_dirty_material(
        scratch.path(),
        vec![
            (b"z.rs".to_vec(), b"z-diff".to_vec()),
            (b"a.rs".to_vec(), b"a-diff".to_vec()),
        ],
        vec![b"b".to_vec(), b"a".to_vec()],
    )
    .expect("hash readable entries");
    let reverse = build_script::hash_dirty_material(
        scratch.path(),
        vec![
            (b"a.rs".to_vec(), b"a-diff".to_vec()),
            (b"z.rs".to_vec(), b"z-diff".to_vec()),
        ],
        vec![b"a".to_vec(), b"b".to_vec()],
    )
    .expect("hash readable entries");
    assert_eq!(forward, reverse);

    assert_eq!(
        build_script::hash_dirty_material(scratch.path(), Vec::new(), vec![b"missing".to_vec()]),
        None
    );
}

#[test]
fn failed_status_probe_is_dirty_and_unknown() {
    let scratch = Scratch::new();
    assert_eq!(
        build_script::dirty_source_identity(scratch.path()),
        (true, "unknown".to_string())
    );
}

#[cfg(unix)]
#[test]
fn untracked_symlink_target_bytes_are_bound_without_following_the_target() {
    use std::os::unix::fs::symlink;

    let scratch = initialized_repo();
    symlink("first-target", scratch.path().join("link")).expect("create first symlink");
    let (_, first) = build_script::dirty_source_identity(scratch.path());
    assert_sha256(&first);

    std::fs::remove_file(scratch.path().join("link")).expect("remove first symlink");
    symlink("second-target", scratch.path().join("link")).expect("create second symlink");
    let (_, second) = build_script::dirty_source_identity(scratch.path());
    assert_sha256(&second);
    assert_ne!(first, second);
}
