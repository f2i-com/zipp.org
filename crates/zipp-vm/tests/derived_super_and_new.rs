//! Regression tests for Commit 3B: fast `ClassData::has_private_names`,
//! inline zero-FFI `ThisCheck`, inline `SuperCtorFetch`, dedicated
//! `jit_super_ctor_ic`, frame-free derived constructors (`derived_ctor_stack`),
//! and plain ctor-less derived subclass walking in `new` and `super(...)`.
//!
//! Expected output lines are Node 24's.

const PROGRAM: &str = r#"
"use strict";

function expectThrow(fn) {
  try {
    fn();
    return "no-throw";
  } catch (e) {
    return e.name;
  }
}

// 1. Deep DOM-style derived hierarchy mixing explicit ctors, default params,
//    and ctor-less subclasses (both leaf and intermediate).
function testDeepDomHierarchy() {
  class EventTarget {
    constructor() {
      this.events = 0;
    }
  }
  class Node extends EventTarget {
    constructor(doc, type) {
      super();
      this.ownerDocument = doc;
      this.nodeType = type;
    }
  }
  class ParentNode extends Node {
    constructor(doc, type) {
      super(doc, type);
      this.children = 0;
    }
  }
  class Element extends ParentNode {
    constructor(doc, localName) {
      super(doc, 1);
      this.localName = localName;
    }
  }
  class HTMLElement extends Element {}
  class HTMLDivElement extends HTMLElement {
    constructor(doc, localName = "div") {
      super(doc, localName);
    }
  }
  class HTMLSpanElement extends HTMLElement {}

  let acc = 0;
  let lastDiv = null;
  let lastSpan = null;
  for (let i = 0; i < 80; i++) {
    const d = new HTMLDivElement("doc", i % 2 === 0 ? undefined : "section");
    const s = new HTMLSpanElement("doc", "span");
    acc += d.nodeType + s.nodeType + d.events + s.children;
    lastDiv = d;
    lastSpan = s;
  }
  return [
    "dom",
    acc,
    lastDiv instanceof HTMLDivElement,
    lastDiv instanceof HTMLElement,
    lastDiv instanceof Element,
    lastDiv instanceof Node,
    lastDiv instanceof EventTarget,
    lastDiv.localName,
    lastSpan instanceof HTMLSpanElement,
    lastSpan instanceof HTMLElement,
    lastSpan.localName,
    Object.getPrototypeOf(lastSpan) === HTMLSpanElement.prototype,
  ].join(":");
}

// 2. Ctor-less derived subclasses over empty base and explicit base (including
//    ignored primitive return and object return-override from base).
function testCtorlessBaseChains() {
  class EmptyBase {}
  class EmptySub1 extends EmptyBase {}
  class EmptySub2 extends EmptySub1 {}

  class PrimRetBase {
    constructor(v) {
      this.v = v;
      return 42;
    }
  }
  class PrimRetSub1 extends PrimRetBase {}
  class PrimRetSub2 extends PrimRetSub1 {}

  const sentinel = { override: 99 };
  class ObjRetBase {
    constructor(v) {
      if (v > 50) return sentinel;
      this.v = v;
    }
  }
  class ObjRetSub extends ObjRetBase {}

  let sum = 0;
  let emptyCount = 0;
  let ovCount = 0;
  for (let i = 0; i < 70; i++) {
    const e = new EmptySub2();
    if (e instanceof EmptySub2 && e instanceof EmptyBase) emptyCount++;
    const p = new PrimRetSub2(i);
    sum += p.v;
    const o = new ObjRetSub(i);
    if (o === sentinel) ovCount++;
    else sum += o.v;
  }
  return `ctorless:${emptyCount}:${sum}:${ovCount}`;
}

// 3. Object.setPrototypeOf on explicit derived, ctor-less leaf, intermediate
//    ctor-less class, and setting prototype to null.
function testSetPrototypeOfSuper() {
  class BaseA {
    constructor(x) {
      this.from = "A:" + x;
    }
  }
  class BaseB {
    constructor(x) {
      this.from = "B:" + x;
    }
  }
  class ExplicitSub extends BaseA {
    constructor(x) {
      super(x);
      this.tag = "exp";
    }
  }
  class CtorlessMid extends BaseA {}
  class CtorlessLeaf extends CtorlessMid {}

  const out = [];
  for (let i = 0; i < 50; i++) {
    const a = new ExplicitSub(i);
    const b = new CtorlessLeaf(i);
    if (i === 49) out.push(a.from + "/" + a.tag, b.from);
  }
  Object.setPrototypeOf(ExplicitSub, BaseB);
  Object.setPrototypeOf(CtorlessMid, BaseB);
  for (let i = 0; i < 10; i++) {
    const a = new ExplicitSub(i);
    const b = new CtorlessLeaf(i);
    if (i === 9) out.push(a.from + "/" + a.tag, b.from);
  }
  Object.setPrototypeOf(ExplicitSub, null);
  out.push(expectThrow(() => new ExplicitSub(1)));
  return "reproto:" + out.join("|");
}

// 4. Repeated class expressions (mixins) so slot.repeated === true and
//    mi_class_epoch advances across evaluations.
function testRepeatedMixins() {
  function makeSub(Base, label) {
    return class extends Base {
      constructor(x) {
        super(x);
        this.chain = (this.chain || "") + label;
      }
    };
  }
  class Root {
    constructor(x) {
      this.x = x;
      this.chain = "R";
    }
  }
  let Cls = Root;
  for (let i = 0; i < 5; i++) {
    Cls = makeSub(Cls, String(i));
  }
  let last = null;
  for (let i = 0; i < 60; i++) {
    last = new Cls(i);
  }
  return `mixin:${last.x}:${last.chain}:${last instanceof Root}`;
}

// 5. Private fields/brands mixed with classes that have NO privates, plus
//    field initializer ordering & new.target === undefined inside fields.
function testPrivatesAndFields() {
  const order = [];
  class PlainBase {
    constructor(x) {
      this.x = x;
    }
  }
  class PlainDerived extends PlainBase {
    constructor(x) {
      super(x);
      this.y = x + 1;
    }
  }
  class PrivBase {
    #secret = 10;
    baseField = (order.push("bf:" + (new.target === undefined)), 1);
    constructor(v) {
      order.push("bc");
      this.#secret += v;
    }
    getSecret() {
      return this.#secret;
    }
  }
  class PrivDerived extends PrivBase {
    #extra = 20;
    derivedField = (order.push("df:" + (new.target === undefined)), 2);
    constructor(v) {
      order.push("dc0");
      super(v);
      order.push("dc1");
      this.#extra += v;
    }
    total() {
      return this.getSecret() + this.#extra + this.baseField + this.derivedField;
    }
  }
  let plainSum = 0;
  let privSum = 0;
  for (let i = 0; i < 50; i++) {
    const p = new PlainDerived(i);
    plainSum += p.x + p.y;
    const q = new PrivDerived(i);
    privSum += q.total();
  }
  return `priv:${plainSum}:${privSum}:${order.slice(0, 5).join(",")}`;
}

// 6. Derived return-override, double-private-init TypeError, non-extensible
//    override, and derived error cases (double super, missing super, primitive
//    return, pre-super this TDZ, arrow super, new.target propagation).
function testDerivedSemanticsAndErrors() {
  const log = [];

  // new.target propagation through explicit and ctor-less derived chains
  class NtBase {
    constructor() {
      this.ntName = new.target ? new.target.name : "none";
    }
  }
  class NtMid extends NtBase {
    constructor() {
      super();
    }
  }
  class NtLeafCtorless extends NtMid {}
  class NtLeafExplicit extends NtMid {
    constructor() {
      super();
    }
  }
  let n1, n2;
  for (let i = 0; i < 50; i++) {
    n1 = new NtLeafCtorless();
    n2 = new NtLeafExplicit();
  }
  log.push(n1.ntName, n2.ntName);

  // Return-override from parent class
  class OvBase {
    constructor(target) {
      return target;
    }
  }
  class OvDerived extends OvBase {
    #mark = 7;
    constructor(target) {
      super(target);
      this.tagged = true;
    }
    getMark() {
      return this.#mark;
    }
  }
  let lastOv = null;
  for (let i = 0; i < 50; i++) {
    const bag = { id: i };
    lastOv = new OvDerived(bag);
  }
  log.push(lastOv.id, lastOv.tagged, OvDerived.prototype.getMark.call(lastOv));
  // Double private init on same override object throws TypeError
  log.push(expectThrow(() => new OvDerived(lastOv)));
  // Non-extensible override object throws TypeError when initializing private
  const frozen = Object.preventExtensions({});
  log.push(expectThrow(() => new OvDerived(frozen)));

  // Error cases warmed past JIT threshold
  class Base0 {
    constructor() {}
  }
  class DoubleSuper extends Base0 {
    constructor(twice) {
      super();
      if (twice) super();
    }
  }
  class MissingSuper extends Base0 {
    constructor(callIt) {
      if (callIt) super();
    }
  }
  class PrimReturn extends Base0 {
    constructor(bad) {
      super();
      if (bad) return 123;
    }
  }
  class PreSuperThis extends Base0 {
    constructor(readEarly) {
      if (readEarly) {
        this.bad = 1;
      }
      super();
      this.ok = 2;
    }
  }
  class ArrowSuper extends Base0 {
    constructor(x) {
      const go = () => {
        super();
        this.x = x;
      };
      go();
    }
  }

  for (let i = 0; i < 50; i++) {
    new DoubleSuper(false);
    new MissingSuper(true);
    new PrimReturn(false);
    new PreSuperThis(false);
    new ArrowSuper(i);
  }
  log.push(
    expectThrow(() => new DoubleSuper(true)),
    expectThrow(() => new MissingSuper(false)),
    expectThrow(() => new PrimReturn(true)),
    expectThrow(() => new PreSuperThis(true)),
    new ArrowSuper(77).x
  );

  // Mid-body deopt / bail both before and after super()
  class BailCtor extends Base0 {
    constructor(x, mode) {
      let pre = x + 1;
      if (mode === 1) {
        // Trigger polymorphic / string path before super
        pre = ({ valueOf() { return x + 2; } }) + 0;
      }
      super();
      this.pre = pre;
      if (mode === 2) {
        this.post = ({ valueOf() { return x + 3; } }) + 0;
      } else {
        this.post = x + 4;
      }
    }
  }
  for (let i = 0; i < 50; i++) {
    new BailCtor(i, 0);
  }
  const b1 = new BailCtor(10, 1);
  const b2 = new BailCtor(10, 2);
  log.push(b1.pre, b1.post, b2.pre, b2.post);

  return "sem:" + log.join("|");
}

console.log(testDeepDomHierarchy());
console.log(testCtorlessBaseChains());
console.log(testSetPrototypeOfSuper());
console.log(testRepeatedMixins());
console.log(testPrivatesAndFields());
console.log(testDerivedSemanticsAndErrors());
"#;

const EXPECTED: &[&str] = &[
    "dom:160:true:true:true:true:true:section:true:true:span:true",
    "ctorless:70:3690:19",
    "reproto:A:49/exp|A:49|B:9/exp|B:9|TypeError",
    "mixin:59:R01234:true",
    "priv:2500:4100:dc0,bf:true,bc,df:true,dc1",
    "sem:NtLeafCtorless|NtLeafExplicit|49|true|7|TypeError|TypeError|ReferenceError|ReferenceError|TypeError|ReferenceError|77|12|14|11|13",
];

fn run_ok(source: &str) -> Vec<String> {
    zipp_vm::run(source).expect("program runs").output
}

#[test]
fn derived_super_and_new_matches_node() {
    assert_eq!(run_ok(PROGRAM), EXPECTED);
}

#[test]
fn derived_super_and_new_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
        ("nursery_verify", "ZIPP_NURSERY_VERIFY", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("derived_super_and_new_matches_node")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
