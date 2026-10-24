//! `arr.pop()` on a plain dense array, while `pop` is the boot intrinsic,
//! removes the last element in the dispatch loop (`interp_array_pop_fast`),
//! beside the `push` lane. Every other receiver keeps the general path, so the
//! results must be Node's for: a stack popped past empty, interleaved pushes
//! and pops of objects, frozen, sealed and non-extensible arrays, a
//! non-writable `length` (the last element is deleted before the length Set
//! throws), a non-configurable or accessor last element, holes before the end
//! and in the last slot with and without an inherited index, sparse and
//! maximum-length arrays, an own, replaced or accessor `pop`, subclasses that
//! override or inherit it, null, custom and reset prototypes, named own
//! properties, a for-in over an array popped mid-loop, young objects popped
//! and kept, and strict mode.

const SOURCE: &str = r##"
(function () {
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }
  function err(e) { return e instanceof TypeError ? 'TypeError' : 'other:' + e; }
  function show(a) { return '[' + Array.prototype.map.call(a, String).join(',') + '] len=' + a.length; }

  // A stack, popped empty and past empty.
  var st = [];
  for (var i = 0; i < 5; i++) st.push(i * 2);
  var got = [];
  for (var j = 0; j < 7; j++) got.push(String(st.pop()));
  log('stack', got.join(',') + ' ' + show(st));

  // Interleaved push/pop with object and string elements.
  var cmds = [];
  var sum = 0;
  for (var n = 0; n < 50; n++) { cmds.push([n, 'x' + n]); cmds.push({ v: n }); sum += cmds.pop().v; sum += cmds.pop()[0]; }
  log('interleaved', sum + ' ' + cmds.length);

  // Frozen, sealed, non-extensible, non-writable length.
  var fz = Object.freeze([1, 2, 3]);
  try { log('frozen pop', fz.pop()); } catch (e) { log('frozen pop', err(e) + ' ' + show(fz)); }
  var se = Object.seal([1, 2, 3]);
  try { log('sealed pop', se.pop()); } catch (e) { log('sealed pop', err(e) + ' ' + show(se)); }
  var ne = Object.preventExtensions([1, 2, 3]);
  log('non-extensible pop', ne.pop() + ' ' + show(ne));
  var nw = [1, 2, 3];
  Object.defineProperty(nw, 'length', { writable: false });
  try { log('nonwritable length pop', nw.pop()); } catch (e) { log('nonwritable length pop', err(e) + ' ' + show(nw)); }
  var empNw = [];
  Object.defineProperty(empNw, 'length', { writable: false });
  try { log('empty nonwritable pop', empNw.pop()); } catch (e) { log('empty nonwritable pop', err(e)); }

  // A non-configurable last element.
  var nc = [1, 2, 3];
  Object.defineProperty(nc, 2, { value: 3, configurable: false });
  try { log('nonconfigurable last', nc.pop()); } catch (e) { log('nonconfigurable last', err(e) + ' ' + show(nc)); }

  // An accessor in the last slot runs its getter.
  var acc = [1, 2];
  Object.defineProperty(acc, 2, { get: function () { return 'got'; }, configurable: true });
  log('accessor last', acc.pop() + ' ' + show(acc));

  // Holes: in the last slot (with and without a prototype index), and before it.
  var h1 = [1, 2, , ];
  log('hole before end', show(h1) + ' -> ' + h1.pop() + ' ' + show(h1));
  var h2 = [1, , 3];
  log('hole middle', h2.pop() + ' ' + show(h2) + ' ' + (1 in h2));
  var h3 = [1, 2];
  h3.length = 4;
  log('length-extended', h3.pop() + ' ' + show(h3));
  Array.prototype[3] = 'proto3';
  var h4 = [1, 2];
  h4.length = 4;
  log('proto index under hole', h4.pop() + ' ' + show(h4));
  var h5 = [0, 1, 2, 3];
  log('own over proto index', h5.pop() + ' ' + show(h5) + ' ' + h5[3]);
  delete Array.prototype[3];

  // Sparse (virtual length) arrays.
  var sp = [];
  sp[100] = 'far';
  log('sparse pop', sp.pop() + ' ' + sp.length);
  log('sparse pop again', sp.pop() + ' ' + sp.length);
  var big = [1];
  big.length = 4294967295;
  log('max length pop', big.pop() + ' ' + big.length);

  // An own `pop`, a replaced prototype method, a getter in its place.
  var own = [1, 2, 3];
  own.pop = function () { return 'own'; };
  log('own pop', own.pop() + ' ' + show(own));
  delete own.pop;
  log('own deleted', own.pop() + ' ' + show(own));
  var origPop = Array.prototype.pop;
  Array.prototype.pop = function () { return 'replaced:' + this.length; };
  var r = [1, 2];
  log('replaced pop', r.pop() + ' ' + show(r));
  Array.prototype.pop = origPop;
  log('restored pop', r.pop() + ' ' + show(r));
  var desc = Object.getOwnPropertyDescriptor(Array.prototype, 'pop');
  Object.defineProperty(Array.prototype, 'pop', { configurable: true, get: function () { return function () { return 'getter'; }; } });
  log('getter pop', [5].pop());
  Object.defineProperty(Array.prototype, 'pop', desc);
  log('getter restored', [5].pop());

  // Subclasses and replaced prototypes.
  class Stack extends Array { pop() { return 'sub:' + super.pop(); } }
  var s1 = new Stack();
  s1.push(1, 2);
  log('subclass override', s1.pop() + ' ' + show(s1));
  class Plain extends Array {}
  var s2 = new Plain();
  s2.push(7, 8);
  log('subclass inherit', s2.pop() + ' ' + show(s2) + ' ' + (s2 instanceof Plain));
  var np = [1, 2];
  Object.setPrototypeOf(np, null);
  try { log('null proto pop', np.pop()); } catch (e) { log('null proto pop', err(e)); }
  var cp = [1, 2];
  Object.setPrototypeOf(cp, { pop: function () { return 'custom'; } });
  log('custom proto pop', cp.pop());
  var ap = [1, 2];
  Object.setPrototypeOf(ap, Array.prototype);
  log('reset proto pop', ap.pop() + ' ' + show(ap));

  // Named own properties and a for-in over a popped array.
  var named = [1, 2, 3];
  named.tag = 'x';
  log('named prop pop', named.pop() + ' ' + show(named) + ' ' + named.tag);
  var fi = [1, 2, 3, 4];
  var keys = [];
  for (var k in fi) { keys.push(k); if (k === '0') { fi.pop(); fi.pop(); } }
  log('for-in during pop', keys.join(','));

  // The result of an empty pop, and `length` after.
  var e1 = [];
  log('empty pop', e1.pop() + ' ' + e1.length + ' ' + (e1.pop() === undefined));

  // A popped value that is a young object kept elsewhere.
  var keep = [];
  var src = [];
  for (var q = 0; q < 200; q++) { src.push({ q: q }); if (q % 3 === 0) keep.push(src.pop()); }
  var total = 0;
  keep.forEach(function (o) { total += o.q; });
  log('kept popped', keep.length + ' ' + total + ' ' + src.length);

  // `this` stays the array when pop is called through a method on it.
  var holder = { a: [1, 2, 3], take: function () { return this.a.pop(); } };
  log('through a method', holder.take() + ' ' + holder.a.length);

  // Strict-mode frozen pop.
  (function () {
    'use strict';
    try { Object.freeze([1]).pop(); log('strict frozen', 'none'); } catch (e) { log('strict frozen', err(e)); }
  })();

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
fn array_pop_direct_worker() {
    if std::env::var_os("ZIPP_ARRAY_POP_DIRECT_WORKER").is_none() {
        return;
    }
    // The harness has printed `test array_pop_direct_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn array_pop_direct_match_node() {
    if std::env::var_os("ZIPP_ARRAY_POP_DIRECT_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["array_pop_direct_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_ARRAY_POP_DIRECT_WORKER", "1")
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
