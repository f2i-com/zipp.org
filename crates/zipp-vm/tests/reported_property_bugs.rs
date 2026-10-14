//! Property-model bugs reported against `main` at 828038b6 (f2i-com/zipp.org
//! #32, #33, #34), and the arguments-object `length` bug found beside them.
//! Every expected line is what Node 24 prints for the same source. The hot
//! loops make the compiled paths answer too.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// #32: an integer-indexed exotic object owns its in-range indices.
#[test]
fn typed_array_owns_its_in_range_indices() {
    let out = run_ok(
        r#"
        console.log(new Uint8Array(2).hasOwnProperty(0));
        var t = new Float64Array(3);
        console.log([t.hasOwnProperty(2), t.hasOwnProperty(3), t.hasOwnProperty("1"),
            t.hasOwnProperty("-0"), t.hasOwnProperty("1.5"), t.hasOwnProperty(-1),
            Object.prototype.hasOwnProperty.call(t, 0), Object.hasOwn(t, 1),
            Object.hasOwn(t, 5)].join());
        var s = new Uint16Array(8).subarray(2, 5); s.foo = 1;
        console.log([s.hasOwnProperty(2), s.hasOwnProperty(3), s.hasOwnProperty("foo")].join());
        var n = 0, h = new Int32Array(4);
        for (var i = 0; i < 3000; i++) if (h.hasOwnProperty(i % 6)) n++;
        console.log(n);
        "#,
    );
    assert_eq!(
        out,
        [
            "true",
            "true,false,true,false,false,false,true,true,false",
            "true,false,true",
            "2000",
        ]
    );
}

/// #33: a function's `prototype` made non-writable stays non-writable.
#[test]
fn function_prototype_made_non_writable() {
    let out = run_ok(
        r#"
        function f() {}
        Object.defineProperty(f, "prototype", { writable: false });
        console.log(Object.getOwnPropertyDescriptor(f, "prototype").writable);
        var p0 = f.prototype;
        f.prototype = { other: 1 };
        console.log(f.prototype === p0, new f() instanceof f);
        function g() { "use strict"; try { f.prototype = {}; return "no"; } catch (e) { return e.constructor.name; } }
        console.log(g());
        try { Object.defineProperty(f, "prototype", { writable: true }); console.log("no"); } catch (e) { console.log(e.constructor.name); }
        try { Object.defineProperty(f, "prototype", { value: {} }); console.log("no"); } catch (e) { console.log(e.constructor.name); }
        Object.defineProperty(f, "prototype", { value: p0 });
        function k() {}
        Object.freeze(k);
        console.log(Object.getOwnPropertyDescriptor(k, "prototype").writable, Object.isFrozen(k));
        var d = Object.getOwnPropertyDescriptor(function () {}, "prototype");
        console.log(d.writable, d.enumerable, d.configurable);
        var hits = 0;
        for (var i = 0; i < 3000; i++) { f.prototype = {}; if (f.prototype === p0) hits++; }
        console.log(hits);
        "#,
    );
    assert_eq!(
        out,
        [
            "false",
            "true true",
            "TypeError",
            "TypeError",
            "TypeError",
            "false true",
            "true false false",
            "3000",
        ]
    );
}

/// #34: `recv.hasOwnProperty(k)` (and its two siblings) is called as the
/// receiver resolves it: a Proxy's traps run, and an own, inherited, class
/// or replaced method of that name wins.
#[test]
fn object_prototype_methods_resolve_through_the_receiver() {
    let out = run_ok(
        r#"
        let called = false;
        const p = new Proxy({}, {
          getOwnPropertyDescriptor(t, k) {
            called = true;
            return { value: 1, writable: true, enumerable: true, configurable: true };
          },
        });
        console.log(p.hasOwnProperty("x"), called);
        var gets = [];
        var q = new Proxy({}, { get(t, k, r) { gets.push(String(k)); return Reflect.get(t, k, r); },
                                getOwnPropertyDescriptor() { return undefined; } });
        console.log(q.hasOwnProperty("x"), gets.join());
        console.log(({ hasOwnProperty() { return "own"; } }).hasOwnProperty("x"));
        console.log(Object.create({ hasOwnProperty(k) { return "proto:" + k; } }).hasOwnProperty("y"));
        class C { hasOwnProperty(k) { return "class:" + k; } }
        console.log(new C().hasOwnProperty("z"));
        var saved = Object.prototype.hasOwnProperty;
        Object.prototype.hasOwnProperty = function (k) { return "replaced:" + k; };
        console.log(({ a: 1 }).hasOwnProperty("a"));
        Object.prototype.hasOwnProperty = saved;
        var nul = Object.create(null); nul.a = 1;
        try { nul.hasOwnProperty("a"); console.log("no"); } catch (e) { console.log(e.constructor.name); }
        console.log(({ isPrototypeOf() { return "own"; } }).isPrototypeOf({}),
                    ({ propertyIsEnumerable() { return "own"; } }).propertyIsEnumerable("x"));
        var calls = 0;
        var r = new Proxy({ a: 1 }, { getOwnPropertyDescriptor(t, k) { calls++; return Reflect.getOwnPropertyDescriptor(t, k); } });
        var n = 0;
        for (var i = 0; i < 3000; i++) if (r.hasOwnProperty(i % 2 ? "a" : "b")) n++;
        console.log(n, calls);
        class D { constructor() { this.x = 1; } }
        var dd = new D(), m = 0;
        for (var i = 0; i < 3000; i++) if (dd.hasOwnProperty("x")) m++;
        console.log(m);
        "#,
    );
    assert_eq!(
        out,
        [
            "true true",
            "false hasOwnProperty",
            "own",
            "proto:y",
            "class:z",
            "replaced:a",
            "TypeError",
            "own own",
            "1500 3000",
            "3000",
        ]
    );
}

/// An arguments object's `length` is an ordinary data property: making it
/// smaller deletes no element, and an iteration follows ToLength of it.
#[test]
fn arguments_length_is_an_ordinary_property() {
    let out = run_ok(
        r#"
        function a(v) {
          var f = function () {
            arguments.length = v;
            return String(arguments.length) + ":" + [...arguments].join(".") + ":" +
              Object.keys(arguments).join(".") + ":" + arguments[1];
          };
          return f(4, 5, 6);
        }
        console.log(a(1));
        console.log(a("2"));
        console.log(a(-1));
        console.log(a(1.5));
        function b() { arguments.length = 1; arguments.length = 3; return [...arguments].join() + "|" + JSON.stringify(arguments); }
        console.log(b(4, 5, 6));
        "#,
    );
    assert_eq!(
        out,
        [
            "1:4:0.1.2:5",
            "2:4.5:0.1.2:5",
            "-1::0.1.2:5",
            "1.5:4:0.1.2:5",
            "4,5,6|{\"0\":4,\"1\":5,\"2\":6}",
        ]
    );
}
