// class-features: the ES2022 class machinery beyond plain methods.
//
// What it measures:
//   1. construction through a 3-level hierarchy (Entity -> Body -> Probe and
//      Entity -> Body -> Rock) where every level declares private fields with
//      initialisers, public class fields, and the base constructor reads
//      new.target (static field on the concrete class);
//   2. private methods and private accessors (#step(), get #mass()) called
//      from public methods, and `#field in obj` brand checks on foreign
//      objects;
//   3. static fields, static private fields and methods, and static blocks
//      that build lookup tables at class-definition time; a static factory;
//   4. super.method() chains through all three levels on the call path;
//   5. instanceof against every level of the hierarchy and an unrelated class,
//      plus Symbol.hasInstance on one class;
//   6. Symbol.iterator implemented on a class two ways: a hand-written
//      iterator object with next(), and a generator method; consumed by
//      for-of, spread and destructuring;
//   7. classes created inside a function (class expressions evaluated per
//      round, each with its own private names).
// Why it is a likely weak spot: private names are a separate lookup space
// (brand checks, per-class private keys), field initialisers run as hidden
// per-instance functions, derived constructors go through super() with
// new.target plumbing, and instanceof walks prototype chains; engines that
// tuned ordinary property ICs may leave all of these on slow generic paths.
// Dominant operations: derived-class construction with field initialisers,
// private field get/set, private method calls, super calls, instanceof,
// iterator protocol calls.
//
// SIZE is the number of rounds; every round does the same fixed amount of work.

const SIZE = 200;

let seed = 7;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function mix(h, v) { return Math.imul(h ^ (v | 0), 16777619); }

class Entity {
  static count = 0;
  static #serial = 0;
  static kindCodes;
  static {
    const codes = {};
    const names = ["entity", "body", "probe", "rock"];
    for (let i = 0; i < names.length; i++) codes[names[i]] = (i + 1) * 11;
    Entity.kindCodes = codes;
  }
  #id;
  #tag = 0;
  label = "e";
  constructor(seedValue) {
    this.#id = ++Entity.#serial;
    this.#tag = (seedValue * 31) & 1023;
    this.kind = new.target.kind;
    Entity.count++;
  }
  static #bump(n) { return (n * 5 + 1) & 0xffff; }
  static resetSerial(n) { Entity.#serial = Entity.#bump(n) & 7; }
  get id() { return this.#id; }
  get tag() { return this.#tag; }
  set tag(v) { this.#tag = v & 1023; }
  static isEntity(o) { return #id in o; }
  describe() { return Entity.kindCodes[this.kind] + (this.#tag & 15); }
  tick(n) { this.#tag = (this.#tag + n) & 1023; return this.#tag; }
}
Entity.kind = "entity";

class Body extends Entity {
  static kind = "body";
  #x = 0;
  #y = 0;
  #vx;
  #vy;
  mass = 1;
  constructor(seedValue, vx, vy) {
    super(seedValue);
    this.#vx = vx;
    this.#vy = vy;
  }
  #step(dt) {
    this.#x += this.#vx * dt;
    this.#y += this.#vy * dt;
    if (this.#x > 1000 || this.#x < -1000) this.#vx = -this.#vx;
    if (this.#y > 1000 || this.#y < -1000) this.#vy = -this.#vy;
  }
  get #speed() { return Math.abs(this.#vx) + Math.abs(this.#vy); }
  get position() { return (this.#x | 0) * 4096 + (this.#y | 0); }
  static sameBrand(a, b) { return #x in a && #x in b; }
  describe() { return super.describe() * 3 + (this.#speed | 0); }
  tick(n) {
    this.#step(n & 3);
    return super.tick(n) + (this.#x | 0);
  }
}

class Probe extends Body {
  static kind = "probe";
  static launched = 0;
  #fuel = 100;
  #log = [];
  constructor(seedValue) {
    super(seedValue, (seedValue % 7) - 3, (seedValue % 5) - 2);
    Probe.launched++;
  }
  #burn(n) {
    if (this.#fuel > n) { this.#fuel -= n; this.#log.push(n); }
    return this.#fuel;
  }
  get fuel() { return this.#fuel; }
  describe() { return super.describe() + this.#fuel; }
  tick(n) {
    const f = this.#burn(n & 1);
    if (this.#log.length > 16) this.#log.length = 0;
    return super.tick(n) ^ f;
  }
  static make(s) { return new Probe(s); }
}

class Rock extends Body {
  static kind = "rock";
  #hardness;
  constructor(seedValue) {
    super(seedValue, 1, -1);
    this.#hardness = seedValue & 7;
    this.mass = 10 + this.#hardness;
  }
  describe() { return super.describe() - this.#hardness; }
  tick(n) { return super.tick(n + this.#hardness) + this.mass; }
}

class Stranger { constructor() { this.kind = "other"; } }

class EvenTag {
  static [Symbol.hasInstance](o) { return o !== null && typeof o === "object" && (o.tag & 1) === 0; }
}

// Iterable classes: one with a hand-written iterator, one with a generator.
class RangeIterator {
  #cur; #end; #step;
  constructor(a, b, s) { this.#cur = a; this.#end = b; this.#step = s; }
  next() {
    if (this.#cur >= this.#end) return { value: undefined, done: true };
    const v = this.#cur;
    this.#cur += this.#step;
    return { value: v, done: false };
  }
  [Symbol.iterator]() { return this; }
}
class Range {
  #a; #b; #s;
  constructor(a, b, s = 1) { this.#a = a; this.#b = b; this.#s = s; }
  [Symbol.iterator]() { return new RangeIterator(this.#a, this.#b, this.#s); }
}
class Fleet {
  #members = [];
  add(m) { this.#members.push(m); return this; }
  get size() { return this.#members.length; }
  *[Symbol.iterator]() {
    for (let i = 0; i < this.#members.length; i++) yield this.#members[i];
  }
}

// A class expression evaluated per call: fresh private names every time.
function makeCell(initial) {
  const Cell = class {
    static made = 0;
    #v = initial;
    get v() { return this.#v; }
    swap(other) { const t = this.#v; this.#v = other.#v; other.#v = t; }
    static has(o) { return #v in o; }
  };
  return Cell;
}

function phaseConstruct(round, acc) {
  const fleet = new Fleet();
  let h = 0;
  for (let i = 0; i < 160; i++) {
    const s = (i * 13 + round) & 255;
    let o;
    switch (i & 3) {
      case 0: o = Probe.make(s); break;
      case 1: o = new Rock(s); break;
      case 2: o = new Body(s, 2, 1); break;
      default: o = new Probe(s + 1); break;
    }
    fleet.add(o);
  }
  // super chains and private method calls
  for (let t = 0; t < 6; t++) {
    for (const o of fleet) {
      h = (h + o.tick(t + (o.tag & 7))) | 0;
    }
  }
  for (const o of fleet) {
    h = mix(h, o.describe());
    h = (h + o.position + o.id + Entity.kindCodes[o.kind]) | 0;
    if (o instanceof Probe) h = (h + o.fuel) | 0;
    o.tag = o.tag + 3;
  }
  acc.h = mix(acc.h, h);
  acc.size += fleet.size;
  return fleet;
}

function phaseTypeChecks(fleet, others, acc) {
  let entity = 0, body = 0, probe = 0, rock = 0, stranger = 0, even = 0, brand = 0;
  const all = [...fleet, ...others];
  for (let rep = 0; rep < 6; rep++) {
    for (let i = 0; i < all.length; i++) {
      const o = all[i];
      if (o instanceof Entity) entity++;
      if (o instanceof Body) body++;
      if (o instanceof Probe) probe++;
      if (o instanceof Rock) rock++;
      if (o instanceof Stranger) stranger++;
      if (o instanceof EvenTag) even++;
      if (Entity.isEntity(o)) brand++;
      if (i > 0 && Body.sameBrand(o, all[i - 1])) brand += 2;
    }
  }
  acc.types = mix(acc.types, entity * 7 + body * 5 + probe * 3 + rock);
  acc.types = mix(acc.types, stranger * 11 + even * 13 + brand);
}

function phaseIterate(round, acc) {
  let s = 0;
  for (let rep = 0; rep < 8; rep++) {
    for (const v of new Range(0, 300, 1 + (rep & 3))) s += v;
    const arr = [...new Range(round & 15, 200, 3)];
    s += arr.length + arr[arr.length - 1];
    const [a, b, , d = -1] = new Range(rep, rep + 2);
    s += a + b + d;
  }
  acc.iter = mix(acc.iter, s);
}

function phaseCells(round, acc) {
  const A = makeCell(round & 63);
  const B = makeCell(7);
  const xs = [];
  for (let i = 0; i < 16; i++) { xs.push(new A()); A.made++; }
  const y = new B();
  let s = 0;
  for (let i = 0; i < 400; i++) {
    const p = xs[i & 15], q = xs[(i * 5 + 3) & 15];
    p.swap(q);
    s += p.v;
  }
  if (A.has(xs[0]) && !A.has(y) && B.has(y)) s += 1000;
  acc.cells = mix(acc.cells, s + A.made);
}

function main() {
  const rounds = SIZE;
  const acc = { h: 0, size: 0, types: 0, iter: 0, cells: 0 };
  const others = [];
  for (let i = 0; i < 16; i++) others.push(i & 1 ? new Stranger() : { tag: i, kind: "plain" });
  for (let r = 0; r < rounds; r++) {
    Entity.resetSerial(r);
    const fleet = phaseConstruct(r, acc);
    phaseTypeChecks(fleet, others, acc);
    phaseIterate(r, acc);
    phaseCells(r, acc);
  }
  console.log("construct", acc.h >>> 0, acc.size, Entity.count, Probe.launched);
  console.log("types", acc.types >>> 0);
  console.log("iterate", acc.iter >>> 0);
  console.log("cells", acc.cells >>> 0);
}

main();
