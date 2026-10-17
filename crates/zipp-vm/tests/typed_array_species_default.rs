//! TypedArraySpeciesCreate when the species is the default: `subarray`,
//! `slice`, `map` and `filter` on a typed array whose species chain is
//! untouched skip the `constructor` and `@@species` reads, and one whose
//! intrinsic constructor is found builds the default view directly. Each
//! link of the chain changed in turn (an own `constructor`, the prototype's
//! `constructor`, an own `@@species`, %TypedArray%'s `@@species`, the
//! instance's and the constructor's prototypes, a `constructor` getter that
//! counts its reads) must be observed exactly as Node observes it, as must
//! the default path's own checks (bounds, a detached buffer, a subclass).

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function catchName(f) { try { return f(); } catch (e) { return e.constructor.name; } }
function show(ta) {
  return Object.getPrototypeOf(ta) === Uint8Array.prototype ? "u8" :
    Object.getPrototypeOf(ta) === Int16Array.prototype ? "i16" :
    Object.getPrototypeOf(ta) === Float64Array.prototype ? "f64" :
    (ta && ta.constructor && ta.constructor.name) || typeof ta;
}
function run(tag, ta) {
  var sub = ta.subarray(1, 3), sl = ta.slice(1, 3);
  var mp = ta.map(function (x) { return x * 2; }), fl = ta.filter(function (x) { return x & 1; });
  log(tag, show(sub), sub.length, sub.byteOffset, sub.buffer === ta.buffer, Array.from(sub).join(","),
    show(sl), Array.from(sl).join(","), show(mp), Array.from(mp).join(","), show(fl), Array.from(fl).join(","));
}
var base = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
run("default", base);
run("default-i16", new Int16Array([5, -6, 7, -8]));
run("default-f64", new Float64Array([0.5, 1.5, 2.5, 3.5]));
run("offset", new Uint8Array(base.buffer, 2, 5));
log("sub-tracking", base.subarray(6).length, base.subarray(-3, -1).join(","), base.subarray(5, 2).length);
log("sub-bounds", catchName(function () { return base.subarray({ valueOf: function () { return 2; } }, 4).join(","); }));

// A loop over the default path: the species chain is read every time.
var acc = 0;
for (var i = 0; i < 2000; i++) { var s = base.subarray(i & 3, (i & 3) + 4); acc = (acc + s[0] + s.length + s.byteOffset) | 0; }
log("loop", acc);

// An own `constructor` on the instance.
var own = new Uint8Array([9, 8, 7, 6]);
var made = [];
function Mine(a, b, c) { made.push(arguments.length); return new Uint8Array(a, b, c); }
Mine[Symbol.species] = Mine;
own.constructor = Mine;
run("own-ctor", own);
log("own-made", made.join(","));

// The prototype's `constructor` replaced, then a getter that counts.
var saved = Uint8Array.prototype.constructor;
Uint8Array.prototype.constructor = undefined;
run("proto-ctor-undefined", new Uint8Array([4, 3, 2, 1]));
var reads = 0;
Object.defineProperty(Uint8Array.prototype, "constructor", { get: function () { reads++; return saved; }, configurable: true });
run("proto-ctor-getter", new Uint8Array([1, 3, 5, 7]));
log("getter-reads", reads);
Object.defineProperty(Uint8Array.prototype, "constructor", { value: saved, writable: true, configurable: true });
run("restored", base);

// An own @@species on the constructor, then on %TypedArray%.
var calls = 0;
Object.defineProperty(Uint8Array, Symbol.species, { get: function () { calls++; return Int16Array; }, configurable: true });
log("own-species", catchName(function () { return show(base.subarray(0, 2)); }), show(base.slice(0, 2)), calls);
delete Uint8Array[Symbol.species];
var TA = Object.getPrototypeOf(Uint8Array);
var desc = Object.getOwnPropertyDescriptor(TA, Symbol.species);
var tcalls = 0;
Object.defineProperty(TA, Symbol.species, { get: function () { tcalls++; return this; }, configurable: true });
run("ta-species-getter", base);
log("ta-species-calls", tcalls);
Object.defineProperty(TA, Symbol.species, desc);
run("ta-species-restored", base);

// The instance's prototype, then the constructor's.
var moved = new Uint8Array([2, 4, 6, 8]);
Object.setPrototypeOf(moved, Int16Array.prototype);
log("instance-proto", catchName(function () { return show(Uint8Array.prototype.subarray.call(moved, 0, 2)); }),
  catchName(function () { return show(Uint8Array.prototype.slice.call(moved, 0, 2)); }));
Object.setPrototypeOf(Uint8Array, Object.getPrototypeOf(Int16Array));
run("ctor-proto-same", base);
var fakeBase = function () {}; fakeBase[Symbol.species] = Float64Array;
Object.setPrototypeOf(Uint8Array, fakeBase);
log("ctor-proto-changed", show(base.slice(0, 2)), catchName(function () { return show(base.subarray(0, 2)); }));
Object.setPrototypeOf(Uint8Array, TA);
run("ctor-proto-restored", base);

// A subclass: its own species (the subclass) builds the views.
class Bytes extends Uint8Array { sum() { return this.reduce(function (a, b) { return a + b; }, 0); } }
var by = new Bytes([1, 2, 3, 4]);
log("subclass", by.subarray(1, 3) instanceof Bytes, by.slice(0, 2).sum(), by.map(function (x) { return x + 1; }).sum());

// A detached buffer.
var det = new Uint8Array(8);
var buf = det.buffer;
if (typeof buf.transfer === "function") buf.transfer();
log("detached", catchName(function () { return det.subarray(0, 2).length; }), catchName(function () { return det.slice(0, 2).length; }));
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
fn typed_array_species_default_worker() {
    if std::env::var_os("ZIPP_TA_SPECIES_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

#[test]
fn typed_array_species_default_matches_node() {
    if std::env::var_os("ZIPP_TA_SPECIES_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit) in [("default", false), ("interpreter", true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["typed_array_species_default_worker", "--exact", "--nocapture"])
            .env("ZIPP_TA_SPECIES_WORKER", "1")
            .env_remove("ZIPP_NOJIT");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
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
