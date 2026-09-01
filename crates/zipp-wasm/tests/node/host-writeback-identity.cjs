// A host that mirrors state reads the globals and writes them back. That must
// not damage what it could not represent.
//
// The rule already existed for functions: host_set_slot refuses an opaque slot,
// and host_in_over keeps an opaque property the host echoed back as null, and
// even a key the host dropped entirely. Then it allocated a fresh PLAIN object
// for the result, so a class instance lost the class its methods are resolved
// through — every field preserved, every method gone:
//
//     counter.next()  ->  1
//     (host reads the globals and writes the same values back)
//     counter.next()  ->  TypeError: undefined is not a function (property "next")
//
// Nothing unusual is needed to trigger it. That read/modify/write is what the
// sync does every tick.
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0, fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}

function engine() {
  const e = new Engine();
  const syms = e.initScript(`
    class Counter {
      constructor() { this.n = 0 }
      next() { this.n = this.n + 1; return this.n }
    }
    class Other { constructor() { this.z = 0 } }
    let counter = new Counter()
    let plain = { a: 1 }
    let fn = function () { return 7 }
    let nested = { inner: new Counter(), tag: "t" }
    let frozenish = [1, 2, 3]

    function bump()      { return counter.next() }
    function shape()     { return typeof counter + "/" + (typeof counter.next) }
    function isCounter() { return counter instanceof Counter }
    function isOther()   { return counter instanceof Other }
    function callFn()    { return fn() }
    function bumpInner() { return nested.inner.next() }
    function innerShape(){ return typeof nested.inner.next }
    function plainProto(){ return plain instanceof Counter }
  `);
  if (syms && syms.error) throw new Error("initScript: " + syms.error);
  return { e, syms };
}

// Read every global and write the identical values straight back.
function roundTrip(e, syms, names) {
  const idx = names.map((n) => syms[n].index);
  const read = e.getGlobalsBatch(idx);
  e.setGlobalsBatch(idx, read);
  return read;
}

const NAMES = ["counter", "plain", "fn", "nested", "frozenish"];

// 1. THE ONE THAT MATTERS. Methods survive a mirror-and-write-back.
{
  const { e, syms } = engine();
  e.renewInstructionBudget();
  check("the instance works before any host traffic", e.callFunction("bump", []) === 1);
  roundTrip(e, syms, NAMES);
  e.renewInstructionBudget();
  check("it still has its methods afterwards", e.callFunction("shape", []) === "object/function",
    String(e.callFunction("shape", [])));
  let after;
  try { after = e.callFunction("bump", []); } catch (err) { after = "threw: " + String(err && err.message ? err.message : err); }
  check("and calling one continues the state", after === 2, String(after));
  e.dispose();
}

// 2. instanceof is the other thing the class carries, and it must not start lying.
{
  const { e, syms } = engine();
  e.renewInstructionBudget();
  e.callFunction("bump", []);
  roundTrip(e, syms, NAMES);
  e.renewInstructionBudget();
  check("instanceof its own class still holds", e.callFunction("isCounter", []) === true);
  check("instanceof an unrelated class is still false", e.callFunction("isOther", []) === false);
  check("a plain object did not acquire a class", e.callFunction("plainProto", []) === false);
  e.dispose();
}

// 3. Nested instances go through the same merge, one level down.
{
  const { e, syms } = engine();
  e.renewInstructionBudget();
  check("a nested instance works first", e.callFunction("bumpInner", []) === 1);
  roundTrip(e, syms, NAMES);
  e.renewInstructionBudget();
  check("a nested instance keeps its methods", e.callFunction("innerShape", []) === "function",
    String(e.callFunction("innerShape", [])));
  let inner;
  try { inner = e.callFunction("bumpInner", []); } catch (err) { inner = "threw: " + String(err && err.message ? err.message : err); }
  check("and it continues counting", inner === 2, String(inner));
  e.dispose();
}

// 4. The protection that already worked must keep working.
{
  const { e, syms } = engine();
  roundTrip(e, syms, NAMES);
  e.renewInstructionBudget();
  check("a function global is still callable", e.callFunction("callFn", []) === 7);
  e.dispose();
}

// 5. The host must still be able to actually CHANGE data. Preserving identity
//    is not the same as ignoring the write.
{
  const { e, syms } = engine();
  e.renewInstructionBudget();
  e.callFunction("bump", []);
  const i = syms.counter.index;
  e.setGlobalsBatch([i], [{ n: 41 }]);
  e.renewInstructionBudget();
  check("a host write to a field lands", e.callFunction("bump", []) === 42,
    "expected 42");
  check("and the method that read it is still there", e.callFunction("shape", []) === "object/function");
  e.dispose();
}

// 6. The rule has to recurse through ARRAYS, not just objects. It did not, so
//    everything below the first array was rebuilt from the host's projection.
{
  const e = new Engine();
  const syms = e.initScript(`
    class C { m() { return "inst" } }
    let underArray = [ function () { return "arr-fn" } ]
    let deep       = { list: [ { fn: function () { return "deep-fn" } } ] }
    let instInArr  = [ new C() ]
    let mixed      = [ 1, function () { return "f" }, "three" ]
    function arr()   { return typeof underArray[0] === "function" ? underArray[0]() : "GONE" }
    function deepf() { return typeof deep.list[0].fn === "function" ? deep.list[0].fn() : "GONE" }
    function inst()  { return typeof instInArr[0].m === "function" ? instInArr[0].m() : "GONE" }
    function mix()   { return String(mixed[0]) + "/" + (typeof mixed[1] === "function" ? mixed[1]() : "GONE") + "/" + String(mixed[2]) }
  `);
  if (syms && syms.error) throw new Error(syms.error);
  const names = ["underArray", "deep", "instInArr", "mixed"];
  const idx = names.map((n) => syms[n].index);
  e.setGlobalsBatch(idx, e.getGlobalsBatch(idx));
  e.renewInstructionBudget();
  check("a function directly inside an array survives", e.callFunction("arr", []) === "arr-fn",
    String(e.callFunction("arr", [])));
  check("a function under object -> array -> object survives", e.callFunction("deepf", []) === "deep-fn",
    String(e.callFunction("deepf", [])));
  check("an instance inside an array keeps its class", e.callFunction("inst", []) === "inst",
    String(e.callFunction("inst", [])));
  check("ordinary array data around it is untouched", e.callFunction("mix", []) === "1/f/three",
    String(e.callFunction("mix", [])));
  e.dispose();
}

// 7. Properties the host is never shown must not be read as deletions. host_out
//    emits only enumerable, non-accessor properties, so anything else was
//    invisible and its absence carries no intent.
{
  const e = new Engine();
  const syms = e.initScript(`
    let o = { plain: 1 }
    Object.defineProperty(o, "nonEnum", { value: 9, enumerable: false, writable: true })
    Object.defineProperty(o, "getter",  { get: function () { return 5 }, enumerable: true })
    Object.defineProperty(o, "roProp",  { value: 3, enumerable: true, writable: false })
    let err = new Error("boom")
    function probe()   { return [String(o.plain), String(o.nonEnum), String(o.getter), String(o.roProp)].join("|") }
    function writeRo() { o.roProp = 99; return String(o.roProp) }
    function errMsg()  { return String(err.message) }
  `);
  if (syms && syms.error) throw new Error(syms.error);
  const idx = [syms.o.index, syms.err.index];
  const seen = e.getGlobalsBatch(idx)[0];
  check("the host genuinely cannot see them", !("nonEnum" in seen) && !("getter" in seen),
    JSON.stringify(seen));
  e.setGlobalsBatch(idx, e.getGlobalsBatch(idx));
  e.renewInstructionBudget();
  check("a non-enumerable property and an accessor both survive",
    e.callFunction("probe", []) === "1|9|5|3", String(e.callFunction("probe", [])));
  e.renewInstructionBudget();
  check("a read-only property does not become writable",
    e.callFunction("writeRo", []) === "3", String(e.callFunction("writeRo", [])));
  e.renewInstructionBudget();
  check("Error keeps its message, which is non-enumerable",
    e.callFunction("errMsg", []) === "boom", String(e.callFunction("errMsg", [])));
  e.dispose();
}

// 8. Deletion must still work. An ENUMERABLE property the host saw and dropped
//    is a real removal, and preserving the invisible ones must not resurrect it.
{
  const e = new Engine();
  const syms = e.initScript(`
    let o = { keep: 1, drop: 2 }
    function shape() { let k = []; for (const n in o) { k.push(n) } return k.join(",") }
  `);
  if (syms && syms.error) throw new Error(syms.error);
  e.setGlobalsBatch([syms.o.index], [{ keep: 1 }]);
  e.renewInstructionBudget();
  check("a visible property the host omits is still deleted",
    e.callFunction("shape", []) === "keep", String(e.callFunction("shape", [])));
  e.dispose();
}

console.log(`\n${pass} passed, ${fail} failed`);
if (fail > 0) process.exit(1);
