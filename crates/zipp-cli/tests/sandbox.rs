use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "zipp-sandbox-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create script directory");
        }
        std::fs::write(&path, source).expect("write script");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.0.starts_with(std::env::temp_dir()) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn sandbox(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zipp"))
        .arg("sandbox")
        .args(args)
        .output()
        .expect("run sandbox")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[test]
fn dedicated_help_describes_the_boundary() {
    let output = sandbox(&["--help"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("usage: zipp sandbox"), "{stdout}");
    assert!(stdout.contains("classic script"), "{stdout}");
    assert!(
        stdout.contains("not an OS or memory-safety sandbox"),
        "{stdout}"
    );
    assert!(stdout.contains("zipp-wasm"), "{stdout}");
}

#[test]
fn module_entry_spellings_fail_closed() {
    let output = sandbox(&["--module", "entry.mjs"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("classic scripts only"),
        "{}",
        stderr(&output)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .args(["mjs", "--sandbox", "entry.mjs"])
        .output()
        .expect("run module rejection");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("classic scripts only"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn runs_a_script_in_the_supervised_child() {
    let scratch = Scratch::new("success");
    let script = scratch.write("hello.js", "console.log('sandbox-ok');");
    let output = sandbox(&[
        "--timeout-ms",
        "2000",
        "--max-steps",
        "100000",
        script.to_str().expect("utf-8 test path"),
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "sandbox-ok\n");
}

#[test]
fn instruction_budget_stops_an_infinite_loop() {
    let scratch = Scratch::new("steps");
    let script = scratch.write("loop.js", "for (;;) {}");
    let output = sandbox(&[
        "--timeout-ms",
        "2000",
        "--max-steps",
        "5000",
        script.to_str().expect("utf-8 test path"),
    ]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("instruction budget"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn supervisor_enforces_the_wall_clock_deadline() {
    let scratch = Scratch::new("supervisor-timeout");
    let script = scratch.write("loop.js", "for (;;) {}");
    let output = sandbox(&[
        "--timeout-ms",
        "1",
        "--max-steps",
        "100000000",
        script.to_str().expect("utf-8 test path"),
    ]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("wall-clock timeout"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn directly_invoked_worker_has_its_own_deadline_watchdog() {
    let scratch = Scratch::new("worker-watchdog");
    // Burn wall-clock while retiring almost no metered steps: each iteration
    // is one call step plus a ~4 MiB native scan (~ hundreds of microseconds),
    // so the step budget cannot trip inside the watchdog's grace window no
    // matter how fast the VM's loop tiers get. A bare `for (;;) {}` stopped
    // being a valid fixture once tiered loops could retire the 100M-step cap
    // in under `timeout_ms + 250`.
    let script = scratch.write(
        "loop.js",
        "const s = 'x'.repeat(1 << 22); for (;;) s.indexOf('y');",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .args([
            "__sandbox-child",
            "--timeout-ms",
            "1",
            "--max-steps",
            "100000000",
            "--max-heap-mb",
            // Keep this probe focused on the independently armed watchdog.
            // The hardened VM's audited baseline now includes side-table
            // capacities and can legitimately exceed the old 1 MiB fixture.
            "128",
            "--max-output-bytes",
            "1024",
            "--",
            script.to_str().expect("utf-8 test path"),
        ])
        .output()
        .expect("run hidden sandbox worker");

    assert_eq!(output.status.code(), Some(124), "{}", stderr(&output));
}

#[test]
fn blocking_native_wait_is_disabled_before_guest_execution() {
    let scratch = Scratch::new("wall-clock");
    let script = scratch.write(
        "blocking.js",
        "const word = new Int32Array(new SharedArrayBuffer(4)); Atomics.wait(word, 0, 0, 60000); console.log('escaped-wait');",
    );
    let output = sandbox(&[
        "--timeout-ms",
        "3000",
        "--max-steps",
        "50000000",
        script.to_str().expect("utf-8 test path"),
    ]);

    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("cannot suspend in this agent"), "{error}");
    assert!(!error.contains("wall-clock timeout"), "{error}");
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("escaped-wait"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn heap_budget_rejects_large_single_allocations_before_allocation() {
    for (label, source) in [
        (
            "array-buffer",
            "new ArrayBuffer(32 * 1024 * 1024); console.log('escaped-buffer');",
        ),
        (
            "string-repeat",
            "'x'.repeat(32 * 1024 * 1024); console.log('escaped-string');",
        ),
    ] {
        let scratch = Scratch::new(label);
        let script = scratch.write("allocation.js", source);
        let output = sandbox(&[
            "--timeout-ms",
            "5000",
            "--max-steps",
            "5000000",
            "--max-heap-mb",
            "1",
            script.to_str().expect("utf-8 test path"),
        ]);

        assert!(!output.status.success(), "{label} unexpectedly succeeded");
        let error = stderr(&output);
        assert!(error.contains("memory budget"), "{label}: {error}");
        assert!(!error.contains("wall-clock timeout"), "{label}: {error}");
        assert!(output.stdout.is_empty(), "{label}: unexpected guest output");
    }
}

#[test]
fn heap_budget_stops_a_retained_allocation_loop() {
    let scratch = Scratch::new("heap");
    let script = scratch.write(
        "heap.js",
        "const keep = []; for (;;) keep.push({ payload: keep.length }); console.log('escaped-heap');",
    );
    let output = sandbox(&[
        "--timeout-ms",
        "10000",
        "--max-steps",
        "50000000",
        "--max-heap-mb",
        "1",
        script.to_str().expect("utf-8 test path"),
    ]);

    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("memory budget"), "{error}");
    assert!(!error.contains("wall-clock timeout"), "{error}");
    assert!(!error.contains("instruction budget"), "{error}");
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("escaped-heap"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn output_limit_is_fail_closed_even_when_the_child_exits_quickly() {
    let scratch = Scratch::new("output");
    let script = scratch.write(
        "output.js",
        "for (let i = 0; i < 1000; i++) console.log('xxxxxxxxxxxxxxxx');",
    );
    let output = sandbox(&[
        "--timeout-ms",
        "3000",
        "--max-output-bytes",
        "128",
        script.to_str().expect("utf-8 test path"),
    ]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("output limit"),
        "{}",
        stderr(&output)
    );
    assert!(output.stdout.len() <= 128);
}

#[test]
fn dynamic_code_limit_is_terminal_even_when_guest_code_catches_it() {
    let scratch = Scratch::new("dynamic-code");
    let script = scratch.write(
        "dynamic.js",
        "try { eval(' '.repeat(65537) + '1'); console.log('unreachable'); } catch (_) { console.log('caught'); } console.log('also-unreachable');",
    );
    let output = sandbox(&[
        "--timeout-ms",
        "3000",
        script.to_str().expect("utf-8 test path"),
    ]);

    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("dynamic code source"), "{error}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("caught"), "{stdout}");
    assert!(!stdout.contains("unreachable"), "{stdout}");
}

#[test]
fn terminal_controls_are_sanitized_before_forwarding() {
    let scratch = Scratch::new("terminal-controls");
    let script = scratch.write(
        "controls.js",
        "console.log('\\x1b]0;PWNED\\x07ok\\t✓\\rX\\u202eSPOOF\\u2069'); console.error('\\x1b[31mBAD\\x1b[0m');",
    );
    let output = sandbox(&[script.to_str().expect("utf-8 test path")]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8(output.stdout).expect("sanitized stdout is UTF-8"),
        "?]0;PWNED?ok\t✓?X?SPOOF?\n"
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("sanitized stderr is UTF-8"),
        "?[31mBAD?[0m\n"
    );
}

#[test]
fn pre_child_diagnostics_are_single_line_and_control_free() {
    let hostile = "--bad\x1b]0;PWNED\x07\nforged\t\u{202e}spoof\u{2069}";
    let output = sandbox(&[hostile, "unused"]);

    assert!(!output.status.success());
    let error = stderr(&output);
    let line = error.strip_suffix('\n').unwrap_or(&error);
    assert_eq!(line.lines().count(), 1, "{error:?}");
    assert!(!line.chars().any(char::is_control), "{error:?}");
    for bidi in ['\u{061c}', '\u{200e}', '\u{200f}', '\u{202e}', '\u{2069}'] {
        assert!(!line.contains(bidi), "{error:?}");
    }
    assert!(!line.contains('\x1b'), "{error:?}");
    assert!(!line.contains('\x07'), "{error:?}");
}

#[test]
fn imports_are_opt_in_and_cannot_escape_the_canonical_root() {
    let scratch = Scratch::new("imports");
    let root = scratch.path().join("allowed");
    std::fs::create_dir(&root).expect("create import root");
    let secret = scratch.write("secret.mjs", "export const secret = 'DO-NOT-PRINT';");
    let absolute = secret
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let entry = scratch.write(
        "allowed/entry.js",
        &format!(
            "let errors = []; function report(e) {{ errors.push(String(e)); if (errors.length === 3) {{ console.log(errors[0]); console.log(errors[1]); console.log(errors[2]); }} }} import('../secret.mjs').catch(report); import('../missing.mjs').catch(report); import(\"{absolute}\").catch(report);"
        ),
    );
    let output = sandbox(&[
        "--timeout-ms",
        "2000",
        "--allow-imports",
        root.to_str().expect("utf-8 test path"),
        entry.to_str().expect("utf-8 test path"),
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.matches("TypeError: module not found").count(),
        3,
        "{stdout}"
    );
    assert!(!stdout.contains("DO-NOT-PRINT"), "{stdout}");
}

#[cfg(any(unix, windows))]
#[test]
fn import_symlinks_cannot_escape_the_canonical_root() {
    let scratch = Scratch::new("import-symlink");
    let root = scratch.path().join("allowed");
    std::fs::create_dir(&root).expect("create import root");
    let secret = scratch.write("secret.mjs", "export const secret = 'DO-NOT-PRINT';");
    let link = root.join("escape.mjs");
    if let Err(error) = create_file_symlink(&secret, &link) {
        #[cfg(windows)]
        if error.kind() == std::io::ErrorKind::PermissionDenied
            || error.kind() == std::io::ErrorKind::Unsupported
            || error.raw_os_error() == Some(1314)
        {
            eprintln!("skipping symlink escape test: {error}");
            return;
        }
        panic!("create module symlink: {error}");
    }
    let entry = scratch.write(
        "allowed/entry.js",
        "import('./escape.mjs').then(m => console.log(m.secret)).catch(e => console.log(String(e)));",
    );
    let output = sandbox(&[
        "--timeout-ms",
        "3000",
        "--allow-imports",
        root.to_str().expect("utf-8 test path"),
        entry.to_str().expect("utf-8 test path"),
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "TypeError: module not found\n");
    assert!(!stdout.contains("DO-NOT-PRINT"), "{stdout}");
}

#[test]
fn relative_imports_remain_script_relative_with_trusted_child_cwd() {
    let scratch = Scratch::new("relative-import-cwd");
    let root = scratch.path().join("allowed");
    std::fs::create_dir(&root).expect("create import root");
    scratch.write(
        "allowed/nested/value.mjs",
        "export const value = 'script-relative-import-ok';",
    );
    let entry = scratch.write(
        "allowed/nested/entry.js",
        "import('./value.mjs').then(m => console.log(m.value)).catch(e => console.error(String(e)));",
    );
    let output = sandbox(&[
        "--timeout-ms",
        "2000",
        "--allow-imports",
        root.to_str().expect("utf-8 test path"),
        entry.to_str().expect("utf-8 test path"),
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "script-relative-import-ok\n"
    );
}

#[test]
fn oversized_import_is_rejected_before_its_contents_are_loaded() {
    let scratch = Scratch::new("large-import");
    let root = scratch.path().join("allowed");
    std::fs::create_dir(&root).expect("create import root");
    let entry = scratch.write(
        "allowed/entry.js",
        "import('./large.mjs').catch(e => console.log(String(e)));",
    );
    let large = scratch.write("allowed/large.mjs", "console.log('DO-NOT-PRINT');");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&large)
        .expect("open oversized import")
        .set_len((2 << 20) + 1)
        .expect("extend oversized import");
    let output = sandbox(&[
        "--timeout-ms",
        "3000",
        "--allow-imports",
        root.to_str().expect("utf-8 test path"),
        entry.to_str().expect("utf-8 test path"),
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sandbox size limit"), "{stdout}");
    assert!(!stdout.contains("DO-NOT-PRINT"), "{stdout}");
}

#[test]
fn oversized_entry_source_is_rejected_before_the_child_starts() {
    let scratch = Scratch::new("large-entry");
    let script = scratch.write("large.js", "console.log('DO-NOT-PRINT');");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&script)
        .expect("open oversized entry")
        .set_len((16 << 20) + 1)
        .expect("extend oversized entry");
    let output = sandbox(&[
        "--timeout-ms",
        "3000",
        script.to_str().expect("utf-8 test path"),
    ]);

    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("limit is 2097152"), "{error}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("DO-NOT-PRINT"));
}
