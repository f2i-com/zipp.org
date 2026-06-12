"use strict";
// real-world bench 7: class hierarchy with method overrides + super calls,
// getter/setter pair, Symbol.toStringTag; hot polymorphic instance.method()
// calls cycling 4 shapes, getter/setter round-trips, and Object.create
// proto-chain (depth 5) reads. Deterministic accumulators.
class Shape {
  constructor(v) { this._v = v | 0; }
  area() { return this._v + 1; }
  get v() { return this._v; }
  set v(x) { this._v = x | 0; }
  get [Symbol.toStringTag]() { return "Shape:" + this.constructor.name; }
}
class Circle extends Shape {
  area() { return super.area() * 3 + 1; }
}
class Square extends Shape {
  constructor(v) { super(v); this.side = v * 2; }
  area() { return super.area() * 4 + this.side; }
}
class Tri extends Shape {
  area() { return super.area() * 5 + 3; }
  get v() { return super.v * 2; } // override getter, super property access
  set v(x) { super.v = x; }
}
class Hex extends Shape {
  area() { return (super.area() * 6 + 5) | 0; }
  get v() { return super.v; }
  set v(x) { super.v = x + 1; } // override setter
}

var objs = [new Circle(11), new Square(22), new Tri(33), new Hex(44)];

// tags via Object.prototype.toString (Symbol.toStringTag)
function fnv1a(str) {
  var h = 0x811c9dc5;
  for (var i = 0; i < str.length; i++) {
    h = Math.imul(h ^ str.charCodeAt(i), 16777619);
  }
  return h >>> 0;
}
var tagStr = "";
for (var i = 0; i < 4; i++) tagStr += Object.prototype.toString.call(objs[i]) + ";";
var tagHash = fnv1a(tagStr);

// 1) polymorphic method calls cycling the 4 shapes
var CALLS = 32000000;
var acc = 0;
for (var i = 0; i < CALLS; i++) {
  acc = (acc + objs[i & 3].area()) | 0;
}

// 2) getter/setter round-trips (mixes plain + overridden accessors)
var RT = 8000000;
var gacc = 0;
for (var i = 0; i < RT; i++) {
  var o = objs[i & 3];
  o.v = (i & 1023) - 7;
  gacc = (gacc + o.v) | 0;
}

// 3) Object.create proto chain, depth 5; hot reads walking to the root
var root = { deep: 19937, label: 3 };
var lvl1 = Object.create(root); lvl1.l1 = 1;
var lvl2 = Object.create(lvl1); lvl2.l2 = 2;
var lvl3 = Object.create(lvl2); lvl3.l3 = 3;
var lvl4 = Object.create(lvl3); lvl4.l4 = 4;
var leaf = Object.create(lvl4); leaf.own = 5;
var READS = 8000000;
var pacc = 0;
for (var i = 0; i < READS; i++) {
  pacc = (pacc + leaf.deep + leaf.l2 + leaf.own + leaf.label) | 0;
}

console.log("acc=" + acc + " gacc=" + gacc + " pacc=" + pacc + " tagHash=" + tagHash +
  " finalVs=" + objs[0].v + "," + objs[1].v + "," + objs[2].v + "," + objs[3].v);
