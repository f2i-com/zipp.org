//! The path a module specifier names, for the ES module loader of every
//! build. A build with the `node` feature also resolves Node's builtins and
//! npm packages ([`super::packages`]).

#[cfg(feature = "node")]
use super::packages;
#[cfg(not(feature = "wasm-no-fs-loader"))]
use std::path::{Path, PathBuf};

/// The path the specifier `spec` names from the importing module's folder
/// `dir`: a package's entry for a bare name, else `spec` joined to `dir`.
#[cfg(not(feature = "wasm-no-fs-loader"))]
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
#[cfg(not(feature = "wasm-no-fs-loader"))]
pub(crate) fn file_url_path(spec: &str) -> Option<PathBuf> {
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

/// Result of resolving an ESM specifier for `import.meta.resolve`.
#[cfg(not(feature = "wasm-no-fs-loader"))]
pub(crate) enum MetaResolution {
    /// A literal URL string (e.g. `node:fs` or `data:...`).
    Url(String),
    /// A filesystem path (with optional `?query`/`#hash` suffix) to turn into a `file:` URL.
    Path(PathBuf, String),
}

/// Error kind for `import.meta.resolve`.
#[cfg(not(feature = "wasm-no-fs-loader"))]
pub(crate) enum MetaResolveError {
    NotFound(String),
    UnsupportedScheme(String),
}

/// Resolve `spec` relative to `base_dir` using the same ESM resolution rules as
/// static/dynamic `import`, for `import.meta.resolve(specifier[, parent])`.
#[cfg(not(feature = "wasm-no-fs-loader"))]
pub(crate) fn resolve_meta_specifier(
    base_dir: Option<&Path>,
    spec: &str,
    confined: bool,
) -> Result<MetaResolution, MetaResolveError> {
    if spec.is_empty() {
        return Err(MetaResolveError::NotFound(format!(
            "Cannot find module '{spec}'"
        )));
    }
    // 1. Node builtins (`node:fs`, `fs`, `path/posix`, etc.) when Node's
    //    runtime is active (or explicit `node:` scheme).
    #[cfg(feature = "node")]
    if super::enabled() {
        if let Some(name) = packages::builtin_name(spec) {
            return Ok(MetaResolution::Url(format!("node:{name}")));
        }
    }
    if spec.starts_with("data:") {
        return Ok(MetaResolution::Url(spec.to_string()));
    }
    // Split off `?query` / `#hash` (preserving `#...` subpath specifiers).
    let (spec_base, suffix) = if spec.starts_with('#') {
        (spec, "")
    } else if let Some(idx) = spec.find(['?', '#']) {
        (&spec[..idx], &spec[idx..])
    } else {
        (spec, "")
    };
    // 2. Explicit `file:` URL.
    if spec_base.starts_with("file://") {
        let Some(p) = file_url_path(spec_base) else {
            return Err(MetaResolveError::NotFound(format!(
                "Invalid file URL '{spec}'"
            )));
        };
        return Ok(MetaResolution::Path(p, suffix.to_string()));
    }
    // 3. Bare package specifier (`pkg`, `pkg/sub`, `@scope/pkg/sub`).
    #[cfg(feature = "node")]
    if packages::is_bare(spec_base) {
        if !confined {
            let from = base_dir.unwrap_or(Path::new("."));
            if let Some(p) = packages::resolve_package(spec_base, from, false) {
                return Ok(MetaResolution::Path(p, suffix.to_string()));
            }
        }
        return Err(MetaResolveError::NotFound(format!(
            "Cannot find package '{spec_base}'"
        )));
    }
    #[cfg(not(feature = "node"))]
    {
        let _ = confined;
        if !(spec_base.starts_with("./")
            || spec_base.starts_with("../")
            || spec_base.starts_with('/')
            || spec_base == "."
            || spec_base == "..")
        {
            return Err(MetaResolveError::NotFound(format!(
                "Cannot find package '{spec_base}'"
            )));
        }
    }
    // 4. Reject unknown URL schemes (e.g. `http:`, `ftp:`), allowing Windows drive paths.
    if let Some(colon) = spec_base.find(':') {
        let is_win_drive = colon == 1
            && spec_base.as_bytes()[0].is_ascii_alphabetic()
            && spec_base
                .as_bytes()
                .get(2)
                .is_some_and(|&b| b == b'/' || b == b'\\');
        if !is_win_drive
            && !spec_base.starts_with("./")
            && !spec_base.starts_with("../")
            && !spec_base.starts_with('/')
        {
            let scheme = &spec_base[..=colon];
            return Err(MetaResolveError::UnsupportedScheme(format!(
                "Only URLs with a scheme in: file, data, and node are supported by the default ESM loader. Received protocol '{scheme}'"
            )));
        }
    }
    // 5. Relative or root-relative path.
    let joined = match base_dir {
        Some(d) => {
            // On Windows, `/foo` relative to `E:\dir` resolves on drive `E:\foo`
            // (URL resolution semantics against `file:///E:/dir/...`).
            #[cfg(windows)]
            if spec_base.starts_with('/') && !spec_base.starts_with("//") {
                if let Some(std::path::Component::Prefix(prefix)) = d.components().next() {
                    let mut pb = PathBuf::from(prefix.as_os_str());
                    pb.push(spec_base);
                    pb
                } else {
                    d.join(spec_base)
                }
            } else {
                d.join(spec_base)
            }
            #[cfg(not(windows))]
            {
                d.join(spec_base)
            }
        }
        None => PathBuf::from(spec_base),
    };
    Ok(MetaResolution::Path(joined, suffix.to_string()))
}
