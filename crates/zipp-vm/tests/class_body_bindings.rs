//! Class bodies must instantiate bindings before hoisted closures capture them.
fn output(source: &str) -> Vec<String> {
    let result = zipp_vm::run(source).expect("valid class program");
    assert!(result.error.is_none(), "{:?}", result.error);
    result.output
}

#[test]
fn class_methods_capture_lexicals_before_their_declarations() {
    assert_eq!(
        output(
            r#"
        class C {
            #value = 7;
            method() {
                let self = this;
                function read() { return self.#value; }
                return read();
            }
            static method() {
                const {value} = {value: 9};
                function read() { return value; }
                return read();
            }
            get value() {
                class Local { static value = 11; }
                function read() { return Local.value; }
                return read();
            }
        }
        console.log(new C().method(), C.method(), new C().value);
    "#
        ),
        ["7 9 11"]
    );
}

#[test]
fn constructor_closure_writes_share_hoisted_var_cells() {
    assert_eq!(
        output(
            r#"
        class Base {}
        class Child extends Base {
            constructor() {
                super();
                var called = false;
                function argument() { called = true; return 3; }
                try { super(argument()); } catch (error) {
                    console.log(error instanceof ReferenceError, called);
                }
            }
        }
        new Child();
    "#
        ),
        ["true true"]
    );
}

#[test]
fn class_body_capture_preserves_tdz_and_const() {
    assert_eq!(
        output(
            r#"
        class C {
            method() {
                try { read(); } catch (error) { console.log(error instanceof ReferenceError); }
                const value = 7;
                function read() { return value; }
                function write() { value = 8; }
                console.log(read());
                try { write(); } catch (error) { console.log(error instanceof TypeError); }
            }
        }
        new C().method();
    "#
        ),
        ["true", "7", "true"]
    );
}
