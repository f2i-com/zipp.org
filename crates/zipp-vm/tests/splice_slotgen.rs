//! W12: splice slot-generation guard — a spliced leaf call whose callee
//! register provably holds global slot g's value guards `global_gens[g]` (one
//! 32-bit compare) instead of re-checking the callee bits+version per
//! execution. Soundness rests on bump completeness: every possible write to a
//! KEYED slot goes through `Vm::bump_global_gen` (bytecode-storable slots are
//! excluded from keying, fail-closed; eval registration un-keys and bumps).
//!
//! The rebind matrix below flips a hot spliced callee mid-loop through every
//! rebind route and asserts byte-identical output against `node -e` — a
//! missed bump would keep running the STALE spliced body past the rebind
//! iteration and change the printed accumulator. The final test re-runs the
//! matrix under `ZIPP_NO_SPLICE_SLOTGEN=1` (off-switch), `ZIPP_NOJIT=1`,
//! `ZIPP_JIT_THRESHOLD=1`, and `ZIPP_GC_STRESS=1` (shakes the rooting /
//! non-moving-collector half of the argument) in child processes.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (expected values come from `node -e`)");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn assert_matches_node(src: &str) {
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for: {src}");
}

/// The matrix skeleton: a hot loop splicing leaf `f`, rebound at exactly
/// iteration 30000 by `rebind`. The accumulator encodes the flip iteration,
/// so a stale spliced body running even ONE extra iteration changes the line.
fn matrix_src(rebind: &str) -> String {
    format!(
        r#"
        function f(x) {{ return x + 1; }}
        function g(x) {{ return x + 1000; }}
        var acc = 0;
        for (var i = 0; i < 60000; i++) {{
            if (i === 30000) {{ {rebind} }}
            acc += f(i & 7);
        }}
        console.log("acc:" + acc);
        "#
    )
}

/// `globalThis.f = g` — the main rebind path (the global-object own-property
/// write-through sync, access.rs), on a KEYED slot (function-decl bindings
/// are never bytecode-stored).
#[test]
fn sg_parity_globalthis_rebind() {
    assert_matches_node(&matrix_src("globalThis.f = g;"));
}

/// Direct `eval('f = g')` — new bytecode targeting a keyed slot. The eval
/// registration hook must permanently un-key the slot AND bump its generation
/// BEFORE the store runs; no route-epoch change backs this variant up, so the
/// generation guard is the only thing standing between the rebind and a stale
/// spliced body.
#[test]
fn sg_parity_direct_eval_rebind() {
    assert_matches_node(&matrix_src("eval('f = g;');"));
}

/// Indirect eval — same store through the indirect-eval registration path.
#[test]
fn sg_parity_indirect_eval_rebind() {
    assert_matches_node(&matrix_src("(0, eval)('f = g;');"));
}

/// `Object.defineProperty(globalThis, 'f', {value: g})` — a real descriptor
/// appears behind the slot; the existing global-route machinery (not this
/// guard) must keep the tiers agreeing.
#[test]
fn sg_parity_define_property_rebind() {
    assert_matches_node(&matrix_src(
        "Object.defineProperty(globalThis, 'f', { value: g });",
    ));
}

/// `delete globalThis.f` (sloppy): a top-level function declaration's global
/// property is non-configurable, so the delete FAILS and nothing may change —
/// pinned against node so an over-eager uninitialize can never hide behind
/// the guard.
#[test]
fn sg_parity_delete_rebind() {
    assert_matches_node(&matrix_src("delete globalThis.f;"));
}

/// Control: a PLAIN top-level store `f = g` makes the slot bytecode-stored,
/// which must DECLINE keying (asserted via JITLOG below) — and still flip at
/// the exact iteration through today's guard.
#[test]
fn sg_parity_plain_store_control() {
    assert_matches_node(&matrix_src("f = g;"));
}

/// JITLOG engagement: the globalThis variant must KEY its hot site
/// (`slot_guard=g<slot>@gen<v>`), the plain-store control must DECLINE with
/// reason `bytecode-stored`. Runs each case in a child so the log lines land
/// on a capturable stderr.
#[test]
fn slotgen_jitlog_keyed_and_declined() {
    let exe = std::env::current_exe().expect("test exe path");
    for (filter, needle) in [
        ("sg_parity_globalthis_rebind", "slot_guard=g"),
        ("sg_parity_plain_store_control", "slot_guard=DECLINED(bytecode-stored)"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg(filter)
            .arg("--exact")
            .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
            .env("ZIPP_JITLOG", "1")
            .env("ZIPP_JIT_THRESHOLD", "1")
            .output()
            .expect("spawn the test binary");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{filter} child failed:\n{stderr}");
        assert!(
            stderr.contains(needle),
            "{filter}: expected '{needle}' in JITLOG:\n{stderr}"
        );
    }
}

/// Off-switch: with `ZIPP_NO_SPLICE_SLOTGEN=1` no site may key (plans emit
/// today's identity+version guard, byte-identically).
#[test]
fn slotgen_off_switch_never_keys() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg("sg_parity_globalthis_rebind")
        .arg("--exact")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env("ZIPP_NO_SPLICE_SLOTGEN", "1")
        .output()
        .expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed:\n{stderr}");
    assert!(
        !stderr.contains("slot_guard="),
        "off-switch still keyed/logged slot guards:\n{stderr}"
    );
}

/// Re-run the whole `sg_parity_` matrix with the guard off, the JIT off, the
/// JIT forced hot, and under GC stress — each in its own child process (env
/// latches are read once per process). Identical node-pinned answers in every
/// mode is the tier-parity check the engine gates hardest on.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_SPLICE_SLOTGEN", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("sg_parity_")
            .env(key, val)
            .output()
            .expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{key}={val} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the sg_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
