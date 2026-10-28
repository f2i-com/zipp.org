//! `try`/`catch` and `try`/`finally` in callbacks a builtin runs (`map`,
//! `forEach`, `filter`, `sort`, `reduce`, `find`, `findIndex`, `some`,
//! `every`, `flatMap`). The builtins run a compiled callback over a register
//! window with no frame of its own (`Vm::native_cb_entry`), and a body with
//! handler ops pushed its handlers on the caller's frame: a `finally` never
//! ran, a `catch` was missed, and the caller resumed at the callback's
//! handler target (a hang, a stray TypeError, a panic). Every handler must
//! belong to the callback's own activation: a throw lands in its `catch`, a
//! `finally` runs once per call, and a throw that escapes the callback
//! reaches the caller's `try`. Run interpreted, at the default thresholds and
//! with every function compiled at once; the expected line is Node 24's.

const EXPECTED: &str = "2,4|2|outer:TypeError1|1,2,3|true|1,3|TypeError|TypeError|TypeError|inner:TypeError|34248900|399000|2000|1500|777|false|true|0,1000|20100|0|299";

#[test]
fn handler_callbacks_child() {
    if std::env::var_os("ZIPP_HANDLER_CALLBACKS_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn handlers_in_builtin_callbacks_belong_to_the_callback() {
    if std::env::var_os("ZIPP_HANDLER_CALLBACKS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "handler_callbacks_child", "--nocapture"])
            .env("ZIPP_HANDLER_CALLBACKS_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        // The failure this pins was a hang: a child past its time is one.
        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("re-run test binary");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while child.try_wait().expect("poll the child").is_none() {
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                panic!("{mode}: the program did not finish in 120 s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let out = child.wait_with_output().expect("collect the child");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// try/catch and try/finally in callbacks a builtin runs.
const out = [];
let n = 0;
out.push([1, 2].map((x) => { try { return x * 2; } finally { n++; } }).join(), n);
n = 0;
try { [1].forEach(() => { try { null.x; } finally { n++; } }); } catch (e) { out.push("outer:" + e.constructor.name + n); }
n = 0;
out.push([3, 1, 2].sort((a, b) => { try { return a - b; } finally { n++; } }).join(), n > 0);
out.push([1, 2, 3].filter((x) => { try { if (x === 2) throw new Error("e"); return true; } catch (e) { return false; } }).join());
out.push([1].map(() => { try { throw new TypeError("x"); } catch (e) { return e.constructor.name; } })[0]);
out.push([1].map(() => { try { null.x; } catch (e) { return e.constructor.name; } })[0]);
const k = {};
out.push([1].map(() => { try { WeakMap.prototype.get.call(new Map(), k); return "no throw"; } catch (e) { return e.constructor.name; } })[0]);
function outer() {
  try { return [1].map(() => { try { null.x; } catch (e) { return "inner:" + e.constructor.name; } })[0]; }
  catch (e) { return "outer:" + e.constructor.name; }
}
for (let i = 0; i < 50; i++) outer();
out.push(outer());
// Hot: each builtin calls its callback many times, some calls throwing.
const xs = Array.from({ length: 2000 }, (_, i) => i);
const parse = (i) => { try { if (i % 7 === 0) throw new RangeError("r" + i); return i; } catch (e) { return -e.message.length; } };
let s = 0;
for (let r = 0; r < 20; r++) s += xs.map(parse).reduce((a, b) => a + b, 0);
out.push(s);
let fin = 0;
const red = xs.reduce((a, b) => { try { return a + (b % 5 === 0 ? b : 0); } finally { fin++; } }, 0);
out.push(red, fin);
out.push(xs.find((x) => { try { if (x === 1500) throw 0; return false; } catch (e) { return true; } }));
out.push(xs.findIndex((x) => { try { return x === 777; } catch (e) { return false; } }));
out.push(xs.some((x) => { try { return x.y.z; } catch (e) { return false; } }), xs.every((x) => { try { return x >= 0; } finally { } }));
out.push(xs.flatMap((x) => { try { if (x % 1000) return []; throw [x]; } catch (e) { return e; } }).join());
let escaped = 0;
for (let r = 0; r < 200; r++) {
  try { xs.forEach((x) => { try { if (x === r) throw new Error("x" + x); } finally { escaped++; } }); }
  catch (e) { if (e.message !== "x" + r) out.push("wrong " + e.message); }
}
out.push(escaped);
const sorted = xs.slice(0, 300).reverse().sort((a, b) => { try { if (a === b) throw 0; return a - b; } catch (e) { return 0; } });
out.push(sorted[0], sorted[299]);
console.log(out.join("|"));
"####;
