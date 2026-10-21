//! Tier C `throw` (`jit_throw`): every entry unwinds the committed throw as
//! the interpreter does, compiled or not.

use std::process::Command;

const SOURCE: &str = r#"
    const N = 4000;
    const out = [];

    // A guard clause, called through a compiled caller, caught in an
    // interpreted loop (the frame entry and the frame-free cross call).
    function check(v, i) { if ((i & 7) === 0) throw new TypeError('bad ' + v); return v + 1; }
    function outer(v, i) { return check(v, i) | 0; }
    let n = 0, s = 0;
    for (let i = 0; i < N; i++) { try { s = outer(s, i); } catch (e) { n++; s ^= e.message.length; } }
    out.push('guard ' + n + ' ' + s);

    // Values that are not errors.
    function thrower(k) {
      if (k === 0) throw 42;
      if (k === 1) throw 'str';
      if (k === 2) throw { tag: 'obj' };
      if (k === 3) throw null;
      if (k === 4) throw undefined;
      return k;
    }
    let seen = '';
    for (let i = 0; i < N; i++) {
      try { thrower(i % 6); } catch (e) { if (i < 6) seen += typeof e + ':' + (e && e.tag ? e.tag : String(e)) + ','; }
    }
    out.push('values ' + seen);

    // A throw inside for-of closes the iterator (the frame-backed entry).
    let closed = 0;
    function iterable(k) {
      return { [Symbol.iterator]() { let i = 0; return { next() { return { value: i++, done: i > k }; }, return() { closed++; return {}; } }; } };
    }
    function sumUntil(it, stop) { let t = 0; for (const v of it) { if (v === stop) throw new RangeError('stop ' + v); t += v; } return t; }
    let caught = 0, total = 0;
    for (let i = 0; i < N; i++) { try { total += sumUntil(iterable(5), i % 7); } catch (e) { caught++; } }
    out.push('forof ' + caught + ' ' + total + ' ' + closed);

    // A getter that throws, read in a loop.
    const o = { get bad() { if (this.k++ % 3 === 0) throw new Error('g'); return this.k; }, k: 0 };
    let gc = 0, gt = 0;
    for (let i = 0; i < N; i++) { try { gt += o.bad; } catch (e) { gc++; } }
    out.push('getter ' + gc + ' ' + gt);

    // A builtin's callback.
    function cb(x) { if (x === 3) throw new Error('cb'); return x * 2; }
    let mc = 0;
    for (let r = 0; r < N; r++) { try { [1, 2, (r % 5)].map(cb); } catch (e) { mc++; } }
    out.push('callback ' + mc);

    // Several compiled frames down.
    function d3(x) { if (x % 11 === 0) throw new SyntaxError('d3 ' + x); return x; }
    function d2(x) { return d3(x) + 1; }
    function d1(x) { return d2(x) * 2; }
    let dc = 0, dt = 0, last = '';
    for (let i = 1; i < N; i++) { try { dt += d1(i); } catch (e) { dc++; last = e.name + ' ' + e.message; } }
    out.push('chain ' + dc + ' ' + dt + ' ' + last);

    console.log(out.join(' | '));
"#;

/// Node 24's output for `SOURCE`.
fn expected() -> Vec<String> {
    vec![concat!(
        "guard 500 999 | values number:42,string:str,object:obj,object:null,undefined:undefined, | ",
        "forof 2858 11420 2858 | getter 1334 5333333 | callback 800 | chain 363 14549820 SyntaxError d3 3993"
    )
    .into()]
}

#[test]
fn execution_mode_child() {
    if std::env::var_os("ZIPP_TIERC_THROW_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, expected());
}

#[test]
fn optimized_ablation_nojit_and_gc_modes_match() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("hot", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        (
            "throw_off",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_NO_TIERC_THROW", "1")][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        (
            "hot_gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let out = Command::new(&exe)
            .args(["execution_mode_child", "--exact"])
            .env("ZIPP_TIERC_THROW_CHILD", "1")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_NO_TIERC_THROW")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS")
            .envs(env.iter().copied())
            .output()
            .expect("spawn mode child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode} child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A throw nothing catches still ends the run with its value.
#[test]
fn an_uncaught_compiled_throw_is_the_run_error() {
    let out = zipp_vm::run(
        "function fin(x) { if (x > 2998) throw new EvalError('done ' + x); return x; }\n\
         for (let i = 0; i < 3000; i++) fin(i);",
    )
    .expect("source compiles");
    let error = out.error.expect("the throw escapes");
    assert!(error.contains("EvalError: done 2999"), "{error}");
}
