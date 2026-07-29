//! Only the extras that can name an ELEMENT disqualify an array's dense fast
//! paths — and an out-of-range JIT'd read consults the prototype chain.
//!
//! ~15 fast paths (`map`/`filter`/`indexOf`/`for…of`/`JSON.stringify`, the JIT's
//! `a[i]` helper, the region pin) asked "can an element of this array be
//! shadowed?" and answered it with "does `arr_props` hold ANY entry?". A RegExp
//! match result always holds one — it is where `index`/`input`/`groups` live —
//! so every `exec` result in a program fell off all of them, and every JIT'd
//! `m[i]` deopted to the interpreter. `ObjMap::overlays_elements` asks the
//! precise question instead: is there a canonical index key or `"length"`, or
//! has an integrity level been applied?
//!
//! Narrowing that gate exposed a pre-existing JIT/interpreter divergence, fixed
//! in the same change: `jit_get_index` returned `undefined` for an out-of-range
//! index without walking the prototype chain, so a hot `a[5]` read `undefined`
//! where `Array.prototype[5] = "P"` while the interpreter and node read `"P"`.
//! Match results were accidentally immune because they always deopted.
//!
//! See PERF_ROADMAP B63.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Long enough to cross `OSR_THRESHOLD` and run the region-compiled body.
const HOT: &str = "for (var _k = 0; _k < 100000; _k++)";

#[test]
fn match_result_elements_behave_exactly_like_a_plain_array() {
    let out = run_ok(
        r#"
        var m = /(\d+)-(\w+)/.exec("id 42-abc tail");
        var p = ["42-abc", "42", "abc"];
        console.log([
          m.length === p.length,
          m.map(String).join(",") === p.map(String).join(","),
          m.filter(function (x) { return x.length > 2; }).join(",") === p.filter(function (x) { return x.length > 2; }).join(","),
          m.indexOf("42") === p.indexOf("42"),
          m.includes("abc") === p.includes("abc"),
          m.slice(1).join(",") === p.slice(1).join(","),
          m.join("|") === p.join("|"),
          JSON.stringify(m) === JSON.stringify(p),
          Array.from(m).join(",") === Array.from(p).join(","),
          m.concat(["z"]).join(",") === p.concat(["z"]).join(",")
        ].join(","));
        "#,
    );
    assert_eq!(out[0], "true,true,true,true,true,true,true,true,true,true");
}

/// The metadata must still be there, and still enumerate in the right place.
#[test]
fn narrowing_did_not_drop_the_metadata() {
    let out = run_ok(
        r#"
        var m = /(a)(?<n>b)/d.exec("zab");
        console.log([m.index, m.input, JSON.stringify(m.groups),
                     Object.keys(m).join("+"),
                     Object.getOwnPropertyNames(m).join("+"),
                     JSON.stringify(m.indices)].join(" | "));
        "#,
    );
    assert_eq!(
        out[0],
        "1 | zab | {\"n\":\"b\"} | 0+1+2+index+input+groups+indices | \
         0+1+2+length+index+input+groups+indices | [[1,3],[1,2],[2,3]]"
    );
}

/// An index key or an integrity level DOES still disqualify the dense paths.
#[test]
fn an_index_override_still_takes_the_abstract_path() {
    let out = run_ok(
        r#"
        var a = [1, 2, 3];
        Object.defineProperty(a, "1", { get: function () { return "GETTER"; }, configurable: true });
        console.log([a[1], a.map(function (x) { return x; }).join(","), a.indexOf("GETTER"),
                     a.join(","), JSON.stringify(a)].join(" | "));
        "#,
    );
    assert_eq!(out[0], "GETTER | 1,GETTER,3 | 1 | 1,GETTER,3 | [1,\"GETTER\",3]");
}

#[test]
fn a_frozen_array_still_takes_the_abstract_path() {
    let out = run_ok(
        r#"
        var a = [1, 2, 3];
        Object.freeze(a);
        var threw = "no";
        try { a.fill(0); } catch (e) { threw = e.name; }
        console.log([threw, a.join(","), Object.isFrozen(a)].join(" | "));
        "#,
    );
    assert_eq!(out[0], "TypeError | 1,2,3 | true");
}

/// A named-only extras entry must not be mistaken for an element overlay, and a
/// later index write must flip the bit back on.
#[test]
fn the_element_key_bit_tracks_add_and_delete() {
    let out = run_ok(
        r#"
        var a = [1, 2];
        a.tag = "named";
        var before = a.map(function (x) { return x * 2; }).join(",");
        Object.defineProperty(a, "0", { value: 9, writable: false, enumerable: true, configurable: true });
        var during = a.map(function (x) { return x * 2; }).join(",") + "/" + a[0];
        delete a[0];
        var after = a.map(function (x) { return x * 2; }).join(",") + "/" + (0 in a);
        console.log([before, during, after, a.tag].join(" | "));
        "#,
    );
    // `delete a[0]` leaves a HOLE, which `map` copies through as a hole and
    // `join` renders as empty — byte-identical to node.
    assert_eq!(out[0], "2,4 | 18,4/9 | ,4/false | named");
}

/// The divergence the narrowing exposed. Each case must agree with node, and the
/// JIT must agree with the interpreter.
#[test]
fn out_of_range_reads_consult_the_prototype_chain_in_the_jit() {
    let out = run_ok(&format!(
        r#"
        var res = [];
        function get(o, i) {{ var s; {HOT} s = o[i]; return s; }}
        Array.prototype[5] = "AP";
        Object.prototype[9] = "OP";
        res.push(get([1, 2], 5));
        res.push(get([1, 2], 9));
        var b = [3, 4];
        Object.setPrototypeOf(b, {{ 5: "CUSTOM" }});
        res.push(get(b, 5));
        res.push(String(get([1, 2], -1)));
        res.push(String(get([1, 2], 1)));
        res.push(get(/(x)/.exec("x"), 5));
        console.log(res.join(","));
        "#
    ));
    assert_eq!(out[0], "AP,OP,CUSTOM,undefined,2,AP");
}

/// A hot `m[i]` on a match result now stays in the region instead of deopting;
/// what it reads must not change.
#[test]
fn hot_match_result_element_reads_are_unchanged() {
    let out = run_ok(&format!(
        r#"
        var sum = 0, tail = "";
        {HOT} {{
          var m = /(\d)(\d)/.exec("x" + (_k % 10) + "7y");
          sum = (sum + (+m[1]) + (+m[2])) | 0;
          tail = m[0] + ":" + m.index + ":" + (m[9] === undefined);
        }}
        console.log(sum + " " + tail);
        "#
    ));
    assert_eq!(out[0], "1150000 97:1:true");
}

/// An own `constructor` is invisible to `overlays_elements` — it names no
/// element — but ArraySpeciesCreate reads it, so `map`/`filter`/`splice` must
/// stay on the abstract path. Narrowing their gate broke
/// `staging/sm/Array/splice-species-changes-length.js` in both tiers; this is
/// that case, reduced.
#[test]
fn an_own_constructor_keeps_the_species_families_abstract() {
    let out = run_ok(
        r#"
        var seen = [];
        function probe(name, run) {
          var a = [0, 1, 2];
          a.constructor = { [Symbol.species]: function (n) { seen.push(name); return new Array(n); } };
          var r;
          try { r = String(run(a)); } catch (e) { r = e.name; }
          return name + ":" + r;
        }
        var out = [
          probe("splice", function (a) { return Array.prototype.splice.call(a, 0, 1); }),
          probe("map", function (a) { return a.map(function (x) { return x + 1; }); }),
          probe("filter", function (a) { return a.filter(function (x) { return x > 0; }); })
        ];
        console.log(out.join(" | ") + " || species=" + seen.join(","));
        "#,
    );
    assert_eq!(out[0], "splice:0 | map:1,2,3 | filter:1,2 || species=splice,map,filter");
}

/// The exact shape test262 caught: the species callback mutates the receiver and
/// makes `length` non-writable while `splice` is running.
#[test]
fn splice_species_changing_length_still_throws() {
    let out = run_ok(
        r#"
        var array = [];
        array.push(0, 1, 2);
        array.constructor = {
          [Symbol.species]: function (n) {
            array.push(3, 4, 5);
            Object.defineProperty(array, "length", { writable: false });
            return new Array(n);
          }
        };
        var threw = "no-throw";
        try { Array.prototype.splice.call(array, 0, 1); } catch (e) { threw = e.name; }
        console.log(threw + " len=" + array.length + " [" + array.join(",") + "]");
        "#,
    );
    assert_eq!(out[0], "TypeError len=6 [1,2,,3,4,5]");
}

/// A hole must keep deopting: its value comes from the prototype chain.
#[test]
fn holes_still_defer_to_the_prototype() {
    let out = run_ok(&format!(
        r#"
        var h = [1, , 3];
        function get(o, i) {{ var s; {HOT} s = o[i]; return s; }}
        var a = String(get(h, 1));
        Array.prototype[1] = "HOLE";
        var b = String(get(h, 1));
        console.log(a + "," + b);
        "#
    ));
    assert_eq!(out[0], "undefined,HOLE");
}
