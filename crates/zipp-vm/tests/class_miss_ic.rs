//! A class instance's read of a key that neither the instance, nor its class
//! chain's member tables, nor the ordinary chain past them has fills an
//! `IcEntry::ClassMiss` (the receiver's shape, the class, its version, the
//! class-prototype epoch and the chain's hops), whose hit yields `undefined`.
//! The results must be Node's as the key appears on the instance, the class
//! prototype, a parent prototype and Object.prototype, as a getter or a
//! setter-only accessor, as a method added later or a getter a subclass
//! declares; when the instance or the class prototype is given another
//! prototype and restored; at a polymorphic site; on frozen instances; when
//! the key is written after misses; and for a class that extends null.

const SOURCE: &str = r##"
(function () {
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }
  function show(v) { return typeof v === 'function' ? 'fn:' + v.name : typeof v === 'object' && v !== null ? 'obj' : String(v); }
  class Base { constructor(k) { this.kind = k; } baseM() { return 1; } }
  class Node extends Base { constructor(k, n) { super(k); this.n = n; } nodeM() {} }
  class Leaf extends Node {}
  // One site reading an absent key, warmed, then each way the key can appear.
  function rd(o) { return show(o.comments); }
  function many(o) { var r = ''; for (var i = 0; i < 6; i++) r += rd(o)[0]; return r; }
  var a = new Node('a', 1), b = new Node('b', 2), l = new Leaf('l', 3);
  log('absent', many(a) + ',' + many(b) + ',' + many(l));
  a.comments = ['own'];
  log('own on one', rd(a) + ',' + rd(b));
  delete a.comments;
  log('own deleted', rd(a));
  Node.prototype.comments = 'node-proto';
  log('on class prototype', rd(a) + ',' + rd(l) + ',' + rd(new Base('x')));
  delete Node.prototype.comments;
  log('removed', rd(a) + ',' + rd(l));
  Base.prototype.comments = 'base-proto';
  log('on parent prototype', rd(a) + ',' + rd(l));
  delete Base.prototype.comments;
  Object.prototype.comments = 'object-proto';
  log('on Object.prototype', rd(a) + ',' + rd(l));
  delete Object.prototype.comments;
  log('gone again', rd(a) + ',' + rd(l));
  Object.defineProperty(Base.prototype, 'comments', { configurable: true, get: function () { return 'getter:' + this.kind; } });
  log('getter on parent', rd(a) + ',' + rd(l));
  delete Base.prototype.comments;
  Object.defineProperty(Node.prototype, 'comments', { configurable: true, set: function (v) {} });
  log('setter-only', rd(a));
  delete Node.prototype.comments;
  log('after setter-only', rd(a));

  // Prototype changes: the instance's, the class prototype's.
  var c = new Node('c', 4);
  rd(c); rd(c);
  Object.setPrototypeOf(c, { comments: 'reproto' });
  log('instance reproto', rd(c) + ',' + rd(a));
  var saved = Object.getPrototypeOf(Node.prototype);
  Object.setPrototypeOf(Node.prototype, { comments: 'mid', baseM: function () {} });
  log('class proto reproto', rd(a) + ',' + rd(l));
  Object.setPrototypeOf(Node.prototype, saved);
  log('restored', rd(a) + ',' + rd(l));

  // A method added to the class prototype after the fact, and a getter declared in a class.
  var d = new Node('d', 5);
  rd(d); rd(d);
  Node.prototype.comments = function comments() {};
  log('method added', rd(d));
  delete Node.prototype.comments;
  class WithGetter extends Node { get comments() { return 'class-getter'; } }
  log('class getter', rd(new WithGetter('g', 6)) + ',' + rd(d));

  // Polymorphic site, frozen and extensible instances, writes after misses.
  var objs = [a, b, l, new Base('z'), { comments: 'plain' }, Object.create(null), new WithGetter('w', 7)];
  log('polymorphic', objs.map(rd).join(','));
  var f = Object.freeze(new Node('f', 8));
  log('frozen', many(f));
  var w = new Node('w', 9);
  for (var i = 0; i < 4; i++) { rd(w); }
  w.comments = 'written';
  log('written', rd(w));
  function setter(o, v) { o.comments = v; }
  var x1 = new Node('x1', 1), x2 = new Node('x2', 2);
  rd(x1); rd(x2);
  setter(x1, 'one'); setter(x2, 'two');
  log('set after miss', rd(x1) + ',' + rd(x2) + ',' + rd(a));
  class Nul extends null { constructor() { return Object.create(Nul.prototype); } }
  log('extends null', rd(new Nul()));

  out.forEach(function (l) { console.log(l); });
})();
"##;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn zipp_lines() -> Vec<String> {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.join("\n").lines().map(str::to_owned).collect()
}

#[test]
fn class_miss_ic_worker() {
    if std::env::var_os("ZIPP_CLASS_MISS_IC_WORKER").is_none() {
        return;
    }
    // The harness has printed `test class_miss_ic_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn class_miss_ic_match_node() {
    if std::env::var_os("ZIPP_CLASS_MISS_IC_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["class_miss_ic_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_CLASS_MISS_IC_WORKER", "1")
        .env("ZIPP_NOJIT", "1")
        .output()
        .expect("spawn the worker");
    assert!(out.status.success(), "worker failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("LINE ").map(str::to_owned))
        .collect();
    assert_eq!(got, want, "interpreter-only run");
}
