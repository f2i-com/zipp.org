//! A member call on a Map or Set whose argument is not order-transparent
//! (`set.has(keys[i])`) captures its callee first (`GetProp; CallWithThis`).
//! When the captured callee is a Map or Set prototype native and the receiver
//! is a collection of that kind, the call runs the native directly. The
//! results must be what the spec's Call of the captured value gives: hits,
//! misses, -0 and NaN keys, `add`/`set` returning the receiver, `forEach` with
//! a `this` argument and a throwing callback, iterators and set algebra; a
//! prototype method replaced while the argument runs (the call keeps the one
//! it captured), own methods, subclasses that override or inherit, borrowed
//! methods on the wrong kind of receiver (a Map, a Set, a WeakSet, a plain
//! object, an object inheriting from `Set.prototype`, a Proxy) that throw
//! TypeError, an accessor in place of the method, frozen collections, growth
//! during `forEach`, WeakMap and WeakSet, and missing or extra arguments. The
//! expected lines are Node's.

const SOURCE: &str = r##"
(function () {
  'use strict';
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }
  function show(v) {
    if (v instanceof Set) return 'Set{' + Array.from(v).join(',') + '}';
    if (v instanceof Map) return 'Map{' + Array.from(v, function (e) { return e[0] + '=' + e[1]; }).join(',') + '}';
    if (typeof v === 'object' && v !== null) return 'obj';
    return String(v);
  }
  function err(e) { return e instanceof TypeError ? 'TypeError' : 'other:' + e; }
  // Each argument below is a call, so the member call captures its callee
  // before the argument runs (GetProp; CallWithThis).
  var ks = ['a', 'zz', 'line', 'q', -0, NaN, 0];
  function k(i) { return ks[i]; }

  var s = new Set(['a', 'b', 'line', NaN, 0]);
  var hits = 0;
  for (var i = 0; i < 70; i++) if (s.has(k(i % 7))) hits++;
  log('set.has loop', hits);
  log('set.add returns set', s.add(k(3)) === s);
  log('set after add', show(s));
  log('set.delete hit', s.delete(k(0)));
  log('set.delete miss', s.delete(k(1)));
  log('set.has -0', s.has(k(4)));
  log('set.has NaN', s.has(k(5)));
  log('set.size', s.size);

  var m = new Map([['a', 1], ['line', 2]]);
  log('map.get hit', m.get(k(0)));
  log('map.get miss', m.get(k(1)));
  log('map.set returns map', m.set(k(4), 'zero') === m);
  log('map -0 key', Object.is(Array.from(m.keys())[2], -0));
  log('map.get +0', m.get(k(6)));
  log('map.has', m.has(k(2)) + ',' + m.has(k(3)));
  log('map.delete', m.delete(k(2)) + ',' + m.delete(k(2)));
  log('map now', show(m));

  // forEach through the captured lane, a throwing callback, `this` arg.
  var seen = [];
  s.forEach(function (v) { seen.push(v + ':' + this.tag); }, (function () { return { tag: 't' }; })());
  log('set.forEach', seen.join(' '));
  try { m.forEach((function () { return function () { throw new RangeError('cb'); }; })()); log('forEach throw', 'none'); }
  catch (e) { log('forEach throw', e instanceof RangeError ? 'RangeError' : 'other'); }

  // Iterators and set algebra.
  log('set.keys', Array.from(s.keys(k(0))).join(','));
  log('map.entries', Array.from(m.entries(k(0)), function (e) { return e.join('='); }).join(','));
  log('union', show(s.union((function () { return new Set(['u']); })())));
  log('isSubsetOf', new Set(['a']).isSubsetOf((function () { return new Set(['a', 'b']); })()));

  // The argument replaces the prototype method: the call already captured
  // the original, the next call sees the replacement.
  var origHas = Set.prototype.has;
  log('replaced in arg', s.has((Set.prototype.has = function (x) { return 'replaced:' + x; }, k(3))));
  log('replaced next', s.has(k(3)));
  Set.prototype.has = origHas;
  log('restored', s.has(k(3)));

  // An own method on the receiver wins.
  var s2 = new Set(['a']);
  s2.has = function (x) { return 'own:' + x; };
  log('own has', s2.has(k(0)));
  delete s2.has;
  log('own deleted', s2.has(k(0)));

  // Subclasses: an override, an inherited native, super calls.
  class S extends Set { has(x) { return 'sub:' + super.has(x); } }
  var s3 = new S(['a']);
  log('subclass override', s3.has(k(0)) + ',' + s3.has(k(1)));
  class S2 extends Set {}
  var s4 = new S2(['q']);
  log('subclass inherit', s4.has(k(3)) + ',' + (s4.add(k(0)) === s4) + ',' + show(s4));
  class M2 extends Map { get(x) { return 'mget:' + super.get(x); } }
  var m2 = new M2([['a', 7]]);
  log('map subclass', m2.get(k(0)) + ',' + m2.has(k(0)));

  // Wrong receivers: a Set method on a Map, a Map method on a Set, an
  // object inheriting from Set.prototype, a Proxy of a Set, a WeakSet.
  var mm = new Map([['a', 1]]);
  mm.has2 = Set.prototype.has;
  try { log('set.has on map', mm.has2(k(0))); } catch (e) { log('set.has on map', err(e)); }
  var ss = new Set(['a']);
  ss.get = Map.prototype.get;
  try { log('map.get on set', ss.get(k(0))); } catch (e) { log('map.get on set', err(e)); }
  var fake = Object.create(Set.prototype);
  try { log('fake set', fake.has(k(0))); } catch (e) { log('fake set', err(e)); }
  var px = new Proxy(new Set(['a']), {});
  try { log('proxy set', px.has(k(0))); } catch (e) { log('proxy set', err(e)); }
  var ws = new WeakSet();
  ws.has2 = Set.prototype.has;
  try { log('set.has on weakset', ws.has2(k(0))); } catch (e) { log('set.has on weakset', err(e)); }
  var mapAdd = new Map();
  mapAdd.add = Set.prototype.add;
  try { log('set.add on map', mapAdd.add(k(0))); } catch (e) { log('set.add on map', err(e)); }
  log('map untouched', show(mapAdd));

  // A Map method borrowed onto a different Map receiver is fine.
  var other = new Map([['zz', 'other']]);
  var host = { get: Map.prototype.get };
  try { log('map.get on plain', host.get(k(1))); } catch (e) { log('map.get on plain', err(e)); }
  other.look = Map.prototype.get;
  log('borrowed onto map', other.look(k(1)));

  // An accessor replacing the prototype method.
  var desc = Object.getOwnPropertyDescriptor(Set.prototype, 'has');
  Object.defineProperty(Set.prototype, 'has', { configurable: true, get: function () { return function (x) { return 'getter:' + x; }; } });
  log('accessor has', s.has(k(0)));
  Object.defineProperty(Set.prototype, 'has', desc);
  log('accessor restored', s.has(k(2)));

  // Frozen and sealed collections still mutate their internal data.
  var fz = Object.freeze(new Set());
  fz.add(k(0));
  log('frozen add', show(fz));
  var fm = Object.freeze(new Map());
  fm.set(k(0), k(3));
  log('frozen map set', show(fm));

  // Mutation during forEach.
  var grow = new Set([1, 2]);
  var visited = [];
  grow.forEach(function (v) { visited.push(v); if (v < 4) grow.add(v + 2); });
  log('grow forEach', visited.join(','));

  // WeakMap / WeakSet through the captured lane.
  var key = {};
  var wm = new WeakMap();
  function kk() { return key; }
  log('weakmap set', wm.set(kk(), 5) === wm);
  log('weakmap get', wm.get(kk()) + ',' + wm.has(kk()) + ',' + wm.delete(kk()) + ',' + wm.has(kk()));
  var ws2 = new WeakSet();
  log('weakset', (ws2.add(kk()) === ws2) + ',' + ws2.has(kk()));
  try { wm.set(k(0), 1); log('weakmap prim key', 'none'); } catch (e) { log('weakmap prim key', err(e)); }

  // Calling with too few or extra arguments.
  log('has no arg', new Set([undefined]).has((function () {})()));
  log('get extra args', m.get(k(0), k(1), k(2)));

  // The method's result feeding a hot loop of both kinds.
  var counts = new Map();
  for (var j = 0; j < 40; j++) {
    var key2 = k(j % 4);
    counts.set(key2, (counts.get(k(j % 4)) || 0) + 1);
  }
  log('counts', show(counts));

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
fn collection_captured_call_worker() {
    if std::env::var_os("ZIPP_COLLECTION_CAPTURED_CALL_WORKER").is_none() {
        return;
    }
    // The harness has printed `test collection_captured_call_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn collection_captured_call_match_node() {
    if std::env::var_os("ZIPP_COLLECTION_CAPTURED_CALL_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["collection_captured_call_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_COLLECTION_CAPTURED_CALL_WORKER", "1")
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
