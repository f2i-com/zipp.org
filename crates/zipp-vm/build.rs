// Writes comment-stripped copies of the Python frontend's embedded library
// and runtime sources to `$OUT_DIR/pysrc/<same relative path>`, which
// `src/frontend/python/mod.rs` embeds in place of the originals. The
// WebAssembly artifact carries this text verbatim, so comments are shipped
// bytes; see `build/minify.rs` for what is removed and why every line and
// column of code is preserved.
use std::path::{Path, PathBuf};

#[path = "build/minify.rs"]
mod minify;
#[path = "build/pyimports.rs"]
mod pyimports;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build/minify.rs");
    println!("cargo:rerun-if-changed=build/pyimports.rs");
    let root = Path::new("src/frontend/python");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("pysrc");
    for dir in ["lib", "runtime"] {
        // A directory path makes cargo rescan everything under it.
        println!("cargo:rerun-if-changed={}", root.join(dir).display());
        mirror(&root.join(dir), &out.join(dir));
    }
    bundled(&root.join("lib"), &out.parent().unwrap().join("bundled.rs"));
}

/// `$OUT_DIR/bundled.rs`: the bundled library table (`lib/modules.txt`),
/// each module with its source (the stripped copy) and its import
/// statements, which `mod.rs` includes as `BUNDLED_MODULES`.
fn bundled(lib: &Path, dst: &Path) {
    use pyimports::ImportStmt;
    use std::fmt::Write;
    let list = std::fs::read_to_string(lib.join("modules.txt")).expect("read lib/modules.txt");
    let quote = |s: &str| format!("{s:?}");
    let mut out = String::from("&[\n");
    for line in list.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [name, file, package] = fields[..] else {
            panic!("lib/modules.txt: expected `name file package`, got {line:?}");
        };
        let source = std::fs::read_to_string(lib.join(file)).unwrap_or_else(|e| panic!("lib/{file}: {e}"));
        // The import statements as one string: `;` between statements, a
        // statement `i|a.b,c` (`import a.b, c`) or `2|m|x,y` (`from ..m
        // import x, y`; `2||x` without a module). Identical strings (the
        // aliases sharing a file) are stored once.
        let mut imports = Vec::new();
        for stmt in pyimports::imports(&source) {
            imports.push(match stmt {
                ImportStmt::Plain(v) => format!("i|{}", v.join(",")),
                ImportStmt::From(level, module, v) => format!("{level}|{}|{}", module.unwrap_or_default(), v.join(",")),
            });
        }
        writeln!(
            out,
            "    Bundled {{ name: {}, source: pysrc!({}), imports: {}, #[cfg(test)] file: {}, #[cfg(test)] package: {} }},",
            quote(name),
            quote(&format!("lib/{file}")),
            quote(&imports.join(";")),
            quote(file),
            quote(package),
        )
        .unwrap();
    }
    out.push(']');
    write_if_changed(dst, &out);
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
