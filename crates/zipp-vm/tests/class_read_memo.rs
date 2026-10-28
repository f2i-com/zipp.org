//! A class method read in compiled code (`o.m` read ahead of its call's
//! arguments, as `a.add(b.add(a))` is), which the miss helper answers from
//! a per-site memo guarded as the interpreter's class-method hit is (the
//! receiver's class and shape, the class's version, the prototype epoch):
//! a plain class, a subclass override at the same site, a prototype method
//! patched and then deleted mid-loop, an own property shadowing the method,
//! instances re-prototyped onto the subclass, dictionary-mode receivers, a
//! getter beside the methods, and a class declaration evaluated again. Run
//! at the default thresholds and with every function compiled at once; the
//! expected lines are Node 24's.

const EXPECTED: &[&str] = &[
    r#"plain 891915450"#,
    r#"sub -1453086099"#,
    r#"patch -69138279"#,
    r#"own-shadow -1942939786"#,
    r#"reproto -1654747432"#,
    r#"dict 1074139870"#,
    r#"reeval 19990000"#,
    r#"unpatch TypeError"#,
];

fn check(threshold: Option<&str>) {
    // The JIT threshold is read once per process, so each setting runs in a
    // child of this test binary.
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["--exact", "class_read_child", "--nocapture"])
        .env("ZIPP_CLASS_READ_CHILD", "1");
    if let Some(t) = threshold {
        cmd.env("ZIPP_JIT_THRESHOLD", t);
    }
    let out = cmd.output().expect("re-run test binary");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn class_read_child() {
    if std::env::var_os("ZIPP_CLASS_READ_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED);
}

#[test]
fn class_method_reads_in_compiled_code_follow_the_chain() {
    if std::env::var_os("ZIPP_CLASS_READ_CHILD").is_some() {
        return;
    }
    check(None);
    check(Some("1"));
}

const PROGRAM: &str = r####"// Class method reads ahead of a call's arguments (`o.m(o2.x)`), in hot code,
// while the class chain and the receivers change under them.
const out = [];
class Vec {
  constructor(x, y) { this.x = x; this.y = y; }
  add(o) { return new Vec(this.x + o.x, this.y + o.y); }
  get len() { return Math.abs(this.x) + Math.abs(this.y); }
  tag() { return "vec"; }
}
class Vec3 extends Vec {
  constructor(x, y, z) { super(x, y); this.z = z; }
  tag() { return "vec3"; }
}
function step(a, b) { return a.add(b.add(a)).tag() + a.add(b).len; }
function run(label, n, mk) {
  let h = 0;
  for (let i = 0; i < n; i++) {
    const a = mk(i), b = mk(i + 1);
    const s = String(step(a, b));
    for (let k = 0; k < s.length; k++) h = (h * 31 + s.charCodeAt(k)) | 0;
    if (i === n >> 1 && label === "patch") Vec.prototype.tag = function () { return "patched"; };
    if (i === n >> 1 && label === "unpatch") delete Vec.prototype.tag;
  }
  out.push(label + " " + h);
}
run("plain", 4000, (i) => new Vec(i, -i));
run("sub", 4000, (i) => (i % 3 ? new Vec(i, 1) : new Vec3(i, 2, 3)));
run("patch", 4000, (i) => new Vec(i, i));
run("own-shadow", 4000, (i) => { const v = new Vec(i, 1); if (i % 5 === 0) v.tag = () => "own"; return v; });
run("reproto", 4000, (i) => { const v = new Vec(i, 2); if (i % 7 === 0) Object.setPrototypeOf(v, Vec3.prototype); return v; });
run("dict", 4000, (i) => { const v = new Vec(i, 3); if (i % 4 === 0) { v.q = 1; delete v.q; delete v.y; v.y = 3; } return v; });
// A class declaration evaluated again: a new class with the same body.
function mk(k) { return class P { m(o) { return o.v * k; } }; }
let acc = 0;
for (let r = 0; r < 4; r++) {
  const P = mk(r + 1);
  const p = new P();
  for (let i = 0; i < 2000; i++) acc += p.m({ v: [i][0] });
}
out.push("reeval " + acc);
try { run("unpatch", 4000, (i) => new Vec(i, i)); } catch (e) { out.push("unpatch " + e.constructor.name); }
console.log(out.join("\n"));
"####;
