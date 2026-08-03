//! The ACCESSOR inline-cache way (B114).
//!
//! Pre-B114, an accessor-backed property access from compiled code was a
//! PERMANENT native miss: the probe failed all eight ways, `jit_get_prop_miss`
//! rediscovered the accessor and returned `PROP_VIA_IC`, and a SECOND extern
//! call (`jit_get_prop_slow`) finally frame-called the getter — 1.25M GetProp
//! + 250k SetProp times per polymorphic-objects run (B111 proved the misses
//! permanent by construction). Now the miss helper FILLS an accessor-tagged
//! way (identity + receiver version, plus every hop version for an inherited
//! accessor), and a probe hit dispatches the getter/setter directly.
//!
//! Nothing below may change behaviour — that is the point. Every case drives
//! an accessor through a state that must either invalidate the way (version
//! bump) or defeat its baked fn (the no-bump `__defineGetter__` swap), and
//! every expectation was executed in node (v24) and diffs byte-identical. The
//! whole file must also pass with `ZIPP_NO_ACCESSOR_WAY=1` (fills and probe
//! arm both off — pre-B114 behaviour), `ZIPP_NOJIT=1`, and
//! `ZIPP_JIT_THRESHOLD=1`.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// An own defineProperty accessor (getter + setter) driven hot: the exact
/// receiver shape behind polymorphic-objects' permanent miss stream. The way
/// must serve both directions (get dispatch, set dispatch) without drift.
#[test]
fn own_accessor_get_and_set_in_a_hot_loop() {
    let out = run_ok(
        r#""use strict";
        var o = { hidden: 7, pad: 0 };
        Object.defineProperty(o, "val", {
            get: function () { return this.hidden; },
            set: function (x) { this.hidden = x | 0; },
            enumerable: true, configurable: true
        });
        var s = 0;
        for (var i = 0; i < 200000; i++) {
            o.val = (i & 255) + 1;
            s = (s + o.val) | 0;
        }
        console.log(s + ":" + o.val + ":" + o.hidden);
        "#,
    );
    assert_eq!(out, vec!["25693856:64:64"]);
}

/// A getter/setter pair on the PROTOTYPE (accessor at hop 1): the chain
/// accessor way must guard the hop version, and the setter side must stay on
/// the slow path (Set ways never walk hops — own accessor fills only).
#[test]
fn prototype_accessor_at_hop_1() {
    let out = run_ok(
        r#""use strict";
        var proto = { bias: 100 };
        Object.defineProperty(proto, "acc", {
            get: function () { return this.base + this.bias; },
            set: function (v) { this.base = v * 2; },
            configurable: true
        });
        var o = Object.create(proto);
        o.base = 1;
        o.bias = 100; // shadowing DATA prop reads via `this` still hit o
        var s = 0;
        for (var i = 0; i < 200000; i++) {
            o.acc = i & 15;
            s = (s + o.acc) | 0;
        }
        console.log(s + ":" + o.base);
        "#,
    );
    assert_eq!(out, vec!["23000000:30"]);
}

/// Accessor at hop 2 of an Object.create chain: two hop versions guarded.
#[test]
fn prototype_accessor_at_hop_2() {
    let out = run_ok(
        r#""use strict";
        var g0 = {};
        Object.defineProperty(g0, "deep", {
            get: function () { return this.n * 3; }, configurable: true
        });
        var g1 = Object.create(g0); g1.mid = 1;
        var o = Object.create(g1); o.n = 5;
        var s = 0;
        for (var i = 0; i < 200000; i++) s = (s + o.deep) | 0;
        console.log(s);
        "#,
    );
    assert_eq!(out, vec!["3000000"]);
}

/// An accessor REDEFINED to a data property mid-loop must take the new meaning
/// on the very next access — defineProperty bumps the receiver's version, so
/// the filled accessor way stops matching and a data way replaces it in place.
#[test]
fn accessor_redefined_to_data_mid_loop() {
    let out = run_ok(
        r#""use strict";
        var o = { hidden: 3 };
        Object.defineProperty(o, "v", {
            get: function () { return this.hidden; },
            enumerable: true, configurable: true
        });
        var s = 0;
        for (var i = 0; i < 200000; i++) {
            if (i === 100000) Object.defineProperty(o, "v", { value: 1000, writable: true });
            s = (s + o.v) | 0;
        }
        console.log(s + ":" + o.v);
        "#,
    );
    assert_eq!(out, vec!["100300000:1000"]);
}

/// The reverse flip: a DATA property redefined to an accessor mid-loop. The
/// filled data way must stop matching (version bump) and the accessor way must
/// take over — the write direction too (setter observes every store).
#[test]
fn data_redefined_to_accessor_mid_loop() {
    let out = run_ok(
        r#""use strict";
        var o = { v: 1, sink: 0 };
        var s = 0;
        for (var i = 0; i < 200000; i++) {
            if (i === 100000) {
                Object.defineProperty(o, "v", {
                    get: function () { return 2; },
                    set: function (x) { this.sink = (this.sink + x) | 0; },
                    configurable: true
                });
            }
            o.v = 1;
            s = (s + o.v) | 0;
        }
        console.log(s + ":" + o.sink);
        "#,
    );
    assert_eq!(out, vec!["300000:100000"]);
}

/// A getter DELETED mid-loop: delete bumps the version, the way must die, and
/// the read falls through to `undefined` (chain miss) — summed as NaN-free
/// arithmetic via the guard below, matching node.
#[test]
fn getter_deleted_mid_loop() {
    let out = run_ok(
        r#""use strict";
        var o = { hidden: 4 };
        Object.defineProperty(o, "v", {
            get: function () { return this.hidden; }, configurable: true
        });
        var s = 0, undef = 0;
        for (var i = 0; i < 200000; i++) {
            if (i === 100000) delete o.v;
            var x = o.v;
            if (x === undefined) undef++;
            else s = (s + x) | 0;
        }
        console.log(s + ":" + undef);
        "#,
    );
    assert_eq!(out, vec!["400000:100000"]);
}

/// A getter that THROWS: the frame call reports CALL_THREW, the region exits,
/// and the interpreter unwinds into the catch — every iteration, with the way
/// still filled (a throw is not an invalidation).
#[test]
fn getter_that_throws_every_access() {
    let out = run_ok(
        r#""use strict";
        var o = {};
        Object.defineProperty(o, "boom", {
            get: function () { throw new RangeError("no"); }, configurable: true
        });
        var caught = 0;
        for (var i = 0; i < 50000; i++) {
            try { o.boom; } catch (e) { if (e instanceof RangeError) caught++; }
        }
        console.log(caught);
        "#,
    );
    assert_eq!(out, vec!["50000"]);
}

/// A getter that MUTATES the receiver's shape (adds a key on first call): the
/// add bumps the receiver's version, so the way filled BEFORE the mutation
/// must miss and refill — and keep serving the accessor afterwards.
#[test]
fn getter_that_mutates_the_receivers_shape() {
    let out = run_ok(
        r#""use strict";
        var o = { count: 0 };
        Object.defineProperty(o, "v", {
            get: function () {
                this["extra_" + (this.count & 7)] = 1; // shape churn while cached
                this.count++;
                return 2;
            },
            configurable: true
        });
        var s = 0;
        for (var i = 0; i < 50000; i++) s = (s + o.v) | 0;
        console.log(s + ":" + o.count);
        "#,
    );
    assert_eq!(out, vec!["100000:50000"]);
}

/// defineProperty of a getter on a FROZEN object throws TypeError (the object
/// is non-extensible), and the existing accessor on a frozen receiver keeps
/// serving reads: freeze bumps the version once, the way refills, reads
/// continue; strict stores through the getter-only accessor throw.
#[test]
fn frozen_objects_and_accessors() {
    let out = run_ok(
        r#""use strict";
        var o = { hidden: 9 };
        Object.defineProperty(o, "v", {
            get: function () { return this.hidden; }, configurable: false
        });
        Object.freeze(o);
        var addFailed = false;
        try {
            Object.defineProperty(o, "late", { get: function () { return 1; } });
        } catch (e) { addFailed = e instanceof TypeError; }
        var s = 0, threw = 0;
        for (var i = 0; i < 50000; i++) {
            s = (s + o.v) | 0;
            try { o.v = 5; } catch (e) { threw++; }
        }
        console.log(addFailed + ":" + s + ":" + threw);
        "#,
    );
    assert_eq!(out, vec!["true:450000:50000"]);
}

/// The polymorphic-objects shape: one site cycling 4 data layouts + a chain
/// receiver + 2 accessor receivers, read AND written hot. Six ways of three
/// kinds coexist at one site.
#[test]
fn polymorphic_site_with_data_chain_and_accessor_receivers() {
    let out = run_ok(
        r#""use strict";
        function mkAcc(seed) {
            var o = { hidden: seed };
            Object.defineProperty(o, "val", {
                get: function () { return this.hidden; },
                set: function (x) { this.hidden = x | 0; },
                enumerable: true, configurable: true
            });
            return o;
        }
        var shapes = [
            { val: 11, a: 1 },
            { a: 1, val: 22 },
            { a: 1, b: 2, val: 33 },
            { x: 9, val: 44, y: 8 },
            (function () { var o = Object.create({ val: 55 }); o.own = 1; return o; })(),
            mkAcc(66)
        ];
        var s = 0;
        for (var i = 0; i < 300000; i++) s = (s + shapes[i % 6].val) | 0;
        for (var i = 0; i < 120000; i++) {
            var o = shapes[i % 6];
            o.val = (i & 63) + 1;
            s = (s + o.val) | 0;
        }
        console.log(s + ":" + shapes[5].val + ":" + shapes[4].val);
        "#,
    );
    assert_eq!(out, vec!["15450000:64:63"]);
}

/// THE staleness hazard the baked fn guard exists for: `__defineGetter__` on
/// an EXISTING accessor swaps `vals[slot]` with NO version bump
/// (`define_object_accessor` merges in place). The way still hits — identity
/// and version match — so only the live-fn re-read can see the swap. The new
/// getter must be observed on the very next access.
#[test]
fn getter_swapped_in_place_mid_loop_no_version_bump() {
    let out = run_ok(
        r#""use strict";
        var o = { hidden: 1 };
        Object.defineProperty(o, "v", {
            get: function () { return this.hidden; },
            enumerable: true, configurable: true
        });
        var s = 0;
        for (var i = 0; i < 200000; i++) {
            if (i === 100000) o.__defineGetter__("v", function () { return 100; });
            s = (s + o.v) | 0;
        }
        console.log(s);
        "#,
    );
    assert_eq!(out, vec!["10100000"]);
}

/// The setter twin: `__defineSetter__` swaps `attrs[slot].setter` in place
/// (same no-bump merge path). Every store after the swap must run the NEW
/// setter.
#[test]
fn setter_swapped_in_place_mid_loop_no_version_bump() {
    let out = run_ok(
        r#""use strict";
        var o = { a: 0, b: 0, hidden: 0 };
        Object.defineProperty(o, "v", {
            get: function () { return this.hidden; },
            set: function (x) { this.a = (this.a + x) | 0; },
            enumerable: true, configurable: true
        });
        for (var i = 0; i < 200000; i++) {
            if (i === 100000) o.__defineSetter__("v", function (x) { this.b = (this.b + x) | 0; });
            o.v = 1;
        }
        console.log(o.a + ":" + o.b);
        "#,
    );
    assert_eq!(out, vec!["100000:100000"]);
}

/// An ARROW getter binds `this` lexically (its defining scope), not to the
/// receiver. Arrow accessors are never baked (lexical_this excluded at fill),
/// and the slow-path continuation deopts them to the interpreter — the
/// captured `this` must win in every tier, never the receiver.
#[test]
fn arrow_getter_keeps_lexical_this() {
    let out = run_ok(
        r#""use strict";
        var factory = {
            name: "factory",
            build: function () {
                var o = { name: "receiver", hidden: 1 };
                Object.defineProperty(o, "v", {
                    get: () => this.name, // lexical: factory, NOT o
                    configurable: true
                });
                return o;
            }
        };
        var o = factory.build();
        var last = "";
        for (var i = 0; i < 50000; i++) last = o.v;
        console.log(last);
        "#,
    );
    assert_eq!(out, vec!["factory"]);
}

/// A getter-only accessor WRITTEN in sloppy mode is a silent no-op (strict
/// throws — covered by the frozen test); the getter keeps serving. The Set
/// accessor way must dispatch the MISSING setter correctly (undefined setter
/// is never baked; the slow path applies the no-op/throw semantics).
#[test]
fn getter_only_accessor_sloppy_write_is_a_noop() {
    let out = run_ok(
        r#"
        var o = { hidden: 6 };
        Object.defineProperty(o, "v", {
            get: function () { return this.hidden; }, configurable: true
        });
        var s = 0;
        for (var i = 0; i < 100000; i++) {
            o.v = 42; // sloppy: ignored
            s = (s + o.v) | 0;
        }
        console.log(s + ":" + o.hidden);
        "#,
    );
    assert_eq!(out, vec!["600000:6"]);
}

/// setPrototypeOf under a filled CHAIN accessor way: replacing the proto bumps
/// the receiver's version, so the way dies and the new chain's accessor is
/// found (different getter, different result) from the next access on.
#[test]
fn set_prototype_of_invalidates_a_chain_accessor_way() {
    let out = run_ok(
        r#""use strict";
        var protoA = {};
        Object.defineProperty(protoA, "tag", {
            get: function () { return 1; }, configurable: true
        });
        var protoB = {};
        Object.defineProperty(protoB, "tag", {
            get: function () { return 1000; }, configurable: true
        });
        var o = Object.create(protoA);
        var s = 0;
        for (var i = 0; i < 200000; i++) {
            if (i === 100000) Object.setPrototypeOf(o, protoB);
            s = (s + o.tag) | 0;
        }
        console.log(s);
        "#,
    );
    assert_eq!(out, vec!["100100000"]);
}
