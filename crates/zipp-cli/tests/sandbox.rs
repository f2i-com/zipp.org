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

#[test]
fn dedicated_help_describes_the_boundary() {
    let output = sandbox(&["--help"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("usage: zipp sandbox"), "{stdout}");
    assert!(stdout.contains("classic script"), "{stdout}");
    assert!(stdout.contains("not an OS security sandbox"), "{stdout}");
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
        .set_len((16 << 20) + 1)
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
