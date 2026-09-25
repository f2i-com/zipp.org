//! CLI exit status and host reporting, 15 September 2026 review.
//!
//! R237: a Python `SystemExit` sets the process status the way CPython does.
//! An integer code becomes the status (its low 8 bits — see `python_exit`),
//! with nothing printed; `argparse` usage errors therefore exit 2; any other
//! code is printed to stderr and the status is 1. The status used to be 1 in
//! every case, with an extra `zipp: SystemExit: N` line CPython never prints.
//!
//! R256: `zipp js`/`zipp mjs` print the Test262 host report
//! (`ZIPP_REPORT_UNHANDLED=1`) on stderr, before the CLI's own `zipp: <error>`
//! line, so tools/run_test262.py still reads a negative test's diagnostic as
//! the last `zipp: ` line. The report names only exceptions that ESCAPED A JOB
//! unhandled, never an ordinary `reject(value)`.
use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);
/// Windows' clock granularity is coarse enough that two tests starting in the
/// same tick produced the SAME directory, and the second `create_dir` failed
/// with "Cannot create a file when that file already exists" (os error 183).
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zipp-infra-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn zipp(&self, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_zipp"));
        cmd.current_dir(&self.0)
            .args(args)
            .env_remove("ZIPP_REPORT_UNHANDLED");
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
            String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(feature = "python")]
#[test]
fn python_system_exit_sets_the_process_status() {
    for (program, status, stdout, stderr) in [
        ("print('bye')\nimport sys\nsys.exit(3)\n", 3, "bye\n", ""),
        ("import sys\nsys.exit(0)\n", 0, "", ""),
        ("import sys\nsys.exit()\n", 0, "", ""),
        ("import sys\nsys.exit(None)\n", 0, "", ""),
        ("import sys\nsys.exit(True)\n", 1, "", ""),
        // The status is the code's low 8 bits, as a POSIX wait status is and as
        // `std::process::ExitCode` (a u8) can express. CPython agrees on POSIX;
        // on Windows it passes the full 32-bit value, so `sys.exit(-1)` is -1
        // there and `sys.exit(258)` is 258, not 255 and 2. Anything that reads
        // these codes portably already masks them.
        ("import sys\nsys.exit(-1)\n", 255, "", ""),
        ("import sys\nsys.exit(258)\n", 2, "", ""),
        // A code that does not fit 64 bits becomes -1 before the mask, as
        // CPython's `_Py_HandleSystemExit` does.
        ("import sys\nsys.exit(2 ** 70)\n", 255, "", ""),
        ("import sys\nsys.exit(3.5)\n", 1, "", "3.5\n"),
        ("raise SystemExit(7)\n", 7, "", ""),
        ("class Done(SystemExit):\n    pass\nraise Done(4)\n", 4, "", ""),
        ("import sys\nsys.exit('stopping: bad input')\n", 1, "", "stopping: bad input\n"),
        ("raise SystemExit(1, 2)\n", 1, "", "(1, 2)\n"),
        (
            "try:\n    raise SystemExit(5)\nexcept SystemExit as e:\n    print('caught', e.code)\n",
            0,
            "caught 5\n",
            "",
        ),
    ] {
        let f = Fixture::new();
        fs::write(f.0.join("main.py"), program).unwrap();
        let (code, out, err) = f.zipp(&["py", "main.py"], &[]);
        assert_eq!(code, Some(status), "{program}: stderr {err}");
        assert_eq!(out, stdout, "{program}");
        assert_eq!(err, stderr, "{program}");
    }
}

#[cfg(feature = "python")]
#[test]
fn argparse_usage_errors_exit_2() {
    let f = Fixture::new();
    fs::write(
        f.0.join("main.py"),
        "import argparse\n\
         parser = argparse.ArgumentParser(prog='main.py')\n\
         parser.add_argument('n', type=int)\n\
         print(parser.parse_args(['x']).n)\n",
    )
    .unwrap();
    let (code, out, err) = f.zipp(&["py", "main.py"], &[]);
    assert_eq!(code, Some(2), "stderr: {err}");
    assert_eq!(out, "");
    assert!(err.contains("main.py: error: argument n: invalid int value: 'x'"), "{err}");
    assert!(!err.contains("SystemExit"), "{err}");
}

#[cfg(feature = "python")]
#[test]
fn python_status_covers_stdin_and_writes_files_first() {
    let f = Fixture::new();
    let piped = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .current_dir(&f.0)
        .args(["--lang=python", "-"])
        .stdin(std::process::Stdio::piped())
        .output_with_stdin("import sys\nsys.exit(9)\n");
    assert_eq!(piped.status.code(), Some(9));
    assert!(piped.stderr.is_empty(), "{}", String::from_utf8_lossy(&piped.stderr));
    // A project's written files still reach the disk before the exit.
    fs::write(
        f.0.join("main.py"),
        "with open('out.txt', 'w') as out:\n    out.write('saved')\nimport sys\nsys.exit(6)\n",
    )
    .unwrap();
    let (code, _, err) = f.zipp(&["py", "main.py"], &[]);
    assert_eq!(code, Some(6), "{err}");
    assert_eq!(fs::read_to_string(f.0.join("out.txt")).unwrap(), "saved");
    // Any other uncaught exception prints CPython's traceback and exits 1.
    fs::write(f.0.join("main.py"), "raise ValueError('bad')\n").unwrap();
    let (code, _, err) = f.zipp(&["py", "main.py"], &[]);
    assert_eq!(code, Some(1));
    assert_eq!(
        err,
        "Traceback (most recent call last):\n  File \"main.py\", line 1, in <module>\n    raise ValueError('bad')\nValueError: bad\n"
    );
}

trait OutputWithStdin {
    fn output_with_stdin(&mut self, input: &str) -> std::process::Output;
}
impl OutputWithStdin for Command {
    fn output_with_stdin(&mut self, input: &str) -> std::process::Output {
        use std::io::Write;
        let mut child = self
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

#[test]
fn test262_report_precedes_the_error_line() {
    let f = Fixture::new();
    fs::write(
        f.0.join("harness.js"),
        "function Test262Error(message) { this.message = message || ''; }\n",
    )
    .unwrap();
    fs::write(
        f.0.join("lost.js"),
        "Promise.resolve().then(function () { throw new Test262Error('deferred'); });\n",
    )
    .unwrap();
    fs::write(
        f.0.join("negative.js"),
        "Promise.resolve().then(function () { throw new TypeError('left'); });\nnull.x;\n",
    )
    .unwrap();
    fs::write(
        f.0.join("module.mjs"),
        "await 0;\nPromise.resolve().then(function () { throw new Test262Error('in module'); });\n",
    )
    .unwrap();
    // The corpus shape: rejected on purpose, never handled, never a failure.
    fs::write(
        f.0.join("plain.js"),
        "Promise.all([Promise.reject(new Test262Error('deliberate'))]);\n",
    )
    .unwrap();
    let report = [("ZIPP_REPORT_UNHANDLED", "1")];
    let (code, out, err) = f.zipp(&["js", "--script-goal", "lost.js", "harness.js"], &report);
    assert_eq!((code, out.as_str()), (Some(0), ""));
    assert_eq!(err, "zipp: unhandled exception in promise job: Test262Error: deferred\n");
    let (code, _, err) = f.zipp(&["js", "--script-goal", "lost.js", "harness.js"], &[]);
    assert_eq!((code, err.as_str()), (Some(0), ""));
    let (code, _, err) = f.zipp(&["js", "--script-goal", "plain.js", "harness.js"], &report);
    assert_eq!((code, err.as_str()), (Some(0), ""));
    let (code, _, err) = f.zipp(&["js", "--script-goal", "negative.js"], &report);
    assert_eq!(code, Some(1));
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 2, "{err}");
    assert_eq!(lines[0], "zipp: unhandled exception in promise job: TypeError: left");
    assert!(lines[1].starts_with("zipp: TypeError: "), "{err}");
    let (code, _, err) = f.zipp(&["mjs", "module.mjs", "harness.js"], &report);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(err, "zipp: unhandled exception in promise job: Test262Error: in module\n");
}
