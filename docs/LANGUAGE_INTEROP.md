# Opt-in Python / JavaScript interoperability

Enable `python-js-interop` when building a trusted mixed-language program:

```sh
cargo run --locked -p zipp-cli --features python-js-interop -- py examples/python/interop
```

Expected output is `[2, 4, 6]`, `42`, and `zipp-same-vm`.
For a browser artifact, use `./build-variants.sh interop` in `crates/zipp-wasm`.
The ordinary `all` artifact and published playground omit this feature.

`import javascript; javascript.eval(source)` and `from js import eval` invoke
Zipp's captured JavaScript eval intrinsic. The evaluator executes in the same
VM instance as the Python program, never in the browser's JavaScript engine.
JavaScript globals persist across calls in that instance; separate engines have
separate globals. Host APIs are not automatically added by enabling this feature.

Results cross as copied data: finite numbers, strings, booleans, BigInts, lists,
plain dictionaries and null/undefined (Python None). Cycles, accessors, functions,
symbols and non-plain objects are rejected. Conversion is capped at 10,000 visited
values and depth 32; source is capped at 32,768 UTF-16 code units. The embedding
VM's runtime-compilation policy and execution budgets continue to apply.
JavaScript exceptions become Python RuntimeError; invalid results become TypeError.

This deliberately shares VM globals, including Python runtime globals. It is
not an isolation boundary between mutually untrusted languages. Keep the default
feature set when the Python program must not access that shared scope.

This first implementation does not provide JS imports of `.py` modules, live
object or callable proxies, or arbitrary `from js import something` bindings.
The [tests](../crates/zipp-vm/tests/python_javascript.rs) check same-instance state,
separate-engine state, data conversion, errors and instruction-budget termination.
