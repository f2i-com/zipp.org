//! Virtual sparse Array lengths must never be represented by a dense-Array
//! JIT snapshot: its cached length is the dense `Vec` length and is also used
//! for element bounds.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const CHILD_ENV: &str = "ZIPP_TEST_SPARSE_ARRAY_JIT_LENGTH_CHILD";

const STATIC_SRC: &str = r#"
var a = new Array(1048577);
var sum = 0;
for (var i = 0; i < 1000; i++) sum += a.length;
console.log("static", a.length, sum);
"#;

const DYNAMIC_SRC: &str = r#"
var a = [1, 2, 3, 4];
function change(i) {
  var junk = [i, i + 1];
  if (junk[0] === -1) a[0] = junk[1];
  if (i === 100) a.length = 1048577;
  else if (i === 200) a.length = 1048581;
  else if (i === 300) a[1048588] = 9;
  else if (i === 400) a.length = 2;
  return a.length;
}
var sum = 0;
for (var i = 0; i < 500; i++) sum += change(i);
console.log("dynamic", a.length, sum, a[1048588]);
"#;

// The caller's loop first installs and repeatedly reuses a dense-Array length
// snapshot. The cross call then changes only the virtual-length side table.
// B244 must observe that mutation, re-enter `jit_ta_snapshot`, and let its
// runtime side-table check publish an all-zero (declined) snapshot before the
// immediately following `.length` read.
const POST_CROSS_SRC: &str = r#"
var a = [1, 2, 3, 4];
function change(i) {
  if (i === 200000) a.length = 1048577;
  return 1;
}
var sum = 0;
for (var i = 0; i < 400000; i++) {
  var one = change(i);
  sum += a.length + one;
}
console.log("post-cross", a.length, sum);
"#;

fn assert_output(src: &str, expected: &str) {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, [expected]);
}

#[test]
fn sparse_array_length_child() {
    let Some(which) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    match which.to_str() {
        Some("static") => assert_output(STATIC_SRC, "static 1048577 1048577000"),
        Some("dynamic") => assert_output(DYNAMIC_SRC, "dynamic 2 314575300 undefined"),
        Some("post-cross") => assert_output(POST_CROSS_SRC, "post-cross 1048577 209716600000"),
        other => panic!("unexpected child case: {other:?}"),
    }
}

fn child(which: &str, mode: &str) -> (String, String) {
    let exe = std::env::current_exe().expect("integration-test executable");
    let mut command = std::process::Command::new(exe);
    command
        .args([
            "sparse_array_length_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, which)
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_GC_STRESS")
        .env_remove("ZIPP_JITLOG")
        .env_remove("ZIPP_NO_CROSS_ARRAY_SNAPSHOT_EPOCH");
    match mode {
        "jitlog" => {
            command.env("ZIPP_JITLOG", "1");
        }
        "nojit" => {
            command.env("ZIPP_NOJIT", "1");
        }
        "gc" => {
            command.env("ZIPP_GC_STRESS", "1");
        }
        "epoch-off" => {
            command.env("ZIPP_NO_CROSS_ARRAY_SNAPSHOT_EPOCH", "1");
        }
        _ => panic!("unknown child mode: {mode}"),
    }
    let out = command.output().expect("spawn sparse-length child");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "{which}/{mode} child failed:\n{stdout}\n{stderr}"
    );
    (stdout, stderr)
}

#[test]
fn preexisting_virtual_length_declines_the_dense_pin_and_matches_fallbacks() {
    let (stdout, stderr) = child("static", "jitlog");
    let log = format!("{stdout}\n{stderr}");
    assert!(
        log.contains("built pins=0 access=[]"),
        "the virtual-length receiver unexpectedly installed an Array pin:\n{log}"
    );
    child("static", "nojit");
    child("static", "gc");
}

#[test]
fn cross_call_virtual_grow_retain_sparse_write_and_shrink_match_fallbacks() {
    child("dynamic", "jitlog");
    child("dynamic", "nojit");
    child("dynamic", "gc");
}

#[test]
fn post_cross_virtual_growth_invalidates_an_installed_dense_snapshot() {
    let (stdout, stderr) = child("post-cross", "jitlog");
    let log = format!("{stdout}\n{stderr}");
    assert!(
        log.lines()
            .any(|line| line.contains("[pin]") && line.contains("built pins=1")),
        "the caller did not install the dense Array snapshot under test:\n{log}"
    );
    assert!(
        log.contains("Tier C fn1 compiled"),
        "the length mutation did not cross the compiled callee under test:\n{log}"
    );
    child("post-cross", "nojit");
    child("post-cross", "gc");
    child("post-cross", "epoch-off");
}
