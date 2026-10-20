//! `class W extends WeakRef` and `class R extends FinalizationRegistry`.
//!
//! A derived class's `super()` into either left the instance a plain Object,
//! so `deref` and `register` threw "incompatible receiver". The expected
//! lines are Node's (v24).

const PROBE: &str = r#"
class W extends WeakRef {}
const o = {};
const w = new W(o);
console.log(w instanceof WeakRef, w.deref() === o, Object.getPrototypeOf(w) === W.prototype, String(w));
try { new W(1); } catch (e) { console.log(e instanceof TypeError); }
class R extends FinalizationRegistry {}
const r = new R(() => {});
r.register({}, 1);
console.log(r instanceof FinalizationRegistry, Object.getPrototypeOf(r) === R.prototype, r.unregister(o));
try { new R(1); } catch (e) { console.log(e instanceof TypeError); }
"#;

const EXPECTED: &str = "true true true [object WeakRef]\ntrue\ntrue true false\ntrue";

#[test]
fn weakref_and_finalization_registry_subclasses_match_node() {
    let out = zipp_vm::run(PROBE).expect("the probe compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
