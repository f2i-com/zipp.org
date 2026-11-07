//! `JSON.stringify` with an indent through the plain fast walk, and
//! `JSON.parse` with a reviver writing values back in place. The indent
//! cases cover empty and nested containers, shaped and dictionary-mode
//! objects, numeric, string, clamped and lone-surrogate gaps, and every
//! decline the fast walk has (toJSON, accessors, integer keys, holes,
//! proxies, cycles). The reviver cases replace, delete, freeze, redefine and
//! restructure what they are handed. The expected lines are Node 24's.

const SOURCE: &str = r##"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const show = (v) => JSON.stringify(v);

const dict = { a: 1, b: [1, { c: 2 }] };
for (let k = 0; k < 40; k++) dict["x" + k] = k;
for (let k = 0; k < 40; k++) delete dict["x" + k];
const VALUES = [
  {}, [], [[]], [{}], { a: [] }, { a: {} }, [1, "two", null, true, false, -0, NaN, Infinity, 1e21, 1.5],
  { s: "q\"u\\o\nte\u0001 😀\ud800", n: { m: { o: [1, [2, [3]]] } } }, "plain", 42, null,
  [undefined, function () {}, Symbol("x")], { u: undefined, f() {}, y: 1 }, dict,
  { 2: "int", a: 1 }, { toJSON() { return { t: [1] }; } }, [new Date(0)], { g: 1, get h() { return [2]; } },
  Object.create({ inherited: 1 }), [, 1], new Proxy({ p: 1 }, {}), { nested: { deeper: { deepest: [{}, []] } } },
];
const SPACES = [2, "\t", 0, -1, 12, "ab", "0123456789AB", "", 1.9, new Number(3), new String(" "), "\ud800"];
for (const v of VALUES) {
  log(SPACES.map((sp) => show(JSON.stringify(v, null, sp))).join(" "));
}
const cyc = { a: [] };
cyc.a.push(cyc);
try { JSON.stringify(cyc, null, 2); } catch (e) { log("cycle", e.constructor.name); }
log("arrays", JSON.stringify([[1, 2], { a: [3, { b: [] }] }], null, 3));

// Revivers.
const SRC = '{"id":7,"items":[{"qty":2,"sku":"A"},{"qty":5,"sku":"B"}],"meta":{"_secret":"x","depth":1},"tags":["a","b"],"n":null}';
const RE = [
  (k, v) => (typeof v === "number" ? v * 2 : v),
  (k, v) => (k === "_secret" ? undefined : v),
  (k, v) => (k === "qty" ? String(v) : v),
  function (k, v) { if (k === "0" && Array.isArray(this)) Object.freeze(this); return typeof v === "string" ? v + "!" : v; },
  function (k, v) { if (k === "sku") Object.defineProperty(this, "qty", { value: 99, writable: false, configurable: false }); return v; },
  function (k, v) { if (k === "id") Object.defineProperty(this, "tags", { get() { return ["g"]; }, configurable: true, enumerable: true }); return v; },
  function (k, v) { if (k === "items") this.meta = { replaced: true }; return v; },
  function (k, v) { if (Array.isArray(v)) v.length = 1; return v; },
  function (k, v) { if (k === "1") delete this[0]; return v; },
  (k, v, ctx) => (ctx && "source" in ctx && typeof v === "number" ? ctx.source + "#" : v),
  function (k, v) { if (k === "id") Object.preventExtensions(this); return k === "n" ? 0 : v; },
  function (k, v) { if (k === "") return v; return k === "tags" ? undefined : v; },
];
for (const r of RE) {
  try { log("revive", show(JSON.parse(SRC, r))); } catch (e) { log("revive threw", e.constructor.name); }
}
log("revive deep", show(JSON.parse("[[[[1,[2]]]],{\"a\":{\"b\":[3]}}]", (k, v) => (typeof v === "number" ? -v : v))));
let count = 0;
for (let i = 0; i < 300; i++) count += JSON.parse(SRC, (k, v) => (k === "qty" ? v + i : v)).items[1].qty;
log("revive loop", count);
console.log(out.join("\n"));
"##;

fn node_output(src: &str) -> Vec<String> {
    use std::io::Write;
    let mut child = std::process::Command::new("node")
        .args(["--input-type=commonjs", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node on PATH");
    child.stdin.take().expect("stdin").write_all(src.as_bytes()).expect("write source");
    let out = child.wait_with_output().expect("node output");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn json_indent_and_reviver_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    let want = node_output(SOURCE);
    for (i, (a, b)) in lines.iter().zip(want.iter()).enumerate() {
        assert_eq!(a, b, "line {i} differs");
    }
    assert_eq!(lines.len(), want.len());
}

/// The same program with the plain walk off (the general serializer is the
/// reference), without the JIT, and under GC stress.
#[test]
fn json_indent_and_reviver_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [
        ("no-plain", &[("ZIPP_NO_JSON_PLAIN_FAST", "1")][..]),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["json_indent_and_reviver_match_node", "--exact"]);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
