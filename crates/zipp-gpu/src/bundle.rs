//! gpu-lab's ES modules as one classic script, from the sources themselves.
//!
//! The native engine's embedding API runs classic scripts, and its module
//! loader reads files from disk; the CLI must run anywhere, so the modules
//! are compiled into the binary (`include_str!` of crates/zipp-wasm/gpu-lab,
//! the single source of truth) and rewritten here the way
//! `gpu-lab/scripts/bundle_for_test.py` does for the browser harness: each
//! module becomes a function scope returning its exports, and each
//! `import {a, b} from './x.mjs';` reads them from the module it names.
//! gpu-lab only uses single-line named imports and `export`
//! `function`/`class`/`const` declarations; anything else is refused rather
//! than bundled wrong.

/// A module's path under gpu-lab and its source.
pub type Module = (&'static str, &'static str);

macro_rules! lab {
    ($path:literal) => {
        (
            $path,
            include_str!(concat!("../../zipp-wasm/gpu-lab/", $path)),
        )
    };
}

/// The runtime's modules in dependency order (the WASM and WebGL2 backends are
/// imported by runtime.mjs; they are never selected here, but must parse).
pub const RUNTIME: &[Module] = &[
    lab!("src/graph.mjs"),
    lab!("src/kernel-math.mjs"),
    lab!("src/quant.mjs"),
    lab!("src/session.mjs"),
    lab!("src/backends/cpu.mjs"),
    lab!("src/backends/wasm.mjs"),
    lab!("src/backends/webgpu.mjs"),
    lab!("src/backends/webgl2.mjs"),
    lab!("src/runtime.mjs"),
    lab!("src/zipp-adapter.mjs"),
];

/// gpu-lab's protocol case list and benchmark, the ones the browser smoke runs.
pub const CASES: &[Module] = &[lab!("tests/ml-cases.mjs"), lab!("tests/browser-cases.mjs")];

fn normalize(base: &str, relative: &str) -> Result<String, String> {
    let mut parts: Vec<&str> = base.split('/').collect();
    parts.pop();
    for piece in relative.split('/') {
        match piece {
            "." | "" => {}
            ".." => {
                parts
                    .pop()
                    .ok_or_else(|| format!("{base}: import {relative} leaves gpu-lab"))?;
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

fn identifier(text: &str) -> Option<&str> {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .unwrap_or(text.len());
    (end > 0).then(|| &text[..end])
}

/// One module as `__zgpuModules["path"] = (() => { ...; return {exports}; })();`.
fn module(path: &str, source: &str) -> Result<String, String> {
    let mut body = String::with_capacity(source.len() + 256);
    let mut exports: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("import ") {
            // import {a, b} from './x.mjs';
            let open = rest
                .find('{')
                .ok_or_else(|| format!("{path}: unsupported import: {line}"))?;
            let close = rest
                .find('}')
                .ok_or_else(|| format!("{path}: unsupported import: {line}"))?;
            let names = &rest[open + 1..close];
            let from = rest[close + 1..].trim();
            let from = from
                .strip_prefix("from")
                .map(str::trim)
                .and_then(|s| s.strip_suffix(';'))
                .map(|s| s.trim().trim_matches(|c| c == '\'' || c == '"'))
                .ok_or_else(|| format!("{path}: unsupported import: {line}"))?;
            if names.contains(" as ") {
                return Err(format!("{path}: renamed imports are not bundled: {line}"));
            }
            let target = normalize(path, from)?;
            body.push_str(&format!("const {{{names}}} = __zgpuModules[{target:?}];\n"));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("export ") {
            let declaration = rest.trim_start();
            let named = declaration
                .strip_prefix("async function ")
                .or_else(|| declaration.strip_prefix("function "))
                .or_else(|| declaration.strip_prefix("class "))
                .or_else(|| declaration.strip_prefix("const "));
            let name = named
                .and_then(|n| identifier(n.trim_start_matches('*').trim_start()))
                .ok_or_else(|| format!("{path}: unsupported export: {line}"))?;
            exports.push(name.to_owned());
            let indent = &line[..line.len() - trimmed.len()];
            body.push_str(indent);
            body.push_str(declaration);
            body.push('\n');
            continue;
        }
        if trimmed.contains("import(") || trimmed.starts_with("export{") {
            return Err(format!("{path}: unsupported module syntax: {line}"));
        }
        // A classic script has no import.meta; the one use is a default the
        // CLI never evaluates (the WASM backend's kernel URL).
        body.push_str(&line.replace("import.meta.url", "\"zipp-gpu:///gpu-lab/\""));
        body.push('\n');
    }
    Ok(format!(
        "__zgpuModules[{path:?}] = (() => {{\n{body}return {{{}}};\n}})();\n",
        exports.join(", ")
    ))
}

/// The modules as one script defining `__zgpuModules` (a top-level `var`).
pub fn bundle(modules: &[&[Module]]) -> Result<String, String> {
    bundle_with(modules, &[])
}

/// [`bundle`], with each `after` script run right after the module it names
/// (before any later module binds its imports from that one).
pub fn bundle_with(modules: &[&[Module]], after: &[(&str, &str)]) -> Result<String, String> {
    let mut out = String::from("var __zgpuModules = Object.create(null);\n");
    for list in modules {
        for (path, source) in list.iter() {
            out.push_str(&module(path, source)?);
            for (_, script) in after.iter().filter(|(p, _)| p == path) {
                out.push_str(script);
                out.push('\n');
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rewrites_imports_and_exports() {
        let text = module(
            "src/backends/x.mjs",
            "import {check, ComputeError} from '../graph.mjs';\nexport class A {}\nexport async function f() {}\nexport const K = 1;\nconst u = import.meta.url;\n",
        )
        .unwrap();
        assert!(text.contains("const {check, ComputeError} = __zgpuModules[\"src/graph.mjs\"];"));
        assert!(text.contains("return {A, f, K};"));
        assert!(!text.contains("export "));
        assert!(!text.contains("import.meta"));
    }
    #[test]
    fn the_real_sources_bundle() {
        let script = bundle(&[RUNTIME, CASES]).unwrap();
        assert!(script.contains("__zgpuModules[\"src/runtime.mjs\"]"));
        assert!(script.contains("return {createRuntime, ComputeRuntime};"));
    }
    #[test]
    fn refuses_what_it_cannot_bundle() {
        assert!(module("a.mjs", "import * as x from './b.mjs';\n").is_err());
        assert!(module("a.mjs", "export default 1;\n").is_err());
        assert!(module("a.mjs", "import {a as b} from './b.mjs';\n").is_err());
    }
}
