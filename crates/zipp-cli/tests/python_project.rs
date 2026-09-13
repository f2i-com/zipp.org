#![cfg(feature = "python")]
use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zipp-python-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, entry: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_zipp"))
            .current_dir(&self.0)
            .args(["py", entry])
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn writes_empty_files_deletes_and_renames() {
    let f = Fixture::new();
    fs::write(f.0.join("main.py"), "import os\nopen('empty.txt', 'w').close()\nopen('truncate.txt', 'w').close()\nos.remove('delete.txt')\nos.rename('rename.txt', 'renamed.txt')\n").unwrap();
    for name in ["truncate.txt", "delete.txt", "rename.txt"] {
        fs::write(f.0.join(name), "before").unwrap();
    }
    let output = f.run("main.py");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["empty.txt", "truncate.txt"] {
        assert_eq!(fs::read(f.0.join(name)).unwrap(), b"");
    }
    for name in ["delete.txt", "rename.txt"] {
        assert!(!f.0.join(name).exists());
    }
    assert_eq!(fs::read(f.0.join("renamed.txt")).unwrap(), b"before");
}

#[test]
fn all_python_extensions_keep_project_imports_and_data() {
    for entry in ["main.pyw", "MAIN.PY", "Main.PYW"] {
        let f = Fixture::new();
        fs::write(
            f.0.join(entry),
            "import helper\nprint(helper.value, open('data.txt').read())\n",
        )
        .unwrap();
        fs::write(f.0.join("helper.PY"), "value = 42\n").unwrap();
        fs::write(f.0.join("data.txt"), "project").unwrap();
        let output = f.run(entry);
        assert!(
            output.status.success(),
            "{entry}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42 project");
    }
}

#[test]
fn nested_entry_keeps_its_sibling_imports() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("example")).unwrap();
    fs::write(
        f.0.join("example/main.py"),
        "import helper\nprint(helper.value)\n",
    )
    .unwrap();
    fs::write(f.0.join("example/helper.py"), "value = 42\n").unwrap();
    let output = f.run("example/main.py");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}
