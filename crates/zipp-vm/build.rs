// Writes comment-stripped copies of the Python frontend's embedded library
// and runtime sources to `$OUT_DIR/pysrc/<same relative path>`, which
// `src/frontend/python/mod.rs` embeds in place of the originals. The
// WebAssembly artifact carries this text verbatim, so comments are shipped
// bytes; see `build/minify.rs` for what is removed and why every line and
// column of code is preserved.
use std::path::{Path, PathBuf};

#[path = "build/minify.rs"]
mod minify;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build/minify.rs");
    let root = Path::new("src/frontend/python");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("pysrc");
    for dir in ["lib", "runtime"] {
        // A directory path makes cargo rescan everything under it.
        println!("cargo:rerun-if-changed={}", root.join(dir).display());
        mirror(&root.join(dir), &out.join(dir));
    }
}

fn mirror(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create pysrc directory");
    let mut entries: Vec<_> = std::fs::read_dir(src)
        .unwrap_or_else(|e| panic!("read {}: {e}", src.display()))
        .map(|e| e.expect("directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap();
        if path.is_dir() {
            mirror(&path, &dst.join(name));
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => continue, // not UTF-8 text (e.g. a stray .pyc): never embedded
        };
        let stripped = match path.extension().and_then(|e| e.to_str()) {
            Some("py") => minify::strip_python(&text),
            Some("js") => minify::strip_javascript(&text),
            _ => continue,
        };
        assert_eq!(
            text.matches('\n').count(),
            stripped.matches('\n').count(),
            "stripping {} changed its line count",
            path.display()
        );
        write_if_changed(&dst.join(name), &stripped);
    }
}

/// Leave an unchanged file's mtime alone so dependents are not rebuilt.
fn write_if_changed(path: &Path, text: &str) {
    if std::fs::read_to_string(path).ok().as_deref() != Some(text) {
        std::fs::write(path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
}
