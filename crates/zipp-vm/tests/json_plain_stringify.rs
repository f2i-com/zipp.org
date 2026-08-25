//! Correctness boundary for the compact plain-data `JSON.stringify` walk.
//!
//! The fast path is allowed only for a closed graph of ordinary default-proto
//! objects, dense Arrays, and JSON primitive leaves. It writes into a private
//! Rust buffer and publishes only after the complete graph is admitted. Every
//! observable/exotic shape below must therefore decline and run the ordinary
//! serializer from the beginning.

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
fn plain_dense_aliases_strings_and_hidden_slots_are_exact() {
    let out = run_ok(
        r#"
        "use strict";
        var shared = {esc:"a\"b\\c\n", astral:"😀", lone:"\ud800", n:1e21};
        console.log(JSON.stringify({
          a:[1,-0,NaN,true,false,null,shared], b:shared, z:{}
        }));

        // A detached function value reaches the native call arm rather than
        // the direct-call bytecode op; both are intentionally accelerated.
        var stringify = JSON.stringify;
        console.log(stringify({a:[1,"x"],b:{c:2}}));

        // A non-enumerable accessor is never read. It is safe for the direct
        // map walk to skip the slot without declining.
        var effects = 0, hidden = {a:1};
        Object.defineProperty(hidden, "x", {
          enumerable:false,
          get:function () { effects++; return 9; }
        });
        console.log(JSON.stringify(hidden) + ":" + effects);
        "#,
    );
    assert_eq!(
        out,
        [
            r#"{"a":[1,0,null,true,false,null,{"esc":"a\"b\\c\n","astral":"😀","lone":"\ud800","n":1e+21}],"b":{"esc":"a\"b\\c\n","astral":"😀","lone":"\ud800","n":1e+21},"z":{}}"#,
            r#"{"a":[1,"x"],"b":{"c":2}}"#,
            r#"{"a":1}:0"#,
        ]
    );
}

#[test]
fn getters_tojson_prototypes_sparse_indices_and_proxies_decline() {
    let out = run_ok(
        r#"
        "use strict";
        var effects = 0, own = {b:2};
        Object.defineProperty(own, "a", {
          enumerable:true,
          get:function () { effects++; return 1; }
        });
        console.log("accessor=" + JSON.stringify(own) + ":" + effects);

        effects = 0;
        var tj = {a:{x:1}, b:2};
        tj.a.toJSON = function (k) { effects++; tj.b = 7; return k + ":ok"; };
        console.log("own-tojson=" + JSON.stringify(tj) + ":" + effects);

        effects = 0;
        Object.prototype.toJSON = function (k) {
          effects++;
          return "proto:" + k;
        };
        console.log("default-proto=" + JSON.stringify({a:1}) + ":" + effects);
        delete Object.prototype.toJSON;

        effects = 0;
        var customProto = {};
        Object.defineProperty(customProto, "toJSON", {
          get:function () {
            effects++;
            return function () { return "custom"; };
          }
        });
        var custom = Object.create(customProto); custom.a = 1;
        console.log("custom-proto=" + JSON.stringify(custom) + ":" + effects);

        // The engine stores the intrinsic as an Object map even though
        // IsArray(%Array.prototype%) is true. It must take the general Array
        // serializer both at the root and when nested.
        console.log("array-proto=" + JSON.stringify(Array.prototype));
        console.log("nested-array-proto=" + JSON.stringify({p:Array.prototype}));

        Array.prototype[1] = 9;
        console.log("hole=" + JSON.stringify([1,,3]));
        delete Array.prototype[1];
        // Deleting the element does not shrink the intrinsic's Array length.
        console.log("indexed-array-proto=" + JSON.stringify(Array.prototype));
        Array.prototype.length = 0;

        // Named properties are ignored by SerializeJSONArray at length zero.
        Array.prototype.named = 7;
        console.log("named-array-proto=" + JSON.stringify(Array.prototype));
        console.log("nested-named-array-proto=" + JSON.stringify({p:Array.prototype}));
        delete Array.prototype.named;

        console.log("omit=" + JSON.stringify({
          u:undefined, f:function () {}, s:Symbol("s"),
          a:[undefined,function () {},Symbol("s"),2]
        }));
        console.log("indices=" + JSON.stringify({
          "2":"two", "1":"one", "01":"oh", x:3, "0":"zero"
        }));

        var ownKeys=0, descs=0, gets=0;
        var proxy = new Proxy({a:1,b:2}, {
          ownKeys:function (t) { ownKeys++; return Reflect.ownKeys(t); },
          getOwnPropertyDescriptor:function (t,k) {
            descs++; return Object.getOwnPropertyDescriptor(t,k);
          },
          get:function (t,k) { gets++; return t[k]; }
        });
        console.log("proxy=" + JSON.stringify(proxy) + ":" +
                    [ownKeys,descs,gets].join(","));

        var np = Object.create(null); np.a = 1;
        console.log("null-proto=" + JSON.stringify(np));

        "#,
    );
    assert_eq!(
        out,
        [
            r#"accessor={"b":2,"a":1}:1"#,
            r#"own-tojson={"a":"a:ok","b":7}:1"#,
            r#"default-proto="proto:":1"#,
            r#"custom-proto="custom":1"#,
            "array-proto=[]",
            r#"nested-array-proto={"p":[]}"#,
            r#"hole=[1,9,3]"#,
            "indexed-array-proto=[null,null]",
            "named-array-proto=[]",
            r#"nested-named-array-proto={"p":[]}"#,
            r#"omit={"a":[null,null,null,2]}"#,
            r#"indices={"0":"zero","1":"one","2":"two","01":"oh","x":3}"#,
            r#"proxy={"a":1,"b":2}:1,2,3"#,
            r#"null-proto={"a":1}"#,
        ]
    );
}

#[test]
fn errors_options_wrappers_and_depth_keep_the_general_semantics() {
    let mut out = run_ok(
        r#"
        "use strict";
        function caught(label, thunk) {
          try { console.log(label + "=" + thunk()); }
          catch (e) { console.log(label + "=throw:" + e.name); }
        }
        var cycle = {}; cycle.self = cycle;
        caught("cycle", function () { return JSON.stringify(cycle); });
        caught("bigint", function () { return JSON.stringify({x:1n}); });

        var stringify = JSON.stringify;
        console.log("replacer=" + stringify(
          {a:1,b:2},
          function (k,v) { return typeof v === "number" ? v+1 : v; }
        ));
        console.log("indent=" + JSON.stringify(
          stringify({a:[1,{b:2}]}, null, 2)
        ));
        console.log("boxed=" + JSON.stringify([
          new Number(2), new String("x"), new Boolean(false)
        ]));

        "#,
    );
    #[cfg(not(feature = "safe-sandbox"))]
    out.extend(run_ok(
        r#"
        // The native walk caps its own recursion at 256, then discards the
        // private prefix. The ordinary serializer must still complete this in
        // the compatibility profile.
        var deep = 1;
        for (var i=0; i<300; i++) deep=[deep];
        var s = JSON.stringify(deep);
        console.log("deep=" + s.length + ":" + s.slice(0,3) + ":" + s.slice(-3));
        "#,
    ));
    #[cfg(feature = "safe-sandbox")]
    out.extend(run_ok(
        r#"
        var deep = 1;
        for (var i=0; i<300; i++) deep=[deep];
        try { JSON.stringify(deep); }
        catch (e) { console.log("deep=throw:" + e.name); }
        "#,
    ));

    let mut expected = vec![
        "cycle=throw:TypeError",
        "bigint=throw:TypeError",
        r#"replacer={"a":2,"b":3}"#,
        r#"indent="{\n  \"a\": [\n    1,\n    {\n      \"b\": 2\n    }\n  ]\n}""#,
        r#"boxed=[2,"x",false]"#,
    ];
    #[cfg(not(feature = "safe-sandbox"))]
    expected.push("deep=601:[[[:]]]");
    #[cfg(feature = "safe-sandbox")]
    expected.push("deep=throw:RangeError");
    assert_eq!(out, expected);
}

/// Each implementation switch is process-cached. Fresh child processes prove
/// that the old serializer and GC-at-every-safe-point mode produce the same
/// focused corpus, rather than merely exercising an already-cached default.
#[test]
fn zz_off_switch_and_gc_stress_agree() {
    if std::env::var_os("ZIPP_JSON_PLAIN_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (label, key) in [("off", "ZIPP_NO_JSON_PLAIN_FAST"), ("gc", "ZIPP_GC_STRESS")] {
        let out = std::process::Command::new(&exe)
            .args(["--skip", "zz_off_switch_and_gc_stress_agree"])
            .env("ZIPP_JSON_PLAIN_CHILD", "1")
            .env(key, "1")
            .output()
            .expect("re-run focused test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && !stdout.contains(" 0 passed"),
            "{label} child diverged:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
