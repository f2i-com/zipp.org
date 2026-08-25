//! Regression coverage for the compact pristine RegExp match-result metadata.
//!
//! The implementation deliberately avoids an ordinary property map until an
//! operation observes or changes property shape. These tests cover both sides
//! of that boundary, including coexistence with a sparse numeric overlay and
//! GC slot reuse.

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
fn pristine_reads_and_presence_checks_keep_all_standard_fields() {
    let out = run_ok(
        r#"
        var m = /(a)(?<n>b)/d.exec("zab");
        var plain = /(a)/.exec("a");
        var all = Array.from("ab".matchAll(/(?<x>.)/dg));
        console.log([
          m.index === 1,
          m.input === "zab",
          m.groups.n === "b",
          m.indices[0].join("-") === "1-3",
          m.indices.groups.n.join("-") === "2-3",
          Object.hasOwn(m, "index"),
          Object.hasOwn(m, "input"),
          Object.hasOwn(m, "groups"),
          Object.hasOwn(m, "indices"),
          "index" in m,
          Reflect.has(m, "groups"),
          m.propertyIsEnumerable("index"),
          Object.hasOwn(plain, "groups"),
          plain.groups === undefined,
          !Object.hasOwn(plain, "indices"),
          all[1].index === 1,
          all[1].groups.x === "b",
          all[1].indices.groups.x.join("-") === "1-2"
        ].join(","));
        "#,
    );
    assert_eq!(
        out,
        ["true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true,true"]
    );
}

#[test]
fn reflection_materializes_descriptors_order_and_a_sparse_overlay() {
    let out = run_ok(
        r#"
        var m = /(a)(?<n>b)/d.exec("zab");
        // This lives in arr_props without forcing a million-element dense Vec.
        // Materialising the compact named fields must merge, not replace, it.
        m[1048576] = "far";
        m.extra = "later";
        var d = Object.getOwnPropertyDescriptor(m, "index");
        console.log([
          d.value === 1 && d.writable && d.enumerable && d.configurable,
          Reflect.ownKeys(m).join("+"),
          Object.keys(m).join("+"),
          m[1048576],
          m.extra,
          m.length
        ].join(" | "));
        "#,
    );
    assert_eq!(
        out,
        [
            "true | 0+1+2+1048576+length+index+input+groups+indices+extra | \
          0+1+2+1048576+index+input+groups+indices+extra | far | later | 1048577"
        ]
    );
}

#[test]
fn mutation_delete_and_redefinition_preserve_ordinary_property_rules() {
    let out = run_ok(
        r#"
        var m = /(a)/.exec("za");
        m.index = 7;
        Object.defineProperty(m, "input", {
          get: function () { return "G"; },
          enumerable: false,
          configurable: true
        });
        delete m.groups;
        m.extra = 9;
        delete m.index;
        m.index = 11;
        var inputDesc = Object.getOwnPropertyDescriptor(m, "input");
        console.log([
          m.index,
          m.input,
          !Object.hasOwn(m, "groups"),
          typeof inputDesc.get,
          inputDesc.enumerable,
          Object.getOwnPropertyNames(m).join("+"),
          Object.keys(m).join("+")
        ].join(" | "));
        "#,
    );
    assert_eq!(
        out,
        [
            "11 | G | true | function | false | 0+1+length+input+extra+index | \
          0+1+extra+index"
        ]
    );
}

#[test]
fn integrity_levels_and_proxy_forwarding_see_real_properties() {
    let out = run_ok(
        r#"
        var a = /(a)/.exec("a");
        Object.preventExtensions(a);
        var prevent = [
          !Object.isExtensible(a),
          Reflect.set(a, "index", 4),
          a.index === 4,
          !Reflect.set(a, "extra", 1),
          !Object.hasOwn(a, "extra")
        ].join(",");

        var b = /(a)/.exec("a");
        Object.seal(b);
        var sealed = [
          Object.isSealed(b),
          !Reflect.deleteProperty(b, "index"),
          Reflect.set(b, "index", 5),
          b.index === 5,
          !Object.getOwnPropertyDescriptor(b, "index").configurable
        ].join(",");

        var c = /(a)/.exec("a");
        Object.freeze(c);
        Reflect.set(c, "0", "x");
        var frozen = [
          Object.isFrozen(c),
          !Reflect.set(c, "index", 8),
          c.index === 0,
          c[0] === "a"
        ].join(",");

        var target = /(?<x>a)/d.exec("za");
        var p = new Proxy(target, {});
        p.index = 7;
        Object.defineProperty(p, "input", { value: "P" });
        delete p.groups;
        var forwarded = [p.index, p.input, Object.hasOwn(p, "groups"), Object.keys(p).join("+")].join(",");

        Object.seal(target);
        var missing = Reflect.ownKeys(target).filter(function (k) { return k !== "index"; });
        var badKeys = new Proxy(target, { ownKeys: function () { return missing; } });
        var keysError = "none";
        try { Reflect.ownKeys(badKeys); } catch (e) { keysError = e.name; }
        var badHas = new Proxy(target, { has: function () { return false; } });
        var hasError = "none";
        try { "index" in badHas; } catch (e) { hasError = e.name; }

        console.log([prevent, sealed, frozen, forwarded, keysError, hasError].join(" | "));
        "#,
    );
    assert_eq!(
        out,
        ["true,true,true,true,true | true,true,true,true,true | \
          true,true,true,true | 7,P,false,0+1+index+input+indices | \
          TypeError | TypeError"]
    );
}

#[test]
fn compact_values_survive_gc_and_recycled_slots_do_not_leak() {
    let out = run_ok(
        r#"
        var keep = /(?<x>a)(b)/d.exec("zab");
        for (var i = 0; i < 200; i++) {
          /(?<gone>q)/d.exec("q");
          var junk = { i: i };
        }
        var plain = ["plain"];
        console.log([
          keep.index === 1,
          keep.input === "zab",
          keep.groups.x === "a",
          keep.indices[0].join("-") === "1-3",
          keep.indices.groups.x.join("-") === "1-2",
          Object.keys(keep).join("+") === "0+1+2+index+input+groups+indices",
          plain.index === undefined,
          !Object.hasOwn(plain, "index")
        ].join(","));
        "#,
    );
    assert_eq!(out, ["true,true,true,true,true,true,true,true"]);
}
