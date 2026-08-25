//! GC graph regressions for function-internal values stored in keyed side tables.
//! The structural collectability/barrier cases live beside the collector in
//! `vm/gc.rs`; this integration case exercises real object-method bytecode and
//! observable `super` semantics after repeated collections.

#[test]
fn extracted_object_method_keeps_home_for_super_through_gc() {
    let out = zipp_vm::run(
        r#"
        "use strict";
        const proto = { answer: 42 };
        let home = {
          __proto__: proto,
          read() { return super.answer; }
        };
        const read = home.read;
        home = null;

        // The extracted method is the only route to its [[HomeObject]]. Churn
        // enough fresh objects to run several collections before invoking it.
        let sink = 0;
        for (let i = 0; i < 180000; i++) {
          const junk = { i, next: i + 1 };
          sink = (sink ^ junk.i ^ junk.next) | 0;
        }
        console.log(read(), sink);
        "#,
    )
    .expect("source compiles");

    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    assert_eq!(out.output, vec!["42 180000"]);
}

#[test]
fn escaping_arrow_keeps_lexical_new_target_through_gc() {
    let out = zipp_vm::run(
        r#"
        "use strict";
        let Maker = function Maker() {
          return () => new.target.marker;
        };
        Maker.marker = 73;

        // Returning the arrow from [[Construct]] leaves the arrow's lexical
        // new.target edge as the only route back to the constructor object.
        const readMarker = new Maker();
        Maker = null;

        let sink = 0;
        for (let i = 0; i < 100000; i++) {
          const junk = { i, next: i + 1 };
          sink = (sink ^ junk.i ^ junk.next) | 0;
        }
        console.log(readMarker(), sink);
        "#,
    )
    .expect("source compiles");

    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    assert_eq!(out.output, vec!["73 100000"]);
}

#[test]
fn extracted_nested_arrow_keeps_object_method_home_through_gc() {
    let out = zipp_vm::run(
        r#"
        "use strict";
        const proto = { answer: 42 };
        let home = {
          __proto__: proto,
          makeReader() {
            const outer = () => {
              const inner = () => super.answer;
              return inner;
            };
            return outer();
          }
        };

        // `inner` inherits [[HomeObject]] through `outer`; after this assignment
        // it is the only route to the literal that owns makeReader.
        const read = home.makeReader();
        home = null;

        let sink = 0;
        for (let i = 0; i < 100000; i++) {
          const junk = { i, next: i + 1 };
          sink = (sink ^ junk.i ^ junk.next) | 0;
        }
        console.log(read(), sink);
        "#,
    )
    .expect("source compiles");

    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    assert_eq!(out.output, vec!["42 100000"]);
}
