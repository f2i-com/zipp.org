fn run_fixture() {
    let out = zipp_vm::run(include_str!("fixtures/builtin_subclass_payloads.js"))
        .expect("fixture compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    let expected: Vec<&str> = include_str!("fixtures/builtin_subclass_payloads.txt").lines().collect();
    assert_eq!(out.output, expected);
    #[cfg(feature = "wasm-lite")]
    {
        let out = zipp_vm::run(include_str!("fixtures/builtin_subclass_identity.js"))
            .expect("identity fixture compiles");
        assert!(out.error.is_none(), "{:?}", out.error);
        let expected: Vec<&str> = include_str!("fixtures/builtin_subclass_identity.txt").lines().collect();
        assert_eq!(out.output, expected);
    }
}

#[test]
fn subclasses_and_user_constructor_identity_match_node() {
    run_fixture();
}

#[test]
fn subclass_copy_gc_stress_child() {
    if std::env::var_os("ZIPP_SUBCLASS_COPY_GC_CHILD").is_some() {
        run_fixture();
    }
}

#[test]
fn subclasses_keep_their_payloads_under_gc_stress() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "subclass_copy_gc_stress_child", "--nocapture"])
        .env("ZIPP_SUBCLASS_COPY_GC_CHILD", "1")
        .env("ZIPP_GC_STRESS", "1")
        .output().expect("run GC stress fixture");
    assert!(result.status.success(), "{}\n{}",
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
}

#[test]
fn foreign_intrinsic_constructors_keep_subclass_prototypes() {
    let out = zipp_vm::run(r#"
        const g = $262.createRealm().global;
        class A extends g.Array {}
        class T extends g.Uint8Array {}
        class D extends g.DataView {}
        const a = new A(1, 2, 3);
        const t = new T([4, 5]);
        const d = new D(t.buffer);
        console.log(a instanceof A, a instanceof g.Array, a.join(','));
        console.log(t instanceof T, t instanceof g.Uint8Array, t.join(','));
        console.log(d instanceof D, d instanceof g.DataView, d.getUint8(1));
    "#).expect("fixture compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, ["true true 1,2,3", "true true 4,5", "true true 5"]);
}

#[cfg(not(feature = "wasm-lite"))]
#[test]
fn full_profile_intrinsic_payloads() {
    let out = zipp_vm::run(r#"
        class N extends Intl.NumberFormat {}
        const n = new N('en', {minimumFractionDigits: 2});
        console.log(n instanceof N, n.format(1.2));
        class D extends Temporal.Duration {}
        const d = new D(1, 2);
        console.log(d instanceof D, d.years, d.months);
        class P extends Temporal.PlainDate {}
        const p = new P(2025, 3, 8);
        console.log(p instanceof P, p.year, p.month, p.day);
    "#).expect("fixture compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, ["true 1.20", "true 1 2", "true 2025 3 8"]);
}
