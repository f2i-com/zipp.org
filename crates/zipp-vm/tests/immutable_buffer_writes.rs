//! Writes to a TypedArray over an immutable ArrayBuffer (the Immutable
//! ArrayBuffer proposal): a numeric-key [[Set]] returns false before any
//! coercion (a TypeError in strict code), the elements are non-writable,
//! non-configurable data properties, and [[DefineOwnProperty]] accepts only
//! the current value. The hot loops make the compiled paths answer too: a
//! JIT store helper and the pinned element and DataView lanes wrote the
//! buffer.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn immutable_typed_array_writes_fail() {
    let out = run_ok(
        r#"
        "use strict";
        const b = new ArrayBuffer(8).transferToImmutable();
        const u = new Uint8Array(b);
        function t(f) { try { return String(f()); } catch (e) { return e.constructor.name; } }
        let coerced = 0;
        const v = { valueOf() { coerced++; return 1; } };
        console.log(t(() => { u[0] = 1; }), t(() => { u["-0"] = 1; }), t(() => { u[9] = 1; }), u[0]);
        console.log(Reflect.set(u, 0, v), Reflect.set(u, "1", 2), coerced, u[0], u[1]);
        const d = Object.getOwnPropertyDescriptor(u, 0);
        console.log(d.value, d.writable, d.enumerable, d.configurable);
        console.log(t(() => Object.defineProperty(u, 0, { value: 0 }) === u), t(() => Object.defineProperty(u, 0, { value: 9 })),
          Reflect.defineProperty(u, 0, { value: 0, writable: false }), Reflect.defineProperty(u, 0, { writable: true }));
        function poke(a, n) { let thrown = 0; for (let i = 0; i < n; i++) { try { a[i & 7] = 7; } catch (e) { thrown++; } } return thrown; }
        let thrown = 0;
        for (let r = 0; r < 50; r++) thrown += poke(u, 1000);
        console.log(thrown, u[3]);
        const dv = new DataView(b);
        function dpoke(n) { let k = 0; for (let i = 0; i < n; i++) { try { dv.setUint8(i & 7, 5); } catch (e) { k++; } } return k; }
        let dk = 0;
        for (let r = 0; r < 50; r++) dk += dpoke(1000);
        console.log(dk, dv.getUint8(2));
        const m = new Uint8Array(4);
        function fill(a, n) { for (let i = 0; i < n; i++) a[i & 3] = i; return a[3]; }
        let last = 0;
        for (let r = 0; r < 50; r++) last = fill(m, 1000);
        console.log(last, m[0]);
        "#,
    );
    assert_eq!(
        out,
        [
            "TypeError TypeError TypeError 0",
            "false false 0 0 0",
            "0 false true false",
            "true TypeError true false",
            "50000 0",
            "50000 0",
            "231 228",
        ]
    );
}

/// Sloppy code: the failed [[Set]] is silent and the element unchanged.
#[test]
fn sloppy_immutable_write_is_silent() {
    let out = run_ok(
        r#"
        var u = new Uint8Array(new ArrayBuffer(4).transferToImmutable());
        function poke(n) { for (var i = 0; i < n; i++) u[i & 3] = 9; return u[1]; }
        var r = 0;
        for (var k = 0; k < 50; k++) r = poke(1000);
        u[2] = 5;
        console.log(r, u[2]);
        "#,
    );
    assert_eq!(out, ["0 0"]);
}
