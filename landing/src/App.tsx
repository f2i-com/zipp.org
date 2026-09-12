import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react'
import { formatCount, relativeTime, useRepoStats, type RepoStats, type RepoStatus } from './repoStats'
import { SandboxStack, ProjectShowcase } from './Experience'
import { RepositoryActivity } from './RepositoryActivity'

const GITHUB_URL = 'https://github.com/f2i-com/zipp.org'
const F2I_URL = 'https://f2i.com'
const DOCS_URL = `${GITHUB_URL}/blob/main/DOC.md#embedding`
const BENCHMARK_URL = `${GITHUB_URL}/blob/main/bench/real13_8229b3fc_pgo_2026-09-02.json`
const HOSTILE_BENCHMARK_URL = `${GITHUB_URL}/blob/main/bench/hostile/head_clean_8229b3fc_pgo_2026-09-02.json`
const ROADMAP_URL = `${GITHUB_URL}/blob/main/PERF_ROADMAP.md`
const RELEASE_URL = `${GITHUB_URL}/releases/latest`
const RELEASES_URL = `${GITHUB_URL}/releases`
const COMMITS_URL = `${GITHUB_URL}/commits/main`
const CAPTURE_README_URL = `${GITHUB_URL}/blob/e6e0f65dd402f1bf75b9675d7acd904cb239c5a3/README.md#canonical-public-capture`

/** Selectors whose matches fade and rise into view as the reader scrolls. */
const REVEAL_SELECTORS = [
  '.proof-grid > div',
  '.playground-heading > *',
  '.playground-shell',
  '.section-heading > *',
  '.use-case-card',
  '.controls-copy > *',
  '.code-window',
  '.benchmark-heading > *',
  '.benchmark-summary > article',
  '.reading-guide > article',
  '.scoreboard',
  '.methodology-note',
  '.release-strip',
  '.pipeline li',
  '.runtime-grid > article',
  '.quickstart-copy',
  '.terminal-block',
  '.closing-cta > *',
].join(', ')

/**
 * Reveal-on-scroll: every element matched by REVEAL_SELECTORS starts hidden
 * (`.reveal`) and gets `.is-in` when it approaches the viewport, staggered by
 * its position among its siblings. Readers who prefer reduced motion, and
 * browsers without IntersectionObserver, see everything immediately.
 */
function useReveal() {
  useEffect(() => {
    const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    if (reduce || typeof IntersectionObserver === 'undefined') return
    const nodes = Array.from(document.querySelectorAll<HTMLElement>(REVEAL_SELECTORS))
    for (const node of nodes) {
      const siblings = node.parentElement ? Array.from(node.parentElement.children) : [node]
      const index = Math.max(0, siblings.indexOf(node))
      node.style.setProperty('--reveal-delay', `${Math.min(index, 7) * 70}ms`)
      node.classList.add('reveal')
    }
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            entry.target.classList.add('is-in')
            observer.unobserve(entry.target)
          }
        }
      },
      { rootMargin: '0px 0px -8% 0px', threshold: 0.08 },
    )
    for (const node of nodes) observer.observe(node)
    return () => observer.disconnect()
  }, [])
}

/**
 * Browsers resolve an initial URL fragment before React has mounted the target.
 * Re-run that one fragment alignment after the committed layout exists so a
 * direct `https://zipp.org/#playground` visit lands on the playground reliably.
 * Later in-page links keep the browser's native hash and smooth-scroll behavior.
 */
function useInitialHashNavigation() {
  useEffect(() => {
    const hash = window.location.hash
    if (hash.length < 2) return

    let targetId: string
    try {
      targetId = decodeURIComponent(hash.slice(1))
    } catch {
      return
    }

    let firstFrame = 0
    let secondFrame = 0
    firstFrame = window.requestAnimationFrame(() => {
      secondFrame = window.requestAnimationFrame(() => {
        if (window.location.hash !== hash) return
        document.getElementById(targetId)?.scrollIntoView({ block: 'start', behavior: 'instant' })
      })
    })

    return () => {
      window.cancelAnimationFrame(firstFrame)
      window.cancelAnimationFrame(secondFrame)
    }
  }, [])
}

/** Count from 0 to `value` the first time the element scrolls into view. */
function CountUp({ value, format, duration = 1100 }: { value: number; format: (v: number) => string; duration?: number }) {
  const ref = useRef<HTMLElement | null>(null)
  const [shown, setShown] = useState(() => value)
  const [armed, setArmed] = useState(false)

  useEffect(() => {
    const el = ref.current
    if (!el) return
    const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    if (reduce || typeof IntersectionObserver === 'undefined') {
      setShown(value)
      return
    }
    setShown(0)
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        setArmed(true)
        observer.disconnect()
      }
    }, { threshold: 0.4 })
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  useEffect(() => {
    if (!armed) return
    let frame = 0
    const start = performance.now()
    const tick = (now: number) => {
      const t = Math.min(1, (now - start) / duration)
      const eased = 1 - Math.pow(1 - t, 3)
      setShown(value * eased)
      if (t < 1) frame = requestAnimationFrame(tick)
    }
    frame = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(frame)
  }, [armed, value, duration])

  return <strong ref={ref}>{format(armed || shown === value ? shown : shown)}</strong>
}

/**
 * Cursor spotlight: cards expose `--mx`/`--my` so a radial highlight follows
 * the pointer across their surface (see `.spot` in styles.css). Touch and
 * reduced-motion readers get the plain card.
 */
function useSpotlight() {
  useEffect(() => {
    if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) return
    if (window.matchMedia?.('(hover: none)').matches) return
    const selector = '.use-case-card, .runtime-grid > article, .reading-guide article, .release-strip li, .sandbox-card, .code-window, .benchmark-summary > article, .control-list article'
    const cards = Array.from(document.querySelectorAll<HTMLElement>(selector))
    for (const card of cards) card.classList.add('spot')
    const onMove = (event: PointerEvent) => {
      const card = (event.target as HTMLElement | null)?.closest<HTMLElement>('.spot')
      if (!card) return
      const rect = card.getBoundingClientRect()
      card.style.setProperty('--mx', `${event.clientX - rect.left}px`)
      card.style.setProperty('--my', `${event.clientY - rect.top}px`)
    }
    document.addEventListener('pointermove', onMove, { passive: true })
    return () => document.removeEventListener('pointermove', onMove)
  }, [])
}

function StarIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" className="star-icon">
      <path d="M8 1.5l1.9 4.1 4.4.5-3.3 3 .9 4.4L8 11.3l-3.9 2.2.9-4.4-3.3-3 4.4-.5z" />
    </svg>
  )
}

function LiveRepoStrip({ stats, status }: { stats: RepoStats; status: RepoStatus }) {
  const version = stats.releaseTag ?? 'View releases'
  const pushed = relativeTime(stats?.pushedAt ?? stats?.latestCommitDate)
  return (
    <div className={`live-repo ${status === 'synced' ? 'live-repo-loaded' : ''}`} aria-label="Repository status">
      <a className="live-repo-label" href="#updates">{status === 'synced' ? 'Synced with GitHub' : 'Saved GitHub data'}</a>
      <a href={stats?.releaseUrl ?? RELEASE_URL} target="_blank" rel="noreferrer"><b>{version}</b> latest release</a>
      {stats.latestCommitSha && (
        <a href={stats.latestCommitUrl ?? COMMITS_URL} target="_blank" rel="noreferrer"><b>{stats.latestCommitSha.slice(0, 8)}</b> on {stats.branch}</a>
      )}
      {stats?.stars ? (
        <a href={GITHUB_URL} target="_blank" rel="noreferrer"><b>{formatCount(stats.stars)}</b> stars</a>
      ) : null}
      {pushed && <span><b>updated</b> {pushed}</span>}
      {stats?.test262Pct !== undefined && (
        <span><b>{stats.test262Pct}%</b> of test262</span>
      )}
    </div>
  )
}

const playgroundExample = `const orders = [
  { id: "A-104", total: 48 },
  { id: "B-208", total: 73 },
  { id: "C-512", total: 29 },
];

const summary = orders
  .filter((order) => order.total >= 40)
  .map((order) => order.id + ": $" + order.total)
  .join(" | ");

console.log("priority orders", summary);
console.log("total", orders.reduce((sum, order) => sum + order.total, 0));`

const PLAYGROUND_BOOT_TIMEOUT_MS = 15_000
const PLAYGROUND_RUN_TIMEOUT_MS = 6_000

type PlaygroundExample = { id: string; title: string; blurb: string; source: string }

// Samples for the browser playground. The heavier ones are sized so the
// interpreter-only WASM build finishes each well inside the 2.5 s deadline and
// the sandbox's instruction budget: the point is to show a real amount of work
// completing quickly, not to hit the limits.
const playgroundExamples: PlaygroundExample[] = [
  { id: 'adventure', title: 'A story in a sandbox', blurb: 'Random heroes, branching encounters, and a seed to replay.', source: `// A standalone ZIPP demo, not code from Outerstead.
// The playground passes a fresh STORY_SEED from browser crypto on each run.
// Replace STORY_SEED below with a printed number to replay that adventure.
const SEED = typeof STORY_SEED === "number" ? STORY_SEED : 42;
const CHAPTERS = 5; // Try a longer journey!
let state = SEED >>> 0;
const roll = (sides) => {
  state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
  return 1 + Math.floor((state / 4294967296) * sides);
};
const pick = (items) => items[roll(items.length) - 1];

class Adventurer {
  constructor(name, calling) {
    this.name = name;
    this.calling = calling;
    this.spirit = 10;
    this.bag = new Map([["biscuits", 2]]);
    this.places = new Set();
  }
  collect(item) { this.bag.set(item, (this.bag.get(item) || 0) + 1); }
  get title() { return this.name + " the " + this.calling; }
}

const hero = new Adventurer(
  pick(["Moss", "Pip", "Clover", "Wren", "Juniper"]),
  pick(["mapmaker", "cloud collector", "reluctant wizard", "mushroom knight"])
);
const quest = pick(["find the missing moon", "deliver a letter to tomorrow", "wake the sleeping sea"]);
const places = ["Whispering Woods", "Clockwork Marsh", "Lantern Library", "Upside-Down Orchard", "Glass Mountain"];
const weather = ["under a violet sky", "as warm snow falls", "beneath two tiny suns", "in a rain of golden leaves"];
const encounters = [
  { who: "a fox selling borrowed dreams", gift: "bottled dream", good: "trades a dream for a terrible joke", bad: "insists on a riddle with no answer" },
  { who: "a bridge that has forgotten its name", gift: "silver compass", good: "remembers its name when you sing", bad: "sends you on a very long detour" },
  { who: "a dragon the size of a teacup", gift: "dragon ember", good: "shares a spark and a secret", bad: "sneezes sparks into your boots" },
  { who: "a librarian made of autumn leaves", gift: "tomorrow's map", good: "lends you a page from the future", bad: "assigns you three hours of shelving" },
  { who: "an extremely dramatic moon", gift: "moon fragment", good: "applauds your courage and offers a fragment", bad: "demands an encore of your worst memory" },
];

const journal = [];
console.log("THE LITTLE CHRONICLES / seed " + SEED);
console.log(hero.title + " sets out to " + quest + ".\\n");

for (let chapter = 1; chapter <= CHAPTERS && hero.spirit > 0; chapter++) {
  const place = pick(places);
  const encounter = pick(encounters);
  const luck = roll(20);
  hero.places.add(place);
  const success = luck >= 8;
  const twist = success ? encounter.good : encounter.bad;
  if (success) hero.collect(encounter.gift);
  else hero.spirit = Math.max(0, hero.spirit - roll(4));
  const line = "Chapter " + chapter + ": " + place + "\\n"
    + "Arriving " + pick(weather) + ", " + hero.name + " meets " + encounter.who + ".\\n"
    + "The stranger " + twist + ". "
    + (success ? "A keepsake joins the collection." : "Spirit remaining: " + hero.spirit + "/10.");
  journal.push({ chapter, place, luck, success });
  console.log(line + "\\n");
}

const treasures = [...hero.bag.keys()].filter(item => item !== "biscuits");
const ending = hero.spirit === 0
  ? "The quest can wait. A warm hearth and a new friend are adventure enough."
  : treasures.length >= 3
    ? "The keepsakes glow together. The impossible quest suddenly seems possible."
    : "The road bends toward home. Some stories need a second journey.";
console.log("EPILOGUE\\n" + ending);
console.log("Satchel: " + [...hero.bag].map(([item, n]) => n + " " + item).join(", "));
console.log("\\nSAVE GAME " + JSON.stringify({
  seed: SEED, hero: hero.title, spirit: hero.spirit,
  uniquePlaces: hero.places.size,
  luckyEncounters: journal.filter(entry => entry.success).length,
  chapters: journal.length
}));` },
  { id: 'orders', title: 'Orders summary', blurb: 'Array pipeline over a few records', source: playgroundExample },
  { id: 'sieve', title: 'Prime sieve', blurb: '1,000,000 numbers, a Uint8Array and two nested loops', source: `// Sieve of Eratosthenes: count the primes below one million.
const limit = 1_000_000;
const composite = new Uint8Array(limit + 1);
let count = 0;
for (let n = 2; n <= limit; n++) {
  if (composite[n]) continue;
  count++;
  for (let m = n * n; m <= limit; m += n) composite[m] = 1;
}
console.log("primes below", limit, "=", count);` },
  { id: 'mandel', title: 'Mandelbrot', blurb: '22,000 cells of complex arithmetic, drawn as a 44 x 20 picture', source: `// Mandelbrot set: 22,000 cells of complex arithmetic (up to 200 iterations
// each), then a 44 x 20 picture of the result.
const cols = 220, rows = 100, maxIter = 200;
const shades = " .:-=+*#%@";
let inside = 0;
const picture = [];
for (let y = 0; y < rows; y++) {
  let line = "";
  for (let x = 0; x < cols; x++) {
    const cr = -2.05 + (x / cols) * 2.8, ci = -1.15 + (y / rows) * 2.3;
    let zr = 0, zi = 0, i = 0;
    while (i < maxIter && zr * zr + zi * zi < 4) {
      const t = zr * zr - zi * zi + cr;
      zi = 2 * zr * zi + ci;
      zr = t;
      i++;
    }
    if (i === maxIter) inside++;
    if (y % 5 === 2 && x % 5 === 2) {
      line += i === maxIter ? "@" : shades[Math.min(shades.length - 2, Math.floor(Math.log2(i + 1) * 1.3))];
    }
  }
  if (line) picture.push(line);
}
console.log(picture.join("\\n"));
console.log("cells inside the set:", inside, "of", cols * rows);` },
  { id: 'sort', title: 'Sort 100k numbers', blurb: 'three sorts of the same data, checked against each other', source: `// Sort 100,000 pseudo-random numbers three ways and check the results agree.
let seed = 12345;
const next = () => (seed = (seed * 1664525 + 1013904223) >>> 0);
const size = 100_000;
const data = Array.from({ length: size }, () => next() % 1_000_000);
const builtin = data.slice().sort((a, b) => a - b);
function quicksort(a, lo, hi) {
  while (lo < hi) {
    const p = a[(lo + hi) >> 1]; let i = lo, j = hi;
    while (i <= j) { while (a[i] < p) i++; while (a[j] > p) j--; if (i <= j) { const t = a[i]; a[i] = a[j]; a[j] = t; i++; j--; } }
    if (j - lo < hi - i) { quicksort(a, lo, j); lo = i; } else { quicksort(a, i, hi); hi = j; }
  }
}
const quick = data.slice(); quicksort(quick, 0, quick.length - 1);
const typed = Float64Array.from(data).sort();
let agree = true;
for (let i = 0; i < size; i += 997) if (builtin[i] !== quick[i] || quick[i] !== typed[i]) agree = false;
console.log("sorted", size, "numbers · min", builtin[0], "· max", builtin[size - 1], "· all three agree:", agree);` },
  { id: 'json', title: 'JSON round trip', blurb: '40,000 records stringified, parsed back and aggregated', source: `// Build 40,000 records, round-trip them through JSON, and aggregate by region.
const regions = ["north", "south", "east", "west"];
const records = [];
for (let i = 0; i < 40_000; i++) {
  records.push({ id: i, region: regions[i & 3], amount: (i * 7919) % 1000 / 10, tags: ["t" + (i % 13), "k" + (i % 7)], active: i % 3 === 0 });
}
const text = JSON.stringify(records);
const parsed = JSON.parse(text);
const totals = new Map();
for (const r of parsed) totals.set(r.region, (totals.get(r.region) ?? 0) + (r.active ? r.amount : 0));
console.log("payload", (text.length / 1024).toFixed(0), "KiB ·", parsed.length, "records");
for (const [region, total] of totals) console.log(region.padEnd(6), total.toFixed(1));` },
  { id: 'fib', title: 'Recursion & closures', blurb: '630,000 recursive calls, memoisation, 200,000 closure calls', source: `// Recursion and closures: naive fibonacci(27), then the same with memoisation.
function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
const memo = new Map();
const fastFib = (n) => { if (n < 2) return n; if (memo.has(n)) return memo.get(n); const v = fastFib(n - 1) + fastFib(n - 2); memo.set(n, v); return v; };
console.log("fib(27) by brute force  =", fib(27), "(≈ 630k calls)");
console.log("fib(90) with memoisation =", fastFib(90));
const counter = (() => { let n = 0; return () => ++n; })();
for (let i = 0; i < 200_000; i++) counter();
console.log("closure called 200,000 times, counter =", counter());` },
  { id: 'text', title: 'Text processing', blurb: 'a 260 KB document tokenised with a global regex and ranked', source: `// Text processing: generate a 260 KB document, tokenise it with a global
// regex, rank the words, and build a frequency table.
const words = ["zipp", "engine", "rust", "sandbox", "script", "fast", "host", "plugin", "rule", "workflow", "browser", "wasm"];
const parts = [];
for (let i = 0; i < 40_000; i++) parts.push(words[(i * 31 + (i >> 3)) % words.length] + (i % 11 === 10 ? ".\\n" : " "));
const doc = parts.join("");
const tokens = doc.toLowerCase().match(/[a-z]+/g);
const counts = new Map();
for (const token of tokens) counts.set(token, (counts.get(token) ?? 0) + 1);
const top = [...counts.entries()].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1));
console.log("characters", doc.length, "· tokens", tokens.length, "· distinct", counts.size, "· lines", doc.split("\\n").length);
for (const [w, n] of top) console.log(w.padEnd(10), String(n).padStart(6), "#".repeat(Math.round(n / 250)));` },
]

const installCommands = `git clone https://github.com/f2i-com/zipp.org.git zipp
cd zipp
cargo build --locked --release
./target/release/zipp js examples/hello.js`

const sandboxCode = `let mut script = compile_script(user_code)?;

// Build zipp-vm with features = ["instrument"]
script.set_limits(5_000_000, Some(abort));
script.set_heap_limit(32 * 1024 * 1024);
script.set_host_call(Box::new(allowed_calls));

script.run_init()?;
script.call_slot(on_event, &[event])?;`

type Engine = 'node' | 'bun' | 'deno' | 'zipp'
type BenchmarkGroup = 'headline' | 'diagnostic'
type BenchmarkFilter = 'all' | BenchmarkGroup
type Suite = 'normal' | 'hostile'

type BenchmarkRow = {
  id: string
  name: string
  /** Normal rows: headline or diagnostic. Hostile rows: the corpus category. */
  group: string
  times: Record<Engine, number>
  nodeRatio: number
}

// Canonical clean PGO capture at engine commit 8229b3fc (2026-09-02): cold wall
// time medians in milliseconds over 15 counterbalanced repetitions, exact
// output on every row. Zipp / Node is the paired median ratio; below 1 is a win.
const benchmarkRows: BenchmarkRow[] = [
  { id: 'async-promise-chain', name: 'Async / promises', group: 'headline', times: { node: 333.709, bun: 369.218, deno: 358.858, zipp: 372.081 }, nodeRatio: 1.117837201 },
  { id: 'class-prototype-hot', name: 'Class / prototype', group: 'headline', times: { node: 296.712, bun: 332.470, deno: 329.464, zipp: 226.104 }, nodeRatio: 0.765493100 },
  { id: 'json-large', name: 'JSON', group: 'headline', times: { node: 269.681, bun: 192.494, deno: 321.981, zipp: 270.987 }, nodeRatio: 1.005076903 },
  { id: 'map-set-heavy', name: 'Map / Set', group: 'headline', times: { node: 783.680, bun: 855.047, deno: 1264.368, zipp: 671.598 }, nodeRatio: 0.836944805 },
  { id: 'markdown-render', name: 'Markdown render', group: 'headline', times: { node: 268.494, bun: 207.260, deno: 315.656, zipp: 208.937 }, nodeRatio: 0.766882616 },
  { id: 'parse-large-js', name: 'Parse JavaScript', group: 'headline', times: { node: 272.752, bun: 230.340, deno: 295.818, zipp: 232.997 }, nodeRatio: 0.858903804 },
  { id: 'polymorphic-objects', name: 'Polymorphic objects', group: 'headline', times: { node: 327.670, bun: 331.062, deno: 339.806, zipp: 309.218 }, nodeRatio: 0.941870413 },
  { id: 'regex-log-scan', name: 'RegExp log scan', group: 'headline', times: { node: 477.981, bun: 564.193, deno: 459.624, zipp: 447.904 }, nodeRatio: 0.937927904 },
  { id: 'sparse-array', name: 'Sparse array', group: 'headline', times: { node: 81.067, bun: 112.802, deno: 129.281, zipp: 73.196 }, nodeRatio: 0.907736876 },
  { id: 'typedarray-math', name: 'TypedArray math', group: 'headline', times: { node: 199.845, bun: 913.841, deno: 169.910, zipp: 144.073 }, nodeRatio: 0.719316384 },
  { id: 'polymorphic-objects-v2', name: 'Polymorphic objects v2', group: 'diagnostic', times: { node: 81.101, bun: 87.345, deno: 131.428, zipp: 24.475 }, nodeRatio: 0.301551988 },
  { id: 'property-ic-shapes', name: 'Property IC shapes', group: 'diagnostic', times: { node: 265.405, bun: 157.550, deno: 318.663, zipp: 9.578 }, nodeRatio: 0.036206078 },
  { id: 'sparse-array-v2', name: 'Sparse array v2', group: 'diagnostic', times: { node: 171.083, bun: 366.301, deno: 183.858, zipp: 99.229 }, nodeRatio: 0.585244499 },
]

// The 17-case hostile corpus from the same capture: closures, mixed locals,
// shape churn, GC survival, async lifetimes, modules, a React-shaped kernel, a
// warm router, a bytecode VM and vendored NanoID.
const hostileRows: BenchmarkRow[] = [
  { id: 'calls-baseline', name: 'Calls baseline', group: 'scope', times: { node: 35.011, bun: 48.041, deno: 89.448, zipp: 16.645 }, nodeRatio: 0.484551966 },
  { id: 'calls-closures', name: 'Closure calls', group: 'scope', times: { node: 40.840, bun: 54.905, deno: 95.804, zipp: 45.099 }, nodeRatio: 1.117471074 },
  { id: 'shapes-stable', name: 'Stable shapes', group: 'objects', times: { node: 39.412, bun: 60.900, deno: 93.161, zipp: 49.122 }, nodeRatio: 1.240556978 },
  { id: 'shapes-megamorphic', name: 'Megamorphic shapes', group: 'objects', times: { node: 46.973, bun: 66.623, deno: 102.152, zipp: 57.708 }, nodeRatio: 1.244923066 },
  { id: 'types-stable', name: 'Stable types', group: 'types', times: { node: 35.607, bun: 49.166, deno: 91.632, zipp: 18.132 }, nodeRatio: 0.513822180 },
  { id: 'types-churn', name: 'Type churn', group: 'types', times: { node: 44.496, bun: 61.416, deno: 98.173, zipp: 32.367 }, nodeRatio: 0.739798825 },
  { id: 'branch-control', name: 'Branch control', group: 'errors', times: { node: 39.439, bun: 50.227, deno: 93.166, zipp: 31.451 }, nodeRatio: 0.828287808 },
  { id: 'throw-catch', name: 'Throw / catch', group: 'errors', times: { node: 310.219, bun: 94.378, deno: 104.759, zipp: 154.263 }, nodeRatio: 0.497572259 },
  { id: 'allocation-ephemeral', name: 'Ephemeral allocation', group: 'allocation', times: { node: 35.324, bun: 70.550, deno: 94.108, zipp: 12.778 }, nodeRatio: 0.359466993 },
  { id: 'allocation-survival', name: 'Allocation survival', group: 'allocation', times: { node: 57.644, bun: 74.927, deno: 111.657, zipp: 88.851 }, nodeRatio: 1.558883643 },
  { id: 'async-burst', name: 'Async burst', group: 'async', times: { node: 53.767, bun: 53.846, deno: 106.954, zipp: 33.021 }, nodeRatio: 0.612580589 },
  { id: 'async-lived', name: 'Long-lived async', group: 'async', times: { node: 40.254, bun: 66.679, deno: 93.237, zipp: 40.176 }, nodeRatio: 1.005144446 },
  { id: 'reactish-reconcile', name: 'React-shaped reconcile', group: 'applications', times: { node: 44.927, bun: 66.858, deno: 97.706, zipp: 69.853 }, nodeRatio: 1.578095226 },
  { id: 'warm-router', name: 'Warm router', group: 'server', times: { node: 45.753, bun: 69.834, deno: 98.819, zipp: 69.247 }, nodeRatio: 1.520160112 },
  { id: 'bytecode-vm', name: 'Bytecode VM', group: 'endurance', times: { node: 44.155, bun: 57.216, deno: 97.122, zipp: 42.976 }, nodeRatio: 0.978347456 },
  { id: 'module-hot-graph', name: 'Hot module graph', group: 'modules', times: { node: 40.302, bun: 51.111, deno: 92.933, zipp: 16.179 }, nodeRatio: 0.401317326 },
  { id: 'npm-nanoid', name: 'npm nanoid', group: 'npm', times: { node: 84.057, bun: 113.397, deno: 126.571, zipp: 81.913 }, nodeRatio: 0.974690733 },
]

const nodeWins = (rows: BenchmarkRow[]) => rows.filter((row) => row.nodeRatio < 1).length
const nodeGaps = (rows: BenchmarkRow[]) =>
  rows.filter((row) => row.nodeRatio >= 1).sort((a, b) => b.nodeRatio - a.nodeRatio)

const readingGuide = [
  {
    title: 'The ratio is Zipp divided by Node',
    copy: 'Each row runs Node, Bun, Deno and Zipp in a shuffled order, 15 times each, and pairs the medians. 0.72× means Zipp finished in 72% of Node’s time; anything above 1× is a gap we still owe.',
  },
  {
    title: 'Cold time, exact output',
    copy: 'Every number includes process launch, and a row only counts when all four engines print byte-identical output. Zipp’s 7.4 ms launch is real, but the ratios are about the work, not the start.',
  },
  {
    title: 'One number for the whole picture',
    copy: 'The all-30 figure gives every normal and hostile row equal weight and reports a descriptive bootstrap interval. It is a summary, not a proof of universal speed — the table is the evidence.',
  },
]

const useCases = [
  {
    number: '01',
    eyebrow: 'User-authored code',
    title: 'Sandbox-oriented scripts',
    copy: 'Run customer rules inside a VM built for bounded execution. Add instruction budgets, a host-driven abort flag, and an approximate heap ceiling when you enable instrumentation.',
    tags: ['Step budgets', 'Abort signal', 'Heap indicator'],
  },
  {
    number: '02',
    eyebrow: 'Extensibility',
    title: 'Plugin runtimes',
    copy: 'Keep one VM alive, discover stable global slots, and call plugin functions without recompiling. Scripts only reach the host capabilities you deliberately install.',
    tags: ['Persistent state', 'Slot calls', 'Host bridge'],
  },
  {
    number: '03',
    eyebrow: 'Product logic',
    title: 'Rules and workflows',
    copy: 'Move scoring, transforms, policy, and workflow steps out of release cycles. JavaScript stays familiar while your Rust host owns data, side effects, and lifecycle.',
    tags: ['Rules', 'Transforms', 'Automation'],
  },
  {
    number: '04',
    eyebrow: 'Everywhere else',
    title: 'CLI and browser hosts',
    copy: 'Use the fast-starting native binary for trusted jobs, or bring a persistent interpreter to browser hosts through the wasm-bindgen package.',
    tags: ['Native CLI', 'WASM', 'Browser events'],
  },
]

const controls = [
  {
    number: '01',
    title: 'Capabilities start closed',
    copy: 'The host-call bridge is inert until your embedder installs it. Embedded scripts get no ambient Node, Bun, Deno, filesystem, or network API.',
  },
  {
    number: '02',
    title: 'Meter and interrupt',
    copy: 'Optional instrumentation charges matching bytecode units, polls a host abort flag, and reports work used. On x86-64 it stays consistent across interpreter and JIT execution.',
  },
  {
    number: '03',
    title: 'Keep state, not recompiles',
    copy: 'Compile once, run initialization, exchange structured data, call stable function slots, and pump microtasks for the life of the host.',
  },
]

const pipeline = [
  ['01', 'Source', 'Modern JavaScript'],
  ['02', 'Front end', 'Own lexer + parser'],
  ['03', 'Bytecode', 'Register-based VM'],
  ['04', 'Hot loops', 'x86-64 OSR JIT'],
]

function ArrowIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M4 12 12 4M6 4h6v6" />
    </svg>
  )
}

function Brand() {
  const gradientId = `zipp-bolt-${useId().replace(/[^a-zA-Z0-9_-]/g, '')}`

  return (
    <span className="brand-lockup">
      <svg className="brand-symbol" viewBox="0 0 142 208" aria-hidden="true">
        <defs>
          <linearGradient id={gradientId} x1="0" y1="0" x2="1" y2="1">
            <stop offset="0" stopColor="#c4b5fd" />
            <stop offset="0.5" stopColor="#7c3aed" />
            <stop offset="1" stopColor="#22d3ee" />
          </linearGradient>
        </defs>
        <path d="M92 0 0 122h58l-25 86L142 69h-62z" fill={`url(#${gradientId})`} />
      </svg>
      <span>Zipp</span>
    </span>
  )
}

function ExternalLink({ className, href, children }: { className?: string; href: string; children: ReactNode }) {
  return (
    <a className={className} href={href} target="_blank" rel="noreferrer">
      {children}
      <ArrowIcon />
    </a>
  )
}

type PlaygroundStatus = 'idle' | 'loading' | 'running' | 'success' | 'error' | 'timeout'

type PlaygroundWorkerMessage =
  | { type: 'started'; runId: number }
  | { type: 'result'; runId: number; output: string[]; elapsedMs: number }
  | { type: 'error'; runId: number; message: string }

function Playground() {
  const [exampleId, setExampleId] = useState(playgroundExamples[0].id)
  const [source, setSource] = useState(playgroundExamples[0].source)
  const [output, setOutput] = useState('Run the sample to see console output from Zipp WASM.')
  const [status, setStatus] = useState<PlaygroundStatus>('idle')
  const [elapsedMs, setElapsedMs] = useState<number | null>(null)
  const workerRef = useRef<Worker | null>(null)
  const timerRef = useRef<number | undefined>(undefined)
  const runIdRef = useRef(0)
  // A Worker that has already loaded the module and executed some JavaScript, so
  // the WebAssembly functions a run needs are compiled before the click rather
  // than during it. It is handed to the next run and immediately replaced — a run
  // still gets a Worker of its own, so terminating one on a deadline still
  // discards everything that run touched.
  const spareRef = useRef<Worker | null>(null)
  const spareReadyRef = useRef(false)

  const packageBase = () => new URL(`${import.meta.env.BASE_URL}wasm/`, document.baseURI)

  // The glue and the .wasm are hash-matched halves of one artifact; a cache that
  // serves a new .wasm beside an older glue fails instantiation with "function
  // import requires a callable". Neither filename is fingerprinted (they are
  // copied verbatim out of public/), so the build id is what keeps the pair
  // together — it changes whenever either file does.
  const wasmUrls = () => {
    const base = packageBase()
    const v = `?v=${__ZIPP_WASM_BUILD__}`
    return {
      moduleUrl: new URL(`zipp_wasm.js${v}`, base).href,
      wasmUrl: new URL(`zipp_wasm_bg.wasm${v}`, base).href,
    }
  }

  const stopWorker = () => {
    window.clearTimeout(timerRef.current)
    timerRef.current = undefined
    workerRef.current?.terminate()
    workerRef.current = null
  }

  const discardSpare = () => {
    spareRef.current?.terminate()
    spareRef.current = null
    spareReadyRef.current = false
  }

  // V8 compiles this module's WebAssembly lazily, on first call of each function,
  // which measured ~40 ms against ~1.2 ms for the sample's actual work. Paying it
  // on an idle Worker ahead of time is the whole difference.
  const prewarm = () => {
    if (spareRef.current) return
    const worker = new Worker(new URL('./playground.worker.ts', import.meta.url), { type: 'module' })
    spareRef.current = worker
    spareReadyRef.current = false
    worker.onmessage = (event: MessageEvent<{ type?: string }>) => {
      if (event.data?.type === 'warmed') spareReadyRef.current = true
      // A warm-up that fails is not an error the reader should see: the run path
      // loads the module itself and will surface anything real.
      else if (event.data?.type === 'warm-failed') discardSpare()
    }
    worker.onerror = () => discardSpare()
    worker.postMessage({ type: 'warm', ...wasmUrls() })
  }

  // Warm on intent rather than on mount, so a visitor who never touches the
  // playground is not made to download and compile 5.7 MB to scroll past it.
  useEffect(() => {
    const idle = window.requestIdleCallback?.bind(window)
    const handle = idle ? idle(() => prewarm(), { timeout: 4000 }) : undefined
    return () => {
      if (handle !== undefined) window.cancelIdleCallback?.(handle)
    }
  }, [])

  useEffect(() => () => {
    stopWorker()
    discardSpare()
  }, [])

  const armTimeout = (runId: number, delay: number, phase: 'boot' | 'run') => {
    window.clearTimeout(timerRef.current)
    timerRef.current = window.setTimeout(() => {
      if (runId !== runIdRef.current) return
      stopWorker()
      setStatus(phase === 'boot' ? 'error' : 'timeout')
      setElapsedMs(null)
      setOutput(phase === 'boot'
        ? 'Zipp WASM did not finish loading. Check the connection and try again.'
        : `Execution stopped after ${(PLAYGROUND_RUN_TIMEOUT_MS / 1000).toFixed(1)} seconds. The Worker was discarded.`)
    }, delay)
  }

  const runSource = () => {
    stopWorker()
    const runId = ++runIdRef.current

    // Take the pre-warmed Worker if there is one, then start warming its
    // replacement straight away so a second Run is as quick as the first.
    const warmed = spareReadyRef.current
    const worker = spareRef.current ?? new Worker(new URL('./playground.worker.ts', import.meta.url), { type: 'module' })
    spareRef.current = null
    spareReadyRef.current = false

    workerRef.current = worker
    setStatus('loading')
    setElapsedMs(null)
    setOutput(warmed ? 'Running in an isolated Worker…' : 'Loading the browser-safe Zipp runtime…')
    armTimeout(runId, warmed ? PLAYGROUND_RUN_TIMEOUT_MS : PLAYGROUND_BOOT_TIMEOUT_MS, warmed ? 'run' : 'boot')

    worker.onmessage = (event: MessageEvent<PlaygroundWorkerMessage>) => {
      const message = event.data
      if (message.runId !== runIdRef.current) return

      if (message.type === 'started') {
        setStatus('running')
        setOutput('Running in an isolated Worker…')
        armTimeout(runId, PLAYGROUND_RUN_TIMEOUT_MS, 'run')
        return
      }

      stopWorker()
      if (message.type === 'result') {
        setStatus('success')
        setElapsedMs(message.elapsedMs)
        setOutput(message.output.length > 0 ? message.output.join('\n') : '(script completed with no console output)')
      } else {
        setStatus('error')
        setElapsedMs(null)
        setOutput(message.message)
      }
    }

    worker.onerror = (event) => {
      if (runId !== runIdRef.current) return
      stopWorker()
      setStatus('error')
      setElapsedMs(null)
      setOutput(event.message || 'The Zipp Worker could not start.')
    }

    const storySeed = exampleId === 'adventure' ? crypto.getRandomValues(new Uint32Array(1))[0] : undefined
    worker.postMessage({ type: 'run', runId, source, storySeed, ...wasmUrls() })

    prewarm()
  }

  const currentExample = playgroundExamples.find((example) => example.id === exampleId) ?? playgroundExamples[0]

  const selectExample = (id: string) => {
    const example = playgroundExamples.find((candidate) => candidate.id === id)
    if (!example) return
    ++runIdRef.current
    stopWorker()
    setExampleId(id)
    setSource(example.source)
    setStatus('idle')
    setElapsedMs(null)
    setOutput(`Run "${example.title}" to see console output from Zipp WASM.`)
  }

  const resetSource = () => selectExample(exampleId)

  const statusLabel = {
    idle: 'ready',
    loading: 'loading WASM',
    running: 'running',
    success: elapsedMs === null ? 'complete' : `complete · ${elapsedMs.toFixed(1)} ms`,
    error: 'error',
    timeout: 'stopped',
  }[status]

  return (
    <section className="playground-section section-wrap" id="playground">
      <div className="playground-heading">
        <div>
          <p className="section-kicker">LESS TALK. MORE TINKERING.</p>
          <h2>Your code.<br />ZIPP’s sandbox.</h2>
        </div>
        <p>
          Generate a little adventure, find a million primes, or bring your own idea. Pick a sample,
          change the code, and hit Run. This is the real ZIPP WASM engine executing
          JavaScript inside your browser — with a sandbox of its own.
        </p>
      </div>

      <div className="example-picker" role="group" aria-label="Choose an example">
        {playgroundExamples.map((example) => (
          <button
            key={example.id}
            type="button"
            aria-pressed={example.id === exampleId}
            className={example.id === exampleId ? 'active' : ''}
            onClick={() => selectExample(example.id)}
          >
            <strong>{example.title}</strong>
            <span>{example.blurb}</span>
          </button>
        ))}
      </div>

      <div className="playground-shell">
        <div className="playground-pane playground-editor-pane">
          <div className="playground-toolbar">
            <div>
              <span className="terminal-dots" aria-hidden="true"><i /><i /><i /></span>
              <span>{currentExample.id}.js</span>
            </div>
            <span className={`playground-status status-${status}`}><i />{statusLabel}</span>
          </div>
          <label className="sr-only" htmlFor="playground-source">JavaScript source</label>
          <textarea
            id="playground-source"
            value={source}
            spellCheck={false}
            onChange={(event) => setSource(event.target.value)}
            onKeyDown={(event) => {
              if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
                event.preventDefault()
                runSource()
              }
            }}
          />
          <div className="playground-actions">
            <button className="button playground-run" type="button" onClick={runSource} disabled={status === 'loading' || status === 'running'}>
              {status === 'loading' ? 'Loading…' : status === 'running' ? 'Running…' : '▶ Run with ZIPP'}
              <span aria-hidden="true">Ctrl/⌘ + Enter</span>
            </button>
            <button className="playground-reset" type="button" onClick={resetSource}>Reset example</button>
          </div>
        </div>

        <div className="playground-pane playground-output-pane">
          <div className="playground-toolbar">
            <div><span className="output-mark" aria-hidden="true">›_</span><span>Console output</span></div>
            <span>WASM · safe-sandbox</span>
          </div>
          <pre tabIndex={0} aria-live="polite" aria-label="Zipp console output"><code>{output}</code></pre>
          <div className="playground-boundary">
            <span><i />50m instruction lifetime cap</span>
            <span><i />128 MiB VM heap ceiling</span>
            <span><i />6 s host deadline</span>
          </div>
        </div>
      </div>
      <p className="playground-build-note">Bundled engine: <a href={`${GITHUB_URL}/releases/tag/v0.0.15`} target="_blank" rel="noreferrer">ZIPP WASM v0.0.15</a>. Each run uses a disposable Worker with a host-enforced deadline. Repository updates do not swap the engine underneath your code.</p>
    </section>
  )
}

function App() {
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'error'>('idle')
  const [menuOpen, setMenuOpen] = useState(false)
  const [benchmarkFilter, setBenchmarkFilter] = useState<BenchmarkFilter>('all')
  const [suite, setSuite] = useState<Suite>('normal')
  const [scrolled, setScrolled] = useState(false)
  const resetTimer = useRef<number | undefined>(undefined)
  const { stats, status: repoStatus, refreshing, refresh } = useRepoStats()
  useReveal()
  useSpotlight()
  useInitialHashNavigation()

  useEffect(() => {
    const onScroll = () => setScrolled(window.scrollY > 12)
    onScroll()
    window.addEventListener('scroll', onScroll, { passive: true })
    return () => window.removeEventListener('scroll', onScroll)
  }, [])

  // These figures belong to the pinned 2 September capture, not today's release.
  const liveWins = nodeWins(benchmarkRows) + nodeWins(hostileRows)
  const liveAll30 = 0.728
  const liveStartup = 7.4

  const visibleBenchmarks = useMemo(
    () =>
      suite === 'hostile'
        ? hostileRows
        : benchmarkRows.filter((row) => benchmarkFilter === 'all' || row.group === benchmarkFilter),
    [benchmarkFilter, suite],
  )
  const gaps = useMemo(() => [...nodeGaps(benchmarkRows), ...nodeGaps(hostileRows)], [])

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setMenuOpen(false)
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => {
      window.removeEventListener('keydown', handleKeyDown)
      window.clearTimeout(resetTimer.current)
    }
  }, [])

  const copyInstall = async () => {
    try {
      await navigator.clipboard.writeText(installCommands)
      setCopyState('copied')
    } catch {
      setCopyState('error')
    }
    window.clearTimeout(resetTimer.current)
    resetTimer.current = window.setTimeout(() => setCopyState('idle'), 2200)
  }

  const closeMenu = () => setMenuOpen(false)

  return (
    <div className="site-shell">
      <a className="skip-link" href="#main-content">Skip to content</a>

      <header className={`site-header ${scrolled ? 'scrolled' : ''}`}>
        <a className="brand" href="#top" aria-label="Zipp home" onClick={closeMenu}>
          <Brand />
        </a>

        <button
          className="menu-button"
          type="button"
          aria-label="Toggle navigation"
          aria-expanded={menuOpen}
          aria-controls="primary-navigation"
          onClick={() => setMenuOpen((open) => !open)}
        >
          <span />
          <span />
        </button>

        <nav className={`nav-links ${menuOpen ? 'nav-open' : ''}`} id="primary-navigation" aria-label="Primary navigation">
          <a href="#playground" onClick={closeMenu}>Playground</a>
          <a href="#use-cases" onClick={closeMenu}>Use cases</a>
          <a href="#updates" onClick={closeMenu}>What’s new</a>
          <a href="#controls" onClick={closeMenu}>Controls</a>
          <a href="#benchmarks" onClick={closeMenu}>Benchmarks</a>
          <a href="#architecture" onClick={closeMenu}>Engine</a>
          <a className="nav-star" href={GITHUB_URL} target="_blank" rel="noreferrer" onClick={closeMenu}>
            <StarIcon /> Star on GitHub{stats?.stars ? ` · ${formatCount(stats.stars)}` : ''}
          </a>
        </nav>

        <a className="header-cta star-cta" href={GITHUB_URL} target="_blank" rel="noreferrer" aria-label="Star Zipp on GitHub">
          <StarIcon />
          <span>Star</span>
          {stats?.stars ? <b>{formatCount(stats.stars)}</b> : null}
        </a>
      </header>

      <main id="main-content">
        <section className="hero section-wrap" id="top">
          <div className="hero-copy">
            <a className="result-pill" href="#use-cases">
              <span>BUILT TO BE EMBEDDED</span>
              <strong>Already at play in Softn</strong>
              <span aria-hidden="true">↗</span>
            </a>

            <h1>
              Big ideas.<br /><em>Tiny sandbox.</em>
            </h1>

            <p className="hero-intro">
              Give the code your users bring a place to play. ZIPP is a fast,
              embeddable JavaScript engine built in Rust. Run it natively, or use
              <strong> ZIPP WASM to run sandboxed JavaScript on top of JavaScript.</strong>
            </p>

            <div className="hero-actions">
              <a className="button button-primary" href="#playground">Let’s play with JavaScript <span aria-hidden="true">↗</span></a>
              <ExternalLink className="button button-secondary" href={GITHUB_URL}>Explore on GitHub</ExternalLink>
            </div>

            <div className="hero-trust" aria-label="Zipp highlights">
              <a href={`${GITHUB_URL}/blob/${stats.sourceCommit ?? 'main'}/README.md`} target="_blank" rel="noreferrer" title="Compatibility reported by the repository README">{stats.test262Pct !== undefined ? `${stats.test262Pct}% test262` : 'ECMAScript compatibility'}</a>
              <span><i />Native + WASM</span>
              <a href={`${GITHUB_URL}/blob/main/LICENSE-APACHE`} target="_blank" rel="noreferrer">Open source · {stats.license ?? 'Apache-2.0'}</a>
            </div>

            <LiveRepoStrip stats={stats} status={repoStatus} />
          </div>

          <SandboxStack />
        </section>

        <section className="proof-band" aria-label="Measured native Zipp results">
          <div className="proof-caption section-wrap"><span>NATIVE BENCHMARK SNAPSHOT</span><a href="#benchmarks">2 Sep 2026 · v0.0.12 · see the evidence ↗</a></div>
          <div className="section-wrap proof-grid">
            <div className="proof-lead">
              <span className="metric-index">01</span>
              <CountUp value={liveWins} format={(v) => `${Math.round(v)} / 30`} />
              <p>native rows faster than Node</p>
            </div>
            <div>
              <span className="metric-index">02</span>
              <CountUp value={liveAll30} format={(v) => `${v.toFixed(3)}×`} />
              <p>native Zipp / Node · equal-row all 30</p>
            </div>
            <div>
              <span className="metric-index">03</span>
              <CountUp value={liveStartup} format={(v) => `${v.toFixed(1)} ms`} />
              <p>median native process launch</p>
            </div>
            <div>
              <span className="metric-index">04</span>
              <CountUp value={30} format={(v) => `${Math.round(v)} / 30`} />
              <p>exact-output parity</p>
            </div>
          </div>
        </section>

        <ProjectShowcase />

        <RepositoryActivity stats={stats} status={repoStatus} refreshing={refreshing} refresh={refresh} />

        <Playground />

        <section className="use-case-section section-wrap" id="possibilities">
          <div className="section-heading split-heading">
            <div>
              <p className="section-kicker">Built for the edge of trust</p>
              <h2>Put JavaScript where your users already think.</h2>
            </div>
            <p>
              Give product teams a familiar language without handing scripts your whole
              application. Zipp keeps the engine small, the host boundary explicit, and the
              hot path fast.
            </p>
          </div>

          <div className="use-case-grid">
            {useCases.map((useCase) => (
              <article className="use-case-card" key={useCase.number}>
                <div className="card-number">{useCase.number}</div>
                <p className="card-eyebrow">{useCase.eyebrow}</p>
                <h3>{useCase.title}</h3>
                <p>{useCase.copy}</p>
                <div className="tag-row">
                  {useCase.tags.map((tag) => <span key={tag}>{tag}</span>)}
                </div>
              </article>
            ))}
          </div>
        </section>

        <section className="controls-section" id="controls">
          <div className="section-wrap controls-layout">
            <div className="controls-copy">
              <p className="section-kicker">Capability-controlled embedding</p>
              <h2>A script boundary you can reason about.</h2>
              <p className="controls-intro">
                Embedded code cannot wander into your host by accident. Install the calls it
                may make, define the data that crosses, and choose how much work one session gets.
              </p>

              <div className="control-list">
                {controls.map((control) => (
                  <article key={control.number}>
                    <span>{control.number}</span>
                    <div>
                      <h3>{control.title}</h3>
                      <p>{control.copy}</p>
                    </div>
                  </article>
                ))}
              </div>

              <div className="security-note">
                <span aria-hidden="true">!</span>
                <p><strong>One layer, honestly described.</strong> Use the separately resolved, no-JIT <code>zipp-sandbox</code> runner for hostile native code, then add OS/process isolation when the threat model requires it.</p>
              </div>
            </div>

            <div className="code-window" aria-label="Rust embedding example">
              <div className="code-window-header">
                <span className="terminal-dots" aria-hidden="true"><i /><i /><i /></span>
                <span>src / sandbox.rs</span>
                <span>Rust</span>
              </div>
              <pre><code>{sandboxCode}</code></pre>
              <div className="code-window-status">
                <span><i /> host surface</span><strong>explicit</strong>
                <span><i /> VM state</span><strong>persistent</strong>
                <span><i /> execution meter</span><strong>enabled</strong>
              </div>
              <ExternalLink className="text-link" href={DOCS_URL}>Read the embedding guide</ExternalLink>
            </div>
          </div>
        </section>

        <section className="benchmark-section section-wrap" id="benchmarks">
          <div className="benchmark-heading">
            <div>
              <p className="section-kicker">Measured native performance</p>
              <h2>Fast where it counts. Honest where work remains.</h2>
            </div>
            <div className="benchmark-statement">
              <strong>{nodeWins(benchmarkRows) + nodeWins(hostileRows)}<span>/30</span></strong>
              <p>rows faster than Node · every gap visible</p>
            </div>
          </div>

          <p className="benchmark-capture-note">Measured 2 September 2026 · native Windows x86-64 · engine <code>8229b3fc</code>. These are retained benchmark results, not measurements of the latest release or browser playground. <a href={CAPTURE_README_URL} target="_blank" rel="noreferrer">Read the capture summary ↗</a></p>
          <div className="benchmark-summary">
            <article className="headline-result">
              <div>
                <span>Native equal-row all-30 headline</span>
                <strong>0.7278×</strong>
                <p>Native Zipp / Node paired geomean · lower is better</p>
              </div>
              <div className="confidence-pill">95% interval&nbsp; 0.723–0.730</div>
            </article>

            <article className="ratio-card">
              <span>Native all-30 paired geomeans</span>
              <div className="ratio-row"><b>vs Node</b><span><i className="bar-geomean-node" /></span><strong>0.7278×</strong></div>
              <div className="ratio-row"><b>vs Bun</b><span><i className="bar-geomean-bun" /></span><strong>0.5936×</strong></div>
              <div className="ratio-row"><b>vs Deno</b><span><i className="bar-geomean-deno" /></span><strong>0.4604×</strong></div>
              <small>95% intervals: Node 0.723–0.730 · Bun 0.591–0.598 · Deno 0.458–0.464. Normal 13 + hostile 17; equal weight per row. Reported in the capture README.</small>
            </article>

            <article className="ratio-card suite-card">
              <span>Suite geomeans vs Node</span>
              <div className="ratio-row"><b>Normal 13</b><span><i className="bar-suite-normal" /></span><strong>0.6136×</strong></div>
              <div className="ratio-row"><b>Hostile 17</b><span><i className="bar-suite-hostile" /></span><strong>0.8293×</strong></div>
              <small>95% intervals: normal 0.611–0.617 · hostile 0.820–0.833. Rows faster than Node: {nodeWins(benchmarkRows)}/13 + {nodeWins(hostileRows)}/17.</small>
            </article>
          </div>

          <div className="reading-guide" aria-label="How to read the benchmark numbers">
            {readingGuide.map((item, index) => (
              <article key={item.title}>
                <span>0{index + 1}</span>
                <h3>{item.title}</h3>
                <p>{item.copy}</p>
              </article>
            ))}
          </div>

          <div className="scoreboard">
            <div className="scoreboard-toolbar">
              <div>
                <p>
                  {suite === 'normal' ? 'Canonical native normal 13' : 'Canonical native hostile 17'} · cold wall time
                  <span>milliseconds · lower is better</span>
                </p>
              </div>
              <div className="scoreboard-controls">
                <div className="filter-tabs suite-tabs" role="group" aria-label="Choose a benchmark suite">
                  {([
                    ['normal', 'Normal suite'],
                    ['hostile', 'Hostile suite'],
                  ] as const).map(([value, label]) => (
                    <button
                      key={value}
                      type="button"
                      className={suite === value ? 'active' : ''}
                      aria-pressed={suite === value}
                      onClick={() => setSuite(value)}
                    >
                      {label}
                    </button>
                  ))}
                </div>
                {suite === 'normal' && (
                  <div className="filter-tabs" role="group" aria-label="Filter benchmark rows">
                    {([
                      ['all', 'All 13'],
                      ['headline', 'Headline 10'],
                      ['diagnostic', 'Diagnostics 3'],
                    ] as const).map(([value, label]) => (
                      <button
                        key={value}
                        type="button"
                        className={benchmarkFilter === value ? 'active' : ''}
                        aria-pressed={benchmarkFilter === value}
                        onClick={() => setBenchmarkFilter(value)}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            </div>

            <div className="benchmark-table-wrap">
              <table className="benchmark-table">
                <caption>Canonical native cold wall-time medians for Zipp, Node, Bun, and Deno</caption>
                <thead>
                  <tr>
                    <th scope="col">Workload</th>
                    <th scope="col" className="zipp-column">Zipp <span>focus</span></th>
                    <th scope="col">Node</th>
                    <th scope="col">Bun</th>
                    <th scope="col">Deno</th>
                    <th scope="col" className="bar-column">Zipp vs Node <span>relative time</span></th>
                    <th scope="col">Zipp / Node</th>
                  </tr>
                </thead>
                <tbody>
                  {visibleBenchmarks.map((row) => {
                    return (
                      <tr key={row.id}>
                        <th scope="row">
                          <span>{row.name}</span>
                          <small>{suite === 'normal' ? (row.group === 'headline' ? 'Headline' : 'Diagnostic') : row.group}</small>
                        </th>
                        <td className="zipp-time" data-label="Zipp"><strong>{row.times.zipp.toFixed(3)}</strong><span className="sr-only"> milliseconds</span></td>
                        <td data-label="Node">{row.times.node.toFixed(3)}</td>
                        <td data-label="Bun">{row.times.bun.toFixed(3)}</td>
                        <td data-label="Deno">{row.times.deno.toFixed(3)}</td>
                        <td className="bar-cell" data-label="Zipp vs Node" aria-hidden="true">
                          {(() => {
                            const max = Math.max(row.times.zipp, row.times.node)
                            return (
                              <div className="row-bars">
                                <span className="row-bar row-bar-zipp" style={{ width: `${(row.times.zipp / max) * 100}%` }} />
                                <span className="row-bar row-bar-node" style={{ width: `${(row.times.node / max) * 100}%` }} />
                              </div>
                            )
                          })()}
                        </td>
                        <td className="lead-cell" data-label="Zipp divided by Node">
                          <strong className={row.nodeRatio < 1 ? 'ratio-win' : 'ratio-gap'}>{row.nodeRatio.toFixed(3)}×</strong>
                          <span>{row.nodeRatio < 1 ? 'faster than Node' : 'slower than Node'}</span>
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          </div>

          <div className="methodology-note">
            <span className="methodology-mark">i</span>
            <div>
              <p>
                Native Windows x86-64 CLI, high-performance power mode. Cold wall time includes process launch;
                15 paired repetitions with deterministically shuffled engine and benchmark order;
                10,000 paired-bootstrap samples; exact-byte outputs. Node 24.12.0, Bun 1.3.14,
                Deno 2.6.10, Zipp 0.0.12 at clean PGO source <code>8229b3fc</code>; binary SHA-256
                <code>bf9fddab…dc9986</code>. Median startup: Zipp 7.4 ms, Node 30.4 ms, Bun 43.3 ms,
                Deno 82.6 ms. The all-30 result gives equal weight to all normal and hostile rows;
                its bootstrap intervals are descriptive. Ratios above one remain point gaps even when an
                interval crosses one. These native workloads are evidence, not a claim of universal
                runtime superiority; they are not browser-WASM results.
              </p>
              <p className="gap-list">
                <strong>Rows still behind Node ({gaps.length}):</strong>{' '}
                {gaps.map((row, index) => (
                  <span key={row.id}>
                    {row.id} {row.nodeRatio.toFixed(3)}×{index < gaps.length - 1 ? ', ' : '.'}
                  </span>
                ))}
              </p>
            </div>
            <div className="methodology-links">
              <ExternalLink className="text-link" href={BENCHMARK_URL}>Normal capture</ExternalLink>
              <ExternalLink className="text-link" href={HOSTILE_BENCHMARK_URL}>Hostile capture</ExternalLink>
              <ExternalLink className="text-link" href={CAPTURE_README_URL}>Aggregate & intervals</ExternalLink>
              <ExternalLink className="text-link" href={ROADMAP_URL}>What is next</ExternalLink>
            </div>
          </div>

        </section>

        <section className="architecture-section" id="architecture">
          <div className="section-wrap">
            <div className="section-heading split-heading">
              <div>
                <p className="section-kicker">Clean-sheet core</p>
                <h2>Own the path from source to native code.</h2>
              </div>
              <p>
                Zipp’s lexer, parser, bytecode compiler, register VM, collector, inline caches,
                and native JIT live in this repository. No parser or runtime is hiding in the middle.
              </p>
            </div>

            <ol className="pipeline" aria-label="Zipp execution pipeline">
              {pipeline.map(([number, title, detail], index) => (
                <li key={number}>
                  <span>{number}</span>
                  <div><strong>{title}</strong><small>{detail}</small></div>
                  {index < pipeline.length - 1 && <i aria-hidden="true">→</i>}
                </li>
              ))}
            </ol>

            <div className="runtime-grid">
              <article>
                <span className="runtime-platform">Native / x86-64</span>
                <h3>OSR JIT when code gets hot.</h3>
                <p>Start in the interpreter, compile hot loops into native code, and keep optional execution metering consistent across both tiers.</p>
                <div><span>Persistent VM</span><span>Native JIT</span><span>Host controls</span></div>
              </article>
              <article>
                <span className="runtime-platform">Native / aarch64</span>
                <h3>A guarded baseline for integer hot paths.</h3>
                <p>Bounded call-free integer functions and numeric loops can run natively, with exact-instruction fallback whenever a guard declines.</p>
                <div><span>Baseline JIT</span><span>Register VM</span><span>Exact fallback</span></div>
              </article>
              <article>
                <span className="runtime-platform">Browser / wasm32</span>
                <h3>Persistent scripts in the browser.</h3>
                <p>The wasm-bindgen Engine keeps state, calls functions, delivers events, and crosses structured data through a browser host.</p>
                <div><span>Interpreter</span><span>Structured values</span><span>Event bridge</span></div>
              </article>
            </div>
          </div>
        </section>

        <section className="quickstart-section section-wrap" id="quickstart">
          <div className="quickstart-copy">
            <p className="section-kicker">Run it locally</p>
            <h2>Four lines from source to JavaScript.</h2>
            <p>Build the native CLI with stable Rust, run trusted scripts and modules, then move to the embedding API when your host needs a live VM.</p>
            <div className="quickstart-links">
              <ExternalLink className="text-link" href={DOCS_URL}>Embedding docs</ExternalLink>
              <ExternalLink className="text-link" href={GITHUB_URL}>Browse the source</ExternalLink>
            </div>
          </div>

          <div className="terminal-block">
            <div className="terminal-header">
              <span className="terminal-dots" aria-hidden="true"><i /><i /><i /></span>
              <span>Terminal</span>
              <button type="button" onClick={copyInstall}>{copyState === 'copied' ? 'Copied' : copyState === 'error' ? 'Copy failed' : 'Copy'}</button>
            </div>
            <pre><code>{installCommands}</code></pre>
            <div className="terminal-output"><span>↳</span> hello, world</div>
            <div className="sr-only" role="status" aria-live="polite">
              {copyState === 'copied' ? 'Install commands copied to clipboard.' : copyState === 'error' ? 'Could not copy install commands.' : ''}
            </div>
          </div>
        </section>

        <section className="closing-cta section-wrap">
          <div>
            <p className="section-kicker">Fast. Explicit. Yours to embed.</p>
            <h2>Give users JavaScript.<br />Keep control of the runtime.</h2>
            <p className="closing-star-note">
              Zipp is open source and built in the open. If it is useful to you, a star on GitHub is the
              simplest way to help other engineers find it{stats?.stars ? ` — ${formatCount(stats.stars)} already have.` : '.'}
            </p>
          </div>
          <div className="closing-actions">
            <a className="button button-dark star-button" href={GITHUB_URL} target="_blank" rel="noreferrer">
              <StarIcon /> Star Zipp on GitHub
            </a>
            <ExternalLink className="closing-doc-link" href={DOCS_URL}>Read the docs</ExternalLink>
            <ExternalLink className="closing-doc-link" href={RELEASES_URL}>All releases</ExternalLink>
          </div>
        </section>
      </main>

      <footer className="site-footer section-wrap">
        <a className="brand" href="#top" aria-label="Back to top"><Brand /></a>
        <p>
          A clean-sheet JavaScript engine in Rust · part of{' '}
          <a href={F2I_URL} target="_blank" rel="noreferrer">f2i.com</a>
        </p>
        <div>
          <ExternalLink href={DOCS_URL}>Docs</ExternalLink>
          <ExternalLink href={BENCHMARK_URL}>Benchmarks</ExternalLink>
          <ExternalLink href={GITHUB_URL}>GitHub</ExternalLink>
          <ExternalLink href={F2I_URL}>f2i.com</ExternalLink>
        </div>
      </footer>
    </div>
  )
}

export default App
