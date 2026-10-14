//! `i in arr` answers a hole or an index past the dense end as absent while
//! no prototype can supply an index (the protector a hole read uses), with
//! no key text built. This battery pins it to Node's answers: overlays,
//! sparse storage, deleted elements, custom prototypes, arguments objects,
//! and inherited indices once the protector is off.

const SOURCE: &str = r#"
"use strict";
// `i in arr` for holes and indices past the end, on both sides of the
// "no prototype has an index" protector.
function show(label, v) { console.log(label, String(v)); }
var a = [0, , 2, , 4];
show("present", 0 in a);
show("hole", 1 in a);
show("past end", 9 in a);
show("negative", -1 in a);
show("fraction", 1.5 in a);
var sparse = [];
sparse[100000] = 1;
show("sparse present", 100000 in sparse);
show("sparse absent", 5000 in sparse);
var deleted = [1, 2, 3];
delete deleted[1];
show("deleted", 1 in deleted);
var overlay = [0, , 2];
Object.defineProperty(overlay, "1", { get: function () { return 9; }, configurable: true });
show("accessor overlay", 1 in overlay);
var custom = [, 1];
Object.setPrototypeOf(custom, { 0: "from proto" });
show("custom proto", 0 in custom);
(function () {
  delete arguments[0];
  show("arguments hole", 0 in arguments);
  show("arguments present", 1 in arguments);
})(1, 2);
var loop = 0;
for (var i = 0; i < 1000; i++) if (i % 7 in a) loop++;
show("loop", loop);
// The protector turns off: inherited indices must now be found.
Object.prototype[3] = "OP3";
show("object proto index", 3 in a);
show("still absent", 1 in a);
Array.prototype[1] = "AP1";
show("array proto index", 1 in a);
show("past end inherited", 1 in []);
delete Array.prototype[1];
delete Object.prototype[3];
show("removed", 1 in a);
loop = 0;
for (var i = 0; i < 1000; i++) if (i % 7 in a) loop++;
show("loop after", loop);
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
fn array_in_holes_worker() {
    if std::env::var_os("ZIPP_ARRAY_IN_HOLES_WORKER").is_none() {
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
fn array_in_holes_matches_node() {
    if std::env::var_os("ZIPP_ARRAY_IN_HOLES_WORKER").is_some() {
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
            .args(["array_in_holes_worker", "--exact", "--nocapture"])
            .env("ZIPP_ARRAY_IN_HOLES_WORKER", "1")
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
