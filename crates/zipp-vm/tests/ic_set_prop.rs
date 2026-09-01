//! Focused interpreter-IC regressions for named property stores.
//!
//! Each helper stays below the function/loop JIT thresholds, so its repeated
//! `o.x = v` instruction fills and exercises the interpreter SetProp IC.

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
fn set_prop_ic_revalidates_unsafe_first_way_cases() {
    let output = run_ok(
        r#"
        function putData(o, v) { o.x = v; }
        let data = { x: 0 };
        putData(data, 1);
        putData(data, 2);
        putData(data, 3);
        console.log("data", data.x);

        function putSloppyReadonly(o, v) { o.x = v; }
        let sloppyReadonly = { x: 0 };
        putSloppyReadonly(sloppyReadonly, 1);
        putSloppyReadonly(sloppyReadonly, 2);
        Object.defineProperty(
            sloppyReadonly,
            "x",
            { value: 4, writable: false, configurable: true }
        );
        putSloppyReadonly(sloppyReadonly, 9);
        console.log("sloppy-readonly", sloppyReadonly.x);

        function putStrictReadonly(o, v) { "use strict"; o.x = v; }
        let strictReadonly = { x: 0 };
        putStrictReadonly(strictReadonly, 1);
        putStrictReadonly(strictReadonly, 2);
        Object.defineProperty(
            strictReadonly,
            "x",
            { value: 5, writable: false, configurable: true }
        );
        let strictReadonlyError = "";
        try {
            putStrictReadonly(strictReadonly, 9);
        } catch (e) {
            strictReadonlyError = e.name;
        }
        console.log("strict-readonly", strictReadonly.x, strictReadonlyError);

        function putSetter(o, v) { o.x = v; }
        let accessor = { x: 0 };
        putSetter(accessor, 1);
        putSetter(accessor, 2);
        let setterSeen = 0;
        Object.defineProperty(accessor, "x", {
            get: function() { return setterSeen; },
            set: function(v) { setterSeen = v * 2; },
            configurable: true
        });
        putSetter(accessor, 6);
        console.log("setter", accessor.x, setterSeen);

        function putSloppyGetterOnly(o, v) { o.x = v; }
        let sloppyGetterOnly = { x: 0 };
        putSloppyGetterOnly(sloppyGetterOnly, 1);
        putSloppyGetterOnly(sloppyGetterOnly, 2);
        Object.defineProperty(sloppyGetterOnly, "x", {
            get: function() { return 7; },
            configurable: true
        });
        putSloppyGetterOnly(sloppyGetterOnly, 9);
        console.log("sloppy-getter", sloppyGetterOnly.x);

        function putStrictGetterOnly(o, v) { "use strict"; o.x = v; }
        let strictGetterOnly = { x: 0 };
        putStrictGetterOnly(strictGetterOnly, 1);
        putStrictGetterOnly(strictGetterOnly, 2);
        Object.defineProperty(strictGetterOnly, "x", {
            get: function() { return 8; },
            configurable: true
        });
        let strictGetterError = "";
        try {
            putStrictGetterOnly(strictGetterOnly, 9);
        } catch (e) {
            strictGetterError = e.name;
        }
        console.log("strict-getter", strictGetterOnly.x, strictGetterError);

        function putMoved(o, v) { o.x = v; }
        let moved = { x: 0, y: 1 };
        putMoved(moved, 2);
        putMoved(moved, 3);
        delete moved.x;
        moved.z = 4;
        moved.x = 5;
        putMoved(moved, 9);
        console.log("moved", moved.x, moved.y, moved.z);

        function putProto(o, v) { o.x = v; }
        let protoReceiver = { x: 0 };
        putProto(protoReceiver, 1);
        putProto(protoReceiver, 2);
        delete protoReceiver.x;
        let protoSeen = 0;
        let proto = {};
        Object.defineProperty(proto, "x", {
            set: function(v) { protoSeen = v; },
            configurable: true
        });
        Object.setPrototypeOf(protoReceiver, proto);
        putProto(protoReceiver, 11);
        console.log(
            "proto",
            protoSeen,
            Object.prototype.hasOwnProperty.call(protoReceiver, "x")
        );
        "#,
    );

    assert_eq!(
        output,
        [
            "data 3",
            "sloppy-readonly 4",
            "strict-readonly 5 TypeError",
            "setter 12 12",
            "sloppy-getter 7",
            "strict-getter 8 TypeError",
            "moved 9 1 4",
            "proto 11 false",
        ]
    );
}

#[test]
fn set_prop_ic_falls_back_for_proxy_and_exotic_receivers() {
    let output = run_ok(
        r#"
        function putProxy(o, v) { o.x = v; }
        let warmProxySite = { x: 0 };
        putProxy(warmProxySite, 1);
        putProxy(warmProxySite, 2);
        let target = { x: 0 };
        let trapCalls = 0;
        let proxy = new Proxy(target, {
            set: function(t, k, v) {
                trapCalls++;
                t[k] = v + 1;
                return true;
            }
        });
        putProxy(proxy, 7);
        console.log("proxy", trapCalls, target.x);

        function putArray(o, v) { o.x = v; }
        let warmArraySite = { x: 0 };
        putArray(warmArraySite, 1);
        putArray(warmArraySite, 2);
        let array = [];
        putArray(array, 13);
        console.log("array", array.x, array.length);
        "#,
    );

    assert_eq!(output, ["proxy 1 8", "array 13 0"]);
}

#[test]
fn adjacent_set_then_get_forwards_only_the_same_plain_own_data_property() {
    let output = run_ok(
        r#"
        function same(o, v) { o.x = v; return o.x; }
        let plain = { x: 0 };
        console.log("plain", same(plain, 3), same(plain, 4), plain.x);

        let readonly = {};
        Object.defineProperty(readonly, "x", {
            value: 5, writable: false, configurable: true
        });
        console.log("readonly", same(readonly, 9), readonly.x);

        let backing = 0;
        let accessor = {};
        Object.defineProperty(accessor, "x", {
            get: function() { return backing + 1; },
            set: function(v) { backing = v * 2; },
            configurable: true
        });
        console.log("accessor", same(accessor, 6), backing);

        let setCalls = 0;
        let getCalls = 0;
        let target = { x: 0 };
        let proxy = new Proxy(target, {
            set: function(t, k, v) { setCalls++; t[k] = v + 2; return true; },
            get: function(t, k) { getCalls++; return t[k] + 3; }
        });
        console.log("proxy", same(proxy, 7), setCalls, getCalls, target.x);

        function differentKey(o, v) { o.x = v; return o.y; }
        let keys = { x: 0, y: 12 };
        console.log("key", differentKey(keys, 8), keys.x);

        function differentReceiver(a, b, v) { a.x = v; return b.x; }
        let left = { x: 0 };
        let right = { x: 14 };
        console.log("receiver", differentReceiver(left, right, 10), left.x);
        "#,
    );

    assert_eq!(
        output,
        [
            "plain 3 4 4",
            "readonly 5 5",
            "accessor 13 12",
            "proxy 12 1 1 9",
            "key 12 8",
            "receiver 14 10",
        ]
    );
}
