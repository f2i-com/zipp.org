//! npm packages and CommonJS for a program run with Node's globals
//! (`lang/js/node`).
//!
//! A project is written to a temporary folder: a CommonJS package whose
//! `main` names a file without its extension and which requires a relative
//! file and JSON, a dual package (`"type": "module"`, `exports` with
//! `types`, `import` and `require` conditions, a subpath), and two CommonJS
//! files that require each other. A CommonJS main script `require`s them
//! (getting the dual package's `require` build, the cycle's partial
//! exports, a missing package's `MODULE_NOT_FOUND`); an ES module main
//! imports them (a CommonJS package's `module.exports` as its default and
//! its properties as names, the dual package's `import` build, `node:fs`
//! and `path`). The expected lines are Node 24's for the same files.

const FILES: &[(&str, &str)] = &[
    (
        "node_modules/cjs-a/package.json",
        r#"{ "name": "cjs-a", "main": "lib/index" }"#,
    ),
    (
        "node_modules/cjs-a/lib/index.js",
        r#"const b = require("./b");
const data = require("../data.json");
exports.sum = (x, y) => x + y;
exports.b = b;
exports.data = data;
exports.dir = require("path").basename(__dirname);
"#,
    ),
    (
        "node_modules/cjs-a/lib/b.js",
        r#"module.exports = "B";
"#,
    ),
    ("node_modules/cjs-a/data.json", r#"{ "k": 1 }"#),
    (
        "node_modules/dual/package.json",
        r#"{ "name": "dual", "type": "module", "exports": { ".": { "types": "./x.d.ts", "import": "./index.js", "require": "./index.cjs" }, "./extra": "./extra.js" } }"#,
    ),
    (
        "node_modules/dual/index.js",
        r#"export const kind = "esm";
export default 42;
"#,
    ),
    (
        "node_modules/dual/index.cjs",
        r#"module.exports = { kind: "cjs" };
"#,
    ),
    (
        "node_modules/dual/extra.js",
        r#"export const extra = "extra";
"#,
    ),
    (
        "node_modules/cyc/package.json",
        r#"{ "name": "cyc", "main": "a.js" }"#,
    ),
    (
        "node_modules/cyc/a.js",
        r#"exports.early = "a-early";
const b = require("./b");
exports.late = "a-late";
exports.fromB = b.seen;
"#,
    ),
    (
        "node_modules/cyc/b.js",
        r#"const a = require("./a");
exports.seen = [a.early, "late" in a].join(",");
"#,
    ),
    (
        "main.cjs",
        r#"const a = require("cjs-a");
const dual = require("dual");
const cyc = require("cyc");
const path = require("path");
console.log(a.sum(2, 3), a.b, a.data.k, a.dir, dual.kind, cyc.fromB, cyc.late);
console.log(path.basename(__filename), require("util").format("%s:%d", "n", 7), typeof require.resolve("cjs-a"));
try { require("missing-pkg"); } catch (e) { console.log(e.code); }
console.log(require("cjs-a") === a, require.cache[require.resolve("cjs-a")].loaded);
"#,
    ),
    (
        "main.mjs",
        r#"import a, { sum } from "cjs-a";
import dual, { kind } from "dual";
import { extra } from "dual/extra";
import fs from "node:fs";
import { join } from "path";
console.log(sum(4, 5), a.b, dual, kind, extra, typeof fs.readFileSync, join("x", "y").length);
"#,
    ),
];

fn lines(o: &zipp_vm::Outcome) -> Vec<String> {
    assert_eq!(o.error, None, "{:?}", o.errput);
    o.output
        .iter()
        .flat_map(|l| l.lines().map(str::to_string))
        .collect()
}

#[test]
fn packages_resolve_and_commonjs_interoperates_as_in_node() {
    let root = std::env::temp_dir().join(format!("zipp-npm-{}", std::process::id()));
    for (rel, text) in FILES {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    let script = root.join("main.cjs");
    zipp_vm::set_node_runtime(
        true,
        vec!["zipp".into(), script.to_string_lossy().into_owned()],
    );
    let src = std::fs::read_to_string(&script).unwrap();
    let out = zipp_vm::run_with_base(&src, Some(root.clone())).expect("compiles");
    assert_eq!(
        lines(&out),
        [
            "5 B 1 lib cjs a-early,false a-late",
            "main.cjs n:7 string",
            "MODULE_NOT_FOUND",
            "true true",
        ]
    );
    let module = root.join("main.mjs");
    zipp_vm::set_node_runtime(
        true,
        vec!["zipp".into(), module.to_string_lossy().into_owned()],
    );
    let out = zipp_vm::run_module_file(&module, None).expect("compiles");
    assert_eq!(lines(&out), ["9 B 42 esm extra function 3"]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A core module zipp does not have loads, as packages require them at
/// their top (a server library extends `net.Server` as it loads);
/// reading its members and extending its classes work, and calling or
/// constructing one throws ERR_ZIPP_UNSUPPORTED naming it. Not Node's
/// output: Node has these modules.
#[test]
fn core_modules_zipp_lacks_load_and_say_so_when_used() {
    let root = std::env::temp_dir().join(format!("zipp-npm-stub-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("main.cjs");
    std::fs::write(
        &script,
        r#"const net = require("net");
class R extends net.Server {}
console.log(typeof R, R.prototype instanceof net.Server, net.Server === net.Server);
for (const f of [() => new R(), () => require("http").createServer(), () => net.connect()]) {
  try { f(); } catch (e) { console.log(e.code, e.message); }
}
import("node:zlib").then((z) => console.log("zlib", typeof z.default.gzipSync));
"#,
    )
    .unwrap();
    zipp_vm::set_node_runtime(
        true,
        vec!["zipp".into(), script.to_string_lossy().into_owned()],
    );
    let src = std::fs::read_to_string(&script).unwrap();
    let out = zipp_vm::run_with_base(&src, Some(root.clone())).expect("compiles");
    assert_eq!(
        lines(&out),
        [
            "function true true",
            "ERR_ZIPP_UNSUPPORTED zipp: node:net.Server is not supported",
            "ERR_ZIPP_UNSUPPORTED zipp: node:http.createServer is not supported",
            "ERR_ZIPP_UNSUPPORTED zipp: node:net.connect is not supported",
            "zlib function",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}
