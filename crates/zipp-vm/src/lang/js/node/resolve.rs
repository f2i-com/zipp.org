//! Bare module specifiers (`zod`, `@scope/pkg/sub`), resolved as Node
//! resolves them: the nearest `node_modules/<name>` above the importing
//! file, then the package's `package.json`. Its `exports` decides when it
//! has one (subpaths, `*` patterns, conditions in order: `node`, then
//! `import` for an `import` or `require` for a `require`, then `default`;
//! `types` is never one); otherwise `main`, or `index`, with CommonJS's
//! extension and directory probing.
//!
//! The loader asks [`specifier_path`] for every specifier, so a relative or
//! absolute one is joined to the importing module's folder exactly as
//! before.

use crate::lang::ts::config::{parse_json, Value as Json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Is `spec` a package name (with an optional subpath) rather than a path,
/// a URL or a builtin (`node:fs`)?
pub(crate) fn is_bare(spec: &str) -> bool {
    let b = spec.as_bytes();
    !(spec.is_empty()
        || spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with(".\\")
        || spec.starts_with("..\\")
        || spec == "."
        || spec == ".."
        || b[0] == b'/'
        || b[0] == b'\\'
        || b[0] == b'#'
        || spec.contains(':'))
}

/// The path the specifier `spec` names from the importing module's folder
/// `dir`: a package's entry for a bare name, else `spec` joined to `dir`.
pub(crate) fn specifier_path(dir: Option<&Path>, spec: &str) -> PathBuf {
    // A `file:` URL names its path (`import(pathToFileURL(p).href)`).
    if let Some(path) = file_url_path(spec) {
        return path;
    }
    // A builtin (`fs`, `node:fs`) of a program run with Node's globals.
    if super::enabled() {
        if let Some(name) = builtin_name(spec) {
            return PathBuf::from(format!("node:{name}"));
        }
    }
    if is_bare(spec) {
        if let Some(p) = resolve_package(spec, dir.unwrap_or(Path::new(".")), false) {
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

/// The modules Node builds in, by the names `require` and `import` take.
pub(crate) const BUILTINS: &[&str] = &[
    "_stream_duplex",
    "_stream_passthrough",
    "_stream_readable",
    "_stream_transform",
    "_stream_writable",
    "assert",
    "assert/strict",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "dns/promises",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "path/posix",
    "path/win32",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "readline/promises",
    "repl",
    "stream",
    "stream/promises",
    "stream/web",
    "string_decoder",
    "sys",
    "timers",
    "timers/promises",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "util/types",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

/// The builtin `spec` names (`fs`, `node:fs`, `node:test`), without its
/// `node:` scheme.
pub(crate) fn builtin_name(spec: &str) -> Option<&str> {
    match spec.strip_prefix("node:") {
        Some(name) => Some(name),
        None => BUILTINS.contains(&spec).then_some(spec),
    }
}

/// `name` and `./subpath` of a bare specifier (`@a/b/c` → `@a/b`, `./c`).
fn split_package(spec: &str) -> Option<(&str, String)> {
    let mut parts = spec.splitn(if spec.starts_with('@') { 3 } else { 2 }, '/');
    let first = parts.next()?;
    let (name_len, rest) = if spec.starts_with('@') {
        let second = parts.next()?;
        (first.len() + 1 + second.len(), parts.next())
    } else {
        (first.len(), parts.next())
    };
    let name = &spec[..name_len];
    if name.is_empty() || name.ends_with('/') {
        return None;
    }
    Some((
        name,
        rest.map_or_else(|| ".".to_string(), |r| format!("./{r}")),
    ))
}

/// The file the package specifier `spec` resolves to from folder `from`,
/// for a `require` or an `import`.
pub(crate) fn resolve_package(spec: &str, from: &Path, require: bool) -> Option<PathBuf> {
    let (name, sub) = split_package(spec)?;
    let conditions: &[&str] = if require {
        &["node", "require", "module-sync", "default"]
    } else {
        &["node", "import", "module-sync", "default"]
    };
    let mut dir = std::path::absolute(from).ok()?;
    // A package may import itself by its own name through its `exports`.
    if let Some((root, pkg)) = nearest_package(&dir) {
        if pkg.name.as_deref() == Some(name) {
            if let Some(exports) = &pkg.exports {
                return resolve_exports(&root, &sub, exports, conditions);
            }
        }
    }
    loop {
        let candidate = dir.join("node_modules").join(name);
        if candidate.is_dir() {
            return resolve_in_package(&candidate, &sub, conditions);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// `sub` (`.` or `./x`) of the package in folder `pkg_dir`.
fn resolve_in_package(pkg_dir: &Path, sub: &str, conditions: &[&str]) -> Option<PathBuf> {
    let pkg = package_json(pkg_dir);
    if let Some(exports) = pkg.as_ref().and_then(|p| p.exports.as_ref()) {
        return resolve_exports(pkg_dir, sub, exports, conditions);
    }
    if sub == "." {
        if let Some(main) = pkg.as_ref().and_then(|p| p.main.as_deref()) {
            if let Some(p) = probe(&pkg_dir.join(main)) {
                return Some(p);
            }
        }
        return probe_index(pkg_dir);
    }
    probe(&pkg_dir.join(&sub[2..]))
}

/// CommonJS's file probing: the path, with `.js`, `.json`, `.cjs`, `.mjs`,
/// then as a folder (its `package.json`'s `main`, else `index`).
pub(crate) fn probe(p: &Path) -> Option<PathBuf> {
    if p.is_file() {
        return Some(p.to_path_buf());
    }
    for ext in ["js", "json", "cjs", "mjs"] {
        let mut s = p.as_os_str().to_os_string();
        s.push(".");
        s.push(ext);
        let q = PathBuf::from(s);
        if q.is_file() {
            return Some(q);
        }
    }
    if p.is_dir() {
        if let Some(main) = package_json(p).and_then(|pkg| pkg.main.clone()) {
            if let Some(q) = probe(&p.join(main)) {
                return Some(q);
            }
        }
        return probe_index(p);
    }
    None
}

fn probe_index(dir: &Path) -> Option<PathBuf> {
    ["index.js", "index.json", "index.cjs", "index.mjs"]
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
}

/// `PACKAGE_EXPORTS_RESOLVE`: `sub` through the package's `exports`.
fn resolve_exports(
    pkg_dir: &Path,
    sub: &str,
    exports: &Json,
    conditions: &[&str],
) -> Option<PathBuf> {
    let map = match exports {
        Json::Obj(m) if m.first().is_some_and(|(k, _)| k.starts_with('.')) => m,
        // A string, an array or a condition object: the package's `.` alone.
        _ => {
            return (sub == ".")
                .then(|| target(pkg_dir, exports, None, conditions))
                .flatten();
        }
    };
    if let Some((_, t)) = map.iter().rev().find(|(k, _)| k == sub && !k.contains('*')) {
        return target(pkg_dir, t, None, conditions);
    }
    // The pattern (`./features/*.js`) or legacy folder (`./lib/`) key with
    // the longest prefix that matches.
    let mut best: Option<(usize, &Json, String)> = None;
    for (k, t) in map {
        let found = if let Some(star) = k.find('*') {
            let (pre, post) = (&k[..star], &k[star + 1..]);
            (sub.len() >= pre.len() + post.len() && sub.starts_with(pre) && sub.ends_with(post))
                .then(|| {
                    (
                        pre.len(),
                        sub[pre.len()..sub.len() - post.len()].to_string(),
                    )
                })
        } else if k.ends_with('/') && sub.starts_with(k.as_str()) {
            Some((k.len(), sub[k.len()..].to_string()))
        } else {
            None
        };
        if let Some((len, m)) = found {
            if best.as_ref().is_none_or(|(l, _, _)| len > *l) {
                best = Some((len, t, m));
            }
        }
    }
    let (_, t, m) = best?;
    target(pkg_dir, t, Some(&m), conditions)
}

/// `PACKAGE_TARGET_RESOLVE`: an `exports` value, its `*` replaced by `star`.
fn target(pkg_dir: &Path, t: &Json, star: Option<&str>, conditions: &[&str]) -> Option<PathBuf> {
    match t {
        Json::Str(s) => {
            let rel = s.strip_prefix("./")?;
            let rel = match star {
                Some(m) if rel.contains('*') => rel.replace('*', m),
                Some(m) if rel.ends_with('/') => format!("{rel}{m}"),
                _ => rel.to_string(),
            };
            let p = pkg_dir.join(rel);
            p.is_file().then_some(p)
        }
        Json::Arr(items) => items
            .iter()
            .find_map(|i| target(pkg_dir, i, star, conditions)),
        Json::Obj(m) => {
            for (k, v) in m {
                if k == "default" || conditions.contains(&k.as_str()) {
                    if let Some(p) = target(pkg_dir, v, star, conditions) {
                        return Some(p);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// What the loader reads of a `package.json`.
pub(crate) struct Package {
    pub(crate) name: Option<String>,
    pub(crate) main: Option<String>,
    pub(crate) exports: Option<Json>,
    /// `"module"` or `"commonjs"` (the default).
    pub(crate) module_type: Option<String>,
}

/// The `package.json` in folder `dir`, read once per process.
pub(crate) fn package_json(dir: &Path) -> Option<Arc<Package>> {
    static CACHE: Mutex<Option<HashMap<PathBuf, Option<Arc<Package>>>>> = Mutex::new(None);
    if let Ok(mut c) = CACHE.lock() {
        if let Some(hit) = c.get_or_insert_with(HashMap::new).get(dir) {
            return hit.clone();
        }
    }
    let read = std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|text| parse_json(&text))
        .map(|v| {
            let s = |k: &str| v.get(k).and_then(Json::as_str).map(str::to_string);
            let (name, main, module_type) =
                (s("name"), s("main").filter(|m| !m.is_empty()), s("type"));
            let exports = match v {
                Json::Obj(m) => m
                    .into_iter()
                    .rev()
                    .find(|(k, _)| k == "exports")
                    .map(|(_, e)| e),
                _ => None,
            };
            Arc::new(Package {
                name,
                main,
                exports,
                module_type,
            })
        });
    if let Ok(mut c) = CACHE.lock() {
        c.get_or_insert_with(HashMap::new)
            .insert(dir.to_path_buf(), read.clone());
    }
    read
}

/// The nearest `package.json` at or above folder `dir`, and its folder.
pub(crate) fn nearest_package(dir: &Path) -> Option<(PathBuf, Arc<Package>)> {
    let mut d = dir.to_path_buf();
    loop {
        if let Some(p) = package_json(&d) {
            return Some((d, p));
        }
        if !d.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specifiers_split_into_package_and_subpath() {
        assert_eq!(split_package("zod"), Some(("zod", ".".to_string())));
        assert_eq!(
            split_package("zod/v4/core"),
            Some(("zod", "./v4/core".to_string()))
        );
        assert_eq!(split_package("@a/b"), Some(("@a/b", ".".to_string())));
        assert_eq!(
            split_package("@a/b/c/d"),
            Some(("@a/b", "./c/d".to_string()))
        );
        assert!(is_bare("zod") && is_bare("@a/b") && !is_bare("./x") && !is_bare("../x"));
        assert!(
            !is_bare("/abs") && !is_bare("C:\\x") && !is_bare("node:fs") && !is_bare("#internal")
        );
    }

    #[test]
    fn exports_resolve_subpaths_patterns_and_conditions() {
        let dir = std::env::temp_dir().join(format!("zipp-resolve-{}", std::process::id()));
        let pkg = dir.join("node_modules").join("p");
        std::fs::create_dir_all(pkg.join("dist/feat")).unwrap();
        for f in [
            "dist/index.mjs",
            "dist/index.cjs",
            "dist/feat/a.mjs",
            "dist/sub.js",
        ] {
            std::fs::write(pkg.join(f), "").unwrap();
        }
        std::fs::write(
            pkg.join("package.json"),
            r#"{ "name": "p", "exports": {
                ".": { "types": "./x.d.ts", "import": "./dist/index.mjs", "require": "./dist/index.cjs" },
                "./sub": "./dist/sub.js",
                "./feat/*": { "import": "./dist/feat/*.mjs" },
                "./package.json": "./package.json"
            } }"#,
        )
        .unwrap();
        let from = dir.join("src");
        std::fs::create_dir_all(&from).unwrap();
        let r = |spec: &str, req: bool| {
            resolve_package(spec, &from, req).map(|p| {
                p.strip_prefix(&pkg)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
        };
        assert_eq!(r("p", false).as_deref(), Some("dist/index.mjs"));
        assert_eq!(r("p", true).as_deref(), Some("dist/index.cjs"));
        assert_eq!(r("p/sub", false).as_deref(), Some("dist/sub.js"));
        assert_eq!(r("p/feat/a", false).as_deref(), Some("dist/feat/a.mjs"));
        assert_eq!(r("p/feat/a", true), None);
        assert_eq!(r("p/dist/sub.js", false), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
