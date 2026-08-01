//! A promise's subscriptions are ONE list of pairs, not two parallel vectors.
//!
//! Every registration site — `.then`/`.catch`/`.finally`, internal adoption,
//! combinator subscription, and `await` — supplies both handlers at once with the
//! same dependent and the same kind flags; they differed only in the callback.
//! Storing them in a `fulfill: Vec<Reaction>` and a `reject: Vec<Reaction>` meant
//! the overwhelmingly common single-subscriber promise (a chain link, an
//! `await`, a `Promise.all` element) allocated TWO first buffers to hold two
//! halves of one record. `Reactions::One` holds that record inline.
//!
//! What the old layout encoded STRUCTURALLY, the new one encodes by choosing a
//! field: settlement used to drain the matching vector and leave the other
//! behind; now it drains the one list and selects `on_fulfilled` or
//! `on_rejected` per pair. Everything below is about the two things that can
//! therefore break — which handler runs, and in which tick.
//!
//! Merging also means a settled promise no longer retains its opposite-kind
//! reactions. That was a real retention leak (the GC kept tracing dead handlers
//! and their dependents for the promise's whole life) and it is not separable
//! from the change: one list cannot express "half drained".
//!
//! Every expectation here was executed in node and matches. A 39-outcome
//! ordering differential covering the same ground plus thenables, subclasses and
//! GC pressure is byte-identical to node under default, `ZIPP_NOJIT=1`,
//! `ZIPP_JIT_THRESHOLD=1` and `ZIPP_GC_STRESS=1`.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// The `None -> One` case: the single subscriber, which is the whole point.
#[test]
fn a_single_subscriber_still_runs() {
    let out = run_ok(
        r#""use strict";
        Promise.resolve(1).then(v => console.log("A" + v));
        "#,
    );
    assert_eq!(out, vec!["A1"]);
}

/// `One -> Many` must keep the FIRST registration first. Settlement drains in
/// registration order onto a FIFO queue, so an upgrade that prepended — or that
/// drained a spilled `Vec` before the inline slot — would reorder ticks.
#[test]
fn the_one_to_many_upgrade_preserves_registration_order() {
    let out = run_ok(
        r#""use strict";
        let settle;
        const q = new Promise(res => { settle = res; });
        for (let i = 0; i < 6; i++) q.then(v => console.log("U" + i + v));
        settle("u");
        "#,
    );
    assert_eq!(out, vec!["U0u", "U1u", "U2u", "U3u", "U4u", "U5u"]);
}

/// Fulfil and reject handlers registered ALTERNATELY on one promise. Under two
/// vectors their relative order across kinds was never expressible; under one
/// list it is, and it must match the order they were registered in.
#[test]
fn handlers_of_both_kinds_drain_in_registration_order() {
    let out = run_ok(
        r#""use strict";
        let settle;
        const q = new Promise((_, rej) => { settle = rej; });
        q.then(() => console.log("never1")).catch(() => {});
        q.catch(e => console.log("C1" + e));
        q.then(() => console.log("never2"), e => console.log("C2" + e));
        q.catch(e => console.log("C3" + e));
        settle("!");
        "#,
    );
    assert_eq!(out, vec!["C1!", "C2!", "C3!"]);
}

/// The spec's Identity and Thrower defaults are `undefined` here, not function
/// objects — a pair whose `on_fulfilled` is undefined must forward the value and
/// one whose `on_rejected` is undefined must forward the reason. Selecting the
/// wrong field would silently turn a rejection into a fulfilment.
#[test]
fn undefined_handlers_forward_value_and_reason() {
    let out = run_ok(
        r#""use strict";
        Promise.resolve("f").then().then().then(v => console.log("D" + v));
        Promise.reject("g").then(v => console.log("never" + v)).then()
            .catch(e => console.log("E" + e));
        "#,
    );
    assert_eq!(out, vec!["Df", "Eg"]);
}

/// `.finally` forwards the ORIGINAL completion, and a throw inside it overrides.
/// It routes through `then_internal` like everything else, so it exercises the
/// pair path rather than a lane of its own.
#[test]
fn finally_forwards_the_original_completion() {
    let out = run_ok(
        r#""use strict";
        Promise.resolve("h").finally(() => console.log("Fside"))
            .then(v => console.log("F" + v));
        Promise.reject("i").finally(() => console.log("Gside"))
            .catch(e => console.log("G" + e));
        Promise.resolve("j").finally(() => { throw "boom"; })
            .catch(e => console.log("H" + e));
        "#,
    );
    // `Hboom` lands BEFORE `Fh`/`Gi`: the throwing `finally` rejects its
    // dependent from INSIDE the tick that ran it, so the `catch` is queued a tick
    // earlier than the two pass-through forwards, which each cost an extra hop.
    // Verified in node, which prints exactly this.
    assert_eq!(out, vec!["Fside", "Gside", "Hboom", "Fh", "Gi"]);
}

/// Subscribing to a promise that has ALREADY settled takes the immediate
/// microtask branch and never touches the reaction list. Doing it after the list
/// was drained must not resurrect a drained subscription or miss the new one.
#[test]
fn subscribing_after_settlement_still_runs_once() {
    let out = run_ok(
        r#""use strict";
        let settle;
        const q = new Promise(res => { settle = res; });
        q.then(v => console.log("T1" + v));
        q.then(v => console.log("T2" + v));
        settle("t");
        q.then(v => console.log("T3" + v));
        "#,
    );
    assert_eq!(out, vec!["T1t", "T2t", "T3t"]);
}

/// A reaction that subscribes to the promise it is running for. Settlement must
/// take OWNERSHIP of the list before dispatching, or this mutates the list being
/// iterated.
#[test]
fn a_reaction_may_subscribe_to_its_own_promise() {
    let out = run_ok(
        r#""use strict";
        const q = Promise.resolve("k");
        q.then(v => { console.log("I" + v); q.then(w => console.log("I2" + w)); });
        "#,
    );
    assert_eq!(out, vec!["Ik", "I2k"]);
}

/// `await` registers a pair whose `dependent` is a suspended ACTIVATION rather
/// than a promise, flagged `is_async`, and both halves resume it — one with the
/// value, one by throwing the reason in.
#[test]
fn await_resumes_on_both_settlements() {
    let out = run_ok(
        r#""use strict";
        (async function () {
            console.log("K" + await Promise.resolve("m1"));
            try { await Promise.reject("m2"); } catch (e) { console.log("L" + e); }
            let n = 0;
            for (let i = 0; i < 5; i++) n += await Promise.resolve(i);
            console.log("M" + n);
        })();
        "#,
    );
    assert_eq!(out, vec!["Km1", "Lm2", "M10"]);
}

/// Combinators subscribe many promises to one dependent, and `Promise.all`
/// reuses its own result promise as an inert dependent placeholder — the one
/// place a pair's `dependent` is not a fresh promise.
#[test]
fn combinators_subscribe_many_to_one_dependent() {
    let out = run_ok(
        r#""use strict";
        Promise.all([Promise.resolve(1), 2, Promise.resolve(3)])
            .then(a => console.log("N" + a.join("")));
        Promise.allSettled([Promise.resolve(1), Promise.reject("e")])
            .then(rs => console.log("P" + rs.map(
                r => r.status[0] + ("value" in r ? r.value : r.reason)).join("")));
        Promise.any([Promise.reject("a"), Promise.resolve("b")])
            .then(v => console.log("Q" + v));
        "#,
    );
    assert_eq!(out, vec!["N123", "Pf1re", "Qb"]);
}

/// The collector must trace BOTH handlers of a pending pair. Allocating hard
/// between registration and settlement forces a collection while the pair is the
/// only thing keeping the two closures — and the object they capture — alive.
#[test]
fn the_collector_traces_both_handlers_of_a_pending_pair() {
    let out = run_ok(
        r#""use strict";
        let settle;
        const q = new Promise(res => { settle = res; });
        const captured = { n: 0 };
        q.then(v => console.log("W" + v + captured.n),
               e => console.log("never" + e));
        for (let i = 0; i < 200000; i++) { const junk = { i, s: "x" + i }; if (junk.i === -1) console.log(junk.s); }
        captured.n = 7;
        settle("w");
        "#,
    );
    assert_eq!(out, vec!["Ww7"]);
}

/// A subclass promise is built from the variant literally in
/// `construct/construct.rs`, a second construction site that has to agree with
/// `alloc_promise` about the field.
#[test]
fn a_subclass_promise_uses_the_same_storage() {
    let out = run_ok(
        r#""use strict";
        class MyP extends Promise {}
        MyP.resolve("s").then(v => console.log("S" + v));
        "#,
    );
    assert_eq!(out, vec!["Ss"]);
}
