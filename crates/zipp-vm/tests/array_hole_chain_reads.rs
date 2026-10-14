//! A hole or out-of-range read on an Array, once an index defined on a
//! prototype has turned off the "no prototype has an index" shortcut, walks
//! the chain by number (`Vm::hole_chain_get`) where every level is a plain
//! object. This battery pins it to Node's answers: indices inherited from
//! either prototype and the nearer one winning, accessors run with the array
//! as receiver, and custom, null, Proxy and class-instance prototypes.

const SOURCE: &str = r#"
"use strict";
// Hole and out-of-range reads once an index on a prototype has turned off the
// "no prototype has an index" shortcut: every such read walks the chain.
var a = [0, , 2, , 4];
function show(label, x) { console.log(label, typeof x === "string" ? x : String(x)); }
show("before", a[1]);
Object.defineProperty(Array.prototype, "1", { value: "AP1", configurable: true });
show("inherited", a[1]);
show("own wins", a[2]);
show("other hole", a[3]);
show("past end", a[10]);
show("past end inherited", [][1]);
Object.prototype[3] = "OP3";
show("object proto", a[3]);
Array.prototype[3] = "AP3";
show("nearer wins", a[3]);
delete Array.prototype[3];
delete Object.prototype[3];
show("deleted", a[3]);

// An accessor on the chain runs with the array as its receiver.
Object.defineProperty(Array.prototype, "7", {
  get: function () { return "getter:" + (this === a) + ":" + this.length; },
  configurable: true
});
show("accessor", a[7]);
var b = [];
b.length = 9;
show("accessor other receiver", b[7]);
delete Array.prototype[7];

// Non-default chains.
var c = [, 1];
Object.setPrototypeOf(c, { 0: "custom0" });
show("custom proto", c[0]);
show("custom proto miss", c[5]);
var d = [, 1];
Object.setPrototypeOf(d, null);
show("null proto", d[0]);
var e = [, 1];
Object.setPrototypeOf(e, new Proxy({}, { get: function (t, k) { return "proxy:" + String(k); } }));
show("proxy proto", e[0]);
class K { constructor() { this[0] = "instance0"; } }
var f = [, 1];
Object.setPrototypeOf(f, new K());
show("class instance proto", f[0]);
var g = [, 1];
Object.setPrototypeOf(g, Object.create(Array.prototype, { 0: { value: "between", enumerable: true } }));
show("two levels", g[0]);
show("two levels to Array.prototype", g[1 + 0] + "|" + g[2]);

// Many reads in a loop, both shapes, the counts compared.
var probe = [];
for (var p = 0; p < 64; p++) probe[p * 8] = p;
Object.defineProperty(Array.prototype, "5", { value: "P5", configurable: true });
var inherited = 0, missing = 0;
for (var r = 0; r < 20000; r++) {
  if (probe[(r % 64) * 8 + 1] === "AP1" || probe[(r % 64) * 8 + 1] !== undefined) inherited++;
  else missing++;
  if (probe[5] === "P5") inherited++;
}
show("loop", inherited + "/" + missing);
"#;

fn output_from_node(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn array_hole_chain_reads_worker() {
    if std::env::var_os("ZIPP_ARRAY_HOLE_CHAIN_READS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    assert_eq!(out.output, output_from_node(SOURCE));
}

#[test]
fn array_hole_chain_reads_matches_node() {
    if std::env::var_os("ZIPP_ARRAY_HOLE_CHAIN_READS_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc_stress) in [
        ("interpreter", true, false),
        ("interpreter-gc-stress", true, true),
        ("default-tiers", false, false),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["array_hole_chain_reads_worker", "--exact", "--nocapture"])
            .env("ZIPP_ARRAY_HOLE_CHAIN_READS_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
        }
        if gc_stress {
            command.env("ZIPP_GC_STRESS", "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
