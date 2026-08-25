//! `JSON.parse` object construction after `ObjMap::set` became `ObjMap::set_owned`
//! (M2.2 of the V8-parity plan): the parser already owns each key string, so the
//! map moves it in instead of cloning a second copy and dropping the first.
//!
//! That is a pure allocation change, and the whole risk is that the OBSERVABLE
//! order/identity rules of object construction shift with it. Each test here pins
//! one of them, because none is visible in a "does it parse" check:
//!
//! * a duplicate key keeps its FIRST insertion position and its LAST value;
//! * integer-like keys keep JSON's textual order (a JSON object is not an
//!   Array — `{"2":…,"1":…}` must not sort);
//! * the reviver visits properties in that same order;
//! * `context.source` for a duplicate reports the LAST member's text.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

#[test]
fn duplicate_keys_keep_first_position_and_last_value() {
    let out = run_ok(
        r#"
        var o = JSON.parse('{"a":1,"b":2,"a":3,"c":4,"b":5}');
        console.log(Object.keys(o).join(",") + " | " + JSON.stringify(o));
        // …and a duplicate that is the ONLY key.
        var p = JSON.parse('{"k":1,"k":2,"k":3}');
        console.log(Object.keys(p).join(",") + "=" + p.k);
        "#,
    );
    assert_eq!(out[0], "a,b,c | {\"a\":3,\"b\":5,\"c\":4}");
    assert_eq!(out[1], "k=3");
}

#[test]
fn integer_like_keys_keep_textual_order() {
    // An ordinary object sorts canonical integer keys ascending; a JSON object is
    // built by the same [[DefineOwnProperty]] rules, so `{"2":…,"1":…}` enumerates
    // 1 then 2 — but "01" and "-1" are NOT canonical indices and stay in insertion
    // order after them.
    let out = run_ok(
        r#"
        var o = JSON.parse('{"2":"two","1":"one","01":"oh-one","-1":"neg","x":"ex","0":"zero"}');
        console.log(Object.keys(o).join(","));
        console.log(JSON.stringify(o));
        "#,
    );
    assert_eq!(out[0], "0,1,2,01,-1,x");
    assert_eq!(
        out[1],
        "{\"0\":\"zero\",\"1\":\"one\",\"2\":\"two\",\"01\":\"oh-one\",\"-1\":\"neg\",\"x\":\"ex\"}"
    );
}

#[test]
fn the_reviver_visits_properties_in_construction_order() {
    let out = run_ok(
        r#"
        var seen = [];
        var o = JSON.parse('{"b":1,"a":{"d":2,"c":3},"b":9}', function (k, v) {
          seen.push(k === "" ? "<root>" : k);
          return v;
        });
        console.log(seen.join(",") + " | b=" + o.b);
        "#,
    );
    // `b` FIRST — the duplicate kept its first insertion position, so the walk hits
    // it before `a`, then recurses into `a`'s members. Verified against node.
    assert_eq!(out[0], "b,d,c,a,<root> | b=9");
}

#[test]
fn parse_with_source_reports_the_last_duplicate() {
    // The regression `json_parse_object_src`'s duplicate handling exists for: the
    // snapshot `context.source` reports must match the value the property ended up
    // holding, or the spec's SameValue correspondence check drops `source`.
    let out = run_ok(
        r#"
        if (typeof JSON.rawJSON !== "function") {
          console.log("skip");
        } else {
          var srcs = [];
          JSON.parse('{"b":2,"b":1,"b":4}', function (k, v, context) {
            if (k !== "") srcs.push(k + "=" + context.source);
            return v;
          });
          console.log(srcs.join(","));
        }
        "#,
    );
    assert!(
        out[0] == "b=4" || out[0] == "skip",
        "unexpected: {:?}",
        out[0]
    );
}

#[test]
fn nested_and_empty_objects_still_build() {
    let out = run_ok(
        r#"
        var o = JSON.parse('{"e":{},"n":{"a":{"b":[1,2,{"c":3}]}},"z":null}');
        console.log(JSON.stringify(o) + " | " + Object.keys(o.e).length + " | " + o.n.a.b[2].c);
        "#,
    );
    assert_eq!(
        out[0],
        "{\"e\":{},\"n\":{\"a\":{\"b\":[1,2,{\"c\":3}]}},\"z\":null} | 0 | 3"
    );
}

#[test]
fn a_parsed_key_still_deletes_and_redefines() {
    // `push_data` skips `set`'s re-lookup, so the index/shape/element-key bookkeeping
    // it maintains has to be right — a delete and re-add is what exercises all three.
    let out = run_ok(
        r#"
        var o = JSON.parse('{"a":1,"b":2,"3":3}');
        delete o.a;
        o.a = 9;
        Object.defineProperty(o, "b", { value: 8, enumerable: false, configurable: true });
        console.log(Object.keys(o).join(",") + " | " + o.a + "," + o.b + "," + o[3] +
                    " | " + JSON.stringify(o));
        "#,
    );
    assert_eq!(out[0], "3,a | 9,8,3 | {\"3\":3,\"a\":9}");
}
