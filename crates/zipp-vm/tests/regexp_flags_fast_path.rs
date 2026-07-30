//! `get RegExp.prototype.flags` has a pristine shortcut (B70) that returns the
//! internal flag string instead of reading the eight per-flag accessors off the
//! receiver. Per spec those eight reads ARE observable, so the shortcut is only
//! legal while every one of them provably returns the intrinsic.
//!
//! Two things have to hold, and both are tested here:
//!
//!  1. the shortcut's ANSWER is byte-identical to what the eight reads build, for
//!     every flag combination — including canonical ORDER, which is `dgimsuvy`
//!     and not the order the flags were written in;
//!  2. it stops firing the moment any of the eight, or `flags` itself, is shadowed
//!     — per instance or on the prototype — because `@@match`/`@@replace`/
//!     `matchAll` all read `flags` and a user override must reach them.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn every_flag_combination_matches_the_observable_synthesis() {
    // The reference synthesis, in JS, reading the same eight accessors in the same
    // order — then diffed against `re.flags` for all 2^8 subsets.
    let out = run_ok(
        r#"
        var NAMES = ["hasIndices","global","ignoreCase","multiline","dotAll",
                     "unicode","unicodeSets","sticky"];
        var CH = ["d","g","i","m","s","u","v","y"];
        function reference(re) {
          var out = "";
          for (var k = 0; k < NAMES.length; k++) if (re[NAMES[k]]) out += CH[k];
          return out;
        }
        var bad = 0, checked = 0;
        for (var mask = 0; mask < 256; mask++) {
          // u and v are mutually exclusive; d/g/i/m/s/y are free.
          if ((mask & 32) && (mask & 64)) continue;
          var f = "";
          for (var k = 0; k < 8; k++) if (mask & (1 << k)) f += CH[k];
          var re;
          try { re = new RegExp("a", f); } catch (e) { continue; }
          checked++;
          if (re.flags !== reference(re)) { bad++; }
          // and the canonical order is dgimsuvy regardless of input order
          var rev = f.split("").reverse().join("");
          var re2;
          try { re2 = new RegExp("a", rev); } catch (e) { continue; }
          if (re2.flags !== re.flags) bad++;
        }
        console.log("checked=" + checked + " mismatches=" + bad);
        "#,
    );
    assert_eq!(out[0], "checked=192 mismatches=0");
}

#[test]
fn a_per_instance_flag_shadow_is_still_observed() {
    let out = run_ok(
        r#"
        var re = /a/gi;
        console.log("clean=" + re.flags);
        Object.defineProperty(re, "global", { value: false, configurable: true });
        console.log("shadowed=" + re.flags);
        "#,
    );
    assert_eq!(out[0], "clean=gi");
    assert_eq!(out[1], "shadowed=i");
}

#[test]
fn a_replaced_prototype_flag_accessor_is_still_observed() {
    let out = run_ok(
        r#"
        var re = /a/gi;
        console.log("clean=" + re.flags);
        var realDesc = Object.getOwnPropertyDescriptor(RegExp.prototype, "ignoreCase");
        Object.defineProperty(RegExp.prototype, "ignoreCase", {
          get: function () { return false; }, configurable: true
        });
        console.log("patched=" + re.flags);
        Object.defineProperty(RegExp.prototype, "ignoreCase", realDesc);
        console.log("restored=" + re.flags);
        "#,
    );
    assert_eq!(out[0], "clean=gi");
    assert_eq!(out[1], "patched=g");
    assert_eq!(out[2], "restored=gi");
}

#[test]
fn a_throwing_flag_getter_still_propagates() {
    let out = run_ok(
        r#"
        var real = Object.getOwnPropertyDescriptor(RegExp.prototype, "sticky");
        Object.defineProperty(RegExp.prototype, "sticky", {
          get: function () { throw new RangeError("boom"); }, configurable: true
        });
        var got;
        try { got = "v:" + /a/g.flags; } catch (e) { got = "throw:" + e.constructor.name; }
        Object.defineProperty(RegExp.prototype, "sticky", real);
        console.log(got);
        "#,
    );
    assert_eq!(out[0], "throw:RangeError");
}

#[test]
fn a_foreign_receiver_does_not_take_the_shortcut() {
    // `Reflect.get(RegExp.prototype, "flags", other)` runs the getter with `other`
    // as receiver — the shortcut must not answer from the RegExp it was reached
    // through, and per spec the eight reads happen on `other`.
    let out = run_ok(
        r#"
        var re = /a/gi;
        var other = { global: true, sticky: true };
        console.log(Reflect.get(RegExp.prototype, "flags", other));
        var got;
        try { got = Reflect.get(RegExp.prototype, "flags", re); } catch (e) { got = "throw"; }
        console.log(got);
        "#,
    );
    assert_eq!(out[0], "gy");
    assert_eq!(out[1], "gi");
}

#[test]
fn a_reprototyped_instance_does_not_take_the_shortcut() {
    let out = run_ok(
        r#"
        var re = /a/gi;
        Object.setPrototypeOf(re, { global: true, sticky: true, hasIndices: false,
          ignoreCase: false, multiline: false, dotAll: false, unicode: false,
          unicodeSets: false });
        var got;
        try { got = Object.getOwnPropertyDescriptor(RegExp.prototype, "flags").get.call(re); }
        catch (e) { got = "throw"; }
        console.log(got);
        "#,
    );
    assert_eq!(out[0], "gy");
}

#[test]
fn matchall_still_rejects_a_non_global_regexp() {
    // matchAll reads `flags` only to test for `g`; the shortcut must not lose the
    // TypeError, nor the observable ordering when a flag IS shadowed.
    let out = run_ok(
        r#"
        var got;
        try { "abc".matchAll(/b/); got = "no-throw"; } catch (e) { got = e.constructor.name; }
        var n = 0; for (var m of "a=1 b=2".matchAll(/([a-z])=(\d)/g)) n++;
        console.log(got + " n=" + n);
        "#,
    );
    assert_eq!(out[0], "TypeError n=2");
}
