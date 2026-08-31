//! Pure-interpreter semantic coverage for the present dense-Array + Int
//! `GetIndex` leaf.

use std::process::Command;

const SOURCE: &str = r#"
function show(value) {
  if (value === undefined) return "undefined";
  if (value !== value) return "NaN";
  if (value === 0 && 1 / value === -Infinity) return "-0";
  return String(value);
}

// A side-table-free ordinary Array is the intended hot path. Include a
// present `undefined` slot so it cannot be confused with the internal HOLE.
var hot = [1, 2, 3, 4];
var hotSum = 0;
for (var i = 0; i < 200000; i++) hotSum = (hotSum + hot[i & 3]) | 0;
var presentUndefined = [undefined];
console.log("hot", hotSum, 0 in presentUndefined, show(presentUndefined[0]));

// Any arr_props side table declines wholesale. The first case is an ordinary
// named property; the next two make the exclusion observable because the
// authoritative indexed value/getter differs from the dense placeholder.
var named = [7];
named.extra = 8;
console.log("named", named[0], named.extra);

var dataOverride = [11];
Object.defineProperty(dataOverride, "0", {
  value: 73, writable: false, enumerable: true, configurable: true
});
console.log("data", dataOverride[0]);

var getterHits = 0;
var accessorOverride = [13];
Object.defineProperty(accessorOverride, "0", {
  configurable: true,
  get: function () { getterHits++; return 80 + getterHits; }
});
console.log("accessor", accessorOverride[0], accessorOverride[0], getterHits);

// A present own element wins even on a custom chain, while a HOLE must retain
// the inherited getter lookup.
var protoHits = 0;
var customProto = {};
Object.defineProperty(customProto, "1", {
  get: function () { protoHits++; return 91; }
});
var holey = [10, , 30];
Object.setPrototypeOf(holey, customProto);
console.log("proto", holey[0], holey[1], holey[2], protoHits);

// A sloppy mapped arguments element aliases the live formal, not necessarily
// the dense escape snapshot.
function mapped(first, second) {
  var args = arguments;
  first = 41;
  second = 42;
  return args[0] + ":" + args[1];
}
console.log("mapped", mapped(5, 6));

// Integrity metadata and RegExp result metadata also create arr_props entries;
// both remain on the generic path. A Proxy is not an Array heap kind at all.
var frozen = Object.freeze([8, 9]);
var match = /(a)(b)/.exec("zabz");
var proxyHits = 0;
var proxied = new Proxy([4], {
  get: function (target, key) {
    if (key === "0") proxyHits++;
    return target[key];
  }
});
console.log("exotic", frozen[1], match[0], match[1], match.index,
            proxied[0], proxyHits);

// Non-Int keys keep all ordinary ToPropertyKey and numeric-index behavior,
// including observable object-key coercion and the preserved -0 double.
var keyed = [10, 20, 30];
keyed[2147483648] = 55;
var events = [];
var objectKey = { toString: function () { events.push("toString"); return "2"; } };
console.log("keys", keyed["1"], keyed[-0], show(keyed[1.5]),
            show(keyed[-1]), keyed[2147483648], keyed[objectKey], events.join("|"));
"#;

fn node_output() -> Vec<String> {
    let out = Command::new("node")
        .arg("-e")
        .arg(SOURCE)
        .output()
        .expect("node on PATH");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn interp_dense_int_get_semantic_worker() {
    if std::env::var_os("ZIPP_INTERP_DENSE_INT_GET_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    assert_eq!(out.output, node_output());
}

#[test]
fn interp_dense_int_get_and_gc_match_node() {
    if std::env::var_os("ZIPP_INTERP_DENSE_INT_GET_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, gc_stress) in [("fast", false), ("fast-gc-stress", true)] {
        let mut command = Command::new(&exe);
        command
            .args([
                "interp_dense_int_get_semantic_worker",
                "--exact",
                "--nocapture",
            ])
            .env("ZIPP_INTERP_DENSE_INT_GET_WORKER", "1")
            .env("ZIPP_NOJIT", "1")
            .env_remove("ZIPP_GC_STRESS");
        if gc_stress {
            command.env("ZIPP_GC_STRESS", "1");
        }
        let out = command.output().expect("spawn semantic worker");
        assert!(
            out.status.success(),
            "{mode} interpreter mode diverged:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
