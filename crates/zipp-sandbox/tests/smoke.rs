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
            "zipp-native-sandbox-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create scratch directory");
        Self(path)
    }

    fn write(&self, name: &str, source: &str) -> PathBuf {
        let path = self.0.join(name);
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
    Command::new(env!("CARGO_BIN_EXE_zipp-sandbox"))
        .args(args)
        .output()
        .expect("run zipp-sandbox")
}

fn path_arg(path: &Path) -> &str {
    path.to_str().expect("test path is UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_identifies_the_hardened_binary() {
    let output = sandbox(&["--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("usage: zipp-sandbox [options] <file.js>"),
        "{stdout}"
    );
    assert!(stdout.contains("excludes both JITs"), "{stdout}");
}

#[test]
fn runs_a_classic_script_in_the_supervised_child() {
    let scratch = Scratch::new("basic");
    let script = scratch.write("hello.js", "console.log('sandbox-ok');");
    let output = sandbox(&[
        "--timeout-ms",
        "2000",
        "--max-steps",
        "100000",
        path_arg(&script),
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "sandbox-ok\n");
}

#[test]
fn instruction_limit_stops_an_infinite_loop() {
    let scratch = Scratch::new("steps");
    let script = scratch.write("loop.js", "for (;;) {}");
    let output = sandbox(&[
        "--timeout-ms",
        "2000",
        "--max-steps",
        "5000",
        path_arg(&script),
    ]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("instruction budget"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn wall_clock_timeout_kills_the_supervised_child() {
    let scratch = Scratch::new("timeout");
    let script = scratch.write("loop.js", "for (;;) {}");
    let output = sandbox(&[
        "--timeout-ms",
        "1",
        "--max-steps",
        "100000000",
        path_arg(&script),
    ]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("wall-clock timeout"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn output_limit_kills_a_chatty_child() {
    let scratch = Scratch::new("output");
    let script = scratch.write(
        "chatty.js",
        "for (let i = 0; i < 10000; i++) console.log('xxxxxxxxxxxxxxxx');",
    );
    let output = sandbox(&[
        "--timeout-ms",
        "2000",
        "--max-steps",
        "100000000",
        "--max-output-bytes",
        "64",
        path_arg(&script),
    ]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("output limit"),
        "{}",
        stderr(&output)
    );
}

#[cfg(windows)]
#[test]
fn windows_network_and_device_paths_are_rejected_before_io() {
    let network = sandbox(&[r"\\server\share\entry.js"]);
    assert!(!network.status.success());
    assert!(
        stderr(&network).contains("UNC/network"),
        "{}",
        stderr(&network)
    );

    let device = sandbox(&[r"\\.\NUL"]);
    assert!(!device.status.success());
    assert!(
        stderr(&device).contains("device namespace"),
        "{}",
        stderr(&device)
    );
}
