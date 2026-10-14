//! `for-in` over an Array whose prototypes yield no key lists the Array's own
//! enumerable keys directly (`Vm::for_in_array_own_only`), with no shadow set.
//! This battery pins it to Node's answers on both sides of that condition:
//! holes, named and symbol keys, sparse storage, mutation during the loop,
//! enumerable additions to either prototype and their removal, re-pointed
//! prototypes, arguments objects, subclasses and exotic index spellings.

const SOURCE: &str = r#"
"use strict";
function keys(o) { var r = []; for (var k in o) r.push(k); return r.join(","); }
var a = [10, , 30];
a.named = 1;
Object.defineProperty(a, "hidden", { value: 2, enumerable: false });
a[Symbol("s")] = 3;
console.log("plain", keys(a));
var sparse = [];
for (var i = 0; i < 6; i++) sparse[i * 1000] = i;
sparse.x = "x";
console.log("sparse", keys(sparse));
console.log("empty", keys([]), keys(new Array(5)));
// Deleting and adding during the loop.
var b = [1, 2, 3, 4];
var seen = [];
for (var k in b) { seen.push(k); if (k === "0") { delete b[2]; b[9] = 9; } }
console.log("mutate", seen.join(","));
// Enumerable additions to the prototypes appear, after own keys, once.
Array.prototype.extra = "e";
console.log("arrproto", keys([1, 2]));
var shadow = [1];
shadow.extra = "own";
console.log("shadowed", keys(shadow));
delete Array.prototype.extra;
console.log("removed", keys([1, 2]));
Object.prototype.objExtra = 1;
console.log("objproto", keys([5]));
delete Object.prototype.objExtra;
Object.defineProperty(Array.prototype, "7", { value: "p7", enumerable: true, configurable: true });
console.log("indexproto", keys([1, , 3]));
delete Array.prototype[7];
console.log("restored", keys([1, , 3]));
// Non-default prototypes and arguments objects.
var c = [1, 2];
Object.setPrototypeOf(c, { inherited: 1 });
console.log("customproto", keys(c));
(function () { console.log("arguments", keys(arguments)); })(4, 5, 6);
class Sub extends Array {}
var s = new Sub(); s.push(1, 2); s.tag = "t";
console.log("subclass", keys(s));
// Accessors and frozen arrays.
var d = [1, 2, 3];
Object.defineProperty(d, "1", { get: function () { return 22; }, enumerable: false });
console.log("nonenum index", keys(d));
console.log("frozen", keys(Object.freeze([7, 8])));
var big = []; big[4294967294] = 1; big[4294967295] = 2; big["-1"] = 3; big["01"] = 4;
console.log("exotic indices", keys(big));
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
fn for_in_array_own_keys_worker() {
    if std::env::var_os("ZIPP_FOR_IN_ARRAY_OWN_KEYS_WORKER").is_none() {
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
fn for_in_array_own_keys_matches_node() {
    if std::env::var_os("ZIPP_FOR_IN_ARRAY_OWN_KEYS_WORKER").is_some() {
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
            .args(["for_in_array_own_keys_worker", "--exact", "--nocapture"])
            .env("ZIPP_FOR_IN_ARRAY_OWN_KEYS_WORKER", "1")
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
