//! Reads on receivers the property caches cannot serve (a string, an Array,
//! a RegExp; `.prototype` of a function or class) are answered where they lie
//! before the general member walk: an Array's named side-table data
//! (NodeArray's `pos`, `arguments.length`), a match result's record, and
//! past the receiver's own properties a walk of ordinary prototype maps to
//! the first data property or the chain's end. The results must be Node's
//! for absent keys on strings, ropes and arrays; data added to and deleted
//! from Object.prototype, String.prototype and Array.prototype; getters there
//! (sloppy and strict receivers); String.prototype's prototype set to null;
//! named, updated and accessor array properties; arguments `length` read,
//! set, deleted, redefined and in strict mode; match results and template
//! strings; Array subclasses and null or custom prototypes; `re.test` read
//! as a value, other RegExp state, own properties, custom prototypes, a
//! replaced method and a getter on RegExp.prototype; `.prototype` of
//! functions, arrows, async functions, generators, classes, subclasses, bound
//! functions and natives, assigned (object and primitive) and defined.

const SOURCE: &str = r##"
(function () {
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }
  function show(v) { return typeof v === 'function' ? 'fn:' + v.name : typeof v === 'object' && v !== null ? (Array.isArray(v) ? 'arr' : 'obj') : String(v); }
  function k(s) { return s; }
  // Reads in a loop, so each site warms up and then sees the change.
  function rd(o, n) { var r = []; for (var i = 0; i < n; i++) r.push(show(o[k('type')])); return r.join(','); }
  function rds(o) { return show(o.type) + '|' + show(o.peek) + '|' + show(o.toString === Object.prototype.toString); }

  // Absent and inherited keys on strings and arrays.
  var s = 'abc', a = [1, 2];
  log('absent', rds(s) + ' ' + rds(a));
  for (var i = 0; i < 3; i++) log('absent loop ' + i, show(s.type) + show(a.type) + show(s.nope) + show(a.nope));
  Object.prototype.type = 'objproto';
  log('object proto data', show(s.type) + ',' + show(a.type) + ',' + show((5).type));
  String.prototype.type = 'strproto';
  Array.prototype.type = 'arrproto';
  log('own proto data', show(s.type) + ',' + show(a.type));
  delete String.prototype.type;
  delete Array.prototype.type;
  log('deleted', show(s.type) + ',' + show(a.type));
  delete Object.prototype.type;
  log('all gone', show(s.type) + ',' + show(a.type));
  Object.defineProperty(Object.prototype, 'type', { configurable: true, get: function () { return 'getter:' + typeof this + ':' + (Array.isArray(this)); } });
  log('getter', show(s.type) + ',' + show(a.type));
  (function () { 'use strict'; Object.defineProperty(String.prototype, 'kind', { configurable: true, get: function () { return 'strict:' + typeof this; } }); })();
  log('strict getter', show(s.kind));
  delete String.prototype.kind;
  delete Object.prototype.type;
  var sp = Object.getPrototypeOf(String.prototype);
  Object.setPrototypeOf(String.prototype, null);
  log('null String.prototype proto', show(s.hasOwnProperty) + ',' + show(s.slice));
  Object.setPrototypeOf(String.prototype, sp);
  log('restored', show(s.hasOwnProperty));
  var rope = ''; for (var j = 0; j < 20; j++) rope += 'x';
  log('rope', show(rope.type) + ',' + show(rope.length));

  // Arrays: named own properties, arguments, match results, templates, subclasses.
  var na = [1, 2, 3];
  na.pos = 5; na.end = 9; na.hasTrailingComma = false;
  Object.defineProperty(na, 'acc', { get: function () { return 'acc:' + this.length; } });
  log('named', na.pos + ',' + na.end + ',' + na.hasTrailingComma + ',' + na.acc + ',' + show(na.missing));
  na.pos = 6;
  log('named updated', na.pos);
  function args() { return arguments.length + ',' + show(arguments.callee === args) + ',' + show(arguments.type); }
  log('arguments', args(1, 2, 3) + ' ' + args());
  function argsLen() { arguments.length = 7; return arguments.length; }
  log('arguments length set', argsLen(1));
  function argsDel() { delete arguments.length; return show(arguments.length); }
  log('arguments length deleted', argsDel(1, 2));
  function argsAcc() { Object.defineProperty(arguments, 'length', { get: function () { return 42; } }); return arguments.length; }
  log('arguments length accessor', argsAcc(1));
  function argsStrict() { 'use strict'; return arguments.length; }
  log('strict arguments', argsStrict(1, 2));
  var m = /(?<y>\d+)-(\d+)/.exec('x 12-34');
  log('match result', m.index + ',' + m.input + ',' + m.groups.y + ',' + show(m.type));
  function tag(strs) { return strs.raw[0] + ',' + show(strs.type); }
  log('template', tag`a\n`);
  class MyArr extends Array { get type() { return 'sub'; } }
  var ma = MyArr.from([1]);
  log('subclass', show(ma.type) + ',' + show(ma.peek));
  var nullArr = [1];
  Object.setPrototypeOf(nullArr, null);
  log('null proto array', show(nullArr.type) + ',' + show(nullArr.push));
  var objProtoArr = [1];
  Object.setPrototypeOf(objProtoArr, { type: 'custom' });
  log('custom proto array', show(objProtoArr.type));

  // RegExp methods read as values, and RegExp own state.
  var re = /a+/g;
  var tests = [];
  for (var q = 0; q < 3; q++) { var t = re.test; tests.push(show(t) + (t === RegExp.prototype.test)); }
  log('re.test value', tests.join(','));
  log('re other', show(re.exec) + ',' + show(re.type) + ',' + re.lastIndex + ',' + re.global + ',' + re.source + ',' + re.flags);
  re.custom = 'own';
  log('re own', show(re.custom) + ',' + show(re.test));
  var re2 = /b/;
  Object.setPrototypeOf(re2, { test: function myTest() {}, __proto__: RegExp.prototype });
  log('re custom proto', show(re2.test) + ',' + show(re2.exec));
  var origTest = RegExp.prototype.test;
  RegExp.prototype.test = function replacedTest() {};
  log('re replaced', show(/c/.test));
  RegExp.prototype.test = origTest;
  Object.defineProperty(RegExp.prototype, 'probe', { configurable: true, get: function () { return 'probe:' + this.source; } });
  log('re getter', show(/zz/.probe));
  delete RegExp.prototype.probe;

  // `.prototype` of functions and classes.
  function F() {}
  var arrow = () => 1;
  async function af() {}
  function* gf() {}
  class C {}
  class D extends C {}
  var bound = F.bind(null);
  log('prototype', [F, arrow, af, gf, C, D, bound, Math.max].map(function (f) { return show(f.prototype); }).join(','));
  log('prototype identity', (F.prototype === F.prototype) + ',' + (new F() instanceof F) + ',' + (Object.getPrototypeOf(D.prototype) === C.prototype) + ',' + (F.prototype.constructor === F));
  F.prototype = { mine: 1 };
  log('assigned', F.prototype.mine + ',' + (new F().mine));
  F.prototype = 7;
  log('assigned primitive', F.prototype + ',' + (Object.getPrototypeOf(new F()) === Object.prototype));
  function G() {}
  Object.defineProperty(G, 'prototype', { value: { g: 2 } });
  log('defined', G.prototype.g);
  var protos = 0;
  for (var z = 0; z < 50; z++) if (C.prototype && F.prototype) protos++;
  log('loop', protos);

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
fn shapeless_read_lanes_worker() {
    if std::env::var_os("ZIPP_SHAPELESS_READ_LANES_WORKER").is_none() {
        return;
    }
    // The harness has printed `test shapeless_read_lanes_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn shapeless_read_lanes_match_node() {
    if std::env::var_os("ZIPP_SHAPELESS_READ_LANES_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["shapeless_read_lanes_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_SHAPELESS_READ_LANES_WORKER", "1")
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
