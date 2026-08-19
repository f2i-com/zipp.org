//! Stage-3 nursery (remembered set + young-only minor trace) — churn against
//! retained graphs, held identical across every collector mode.
//!
//! A minor traces ONLY young objects (old objects are boundary nodes presumed
//! live; the write barrier's remembered set supplies their young referents)
//! and sweeps only the slots allocated since the previous collection. So the
//! things that can go wrong are: an old→young store site the barrier does not
//! cover (a live young object invisible to the minor trace — SWEPT ALIVE,
//! i.e. silent corruption), a side-table entry surviving its freed key, a
//! survivor swept, or floated old garbage accumulating without bound. Each
//! test aims at one, per edge idiom of NURSERY_DESIGN.md §1:
//!
//!   * the two old→young idioms B119's oracle measured as dominant — young
//!     values pushed into a retained array (227k, `jit_set_index`/push lanes)
//!     and young entries into an old Map (133k, `coll_insert`) — plus
//!     property overwrites, captured-cell writes, Map VALUE updates, async
//!     reaction edges and suspended-window re-parks, must all enter the
//!     remembered set and survive minors;
//!   * slot reuse after a minor must never serve a stale inline cache
//!     (versions bump on free — also pinned at the unit level in `heap.rs`);
//!   * side tables keyed by freed young slots (`arr_props` named array
//!     properties, `collection_index`) must drop their entries;
//!   * sustained OLD-garbage production must stay bounded: a minor that
//!     cannot shrink the heap below the pre-nursery collection point latches
//!     a major (`peak slots` asserts it).
//!
//! `all_modes_answer_identically` re-runs the `nursery_parity_` set in child
//! processes (the env latches are read once per process) under
//! `ZIPP_NO_NURSERY=1` (majors only — the exact pre-nursery collector),
//! `ZIPP_GC_STRESS=1` (a collection at EVERY safe point, alternating three
//! minors to a major), both combined, `ZIPP_SHAPE_VERIFY=1` + stress,
//! `ZIPP_NURSERY_VERIFY=1` (the full mark re-run beside EVERY minor,
//! panicking on any young object the young-only trace missed — the direct
//! executable form of the completeness argument in `vm/gc.rs`), verify +
//! stress, the GCSTATS oracle, and `ZIPP_NOJIT=1` (the cross-check that the
//! interpreter-only store population hits the same barriers the JIT helpers
//! do). Every assertion is mode-independent arithmetic, so agreeing in all
//! modes IS the parity check.

fn run_ok(src: &str) -> Vec<String> {
    // Stage 1 is OPT-IN (B120): this binary exists to exercise minors, so it
    // latches the nursery on for every in-process engine it constructs. The
    // all-modes children inherit it; ZIPP_NO_NURSERY still forces off.
    static NURSERY_ON: std::sync::Once = std::sync::Once::new();
    NURSERY_ON.call_once(|| std::env::set_var("ZIPP_NURSERY", "1"));
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Iteration scale. The default counts are sized to cross the 65,536-live GC
/// threshold repeatedly (real minors in default mode); under `ZIPP_GC_STRESS`
/// a collection runs at every safe point, so the same counts would multiply
/// into O(iterations x heap) — a fraction of the allocations reaches the same
/// minor/major interleavings there.
fn scaled(n: usize) -> usize {
    if std::env::var_os("ZIPP_GC_STRESS").is_some() { n / 50 } else { n }
}

/// Oracle idiom 1: young objects pushed into a RETAINED array. The array goes
/// old after the first collection; every push afterwards is an old→young edge
/// that only the full mark knows about. Losing any element to a minor sweep
/// shows up as a wrong `.v` (the slot got recycled) or a crash.
#[test]
fn nursery_parity_retained_array_pushes_survive_minors() {
    let n = scaled(300_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var keep = [];
        for (var i = 0; i < N; i++) {{
          keep.push({{ v: i }});
          var g = {{ a: i, b: i + 1 }}; // young garbage driving collections
          if (g.a === -1) console.log("unreachable");
        }}
        var ok = true;
        for (var j = 0; j < N; j += 997) if (keep[j].v !== j) {{ ok = false; break; }}
        console.log(keep.length + ":" + ok + ":" + keep[0].v + ":" + keep[N - 1].v);
        "#
    ));
    assert_eq!(out[0], format!("{n}:true:0:{}", n - 1));
}

/// Oracle idiom 2: young keys and values inserted into an OLD Map, with array
/// garbage alongside so the Map's backing store is repeatedly the survivor of
/// a young sweep.
#[test]
fn nursery_parity_old_map_inserts_survive_minors() {
    let n = scaled(200_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var m = new Map();
        for (var i = 0; i < N; i++) {{
          m.set("k" + i, {{ v: i * 3 }});
          var g = [i, i + 1];
          if (g.length !== 2) console.log("unreachable");
        }}
        var probe = m.get("k0").v + m.get("k" + ((N / 2) | 0)).v + m.get("k" + (N - 1)).v;
        console.log(m.size + ":" + probe);
        "#
    ));
    let probe = (n / 2) * 3 + (n - 1) * 3;
    assert_eq!(out[0], format!("{n}:{probe}"));
}

/// Free-list reuse integrity: churned slots recycle through minors while an
/// inline cache keeps reading the same site, then the SAME slots are refilled
/// with a different shape. A version not bumped on a minor free would let a
/// stale cache read the dead occupant's layout — a wrong sum here, not a
/// subtle one.
#[test]
fn nursery_parity_slot_reuse_after_minors_keeps_caches_honest() {
    let n = scaled(300_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        function read(o) {{ return o.x; }}
        var sum = 0;
        for (var i = 0; i < N; i++) sum += read({{ x: i }});
        for (var i = 0; i < ((N / 4) | 0); i++) sum += read({{ y: 1, x: 7 }});
        console.log(sum);
        "#
    ));
    let sum = (n as u64 * (n as u64 - 1)) / 2 + (n as u64 / 4) * 7;
    assert_eq!(out[0], format!("{sum}"));
}

/// A generation-spanning graph through CELLS: closures capture per-iteration
/// state that must survive every minor between creation and the read at the
/// end, while 99% of iterations are pure young garbage.
#[test]
fn nursery_parity_captured_cells_span_generations() {
    let n = scaled(200_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        function mk(i) {{ var c = {{ n: i }}; return function () {{ return c.n; }}; }}
        var fns = [];
        for (var i = 0; i < N; i++) {{
          if ((i % 100) === 0) fns.push(mk(i));
          var junk = {{ j: i }};
          if (junk.j < 0) console.log("unreachable");
        }}
        var t = 0;
        for (var k = 0; k < fns.length; k++) t += fns[k]();
        console.log(fns.length + ":" + t);
        "#
    ));
    let kept = n.div_ceil(100);
    let t: u64 = (0..kept as u64).map(|k| k * 100).sum();
    assert_eq!(out[0], format!("{kept}:{t}"));
}

/// Side-table pruning on freed young slots: EVERY array here gets a named
/// property (an `arr_props` entry keyed by its slot), and almost all of them
/// die young. A minor that freed the slot but kept the entry would hand the
/// next occupant a `tag` it never wrote.
#[test]
fn nursery_parity_arr_props_entries_die_with_their_young_arrays() {
    let n = scaled(200_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var keep = [];
        for (var i = 0; i < N; i++) {{
          var a = [i, i + 1];
          a.tag = i;
          if ((i % 1000) === 0) keep.push(a);
        }}
        var s = 0, phantom = 0;
        for (var k = 0; k < keep.length; k++) s += keep[k].tag + keep[k][0];
        // A freshly built array must NOT inherit a dead one's tag.
        for (var q = 0; q < 1000; q++) if ([q].tag !== undefined) phantom++;
        console.log(keep.length + ":" + s + ":" + phantom);
        "#
    ));
    let kept = n.div_ceil(1000);
    let s: u64 = (0..kept as u64).map(|k| 2 * k * 1000).sum();
    assert_eq!(out[0], format!("{kept}:{s}:0"));
}

/// `collection_index` pruning: short-lived Maps/Sets (young, dead) churn while
/// a retained Map keeps being written — a stale index inherited by a recycled
/// Map slot would corrupt lookups.
#[test]
fn nursery_parity_collection_churn_keeps_live_collections_exact() {
    let n = scaled(150_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var m = new Map(), s = new Set();
        for (var i = 0; i < N; i++) {{
          var tmp = new Map();
          tmp.set(i, i);
          var ts = new Set();
          ts.add(i);
          m.set(i % 1000, {{ v: i }});
          s.add("s" + (i % 500));
        }}
        console.log(m.size + ":" + s.size + ":" + m.get(0).v + ":" + s.has("s499"));
        "#
    ));
    let last0 = n - 1000; // last i with i % 1000 == 0
    assert_eq!(out[0], format!("1000:500:{last0}:true"));
}

/// Remset battery — §1 case 1: an OLD object's existing field overwritten to
/// point at a NEW young object (the in-place data store: no key add, no
/// version bump, the exact write the stage-3 barrier exists for). The holders
/// are hot enough that the JIT's SetProp IC fills for them, so the same run
/// also exercises the call-free-way scan-root registration: after the fill,
/// stores to those receivers make NO helper call, and only the persistent
/// scan root keeps their young referents alive across minors.
#[test]
fn nursery_parity_old_field_overwrite_reaches_young() {
    let n = scaled(300_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var holders = [];
        for (var h = 0; h < 8; h++) holders.push({{ f: null, id: h }});
        function w(o, v) {{ o.f = v; }}
        for (var i = 0; i < N; i++) {{
          w(holders[i & 7], {{ v: i }});
          var g = {{ a: i }}; // young garbage driving collections
          if (g.a === -1) console.log("unreachable");
        }}
        var s = 0;
        for (var h2 = 0; h2 < 8; h2++) s += holders[h2].f.v;
        console.log(s);
        "#
    ));
    // holder h & 7 last written at the largest i < N with i & 7 == h.
    let last: u64 = (0..8u64).map(|h| (n as u64 - 8) + ((h + 8 - (n as u64 & 7)) % 8)).sum();
    assert_eq!(out[0], format!("{last}"));
}

/// Remset battery — §1 case 3: an OLD captured cell REASSIGNED to a fresh
/// young object every iteration (`Heap::cell_set`'s barrier; the JIT routes
/// CellSet/UpvalSet through the same helper). The cell itself goes old after
/// the first collection; every later write is an old→young edge.
#[test]
fn nursery_parity_old_cell_reassigned_to_young() {
    let n = scaled(250_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        function mk() {{
          var state = {{ v: -1 }};
          return {{
            set: function (i) {{ state = {{ v: i }}; }},
            get: function () {{ return state.v; }},
          }};
        }}
        var c = mk();
        for (var i = 0; i < N; i++) {{
          c.set(i);
          var g = [i]; // young garbage driving collections
          if (g.length !== 1) console.log("unreachable");
        }}
        console.log(c.get());
        "#
    ));
    assert_eq!(out[0], format!("{}", n - 1));
}

/// Remset battery — §1 cases 5/6: async reaction edges and suspended-window
/// re-parks. Old pending promises gain young reactions (`.then` on a promise
/// created before the churn), an async activation suspends across the churn
/// (its re-parked window holds young objects), and the resolves deliver young
/// values through old promises.
#[test]
fn nursery_parity_async_reactions_and_reparks_span_minors() {
    let n = scaled(200_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var resolveLate;
        var late = new Promise(function (res) {{ resolveLate = res; }});
        async function driver() {{
          var local = {{ tag: "kept-across-await" }}; // parked in the window
          var got = await late;
          return got.v + ":" + local.tag;
        }}
        var done = driver();
        for (var i = 0; i < N; i++) {{
          var g = {{ a: i, b: [i] }}; // young garbage driving collections
          if (g.a === -1) console.log("unreachable");
        }}
        // `late` and the suspended activation are OLD now; the resolution
        // value and the .then callback are young.
        resolveLate({{ v: 42 }});
        done.then(function (s) {{ console.log(s); }});
        "#
    ));
    assert_eq!(out[0], "42:kept-across-await");
}

/// Remset battery — a plain GENERATOR's re-parked register window: the
/// generator goes old while suspended; each resume writes a window holding
/// fresh young objects back into it (the `repark_window` card), and the
/// young values must survive the minors that run between `next()` calls.
#[test]
fn nursery_parity_generator_windows_span_minors() {
    let n = scaled(120_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        function* gen() {{
          var kept = {{ sum: 0 }}; // lives in the parked window between resumes
          while (true) {{
            var got = yield kept.sum;
            kept = {{ sum: kept.sum + got.d }}; // fresh young object each resume
          }}
        }}
        var g = gen();
        g.next();
        var out = 0;
        for (var i = 0; i < N; i++) {{
          if ((i % 1000) === 0) out = g.next({{ d: 1 }}).value;
          var junk = {{ j: i }};
          if (junk.j < 0) console.log("unreachable");
        }}
        console.log(out + ":" + g.next({{ d: 5 }}).value);
        "#
    ));
    let resumes = n.div_ceil(1000); // i % 1000 == 0 resumes
    assert_eq!(out[0], format!("{}:{}", resumes, resumes + 5));
}

/// Remset battery — Map VALUE update in place (`m.set(existingKey, young)`
/// with a NON-heap key, so the insert-path key barrier never fires and only
/// the value barrier in the `set` arm covers the edge), plus `getOrInsert`.
#[test]
fn nursery_parity_old_map_value_updates_reach_young() {
    let n = scaled(200_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var m = new Map();
        for (var k = 0; k < 64; k++) m.set(k, null);
        for (var i = 0; i < N; i++) {{
          m.set(i & 63, {{ v: i }}); // int key: update-in-place after round 1
          var g = {{ a: i }};
          if (g.a === -1) console.log("unreachable");
        }}
        var s = 0;
        for (var k2 = 0; k2 < 64; k2++) s += m.get(k2).v;
        console.log(m.size + ":" + s);
        "#
    ));
    let s: u64 = (0..64u64).map(|k| (n as u64 - 64) + ((k + 64 - (n as u64 & 63)) % 64)).sum();
    assert_eq!(out[0], format!("64:{s}"));
}

/// Remset battery — trivial class SETTERS: the method-inline planner bakes a
/// call-free `this.<field> = v` store for hot receivers, which only the
/// persistent scan-root registration covers once the receivers are old.
#[test]
fn nursery_parity_trivial_setter_stores_reach_young() {
    let n = scaled(250_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        class Box {{
          constructor() {{ this._v = null; }}
          set v(x) {{ this._v = x; }}
          get v() {{ return this._v; }}
        }}
        var boxes = [new Box(), new Box(), new Box(), new Box()];
        for (var i = 0; i < N; i++) {{
          boxes[i & 3].v = {{ n: i }};
          var g = {{ a: i }};
          if (g.a === -1) console.log("unreachable");
        }}
        var s = 0;
        for (var b = 0; b < 4; b++) s += boxes[b].v.n;
        console.log(s);
        "#
    ));
    let s: u64 = (0..4u64).map(|b| (n as u64 - 4) + ((b + 4 - (n as u64 & 3)) % 4)).sum();
    assert_eq!(out[0], format!("{s}"));
}

/// W9 static pretenure, idiom: `JSON.parse`'s tree allocates OLD-clean
/// (skipping the young log), then user code stores YOUNG values into the
/// pretenured holders across many minors. Every such store is an old→young
/// edge from the very first write — if any store path into the parsed tree
/// missed its barrier, the young value would be swept alive; the
/// `ZIPP_NURSERY_VERIFY` mode children turn that into a panic naming the
/// slot. The final sums are mode-independent arithmetic.
#[test]
fn nursery_parity_pretenured_json_tree_mutates_young() {
    let n = scaled(150_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var parts = [];
        for (var b = 0; b < 512; b++) parts.push('{{"id":' + b + ',"tag":"t' + b + '"}}');
        var tree = JSON.parse("[" + parts.join(",") + "]"); // pretenured OLD wholesale
        for (var i = 0; i < N; i++) {{
          tree[i & 511].fresh = {{ v: i }};    // young value into a pretenured object
          tree[(i + 7) & 511].tag = "y" + i;  // young string into a pretenured object
          var g = {{ a: i, b: [i] }};          // young garbage driving minors
          if (g.a === -1) console.log("unreachable");
        }}
        var vsum = 0, ok = true;
        for (var j = 0; j < 512; j++) {{
          if (tree[j].id !== j) {{ ok = false; break; }}
          if (tree[j].fresh) vsum += tree[j].fresh.v;
        }}
        console.log(ok + ":" + vsum);
        "#
    ));
    // The last 512 iterations each leave tree[i & 511].fresh = {v: i}.
    let vsum: u64 = ((n as u64 - 512)..n as u64).sum();
    assert_eq!(out[0], format!("true:{vsum}"));
}

/// W9 static pretenure, idiom: `String.prototype.split`'s parts array
/// allocates OLD-clean, then young objects are stored into it by index (the
/// B119 idiom-1 lane, `jit_set_index`). A missed barrier on the pretenured
/// array sweeps the young elements alive; VERIFY-mode children prove
/// coverage, and the sum is mode-independent.
#[test]
fn nursery_parity_pretenured_split_array_receives_young() {
    let n = scaled(150_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var s = "";
        for (var b = 0; b < 256; b++) s += (b ? "," : "") + "w" + b;
        var arr = s.split(","); // parts + array pretenured OLD
        for (var i = 0; i < N; i++) {{
          arr[i & 255] = {{ v: i }};        // young object into the pretenured array
          var g = [i, i + 1];               // young garbage driving minors
          if (g.length !== 2) console.log("unreachable");
        }}
        var vsum = 0;
        for (var j = 0; j < 256; j++) vsum += arr[j].v;
        console.log(arr.length + ":" + vsum);
        "#
    ));
    let vsum: u64 = ((n as u64 - 256)..n as u64).sum();
    assert_eq!(out[0], format!("256:{vsum}"));
}

/// W10 adaptive budget: high young survival (a retained build) must grow the
/// budget; the churn tail must bring it back to the floor; env pins hold it
/// fixed. Each probe spawns a child with a clean env (the latches are
/// per-process) running an in-process assert against
/// `zipp_vm::gc_young_budget_stats()`.
#[test]
fn budget_adapts_and_pins() {
    let exe = std::env::current_exe().expect("test exe path");
    for (probe, envs) in [
        ("probe_budget_grows_then_shrinks", vec![]),
        ("probe_budget_pinned_by_env", vec![("ZIPP_NURSERY_YOUNG_BUDGET", "32768")]),
        ("probe_budget_pinned_by_no_adapt", vec![("ZIPP_NO_NURSERY_ADAPT", "1")]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg(probe).arg("--ignored");
        cmd.env_remove("ZIPP_NURSERY_YOUNG_BUDGET");
        cmd.env_remove("ZIPP_NO_NURSERY_ADAPT");
        cmd.env_remove("ZIPP_GC_STRESS");
        cmd.env_remove("ZIPP_NO_NURSERY");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{probe} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("running 0 tests"), "{probe} filter matched nothing");
    }
}

/// Spawned by `budget_adapts_and_pins` with a clean env: build a large
/// retained structure (survival ~100% per epoch → budget doubles), then
/// churn (survival ~0% → halves back to the 16384 floor).
#[test]
#[ignore = "spawned by budget_adapts_and_pins with a clean env"]
fn probe_budget_grows_then_shrinks() {
    let out = run_ok(
        r#"
        "use strict";
        var keep = [];
        for (var i = 0; i < 200000; i++) keep.push({ v: i });   // high survival
        var s = 0;
        for (var j = 0; j < 400000; j++) { var g = { a: j }; s = (s + g.a) | 0; } // churn
        console.log(keep.length + ":" + (s !== 0));
        "#,
    );
    assert_eq!(out[0], "200000:true");
    let (last, peak) = zipp_vm::gc_young_budget_stats();
    assert!(peak >= 65536, "budget never grew: peak {peak}");
    assert_eq!(last, 16384, "budget did not shrink back: last {last}");
}

/// Spawned with ZIPP_NURSERY_YOUNG_BUDGET=32768: pinned at the env value.
#[test]
#[ignore = "spawned by budget_adapts_and_pins with ZIPP_NURSERY_YOUNG_BUDGET=32768"]
fn probe_budget_pinned_by_env() {
    let out = run_ok(
        r#"
        "use strict";
        var keep = [];
        for (var i = 0; i < 200000; i++) keep.push({ v: i });
        console.log(keep.length);
        "#,
    );
    assert_eq!(out[0], "200000");
    let (last, peak) = zipp_vm::gc_young_budget_stats();
    assert_eq!((last, peak), (32768, 32768), "env pin did not hold");
}

/// Spawned with ZIPP_NO_NURSERY_ADAPT=1: pinned at the 16384 default.
#[test]
#[ignore = "spawned by budget_adapts_and_pins with ZIPP_NO_NURSERY_ADAPT=1"]
fn probe_budget_pinned_by_no_adapt() {
    let out = run_ok(
        r#"
        "use strict";
        var keep = [];
        for (var i = 0; i < 200000; i++) keep.push({ v: i });
        console.log(keep.length);
        "#,
    );
    assert_eq!(out[0], "200000");
    let (last, peak) = zipp_vm::gc_young_budget_stats();
    assert_eq!((last, peak), (16384, 16384), "no-adapt pin did not hold");
}

/// W10 value-grain remset: the SAME young value stored repeatedly into an old
/// holder in a tight loop — pins the GEN_VLOG dedup (one vremset entry, not
/// one per store) and the repeat-store fast path, while minors keep the value
/// alive across the loop.
#[test]
fn nursery_parity_same_young_value_stored_repeatedly() {
    let n = scaled(400_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var keep = [null];
        for (var w = 0; w < 64; w++) keep.push({{ pin: w }}); // grow + survive
        var y = {{ v: 42 }};
        for (var i = 0; i < N; i++) {{
          keep[0] = y;                    // same young value, every iteration
          var g = {{ a: i }};              // young garbage driving minors
          if (g.a === -1) console.log("unreachable");
        }}
        console.log(keep[0].v + ":" + keep.length);
        "#
    ));
    assert_eq!(out[0], "42:65");
}

/// W10 value-grain remset: young values stored into old array slots and then
/// OVERWRITTEN with ints before the next minor — the recorded values float
/// one epoch by design (conservative), and the majors reclaim them: the
/// bounded-heap arithmetic must still hold and the final ints must read back.
#[test]
fn nursery_parity_young_store_overwritten_before_minor() {
    let n = scaled(300_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var arr = [];
        for (var w = 0; w < 128; w++) arr.push(0);
        for (var i = 0; i < N; i++) {{
          arr[i & 127] = {{ v: i }};       // young in
          arr[i & 127] = i;               // overwritten with an int
          var g = [i];
          if (g.length !== 1) console.log("unreachable");
        }}
        var s = 0;
        for (var j = 0; j < 128; j++) s += arr[j];
        console.log(s);
        "#
    ));
    let s: u64 = ((n as u64 - 128)..n as u64).sum();
    assert_eq!(out[0], format!("{s}"));
}

/// W10 value-grain remset: a young value recorded via old holder A, then A's
/// slot nulled and the value kept ONLY through old holder B (stored in the
/// same epoch) — both records independently keep it alive; losing either
/// would sweep it live.
#[test]
fn nursery_parity_value_moved_between_old_holders() {
    let n = scaled(200_000);
    let out = run_ok(&format!(
        r#"
        "use strict";
        var N = {n};
        var a = {{ slot: null }};
        var b = {{ slot: null }};
        var g0 = {{ warm: 1 }};
        for (var w = 0; w < 70000; w++) {{ var t = {{ x: w }}; if (t.x < 0) console.log(t.x); }}
        var sum = 0;
        for (var i = 0; i < N; i++) {{
          var y = {{ v: i }};
          a.slot = y;                      // record via A
          b.slot = y;                      // record via B (same epoch)
          a.slot = null;                   // A no longer holds it
          sum = (sum + b.slot.v) | 0;      // alive through B across minors
          var g = {{ churn: i }};
          if (g.churn === -1) console.log("unreachable");
        }}
        console.log(sum + ":" + (a.slot === null) + ":" + b.slot.v);
        "#
    ));
    let mut sum: i64 = 0;
    for i in 0..n as i64 {
        sum = (sum + i) as i32 as i64;
    }
    assert_eq!(out[0], format!("{sum}:true:{}", n - 1));
}

/// Re-run every `nursery_parity_` case in eight more modes, each in its own
/// child process (the env latches are read once per process). The same
/// arithmetic assertions passing in all modes IS the parity check; a swept
/// survivor or stale side-table entry fails the child loudly, and the
/// `ZIPP_NURSERY_VERIFY` rows turn any missed write barrier into a panic
/// NAMING the slot at the very minor where the hole opened.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 12] = [
        &[("ZIPP_NO_NURSERY", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NO_NURSERY", "1"), ("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_SHAPE_VERIFY", "1"), ("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NURSERY_VERIFY", "1")],
        &[("ZIPP_NURSERY_VERIFY", "1"), ("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_GCSTATS", "1")],
        &[("ZIPP_NOJIT", "1")],
        // W10: value-grain remset off (holder-grain), alone, under stress,
        // and under the verifier — the escape hatch must stay a tested
        // configuration.
        &[("ZIPP_NO_VALGRAIN_REMSET", "1")],
        &[("ZIPP_NO_VALGRAIN_REMSET", "1"), ("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NO_VALGRAIN_REMSET", "1"), ("ZIPP_NURSERY_VERIFY", "1")],
        // W10: minor-marks cache off (per-minor rebuild).
        &[("ZIPP_NO_NONYOUNG_CACHE", "1")],
    ];
    for envs in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("nursery_parity_");
        cmd.env("ZIPP_NURSERY", "1"); // opt-in (B120); NO_NURSERY modes still force off
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let label: Vec<String> = envs.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let label = label.join(" ");
        assert!(
            out.status.success(),
            "{label} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the nursery_parity_ filter matched nothing under {label}:\n{stdout}"
        );
    }
}

/// Spawned by `sustained_old_garbage_keeps_the_heap_bounded` with a clean env
/// so the numbers are deterministic whatever mode the suite runs under: the
/// nursery on (minors actually happen) and no stress (a collection per safe
/// point would make 1.2M allocations glacial).
///
/// The workload is the one shape the B119 oracle says produces OLD garbage:
/// a sawtooth — build a large retained structure, then drop it whole. Objects
/// allocated early in a round survive the collections that fire while the
/// round keeps building (they are reachable through `big`), so by the time
/// the round is dropped they are OLD, and a young-only sweep can never
/// reclaim them. Without the post-minor major latch (`Heap::major_at`: a
/// minor that leaves the heap at/above the pre-nursery collection point
/// majors next) they would pile up round after round; `peak slots` staying
/// under the bound plus `majors >= 1` is the direct proof the latch fires
/// and the floated garbage is reclaimed.
#[test]
#[ignore = "spawned by sustained_old_garbage_keeps_the_heap_bounded with a clean env"]
fn probe_bounded_heap_under_sustained_old_garbage() {
    let out = run_ok(
        r#"
        "use strict";
        var big = null;
        for (var r = 0; r < 9; r++) {
          big = [];
          for (var i = 0; i < 200000; i++) big.push({ r: r, i: i });
          console.log(r + ":" + big.length);
        }
        "#,
    );
    assert_eq!(out.last().map(String::as_str), Some("8:200000"));
    let (minors, majors, _, _, swept_young, _, _, peak) = zipp_vm::gc_nursery_stats();
    assert!(minors >= 2, "the nursery never ran a minor (minors={minors})");
    assert!(
        majors >= 1,
        "sustained old garbage must eventually latch a major (majors={majors}, minors={minors})"
    );
    assert!(swept_young > 0, "minors reclaimed nothing young");
    assert!(
        peak < 2_500_000,
        "heap ballooned to {peak} slots — the float budget did not bound floated garbage"
    );
}

/// Pure churn twin of the probe above: half a million short-lived objects
/// against a near-empty live set. Minors must keep recycling the same free
/// slots, so the slot vector plateaus around the 65,536 GC floor instead of
/// tracking the allocation count.
#[test]
#[ignore = "spawned by sustained_old_garbage_keeps_the_heap_bounded with a clean env"]
fn probe_churn_reuses_slots_through_minors() {
    let out = run_ok(
        r#"
        "use strict";
        var last = 0;
        for (var i = 0; i < 500000; i++) {
          var o = { a: i, b: i + 1 };
          last = o.a;
        }
        console.log("last:" + last);
        "#,
    );
    assert_eq!(out[0], "last:499999");
    let (minors, _, _, _, _, _, _, peak) = zipp_vm::gc_nursery_stats();
    assert!(minors >= 3, "churn at this scale must run several minors (minors={minors})");
    assert!(
        peak < 200_000,
        "peak {peak} slots for a ~zero live set — minors are not recycling the free list"
    );
}

#[test]
fn sustained_old_garbage_keeps_the_heap_bounded() {
    let exe = std::env::current_exe().expect("test exe path");
    for probe in
        ["probe_bounded_heap_under_sustained_old_garbage", "probe_churn_reuses_slots_through_minors"]
    {
        let out = std::process::Command::new(&exe)
            .args([probe, "--ignored", "--exact"])
            .env("ZIPP_NURSERY", "1") // opt-in (B120)
            .env_remove("ZIPP_NO_NURSERY")
            .env_remove("ZIPP_GC_STRESS")
            .output()
            .expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{probe} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the {probe} filter matched nothing:\n{stdout}"
        );
    }
}
