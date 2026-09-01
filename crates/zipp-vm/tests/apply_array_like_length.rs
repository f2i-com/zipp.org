//! CreateListFromArrayLike must honour the JS `length` of a real Array, not
//! the size of its dense storage.
//!
//! Two shapes used to diverge from the spec (and from node):
//!
//! * an array whose length lives past the dense storage — `new Array(n)` past
//!   the dense cap, or `a.length = n` — passed ZERO arguments through
//!   `Function.prototype.apply` and `Reflect.apply`;
//! * a hole inside the dense storage was copied verbatim, so the callee's
//!   `arguments` had an ABSENT index where Get yields `undefined` (or an
//!   inherited element from `Array.prototype`).
//!
//! Spread (`f(...a)`) already went through the iterator protocol and was right;
//! it is pinned here so the three call forms stay in agreement.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

const PROBE: &str = r#"
    function f() {
        return arguments.length + ":" +
            Array.prototype.map.call(arguments, function (x) { return String(x); }).join(",");
    }
"#;

#[test]
fn holes_inside_dense_storage_arrive_as_undefined() {
    let src = format!(
        r#"{PROBE}
        console.log(f.apply(null, [1, , 3]));
        console.log(Reflect.apply(f, null, [1, , 3]));
        console.log(f(...[1, , 3]));
        var a = [1, 2]; a[5] = 3;
        console.log(f.apply(null, a));
        console.log(Reflect.apply(f, null, a));
        console.log(f(...a));
        "#
    );
    assert_eq!(
        run_ok(&src),
        [
            "3:1,undefined,3",
            "3:1,undefined,3",
            "3:1,undefined,3",
            "6:1,2,undefined,undefined,undefined,3",
            "6:1,2,undefined,undefined,undefined,3",
            "6:1,2,undefined,undefined,undefined,3",
        ]
    );
}

#[test]
fn holes_read_through_the_prototype_like_get() {
    let src = format!(
        r#"{PROBE}
        Array.prototype[1] = "proto";
        console.log(f.apply(null, [1, , 3]));
        console.log(Reflect.apply(f, null, [1, , 3]));
        delete Array.prototype[1];
        "#
    );
    assert_eq!(run_ok(&src), ["3:1,proto,3", "3:1,proto,3"]);
}

#[test]
fn a_length_past_the_dense_storage_is_the_argument_count() {
    // `a.length = n` records a virtual length without materialising holes;
    // the argument list is still `n` long.
    let src = format!(
        r#"{PROBE}
        var b = [1, 2, 3]; b.length = 6;
        console.log(f.apply(null, b));
        console.log(Reflect.apply(f, null, b));
        console.log(f(...b));
        "#
    );
    assert_eq!(
        run_ok(&src),
        [
            "6:1,2,3,undefined,undefined,undefined",
            "6:1,2,3,undefined,undefined,undefined",
            "6:1,2,3,undefined,undefined,undefined",
        ]
    );
}

#[test]
fn an_array_past_the_eager_materialisation_cap_passes_every_element() {
    // Above the dense cap `new Array(n)` is a virtual-length array with no
    // dense elements at all; apply used to see an empty list.
    let n = 1_300_000u32;
    let src = format!(
        r#"
        function count() {{ return arguments.length; }}
        function last() {{ return String(arguments[arguments.length - 1]); }}
        var a = new Array({n});
        console.log(count.apply(null, a), Reflect.apply(count, null, a));
        console.log(last.apply(null, a));
        a[{n} - 1] = "tail";
        console.log(last.apply(null, a), Reflect.apply(last, null, a));
        "#
    );
    assert_eq!(
        run_ok(&src),
        [format!("{n} {n}"), "undefined".to_string(), "tail tail".to_string()]
    );
}
