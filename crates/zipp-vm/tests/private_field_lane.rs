//! A compiled private FIELD read or write where it lies (`codegen::PrivLane`,
//! planned by `Vm::build_priv_lane_plan`): the brand baked from the site's
//! cache while the field's class declaration has been evaluated once, its
//! byte in `Vm::jit_private_class_flags` cleared by the second evaluation
//! before that evaluation's code runs. Numbers, strings and objects stored
//! (a heap value takes the helper); a declaration evaluated again after its
//! methods compiled, an old instance through the new methods and the reverse;
//! the second evaluation's static block running the shared compiled method on
//! an old instance; fields stamped onto plain, frozen, array and Proxy
//! objects by a return-override base, so positions differ by receiver; a
//! field read before its initializer ran. The expected lines are Node's
//! (v24), with the lane, without it, in the interpreter and with every
//! function compiled at once.

const SOURCE: &str = r##""use strict";
// Private FIELD reads and writes from hot compiled methods, with every way the baked brand,
// position or value kind can stop applying.
const out = [];
// 1. a hot class: numbers, strings and objects stored and read back
class P {
  #a = 1; #b = "s"; #c = null;
  step(i) {
    this.#a = (this.#a + i) | 0;
    this.#b = i & 1 ? "odd" : "even";
    this.#c = i & 3 ? { i } : null;
    return this.#a + this.#b.length + (this.#c ? this.#c.i : -1);
  }
  get a() { return this.#a; }
}
const p = new P();
let s = 0;
for (let i = 0; i < 20000; i++) s = (s + p.step(i)) | 0;
out.push("P " + s + " " + p.a);
// 2. a class declaration evaluated again AFTER its methods compiled hot: an old instance through
// the new evaluation's methods and a new instance through the old ones must both throw.
function mk(n) {
  return class C {
    #x = n;
    get() { return this.#x; }
    set(v) { this.#x = v; return this.#x; }
    static read(o) { return o.#x; }
  };
}
const C1 = mk(1);
const old = new C1();
let t = 0;
for (let i = 0; i < 20000; i++) t = (t + old.get() + old.set(i & 7) + C1.read(old)) | 0;
const C2 = mk(2);
const fresh = new C2();
const errs = [];
for (const [what, f] of [
  ["old via new get", () => C2.prototype.get.call(old)],
  ["new via old get", () => C1.prototype.get.call(fresh)],
  ["old via new set", () => C2.prototype.set.call(old, 5)],
  ["new via old read", () => C1.read(fresh)],
  ["new via new read", () => C2.read(fresh)],
  ["old via old read", () => C1.read(old)],
]) {
  try { errs.push(what + "=" + f()); } catch (e) { errs.push(what + "=" + e.constructor.name); }
}
out.push("C " + t + " " + errs.join(" "));
// 3. the second evaluation's static block runs the shared compiled method on an old instance
function mkS() {
  return class D {
    #x = 1;
    static get(o) { return o.#x; }
    static { if (globalThis.__old) { try { globalThis.__res = D.get(globalThis.__old); } catch (e) { globalThis.__res = e.constructor.name; } } }
  };
}
const D1 = mkS();
globalThis.__old = new D1();
let d = 0;
for (let i = 0; i < 20000; i++) d += D1.get(globalThis.__old);
mkS();
out.push("D " + d + " " + globalThis.__res);
// 4. a return-override base stamps the fields onto other objects, so positions differ by receiver
class Base { constructor(o) { return o; } }
class Stamp extends Base {
  #v = 0; #w = 0;
  static add(o) { new Stamp(o); return o; }
  static bump(o, k) { o.#w = (o.#w + k) | 0; return o.#v + o.#w; }
  static has(o) { return #w in o; }
}
class Other extends Base { #o1 = 7; static add(o) { new Other(o); return o; } }
const plain = Stamp.add({});
const pre = Other.add({}); Stamp.add(pre);          // Other's field first, then Stamp's
const frozen = Object.freeze(Stamp.add({}));         // private fields ignore freezing
const arr = Stamp.add([1, 2, 3]);
const prox = Stamp.add(new Proxy({}, {}));
let st = 0;
const targets = [plain, pre, frozen, arr, prox];
for (let i = 0; i < 25000; i++) st = (st + Stamp.bump(targets[i % 5], i & 3)) | 0;
let bad = "";
try { Stamp.bump({}, 1); } catch (e) { bad = e.constructor.name; }
out.push("S " + st + " " + bad + " " + targets.map((o) => Stamp.has(o)).join(","));
// 5. a field read before its initializer ran (an earlier initializer reads a later field)
class Early {
  #first = (() => { try { return this.#second; } catch (e) { return e.constructor.name; } })();
  #second = 2;
  get first() { return this.#first; }
}
let early = "";
for (let i = 0; i < 2000; i++) early = new Early().first;
out.push("E " + early);
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "P 2043553240 199990001\nC 209994 old via new get=TypeError new via old get=TypeError old via new set=TypeError new via old read=TypeError new via new read=2 old via old read=7\nD 20000 TypeError\nS 93762500 TypeError true,true,true,true,true\nE TypeError";

#[test]
fn private_field_lane_worker() {
    if std::env::var_os("ZIPP_PRIVATE_FIELD_LANE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn private_field_lane_matches_node() {
    if std::env::var_os("ZIPP_PRIVATE_FIELD_LANE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("lane-off", Some("ZIPP_NO_PRIV_LANE")),
        ("interpreter", Some("ZIPP_NOJIT")),
        ("compile-at-once", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["private_field_lane_worker", "--exact", "--nocapture"])
            .env("ZIPP_PRIVATE_FIELD_LANE_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_PRIV_LANE")
            .env_remove("ZIPP_JIT_THRESHOLD");
        if let Some(v) = var {
            command.env(v, "1");
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