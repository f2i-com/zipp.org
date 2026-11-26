//! A user method that shares a built-in method's name.
//!
//! Compiled code picks a call's fast lane by the method's NAME: `get` and
//! `has` try a Map or Set, `add` a Set, `push` and `indexOf` an Array or a
//! String, and so on. What such a lane declines is, as often as not, a user
//! method of that name (a cache's `get`, a list's `add`). Its decline used
//! to land on the helper route, a call into the runtime for every call:
//! `acc.add(v)` took 40 ns where the same body named `bump` took 6. It
//! lands on the user-method lanes now, and these programs hold the two
//! kinds of receiver to one site: the built-in's own (a Map, a Set, an
//! Array, a String), a class's, a subclass's and a plain object's, in
//! rotation, and a method replaced, redefined as an accessor and moved to
//! a prototype while its site is hot.
//!
//! The expected lines are Node 24's.

const MIXED: &str = r#"
class Bag {
  constructor() { this.n = 0; this.items = []; this.keys = {}; }
  add(v) { this.n += v; return this; }
  push(v) { this.items[this.items.length] = v; return this.items.length * 10; }
  get(k) { return "bag:" + k; }
  has(k) { return k === "yes"; }
  set(k, v) { this.keys[k] = v; return this; }
  delete(k) { const had = k in this.keys; delete this.keys[k]; return had ? "gone" : "none"; }
  clear() { this.keys = {}; return "cleared"; }
  indexOf(v) { return v * 2; }
  slice(a, b) { return "slice(" + a + "," + b + ")"; }
  substring(a, b) { return "sub(" + a + "," + b + ")"; }
  charCodeAt(i) { return -i; }
  at(i) { return "at" + i; }
  charAt(i) { return "ch" + i; }
  toLowerCase() { return "LOWER"; }
  toUpperCase() { return "upper"; }
  match(r) { return "match:" + r; }
  pop() { return "popped"; }
  includes(v) { return v === 1; }
  join(s) { return "j" + s; }
  map(f) { return f(this.n); }
}
class SubBag extends Bag { add(v) { this.n -= v; return this; } get(k) { return "sub:" + k; } }
const plain = {
  add(v) { return "plain+" + v; }, get(k) { return "plain:" + k; }, has(k) { return false; }, push(v) { return "p" + v; },
  indexOf(v) { return "pi" + v; }, slice(a, b) { return [a, b].join("~"); }, toLowerCase() { return 7; },
};
function mixAdd(recvs, n) { let out = 0; for (let i = 0; i < n; i++) { const r = recvs[i % recvs.length].add(i & 3); out += typeof r === "object" ? 1 : String(r).length; } return out; }
function mixGet(recvs, n) { let out = ""; for (let i = 0; i < n; i++) { const r = recvs[i % recvs.length].get("k" + (i & 1)); if (i < 8) out += r + ";"; } return out; }
function mixHas(recvs, n) { let c = 0; for (let i = 0; i < n; i++) if (recvs[i % recvs.length].has(i & 1 ? "yes" : "k1")) c++; return c; }
function mixPush(recvs, n) { let s = 0; for (let i = 0; i < n; i++) { const r = recvs[i % recvs.length].push(i); s += typeof r === "number" ? r : r.length; } return s; }
function mixIndexOf(recvs, n) { let s = ""; for (let i = 0; i < n; i++) { const r = recvs[i % recvs.length].indexOf(i % 5); if (i < 8) s += r + ","; } return s; }
function mixSlice(recvs, n) { let s = ""; for (let i = 0; i < n; i++) { const r = recvs[i % recvs.length].slice(1, 3); if (i < 8) s += String(r) + "|"; } return s; }
function mixCase(recvs, n) { let s = ""; for (let i = 0; i < n; i++) { const r = recvs[i % recvs.length].toLowerCase(); if (i < 6) s += r + "|"; } return s; }
function one(b, n) { let s = 0; for (let i = 0; i < n; i++) { b.add(1); s += b.has("yes") ? 1 : 0; s += b.get("x").length; s += b.push(i) > 0 ? 1 : 0; s += b.indexOf(2); s += b.charCodeAt(1); } return s + ":" + b.n + ":" + b.items.length; }
function rest(b, n) { let s = ""; for (let i = 0; i < n; i++) { const t = b.set("a", i).delete("a") + b.clear() + b.slice(1, 2) + b.substring(3, 4) + b.at(1) + b.charAt(2) + b.toUpperCase() + b.match("m") + b.pop() + b.includes(1) + b.join("-") + b.map((x) => x + 1); if (i === n - 1) s = t; } return s; }
const N = 6000;
const bag = new Bag(), sub = new SubBag(), set = new Set(), map = new Map([["k0", "map0"], ["k1", "map1"]]), arr = [3, 1, 4, 1, 5];
console.log(mixAdd([bag, sub, set, plain], N), bag.n, sub.n, set.size);
console.log(mixGet([bag, map, sub, plain], N));
console.log(mixHas([bag, map, new Set(["yes"]), plain, new WeakSet()], N));
console.log(mixPush([bag, [], plain], N), bag.items.length);
console.log(mixIndexOf([bag, "a1b2c3d4", arr, plain], N));
console.log(mixSlice([bag, "abcdef", arr, plain], N));
console.log(mixCase([bag, "MiXeD", plain], N));
console.log(one(new Bag(), N));
console.log(rest(new Bag(), N));
// A method replaced, deleted and redefined as an accessor while the site is hot.
function hot(o, n) { let s = 0; for (let i = 0; i < n; i++) s += o.get(i); return s; }
const live = { get(k) { return 1; } };
let acc = hot(live, N);
live.get = function (k) { return 2; };
acc += hot(live, N);
Object.defineProperty(live, "get", { get() { return (k) => 3; }, configurable: true });
acc += hot(live, N);
delete live.get;
Object.setPrototypeOf(live, { get(k) { return 4; } });
acc += hot(live, N);
console.log(acc);
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn builtin_named_methods_answer_as_node() {
    assert_eq!(
        run_ok(MIXED),
        [
            "15000 0 -1500 1",
            "bag:k0;map1;sub:k0;plain:k1;bag:k0;map1;sub:k0;plain:k1;",
            "1800",
            "22020631 2000",
            "0,1,-1,pi3,8,-1,1,pi2,",
            "slice(1,3)|bc|1,4|1~3|slice(1,3)|bc|1,4|1~3|",
            "LOWER|mixed|7|LOWER|mixed|7|",
            "60000:6000:6000",
            "goneclearedslice(1,2)sub(3,4)at1ch2uppermatch:mpoppedtruej-1",
            "60000",
        ]
    );
}

/// The same answers with the JIT compiling at once and with no native code.
#[test]
fn builtin_named_methods_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("builtin_named_methods_answer")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:
{stdout}
{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
