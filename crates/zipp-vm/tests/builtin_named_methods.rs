//! A user method that shares a builtin's name (a queue's `push`/`pop`, a
//! cache's `get`/`has`/`set`/`clear`, `slice`, `substring`, `indexOf`,
//! `charCodeAt`, `toUpperCase`), called hot from compiled code with real
//! builtins at the same sites and at one site that sees both. The compiled
//! tiers choose a builtin lane by the name; when its helper declines, the
//! call takes the general method route instead of leaving compiled code.
//! The expected line is Node 24's.

#[test]
fn builtin_named_user_methods_run_as_node_runs_them() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(
        lines,
        [
            r#"15987316,21887,2813,343472,10,15987316,21887,2813,343472,10,15987316,21887,2813,343472,10,15987316,21887,2813,343472,10,7500"#
        ]
    );
}

const PROGRAM: &str = r####"
// User methods that share a builtin's name, called hot from compiled code,
// with real builtins at the same call sites.
class Box {
  constructor() { this.items = []; this.map = new Map(); this.log = 0; }
  push(v) { this.items.push(v * 2); this.log++; return this.items.length; }
  pop() { this.log++; return this.items.pop(); }
  get(k) { return this.map.get(k) ?? -1; }
  has(k) { return this.map.has(k); }
  set(k, v) { this.map.set(k, v + 1); return this; }
  clear() { this.map.clear(); this.log += 100; }
  slice(a, b) { return this.items.slice(a, b).length; }
  substring(a, b) { return "sub" + a + b; }
  indexOf(v) { return this.items.indexOf(v * 2); }
  charCodeAt(i) { return 1000 + i; }
  toUpperCase() { return "UP" + this.items.length; }
}
function work(o, n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    s += o.push(i);
    o.set(i & 7, i);
    s += o.get(i & 7) + (o.has(i & 3) ? 1 : 0);
    s += o.indexOf(i) + o.charCodeAt(i & 3) + o.slice(0, 2);
    s += o.substring(1, 2).length + o.toUpperCase().length;
    if ((i & 15) === 15) { o.pop(); o.clear(); }
  }
  return s;
}
const results = [];
for (let r = 0; r < 4; r++) {
  const b = new Box();
  results.push(work(b, 3000), b.log, b.items.length);
  // The same sites with real builtins.
  const arr = [1, 2, 3], m = new Map([[1, 2]]);
  let t = 0;
  for (let i = 0; i < 3000; i++) {
    t += arr.push(i) + (m.has(i & 1) ? 1 : 0) + "abc".charCodeAt(i % 3) + "abcdef".indexOf("c");
    if (arr.length > 10) arr.pop();
    t += String(m.get(1)).length + "xyz".substring(1, 2).length + "Q".toUpperCase().length;
  }
  results.push(t, arr.length);
}
// One site that sees both kinds.
function mixed(o) { return o.push(1); }
let mx = 0;
for (let i = 0; i < 5000; i++) mx += mixed(i % 2 ? new Box() : [i]);
results.push(mx);
console.log(results.join(","));
"####;
