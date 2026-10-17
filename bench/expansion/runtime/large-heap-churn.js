// large-heap-churn: an application-shaped LRU cache run.
//
// Measures a long, steady-state run of an LRU cache: a Map from string keys
// to doubly-linked list nodes (up to 50,000 entries, warmed to capacity
// before the churn starts), each holding a value object with a name string,
// a payload string of varying length and a small array. Requests are 60%
// hot keys (half the capacity) and 40% cold keys (four times the capacity),
// giving roughly a 57% hit rate. Each operation builds its key string, looks
// it up, and on a hit moves the node to the front (pointer rewrites in old objects) and
// sometimes replaces its value with a fresh object; on a miss it inserts a
// new node and evicts the least-recently-used one (unlinking it and nulling
// prev/next, as real code does). Every 16th inserted value also joins a
// retained survivor ring of 20,000 slots that outlives cache eviction, and
// at regular intervals the run bulk-frees: it flushes a third of the cache at
// once, and every other time it also drops the whole survivor ring.
//
// Why a likely weak spot: the live heap is large and long-lived (old space)
// while allocation never stops, so the run is dominated by how the collector
// copes: full-heap marking cost, pause length, heap growth policy after bulk
// frees, and remembered-set traffic from list-pointer updates in old nodes.
// The large size is an extended run meant to expose heap growth and
// collection behaviour rather than steady-state throughput alone. QuickJS
// frees acyclic garbage by refcount but its cycle collector still scans the
// whole live heap whenever its allocation threshold trips; the list nodes are
// cyclic while linked.
//
// Dominant operations: string concatenation for keys, Map get/set/delete
// with string keys, small object allocation, pointer stores into old
// objects, GC.
"use strict";

const SIZE = 100;

const OPS = SIZE * 2000;
const CAPACITY = Math.min(50000, SIZE * 500);
const HOT = CAPACITY >> 1;                 // keys that mostly stay resident
const COLD = CAPACITY * 4;                 // keys that cause misses and evictions
const RING = Math.min(20000, SIZE * 200);  // retained survivor cohort
const FLUSH_EVERY = Math.max(500, Math.min(60000, OPS >> 2));

let seed = 0x1234567;
function rndInt(n) {
  seed = (Math.imul(seed, 1664525) + 1013904223) | 0;
  return (((seed >>> 8) & 0xffffff) % n) | 0;
}

const PAD = "abcdefghijklmnopqrstuvwxyz0123456789";

function makeValue(id, op) {
  const len = 8 + (id % 29);
  return {
    id,
    name: "item-" + id,
    payload: PAD.slice(0, len) + ":" + op,
    tags: [id & 7, (id >> 3) & 15, op & 255],
    version: 1,
  };
}

class LRU {
  constructor(capacity) {
    this.capacity = capacity;
    this.map = new Map();
    this.head = null;   // most recently used
    this.tail = null;   // least recently used
    this.evictions = 0;
  }
  unlink(node) {
    if (node.prev !== null) node.prev.next = node.next; else this.head = node.next;
    if (node.next !== null) node.next.prev = node.prev; else this.tail = node.prev;
    node.prev = null;
    node.next = null;
  }
  pushFront(node) {
    node.next = this.head;
    node.prev = null;
    if (this.head !== null) this.head.prev = node;
    this.head = node;
    if (this.tail === null) this.tail = node;
  }
  get(key) {
    const node = this.map.get(key);
    if (node === undefined) return undefined;
    if (node !== this.head) { this.unlink(node); this.pushFront(node); }
    return node;
  }
  insert(key, value) {
    const node = { key, value, prev: null, next: null };
    this.map.set(key, node);
    this.pushFront(node);
    if (this.map.size > this.capacity) this.evictOldest();
    return node;
  }
  evictOldest() {
    const node = this.tail;
    this.map.delete(node.key);
    this.unlink(node);
    this.evictions++;
    return node;
  }
}

const cache = new LRU(CAPACITY);
let survivors = new Array(RING).fill(null);
let ringPos = 0, inserted = 0, hits = 0, misses = 0, updates = 0, flushes = 0, dropped = 0;
let checksum = 0;

// warm the cache to capacity before the measured churn
for (let i = 0; i < CAPACITY; i++) {
  const id = i < HOT ? i : HOT + rndInt(COLD - HOT);
  const key = "user:" + id + "/" + (id & 15);
  if (cache.map.get(key) === undefined) cache.insert(key, makeValue(id, -1));
}

for (let op = 0; op < OPS; op++) {
  const id = rndInt(5) < 3 ? rndInt(HOT) : rndInt(COLD);
  const key = "user:" + id + "/" + (id & 15);
  const node = cache.get(key);
  if (node !== undefined) {
    hits++;
    const v = node.value;
    checksum = (checksum + v.id + v.payload.length + v.tags[1] + v.version) | 0;
    if ((op & 7) === 0) {                     // replace with a fresh value object
      const nv = makeValue(id, op);
      nv.version = v.version + 1;
      node.value = nv;
      updates++;
    }
  } else {
    misses++;
    const v = makeValue(id, op);
    cache.insert(key, v);
    inserted++;
    if (inserted % 16 === 0) {                // long-lived survivor cohort
      survivors[ringPos] = v;
      ringPos = (ringPos + 1) % RING;
    }
  }

  // short-lived request object that never escapes
  const req = { key, op, flags: [op & 1, op & 2] };
  checksum = (checksum ^ (req.key.length + req.flags[1])) | 0;

  if (op % FLUSH_EVERY === (FLUSH_EVERY >> 1)) { // bulk free
    flushes++;
    const n = (cache.map.size / 3) | 0;
    for (let i = 0; i < n; i++) {
      const old = cache.evictOldest();
      checksum = (checksum + old.value.tags[0]) | 0;
    }
    if ((flushes & 1) === 0) {
      for (let i = 0; i < RING; i++) if (survivors[i] !== null) dropped++;
      survivors = new Array(RING).fill(null);
      ringPos = 0;
    }
  }
}

// fold the final state: walk the list from most to least recent, then the ring
let walked = 0;
for (let n = cache.head; n !== null; n = n.next) {
  walked++;
  checksum = (Math.imul(checksum, 31) + n.value.id + n.value.version + n.key.length) | 0;
}
let ringLive = 0;
for (let i = 0; i < RING; i++) {
  const v = survivors[i];
  if (v !== null) { ringLive++; checksum = (checksum + v.id + v.payload.length) | 0; }
}

console.log("large-heap-churn ops=" + OPS + " capacity=" + CAPACITY + " size=" + cache.map.size + " walked=" + walked);
console.log("hits=" + hits + " misses=" + misses + " updates=" + updates + " evictions=" + cache.evictions);
console.log("flushes=" + flushes + " ringLive=" + ringLive + " dropped=" + dropped + " checksum=" + checksum);
