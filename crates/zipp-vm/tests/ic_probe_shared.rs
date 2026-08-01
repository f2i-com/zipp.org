//! The emitted inline-cache probe, now written once instead of four times.
//!
//! `GetProp` and `SetProp` each emitted their own 8-way probe in `region_mem.rs`
//! and again in `proto_mem.rs` — four copies of ~140 lines of dynasm with the
//! entry layout spelled out as literal displacements (`+0`, `+8`, `+16`, `+20`,
//! `+24`) and a literal stride of 64, guarded by no static assertion.
//!
//! They did not stay identical. The store path's `and edx, 0x00FF_FFFF`, which
//! masks the hop count out of `slot_nhops` before indexing, was once absent from
//! one of them — and an unmasked hop count there is a store at
//! `vals + nhops*2^24*8`. A wild write, not a wrong read. `emit_ic_probe` makes
//! that class of divergence unrepresentable, and
//! `assert!(size_of::<IcEntry>() == JIT_IC_STRIDE)` makes the other one
//! (raising `JIT_IC_MAX_HOPS` and silently reading every way misaligned) a
//! compile error.
//!
//! Nothing below is new behaviour — that is the point. Each case drives one of
//! the four probes through a state that must invalidate a cached way, and each
//! result was checked against node and against the pre-refactor binary.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Own get and own set in a hot region — the `nhops == 0` fast lane of both
/// probes, the one that must never walk a hop.
#[test]
fn own_get_and_set_in_a_hot_region() {
    let out = run_ok(
        r#""use strict";
        const o = { x: 0, y: 1 };
        let s = 0;
        for (let i = 0; i < 200000; i++) { o.x = i; s += o.x + o.y; }
        console.log(s);
        "#,
    );
    assert_eq!(out, vec!["20000100000"]);
}

/// Reads resolving at depths 1, 2 and 3 plus an accessor: the hop-walk arm,
/// which only `GetProp` emits.
#[test]
fn prototype_chain_reads_walk_the_guarded_hops() {
    let out = run_ok(
        r#""use strict";
        function Base() { this.a = 1; this.b = 2; }
        Object.defineProperty(Base.prototype, "acc",
            { get() { return this.a * 10; }, configurable: true });
        function Mid() { Base.call(this); this.c = 3; }
        Mid.prototype = Object.create(Base.prototype);
        function Leaf() { Mid.call(this); this.d = 4; }
        Leaf.prototype = Object.create(Mid.prototype);
        Leaf.prototype.depth = 5;
        const l = new Leaf();
        let s = 0;
        for (let i = 0; i < 200000; i++) s += l.a + l.c + l.d + l.depth + l.acc;
        console.log(s);
        "#,
    );
    assert_eq!(out, vec!["4600000"]);
}

/// Seventeen distinct receivers of ONE shape against eight ways: every access
/// evicts and refills, so this is the miss-helper path rather than the probe's.
/// It is the 9th-receiver cliff `property-ic-shapes` exists to measure, and the
/// reason a shape-keyed way is the next step.
#[test]
fn more_receivers_than_ways_thrashes_correctly() {
    let out = run_ok(
        r#""use strict";
        const objs = [];
        for (let i = 0; i < 17; i++) objs.push({ k: i });
        let s = 0;
        for (let i = 0; i < 200000; i++) { const o = objs[i % 17]; o.k = o.k + 1; s += o.k; }
        console.log(s);
        "#,
    );
    assert_eq!(out, vec!["1178170560"]);
}

/// A key added halfway through a hot loop bumps the receiver's version, so every
/// filled way must stop matching from that iteration on.
#[test]
fn a_shape_change_mid_loop_invalidates_filled_ways() {
    let out = run_ok(
        r#""use strict";
        const o = { p: 1 };
        let s = 0;
        for (let i = 0; i < 200000; i++) {
            if (i === 100000) o.q = 2;
            s += o.p + (o.q === undefined ? 0 : o.q);
        }
        console.log(s);
        "#,
    );
    assert_eq!(out, vec!["400000"]);
}

/// A frozen receiver must never take the call-free store: in strict code every
/// write throws, and the slot keeps its original value.
#[test]
fn a_frozen_receiver_never_takes_the_call_free_store() {
    let out = run_ok(
        r#""use strict";
        const o = Object.freeze({ f: 1 });
        let threw = 0;
        for (let i = 0; i < 50000; i++) { try { o.f = i; } catch (e) { threw++; } }
        console.log(o.f + ":" + threw);
        "#,
    );
    assert_eq!(out, vec!["1:50000"]);
}

/// Deleting a key SHIFTS the remaining slots. A cached way that survived would
/// read the wrong property, so this is the case the version guard exists for.
#[test]
fn deleting_a_key_shifts_slots_and_invalidates() {
    let out = run_ok(
        r#""use strict";
        const o = { m: 1, n: 2, p: 3 };
        let s = 0;
        for (let i = 0; i < 200000; i++) { if (i === 1000) delete o.n; s += o.m + o.p; }
        console.log(s);
        "#,
    );
    assert_eq!(out, vec!["800000"]);
}

/// A prototype SETTER cannot be a cached data-slot store — it routes through
/// PROP_VIA_IC and frame-calls user code, after which the pinned `r13`/`r14`
/// must be re-derived.
#[test]
fn a_prototype_setter_routes_through_the_slow_helper() {
    let out = run_ok(
        r#""use strict";
        let seen = 0;
        const proto = { set s(v) { seen += v; } };
        const o = Object.create(proto);
        for (let i = 0; i < 100000; i++) o.s = 1;
        console.log(seen);
        "#,
    );
    assert_eq!(out, vec!["100000"]);
}

/// A method resolved through the chain and called in a loop — the whole-function
/// (`proto_mem.rs`) compile, so its copy of both probes is the one under test.
#[test]
fn a_chain_method_call_uses_the_tier_c_probes() {
    let out = run_ok(
        r#""use strict";
        function Base() { this.a = 1; this.b = 2; }
        Base.prototype.pget = function () { return this.a + this.b; };
        const b = new Base();
        let s = 0;
        for (let i = 0; i < 200000; i++) s += b.pget();
        console.log(s);
        "#,
    );
    assert_eq!(out, vec!["600000"]);
}
