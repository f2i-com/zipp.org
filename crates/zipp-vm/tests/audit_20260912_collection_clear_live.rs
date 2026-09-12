//! `Map`/`Set` clear must preserve the empty slots seen by an active cursor.
//! Entries added after `clear()` are appended after those slots and remain
//! visible to both collection iterators and `forEach`.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const SOURCE: &str = r#"
  "use strict";

  function churn(tag) {
    var keep = [];
    for (var i = 0; i < 24; i++) {
      keep.push({ tag: tag + i, payload: [i, i + 1, i + 2] });
    }
    return keep.length;
  }

  var map = new Map([["a", { id: "A" }], ["b", { id: "B" }]]);
  var mapIter = map.entries();
  var mapFirst = mapIter.next();
  map.clear();
  churn("map-iterator-");
  map.set("c", { id: "C" });
  var mapSecond = mapIter.next();
  var mapDone = mapIter.next();

  var set = new Set(["a", "b"]);
  var setIter = set.values();
  var setFirst = setIter.next();
  set.clear();
  churn("set-iterator-");
  set.add("c");
  var setSecond = setIter.next();
  var setDone = setIter.next();

  var mapEach = new Map([["a", 1], ["b", 2]]);
  var mapSeen = [];
  mapEach.forEach(function (value, key) {
    churn("map-foreach-");
    mapSeen.push(key + value);
    if (key === "a") {
      mapEach.clear();
      mapEach.set("c", 3);
    }
  });

  var setEach = new Set(["a", "b"]);
  var setSeen = [];
  setEach.forEach(function (value) {
    churn("set-foreach-");
    setSeen.push(value);
    if (value === "a") {
      setEach.clear();
      setEach.add("c");
    }
  });

  // Cross the lazy hash-index threshold, build each index with a lookup, then
  // prove clear invalidates it while keeping a live cursor's positions stable.
  var bigMap = new Map();
  var bigSet = new Set();
  for (var i = 0; i < 64; i++) {
    bigMap.set(i, "v" + i);
    bigSet.add(i);
  }
  bigMap.has(63);
  bigSet.has(63);
  var bigMapIter = bigMap.keys();
  var bigSetIter = bigSet.values();
  bigMapIter.next();
  bigSetIter.next();
  bigMap.clear();
  bigSet.clear();
  churn("large-index-");
  bigMap.set("fresh", { id: "map-fresh" });
  bigSet.add("fresh");
  var bigMapNext = bigMapIter.next();
  var bigSetNext = bigSetIter.next();

  console.log([
    mapFirst.value[0] + mapFirst.value[1].id,
    mapSecond.done,
    mapSecond.value[0] + mapSecond.value[1].id,
    mapDone.done,
    map.size,
    setFirst.value,
    setSecond.done,
    setSecond.value,
    setDone.done,
    set.size,
    mapSeen.join(","),
    setSeen.join(","),
    bigMap.size,
    bigMap.has(63),
    bigMap.has("fresh"),
    bigMap.get("fresh").id,
    bigMapNext.done,
    bigMapNext.value,
    bigSet.size,
    bigSet.has(63),
    bigSet.has("fresh"),
    bigSetNext.done,
    bigSetNext.value
  ].join(":"));
"#;

const WANT: &str =
    "aA:false:cC:true:1:a:false:c:true:1:a1,c3:a,c:1:false:true:map-fresh:false:fresh:1:false:true:false:fresh";

const ROOT_SOURCE: &str = r#"
  "use strict";

  function churn(tag) {
    var keep = [];
    for (var i = 0; i < 24; i++) {
      keep.push({ tag: tag + i, payload: [i, i + 1, i + 2] });
    }
    return keep[23];
  }

  function closable(value, state) {
    return {
      [Symbol.iterator]: function () {
        var stepped = false;
        return {
          next: function () {
            if (stepped) return { done: true };
            stepped = true;
            return { done: false, value: value };
          },
          get return() {
            churn("return-getter-");
            return function () {
              churn("return-call-");
              state.closed++;
              return { done: true };
            };
          }
        };
      }
    };
  }

  var mapSet = Map.prototype.set;
  var mapAdderGets = 0;
  Object.defineProperty(Map.prototype, "set", {
    configurable: true,
    get: function () { mapAdderGets++; churn("map-adder-"); return mapSet; }
  });
  var mapKey = { id: "map-key" };
  var map = new Map([[mapKey, { id: "map-value" }]]);
  Object.defineProperty(Map.prototype, "set", {
    configurable: true, writable: true, value: mapSet
  });

  var setAdd = Set.prototype.add;
  var setAdderGets = 0;
  Object.defineProperty(Set.prototype, "add", {
    configurable: true,
    get: function () { setAdderGets++; churn("set-adder-"); return setAdd; }
  });
  var setValue = { id: "set-value" };
  var set = new Set([setValue]);
  Object.defineProperty(Set.prototype, "add", {
    configurable: true, writable: true, value: setAdd
  });

  var weakMapSet = WeakMap.prototype.set;
  var weakMapAdderGets = 0;
  Object.defineProperty(WeakMap.prototype, "set", {
    configurable: true,
    get: function () { weakMapAdderGets++; churn("weakmap-adder-"); return weakMapSet; }
  });
  var weakMapKey = { id: "weak-map-key" };
  var weakMap = new WeakMap([[weakMapKey, { id: "weak-map-value" }]]);
  Object.defineProperty(WeakMap.prototype, "set", {
    configurable: true, writable: true, value: weakMapSet
  });

  var weakSetAdd = WeakSet.prototype.add;
  var weakSetAdderGets = 0;
  Object.defineProperty(WeakSet.prototype, "add", {
    configurable: true,
    get: function () { weakSetAdderGets++; churn("weakset-adder-"); return weakSetAdd; }
  });
  var weakSetValue = { id: "weak-set-value" };
  var weakSet = new WeakSet([weakSetValue]);
  Object.defineProperty(WeakSet.prototype, "add", {
    configurable: true, writable: true, value: weakSetAdd
  });

  var entry = {};
  Object.defineProperty(entry, "0", {
    get: function () { return { id: "fresh-key" }; }
  });
  Object.defineProperty(entry, "1", {
    get: function () { churn("entry-value-"); return { id: "fresh-value" }; }
  });
  var accessorPair = new Map([entry]).entries().next().value;

  var computedMap = new Map();
  var computedKey = { id: "computed-map-key" };
  var computedMapValue = computedMap.getOrInsertComputed(computedKey, function (key) {
    churn("computed-map-");
    computedMap.set(key, { id: "intermediate" });
    return { id: "computed-map-value" };
  });
  var computedWeakMap = new WeakMap();
  var computedWeakKey = { id: "computed-weak-key" };
  var computedWeakValue = computedWeakMap.getOrInsertComputed(computedWeakKey, function (key) {
    churn("computed-weakmap-");
    computedWeakMap.set(key, { id: "intermediate" });
    return { id: "computed-weak-value" };
  });

  var mapCloseState = { closed: 0 };
  var mapSentinel = { id: "map-sentinel" };
  var throwingEntry = { 0: { id: "throw-key" } };
  Object.defineProperty(throwingEntry, "1", {
    get: function () { churn("map-value-throw-"); throw mapSentinel; }
  });
  var mapCaught;
  try { new Map(closable(throwingEntry, mapCloseState)); }
  catch (error) { mapCaught = error; }

  var setCloseState = { closed: 0 };
  var setSentinel = { id: "set-sentinel" };
  Set.prototype.add = function () { churn("set-add-throw-"); throw setSentinel; };
  var setCaught;
  try { new Set(closable({ id: "set-entry" }, setCloseState)); }
  catch (error) { setCaught = error; }
  Set.prototype.add = setAdd;

  var weakMapCloseState = { closed: 0 };
  var weakMapCaught;
  try { new WeakMap(closable([1, {}], weakMapCloseState)); }
  catch (error) { weakMapCaught = error; }
  var weakSetCloseState = { closed: 0 };
  var weakSetCaught;
  try { new WeakSet(closable(1, weakSetCloseState)); }
  catch (error) { weakSetCaught = error; }

  console.log([
    mapAdderGets, map.get(mapKey).id,
    setAdderGets, set.has(setValue),
    weakMapAdderGets, weakMap.get(weakMapKey).id,
    weakSetAdderGets, weakSet.has(weakSetValue),
    accessorPair[0].id, accessorPair[1].id,
    computedMapValue.id, computedMap.get(computedKey).id,
    computedWeakValue.id, computedWeakMap.get(computedWeakKey).id,
    mapCaught === mapSentinel, mapCloseState.closed,
    setCaught === setSentinel, setCloseState.closed,
    weakMapCaught instanceof TypeError, weakMapCloseState.closed,
    weakSetCaught instanceof TypeError, weakSetCloseState.closed
  ].join(":"));
"#;

const ROOT_WANT: &str =
    "1:map-value:1:true:1:weak-map-value:1:true:fresh-key:fresh-value:computed-map-value:computed-map-value:computed-weak-value:computed-weak-value:true:1:true:1:true:1:true:1";

#[test]
fn audit_20260912_clear_keeps_live_collection_cursors_monotonic() {
    assert_eq!(run_ok(SOURCE), [WANT]);
    assert_eq!(run_ok(ROOT_SOURCE), [ROOT_WANT]);
}

#[test]
fn audit_20260912_clear_live_collection_cursors_match_gc_modes() {
    let exe = std::env::current_exe().expect("test binary path");
    for (name, env) in [
        ("default", &[][..]),
        ("no-nursery", &[("ZIPP_NO_NURSERY", "1")][..]),
        ("gc-stress", &[("ZIPP_GC_STRESS", "1")][..]),
        (
            "gc-stress-no-nursery",
            &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NO_NURSERY", "1")][..],
        ),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args([
            "audit_20260912_clear_keeps_live_collection_cursors_monotonic",
            "--exact",
            "--nocapture",
        ])
        .env_remove("ZIPP_GC_STRESS")
        .env_remove("ZIPP_NO_NURSERY");
        for &(key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} mode failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
