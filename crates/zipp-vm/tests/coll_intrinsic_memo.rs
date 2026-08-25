//! B183: the memoized Map/Set intrinsic proof must observe every tamper the
//! full per-call proof observed — including the in-place prototype-slot
//! overwrite that bumps NO object version (the fn-bits guard's whole reason).

fn run(source: &str) -> Vec<String> {
    let result = zipp_vm::run(source).expect("source compiles");
    assert!(
        result.error.is_none(),
        "unexpected uncaught error: {:?}",
        result.error
    );
    result.output
}

/// Warm the native lane, then overwrite `Map.prototype.get` IN PLACE (same
/// slot, no version bump) and confirm the override is observed immediately.
#[test]
fn in_place_prototype_overwrite_is_observed_after_warm() {
    let out = run(r#"
        var m = new Map(); m.set(1, "real");
        var warm = 0;
        for (var i = 0; i < 20000; i++) { if (m.get(1) === "real") warm++; }
        Map.prototype.get = function () { return "hijacked"; };
        var after = m.get(1);
        Map.prototype.get = Map.prototype.get; // keep shape stable
        console.log(warm + ":" + after);
    "#);
    assert_eq!(out, ["20000:hijacked"]);
}

/// Same for the mutate family: overwrite `Set.prototype.add` in place after
/// the region lane warmed on it.
#[test]
fn in_place_set_add_overwrite_is_observed_after_warm() {
    let out = run(r#"
        var s = new Set(); var n = 0;
        for (var i = 0; i < 20000; i++) { s.add(i); }
        Set.prototype.add = function () { n = 777; return this; };
        s.add(99999999);
        console.log(s.size + ":" + n + ":" + s.has(99999999));
    "#);
    assert_eq!(out, ["20000:777:false"]);
}

/// An accessor redefinition (bumps the version) and an own shadow must both
/// divert too — the memo's other two guard directions.
#[test]
fn accessor_redefinition_and_own_shadow_divert() {
    let out = run(r#"
        var m = new Map(); m.set(1, 2);
        for (var i = 0; i < 20000; i++) m.has(1);
        Object.defineProperty(Map.prototype, "has", {
            get: function () { return function () { return "acc"; }; },
            configurable: true,
        });
        var viaAccessor = m.has(1);
        Object.defineProperty(Map.prototype, "has", {
            value: Map.prototype.get, writable: true, configurable: true,
        });
        var m2 = new Map();
        m2.has = function () { return "own"; };
        console.log(viaAccessor + ":" + m2.has(1));
    "#);
    assert_eq!(out, ["acc:own"]);
}
