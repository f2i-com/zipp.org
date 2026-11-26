//! A read of an absent key on a null-prototype object must not teach the
//! shape's memo that ordinary objects of that shape lack the key.
//!
//! The megamorphic memo is keyed by shape and key. Its "absent" entry is
//! read for objects whose prototype is %Object.prototype% as well as for
//! null-prototype ones, checked only against %Object.prototype%'s version.
//! The leaf read's helper wrote that entry for a null-prototype receiver
//! without asking whether %Object.prototype% has the key, so `valueOf` on
//! `{a, b}` read `undefined` once `Object.create(null)` objects with `a` and
//! `b` had been read (0.0.41, wherever the Node layer is not installed: this
//! crate's own `run`, and every embedder).
//!
//! The expected lines are Node 24's.

const INHERITED_AFTER_NULL_PROTO: &str = r#"
function getV(o) { return o.valueOf; }
function useIt(o) { return getV(o) === undefined ? 0 : 1; }
function mkWarm(i) { var n = Object.create(null); n.x = i; return n; }
function mkNull(i) { var n = Object.create(null); n.a = i; n.b = 2; return n; }
function mkNorm(i) { return { a: i, b: 2 }; }
var t0 = 0, t1 = 0, t2 = 0;
for (var i = 0; i < 50000; i++) t0 += useIt(mkWarm(i));
for (var i = 0; i < 50000; i++) t1 += useIt(mkNull(i));
for (var i = 0; i < 50000; i++) t2 += useIt(mkNorm(i));
console.log(t0 + " " + t1 + " " + t2 + " " + typeof mkNorm(1).valueOf);
function getH(o) { return o.hasOwnProperty; }
function getT(o) { return o.toString; }
function getQ(o) { return o.nothingHere; }
var h = 0, t = 0, q = 0;
for (var i = 0; i < 40000; i++) {
  var n = mkNull(i);
  h += getH(n) === undefined ? 0 : 1;
  t += getT(n) === undefined ? 0 : 1;
  q += getQ(n) === undefined ? 0 : 1;
}
for (var i = 0; i < 40000; i++) {
  var o = mkNorm(i);
  h += getH(o) === undefined ? 0 : 1;
  t += getT(o) === undefined ? 0 : 1;
  q += getQ(o) === undefined ? 0 : 1;
}
console.log(h + " " + t + " " + q);
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn absent_memo_keeps_inherited_keys_of_ordinary_objects() {
    assert_eq!(
        run_ok(INHERITED_AFTER_NULL_PROTO),
        ["0 0 50000 function", "40000 40000 0"]
    );
}

#[test]
fn absent_memo_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("absent_memo_keeps")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
