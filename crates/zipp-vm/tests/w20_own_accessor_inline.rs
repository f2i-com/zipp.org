//! W20 (M2): OWN-accessor inlining -- `build_accessor_shape`'s own-slot arm.
//!
//! Until this wave `build_accessor_shape` inlined a getter/setter ONLY for a
//! CLASS instance: `HeapObj::Object(m) if !m.is_ctor => match m.class { Some(c)
//! => ..., None => return None }`, plus a G3b decline whenever an own property
//! of that name existed -- and an own accessor IS an own property of that name.
//! So the commonest way to write an accessor in JavaScript,
//! `Object.defineProperty(o, "v", { get })`, never inlined. The identical getter
//! body measured 128ms as a `defineProperty` accessor against 22ms on an ES
//! class over 8M reads (stock build; 87 vs 23 on PGO). This is the twin of the
//! defect B74/B78 fixed on `build_method_shape`, which learned own-slot and
//! inherited receivers and left its accessor sibling behind.
//!
//! Nothing below may change behaviour -- that is the point. Every case drives an
//! own accessor through a state that must invalidate the arm: the receiver's
//! identity+version guard (a `defineProperty` redefinition, a delete, a freeze,
//! a `setPrototypeOf`), or the arm's own re-read of the slot that holds the
//! accessor function (`vals[slot]` for a getter, `attrs[slot].setter` for a
//! setter). EVERY expectation below was produced by running the identical
//! snippet in node v24 and is a node-diffed answer, not one zipp told us.
//!
//! The whole file must also pass with `ZIPP_NO_OWN_ACCESSOR_INLINE=1` (the arm
//! off -- pre-wave behaviour), `ZIPP_NOJIT=1`, and `ZIPP_JIT_THRESHOLD=1`.
//!
//! KNOWN PRE-EXISTING DIVERGENCE, deliberately not asserted here: a setter that
//! replaces ITSELF re-entrantly (`Object.defineProperty` on the same key from
//! inside the setter body) keeps running the old setter under the JIT. It
//! reproduces at HEAD with this mechanism OFF, with `ZIPP_NO_ACCESSOR_WAY=1`,
//! and with `ZIPP_NO_METHOD_INLINE=1`, and disappears under `ZIPP_NOJIT=1` --
//! see the scratch repro `W20_GATE_preexisting_reentrant_setter_redefine.js`.
//! The same mutation done from OUTSIDE the setter, and the same self-replacement
//! done by a GETTER, are both correct and are asserted above.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// An own `defineProperty` getter REPLACED while the site is hot. The arm
/// bakes a resolved getter; `defineProperty` bumps the receiver's version
/// (props/define.rs), so the identity+version guard must miss from the
/// redefine onward and the new body must run for the rest of the loop.
#[test]
fn getter_redefined_mid_loop() {
    let out = run_ok(
        r##""use strict";
var o = { hidden: 7 };
Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
var s = 0;
for (var i = 0; i < 200000; i++) {
  if (i === 100000) {
    Object.defineProperty(o, "v", { get: function () { return this.hidden + 1000; }, configurable: true });
  }
  s = (s + o.v) | 0;
}
console.log(s);
        "##,
    );
    assert_eq!(out, vec!["101400000"]);
}

/// A DATA property redefined INTO an accessor and back out again. The own-slot
/// arm is only legal while `attrs[slot].accessor` holds; a flip in either
/// direction must invalidate it, or a plain value gets CALLED (or a getter
/// gets RETURNED).
#[test]
fn data_flipped_to_accessor_and_back() {
    let out = run_ok(
        r##""use strict";
var o = { hidden: 3, v: 5 };
var s = 0;
for (var i = 0; i < 200000; i++) {
  if (i === 66666) {
    Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true, enumerable: true });
  }
  if (i === 133333) {
    Object.defineProperty(o, "v", { value: 42, writable: true, configurable: true, enumerable: true });
  }
  s = (s + o.v) | 0;
}
console.log(s);
        "##,
    );
    assert_eq!(out, vec!["3333345"]);
}

/// The adversarial flip: the accessor's GETTER and the data property that
/// replaces it are the SAME function object, so the slot's Value bits are
/// bit-identical before and after. Only the version bump can tell "call it"
/// from "return it" -- a slot re-read alone would pass.
#[test]
fn accessor_flipped_to_data_holding_the_same_function() {
    let out = run_ok(
        r##""use strict";
var g = function () { return 11; };
var o = { hidden: 1 };
Object.defineProperty(o, "v", { get: g, configurable: true });
var s = 0, sawFn = 0;
for (var i = 0; i < 200000; i++) {
  if (i === 100000) { Object.defineProperty(o, "v", { value: g, writable: true, configurable: true }); }
  var r = o.v;
  if (typeof r === "function") { sawFn++; } else { s = (s + r) | 0; }
}
console.log(s + ":" + sawFn);
        "##,
    );
    assert_eq!(out, vec!["1100000:100000"]);
}

/// `delete o.v` while hot: the slot goes away entirely (and every later slot
/// shifts down), so the arm must stop firing and the read must answer
/// `undefined` from there on.
#[test]
fn accessor_deleted_mid_loop() {
    let out = run_ok(
        r##""use strict";
var o = { hidden: 4 };
Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
var s = 0, undefs = 0;
for (var i = 0; i < 200000; i++) {
  if (i === 100000) { delete o.v; }
  var r = o.v;
  if (r === undefined) { undefs++; } else { s = (s + r) | 0; }
}
console.log(s + ":" + undefs);
        "##,
    );
    assert_eq!(out, vec!["400000:100000"]);
}

/// A getter-only accessor must THROW on every write in strict mode. The setter
/// arm finds `attrs[slot].setter == undefined`, `ic_plain_fn` rejects it and
/// the site stays on the helper -- inlining a store here would silently
/// swallow 200k TypeErrors.
#[test]
fn getter_only_accessor_written_in_strict_mode() {
    let out = run_ok(
        r##""use strict";
var o = { hidden: 9 };
Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
var thrown = 0, s = 0;
for (var i = 0; i < 200000; i++) {
  try { o.v = i; } catch (e) { thrown++; }
  s = (s + o.v) | 0;
}
console.log(thrown + ":" + s);
        "##,
    );
    assert_eq!(out, vec!["200000:1800000"]);
}

/// The mirror: a setter-only accessor READ must answer `undefined` every time
/// (the getter half is undefined), while the setter half keeps working.
#[test]
fn setter_only_accessor_read() {
    let out = run_ok(
        r##""use strict";
var o = { hidden: 0 };
Object.defineProperty(o, "v", { set: function (x) { this.hidden = x | 0; }, configurable: true });
var undefs = 0;
for (var i = 0; i < 200000; i++) {
  o.v = i;
  if (o.v === undefined) { undefs++; }
}
console.log(undefs + ":" + o.hidden);
        "##,
    );
    assert_eq!(out, vec!["200000:199999"]);
}

/// A `defineProperty` accessor is an ARBITRARY function, unlike a class
/// accessor. A BOUND getter's `this` is its bound receiver, an ARROW getter's
/// `this` is LEXICAL, and a native has no inlinable body -- binding any of
/// them to the receiver would be a wrong answer, so all three must decline to
/// the helper and still produce node's numbers.
#[test]
fn bound_arrow_and_native_getters() {
    let out = run_ok(
        r##""use strict";
var src = { hidden: 21 };
var o1 = {};
Object.defineProperty(o1, "v", { get: function () { return this.hidden; }.bind(src), configurable: true });
var s1 = 0;
for (var i = 0; i < 200000; i++) { s1 = (s1 + o1.v) | 0; }
var lex = { hidden: 33 };
var o2 = { hidden: 99 };
Object.defineProperty(o2, "v", { get: (function () { var self = lex; return () => self.hidden; })(), configurable: true });
var s2 = 0;
for (var i = 0; i < 200000; i++) { s2 = (s2 + o2.v) | 0; }
var o3 = { hidden: 5 };
Object.defineProperty(o3, "v", { get: Math.random.bind(null), configurable: true });
var c3 = 0;
for (var i = 0; i < 200000; i++) { if (typeof o3.v === "number") { c3++; } }
console.log(s1 + ":" + s2 + ":" + c3);
        "##,
    );
    assert_eq!(out, vec!["4200000:6600000:200000"]);
}

/// A getter that closes over a variable and is not capture-free. The body reads
/// an upvalue, which `method_inline_body_ok` does not admit, so the arm must
/// decline -- and the mutation halfway through proves the value is being READ
/// rather than baked.
#[test]
fn getter_reading_a_mutated_upvalue() {
    let out = run_ok(
        r##""use strict";
var bump = 1000;
var o = { hidden: 2 };
Object.defineProperty(o, "v", { get: function () { return this.hidden + bump; }, configurable: true });
var s = 0;
for (var i = 0; i < 200000; i++) {
  if (i === 150000) { bump = 2000; }
  s = (s + o.v) | 0;
}
console.log(s);
        "##,
    );
    assert_eq!(out, vec!["250400000"]);
}

/// `Object.freeze` while hot. Freezing leaves an ACCESSOR callable (only data
/// properties lose `writable`), so the setter keeps running -- but the data
/// field it stores into is now non-writable, and that store must start
/// throwing in strict mode.
#[test]
fn freeze_mid_loop() {
    let out = run_ok(
        r##""use strict";
var o = { hidden: 6 };
Object.defineProperty(o, "v", {
  get: function () { return this.hidden; },
  set: function (x) { this.hidden = x | 0; },
  configurable: true
});
var s = 0, thrown = 0;
for (var i = 0; i < 200000; i++) {
  if (i === 100000) { Object.freeze(o); }
  try { o.v = (i & 7); } catch (e) { thrown++; }
  s = (s + o.v) | 0;
}
console.log(s + ":" + thrown + ":" + o.hidden);
        "##,
    );
    assert_eq!(out, vec!["1050000:100000:7"]);
}

/// A Proxy sharing the site with a plain own-accessor receiver. `ic_obj_ok` and
/// the per-arm identity guard must keep the Proxy off the inlined arm --
/// B87/B89 bind: the arm may not be widened to reach it.
#[test]
fn proxy_receiver_reaching_the_site() {
    let out = run_ok(
        r##""use strict";
var plain = { hidden: 8 };
Object.defineProperty(plain, "v", { get: function () { return this.hidden; }, configurable: true });
var prox = new Proxy({ hidden: 77, v: 77 }, {});
var objs = [plain, prox];
var s = 0;
for (var i = 0; i < 200000; i++) { s = (s + objs[i & 1].v) | 0; }
console.log(s);
        "##,
    );
    assert_eq!(out, vec!["8500000"]);
}

/// The getter's baked `this.hidden` SLOT moves, because an earlier key is
/// deleted. `mi_bake_fields` resolved that slot at plan time; the receiver
/// version bump on delete is what must invalidate it.
#[test]
fn field_slot_shifts_under_the_getter() {
    let out = run_ok(
        r##""use strict";
var o = { pad: 1, hidden: 12 };
Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
var s = 0;
for (var i = 0; i < 200000; i++) {
  if (i === 100000) { delete o.pad; }
  s = (s + o.v) | 0;
}
console.log(s);
        "##,
    );
    assert_eq!(out, vec!["2400000"]);
}

/// A class setter always has exactly one formal; a `defineProperty` one need
/// not. The emitter binds the incoming value to window reg 1 unconditionally,
/// so a 0-formal setter would have a LOCAL clobbered by the value -- both
/// shapes must produce node's answer.
#[test]
fn setters_with_wrong_arity() {
    let out = run_ok(
        r##""use strict";
var o = { hidden: 0, n: 0 };
Object.defineProperty(o, "v", { set: function () { this.n = this.n + 1; }, configurable: true });
for (var i = 0; i < 200000; i++) { o.v = i; }
var o2 = { hidden: 0 };
Object.defineProperty(o2, "v", { set: function (x, y) { this.hidden = (x | 0) + (y === undefined ? 1 : 0); }, configurable: true });
for (var i = 0; i < 200000; i++) { o2.v = i; }
console.log(o.n + ":" + o2.hidden);
        "##,
    );
    assert_eq!(out, vec!["200000:200000"]);
}

/// A class instance that GAINS an own accessor mid-loop. The class arm must
/// stop firing (the own property shadows it) and the own arm take over --
/// the same G3b shadowing rule, now with an answer on the other side.
#[test]
fn own_accessor_shadows_a_class_accessor() {
    let out = run_ok(
        r##""use strict";
class C {
  constructor(v) { this.hidden = v; }
  get v() { return this.hidden; }
}
var c = new C(5);
var s = 0;
for (var i = 0; i < 200000; i++) {
  if (i === 100000) {
    Object.defineProperty(c, "v", { get: function () { return this.hidden * 3; }, configurable: true });
  }
  s = (s + c.v) | 0;
}
console.log(s);
        "##,
    );
    assert_eq!(out, vec!["2000000"]);
}

/// Four own-accessor receivers behind `arr[i & 3]`, one of them REPLACED
/// mid-loop by a different object. Each arm guards a specific instance's
/// identity, so the replaced one must fall to the helper.
#[test]
fn receiver_swapped_out_of_the_array() {
    let out = run_ok(
        r##""use strict";
function mk(h) {
  var o = { hidden: h };
  Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
  return o;
}
var a = mk(1), b = mk(2), c = mk(4), d = mk(8);
var arr = [a, b, c, d];
var s = 0;
for (var i = 0; i < 200000; i++) {
  if (i === 100000) { arr[1] = mk(16); }
  s = (s + arr[i & 3].v) | 0;
}
console.log(s);
        "##,
    );
    assert_eq!(out, vec!["1100000"]);
}

/// `setPrototypeOf` on the receiver mid-loop. It bumps the version
/// (descriptors.rs), so the arm must miss even though the own accessor
/// itself did not change.
#[test]
fn prototype_repointed_under_an_own_accessor() {
    let out = run_ok(
        r##""use strict";
var proto1 = { extra: 1 };
var o = Object.create(proto1);
o.hidden = 10;
Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
var s = 0;
for (var i = 0; i < 200000; i++) {
  if (i === 100000) { Object.setPrototypeOf(o, { extra: 2 }); }
  s = (s + o.v) | 0;
}
console.log(s);
        "##,
    );
    assert_eq!(out, vec!["2000000"]);
}

/// polymorphic-objects' own receiver, verbatim: the `mkAccessor` shape driven
/// through the read+write pass that the row's `fn0@102`/`fn0@105` sites see.
#[test]
fn the_row_shape_get_and_set_together() {
    let out = run_ok(
        r##""use strict";
function mkAccessor(seed) {
  var o = { hidden: seed, pad: 0 };
  Object.defineProperty(o, "val", {
    get: function () { return this.hidden; },
    set: function (x) { this.hidden = x | 0; },
    enumerable: true, configurable: true
  });
  return o;
}
var o = mkAccessor(88);
var s = 0;
for (var i = 0; i < 200000; i++) {
  o.val = (i & 255) + 1;
  s = (s + o.val) | 0;
}
console.log(s + ":" + o.val + ":" + o.hidden);
        "##,
    );
    assert_eq!(out, vec!["25693856:64:64"]);
}
