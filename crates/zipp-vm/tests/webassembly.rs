//! `WebAssembly`: modules compiled, instantiated and called from JavaScript.
//!
//! Each module is written in the text format and compiled to bytes here; the
//! program sees them as a `Uint8Array`. What the lines pin: the namespace's
//! shape, values crossing both ways (i32 wraps, i64 is BigInt, f32 rounds),
//! imports (a throw from one arrives as itself, through the wasm frames),
//! traps as `RuntimeError`, memory shared without copying and detached by
//! `grow`, tables with `call_indirect`, `br_table`, globals, linking errors,
//! re-entrant calls from an import, and the promise-returning API.
#![cfg(feature = "webassembly")]

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    state.run_init().expect("source runs");
    state.run_microtasks();
    state.take_output()
}

/// `wat` as a JavaScript expression: a Uint8Array of its binary.
fn bytes(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let list: Vec<String> = wasm.iter().map(|b| b.to_string()).collect();
    format!("new Uint8Array([{}])", list.join(","))
}

fn program(modules: &[(&str, &str)], body: &str) -> String {
    let mut src = String::new();
    for (name, wat) in modules {
        src.push_str(&format!("var {name} = {};\n", bytes(wat)));
    }
    src.push_str(body);
    src
}

#[test]
fn namespace_and_values() {
    let src = program(
        &[(
            "arith",
            r#"(module
                (func (export "add") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add)
                (func (export "mul64") (param i64 i64) (result i64) local.get 0 local.get 1 i64.mul)
                (func (export "half") (param f32) (result f32) local.get 0 f32.const 0.5 f32.mul)
                (func (export "pair") (result i32 f64) i32.const 7 f64.const 2.5))"#,
        )],
        r#"
        console.log(typeof WebAssembly, Object.prototype.toString.call(WebAssembly));
        console.log(WebAssembly.validate(arith), WebAssembly.validate(new Uint8Array([0, 97, 115, 109])));
        var m = new WebAssembly.Module(arith);
        console.log(JSON.stringify(WebAssembly.Module.exports(m)));
        var e = new WebAssembly.Instance(m).exports;
        console.log(e.add(2, 3), e.add(0x7fffffff, 1), e.add("4", 1.9), e.add.length);
        console.log(e.mul64(1n << 20n, 1n << 30n), e.mul64(1n << 40n, 1n << 30n), e.mul64(-3n, 5n));
        console.log(e.half(3), e.half(0.1));
        console.log(JSON.stringify(e.pair()));
        try { e.mul64(1, 2); } catch (err) { console.log(err.name); }
        console.log(Object.isFrozen(e), e.add === new WebAssembly.Instance(m).exports.add);
        "#,
    );
    assert_eq!(
        run(&src),
        vec![
            "object [object WebAssembly]",
            "true false",
            // wasmi keeps exports by name, so they list in name order, not the
            // module's (a known difference from the JS API's ordering).
            r#"[{"name":"add","kind":"function"},{"name":"half","kind":"function"},{"name":"mul64","kind":"function"},{"name":"pair","kind":"function"}]"#,
            "5 -2147483648 5 2",
            "1125899906842624n 0n -15n",
            "1.5 0.05000000074505806",
            "[7,2.5]",
            "TypeError",
            "true false",
        ]
    );
}

#[test]
fn imports_throws_and_traps() {
    let src = program(
        &[(
            "calls",
            r#"(module
                (import "env" "log" (func $log (param i32)))
                (import "env" "fail" (func $fail))
                (func (export "run") (param i32) local.get 0 call $log local.get 0 i32.const 1 i32.add call $log)
                (func (export "throws") call $fail)
                (func (export "div") (param i32 i32) (result i32) local.get 0 local.get 1 i32.div_s)
                (func (export "boom") unreachable))"#,
        )],
        r#"
        var seen = [];
        var marker = { why: "from js" };
        var e = new WebAssembly.Instance(new WebAssembly.Module(calls), {
          env: { log: (v) => seen.push(v), fail: () => { throw marker; } },
        }).exports;
        e.run(41);
        console.log(seen.join(","));
        try { e.throws(); } catch (err) { console.log(err === marker); }
        try { e.div(1, 0); } catch (err) { console.log(err instanceof WebAssembly.RuntimeError, err.message); }
        try { e.boom(); } catch (err) { console.log(err.name, err instanceof Error); }
        console.log(e.div(-7, 2));
        "#,
    );
    assert_eq!(
        run(&src),
        vec!["41,42", "true", "true integer divide by zero", "RuntimeError true", "-3"]
    );
}

#[test]
fn memory_is_shared_and_grow_detaches() {
    let src = program(
        &[(
            "mem",
            r#"(module
                (memory (export "memory") 1 4)
                (func (export "store") (param i32 i32) local.get 0 local.get 1 i32.store8)
                (func (export "load") (param i32) (result i32) local.get 0 i32.load8_u)
                (func (export "grow") (param i32) (result i32) local.get 0 memory.grow)
                (func (export "size") (result i32) memory.size))"#,
        )],
        r#"
        var e = new WebAssembly.Instance(new WebAssembly.Module(mem)).exports;
        var buf = e.memory.buffer;
        console.log(buf instanceof ArrayBuffer, buf.byteLength, buf === e.memory.buffer);
        new Uint8Array(buf)[100] = 77;
        e.store(200, 99);
        console.log(e.load(100), new Uint8Array(buf)[200]);
        console.log(e.memory.grow(1), buf.byteLength, e.memory.buffer.byteLength, e.size());
        console.log(new Uint8Array(e.memory.buffer)[100]);
        var before = e.memory.buffer;
        console.log(e.grow(1), before.byteLength, e.memory.buffer.byteLength);
        try { e.memory.grow(10); } catch (err) { console.log(err.name); }
        var imported = new WebAssembly.Memory({ initial: 1 });
        new Uint8Array(imported.buffer)[0] = 5;
        console.log(imported.buffer.byteLength, new Uint8Array(imported.buffer)[0]);
        "#,
    );
    assert_eq!(
        run(&src),
        vec![
            "true 65536 true",
            "77 99",
            "1 0 131072 2",
            "77",
            "2 0 196608",
            "RangeError",
            "65536 5",
        ]
    );
}

#[test]
fn tables_branches_and_globals() {
    let src = program(
        &[
            (
                "tab",
                r#"(module
                    (import "env" "base" (global $base i32))
                    (type $t (func (param i32) (result i32)))
                    (table (export "table") 3 funcref)
                    (func $double (param i32) (result i32) local.get 0 i32.const 2 i32.mul)
                    (func $square (param i32) (result i32) local.get 0 local.get 0 i32.mul)
                    (elem (i32.const 0) $double $square)
                    (func (export "apply") (param i32 i32) (result i32)
                        local.get 1 local.get 0 call_indirect (type $t))
                    (func (export "pick") (param i32) (result i32)
                        (block $c (block $b (block $a
                            local.get 0 br_table $a $b $c)
                            i32.const 10 return)
                            i32.const 20 return)
                        i32.const 30)
                    (global $count (export "count") (mut i32) (i32.const 1))
                    (func (export "bump") (result i32)
                        global.get $count global.get $base i32.add global.set $count global.get $count))"#,
            ),
        ],
        r#"
        var e = new WebAssembly.Instance(new WebAssembly.Module(tab), { env: { base: 5 } }).exports;
        console.log(e.apply(0, 21), e.apply(1, 9));
        try { e.apply(2, 1); } catch (err) { console.log(err.name); }
        console.log(e.pick(0), e.pick(1), e.pick(2), e.pick(9));
        console.log(e.table.length, typeof e.table.get(0), e.table.get(0)(4), e.table.get(2));
        e.table.set(2, e.table.get(1));
        console.log(e.apply(2, 6));
        console.log(e.count.value, e.bump(), e.bump(), e.count.value);
        e.count.value = 100;
        console.log(e.bump());
        var g = new WebAssembly.Global({ value: "i64", mutable: false }, 9n);
        console.log(g.value, g.valueOf());
        try { g.value = 1n; } catch (err) { console.log(err.name); }
        "#,
    );
    assert_eq!(
        run(&src),
        vec![
            "42 81",
            "RuntimeError",
            "10 20 30 30",
            "3 function 8 null",
            "36",
            "1 6 11 11",
            "105",
            "9n 9n",
            "TypeError",
        ]
    );
}

#[test]
fn errors_reentry_and_promises() {
    let src = program(
        &[
            (
                "needs",
                r#"(module (import "env" "f" (func)) (func (export "go") call 0))"#,
            ),
            (
                "reenter",
                r#"(module
                    (import "env" "back" (func $back (param i32) (result i32)))
                    (func (export "inc") (param i32) (result i32) local.get 0 i32.const 1 i32.add)
                    (func (export "viaJs") (param i32) (result i32) local.get 0 call $back))"#,
            ),
        ],
        r#"
        try { new WebAssembly.Module(new Uint8Array([1, 2, 3])); } catch (err) { console.log(err instanceof WebAssembly.CompileError); }
        try { new WebAssembly.Instance(new WebAssembly.Module(needs), { env: {} }); } catch (err) { console.log(err.name); }
        try { new WebAssembly.Instance(new WebAssembly.Module(needs)); } catch (err) { console.log(err.name); }
        var inst;
        inst = new WebAssembly.Instance(new WebAssembly.Module(reenter), {
          env: { back: (v) => inst.exports.inc(v) * 10 },
        });
        console.log(inst.exports.viaJs(4));
        WebAssembly.instantiate(reenter, { env: { back: (v) => v } }).then((r) => {
          console.log(r.module instanceof WebAssembly.Module, r.instance.exports.viaJs(8));
        });
        WebAssembly.compile(new Uint8Array([0])).catch((err) => console.log("rejected", err.name));
        "#,
    );
    assert_eq!(
        run(&src),
        vec!["true", "LinkError", "TypeError", "50", "rejected CompileError", "true 8"]
    );
}

/// `externref`: JavaScript values held by WebAssembly -- in tables, as
/// parameters and results -- coming back as themselves. wasm-bindgen's
/// reference-types output grows an exported externref table and fills it
/// from JavaScript as it starts, which is the first half of this.
#[test]
fn externref_tables_and_values() {
    let src = program(
        &[(
            "refs",
            r#"(module
                (table (export "table") 1 externref)
                (func (export "id") (param externref) (result externref) local.get 0)
                (func (export "isNull") (param externref) (result i32) local.get 0 ref.is_null)
                (func (export "keep") (param i32 externref) local.get 0 local.get 1 table.set 0)
                (func (export "fetch") (param i32) (result externref) local.get 0 table.get 0))"#,
        )],
        r#"
        var e = new WebAssembly.Instance(new WebAssembly.Module(refs)).exports;
        var t = e.table;
        var old = t.grow(4);
        t.set(0, undefined); t.set(old, undefined); t.set(old + 1, null); t.set(old + 2, true); t.set(old + 3, false);
        console.log(old, t.length, t.get(0), t.get(old + 1), t.get(old + 2), t.get(old + 3));
        var o = { name: "kept" };
        console.log(e.id(o) === o, e.id("s"), e.id(42), e.isNull(null), e.isNull(o), e.isNull(undefined));
        e.keep(2, o);
        for (var i = 0; i < 2000; i++) { var junk = { i: i, pad: [i, i, i] }; }
        if (typeof gc === "function") gc();
        console.log(e.fetch(2) === o, e.fetch(2).name, t.get(2) === o);
        var mine = new WebAssembly.Table({ element: "externref", initial: 2 });
        mine.set(1, o);
        console.log(mine.length, mine.get(0), mine.get(1) === o, mine.grow(1, "x"), mine.get(2));
        try { new WebAssembly.Table({ element: "i32", initial: 1 }); } catch (err) { console.log(err.name); }
        "#,
    );
    assert_eq!(
        run(&src),
        vec![
            "1 5 undefined null true false",
            "true s 42 1 0 0",
            "true kept true",
            "2 undefined true 2 x",
            "TypeError",
        ]
    );
}

/// A long-running module: ten million instructions in one call must not
/// take native stack per instruction (see the `wasmi` dependency's dispatch
/// in Cargo.toml). Only an optimised build could show it; the line keeps the
/// shape pinned everywhere.
#[test]
fn long_loops_run_in_constant_stack() {
    let src = program(
        &[(
            "spin",
            r#"(module
                (func (export "count") (param i32) (result i32) (local i32)
                  (block (loop
                    local.get 1 local.get 0 i32.ge_u br_if 1
                    local.get 1 i32.const 1 i32.add local.set 1
                    br 0))
                  local.get 1))"#,
        )],
        r#"console.log(new WebAssembly.Instance(new WebAssembly.Module(spin)).exports.count(3000000));"#,
    );
    assert_eq!(run(&src), vec!["3000000"]);
}
