//! B192: module top-level loops must not pay the statement-completion tax.
//!
//! A module's completion value is spec-unobservable (Module Record
//! `Evaluate()` resolves undefined; dynamic `import()` resolves the
//! NAMESPACE), yet the compiler tracked it — planting a `LoadUndefined` +
//! per-statement `Move` into every module top-level loop body, which alone
//! demoted such loops off the INT register tier (the isolated nest ran 3×
//! slower as a module than as a script). The completion accumulator is now
//! eval-only, and the INT tier additionally admits `LoadUndefined` for
//! dead-in-region completion regs (the eval shapes that legitimately keep
//! them), writing every def through to the frame slot.
//!
//! These tests pin: (1) module output equals the identical script's, with
//! the loop hot enough that both tiers engage; (2) `eval()`'s RESULT — the
//! one observable completion value — is preserved, including through a hot
//! loop as the final statement.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const NEST: &str = r#"
var s = "useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";
var id = "";
for (var k = 0; k < 21; k++) id += s[k];
var checksum = 0x811c9dc5;
for (var i = 0; i < 120000; i++) {
  for (var j = 0; j < id.length; j++) {
    checksum = Math.imul(checksum ^ id.charCodeAt(j), 0x01000193) >>> 0;
  }
}
console.log(checksum);
"#;

#[test]
fn module_nest_matches_script_nest() {
    let script = zipp_vm::run(NEST).expect("script compiles");
    assert!(script.error.is_none(), "script error: {:?}", script.error);
    let module = zipp_vm::run_module_with_base(NEST, None).expect("module compiles");
    assert!(module.error.is_none(), "module error: {:?}", module.error);
    assert_eq!(script.output, module.output);
    assert_eq!(script.output.len(), 1);
}

#[test]
fn eval_completion_value_is_preserved() {
    let out = zipp_vm::run(
        r#"
        // The final statement's value IS eval's result — including when that
        // statement is a hot loop whose completion accumulator now runs
        // through the INT tier's write-through lane.
        var r1 = eval("var x = 5; x + 1");
        var r2 = eval("var t = 0; for (var i = 0; i < 200000; i++) { t = (t + i) | 0; } t");
        var r3 = eval("'use strict'");
        var r4 = eval("var q = 0; for (var i = 0; i < 10; i++) {} q");
        console.log(r1, r2, r3, r4);
        "#,
    )
    .expect("source compiles");
    assert!(out.error.is_none(), "error: {:?}", out.error);
    assert_eq!(out.output, vec!["6 -1474936480 use strict 0".to_string()]);
}
