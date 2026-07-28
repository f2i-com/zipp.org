//! Focused correctness coverage for pinned-TypedArray snapshots and fallbacks.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

#[test]
fn pinned_typedarray_snapshot_safety_covers_detach_resize_and_shared_growth() {
    let output = run_ok(
        r#"
        function sumDetach(t, n) {
            let s = 0;
            for (let r = 0; r < n; r++) {
                let k = r & 7;
                t[k] = t[k] + 1;
                s = (s + t[k]) | 0;
            }
            return s;
        }
        function sumResize(t, n) {
            let s = 0;
            for (let r = 0; r < n; r++) {
                let k = r & 7;
                t[k] = t[k] + 1;
                s = (s + t[k]) | 0;
            }
            return s;
        }
        function sumShared(t, n, mask) {
            let s = 0;
            for (let r = 0; r < n; r++) {
                let k = r & mask;
                t[k] = t[k] + 1;
                s = (s + t[k]) | 0;
            }
            return s;
        }

        let b1 = new ArrayBuffer(32);
        let t1 = new Int32Array(b1);
        let beforeDetach = sumDetach(t1, 400);
        b1.transfer();
        let afterDetach = sumDetach(t1, 40);

        let b2 = new ArrayBuffer(32, {maxByteLength: 64});
        let t2 = new Int32Array(b2, 0, 8);
        let beforeResize = sumResize(t2, 400);
        b2.resize(16);
        let afterResize = sumResize(t2, 40);

        let sab = new SharedArrayBuffer(16, {maxByteLength: 32});
        let ts = new Int32Array(sab);
        let beforeGrow = sumShared(ts, 400, 3);
        sab.grow(32);
        let afterGrow = sumShared(ts, 400, 7);

        console.log(
            beforeDetach,
            afterDetach,
            t1[0] === undefined,
            beforeResize,
            afterResize,
            t2.length,
            beforeGrow,
            afterGrow,
            ts.length
        );
        "#,
    );

    assert_eq!(output, ["10200 0 true 10200 0 0 20200 30200 8"]);
}

#[test]
fn pinned_typedarray_access_safety_covers_key_changes_branches_and_oob() {
    let output = run_ok(
        r#"
        function intKernel(a, n) {
            let s = 0;
            for (let i = 0; i < n; i++) {
                let k = i & 7;
                a[k] = (a[k] + 1) | 0;
                if ((i & 31) === 0) k = 99;
                else k = (k + 1) & 7;
                a[k] = 123;
                s = (s + a[i & 7]) | 0;
            }
            return s + "/" + a.join(",");
        }

        function f64Kernel(x, n) {
            let s = 0;
            for (let i = 0; i < n; i++) {
                let k = i === 500 ? 1.5 : (i === 700 ? -1 : (i & 7));
                x[k] *= 1.000001;
                s += x[i & 7];
            }
            return s.toFixed(6) + "/" +
                Array.from(x).map(v => v.toFixed(6)).join(",");
        }

        console.log(intKernel(new Int32Array(8), 2000));
        let x = new Float64Array(8);
        for (let i = 0; i < 8; i++) x[i] = i + 0.25;
        console.log(f64Kernel(x, 2000));
        "#,
    );

    assert_eq!(
        output,
        [
            "247816/123,124,124,124,124,124,124,124",
            "7500.939836/0.250063,1.250313,2.250563,3.250813,4.251054,5.251313,6.251563,7.251813",
        ]
    );
}
