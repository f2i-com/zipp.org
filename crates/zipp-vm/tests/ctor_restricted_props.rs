//! B189 regression: `Function.arguments` / `Function.caller` must THROW a
//! TypeError (the restricted-property accessors inherited from
//! %Function.prototype%) even when the reading closure body is Tier-C
//! compiled.
//!
//! The defect this pins: the JIT property-miss helpers' proto walks treat "no
//! `proto_of` entry" as "[[Prototype]] is %Object.prototype%". That default is
//! wrong for a CTOR-map receiver (`Function`, builtin constructors), whose
//! chain runs through %Function.prototype% — where the throwing accessors
//! live. The walk therefore answered `undefined` without throwing once the
//! access site compiled. Hop objects were already `!is_ctor`-guarded; the
//! receiver side was not. It surfaced the moment B189's admission-floor drop
//! first compiled a tiny `() => f.arguments` body (test262
//! staging/sm/extensions/newer-type-functions-caller-arguments.js); before
//! that, such bodies were blacklisted and the interpreter's restricted
//! protocol always ran.
//!
//! The loop runs far past every JIT threshold so the access executes through
//! the compiled probe + miss helper, not just the interpreter IC.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

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
fn ctor_receiver_restricted_props_throw_when_hot() {
    let out = run_ok(
        r#"
        function check(f) {
          var argOk = false, calOk = false;
          try { (function () { return f.arguments; })(); }
          catch (e) { argOk = e instanceof TypeError; }
          try { (function () { return f.caller; })(); }
          catch (e) { calOk = e instanceof TypeError; }
          return argOk && calOk;
        }
        var fns = [Function, Array, Object, function*(){}, async function(){}, () => {}];
        var bad = 0;
        for (var round = 0; round < 4000; round++) {
          for (var i = 0; i < fns.length; i++) {
            if (!check(fns[i])) bad++;
          }
        }
        console.log("bad=" + bad);
        "#,
    );
    assert_eq!(out, vec!["bad=0".to_string()]);
}

#[test]
fn ctor_receiver_static_reads_still_serve_when_hot() {
    // The guard must DECLINE to the interpreter, not break ctor statics: own
    // data props on ctor maps (`Array.isArray`) and absent lookups keep their
    // answers under heat.
    let out = run_ok(
        r#"
        var hits = 0, absents = 0;
        for (var i = 0; i < 4000; i++) {
          if (typeof Array.isArray === "function") hits++;
          if (Array.certainlyNotThere === undefined) absents++;
        }
        console.log(hits, absents);
        "#,
    );
    assert_eq!(out, vec!["4000 4000".to_string()]);
}
