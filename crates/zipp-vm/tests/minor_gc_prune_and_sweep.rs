//! Regression tests for Workstream B2.2 + A10: epoch-fresh side-table
//! pruning (`SlotTable`, `PropsTable`, `SlotSet`, `ProtoTable`), `regexp_result_props`
//! fresh root walk (A10), `GEN_STR_PINNED` mirror cleanup, and single-match
//! `free_slot` reclamation across minor/major collections and slot reuse.
//!
//! The expected lines are Node 24's.

const PROGRAM: &str = r#"
const out = [];

// 1. A10 + SlotTable fresh_keys: RegExp match results surviving across minors
//    while ephemeral matches die and recycle slots.
const keptMatches = [];
const re = /([a-z]+)(\d+)/;
for (let i = 0; i < 600; i++) {
  const s = "item_" + "tok" + i + "_end";
  const m = re.exec(s);
  if (i % 100 === 0) keptMatches.push(m);
}
let churnSum = 0;
for (let i = 0; i < 1200; i++) {
  const a = [i, i + 1, i + 2];
  churnSum += a[0] & 1;
}
out.push("rx-kept=" + keptMatches.map(m => m[1] + ":" + m[2] + "@" + m.index + ":" + m.input.length).join(","));

// Verify recycled array slots do not inherit stale regexp_result_props.
let staleRx = 0;
for (let i = 0; i < 500; i++) {
  const a = [1, 2];
  if ("index" in a || "input" in a || "groups" in a) staleRx++;
}
out.push("rx-stale=" + staleRx);

// 2. SlotSet fresh inline (<=8) AND overflow (>8): const_cells and error_data
//    across minors and slot reuse.
let constFail = 0;
for (let i = 0; i < 400; i++) {
  const x = i;
  const f = () => x;
  if (f() !== i) throw new Error("const mismatch");
  if (i % 100 === 0) {
    try {
      eval("x = 999");
    } catch (e) {
      if (e instanceof TypeError) constFail++;
    }
  }
}
let mutOk = 0;
for (let i = 0; i < 400; i++) {
  let y = i;
  const inc = () => { y += 1; return y; };
  if (inc() === i + 1) mutOk++;
}
out.push("const-cells=" + constFail + "," + mutOk);

// Error objects (error_data SlotSet + error_stacks SlotTable) churn + slot reuse.
let errCheck = 0;
for (let i = 0; i < 400; i++) {
  const e = new Error("e" + i);
  if (i % 100 === 0 && e instanceof Error) errCheck++;
}
let plainIsErr = 0;
for (let i = 0; i < 400; i++) {
  const o = { a: i, b: i + 1 };
  if (typeof Error.isError === "function" && Error.isError(o)) plainIsErr++;
}
out.push("err-reuse=" + errCheck + "," + plainIsErr);

// 3. ProtoTable fresh_keys + JIT emit_new_lane (count != rust_count) across minors.
function Point(x, y) { this.x = x; this.y = y; }
Point.prototype.sum = function () { return this.x + this.y; };
let pSum = 0;
for (let i = 0; i < 800; i++) {
  const p = new Point(i & 7, (i >> 3) & 7);
  pSum += p.sum();
}
const protoA = { tag: "A" };
let ocHits = 0;
for (let i = 0; i < 400; i++) {
  const o = Object.create(protoA);
  if (o.tag === "A") ocHits++;
}
let plainTag = 0;
for (let i = 0; i < 800; i++) {
  const o = { x: i, y: i + 1 };
  if (o.tag !== undefined) plainTag++;
}
out.push("proto=" + pSum + "," + ocHits + "," + plainTag);

// 4. GEN_STR_PINNED: pin inline SSO strings and heap strings via .length and charCodeAt,
//    reclaim them across minors, then allocate closures in recycled slots.
function strTouch(s) {
  return s.length + s.charCodeAt(0) + s.charCodeAt(s.length - 1);
}
let sAcc = 0;
for (let i = 0; i < 800; i++) {
  const shortS = "a" + (i & 63) + "z";
  const longS = "prefix_0123456789_middle_" + i + "_suffix_abcdef";
  sAcc += strTouch(shortS) + strTouch(longS);
}
let cAcc = 0;
for (let i = 0; i < 800; i++) {
  let k = i & 15;
  const fn = () => k + 1;
  cAcc += fn();
}
out.push("str-clo=" + sAcc + "," + cAcc + "," + churnSum);

for (const line of out) print(line);
"#;

#[test]
fn minor_gc_prune_and_sweep_answers_as_node() {
    let lines = zipp_vm::run(PROGRAM).expect("program runs").output;
    assert_eq!(
        lines,
        vec![
            "rx-kept=tok:0@5:13,tok:100@5:15,tok:200@5:15,tok:300@5:15,tok:400@5:15,tok:500@5:15",
            "rx-stale=0",
            "const-cells=4,400",
            "err-reuse=4,0",
            "proto=5536,400,0",
            "str-clo=382960,6800,600",
        ]
    );
}

#[test]
fn minor_gc_prune_and_sweep_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, envs) in [
        ("eager", vec![("ZIPP_JIT_THRESHOLD", "1")]),
        ("stress", vec![("ZIPP_GC_STRESS", "1")]),
        ("nojit", vec![("ZIPP_NOJIT", "1")]),
        (
            "nursery-verify",
            vec![
                ("ZIPP_NURSERY_VERIFY", "1"),
                ("ZIPP_NURSERY_YOUNG_BUDGET", "256"),
            ],
        ),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("minor_gc_prune_and_sweep_answers_as_node")
            .arg("--nocapture");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn mode child");
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
