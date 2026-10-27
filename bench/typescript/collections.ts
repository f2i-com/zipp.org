// TypeScript bench: generic data structures. A binary-heap priority queue,
// an LRU cache over Map, and Dijkstra on a generated graph, with generic
// constraints, private fields, iterators and tuples.

interface Comparator<T> {
  (a: T, b: T): number;
}

class PriorityQueue<T> implements Iterable<T> {
  #heap: T[] = [];
  constructor(private readonly cmp: Comparator<T>) {}
  get size(): number { return this.#heap.length; }
  push(value: T): void {
    const h = this.#heap;
    h.push(value);
    let i = h.length - 1;
    while (i > 0) {
      const p = (i - 1) >> 1;
      if (this.cmp(h[i]!, h[p]!) >= 0) break;
      [h[i], h[p]] = [h[p]!, h[i]!];
      i = p;
    }
  }
  pop(): T | undefined {
    const h = this.#heap;
    if (h.length === 0) return undefined;
    const top = h[0];
    const last = h.pop()!;
    if (h.length > 0) {
      h[0] = last;
      let i = 0;
      for (;;) {
        const l = 2 * i + 1, r = l + 1;
        let m = i;
        if (l < h.length && this.cmp(h[l]!, h[m]!) < 0) m = l;
        if (r < h.length && this.cmp(h[r]!, h[m]!) < 0) m = r;
        if (m === i) break;
        [h[i], h[m]] = [h[m]!, h[i]!];
        i = m;
      }
    }
    return top;
  }
  *[Symbol.iterator](): Iterator<T> {
    yield* this.#heap;
  }
}

class LRUCache<K, V> {
  private readonly map = new Map<K, V>();
  hits = 0;
  misses = 0;
  constructor(private readonly capacity: number) {}
  get(key: K, load: (k: K) => V): V {
    const v = this.map.get(key);
    if (v !== undefined) {
      this.hits++;
      this.map.delete(key);
      this.map.set(key, v);
      return v;
    }
    this.misses++;
    const fresh = load(key);
    this.map.set(key, fresh);
    if (this.map.size > this.capacity) this.map.delete(this.map.keys().next().value as K);
    return fresh;
  }
}

type Edge = readonly [to: number, weight: number];

function makeGraph(n: number, degree: number): Edge[][] {
  let s = 42;
  const rnd = (m: number) => ((s = (s * 48271) % 2147483647) % m);
  const g: Edge[][] = Array.from({ length: n }, () => []);
  for (let v = 0; v < n; v++) {
    for (let d = 0; d < degree; d++) g[v]!.push([rnd(n), 1 + rnd(100)] as const);
  }
  return g;
}

function dijkstra(graph: readonly (readonly Edge[])[], source: number): Float64Array {
  const dist = new Float64Array(graph.length).fill(Infinity);
  dist[source] = 0;
  const pq = new PriorityQueue<[node: number, d: number]>((a, b) => a[1] - b[1]);
  pq.push([source, 0]);
  while (pq.size > 0) {
    const [v, d] = pq.pop()!;
    if (d > dist[v]!) continue;
    for (const [to, w] of graph[v]!) {
      const nd = d + w;
      if (nd < dist[to]!) {
        dist[to] = nd;
        pq.push([to, nd]);
      }
    }
  }
  return dist;
}

const graph = makeGraph(20000, 6);
const cache = new LRUCache<number, Float64Array>(8);
let checksum = 0;
for (let q = 0; q < 24; q++) {
  const src = (q * 7919) % 12;
  const dist = cache.get(src, (s) => dijkstra(graph, s));
  for (let i = 0; i < dist.length; i += 97) if (Number.isFinite(dist[i])) checksum = (checksum + dist[i]!) % 1000000007;
}
const sorted = new PriorityQueue<number>((a, b) => b - a);
for (let i = 0; i < 200000; i++) sorted.push((i * 2654435761) % 1000003);
let prev = Infinity;
let ordered = true;
while (sorted.size > 0) {
  const v = sorted.pop()!;
  if (v > prev) ordered = false;
  prev = v;
}
console.log(checksum, cache.hits, cache.misses, ordered);
