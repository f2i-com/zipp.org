// The Zipp playground page: a project of .py or .js files in the sidebar, an
// editor, and a canvas + console fed by the engine running in
// engine.worker.js. See README.md for the program contract (`ui`, `draw`,
// `update`, `on_click`, `on_key`).
"use strict";

// A reply later than this restarts the engine. A `zipp_gpu` graph does not
// delay the reply that produced it: the adapter chains execution on its queue,
// so it runs after the worker has answered. It does occupy the worker before
// the next frame, so the budget still has to be small against this deadline.
// The graph validator bounds that work per backend, and the slowest of them
// (the JavaScript reference, 100M estimated operations) measured ~27 ms for an
// MNIST-scale MLP training step of 58M — so no extra frame-level GPU cap.
const DEADLINE_MS = 5000;
const INSTRUCTION_BUDGET = 2e9;    // the engine's maximum lifetime budget
const STORAGE_KEY = "zipp-playground-project";
const MAX_CONSOLE_LINES = 2000;
const SOURCE_EXTENSIONS = { py: "python", js: "javascript", mjs: "javascript", php: "php", rs: "rust" };
// Files a project folder brings along besides sources: shown as text when
// they decode as UTF-8, otherwise kept as bytes (checkpoints, images) that
// the program can open(). Tool folders are skipped, as are big files.
const SKIP_DIRS = new Set([".git", "__pycache__", "node_modules", "target", ".venv", "venv", ".idea", ".vscode", "dist"]);
const MAX_FILE_BYTES = 8 * 1024 * 1024;
const MAX_PROJECT_BYTES = 64 * 1024 * 1024;
const SAMPLES = {
  training: { name: "torch-training", entry: "main.py", base: "../../../examples/python/torch_training/", files: ["main.py", "model.py"] },
  torch: { name: "torch-inference", entry: "main.py", base: "../../../examples/python/torch_gpu/", files: ["main.py"] },
  python: { name: "python-balls", entry: "main.py", base: "../../../examples/python/project/", files: ["main.py", "physics.py"] },
  javascript: { name: "js-balls", entry: "main.js", base: "../../../examples/js/project/", files: ["physics.js", "main.js"] },
  ant: { name: "langtons-ant", entry: "main.py", base: "../../../examples/python/langtons_ant/", files: ["main.py", "rules.py"] },
  gpu: { name: "gpu-compute", entry: "main.py", base: "../../../examples/python/gpu/", files: ["main.py", "life.py"] },
  "py-html": {
    name: "py-html-markdown",
    entry: "main.py",
    inline: {
      "main.py": `from html.parser import HTMLParser
import re, textwrap

HTML = """
<article>
  <h1>ZIPP v0.0.40 Release Notes</h1>
  <p>One <strong>Rust register VM</strong> running <em>JavaScript, TypeScript, Python 3.13, PHP 8.x,</em> and <em>Rust</em>.</p>
  <ul>
    <li>23 new Python stdlib modules including <code>html.parser</code> and <code>textwrap</code></li>
    <li>Native x86-64 JIT &amp; pure-safe-Rust WebAssembly sandbox</li>
    <li>Composable <code>addAppBridge</code> plugins for GPU &amp; SQLite 3</li>
  </ul>
  <p>Read more at <a href="https://www.zipp.org/benchmarks/">zipp.org/benchmarks</a>.</p>
</article>
"""

class MarkdownConverter(HTMLParser):
    def __init__(self):
        super().__init__()
        self.out = []
        self.links = []
        self._href = None

    def handle_starttag(self, tag, attrs):
        attr_map = dict(attrs)
        if tag == "h1":
            self.out.append("# ")
        elif tag == "li":
            self.out.append("- ")
        elif tag == "strong":
            self.out.append("**")
        elif tag == "em":
            self.out.append("*")
        elif tag == "code":
            self.out.append("\`")
        elif tag == "a":
            self._href = attr_map.get("href", "")
            self.out.append("[")

    def handle_endtag(self, tag):
        if tag in ("h1", "p"):
            self.out.append("\\n\\n")
        elif tag == "li":
            self.out.append("\\n")
        elif tag == "ul":
            self.out.append("\\n")
        elif tag == "strong":
            self.out.append("**")
        elif tag == "em":
            self.out.append("*")
        elif tag == "code":
            self.out.append("\`")
        elif tag == "a":
            self.out.append(f"]({self._href})")
            self.links.append(self._href)
            self._href = None

    def handle_data(self, data):
        cleaned = re.sub(r"\\s+", " ", data)
        if cleaned.strip():
            self.out.append(cleaned)

parser = MarkdownConverter()
parser.feed(HTML)
markdown = "".join(parser.out).strip()
for block in markdown.split("\\n\\n"):
    if block.startswith("#") or block.startswith("-"):
        print(block)
    else:
        print(textwrap.fill(block.strip(), width=68))
    print()

words = re.findall(r"[A-Za-z0-9._-]+", markdown)
print(f"Summary: {len(words)} words, {len(parser.links)} link(s) extracted in ZIPP Python WASM.")
`,
    },
  },
  "py-async": {
    name: "py-async-pipeline",
    entry: "main.py",
    inline: {
      "main.py": `import asyncio
from dataclasses import dataclass

@dataclass
class TaskResult:
    service: str
    latency_ms: float
    items: int

async def fetch_batch(service: str, delay: float, items: int) -> TaskResult:
    await asyncio.sleep(delay)
    return TaskResult(service=service, latency_ms=round(delay * 1000, 1), items=items)

async def main():
    jobs = [
        ("inventory-sql", 0.012, 128),
        ("orders-php",    0.008,  64),
        ("pricing-rust",  0.005,  96),
        ("analytics-py",  0.015, 256),
    ]
    results = await asyncio.gather(*(fetch_batch(s, d, n) for s, d, n in jobs))
    total_items = sum(r.items for r in results)
    fastest = min(results, key=lambda r: r.latency_ms)
    print("Async pipeline completed across 4 services:")
    for r in sorted(results, key=lambda x: x.latency_ms):
        bar = "#" * (r.items // 16)
        print(f"  {r.service:<14} {r.latency_ms:>5.1f} ms | {r.items:>3} items {bar}")
    print(f"Total processed: {total_items} items · fastest: {fastest.service}")

asyncio.run(main())
`,
    },
  },
  "py-fractal": {
    name: "py-mandelbrot",
    entry: "main.py",
    inline: {
      "main.py": `import ui

W, H = 360, 240
CELL = 4
PALETTE = [
    "#101018", "#1d1536", "#311b63", "#4c2296",
    "#7c3aed", "#a78bfa", "#22d3ee", "#67e8f9",
    "#c4f1be", "#edff80",
]

ui.canvas(W, H)
ui.clear("#0d0c14")

inside = 0
cols = W // CELL
rows = H // CELL
for py in range(rows):
    ci = -1.2 + (py / rows) * 2.4
    for px in range(cols):
        cr = -2.1 + (px / cols) * 3.0
        zr = 0.0
        zi = 0.0
        it = 0
        while it < 36 and zr * zr + zi * zi <= 4.0:
            zr, zi = zr * zr - zi * zi + cr, 2.0 * zr * zi + ci
            it += 1
        if it == 36:
            inside += 1
            color = "#0b0a12"
        else:
            color = PALETTE[it % len(PALETTE)]
        ui.rect(px * CELL, py * CELL, CELL, CELL, color)

ui.font(14)
ui.text(12, H - 12, f"Mandelbrot in Python WASM · {inside}/{cols * rows} cells inside", "#edff80")
print(f"Rendered {cols}x{rows} fractal grid ({inside} interior cells).")
`,
    },
  },
  "py-analytics": {
    name: "py-sales-analytics",
    entry: "main.py",
    inline: {
      "sales.csv": `region,category,units,price
north,espresso,42,4.50
south,pastry,31,5.25
north,matcha,27,5.75
west,espresso,58,4.50
east,pastry,24,5.25
south,espresso,36,4.50
west,matcha,19,5.75
east,espresso,45,4.50
`,
      "main.py": `import json
from collections import defaultdict

revenue_by_region = defaultdict(float)
units_by_cat = defaultdict(int)

with open("sales.csv", "r") as f:
    lines = [line.strip() for line in f if line.strip()]
headers = lines[0].split(",")
for line in lines[1:]:
    row = dict(zip(headers, line.split(",")))
    units = int(row["units"])
    rev = units * float(row["price"])
    revenue_by_region[row["region"]] += rev
    units_by_cat[row["category"]] += units

report = {
    "total_revenue": round(sum(revenue_by_region.values()), 2),
    "by_region": {k: round(v, 2) for k, v in sorted(revenue_by_region.items(), key=lambda kv: -kv[1])},
    "by_category_units": dict(sorted(units_by_cat.items(), key=lambda kv: -kv[1])),
}

with open("report.json", "w") as out:
    json.dump(report, out, indent=2)

print("Revenue by region (read from sales.csv):")
for region, rev in report["by_region"].items():
    print(f"  {region:<8} \${rev:>7.2f}  " + "#" * int(rev // 20))
print(f"Total revenue: \${report['total_revenue']:.2f}")
print("Wrote report.json to the folder sidebar!")
`,
    },
  },
  "php-modern": {
    name: "php-modern-orders",
    entry: "index.php",
    inline: {
      "Order.php": `<?php
declare(strict_types=1);

enum Priority: string {
    case Standard = 'standard';
    case Express = 'express';
    case Rush = 'rush';

    public function fee(): float {
        return match ($this) {
            self::Standard => 0.0,
            self::Express  => 9.50,
            self::Rush     => 24.00,
        };
    }
}

readonly class Order {
    public function __construct(
        public string $id,
        public string $customer,
        public float $subtotal,
        public Priority $priority,
    ) {}

    public function total(): float {
        return round($this->subtotal + $this->priority->fee(), 2);
    }
}
`,
      "index.php": `<?php
declare(strict_types=1);
require_once 'Order.php';

$orders = [
    new Order('ORD-101', 'Kiln & Wheel Studio', 148.00, Priority::Express),
    new Order('ORD-102', 'BrewDesk Roasters',    84.50, Priority::Standard),
    new Order('ORD-103', 'Harbor Ceramic Co.',  215.00, Priority::Rush),
    new Order('ORD-104', 'Pinecone Bakery',      62.25, Priority::Standard),
];

$priority = array_values(array_filter(
    $orders,
    fn(Order $o): bool => $o->priority !== Priority::Standard
));

echo "PHP 8.x Order Pipeline on ZIPP WASM\\n";
echo str_repeat('-', 54) . "\\n";
foreach ($orders as $o) {
    printf("%-8s %-20s %-9s \$%7.2f\\n", $o->id, $o->customer, $o->priority->value, $o->total());
}

$grandTotal = array_reduce($orders, fn(float $acc, Order $o): float => $acc + $o->total(), 0.0);
echo str_repeat('-', 54) . "\\n";
echo "Priority orders: " . count($priority) . " of " . count($orders) . "\\n";
echo "Grand total:     \$" . number_format($grandTotal, 2) . "\\n";
echo "JSON summary:    " . json_encode([
    'count' => count($orders),
    'priority_ids' => array_map(fn(Order $o) => $o->id, $priority),
    'grand_total' => round($grandTotal, 2),
]) . "\\n";
`,
    },
  },
  "php-template": {
    name: "php-invoice-template",
    entry: "invoice.php",
    inline: {
      "invoice.php": `<?php
declare(strict_types=1);

$invoice = [
    'number' => 'INV-2026-040',
    'client' => 'Kiln & Wheel Pottery Studio',
    'tax_rate' => 0.10,
    'items' => [
        ['name' => 'Stoneware Clay (25 kg bag)', 'qty' => 4, 'unit' => 38.00],
        ['name' => 'Celadon Glaze Pack',         'qty' => 2, 'unit' => 29.50],
        ['name' => 'Kiln Firing Session',        'qty' => 3, 'unit' => 45.00],
    ],
];

$subtotal = 0.0;
foreach ($invoice['items'] as $item) {
    $subtotal += $item['qty'] * $item['unit'];
}
$tax = round($subtotal * $invoice['tax_rate'], 2);
$total = $subtotal + $tax;
?>
=== INVOICE <?= $invoice['number'] ?> ===
Client: <?= $invoice['client'] ?>

<?php foreach ($invoice['items'] as $i => $item): ?>
  <?= ($i + 1) ?>. <?= str_pad($item['name'], 30) ?> x<?= $item['qty'] ?>  $<?= number_format($item['qty'] * $item['unit'], 2) ?>

<?php endforeach; ?>
-----------------------------------------------
Subtotal: $<?= number_format($subtotal, 2) ?>

Tax (10%): $<?= number_format($tax, 2) ?>

TOTAL DUE: $<?= number_format($total, 2) ?>
`,
    },
  },
  "php-regex": {
    name: "php-word-frequency",
    entry: "analyze.php",
    inline: {
      "analyze.php": `<?php
declare(strict_types=1);

$text = <<<TEXT
ZIPP runs JavaScript, TypeScript, Python 3.13, PHP 8.x, and Rust on one shared
Rust register bytecode VM. SoftN packages web apps into portable containers, and
FormLogic hosts 58+ marketplace apps inside ZIPP sandboxes with SQLite 3 and
strict default-deny permissions. One VM, one sandbox, five languages.
TEXT;

preg_match_all('/[a-zA-Z0-9.+_-]+/', strtolower($text), $matches);
$tokens = $matches[0];
$counts = array_count_values($tokens);
arsort($counts);

echo "Tokens: " . count($tokens) . " · Unique: " . count($counts) . "\\n";
echo "Top words:\\n";
$shown = 0;
foreach ($counts as $word => $n) {
    printf("  %-14s %2d  %s\\n", $word, $n, str_repeat('#', $n * 3));
    if (++$shown >= 10) break;
}
`,
    },
  },
  "rust-enums": {
    name: "rust-event-ledger",
    entry: "main.rs",
    inline: {
      "main.rs": `#[derive(Debug, Clone)]
enum Event {
    Booking { guest: String, seats: u32, price: f64 },
    Cancel { guest: String, refund: f64 },
    WorkshopAdded { title: String, capacity: u32 },
}

impl Event {
    fn revenue_delta(&self) -> f64 {
        match self {
            Event::Booking { seats, price, .. } => (*seats as f64) * *price,
            Event::Cancel { refund, .. } => -(*refund),
            Event::WorkshopAdded { .. } => 0.0,
        }
    }
}

fn main() {
    let log = vec![
        Event::WorkshopAdded { title: "Wheel Throwing".into(), capacity: 8 },
        Event::Booking { guest: "Claire".into(), seats: 2, price: 68.0 },
        Event::Booking { guest: "Devon".into(), seats: 1, price: 75.0 },
        Event::Cancel { guest: "Devon".into(), refund: 75.0 },
        Event::Booking { guest: "Nadia".into(), seats: 3, price: 58.0 },
    ];

    println!("Rust Event Ledger on ZIPP WASM");
    let mut balance = 0.0;
    for (idx, ev) in log.iter().enumerate() {
        let delta = ev.revenue_delta();
        balance += delta;
        match ev {
            Event::Booking { guest, seats, .. } =>
                println!("  #{} BOOK   {:<8} {} seat(s)  +{:.2}", idx + 1, guest, seats, delta),
            Event::Cancel { guest, .. } =>
                println!("  #{} CANCEL {:<8}            {:.2}", idx + 1, guest, delta),
            Event::WorkshopAdded { title, capacity } =>
                println!("  #{} ADD    {} (cap {})", idx + 1, title, capacity),
        }
    }
    println!("Net studio revenue: \${:.2}", balance);
}
`,
    },
  },
  "rust-multifile": {
    name: "rust-task-pipeline",
    entry: "main.rs",
    inline: {
      "models.rs": `use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: u32,
    pub title: String,
    pub owner: String,
    pub done: bool,
}

impl Task {
    pub fn new(id: u32, title: &str, owner: &str, done: bool) -> Self {
        Self { id, title: title.to_string(), owner: owner.to_string(), done }
    }
}

pub fn completion_rate(tasks: &[Task]) -> f64 {
    if tasks.is_empty() { return 0.0; }
    let done = tasks.iter().filter(|t| t.done).count();
    (done as f64) * 100.0 / (tasks.len() as f64)
}
`,
      "main.rs": `mod models;
use models::{Task, completion_rate};
use std::collections::HashMap;

fn main() {
    let tasks = vec![
        Task::new(1, "Compile Rust to ZIPP bytecode", "vm", true),
        Task::new(2, "Add PHP & Rust playground samples", "web", true),
        Task::new(3, "Embed FormLogic SoftN container", "web", true),
        Task::new(4, "Ship v0.0.40 release binaries", "ci", true),
        Task::new(5, "Explore next JIT optimizations", "vm", false),
    ];

    let mut by_owner: HashMap<String, usize> = HashMap::new();
    for t in &tasks {
        *by_owner.entry(t.owner.clone()).or_insert(0) += 1;
    }

    println!("Multi-file Rust Project ({} tasks, {:.0}% done)", tasks.len(), completion_rate(&tasks));
    for t in &tasks {
        let mark = if t.done { "[x]" } else { "[ ]" };
        println!("  {} #{} ({}) {}", mark, t.id, t.owner, t.title);
    }

    let json = serde_json::to_string(&tasks[0]).unwrap();
    println!("Serialized task #1: {}", json);
}
`,
    },
  },
  "rust-sieve": {
    name: "rust-primes-iter",
    entry: "main.rs",
    inline: {
      "main.rs": `struct Fib { a: u64, b: u64 }

impl Fib {
    fn new() -> Self { Self { a: 0, b: 1 } }
}

impl Iterator for Fib {
    type Item = u64;
    fn next(&mut self) -> Option<u64> {
        let cur = self.a;
        let next = self.a + self.b;
        self.a = self.b;
        self.b = next;
        Some(cur)
    }
}

fn count_primes(limit: usize) -> (usize, usize) {
    let mut is_prime = vec![true; limit + 1];
    is_prime[0] = false;
    is_prime[1] = false;
    let mut p = 2;
    while p * p <= limit {
        if is_prime[p] {
            let mut m = p * p;
            while m <= limit {
                is_prime[m] = false;
                m += p;
            }
        }
        p += 1;
    }
    let mut count = 0;
    let mut last = 0;
    for (n, &flag) in is_prime.iter().enumerate() {
        if flag { count += 1; last = n; }
    }
    (count, last)
}

fn main() {
    let fibs: Vec<u64> = Fib::new().take(12).collect();
    println!("First 12 Fibonacci numbers: {:?}", fibs);

    for limit in [1_000, 10_000, 50_000] {
        let (count, last) = count_primes(limit);
        println!("Primes <= {:>6}: {:>5} (largest: {})", limit, count, last);
    }
}
`,
    },
  },
  "python-hello": { name: "python-hello", entry: "main.py", inline: { "main.py": 'import ui\n\ndef fib(n):\n    a = 0\n    b = 1\n    for i in range(n):\n        a, b = b, a + b\n    return a\n\nprint("fib(30) =", fib(30))\n\nui.canvas(320, 120)\nui.clear("#10141c")\nui.font(28)\nui.text(24, 70, "hello from Python", "#ffcc00")\n' } },
  "javascript-hello": { name: "js-hello", entry: "main.js", inline: { "main.js": 'const fib = (n) => (n < 2 ? n : fib(n - 1) + fib(n - 2));\nconsole.log("fib(20) =", fib(20));\n\nui.canvas(320, 120);\nui.clear("#10141c");\nui.font(28);\nui.text(24, 70, "hello from JavaScript", "#58a6ff");\n' } },
};

const $ = (selector) => document.querySelector(selector);
const el = {
  fileList: $("#file-list"), projectName: $("#project-name"), projectLanguage: $("#project-language"),
  currentFile: $("#current-file"), editor: $("#editor"), gutter: $("#gutter"),
  highlight: $("#highlight"), highlightCode: $("#highlight-code"),
  canvas: $("#canvas"), canvasHint: $("#canvas-hint"), consoleOut: $("#console"),
  status: $("#status"), frameStats: $("#frame-stats"), run: $("#run"), stop: $("#stop"),
  dropOverlay: $("#drop-overlay"), sampleMenu: $("#sample-menu"), gpuBackend: $("#gpu-backend"), args: $("#program-args"),
};
const ctx = el.canvas.getContext("2d");

// ---- project state ---------------------------------------------------------
// `files` maps root-relative paths to a string (text) or {bytes: Uint8Array}
// (binary). `written` marks files the last run produced.
const project = { name: "untitled", files: new Map(), entry: null, current: null, written: new Set() };
function isBinary(value) {
  return value !== null && typeof value === "object" && value.bytes instanceof Uint8Array;
}
function baseName(path) {
  return path.slice(path.lastIndexOf("/") + 1);
}
function bytesToBase64(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(s);
}
function base64ToBytes(text) {
  const s = atob(text);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}
function decodeUtf8(bytes) {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

function extensionOf(name) {
  const dot = name.lastIndexOf(".");
  return dot < 0 ? "" : name.slice(dot + 1).toLowerCase();
}
function languageOf(name) {
  return name ? SOURCE_EXTENSIONS[extensionOf(name)] || null : null;
}
function stemOf(name) {
  const dot = name.lastIndexOf(".");
  return dot < 0 ? name : name.slice(0, dot);
}
function projectLanguage() {
  return languageOf(project.entry);
}
function pickEntry() {
  const names = [...project.files.keys()].sort();
  if (project.entry && project.files.has(project.entry)) return;
  const top = names.filter((n) => !n.includes("/"));
  project.entry = top.find((n) => n === "main.py") || top.find((n) => n === "index.php")
    || top.find((n) => n === "main.rs") || top.find((n) => n === "main.js")
    || top.find((n) => languageOf(n)) || names.find((n) => languageOf(n)) || names[0] || null;
}
// Autosave notes are logged once per loaded project.
const autosave = { refused: false, skipped: false };
function save() {
  // Text is stored as is; binaries as base64 while the total stays small.
  let budget = 3 * 1024 * 1024;
  const files = [];
  let skipped = 0;
  for (const [name, value] of project.files) {
    if (isBinary(value)) {
      if (value.bytes.length > budget) { skipped++; continue; }
      budget -= value.bytes.length;
      files.push([name, { base64: bytesToBase64(value.bytes) }]);
    } else {
      files.push([name, value]);
    }
  }
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ name: project.name, entry: project.entry, current: project.current, files }));
  } catch (error) {
    // Usually the browser's storage quota (about 5 MB). The previous snapshot
    // must not survive either: the next visit would restore that older
    // project in place of this one.
    try { localStorage.removeItem(STORAGE_KEY); } catch { /* storage unavailable */ }
    if (!autosave.refused) {
      autosave.refused = true;
      log(`Autosave is off for ${project.name}: this browser would not store it (${error && error.name ? error.name : "storage unavailable"}). Edits last for this session only.`, "note");
    }
    return;
  }
  if (skipped && !autosave.skipped) {
    autosave.skipped = true;
    log(`Autosave leaves out ${skipped} binary file(s) over its 3 MiB budget; after a reload, open the folder again to use them.`, "note");
  }
}
function restore() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return false;
    const saved = JSON.parse(raw);
    if (!Array.isArray(saved.files) || saved.files.length === 0) return false;
    const files = saved.files.map(([name, value]) => [name, value && typeof value === "object" && typeof value.base64 === "string" ? { bytes: base64ToBytes(value.base64) } : value]);
    setProject(saved.name || "untitled", files, saved.entry, saved.current);
    return true;
  } catch {
    return false;
  }
}
function setProject(name, entries, entry, current) {
  project.name = name;
  project.files = new Map(entries);
  project.written = new Set();
  autosave.refused = autosave.skipped = false;
  project.entry = entry && project.files.has(entry) ? entry : null;
  pickEntry();
  project.current = current && project.files.has(current) ? current : project.entry;
  renderFiles();
  openFile(project.current);
  save();
}

// ---- sidebar + editor ------------------------------------------------------
// The sidebar is a tree: folders first at each level, then files, each row
// indented by depth. Folders are labels; files open in the editor.
function renderFiles() {
  const language = projectLanguage();
  el.projectName.textContent = project.name;
  el.projectName.title = project.name;
  el.projectLanguage.textContent = language || "";
  el.fileList.replaceChildren();
  const tree = new Map();
  for (const name of project.files.keys()) {
    const parts = name.split("/");
    let node = tree;
    for (let i = 0; i < parts.length - 1; i++) {
      if (!node.has(parts[i] + "/")) node.set(parts[i] + "/", new Map());
      node = node.get(parts[i] + "/");
    }
    node.set(parts[parts.length - 1], name);
  }
  const tag = (li, cls, text, title) => {
    const span = document.createElement("span");
    span.className = cls;
    span.textContent = text;
    if (title) span.title = title;
    li.append(" ", span);
  };
  const walk = (node, depth) => {
    const keys = [...node.keys()].sort((a, b) => {
      const fa = a.endsWith("/"), fb = b.endsWith("/");
      return fa === fb ? a.localeCompare(b) : fa ? -1 : 1;
    });
    for (const key of keys) {
      const li = document.createElement("li");
      li.style.paddingLeft = `${8 + depth * 14}px`;
      if (key.endsWith("/")) {
        li.className = "folder";
        li.textContent = "▾ " + key.slice(0, -1);
        el.fileList.append(li);
        walk(node.get(key), depth + 1);
        continue;
      }
      const name = node.get(key);
      const value = project.files.get(name);
      li.dataset.name = name;
      li.textContent = key;
      if (name === project.current) li.classList.add("active");
      const fileLanguage = languageOf(name);
      if (language && fileLanguage && fileLanguage !== language) {
        li.classList.add("ignored");
        li.title = `not run: the project language is ${language}`;
      } else if (!fileLanguage) {
        li.classList.add("data");
        li.title = isBinary(value) ? `binary file, ${value.bytes.length} bytes: the program can open() it` : "a data file the program can open()";
      }
      if (isBinary(value)) tag(li, "bin", "bin");
      if (project.written.has(name)) tag(li, "written", "written", "written by the last run");
      if (name === project.entry) tag(li, "entry", "entry");
      li.addEventListener("click", () => openFile(name));
      el.fileList.append(li);
    }
  };
  walk(tree, 0);
  $("#set-entry").disabled = !project.current || project.current === project.entry || !languageOf(project.current);
  $("#rename-file").disabled = !project.current;
  $("#delete-file").disabled = !project.current;
}
function openFile(name) {
  project.current = name;
  el.currentFile.textContent = name || "no file";
  const value = name ? project.files.get(name) : "";
  if (isBinary(value)) {
    el.editor.value = `(binary file, ${value.bytes.length} bytes)`;
    el.editor.disabled = true;
  } else {
    el.editor.value = value || "";
    el.editor.disabled = !name;
  }
  updateGutter();
  for (const li of el.fileList.children) li.classList.toggle("active", li.dataset.name === name);
  renderFiles();
  save();
}
function updateGutter() {
  const lines = el.editor.value.split("\n").length;
  el.gutter.textContent = Array.from({ length: lines }, (_, i) => i + 1).join("\n");
  syncScroll();
  updateHighlight();
}
function syncScroll() {
  el.gutter.scrollTop = el.editor.scrollTop;
  el.highlight.scrollTop = el.editor.scrollTop;
  el.highlight.scrollLeft = el.editor.scrollLeft;
}
el.editor.addEventListener("input", () => {
  if (project.current && !isBinary(project.files.get(project.current))) project.files.set(project.current, el.editor.value);
  updateGutter();
  save();
});
el.editor.addEventListener("scroll", syncScroll);

// ---- syntax highlighting: a small tokenizer per language, rendered into the
// mirror `pre` behind the textarea. Groups: comment, string, number, word,
// punctuation; words are classified by the language's keyword/builtin sets.
const HIGHLIGHT = {
  python: {
    pattern: /(#[^\n]*)|("""[\s\S]*?"""|'''[\s\S]*?'''|"(?:\\.|[^"\\\n])*"?|'(?:\\.|[^'\\\n])*'?)|(\b\d[\w.]*\b)|(@[A-Za-z_]\w*)|([A-Za-z_]\w*)|([^\sA-Za-z_\d]+)/g,
    keywords: new Set("def return if elif else while for in break continue pass import from as and or not is True False None assert lambda class try except finally raise with yield del global nonlocal".split(" ")),
    builtins: new Set("print len range str int bool list tuple abs min max ui self".split(" ")),
  },
  javascript: {
    pattern: /(\/\/[^\n]*|\/\*[\s\S]*?\*\/)|(`(?:\\.|[^`\\])*`?|"(?:\\.|[^"\\\n])*"?|'(?:\\.|[^'\\\n])*'?)|(\b\d[\w.]*\b)|(@[A-Za-z_$][\w$]*)|([A-Za-z_$][\w$]*)|([^\sA-Za-z_$\d]+)/g,
    keywords: new Set("const let var function return if else for while do break continue switch case default new this class extends super import export from of in typeof instanceof void delete throw try catch finally async await yield true false null undefined static get set".split(" ")),
    builtins: new Set("console ui Math JSON Object Array String Number Boolean Map Set Promise Error Date window document".split(" ")),
  },
  php: {
    pattern: /(\/\/[^\n]*|#[^\n]*|\/\*[\s\S]*?\*\/)|("(?:\\.|[^"\\])*"?|'(?:\\.|[^'\\])*'?)|(\b\d[\w.]*\b)|(\$[A-Za-z_]\w*)|([A-Za-z_]\w*)|([^\sA-Za-z_$\d]+)/g,
    keywords: new Set("function fn return if elseif else endif while endwhile for endfor foreach endforeach as in break continue switch match case default new class interface trait enum extends implements public private protected readonly static abstract final const declare strict_types namespace use require require_once include include_once echo print try catch finally throw yield true false null self parent".split(" ")),
    builtins: new Set("array_map array_filter array_reduce array_values array_count_values arsort count strlen str_repeat str_pad printf number_format json_encode json_decode preg_match preg_match_all strtolower round PDO".split(" ")),
  },
  rust: {
    pattern: /(\/\/[^\n]*|\/\*[\s\S]*?\*\/)|("(?:\\.|[^"\\])*"?|'(?:\\.|[^'\\\n])')|(\b\d[\w.]*\b)|(#!?\[[^\]]*\])|([A-Za-z_]\w*!?)|([^\sA-Za-z_\d]+)/g,
    keywords: new Set("fn pub mod use struct enum impl trait for in while loop if else match let mut const static ref move return break continue as where type crate super self Self async await true false".split(" ")),
    builtins: new Set("println! print! eprintln! format! vec! Some None Ok Err Option Result Vec String HashMap BTreeMap Iterator Serialize Deserialize serde_json u8 u16 u32 u64 usize i32 i64 f32 f64 bool str".split(" ")),
  },
};
function escapeHtml(text) {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
function highlightSource(source, language) {
  const rules = HIGHLIGHT[language];
  if (!rules) return escapeHtml(source);
  const out = [];
  let last = 0;
  rules.pattern.lastIndex = 0;
  let m;
  while ((m = rules.pattern.exec(source)) !== null) {
    if (m.index > last) out.push(escapeHtml(source.slice(last, m.index)));
    const text = escapeHtml(m[0]);
    let cls = "";
    if (m[1] !== undefined) cls = "tok-comment";
    else if (m[2] !== undefined) cls = "tok-string";
    else if (m[3] !== undefined) cls = "tok-number";
    else if (m[4] !== undefined) cls = "tok-decorator";
    else if (m[5] !== undefined) {
      if (rules.keywords.has(m[0])) cls = "tok-keyword";
      else if (rules.builtins.has(m[0])) cls = "tok-builtin";
      else if (source[rules.pattern.lastIndex] === "(") cls = "tok-function";
    } else cls = "tok-punct";
    out.push(cls ? `<span class="${cls}">${text}</span>` : text);
    last = rules.pattern.lastIndex;
  }
  if (last < source.length) out.push(escapeHtml(source.slice(last)));
  return out.join("");
}
function updateHighlight() {
  const source = el.editor.value;
  // A trailing newline needs a visible last line, or the mirror is one line
  // shorter than the textarea and scrolling drifts.
  const html = highlightSource(source, languageOf(project.current));
  el.highlightCode.innerHTML = source.endsWith("\n") ? html + " " : html;
}
el.editor.addEventListener("keydown", (event) => {
  if (event.key === "Tab") {
    event.preventDefault();
    const indent = projectLanguage() === "javascript" ? "  " : "    ";
    const { selectionStart, selectionEnd, value } = el.editor;
    el.editor.value = value.slice(0, selectionStart) + indent + value.slice(selectionEnd);
    el.editor.selectionStart = el.editor.selectionEnd = selectionStart + indent.length;
    el.editor.dispatchEvent(new Event("input"));
  } else if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
    event.preventDefault();
    run();
  } else if (event.key === "Enter") {
    // keep the current line's indentation
    const { selectionStart, value } = el.editor;
    const lineStart = value.lastIndexOf("\n", selectionStart - 1) + 1;
    const indent = /^[ \t]*/.exec(value.slice(lineStart, selectionStart))[0];
    if (indent) {
      event.preventDefault();
      el.editor.setRangeText("\n" + indent, selectionStart, el.editor.selectionEnd, "end");
      el.editor.dispatchEvent(new Event("input"));
    }
  }
});

function validFileName(name) {
  // A relative path of simple segments: `main.py`, `pkg/module.py`, `data/x.json`.
  return /^[A-Za-z0-9_][A-Za-z0-9_.-]*(\/[A-Za-z0-9_][A-Za-z0-9_.-]*)*$/.test(name) && !name.includes("..");
}
$("#new-file").addEventListener("click", () => {
  const language = projectLanguage() || "python";
  const defaultName = { python: "module.py", php: "helper.php", rust: "helper.rs", javascript: "module.js" }[language] || "module.py";
  const name = prompt("New file name:", defaultName);
  if (!name) return;
  if (!validFileName(name)) { alert("Use a simple relative path such as module.py or pkg/module.py."); return; }
  if (project.files.has(name)) { alert(`${name} already exists.`); return; }
  project.files.set(name, "");
  pickEntry();
  renderFiles();
  openFile(name);
});
$("#rename-file").addEventListener("click", () => {
  const from = project.current;
  if (!from) return;
  const to = prompt("Rename file:", from);
  if (!to || to === from) return;
  if (!validFileName(to)) { alert("Use a simple relative path such as module.py or pkg/module.py."); return; }
  if (project.files.has(to)) { alert(`${to} already exists.`); return; }
  project.files.set(to, project.files.get(from));
  project.files.delete(from);
  if (project.entry === from) project.entry = to;
  renderFiles();
  openFile(to);
});
$("#delete-file").addEventListener("click", () => {
  const name = project.current;
  if (!name || !confirm(`Delete ${name}?`)) return;
  project.files.delete(name);
  if (project.entry === name) project.entry = null;
  pickEntry();
  renderFiles();
  openFile(project.entry);
});
$("#set-entry").addEventListener("click", () => {
  if (!project.current || !languageOf(project.current)) return;
  project.entry = project.current;
  renderFiles();
  save();
});

// ---- loading folders, files and samples -----------------------------------
// `files` are File objects, or {file, relative} pairs from a dropped folder.
// The first path segment (the folder itself) is dropped; every file below is
// kept: sources, data files and binaries alike, minus tool folders.
async function loadFileObjects(files, folderName) {
  const entries = [];
  const skipped = [];
  let total = 0;
  for (const item of files) {
    const file = item instanceof File ? item : item.file;
    const relative = (item instanceof File ? file.webkitRelativePath : item.relative) || file.name;
    let parts = relative.split("/").filter(Boolean);
    // Only a path inside a folder has a first segment to drop: a loose file
    // is its bare name. (A folder that happens to be called "files" is still
    // a folder.)
    if (parts.length > 1) parts = parts.slice(1);
    if (parts.some((p) => SKIP_DIRS.has(p) || (p.startsWith(".") && p !== "."))) continue;
    const path = parts.join("/");
    if (!path) continue;
    if (file.size > MAX_FILE_BYTES || total + file.size > MAX_PROJECT_BYTES) { skipped.push(path); continue; }
    total += file.size;
    const bytes = new Uint8Array(await file.arrayBuffer());
    const text = languageOf(path) || /\.(txt|md|json|jsonl|csv|tsv|html|css|yaml|yml|toml|ini|cfg|xml|svg|sh|bat|ps1|rst|log)$/i.test(path) ? decodeUtf8(bytes) : (bytes.length < 512 * 1024 ? decodeUtf8(bytes) : null);
    entries.push([path, text === null ? { bytes } : text]);
  }
  if (!entries.some(([path]) => languageOf(path))) {
    log("No .py, .php, .rs or .js files found in that selection.", "error");
    return;
  }
  const name = folderName || (files[0] instanceof File ? (files[0].webkitRelativePath || "").split("/")[0] : "") || "files";
  await stopRun();
  setProject(name, entries, null, null);
  const folders = new Set(entries.map(([p]) => p.includes("/") ? p.slice(0, p.lastIndexOf("/")) : "").values());
  log(`Opened ${entries.length} file(s) from ${name}` + (folders.size > 1 ? ` in ${folders.size} folders` : "") + "." + (skipped.length ? ` Skipped ${skipped.length} file(s) over the size limits.` : ""), "note");
}
// Every file below a dropped directory entry, with paths relative to the drop.
async function readEntryTree(entry, prefix, out) {
  if (entry.isFile) {
    const file = await new Promise((resolve) => entry.file(resolve, () => resolve(null)));
    if (file) out.push({ file, relative: prefix + entry.name });
    return;
  }
  // The dropped folder itself is the project, whatever it is called; only
  // tool folders inside it are skipped, as "Open folder" does.
  if (!entry.isDirectory || (prefix !== "" && SKIP_DIRS.has(entry.name))) return;
  const reader = entry.createReader();
  for (;;) {
    const batch = await new Promise((resolve) => reader.readEntries(resolve, () => resolve([])));
    if (!batch.length) break;
    for (const child of batch) await readEntryTree(child, prefix + entry.name + "/", out);
  }
}
$("#folder-input").addEventListener("change", (event) => {
  loadFileObjects([...event.target.files]);
  event.target.value = "";
});
$("#files-input").addEventListener("change", (event) => {
  loadFileObjects([...event.target.files], "files");
  event.target.value = "";
});

// Drag and drop: a folder (via the File System Entry API) or loose files.
let dragDepth = 0;
document.addEventListener("dragenter", (event) => { event.preventDefault(); dragDepth++; el.dropOverlay.hidden = false; });
document.addEventListener("dragover", (event) => { event.preventDefault(); });
document.addEventListener("dragleave", () => { if (--dragDepth <= 0) { dragDepth = 0; el.dropOverlay.hidden = true; } });
document.addEventListener("drop", async (event) => {
  event.preventDefault();
  dragDepth = 0;
  el.dropOverlay.hidden = true;
  const items = [...(event.dataTransfer.items || [])];
  const files = [];
  let folderName = null;
  for (const item of items) {
    const entry = item.webkitGetAsEntry ? item.webkitGetAsEntry() : null;
    if (entry && entry.isDirectory) {
      folderName = folderName || entry.name;
      await readEntryTree(entry, "", files);
    } else if (item.kind === "file") {
      // A loose file sits at the project root, as with "Open files": a
      // prefix folder would hide it from imports and relative open().
      const file = item.getAsFile();
      if (file) files.push({ file, relative: file.name });
    }
  }
  await loadFileObjects(files.filter(Boolean), folderName || "files");
});

async function loadSample(key) {
  const sample = SAMPLES[key];
  if (!sample) return;
  const entries = [];
  try {
    if (sample.inline) {
      for (const [name, source] of Object.entries(sample.inline)) entries.push([name, source]);
    } else {
      for (const name of sample.files) {
        const response = await fetch(sample.base + name, { cache: "no-store" });
        if (!response.ok) throw new Error(`${response.status} for ${name}`);
        entries.push([name, await response.text()]);
      }
    }
  } catch (error) {
    log(`Could not fetch the sample (${error.message}). Serve the repository root with "node playground/serve.cjs".`, "error");
    return;
  }
  await stopRun();
  setProject(sample.name, entries, sample.entry, sample.entry);
  log(`Loaded sample ${sample.name}. Press Run.`, "note");
}
$("#sample-button").addEventListener("click", (event) => {
  event.stopPropagation();
  el.sampleMenu.hidden = !el.sampleMenu.hidden;
});
document.addEventListener("click", () => { el.sampleMenu.hidden = true; });
for (const button of el.sampleMenu.querySelectorAll("button")) {
  button.addEventListener("click", () => { el.sampleMenu.hidden = true; loadSample(button.dataset.sample); });
}

// ---- console + status --------------------------------------------------------
function log(text, kind = "") {
  const line = document.createElement("div");
  if (kind) line.className = kind;
  line.textContent = text;
  el.consoleOut.append(line);
  while (el.consoleOut.childElementCount > MAX_CONSOLE_LINES) el.consoleOut.firstElementChild.remove();
  el.consoleOut.scrollTop = el.consoleOut.scrollHeight;
}
function logConsole(lines) {
  for (const entry of lines || []) log(entry.text, entry.stream === "stdout" ? "" : "stderr");
}
$("#clear-console").addEventListener("click", () => el.consoleOut.replaceChildren());
function setStatus(parts) {
  el.status.replaceChildren();
  for (const [text, cls] of parts) {
    const span = document.createElement("span");
    span.textContent = text;
    if (cls) span.className = cls;
    el.status.append(span);
  }
}

// ---- the engine worker -------------------------------------------------------
const worker = { instance: null, pending: new Map(), nextId: 1, profile: null };
function startWorker() {
  if (worker.instance) worker.instance.terminate();
  for (const p of worker.pending.values()) { clearTimeout(p.timer); p.reject(new Error("engine restarted")); }
  worker.pending.clear();
  worker.instance = new Worker(new URL("./engine.worker.js", import.meta.url), { type: "module" });
  worker.instance.onmessage = (event) => {
    const m = event.data;
    if (m.type === "event") { hostEvent(m); return; }
    const p = worker.pending.get(m.id);
    if (!p) return;
    clearTimeout(p.timer);
    worker.pending.delete(m.id);
    p.resolve(m);
  };
  worker.instance.onerror = (event) => {
    log(`Engine worker error: ${event.message || event}`, "error");
    setStatus([["Engine failed to load. Build it with ./build-variants.sh all and serve the repository root.", "bad"]]);
  };
  request({ type: "hello" }, 60000).then((reply) => {
    if (reply.type === "error") {
      log(reply.error, "error");
      setStatus([[reply.error, "bad"]]);
      return;
    }
    worker.profile = reply.profile;
    const languages = reply.profile.languages || [];
    if (!languages.includes("python")) {
      log("This engine build has no Python frontend; rebuild with ./build-variants.sh all.", "error");
    }
    refreshStatus();
  }).catch((error) => {
    setStatus([[`Engine did not answer: ${error.message}`, "bad"]]);
  });
}
function request(message, timeoutMs = DEADLINE_MS) {
  return new Promise((resolve, reject) => {
    const id = worker.nextId++;
    const timer = setTimeout(() => {
      worker.pending.delete(id);
      reject(new Error(`no reply within ${timeoutMs / 1000}s`));
    }, timeoutMs);
    worker.pending.set(id, { resolve, reject, timer });
    worker.instance.postMessage({ ...message, id });
  });
}
function refreshStatus() {
  const p = worker.profile;
  const parts = [];
  if (p) {
    const langs = [...new Set([...(p.languages || []), "php", "rust"])];
    parts.push([`zipp-wasm ${p.version} · languages: ${langs.join(", ")}`]);
  }
  parts.push([loop.running ? `running ${projectLanguage()} · hooks: ${hookSummary()}` : "idle"]);
  setStatus(parts);
}

// ---- running + the frame loop ---------------------------------------------
const loop = { running: false, busy: false, frames: 0, fpsWindow: [], hooks: {} };
const input = { mx: 0, my: 0, down: false, clicked: false, keys: {}, events: [] };

function hookSummary() {
  const found = Object.entries(loop.hooks).filter(([, v]) => v).map(([k]) => k);
  return found.length ? found.join(", ") : "none";
}
function projectSources(language) {
  const files = {};
  const order = [];
  if (language === "python" || language === "php" || language === "rust") {
    // Every file goes along by path: source modules/includes and data files
    // alike; binaries as base64.
    for (const [name, value] of [...project.files].sort((a, b) => a[0].localeCompare(b[0]))) {
      files[name] = isBinary(value) ? { base64: bytesToBase64(value.bytes) } : value;
    }
    return { files, order, entry: project.entry };
  }
  for (const name of [...project.files.keys()].sort()) {
    if (languageOf(name) !== language || isBinary(project.files.get(name))) continue;
    if (name === project.entry) continue;
    files[name] = project.files.get(name);
    order.push(name);
  }
  files[project.entry] = project.files.get(project.entry);
  order.push(project.entry);
  return { files, order, entry: project.entry };
}
function programArgs() {
  const text = el.args.value.trim();
  if (!text) return [];
  const out = [];
  const re = /"([^"]*)"|'([^']*)'|(\S+)/g;
  let m;
  while ((m = re.exec(text)) !== null) out.push(m[1] ?? m[2] ?? m[3]);
  return out;
}
// Files the program wrote: shown in the tree (tagged) and openable.
function applyWrittenFiles(list) {
  if (!Array.isArray(list) || list.length === 0) return;
  for (const { path, base64, deleted } of list) {
    if (typeof path !== "string" || !path) continue;
    if (deleted === true) { project.files.delete(path); project.written.delete(path); continue; }
    if (typeof base64 !== 'string') continue;
    const bytes = base64ToBytes(base64);
    const text = decodeUtf8(bytes);
    project.files.set(path, text === null ? { bytes } : text);
    project.written.add(path);
  }
  renderFiles();
  if (project.current && list.some((f) => f.path === project.current)) openFile(project.current);
  save();
}
async function run() {
  // One run at a time: Ctrl+Enter reaches here while the button is disabled.
  if (el.run.disabled) return;
  if (!project.entry) { log("Nothing to run: open a folder or a sample first.", "error"); return; }
  const language = projectLanguage();
  if (!language) { log(`The entry file ${project.entry} is not a .py, .php, .rs or .js file.`, "error"); return; }
  el.run.disabled = true;
  // The stop must be answered (or the engine replaced) before this run is
  // sent, or its two-second deadline restarts the engine under the new run.
  try { await stopRun(); } finally { el.run.disabled = false; }
  el.consoleOut.replaceChildren();
  clearCanvas();
  const { files, order, entry } = projectSources(language);
  // The "written" tags describe the latest run only.
  if (project.written.size) { project.written = new Set(); renderFiles(); }
  const argv = programArgs();
  const lazyNote = (language === "php" || language === "rust") ? " · loading engine on demand if needed" : "";
  log(`▶ ${project.name} (${language}, entry ${project.entry}${argv.length ? ", args " + argv.join(" ") : ""}${lazyNote})`, "note");
  el.run.disabled = true;
  const runTimeout = (language === "php" || language === "rust") ? 30000 : DEADLINE_MS;
  try {
    const reply = await request({ type: "run", language, files, order, entry, argv: programArgs(), budget: INSTRUCTION_BUDGET, gpuBackend: el.gpuBackend.value }, runTimeout);
    logConsole(reply.console);
    applyWrittenFiles(reply.files);
    if (reply.type === "error") {
      reportError(reply);
      return;
    }
    render(reply.ui);
    loop.hooks = reply.hooks;
    if (reply.hooks.draw || reply.hooks.update) {
      startLoop();
      log(`Program defines ${hookSummary()}; running frames. Click the canvas for keyboard input.`, "ok");
    } else {
      log("Program finished.", "ok");
      refreshStatus();
    }
  } catch (error) {
    deadline(error);
  } finally {
    el.run.disabled = false;
  }
}
// Something happened between requests: a GPU result reached the program
// (whatever its callback printed and drew arrives here), or the compute
// runtime reported which backend it is using.
function hostEvent(m) {
  if (m.gpu) {
    const why = m.gpu.attempts && m.gpu.attempts.length ? ` (after: ${m.gpu.attempts.join("; ")})` : "";
    const device = m.gpu.adapter?.description || m.gpu.adapter?.architecture || m.gpu.adapter?.vendor;
    log(`GPU compute: ${m.gpu.backend} — ${m.gpu.description}${device ? ` (${device})` : ""}${why}`, "note");
    return;
  }
  logConsole(m.console);
  render(m.ui);
  applyWrittenFiles(m.files);
  if (m.error) reportError(m);
}
function reportError(reply) {
  log(reply.error, "error");
  if (reply.kind === "resource") log("The engine hit a resource limit and was disposed; press Run to start again.", "note");
  else if (reply.disposed) log("The engine was disposed; press Run to start again.", "note");
  stopLoop();
}
function deadline(error) {
  // Someone else already replaced the engine (a stop that went unanswered);
  // this request was simply cut off, and restarting again would cut the next.
  if (error.message === "engine restarted") {
    log("Stopped: the engine was restarted.", "note");
    stopLoop();
    return;
  }
  log(`Stopped: ${error.message}. The program did not respond (an infinite loop?), so its engine was discarded.`, "error");
  stopLoop();
  startWorker();
}
function startLoop() {
  loop.running = true;
  loop.busy = false;
  loop.frames = 0;
  loop.fpsWindow = [];
  input.events = [];
  input.clicked = false;
  el.stop.disabled = false;
  el.canvasHint.hidden = true;
  refreshStatus();
  requestAnimationFrame(tick);
}
function stopLoop() {
  loop.running = false;
  el.stop.disabled = true;
  refreshStatus();
}
async function stopRun() {
  if (!worker.instance) return;
  const wasRunning = loop.running;
  stopLoop();
  try { await request({ type: "stop" }, 2000); } catch { startWorker(); }
  if (wasRunning) log("Stopped.", "note");
}
el.run.addEventListener("click", run);
el.stop.addEventListener("click", stopRun);

async function tick(now) {
  if (!loop.running) return;
  if (loop.busy) { requestAnimationFrame(tick); return; }
  loop.busy = true;
  const events = input.events;
  input.events = [];
  const snapshot = {
    mx: Math.round(input.mx), my: Math.round(input.my), down: input.down, clicked: input.clicked,
    keys: { ...input.keys }, w: el.canvas.width, h: el.canvas.height,
  };
  input.clicked = false;
  try {
    const reply = await request({ type: "frame", input: snapshot, events }, DEADLINE_MS);
    logConsole(reply.console);
    applyWrittenFiles(reply.files);
    if (reply.type === "error") { render(reply.ui); reportError(reply); return; }
    render(reply.ui);
    loop.frames++;
    loop.fpsWindow.push(now);
    while (loop.fpsWindow.length && now - loop.fpsWindow[0] > 1000) loop.fpsWindow.shift();
    if (loop.frames % 10 === 0) el.frameStats.textContent = `${loop.fpsWindow.length} fps · frame ${loop.frames}`;
  } catch (error) {
    deadline(error);
    return;
  } finally {
    loop.busy = false;
  }
  if (loop.running) requestAnimationFrame(tick);
}

// ---- canvas rendering + input ---------------------------------------------
const paint = { font: 14 };
// Guest drawing is painted here, on the page's main thread, where no engine
// deadline applies. Each text or label is cut at the JavaScript `ui` shim's
// 4096-character limit (Python's `ui` accepts much longer strings), and one
// frame paints at most MAX_FRAME_TEXT characters, so a program cannot stall
// the tab with text shaping.
const MAX_UI_TEXT = 4096;
const MAX_FRAME_TEXT = 65536;
function clearCanvas() {
  ctx.fillStyle = "#10141c";
  ctx.fillRect(0, 0, el.canvas.width, el.canvas.height);
  el.frameStats.textContent = "";
}
function applyFont() {
  ctx.font = `${paint.font}px system-ui, -apple-system, "Segoe UI", sans-serif`;
  ctx.textBaseline = "alphabetic";
}
function render(commands) {
  if (!Array.isArray(commands) || commands.length === 0) return;
  el.canvasHint.hidden = true;
  applyFont();
  let textBudget = MAX_FRAME_TEXT;
  const text = (value) => {
    const s = String(value).slice(0, Math.min(MAX_UI_TEXT, textBudget));
    textBudget -= s.length;
    return s;
  };
  for (const c of commands) {
    switch (c[0]) {
      case "canvas": {
        const w = Math.max(1, Math.min(4096, c[1] | 0)), h = Math.max(1, Math.min(4096, c[2] | 0));
        if (el.canvas.width !== w || el.canvas.height !== h) { el.canvas.width = w; el.canvas.height = h; applyFont(); }
        break;
      }
      case "clear": ctx.fillStyle = c[1]; ctx.fillRect(0, 0, el.canvas.width, el.canvas.height); break;
      case "rect": ctx.fillStyle = c[5]; ctx.fillRect(c[1], c[2], c[3], c[4]); break;
      case "circle": ctx.fillStyle = c[4]; ctx.beginPath(); ctx.arc(c[1], c[2], Math.max(0, c[3]), 0, Math.PI * 2); ctx.fill(); break;
      case "line": ctx.strokeStyle = c[5]; ctx.lineWidth = 2; ctx.beginPath(); ctx.moveTo(c[1], c[2]); ctx.lineTo(c[3], c[4]); ctx.stroke(); break;
      case "text": ctx.fillStyle = c[4]; ctx.fillText(text(c[3]), c[1], c[2]); break;
      case "font": paint.font = Math.max(4, Math.min(200, c[1])); applyFont(); break;
      case "button": {
        const [, x, y, w, h, label] = c;
        const hover = input.mx >= x && input.mx < x + w && input.my >= y && input.my < y + h;
        ctx.fillStyle = hover ? (input.down ? "#1f6feb" : "#30363d") : "#21262d";
        ctx.strokeStyle = "#8b949e";
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.roundRect(x + 0.5, y + 0.5, w - 1, h - 1, 5);
        ctx.fill();
        ctx.stroke();
        ctx.fillStyle = "#e6edf3";
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillText(text(label), x + w / 2, y + h / 2);
        ctx.textAlign = "start";
        ctx.textBaseline = "alphabetic";
        break;
      }
      default: break;
    }
  }
}
function canvasPoint(event) {
  const rect = el.canvas.getBoundingClientRect();
  return {
    x: (event.clientX - rect.left) * (el.canvas.width / rect.width),
    y: (event.clientY - rect.top) * (el.canvas.height / rect.height),
  };
}
el.canvas.addEventListener("mousemove", (event) => { const p = canvasPoint(event); input.mx = p.x; input.my = p.y; });
el.canvas.addEventListener("mousedown", (event) => { const p = canvasPoint(event); input.mx = p.x; input.my = p.y; input.down = true; el.canvas.focus(); });
window.addEventListener("mouseup", () => { input.down = false; });
el.canvas.addEventListener("click", (event) => {
  const p = canvasPoint(event);
  input.clicked = true;
  input.events.push({ type: "click", x: Math.round(p.x), y: Math.round(p.y) });
});
el.canvas.addEventListener("keydown", (event) => {
  if (event.key === "Tab") return;
  event.preventDefault();
  if (!input.keys[event.key]) input.events.push({ type: "key", key: event.key });
  input.keys[event.key] = true;
});
el.canvas.addEventListener("keyup", (event) => { delete input.keys[event.key]; });
el.canvas.addEventListener("blur", () => { input.keys = {}; });

// ---- boot ---------------------------------------------------------------------
startWorker();
clearCanvas();
// A link can say what to show: `?sample=training` opens that sample rather
// than the project kept in this browser, `gpu=webgpu` picks where its GPU
// graphs run, and `run=1` starts it -- a shared link, or a kiosk that boots
// into a demo (zipp-linux).
const linked = new URLSearchParams(location.search);
if ([...el.gpuBackend.options].some((o) => o.value === linked.get("gpu"))) el.gpuBackend.value = linked.get("gpu");
if (Object.hasOwn(SAMPLES, linked.get("sample") ?? "")) {
  loadSample(linked.get("sample")).then(() => { if (linked.get("run") === "1") run(); });
} else if (!restore()) loadSample("python");
else log(`Restored ${project.name} from this browser. Press Run.`, "note");
renderFiles();
