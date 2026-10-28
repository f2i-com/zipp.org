//! Method reads on class instances from compiled code (Tier C's class-method
//! lane, `ClassReadPlan`, with the class in the receiver's mirror), with what
//! must send a read elsewhere: another class with the same fields, a
//! subclass, a plain object of the same shape, the method replaced, deleted
//! and restored on the prototype, an own property shadowing it, an instance
//! given another prototype, and a getter defined over it. Run interpreted, at
//! the default thresholds, with every function compiled at once, under GC
//! stress and with the lane off; the expected lines are Node 24's.

const EXPECTED: &str = "210000
1787.5
350
TypeError
350
TypeError
350
312.5
550
787.5
150
350
100
350
100";

#[test]
fn class_read_lane_child() {
    if std::env::var_os("ZIPP_CLASS_READ_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn method_reads_follow_the_class_chain() {
    if std::env::var_os("ZIPP_CLASS_READ_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("lane-off", Some(("ZIPP_NO_CLASS_READ_LANE", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "class_read_lane_child", "--nocapture"])
            .env("ZIPP_CLASS_READ_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// Method reads on class instances from compiled code, with what must send a
// read elsewhere: another class with the same fields, a subclass, the
// method replaced or deleted on the prototype, an own property shadowing
// it, an instance given another prototype, a getter defined over it, and
// plain objects of the same shape. The read and its call are separate ops
// (`o.m` then a call with `o` as `this`), as for any method call.
"use strict";
class V {
  constructor(x, y) { this.x = x; this.y = y; }
  sum() { return this.x + this.y; }
  scale(k) { return new V(this.x * k, this.y * k); }
}
class W {
  constructor(x, y) { this.x = x; this.y = y; }
  sum() { return this.x * this.y; }
  scale(k) { return new W(this.x + k, this.y + k); }
}
class V2 extends V {
  sum() { return 1000 + super.sum(); }
}
// The compiled reader: `o.scale` and `.sum` read as values (a `GetProp`
// each, as compiled TypeScript method calls read them) and then called, one
// round per call so the body compiles whole.
function round(o) {
  const f = o.scale;
  const a = f.call(o, 2);
  const g = a.scale;
  const b = g.call(a, 0.5);
  const h = b.sum;
  return h.call(b);
}
function chain(o, n) {
  let acc = 0;
  for (let i = 0; i < n; i++) acc += round(o);
  return acc;
}
function tryChain(o, n) {
  try { return chain(o, n); } catch (e) { return e.constructor.name; }
}
const out = [];
const v = new V(3, 4);
out.push(chain(v, 30000));
// Same fields, another class; a subclass (its instances are V2's? no: scale
// builds V); a plain object of the same shape.
out.push(chain(new W(3, 4), 50), chain(new V2(3, 4), 50));
out.push(tryChain({ x: 3, y: 4 }, 5));
// The method replaced, then deleted, then restored.
const savedSum = V.prototype.sum;
V.prototype.sum = function () { return 7; };
out.push(chain(v, 50));
delete V.prototype.sum;
out.push(tryChain(v, 3));
V.prototype.sum = savedSum;
out.push(chain(v, 50));
// An own property shadowing `scale` on one instance.
const shadow = new V(5, 6);
shadow.scale = function (k) { return new W(k, k); };
out.push(chain(shadow, 50), chain(new V(5, 6), 50));
// An instance given another prototype.
const moved = new V(1, 2);
Object.setPrototypeOf(moved, W.prototype);
out.push(chain(moved, 50), chain(new V(1, 2), 50));
// A getter defined over `scale` on the prototype.
let calls = 0;
const savedScale = V.prototype.scale;
Object.defineProperty(V.prototype, "scale", {
  get() { calls++; return function (k) { return new V(this.y * k, this.x * k); }; },
  configurable: true,
});
out.push(chain(v, 50), calls);
Object.defineProperty(V.prototype, "scale", { value: savedScale, writable: true, configurable: true });
out.push(chain(v, 50), chain(new V2(1, 1), 50));
console.log(out.join("\n"));
"####;
