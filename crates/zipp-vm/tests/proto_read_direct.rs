//! `Array.prototype.slice`, `String.prototype.at` and `C.prototype` on a
//! built-in constructor are read where they lie: a method of %Array.prototype%
//! or %String.prototype% itself through the boot-intrinsic proof an array or
//! string receiver's read takes, and a constructor's `prototype` from its own
//! data property when no override is recorded. The results must be Node's for
//! prototype identities, constructors with and without a `prototype`,
//! assignments to a non-writable `prototype` (sloppy and strict), plain
//! functions, own methods read repeatedly, `constructor`, @@iterator,
//! `length`, inherited and missing keys, computed keys, replaced, deleted,
//! redefined and accessor methods, added methods, and the bundler idioms
//! (`Array.prototype.slice.call(arguments)`,
//! `Object.prototype.hasOwnProperty.call`, `Function.prototype.call.call`).

const SOURCE: &str = r##"
(function () {
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }
  function t(v) { return typeof v === 'function' ? 'fn:' + v.name : String(v); }
  function k(s) { return s; }

  // Constructor `prototype` reads.
  log('identity', (Array.prototype === Object.getPrototypeOf([])) + ',' + (String.prototype === Object.getPrototypeOf(new String(''))) + ',' + (Object.prototype === Object.getPrototypeOf({})));
  log('ctors', [Map, Set, Promise, Date, RegExp, Error, TypeError, Symbol, Number, Boolean, Function, Uint8Array, ArrayBuffer, WeakMap].map(function (C) { return typeof C.prototype; }).join(','));
  log('no prototype', String(Proxy.prototype) + ',' + String(Math.prototype) + ',' + String(JSON.prototype) + ',' + String(Reflect.prototype));
  var before = Array.prototype;
  Array.prototype = {};
  log('assign ignored', Array.prototype === before);
  (function () { 'use strict'; try { Array.prototype = {}; log('strict assign', 'none'); } catch (e) { log('strict assign', e instanceof TypeError); } })();
  log('descriptor', JSON.stringify(Object.getOwnPropertyDescriptor(Array, 'prototype').writable));
  function F() {}
  log('plain fn prototype', typeof F.prototype + ',' + (F.prototype.constructor === F));
  F.prototype = 5;
  log('plain fn replaced', F.prototype);

  // Own methods of the prototypes.
  for (var i = 0; i < 3; i++) {
    log('arr methods ' + i, [Array.prototype.at, Array.prototype.slice, Array.prototype.push, Array.prototype.indexOf, Array.prototype.concat].map(t).join(','));
    log('str methods ' + i, [String.prototype.slice, String.prototype.at, String.prototype.replaceAll, String.prototype.charCodeAt].map(t).join(','));
  }
  log('constructor', (Array.prototype.constructor === Array) + ',' + (String.prototype.constructor === String));
  log('iterator', (Array.prototype[Symbol.iterator] === Array.prototype.values) + ',' + typeof String.prototype[Symbol.iterator]);
  log('length', Array.prototype.length + ',' + String.prototype.length);
  log('inherited', t(Array.prototype.hasOwnProperty) + ',' + t(String.prototype.toString) + ',' + t(Array.prototype.toLocaleString));
  log('missing', String(Array.prototype.nope) + ',' + String(String.prototype.nope));
  Object.prototype.extra = 'ext';
  log('from Object.prototype', Array.prototype.extra + ',' + String.prototype.extra);
  delete Object.prototype.extra;
  log('computed key', t(Array.prototype[k('at')]) + ',' + t(String.prototype[k('slice')]));

  // Replaced, deleted, and accessor methods.
  var origAt = Array.prototype.at;
  Array.prototype.at = function at2() { return 'mine'; };
  log('replaced at', t(Array.prototype.at) + ' ' + [1].at(0));
  Array.prototype.at = origAt;
  log('restored at', t(Array.prototype.at) + ' ' + [1].at(0));
  var sliceDesc = Object.getOwnPropertyDescriptor(String.prototype, 'slice');
  delete String.prototype.slice;
  log('deleted slice', String(String.prototype.slice));
  Object.defineProperty(String.prototype, 'slice', sliceDesc);
  log('redefined slice', t(String.prototype.slice));
  var gets = 0;
  var joinDesc = Object.getOwnPropertyDescriptor(Array.prototype, 'join');
  Object.defineProperty(Array.prototype, 'join', { configurable: true, get: function () { gets++; return function joinG() { return 'got'; }; } });
  log('accessor join', t(Array.prototype.join) + ' gets=' + gets + ' ' + String([1, 2]));
  Object.defineProperty(Array.prototype, 'join', joinDesc);
  log('restored join', t(Array.prototype.join) + ' ' + String([1, 2]));
  Array.prototype.custom = function custom() { return 'c'; };
  log('added method', t(Array.prototype.custom) + ' ' + [].custom());
  delete Array.prototype.custom;
  log('removed method', String(Array.prototype.custom));

  // The bundler idioms that read these.
  function args() { return Array.prototype.slice.call(arguments, 1); }
  log('slice.call(arguments)', args(1, 2, 3).join(','));
  log('hasOwnProperty.call', Object.prototype.hasOwnProperty.call({ a: 1 }, 'a') + ',' + Object.prototype.toString.call([]));
  log('fn proto call', Function.prototype.call.call(function () { return this.v; }, { v: 'cv' }));
  var arrp = Array.prototype;
  var sum = 0;
  for (var j = 0; j < 100; j++) sum += arrp.indexOf.call([5, 6, 7], 6);
  log('loop read', sum);

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
fn proto_read_direct_worker() {
    if std::env::var_os("ZIPP_PROTO_READ_DIRECT_WORKER").is_none() {
        return;
    }
    // The harness has printed `test proto_read_direct_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn proto_read_direct_match_node() {
    if std::env::var_os("ZIPP_PROTO_READ_DIRECT_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["proto_read_direct_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_PROTO_READ_DIRECT_WORKER", "1")
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
