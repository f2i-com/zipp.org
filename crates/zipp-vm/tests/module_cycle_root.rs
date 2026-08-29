//! [[CycleRoot]] is recorded at DFS time (InnerModuleEvaluation step 16),
//! and a later import of an EVALUATING-ASYNC / EVALUATED module answers
//! through that root (Evaluate step 2.a): its recorded [[EvaluationError]],
//! its still-pending capability. The reductions here are the shapes the
//! static-graph derivation got wrong or paid for:
//!
//! - a deferred back-edge (`import defer` of a module that imports its
//!   importer) is NOT an evaluation edge — the deferred module's trigger-time
//!   error must not be attributed to the importer;
//! - a synchronous throw marks every module still on the stack, including a
//!   finished member of the unclosed component (Evaluate step 9);
//! - when both a member's own rejection and the root's exist, the root's
//!   error wins (the root's [[TopLevelCapability]] is what settles);
//! - GatherAsynchronousTransitiveDependencies stops at an EVALUATING module
//!   without walking its requests (step 6);
//! - ordinary imports keep working (and stay cheap) after an unrelated
//!   module failed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "zipp-cycle-root-{name}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create fixture directory");
        for (file, src) in files {
            std::fs::write(dir.join(file), src).expect("write fixture");
        }
        Self(dir)
    }

    /// Run `src` as a classic script whose dynamic imports resolve against
    /// the fixture directory; the event loop drains before returning.
    fn script(&self, src: &str) -> String {
        let out = zipp_vm::run_with_base(src, Some(self.0.clone())).expect("script compiles");
        assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
        out.output.join("\n")
    }

    /// Run `entry` as an ES-module entry through the loader.
    fn module(&self, entry: &str) -> String {
        let out = zipp_vm::run_module_file(&self.0.join(entry), None).expect("module compiles");
        assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
        out.output.join("\n")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn deferred_back_edge_is_not_a_cycle_edge() {
    // main defers plugin; plugin imports main and throws when triggered.
    // plugin is its own root: its error is plugin's alone — a re-import of
    // main fulfils.
    let fx = Fixture::new(
        "defer-cycle",
        &[
            (
                "main.js",
                "import defer * as ns from \"./plugin.js\";\nexport var x = 1;\nglobalThis.ns = ns;\nglobalThis.log.push(\"main-body\");\n",
            ),
            (
                "plugin.js",
                "import \"./main.js\";\nglobalThis.log.push(\"plugin-body\");\nthrow new Error(\"plugin boom\");\n",
            ),
        ],
    );
    let out = fx.script(
        r#"
globalThis.log = [];
import("./main.js").then(function () {
  var t = null;
  try { globalThis.ns.x; } catch (e) { t = e; }
  log.push("trigger:" + (t ? "threw:" + t.message : "ok"));
  return import("./main.js").then(function (m) { log.push("reimport-main:fulfilled x=" + m.x); },
                                  function (e) { log.push("reimport-main:rejected:" + e.message); });
}, function (e) { log.push("first-main:rejected:" + e.message); }).then(function () { console.log(log.join("\n")); });
"#,
    );
    assert_eq!(
        out,
        "main-body\nplugin-body\ntrigger:threw:plugin boom\nreimport-main:fulfilled x=1"
    );
}

#[test]
fn sync_throw_marks_every_module_on_the_stack() {
    // a imports b, b imports a; b's body finishes, a's throws synchronously.
    // b never left the stack (its ancestor index is a's): Evaluate step 9
    // records a's error on b too, so a later import of b rejects with the
    // IDENTICAL error object.
    let fx = Fixture::new(
        "sync-cycle",
        &[
            (
                "a.js",
                "import \"./b.js\";\nglobalThis.log.push(\"a-body\");\nthrow new Error(\"a boom\");\n",
            ),
            (
                "b.js",
                "import \"./a.js\";\nglobalThis.log.push(\"b-body\");\nexport var y = 1;\n",
            ),
        ],
    );
    let out = fx.script(
        r#"
globalThis.log = [];
var e1;
import("./a.js").then(function(){ log.push("a:fulfilled"); }, function(e){ e1 = e; log.push("a:rejected:" + e.message); })
.then(function(){ return import("./b.js").then(function(){ log.push("b:fulfilled"); }, function(e){ log.push("b:rejected:" + e.message + " identical:" + (e === e1)); }); })
.then(function(){ console.log(log.join("\n")); });
"#,
    );
    assert_eq!(
        out,
        "b-body\na-body\na:rejected:a boom\nb:rejected:a boom identical:true"
    );
}

#[test]
fn cycle_root_error_wins_over_a_members_own_rejection() {
    // {a, b, c} with root a; b rejects first (errB), then c (errC). The
    // root's [[EvaluationError]] is errB and stays: every member — c
    // included, despite its own errC — re-imports with errB.
    let fx = Fixture::new(
        "two-errors",
        &[
            (
                "a.js",
                "import \"./b.js\";\nimport \"./c.js\";\nglobalThis.log.push(\"a-body\");\n",
            ),
            (
                "b.js",
                "import \"./a.js\";\nawait 0;\nthrow new Error(\"errB\");\n",
            ),
            (
                "c.js",
                "import \"./a.js\";\nawait 0;\nthrow new Error(\"errC\");\n",
            ),
        ],
    );
    let out = fx.script(
        r#"
globalThis.log = [];
var e1;
function rec(tag, p) { return p.then(function(){ log.push(tag + ":fulfilled"); }, function(e){ log.push(tag + ":rejected:" + e.message + " identical:" + (e === e1)); }); }
import("./a.js").then(function(){ log.push("a:fulfilled"); }, function(e){ e1 = e; log.push("a:rejected:" + e.message); })
.then(function(){ return rec("c", import("./c.js")); })
.then(function(){ return rec("b", import("./b.js")); })
.then(function(){ return rec("a2", import("./a.js")); })
.then(function(){ console.log(log.join("\n")); });
"#,
    );
    assert_eq!(
        out,
        "a:rejected:errB\nc:rejected:errB identical:true\nb:rejected:errB identical:true\na2:rejected:errB identical:true"
    );
}

#[test]
fn fulfilled_member_of_an_errored_async_cycle_rejects_with_the_root_error() {
    // The test262 shape: main → b, x; b → c → a → b (root b, top-level await
    // in each); x → a. b throws after its await; a and c finished cleanly.
    // A later import of c redirects to root b and rejects with the SAME
    // error the import of main rejected with.
    let fx = Fixture::new(
        "errored-cycle",
        &[
            ("main.js", "import \"./b.js\";\nimport \"./x.js\";\n"),
            ("a.js", "import \"./b.js\";\nawait Promise.resolve(0);\n"),
            (
                "b.js",
                "import \"./c.js\";\nawait Promise.resolve(0);\nthrow new Error(\"async error in B\");\n",
            ),
            ("c.js", "import \"./a.js\";\nawait Promise.resolve(0);\n"),
            ("x.js", "import \"./a.js\";\nawait Promise.resolve(0);\n"),
        ],
    );
    let out = fx.script(
        r#"
globalThis.log = [];
var e1;
import("./main.js").then(function(){ log.push("main:fulfilled"); }, function(e){ e1 = e; log.push("main:rejected:" + e.message); })
.then(function(){ return import("./c.js").then(function(){ log.push("c:fulfilled"); }, function(e){ log.push("c:rejected:" + e.message + " identical:" + (e === e1)); }); })
.then(function(){ console.log(log.join("\n")); });
"#,
    );
    assert_eq!(
        out,
        "main:rejected:async error in B\nc:rejected:async error in B identical:true"
    );
}

#[test]
fn async_dependency_walk_stops_at_an_evaluating_module() {
    // main → s → setup, t (top-level await), i; i defers d; d → s. When i
    // links, s is EVALUATING: GatherAsynchronousTransitiveDependencies(d)
    // stops at s without walking ITS requests, so t is not an extra
    // dependency of i — i runs before t settles: I, T, S.
    let fx = Fixture::new(
        "through-stack",
        &[
            ("main.mjs", "import \"./s.js\";\nconsole.log(log.join(\",\"));\n"),
            (
                "s.js",
                "import \"./setup.js\";\nimport \"./t.js\";\nimport \"./i.js\";\nglobalThis.log.push(\"S\");\n",
            ),
            ("setup.js", "globalThis.log = [];\n"),
            ("t.js", "await 0; await 0; await 0;\nglobalThis.log.push(\"T\");\n"),
            (
                "i.js",
                "import defer * as ns from \"./d.js\";\nglobalThis.log.push(\"I\");\n",
            ),
            (
                "d.js",
                "import \"./s.js\";\nexport var z = 1;\nglobalThis.log.push(\"D\");\n",
            ),
        ],
    );
    assert_eq!(fx.module("main.mjs"), "I,T,S");
}

#[test]
fn deferred_walk_waits_for_a_pending_cycle_root_reached_through_a_member() {
    // The test262 import-defer shape: a (top-level await) ↔ b, root a;
    // middle defers d, d → b. b is EVALUATED but its root a is still
    // pending: middle waits for the whole component, and the trigger of d
    // then only evaluates d.
    let fx = Fixture::new(
        "defer-pending-root",
        &[
            (
                "setup.js",
                "globalThis.evaluations = [];\nexport const blocker = Promise.withResolvers();\nexport const aStarted = Promise.withResolvers();\n",
            ),
            (
                "a.js",
                "import { blocker, aStarted } from \"./setup.js\";\nimport \"./b.js\";\nglobalThis.evaluations.push(\"A-before-await\");\naStarted.resolve();\nawait blocker.promise;\nglobalThis.evaluations.push(\"A-after-await\");\n",
            ),
            ("b.js", "import \"./a.js\";\nglobalThis.evaluations.push(\"B\");\n"),
            (
                "c.js",
                "import \"./middle.js\";\nimport \"./resolve-blocker.js\";\nglobalThis.evaluations.push(\"C\");\n",
            ),
            ("d.js", "import \"./b.js\";\nglobalThis.evaluations.push(\"D\");\nexport var z = 1;\n"),
            (
                "middle.js",
                "import defer * as nsD from \"./d.js\";\nglobalThis.evaluations.push(\"Middle-before-nsD.z\");\nnsD.z;\nglobalThis.evaluations.push(\"Middle-after-nsD.z\");\n",
            ),
            (
                "resolve-blocker.js",
                "import { blocker } from \"./setup.js\";\nglobalThis.evaluations.push(\"resolve-blocker\");\nblocker.resolve();\n",
            ),
            (
                "main.mjs",
                "import { aStarted } from \"./setup.js\";\nconst pA = import(\"./a.js\");\nawait aStarted.promise;\nconst pC = import(\"./c.js\");\nawait Promise.all([pA, pC]);\nconsole.log(globalThis.evaluations.join(\",\"));\n",
            ),
        ],
    );
    assert_eq!(
        fx.module("main.mjs"),
        "B,A-before-await,resolve-blocker,A-after-await,Middle-before-nsD.z,D,Middle-after-nsD.z,C"
    );
}

#[test]
fn imports_keep_working_after_an_unrelated_failure() {
    // Once any module has failed, a cache hit of an unrelated module must
    // still fulfil (and a static link over cached dependencies still link)
    // — the errored-component test is a recorded-root lookup, not a graph
    // search over the failure set.
    let fx = Fixture::new(
        "unrelated-failure",
        &[
            ("m0.js", "export var m0 = 1;\n"),
            ("m1.js", "export var m1 = 1;\n"),
            ("m2.js", "import \"./m1.js\";\nexport var m2 = 1;\n"),
            ("bad.js", "throw new Error(\"bad\");\n"),
            (
                "top.js",
                "import \"./m0.js\";\nimport \"./m1.js\";\nimport \"./m2.js\";\nexport var top = 1;\n",
            ),
        ],
    );
    let out = fx.script(
        r#"
globalThis.log = [];
import("./m0.js").then(function(){ return import("./bad.js").then(function(){ log.push("bad:fulfilled"); }, function(e){ log.push("bad:" + e.message); }); })
.then(function(){ return import("./bad.js").then(function(){ log.push("bad2:fulfilled"); }, function(e){ log.push("bad2:" + e.message); }); })
.then(function(){ return import("./m0.js"); }).then(function(m){ log.push("m0:" + m.m0); })
.then(function(){ return import("./m0.js"); }).then(function(m){ log.push("m0 again:" + m.m0); })
.then(function(){ return import("./top.js"); }).then(function(m){ log.push("top:" + m.top); })
.then(function(){ console.log(log.join("\n")); });
"#,
    );
    assert_eq!(out, "bad:bad\nbad2:bad\nm0:1\nm0 again:1\ntop:1");
}
