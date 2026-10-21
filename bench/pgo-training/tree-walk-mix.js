"use strict";

// PGO-only workload: a syntax-tree walk in the style of compilers and
// bundlers. A wrapper object per visited node, visitor tables keyed by node
// kind, a loose iteration helper over arrays, prototype predicates, string
// switches and per-wrapper caches. Independent of every scored benchmark.

var KINDS = ["Name", "Call", "Member", "Block", "Literal", "Binary", "Unary", "Func", "Return", "Cond", "Assign", "Seq"];
var CHILDREN = {
  Name: [],
  Literal: [],
  Call: ["callee", "args"],
  Member: ["object", "prop"],
  Block: ["body"],
  Binary: ["left", "right"],
  Unary: ["arg"],
  Func: ["params", "body"],
  Return: ["arg"],
  Cond: ["test", "then", "alt"],
  Assign: ["target", "value"],
  Seq: ["items"]
};
var LISTS = { args: true, body: true, params: true, items: true };

var seed = 48271;
function pick(n) {
  seed = (Math.imul(seed, 69621) + 11) | 0;
  return (seed >>> 9) % n;
}

function build(depth) {
  var kind = depth > 7 ? (pick(3) === 0 ? "Literal" : "Name") : KINDS[pick(KINDS.length)];
  var node = { kind: kind, start: pick(9000), end: 0, label: null, value: 0 };
  var keys = CHILDREN[kind];
  for (var i = 0; i < keys.length; i++) {
    var key = keys[i];
    if (LISTS[key]) {
      var list = [];
      var count = pick(5);
      for (var j = 0; j < count; j++) list.push(build(depth + 1));
      node[key] = list;
    } else {
      node[key] = build(depth + 1);
    }
  }
  if (kind === "Name") node.label = "id" + pick(48);
  if (kind === "Literal") node.value = pick(1000);
  node.end = node.start + keys.length + 1;
  return node;
}

function looseEach(arr) {
  var it = typeof Symbol !== "object" && arr[Symbol.iterator];
  if (it) return (it = it.call(arr)).next.bind(it);
  var i = 0;
  return function () {
    return i < arr.length ? { done: false, value: arr[i++] } : { done: true };
  };
}

function Cursor(owner, node, parent, slot, index) {
  this.owner = owner;
  this.node = node;
  this.parent = parent;
  this.slot = slot;
  this.index = index;
  this.kind = node ? node.kind : null;
  this.level = parent ? parent.level + 1 : 0;
  this.skipped = false;
  this.removed = false;
  this.children = null;
  this.marks = 0;
  this.scope = null;
  this.notes = null;
  this.flags = 0;
  this.extraA = null;
  this.extraB = null;
  this.extraC = 0;
  this.extraD = null;
}
var cp = Cursor.prototype;
cp.enclosing = function () {
  var at = this.parent;
  while (at && !at.isFunc()) at = at.parent;
  return at;
};
cp.note = function (text) {
  (this.notes || (this.notes = [])).push(text);
  return this;
};
cp.childCursor = function (key, index) {
  var store = this.children || (this.children = new Map());
  var list = store.get(key);
  if (!list) store.set(key, list = []);
  var child = this.node[key];
  var target = index < 0 ? child : child[index];
  var found = null;
  for (var k = 0; k < list.length; k++) if (list[k].node === target) found = list[k];
  if (!found) list.push(found = new Cursor(this.owner, target, this, key, index));
  return found;
};
for (var q = 0; q < KINDS.length; q++) {
  (function (kind) {
    cp["is" + kind] = function () {
      return this.kind === kind;
    };
  })(KINDS[q]);
}

function category(node) {
  switch (node.kind) {
    case "Call":
    case "Member":
    case "Binary":
    case "Unary":
    case "Cond":
    case "Assign":
    case "Seq":
      return 1;
    case "Block":
    case "Func":
    case "Return":
      return 2;
    case "Name":
      return node.label === "id7" ? 4 : 3;
    default:
      return 0;
  }
}

var seen = new WeakMap();
var visitors = {
  Name: {
    enter: [
      function (cur, st) {
        var fn = cur.enclosing();
        var bucket = st.names.get(cur.node.label);
        if (bucket === undefined) st.names.set(cur.node.label, bucket = { uses: 0, inFunc: 0 });
        bucket.uses++;
        if (fn !== null && fn !== undefined) bucket.inFunc++;
      }
    ]
  },
  Call: {
    enter: [
      function (cur, st) {
        st.calls++;
        if (cur.node.args.length > 2) cur.note("wide");
      },
      function (cur, st) {
        var prev = seen.get(cur.node);
        seen.set(cur.node, (prev || 0) + 1);
        st.mix = (st.mix + category(cur.node) * 31 + cur.level) | 0;
      }
    ]
  },
  Func: {
    enter: [
      function (cur, st) {
        cur.scope = { owner: cur, size: cur.node.params.length };
        st.funcs++;
      }
    ]
  },
  Literal: {
    enter: [
      function (cur, st) {
        st.lits = (st.lits + cur.node.value) | 0;
        if (cur.node.value > 990) cur.skipped = true;
      }
    ]
  },
  Cond: {
    enter: [
      function (cur, st) {
        if (cur.isCond() && cur.parent && (cur.parent.isBlock() || cur.parent.isSeq())) st.mix ^= cur.level;
      }
    ]
  }
};

function walk(owner, cur, st) {
  var node = cur.node;
  if (!node) return;
  var table = visitors[node.kind];
  if (table != null && table.enter) {
    for (var step, next = looseEach(table.enter); !(step = next()).done;) {
      step.value.call(st, cur, st);
    }
  }
  st.visits++;
  if (cur.skipped) return;
  for (var s2, n2 = looseEach(CHILDREN[node.kind]); !(s2 = n2()).done;) {
    var key = s2.value;
    var child = node[key];
    if (Array.isArray(child)) {
      for (var i = 0; i < child.length; i++) walk(owner, cur.childCursor(key, i), st);
    } else {
      walk(owner, cur.childCursor(key, -1), st);
    }
  }
}

var st = { names: new Map(), calls: 0, funcs: 0, lits: 0, mix: 0, visits: 0 };
for (var round = 0; round < 120; round++) {
  var tree = build(0);
  var owner = { round: round };
  var root = new Cursor(owner, tree, null, "root", -1);
  walk(owner, root, st);
  walk(owner, root, st);
}
var tags = 0;
st.names.forEach(function (bucket, label) {
  tags = (tags + bucket.uses * label.length + bucket.inFunc) | 0;
});
console.log("tree-walk=" + st.visits + ":" + st.calls + ":" + st.funcs + ":" + ((st.lits ^ st.mix ^ tags) >>> 0));
