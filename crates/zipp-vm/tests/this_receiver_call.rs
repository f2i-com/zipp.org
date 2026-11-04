//! A split method call on `this` (`this.m(this.x)`: the method read before an
//! argument that may run code) uses `this`'s own register as the captured
//! receiver, with no snapshot copy, outside a derived constructor and a
//! redirected `this`. Getter order, arrows, sloppy boxing, derived
//! constructors, static blocks and field initializers keep their meaning;
//! the expected lines are Node's.

const SOURCE: &str = r##"// Split method calls on `this` (`this.m(this.x)`): the method is read before
// the arguments, `this` is the receiver, and getters that replace the method
// mid-call keep the order. Run hot enough for the native tiers.
var out = [];
function t(name, f) { try { out.push(name + " " + f()); } catch (e) { out.push(name + " threw " + e.constructor.name); } }
class Vec {
  constructor(x, y) { this.x = x; this.y = y; }
  get len() { return Math.sqrt(this.x * this.x + this.y * this.y); }
  scale(k) { return new Vec(this.x * k, this.y * k); }
  norm() { return this.scale(1 / this.len); }
  add(o) { return new Vec(this.x + o.x, this.y + o.y); }
  twice() { return this.add(this.scale(1)); }
}
t("norm", () => { var v = new Vec(3, 4).norm(); return v.x + "," + v.y; });
t("hot", () => { var s = 0; for (var i = 1; i < 4000; i++) { var v = new Vec(i, i + 1).norm().twice(); s += v.x; } return s.toFixed(6); });
t("getter swaps method", () => {
  var log = [];
  var o = { n: 0,
    get m() { log.push("get m"); return function (a) { return "first " + a; }; },
    get k() { log.push("get k"); Object.defineProperty(this, "m", { value: function (a) { return "second " + a; }, configurable: true }); return 1; },
    run() { return this.m(this.k); } };
  var r = o.run();
  return r + " | " + log.join(",") + " | " + o.m(2);
});
t("arrow this", () => { var o = { v: 5, f() { var g = () => this.h(this.v + 1); return g(); }, h(x) { return x * 2; } }; return o.f(); });
t("sloppy this", function () { function F() { return this.toString(this.valueOf()); } return typeof F.call(7); });
t("derived ctor", () => { class A { m(x) { return x; } } class B extends A { constructor() { super(); this.r = this.m(this.k = 3); } } return new B().r; });
t("static block", () => { class S { static v = 2; static m(x) { return x + 1; } static { S.r = this.m(this.v); } } return S.r; });
t("field init", () => { class F { a = 4; m(x) { return x * 3; } b = this.m(this.a); } return new F().b; });
console.log(out.join("\n"));
"##;

const NODE: &str = r##"norm 0.6000000000000001,0.8
hot 5649.477471
getter swaps method first 1 | get m,get k | second 2
arrow this 12
sloppy this string
derived ctor 3
static block 3
field init 12
"##;

#[test]
fn this_receiver_call_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    let want: Vec<String> = NODE.trim_start_matches('\n').lines().map(str::to_owned).collect();
    assert_eq!(got, want);
}

/// The corpus again with the snapshot back, eager JIT, GC stress and the
/// interpreter, each in a fresh process.
#[test]
fn this_receiver_call_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_THIS_RECEIVER", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["this_receiver_call_parity", "--exact"])
            .env(mode.0, mode.1)
            .output()
            .expect("spawn test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("running 0 tests"), "mode filter matched nothing: {stdout}");
    }
}