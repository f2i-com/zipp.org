//! Sloppy global writes that must not become plain slot writes, written by
//! assignment and by a `var` initializer, cold and in hot loops: the
//! non-writable builtin globals, and a declared `var` that `defineProperty`
//! later made non-writable (allowed on a non-configurable data property).
//! A plain `StoreGlobal` gates its route probe on the global-route epoch, as
//! the strict store and `LoadGlobal` do; these are the routes the epoch must
//! still catch. Node evaluates `-e` source as a script, so `var` is a global
//! binding here (the expected lines also match Bellard's QuickJS).

const SOURCE: &str = r#"
// Sloppy SCRIPT global writes that must not become plain slot writes: the non-writable builtin
// globals, and a declared var later made non-writable on the global object (allowed on a
// non-configurable data property), written by assignment and by a `var` initializer, cold and hot.
// Run as a script (QuickJS is the reference; Node would run it as a CommonJS module).
var out = [];
NaN = 5; out.push("NaN=" + NaN);
undefined = 5; out.push("undefined=" + undefined);
Infinity = 1; out.push("Infinity=" + Infinity);
var NaN = 7; out.push("varNaN=" + NaN);
var undefined = 7; out.push("varUndefined=" + undefined);
for (var k1 = 0; k1 < 3000; k1++) { NaN = k1; var Infinity = k1; }
out.push("hotNaN=" + NaN + " hotInfinity=" + Infinity);
var fixed = 1;
for (var k0 = 0; k0 < 3000; k0++) { fixed = k0; var fixed = k0 + 1; }
out.push("beforeFreeze=" + fixed);
Object.defineProperty(globalThis, "fixed", { writable: false });
fixed = 99; out.push("assignFrozen=" + fixed);
var fixed = 98; out.push("varFrozen=" + fixed);
for (var k2 = 0; k2 < 3000; k2++) { fixed = k2; var fixed = -k2; }
out.push("hotFrozen=" + fixed + " desc=" + JSON.stringify(Object.getOwnPropertyDescriptor(globalThis, "fixed")));
var plain = 1; for (var j = 0; j < 3000; j++) { var plain = j; } out.push("plain=" + plain);
for (var q = 0; q < out.length; q++) console.log(out[q]);
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
fn global_store_routes_worker() {
    if std::env::var_os("ZIPP_GLOBAL_STORE_ROUTES_WORKER").is_none() {
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
fn global_store_routes_matches_node() {
    if std::env::var_os("ZIPP_GLOBAL_STORE_ROUTES_WORKER").is_some() {
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
            .args(["global_store_routes_worker", "--exact", "--nocapture"])
            .env("ZIPP_GLOBAL_STORE_ROUTES_WORKER", "1")
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
