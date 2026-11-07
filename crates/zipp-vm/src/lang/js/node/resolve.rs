//! The path a module specifier names, for the ES module loader of every
//! build. A build with the `node` feature also resolves Node's builtins and
//! npm packages ([`super::packages`]).

#[cfg(feature = "node")]
use super::packages;
use std::path::{Path, PathBuf};

/// The path the specifier `spec` names from the importing module's folder
/// `dir`: a package's entry for a bare name, else `spec` joined to `dir`.
pub(crate) fn specifier_path(dir: Option<&Path>, spec: &str) -> PathBuf {
    // A `file:` URL names its path (`import(pathToFileURL(p).href)`).
    if let Some(path) = file_url_path(spec) {
        return path;
    }
    // A builtin (`fs`, `node:fs`) of a program run with Node's globals.
    #[cfg(feature = "node")]
    if super::enabled() {
        if let Some(name) = packages::builtin_name(spec) {
            return PathBuf::from(format!("node:{name}"));
        }
    }
    #[cfg(feature = "node")]
    if packages::is_bare(spec) {
        if let Some(p) = packages::resolve_package(spec, dir.unwrap_or(Path::new(".")), false) {
            return p;
        }
    }
    match dir {
        Some(d) => d.join(spec),
        None => PathBuf::from(spec),
    }
}

/// The path of a `file:` URL (`file:///C:/a%20b/x.js` is `C:/a b/x.js`).
fn file_url_path(spec: &str) -> Option<PathBuf> {
    let rest = spec.strip_prefix("file://")?;
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let mut bytes = Vec::with_capacity(rest.len());
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                bytes.push(v);
                i += 3;
                continue;
            }
        }
        bytes.push(b[i]);
        i += 1;
    }
    let path = String::from_utf8(bytes).ok()?;
    // `/C:/x` on Windows is `C:/x`.
    let p = path.as_bytes();
    let path = if p.len() >= 3 && p[0] == b'/' && p[1].is_ascii_alphabetic() && p[2] == b':' {
        path[1..].to_string()
    } else {
        path
    };
    Some(PathBuf::from(path))
}
