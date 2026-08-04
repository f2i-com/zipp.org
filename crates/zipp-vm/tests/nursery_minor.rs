//! Stage-1 nursery (the barrier-free minor GC) — churn against retained
//! graphs, held identical across every collector mode.
//!
//! A minor marks FULLY (all roots, all edges — no remembered set, no write
//! barrier anywhere) and sweeps ONLY the slots allocated since the previous
//! collection, so the one thing that can go wrong is bookkeeping around what
//! the young sweep freed: a side-table entry surviving its freed key (a
//! recycled slot inheriting state), a survivor swept, or floated old garbage
//! accumulating without bound. Each test below aims at one of those:
//!
//!   * the two old→young idioms B119's oracle measured as the ONLY ones the
//!     suite produces — young values pushed into a retained array (227k,
//!     `jit_set_index`) and young entries into an old Map (133k,
//!     `coll_insert`) — must survive minors because the full mark sees them;
//!   * slot reuse after a minor must never serve a stale inline cache
//!     (versions bump on free — also pinned at the unit level in `heap.rs`);
//!   * side tables keyed by freed young slots (`arr_props` named array
//!     properties, `collection_index`) must drop their entries;
//!   * sustained OLD-garbage production must stay bounded: the float census
//!     latches a major, or the heap balloons (`peak slots` asserts it).
//!
//! `all_modes_answer_identically` re-runs the `nursery_parity_` set in child
//! processes (the env latches are read once per process) under
//! `ZIPP_NO_NURSERY=1` (majors only — the exact pre-nursery collector),
//! `ZIPP_GC_STRESS=1` (a collection at EVERY safe point, alternating three
//! minors to a major), both combined, `ZIPP_SHAPE_VERIFY=1` + stress, the
//! GCSTATS oracle, and `ZIPP_NOJIT=1`. Every assertion is mode-independent
//! arithmetic, so agreeing in all modes IS the parity check.

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

/// Re-run every `nursery_parity_` case in six more modes, each in its own
/// child process (the env latches are read once per process). The same
/// arithmetic assertions passing in all modes IS the parity check; a swept
/// survivor or stale side-table entry fails the child loudly.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 6] = [
        &[("ZIPP_NO_NURSERY", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NO_NURSERY", "1"), ("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_SHAPE_VERIFY", "1"), ("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_GCSTATS", "1")],
        &[("ZIPP_NOJIT", "1")],
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
/// reclaim them. Without the float-census major trigger they would pile up
/// round after round while the threshold compounds off them (~3x per epoch);
/// `peak slots` staying under the bound plus `majors >= 1` is the direct
/// proof the trigger fires and the floated garbage is reclaimed.
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
