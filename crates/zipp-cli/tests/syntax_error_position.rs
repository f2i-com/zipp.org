//! A script or module entry that does not parse is reported where it fails,
//! as a frame of the file under the name its stack frames use: the absolute
//! path for `zipp js`, the file URL (its `import.meta.url`) for `zipp mjs`.
//! Line and column are Node 24's for the same file (Node prints them as a
//! `file:line` header and a caret).
use std::{fs, path::PathBuf, process::Command};

struct Dir(PathBuf);
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
impl Dir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zipp-syntax-position-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        Self(std::path::absolute(path).unwrap())
    }
    /// `zipp <mode> name` on `text`: exit code and stderr.
    fn run(&self, mode: &str, name: &str, text: &str) -> (Option<i32>, String) {
        fs::write(self.0.join(name), text).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_zipp"))
            .arg(mode)
            .arg(self.0.join(name))
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"))
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_script_syntax_error_names_its_file_line_and_column() {
    let dir = Dir::new();
    let (code, err) = dir.run("js", "syn.js", "let a = 1;\nlet b = ;\n");
    assert_eq!(code, Some(1));
    let path = dir.0.join("syn.js");
    assert_eq!(
        err,
        format!("zipp: SyntaxError: unexpected token ';'\n    at {}:2:9\n", path.display())
    );
}

#[test]
fn a_class_declaration_without_a_name_does_not_run() {
    let dir = Dir::new();
    let (code, err) = dir.run("js", "cls.js", "class { }\nconsole.log('ran');\n");
    assert_eq!(code, Some(1));
    let path = dir.0.join("cls.js");
    assert_eq!(
        err,
        format!("zipp: SyntaxError: unexpected token '{{'\n    at {}:1:7\n", path.display())
    );
}

#[test]
fn a_module_syntax_error_names_its_file_url() {
    let dir = Dir::new();
    let (code, err) = dir.run("mjs", "syn.mjs", "export const a = 1;\nlet b = ;\n");
    assert_eq!(code, Some(1));
    let first = "zipp: SyntaxError: unexpected token ';'\n    at file://";
    assert!(err.starts_with(first) && err.ends_with("/syn.mjs:2:9\n"), "{err}");
}
