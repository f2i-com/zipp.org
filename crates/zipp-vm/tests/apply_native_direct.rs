//! `native.apply(t, list)` while `apply` is the pristine
//! `%Function.prototype.apply%`, with no other realm in existence and a list
//! that reads without running code, runs the native directly, as the `call`
//! lane does (CallMethod, and the spec-order GetProp; CallWithThis pair). The
//! results must be Node's for Math, String, Array, Object, JSON, Number,
//! parseInt and Reflect natives, constructors called without `new`, `call`
//! and `apply` applied to themselves, nullish lists and `arguments` (mapped,
//! strict, shortened); declined lists (holes, array-likes, primitives, a
//! getter element, sparse arrays); throwing natives and bad receivers; a
//! replaced `apply`, an own `apply` on a bound function, bound natives; a
//! bundler's method helper forwarding its rest array; a 5,000-element list;
//! and captured callees.

const SOURCE: &str = r##"
(function () {
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }
  function err(e) { return e instanceof TypeError ? 'TypeError' : e instanceof RangeError ? 'RangeError' : 'other:' + e; }
  function show(a) { return Array.isArray(a) ? '[' + a.map(String).join(',') + ']' : String(a); }

  // Natives through `.apply` with arrays, nullish lists and `arguments`.
  log('Math.max', Math.max.apply(null, [3, 9, 2]) + ',' + Math.max.apply(Math, []) + ',' + Math.max.apply(null, null));
  log('Math.min undefined list', Math.min.apply(null, undefined));
  log('fromCharCode', String.fromCharCode.apply(null, [104, 105]));
  var a = [1, 2];
  log('push', Array.prototype.push.apply(a, [3, 4, 5]) + ' ' + show(a));
  log('concat', show([].concat.apply([], [[1], [2, [3]], 4])));
  log('hasOwn', Object.prototype.hasOwnProperty.apply({ k: 1 }, ['k']) + ',' + Object.prototype.hasOwnProperty.apply({}, ['k']));
  log('slice', show(Array.prototype.slice.apply([1, 2, 3, 4], [1, 3])));
  log('join this', Array.prototype.join.apply(['a', 'b'], ['-']));
  log('toUpperCase', String.prototype.toUpperCase.apply('abc', []));
  log('replaceAll', String.prototype.replaceAll.apply('a.b.c', ['.', '/']));
  log('at', Array.prototype.at.apply([1, 2, 3], [-1]));
  log('call via apply', Function.prototype.call.apply(function (x) { return this.v + x; }, [{ v: 10 }, 5]));
  log('apply via apply', Function.prototype.apply.apply(Math.max, [null, [4, 8]]));
  log('Reflect.apply', Reflect.apply.apply(null, [Math.min, null, [7, 3]]));
  log('Array ctor', show(Array.apply(null, [3])) + ' len ' + Array.apply(null, [3]).length + ',' + show(Array.apply(null, [1, 2])));
  log('Object ctor', typeof Object.apply(null, []));
  log('Number', Number.apply(null, ['42']) + ',' + Number.apply(null, []));
  log('parseInt', parseInt.apply(null, ['ff', 16]));
  log('isArray', Array.isArray.apply(null, [[]]));
  log('keys', show(Object.keys.apply(Object, [{ x: 1, y: 2 }])));
  log('JSON', JSON.stringify.apply(JSON, [{ a: [1] }]));

  // `arguments` as the list.
  function fwd() { return Math.max.apply(null, arguments); }
  log('arguments list', fwd(1, 7, 3) + ',' + fwd());
  function fwdMapped(x, y) { x = 100; return Math.max.apply(null, arguments); }
  log('mapped arguments', fwdMapped(1, 2));
  function fwdStrict(x) { 'use strict'; x = 100; return Math.max.apply(null, arguments); }
  log('strict arguments', fwdStrict(1, 2));
  function fwdLen() { arguments.length = 1; return Math.max.apply(null, arguments); }
  log('shortened arguments', fwdLen(5, 9));

  // Lists the fast path declines: holes, array-likes, a non-object, a getter.
  log('holey', Math.max.apply(null, [1, , 3]));
  log('array-like', Math.max.apply(null, { length: 2, 0: 5, 1: 6 }));
  try { Math.max.apply(null, 5); log('number list', 'none'); } catch (e) { log('number list', err(e)); }
  try { Math.max.apply(null, 'ab'); log('string list', 'none'); } catch (e) { log('string list', err(e)); }
  var getterList = [1, 2];
  Object.defineProperty(getterList, 1, { get: function () { return 50; } });
  log('getter element', Math.max.apply(null, getterList));
  var sparse = [];
  sparse[3] = 4;
  log('sparse', Math.max.apply(null, sparse));

  // Throwing natives and bad receivers.
  try { Array.prototype.at.apply(null, [0]); log('at on null', 'none'); } catch (e) { log('at on null', err(e)); }
  try { Map.prototype.get.apply({}, ['k']); log('map.get on object', 'none'); } catch (e) { log('map.get on object', err(e)); }
  try { new Array(-1); } catch (e) { log('array ctor throw', err(e)); }
  try { Array.apply(null, [-1]); log('array apply throw', 'none'); } catch (e) { log('array apply throw', err(e)); }
  try { Symbol.apply(null, ['s']).toString(); log('symbol', Symbol.apply(null, ['s']).toString()); } catch (e) { log('symbol', err(e)); }
  try { Map.apply(null, []); log('Map without new', 'none'); } catch (e) { log('Map without new', err(e)); }

  // `apply` replaced, shadowed, or the receiver changed.
  var nativeMax = Math.max;
  var origApply = Function.prototype.apply;
  Function.prototype.apply = function (t, l) { return 'replaced:' + (l ? l.length : 0); };
  log('replaced apply', nativeMax.apply(null, [1, 2]));
  Function.prototype.apply = origApply;
  log('restored apply', nativeMax.apply(null, [1, 2]));
  var ownApply = Math.min.bind(null);
  ownApply.apply = function () { return 'own'; };
  log('own apply on bound', ownApply.apply(null, [1]));
  log('bound native apply', Math.min.bind(null, 0).apply(null, [5, -2]));

  // The bundler helper shape: a captured method value applied to its
  // receiver with a rest array.
  var X = function (name, poly) { return function (skip, obj) { var rest = Array.prototype.slice.call(arguments, 2); return skip | 1 && obj == null ? void 0 : (poly.call(obj) || obj[name]).apply(obj, rest); }; };
  var at = X('at', function () { return undefined; });
  var sum = 0;
  var arr = [10, 20, 30];
  for (var i = 0; i < 200; i++) sum += at(false, arr, -1 - (i % 3));
  log('helper loop', sum);
  var rep = X('replaceAll', function () { return undefined; });
  log('helper replaceAll', rep(false, 'x-y-z', '-', '+'));

  // A native applied with a large list.
  var big = [];
  for (var j = 0; j < 5000; j++) big.push(j % 97);
  log('big list', Math.max.apply(null, big) + ',' + String.fromCharCode.apply(null, big.slice(65, 70)).length);

  // Captured in spec order: `f` an upvalue, its Get before the arguments.
  var g = Math.max;
  function cap(list) { return g.apply(null, list); }
  log('captured', cap([1, 4, 2]) + ',' + cap(null));
  var nat = String.prototype.slice;
  function capThis(s, list) { return nat.apply(s, list); }
  log('captured with this', capThis('hello', [1, 3]));

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
fn apply_native_direct_worker() {
    if std::env::var_os("ZIPP_APPLY_NATIVE_DIRECT_WORKER").is_none() {
        return;
    }
    // The harness has printed `test apply_native_direct_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn apply_native_direct_match_node() {
    if std::env::var_os("ZIPP_APPLY_NATIVE_DIRECT_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["apply_native_direct_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_APPLY_NATIVE_DIRECT_WORKER", "1")
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
