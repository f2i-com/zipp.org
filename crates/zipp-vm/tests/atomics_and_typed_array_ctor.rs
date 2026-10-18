//! Three conformance fixes of 18 October 2026, checked in every profile:
//!
//! - GitHub #59: the writing Atomics operations revalidate the access after
//!   converting their value(s) (RevalidateAtomicAccess): a conversion that
//!   shrinks a resizable buffer makes a length-tracking view's index a
//!   RangeError, and a fixed-length view that is now out of bounds a
//!   TypeError. The value is still converted when the index conversion was the
//!   one that shrank the buffer. (Node 24 throws a TypeError for the RangeError
//!   rows; these follow the specification's RevalidateAtomicAccess step 5.
//!   An element the shrunk buffer cuts in two is refused too, where the
//!   specification's byte-index test would let the access run past the end.)
//! - GitHub #60: `%TypedArray%` throws from a subclass's `super()`, before
//!   newTarget's prototype is read, as it does from `new`.
//! - GitHub #61: SharedArrayBuffer and Atomics exist in the safe-sandbox
//!   profile too (agent-local bytes there), with the same observable results
//!   for one agent.

const SOURCE: &str = r##"
var out = [];
function t(label, f) {
  var r;
  try { r = String(f()); } catch (e) { r = "throws " + e.constructor.name; }
  out.push(label + " => " + r);
}
["store", "add", "sub", "and", "or", "xor", "exchange", "compareExchange"].forEach(function (op) {
  t("#59 " + op, function () {
    var rab = new ArrayBuffer(16, { maxByteLength: 64 });
    var i32 = new Int32Array(rab);
    var shrink = { valueOf: function () { rab.resize(0); return 7; } };
    if (op === "compareExchange") Atomics.compareExchange(i32, 2, 0, shrink); else Atomics[op](i32, 2, shrink);
    return "no throw";
  });
});
t("#59 compareExchange expected shrinks", function () {
  var rab = new ArrayBuffer(16, { maxByteLength: 64 });
  return Atomics.compareExchange(new Int32Array(rab), 2, { valueOf: function () { rab.resize(0); return 0; } }, 1);
});
t("#59 bigint", function () {
  var rab = new ArrayBuffer(16, { maxByteLength: 64 });
  return Atomics.add(new BigInt64Array(rab), 1, { valueOf: function () { rab.resize(0); return 1n; } });
});
t("#59 fixed-length view", function () {
  var rab = new ArrayBuffer(16, { maxByteLength: 64 });
  return Atomics.store(new Int32Array(rab, 0, 4), 0, { valueOf: function () { rab.resize(8); return 1; } });
});
t("#59 partial element", function () {
  var rab = new ArrayBuffer(16, { maxByteLength: 64 });
  return Atomics.store(new Int32Array(rab), 2, { valueOf: function () { rab.resize(10); return 1; } });
});
t("#59 still in bounds", function () {
  var rab = new ArrayBuffer(16, { maxByteLength: 64 });
  var v = new Int32Array(rab);
  Atomics.store(v, 1, { valueOf: function () { rab.resize(8); return 5; } });
  return v.length + ":" + v[1];
});
(function () {
  var called = false, rab = new ArrayBuffer(16, { maxByteLength: 64 });
  t("#59 index shrinks", function () {
    return Atomics.store(new Int32Array(rab), { valueOf: function () { rab.resize(0); return 1; } }, { valueOf: function () { called = true; return 1; } });
  });
  out.push("#59 value converted after index shrink => " + called);
})();
t("#59 load", function () {
  var rab = new ArrayBuffer(16, { maxByteLength: 64 });
  return Atomics.load(new Int32Array(rab), { valueOf: function () { rab.resize(0); return 1; } });
});
t("#59 plain", function () {
  var l = new Int32Array(4);
  Atomics.store(l, 3, 1);
  return [Atomics.exchange(l, 3, 4), Atomics.compareExchange(l, 3, 4, 9), l[3]].join(",");
});

var TypedArray = Object.getPrototypeOf(Uint8Array);
class C extends TypedArray {}
class D extends C {}
t("#60 new C", function () { return Object.prototype.toString.call(new C()); });
t("#60 new D", function () { return Object.prototype.toString.call(new D()); });
t("#60 new TypedArray", function () { return new TypedArray(); });
(function () {
  var read = false;
  var NT = function () {}.bind();
  Object.defineProperty(NT, "prototype", { get: function () { read = true; return Object.prototype; } });
  t("#60 Reflect.construct", function () { return Reflect.construct(TypedArray, [], NT); });
  out.push("#60 newTarget.prototype read => " + read);
})();
class U extends Uint8Array {}
t("#60 Uint8Array subclass", function () { return new U(3).length; });

t("#61 globals", function () { return typeof SharedArrayBuffer + " " + typeof Atomics; });
t("#61 brand", function () { return Object.prototype.toString.call(new SharedArrayBuffer(4)); });
t("#61 growable", function () {
  var s = new SharedArrayBuffer(4, { maxByteLength: 16 });
  s.grow(12);
  var v = new Int32Array(s);
  return [s.growable, s.byteLength, s.maxByteLength, v.length].join(",");
});
t("#61 shrink refused", function () { var s = new SharedArrayBuffer(8, { maxByteLength: 16 }); s.grow(4); });
t("#61 atomics", function () {
  var v = new Int32Array(new SharedArrayBuffer(16));
  Atomics.store(v, 1, 5);
  return [Atomics.add(v, 1, 2), Atomics.load(v, 1), Atomics.compareExchange(v, 1, 7, 9), v[1], Atomics.notify(v, 1, 1)].join(",");
});
t("#61 waitAsync", function () {
  var r = Atomics.waitAsync(new Int32Array(new SharedArrayBuffer(16)), 0, 1);
  return r.async + ":" + r.value;
});
t("#61 slice", function () { return new SharedArrayBuffer(8).slice(2, 6).byteLength; });
t("#61 not an ArrayBuffer", function () { return ArrayBuffer.prototype.slice.call(new SharedArrayBuffer(4)); });
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "\
#59 store => throws RangeError
#59 add => throws RangeError
#59 sub => throws RangeError
#59 and => throws RangeError
#59 or => throws RangeError
#59 xor => throws RangeError
#59 exchange => throws RangeError
#59 compareExchange => throws RangeError
#59 compareExchange expected shrinks => throws RangeError
#59 bigint => throws RangeError
#59 fixed-length view => throws TypeError
#59 partial element => throws RangeError
#59 still in bounds => 2:5
#59 index shrinks => throws RangeError
#59 value converted after index shrink => true
#59 load => throws RangeError
#59 plain => 1,4,9
#60 new C => throws TypeError
#60 new D => throws TypeError
#60 new TypedArray => throws TypeError
#60 Reflect.construct => throws TypeError
#60 newTarget.prototype read => false
#60 Uint8Array subclass => 3
#61 globals => function object
#61 brand => [object SharedArrayBuffer]
#61 growable => true,12,16,3
#61 shrink refused => throws RangeError
#61 atomics => 5,7,7,9,0
#61 waitAsync => false:not-equal
#61 slice => 4
#61 not an ArrayBuffer => throws TypeError";

#[test]
fn atomics_revalidate_typed_array_abstract_and_shared_buffers() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let text = out.output.join("\n");
    for (got, want) in text.lines().zip(EXPECTED.lines()) {
        assert_eq!(got, want);
    }
    assert_eq!(text.lines().count(), EXPECTED.lines().count(), "row count differs:\n{text}");
}
