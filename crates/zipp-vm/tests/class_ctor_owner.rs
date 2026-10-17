//! The running constructor's class (`running_class_value`) found from the
//! kept constructor callable: each evaluation of a class expression must
//! still construct with its OWN fields, private names, statics and parent,
//! across derived chains, return overrides, classes that die and are made
//! again (heap slots reused), and under a collection at every safe point.
//! The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function make(tag) {
  class Base {
    #secret = tag + "-base";
    static made = 0;
    field = tag;
    constructor(x) { this.x = x; Base.made++; }
    reveal() { return this.#secret; }
    static count() { return Base.made; }
  }
  class Mid extends Base {
    #mid = tag + "-mid";
    constructor(x) { super(x * 2); this.mid = this.#mid; }
    midOf(o) { return #mid in o ? o.#mid : "none"; }
  }
  class Leaf extends Mid {
    #leaf;
    constructor(x) { super(x + 1); this.#leaf = tag + "-leaf:" + this.x; }
    leaf() { return this.#leaf; }
  }
  return { Base, Mid, Leaf };
}
var kinds = [], prev = null;
for (var round = 0; round < 6; round++) {
  var k = make("r" + round);
  var objs = [];
  for (var i = 0; i < 40; i++) objs.push(new k.Leaf(i));
  var last = objs[objs.length - 1];
  kinds.push(k);
  log("round", round, last.leaf(), last.reveal(), last.mid, last.field, k.Base.count(), k.Mid.prototype.midOf(last));
  if (prev) log("cross", prev.Mid.prototype.midOf(last), last instanceof prev.Base);
  var junk = []; for (var j = 0; j < 2000; j++) junk.push({ j: j, s: "x" + j });
  if (round % 2 === 0) kinds.shift();
  prev = k;
}
class Override { constructor() { return { overridden: true }; } }
class Derived extends Override { #p = 1; constructor() { super(); this.after = 1; } static has(o) { return #p in o; } }
var d = new Derived();
log("override", JSON.stringify(d), Derived.has(d));
var built = [];
for (var i = 0; i < 30; i++) {
  var C = make("again" + (i % 3)).Leaf;
  built.push(new C(i).leaf());
}
log("again", built.slice(-3).join());
console.log(out.join("\n"));
"##;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn class_ctor_owner_worker() {
    if std::env::var_os("ZIPP_CLASS_CTOR_OWNER_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

#[test]
fn class_ctor_owner_matches_node() {
    if std::env::var_os("ZIPP_CLASS_CTOR_OWNER_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, gc_stress) in [("interpreter", false), ("interpreter-gc-stress", true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["class_ctor_owner_worker", "--exact", "--nocapture"])
            .env("ZIPP_CLASS_CTOR_OWNER_WORKER", "1")
            .env("ZIPP_NOJIT", "1")
            .env_remove("ZIPP_GC_STRESS");
        if gc_stress {
            command.env("ZIPP_GC_STRESS", "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
