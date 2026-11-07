//! The interpreter's property read past its first way: `.length` answered
//! before any probe, and a site's own field in another layout answered by the
//! (shape, key) memo from the first way's check. Every receiver kind meets one
//! site here: an own `length` field, a `length` getter (own, inherited, class),
//! arrays, strings, typed arrays, `arguments`, String wrappers, functions; and
//! a field read through many layouts, then deleted, turned into an accessor,
//! shadowed on the prototype and read from a dictionary-mode object. The
//! expected lines are Node 24's.

const SOURCE: &str = r#"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));

function len(o) { return o.length; }
class WithGetter { get length() { return "getter"; } }
const protoLen = Object.create({ length: "inherited" });
const ownGetter = Object.defineProperty({}, "length", { get() { return "own getter"; } });
function args() { return len(arguments); }
const kinds = [
  [1, 2, 3], "abcd", new Uint8Array(5), { length: 7 }, new WithGetter(), protoLen, ownGetter,
  new String("xyz"), function (a, b) {}, [], "", { length: undefined }, Object.assign([1, 2], { extra: 1 }),
];
for (let r = 0; r < 3; r++) {
  log("len", kinds.map(len).join(","), args(1, 2), len(new Array(10)));
}

// One site, many layouts of the same field.
function flags(n) { return n.flags; }
const nodes = [];
for (let i = 0; i < 40; i++) {
  const o = {};
  for (let k = 0; k < i % 9; k++) o["k" + k] = k;
  o.flags = i;
  if (i % 5 === 0) o["late" + i] = i;
  nodes.push(o);
}
let sum = 0;
for (let r = 0; r < 50; r++) for (const n of nodes) sum += flags(n);
log("flags", sum);

// The field changes under the site: deleted, an accessor, inherited, a dictionary.
const changed = nodes.slice(0, 6);
delete changed[0].flags;
Object.defineProperty(changed[1], "flags", { get() { return "acc"; } });
const child = Object.create(changed[2]);
const dict = { flags: "dict" };
for (let k = 0; k < 100; k++) dict["d" + k] = k;
for (let k = 0; k < 100; k++) delete dict["d" + k];
const proxy = new Proxy({ flags: "target" }, { get(t, k) { return k === "flags" ? "trap" : t[k]; } });
for (let r = 0; r < 3; r++) {
  log("changed", flags(changed[0]), flags(changed[1]), flags(child), flags(dict), flags(proxy), flags(changed[3]));
}
Object.prototype.flags = "proto";
log("proto", flags({}), flags(changed[0]), flags(nodes[7]));
delete Object.prototype.flags;
log("after", flags({}), flags(changed[0]));

// A read that throws keeps its message.
try { flags(undefined); } catch (e) { log("throw", e.constructor.name); }
try { len(null); } catch (e) { log("throw", e.constructor.name); }
console.log(out.join("\n"));
"#;

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
fn prop_reads_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

/// The same program in fresh children: the interpreter alone, with the memo
/// off, with the lite loop off, with the JIT compiling eagerly, and under GC
/// stress.
#[test]
fn prop_reads_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("no-mega", &[("ZIPP_NOJIT", "1"), ("ZIPP_NO_IC_MEGA", "1")][..]),
        ("no-lite", &[("ZIPP_NOJIT", "1"), ("ZIPP_NO_LITE", "1")][..]),
        ("eager-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["prop_reads_match_node", "--exact"]);
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
