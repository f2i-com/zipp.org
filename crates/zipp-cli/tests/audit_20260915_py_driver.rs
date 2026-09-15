//! `zipp py` project runs (15 September 2026 review, track P-driver).
//!
//! - A write-back never replaces a file the program could not see: one in a
//!   skipped folder (`dist`, `target`, dot-folders), over the size limits or
//!   unreadable. Such a change is refused and reported, the rest still apply,
//!   and the run fails (R231).
//! - A Python file run by name is the entry of its project whatever its name:
//!   an extensionless shebang script (R236), `my-script.py`, `2024_report.py`,
//!   a script under a non-identifier, dot or skipped folder (R238).
//! - The script's folder shadows a same-named module elsewhere in the tree,
//!   as `sys.path[0]` does, instead of aborting (R239).
//! - Every change is checked before anything is written, a refused change
//!   does not stop the others, and the program's own error and traceback
//!   stay the run's result (R240).
//! - An unreadable folder in the tree is noted and skipped, and a script run
//!   from the home folder is rooted at its own folder (R241).
//! - `--bc DIR` is refused rather than running the project (R242).
//! - On a case-insensitive disk two spellings of one name are one file: a
//!   case-only rename keeps the file (R243).
//! - Console output streams while the program runs, with the two streams in
//!   production order.
#![cfg(feature = "python")]
use std::io::{BufRead, BufReader};
use std::process::{Command, Output, Stdio};
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "zipp-py-driver-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, relative: &str, contents: impl AsRef<[u8]>) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.0.join(relative)).unwrap_or_else(|e| panic!("{relative}: {e}"))
    }
    fn zipp(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_zipp"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

#[test]
fn r231_files_the_program_never_loaded_are_not_overwritten() {
    let f = Fixture::new("r231");
    f.write("big.log", vec![b'L'; 8 * 1024 * 1024 + 1]);
    f.write("dist/report.txt", "keep-me\n");
    f.write(".cache/state.txt", "secret-cache\n");
    f.write("target/out.txt", "tgt\n");
    f.write(
        "main.py",
        "import os\n\
         with open('big.log', 'a') as fh: fh.write('appended\\n')\n\
         with open('dist/report.txt', 'a') as fh: fh.write('new line\\n')\n\
         for p in ['.cache/state.txt', 'target/out.txt']:\n\
         \x20   if not os.path.exists(p):\n\
         \x20       with open(p, 'w') as fh: fh.write('default\\n')\n\
         with open('dist/fresh.txt', 'w') as fh: fh.write('fresh\\n')\n\
         with open('ok.txt', 'w') as fh: fh.write('ok\\n')\n\
         print('done')\n",
    );
    let output = f.zipp(&["py", "main.py"]);
    let err = stderr(&output);
    assert!(!output.status.success(), "refused changes must fail the run");
    assert_eq!(stdout(&output), "done\n");
    for path in ["big.log", "dist/report.txt", ".cache/state.txt", "target/out.txt"] {
        assert!(
            err.contains(&format!("not written back: {path}:")),
            "{path} should be reported: {err}"
        );
    }
    assert!(err.contains("big.log"), "{err}");
    assert_eq!(fs::metadata(f.0.join("big.log")).unwrap().len(), 8 * 1024 * 1024 + 1);
    assert_eq!(f.read("dist/report.txt"), "keep-me\n");
    assert_eq!(f.read(".cache/state.txt"), "secret-cache\n");
    assert_eq!(f.read("target/out.txt"), "tgt\n");
    // New files, even in a skipped folder, and loaded files still go back.
    assert_eq!(f.read("dist/fresh.txt"), "fresh\n");
    assert_eq!(f.read("ok.txt"), "ok\n");
}

#[test]
fn r236_extensionless_python_scripts_are_project_entries() {
    let f = Fixture::new("r236");
    f.write("input.txt", "in\n");
    f.write(
        "tool",
        "#!/usr/bin/env python3\nimport os, sys\n\
         print('sees input:', os.path.exists('input.txt'), __name__, __file__, sys.argv)\n\
         with open('result.txt', 'w') as fh: fh.write('x')\n",
    );
    for command in [["py", "tool", "a"], ["run", "tool", "a"]] {
        let _ = fs::remove_file(f.0.join("result.txt"));
        let output = f.zipp(&command);
        assert!(output.status.success(), "{command:?}: {}", stderr(&output));
        assert_eq!(
            stdout(&output),
            "sees input: True __main__ tool ['tool', 'a']\n",
            "{command:?}"
        );
        assert_eq!(f.read("result.txt"), "x", "{command:?}");
    }
    // A program on standard input has no folder: its writes are reported as
    // discarded rather than silently dropped.
    let mut child = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .current_dir(&f.0)
        .args(["--lang=python", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"open('stdin.txt', 'w').write('x')\nprint('ran')\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "ran\n");
    assert!(
        stderr(&output).contains("discarded") && stderr(&output).contains("stdin.txt"),
        "{}",
        stderr(&output)
    );
    assert!(!f.0.join("stdin.txt").exists());
}

#[test]
fn r238_any_script_file_name_runs_from_the_project_root() {
    let f = Fixture::new("r238");
    let scripts = [
        "my-script.py",
        "2024_report.py",
        "my-scripts/run.py",
        ".github/scripts/check.py",
        "dist/run.py",
        "pkg/tool.py",
    ];
    for script in scripts {
        f.write(script, "import helper\nprint(__name__, __file__, helper.WHERE)\n");
        let folder = script.rsplit_once('/').map_or("", |(dir, _)| dir);
        let helper = if folder.is_empty() {
            "helper.py".to_owned()
        } else {
            format!("{folder}/helper.py")
        };
        f.write(&helper, format!("WHERE = {folder:?}\n"));
    }
    for script in scripts {
        let output = f.zipp(&["py", script]);
        let folder = script.rsplit_once('/').map_or("", |(dir, _)| dir);
        assert!(output.status.success(), "{script}: {}", stderr(&output));
        assert_eq!(
            stdout(&output),
            format!("__main__ {script} {folder}\n"),
            "{script}"
        );
    }
    // A missing script and a non-UTF-8 one say what is wrong.
    let output = f.zipp(&["py", "missing.py"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("missing.py"), "{}", stderr(&output));
    f.write("latin1.py", b"print('caf\xe9')\n");
    let output = f.zipp(&["py", "latin1.py"]);
    assert!(
        stderr(&output).contains("latin1.py: not UTF-8 text"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn r239_the_script_folder_shadows_a_same_named_module() {
    let f = Fixture::new("r239");
    f.write("main.py", "print('root main')\n");
    f.write("util.py", "W = 'root'\n");
    f.write(
        "sub/main.py",
        "import util, main_helper\nprint('util is', util.W, main_helper.W)\nprint(open('util.py').read().strip())\n",
    );
    f.write("sub/util.py", "W = 'sub'\n");
    f.write("sub/main_helper.py", "W = 'helper'\n");
    let output = f.zipp(&["py", "sub/main.py"]);
    assert!(output.status.success(), "{}", stderr(&output));
    // The shadowed root file stays readable through the filesystem.
    assert_eq!(stdout(&output), "util is sub helper\nW = 'root'\n");
}

#[test]
fn r240_refused_changes_do_not_stop_the_rest_or_hide_the_error() {
    let f = Fixture::new("r240");
    f.write("dist/report.txt", "keep-me\n");
    f.write(
        "main.py",
        "open('a.txt', 'w').close()\n\
         open('dist/report.txt', 'w').write('lost')\n\
         open('z.txt', 'w').close()\n\
         raise ValueError('the program error')\n",
    );
    let output = f.zipp(&["py", "main.py"]);
    let err = stderr(&output);
    assert!(!output.status.success());
    assert!(err.contains("ValueError: the program error"), "{err}");
    assert!(err.contains("Traceback"), "{err}");
    assert!(err.contains("not written back: dist/report.txt"), "{err}");
    assert!(f.0.join("a.txt").exists() && f.0.join("z.txt").exists());
    assert_eq!(f.read("dist/report.txt"), "keep-me\n");
}

/// `:` is stream syntax on Windows: the path is refused when the changes are
/// checked, and the files around it are still written.
#[cfg(windows)]
#[test]
fn r240_a_windows_stream_name_is_refused_without_a_partial_write() {
    let f = Fixture::new("r240w");
    f.write(
        "main.py",
        "open('a.txt', 'w').close()\nopen('run 12:30.log', 'w').close()\nopen('z.txt', 'w').close()\nraise ValueError('mine')\n",
    );
    let output = f.zipp(&["py", "main.py"]);
    let err = stderr(&output);
    assert!(!output.status.success());
    assert!(err.contains("ValueError: mine"), "{err}");
    assert!(err.contains("not written back: run 12:30.log"), "{err}");
    assert!(f.0.join("a.txt").exists() && f.0.join("z.txt").exists());
}

#[test]
fn r241_an_unreadable_folder_is_noted_and_skipped() {
    let f = Fixture::new("r241");
    f.write("app/main.py", "print('app ran')\n");
    f.write("locked/f.txt", "x");
    let locked = f.0.join("locked");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    }
    #[cfg(windows)]
    let user = std::env::var("USERNAME").unwrap();
    #[cfg(windows)]
    let denied = Command::new("icacls")
        .arg(&locked)
        .args(["/deny", &format!("{user}:(RX)")])
        .output()
        .is_ok_and(|o| o.status.success());
    let output = f.zipp(&["py", "app/main.py"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(windows)]
    if denied {
        let _ = Command::new("icacls")
            .arg(&locked)
            .args(["/remove:d", &user])
            .output();
    }
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "app ran\n");
    #[cfg(windows)]
    if denied {
        assert!(
            stderr(&output).contains("left out of the project: locked"),
            "{}",
            stderr(&output)
        );
    }
}

/// A script below the home folder is rooted at its own folder, so the run
/// does not read the whole home tree first.
#[test]
fn r241_a_script_run_from_home_is_rooted_at_its_folder() {
    let f = Fixture::new("r241h");
    f.write("home-data.txt", "home");
    f.write("sub/sub-data.txt", "sub");
    f.write(
        "sub/hello.py",
        "import os\nprint(os.path.exists('home-data.txt'), os.path.exists('sub-data.txt'))\n",
    );
    let home = fs::canonicalize(&f.0).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .current_dir(&f.0)
        .env(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, &home)
        .args(["py", "sub/hello.py"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "False True\n");
}

#[test]
fn r242_bytecode_inspection_of_a_folder_is_refused() {
    let f = Fixture::new("r242");
    f.write(
        "proj/main.py",
        "print('EXECUTED')\nopen('side_effect.txt', 'w').close()\n",
    );
    let output = f.zipp(&["py", "--bc", "proj"]);
    assert!(!output.status.success());
    assert!(!stdout(&output).contains("EXECUTED"));
    assert!(stderr(&output).contains("--bc takes a file"), "{}", stderr(&output));
    assert!(!f.0.join("proj/side_effect.txt").exists());
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn r243_spellings_of_one_name_are_one_file_on_a_case_insensitive_disk() {
    let f = Fixture::new("r243");
    f.write("Data.txt", "ORIGINAL");
    f.write(
        "main.py",
        "with open('data.txt', 'w') as fh: fh.write('lower')\nwith open('DATA.TXT', 'w') as fh: fh.write('upper')\n",
    );
    let output = f.zipp(&["py", "main.py"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let names: Vec<String> = fs::read_dir(&f.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.eq_ignore_ascii_case("data.txt"))
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert_eq!(f.read("Data.txt"), "upper");

    // A case-only rename is a write of the new spelling plus a deletion of
    // the old one, and both name the same file: it must survive.
    let f = Fixture::new("r243r");
    f.write("Data.txt", "IMPORTANT");
    f.write("main.py", "import os\nos.rename('Data.txt', 'data.txt')\n");
    let output = f.zipp(&["py", "main.py"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let names: Vec<String> = fs::read_dir(&f.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.eq_ignore_ascii_case("data.txt"))
        .collect();
    assert_eq!(names, ["data.txt"]);
    assert_eq!(f.read("data.txt"), "IMPORTANT");
}

/// The first line reaches the terminal while the program is still running.
#[test]
fn console_output_streams_while_the_program_runs() {
    let f = Fixture::new("stream");
    f.write(
        "main.py",
        "import time\nprint('start')\nt = time.time()\nwhile time.time() - t < 3:\n    pass\nprint('end')\n",
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .current_dir(&f.0)
        .args(["py", "main.py"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    assert_eq!(lines.next().unwrap().unwrap(), "start");
    assert!(
        child.try_wait().unwrap().is_none(),
        "the first line only arrived when the program exited"
    );
    assert_eq!(lines.next().unwrap().unwrap(), "end");
    assert!(child.wait().unwrap().success());
}

/// With both streams on one file, lines appear in the order written.
#[test]
fn stdout_and_stderr_keep_production_order() {
    let f = Fixture::new("order");
    f.write(
        "main.py",
        "import sys\nprint('one')\nprint('two', file=sys.stderr)\nprint('three')\nsys.stderr.write('four\\n')\nprint('five')\n",
    );
    let log = f.0.join("both.log");
    let file = fs::File::create(&log).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .current_dir(&f.0)
        .args(["py", "main.py"])
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file))
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        fs::read_to_string(&log).unwrap().replace("\r\n", "\n"),
        "one\ntwo\nthree\nfour\nfive\n"
    );
}
