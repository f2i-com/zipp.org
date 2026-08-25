//! Safe-profile regressions for guest-controlled Rust recursion inside one
//! object meta-operation. These probes run on the same 1 MiB stack budget as
//! the hardened Wasm worker so a missing guard fails here instead of trapping
//! the production sandbox.

#![cfg(feature = "safe-sandbox")]

fn run_on_small_stack(source: &str) -> Vec<String> {
    let source = source.to_string();
    std::thread::Builder::new()
        .name("native-meta-recursion-probe".into())
        .stack_size(1024 * 1024)
        .spawn(move || {
            let result = zipp_vm::run(&source).expect("source compiles");
            assert!(
                result.error.is_none(),
                "unexpected uncaught runtime error: {:?}",
                result.error
            );
            result.output
        })
        .expect("spawn bounded-stack VM probe")
        .join()
        .expect("meta-operation must return instead of overflowing its native stack")
}

#[test]
fn deep_object_create_has_is_catchable_and_counter_unwinds() {
    assert_eq!(
        run_on_small_stack(
            r#"
            var deep = null;
            for (var i = 0; i < 512; i++) deep = Object.create(deep);
            // Plain gets use an iterative fast walk and can safely traverse the
            // entire chain without consuming the shared Rust recursion budget.
            console.log(deep.missing === undefined ? "get-safe" : "get-wrong");
            try {
                "missing" in deep;
                console.log("has-unexpected");
            } catch (error) {
                console.log(error instanceof RangeError, String(error).includes("Maximum"));
            }
            // Numeric array-hole lookup has a separate exotic-to-prototype edge.
            var array = [];
            Object.setPrototypeOf(array, deep);
            console.log(array[12345] === undefined ? "index-safe" : "index-wrong");
            // A caught depth failure must restore the shared counter.
            console.log("present" in {present: 1});
            "#,
        ),
        ["get-safe", "true true", "index-safe", "true"]
    );
}

#[test]
fn nested_proxy_get_and_has_fail_closed_but_shallow_semantics_survive() {
    assert_eq!(
        run_on_small_stack(
            r#"
            var target = {answer: 42};
            var deep = target;
            for (var i = 0; i < 512; i++) deep = new Proxy(deep, {});

            try { deep.answer; console.log("get-unexpected"); }
            catch (error) { console.log(error instanceof RangeError ? "get-range" : "get-other"); }

            try { Reflect.has(deep, "answer"); console.log("has-unexpected"); }
            catch (error) { console.log(error instanceof RangeError ? "has-range" : "has-other"); }

            var shallow = new Proxy(new Proxy(target, {}), {});
            console.log(shallow.answer, "answer" in shallow);

            // JavaScript traps re-enter the interpreter and have a larger native
            // frame than transparent forwarding. Their separate safe ceiling
            // must fail catchably too, and unwind before the next operation.
            var trapped = target;
            for (var j = 0; j < 512; j++) {
                trapped = new Proxy(trapped, {
                    get: function (inner, key, receiver) {
                        return Reflect.get(inner, key, receiver);
                    },
                    has: function (inner, key) {
                        return Reflect.has(inner, key);
                    }
                });
            }
            try { trapped.answer; console.log("trap-get-unexpected"); }
            catch (error) { console.log(error instanceof RangeError ? "trap-get-range" : "trap-get-other"); }
            try { Reflect.has(trapped, "answer"); console.log("trap-has-unexpected"); }
            catch (error) { console.log(error instanceof RangeError ? "trap-has-range" : "trap-has-other"); }

            // Observable traps still forward in order below that ceiling.
            var gets = 0, hases = 0;
            var observed = target;
            for (var k = 0; k < 2; k++) {
                observed = new Proxy(observed, {
                    get: function (inner, key, receiver) {
                        gets++;
                        return Reflect.get(inner, key, receiver);
                    },
                    has: function (inner, key) {
                        hases++;
                        return Reflect.has(inner, key);
                    }
                });
            }
            console.log(observed.answer, "answer" in observed, gets, hases);
            "#,
        ),
        [
            "get-range",
            "has-range",
            "42 true",
            "trap-get-range",
            "trap-has-range",
            "42 true 2 2",
        ]
    );
}

#[test]
fn proxy_meta_operation_siblings_share_the_depth_budget() {
    assert_eq!(
        run_on_small_stack(
            r#"
            function wrapped() {
                var value = {x: 1};
                for (var i = 0; i < 512; i++) value = new Proxy(value, {});
                return value;
            }
            function expectRange(operation) {
                try { operation(); console.log("unexpected"); }
                catch (error) { console.log(error instanceof RangeError ? "range" : "other"); }
            }
            expectRange(function () { delete wrapped().x; });
            expectRange(function () { Reflect.defineProperty(wrapped(), "y", {value: 2}); });
            expectRange(function () { Reflect.ownKeys(wrapped()); });
            expectRange(function () { Reflect.getPrototypeOf(wrapped()); });
            expectRange(function () { Reflect.setPrototypeOf(wrapped(), null); });
            expectRange(function () { Reflect.isExtensible(wrapped()); });
            expectRange(function () { Reflect.preventExtensions(wrapped()); });
            expectRange(function () { Reflect.set(wrapped(), "x", 2); });
            "#,
        ),
        ["range", "range", "range", "range", "range", "range", "range", "range"]
    );
}

#[test]
fn transparent_callable_and_constructor_wrappers_cannot_recurse_the_host_stack() {
    assert_eq!(
        run_on_small_stack(
            r#"
            function F() { return 7; }
            var proxyCall = F;
            var boundCall = F;
            for (var i = 0; i < 512; i++) {
                proxyCall = new Proxy(proxyCall, {});
                boundCall = boundCall.bind(null);
            }
            console.log(typeof proxyCall, typeof boundCall);
            try { proxyCall(); console.log("proxy-call-unexpected"); }
            catch (error) { console.log(error instanceof RangeError ? "proxy-call-range" : "proxy-call-other"); }
            try { boundCall(); console.log("bound-call-unexpected"); }
            catch (error) { console.log(error instanceof RangeError ? "bound-call-range" : "bound-call-other"); }

            var proxyCtor = F;
            var boundCtor = F;
            for (var j = 0; j < 512; j++) {
                proxyCtor = new Proxy(proxyCtor, {});
                boundCtor = boundCtor.bind(null);
            }
            try { new proxyCtor(); console.log("proxy-ctor-unexpected"); }
            catch (error) { console.log(error instanceof RangeError ? "proxy-ctor-range" : "proxy-ctor-other"); }
            try { new boundCtor(); console.log("bound-ctor-unexpected"); }
            catch (error) { console.log(error instanceof RangeError ? "bound-ctor-range" : "bound-ctor-other"); }

            var shallow = new Proxy(F.bind(null), {});
            console.log(shallow());
            "#,
        ),
        [
            "function function",
            "proxy-call-range",
            "bound-call-range",
            "proxy-ctor-range",
            "bound-ctor-range",
            "7",
        ]
    );
}
