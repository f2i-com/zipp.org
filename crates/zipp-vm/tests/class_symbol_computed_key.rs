//! A computed class key is an ordinary runtime expression. `Symbol.iterator`
//! may look constant, but resolving `Symbol` and getting `iterator` are both
//! observable and must not be folded by identifier spelling.

use zipp_vm::run;

fn out(src: &str) -> String {
    let outcome = run(src).expect("source compiles");
    assert!(
        outcome.error.is_none(),
        "unexpected throw: {:?}; output: {:?}\nsource:\n{src}",
        outcome.error,
        outcome.output
    );
    outcome.output.join("\n")
}

#[test]
fn lexical_shadow_getter_runs_once_in_class_element_order() {
    let src = r#"
      const NativeSymbol = Symbol;
      const log = [];
      const shadow = {};
      Object.defineProperty(shadow, "iterator", {
        get: function () { log.push("get"); return "shadowIterator"; }
      });
      {
        const Symbol = shadow;
        class C {
          [Symbol.iterator]() { return 17; }
          [(log.push("later"), "later")]() {}
        }
        console.log(
          log.join(","),
          new C().shadowIterator(),
          typeof C.prototype[NativeSymbol.iterator],
          C.prototype.shadowIterator.name
        );
      }
    "#;

    assert_eq!(out(src), "get,later 17 undefined shadowIterator");
}

#[test]
fn throwing_getter_aborts_before_later_elements() {
    let src = r#"
      const log = [];
      const shadow = {};
      Object.defineProperty(shadow, "iterator", {
        get: function () { log.push("get"); throw "boom"; }
      });
      let afterClass = false;
      let caught = false;
      try {
        const Symbol = shadow;
        class C {
          [Symbol.iterator]() {}
          [(log.push("later"), "later")]() {}
        }
        afterClass = true;
      } catch (e) {
        caught = e === "boom";
      }
      console.log(log.join(","), caught, afterClass);
    "#;

    assert_eq!(out(src), "get true false");
}

#[test]
fn lexical_tdz_is_not_bypassed_by_symbol_spelling() {
    let src = r#"
      let afterClass = false;
      let caught = false;
      try {
        {
          class C {
            [Symbol.iterator]() {}
          }
          afterClass = true;
          let Symbol;
        }
      } catch (e) {
        caught = true;
      }
      console.log(caught, afterClass);
    "#;

    assert_eq!(out(src), "true false");
}

#[test]
fn global_symbol_rebinding_is_observed_per_class_evaluation() {
    let src = r#"
      const NativeSymbol = Symbol;
      const first = { iterator: "first" };
      const second = { iterator: "second" };

      Symbol = first;
      class A { [Symbol.iterator]() { return "A"; } }
      Symbol = second;
      class B { [Symbol.iterator]() { return "B"; } }
      Symbol = NativeSymbol;

      console.log(
        new A().first(),
        new B().second(),
        typeof A.prototype[NativeSymbol.iterator],
        typeof B.prototype[NativeSymbol.iterator]
      );
    "#;

    assert_eq!(out(src), "A B undefined undefined");
}

#[test]
fn intrinsic_iterator_and_to_primitive_keep_symbol_keys_and_names() {
    let src = r#"
      class C {
        [Symbol.iterator]() { return 23; }
        [Symbol.toPrimitive](hint) { return hint === "number" ? 7 : "C"; }
      }
      const c = new C();
      console.log(
        c[Symbol.iterator](),
        +c,
        C.prototype[Symbol.iterator].name,
        C.prototype[Symbol.toPrimitive].name
      );
    "#;

    assert_eq!(out(src), "23 7 [Symbol.iterator] [Symbol.toPrimitive]");
}

#[test]
fn shadowed_symbol_keys_work_for_instance_and_static_fields() {
    let src = r#"
      const NativeSymbol = Symbol;
      const log = [];
      const shadow = {};
      Object.defineProperty(shadow, "iterator", {
        get: function () { log.push("instance"); return "instanceField"; }
      });
      Object.defineProperty(shadow, "toPrimitive", {
        get: function () { log.push("static"); return "staticField"; }
      });
      {
        const Symbol = shadow;
        class C {
          [Symbol.iterator] = 31;
          static [Symbol.toPrimitive] = 41;
        }
        const c = new C();
        console.log(
          log.join(","),
          c.instanceField,
          C.staticField,
          typeof c[NativeSymbol.iterator],
          typeof C[NativeSymbol.toPrimitive]
        );
      }
    "#;

    assert_eq!(out(src), "instance,static 31 41 undefined undefined");
}
