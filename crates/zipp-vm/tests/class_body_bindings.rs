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
fn arrows_see_method_locals_named_like_their_class() {
    // The class's own name is a scope OUTSIDE its methods: a method's local of
    // that name shadows it for every closure inside the method, arrows too.
    // Minifiers name both alike (`class e { parse() { let e = ...; r = () => e.x } }`).
    assert_eq!(
        output(
            r#"
        var A = class e { m() { let e = { v: 1 }; return (() => e.v)(); } };
        var B = class e { m() { let e = { v: 2 }; return (() => () => e.v)()(); } };
        var C = class e { m() { let e = { v: 3 }; return (function () { return e.v; })(); } };
        var D = class e { m() { return (() => typeof e)(); } };
        var E = class e { f = (() => { let e = 5; return () => e; })(); };
        var F = class e { m() { for (let e of [6]) return (() => e)(); } };
        console.log(new A().m(), new B().m(), new C().m(), new D().m(), new E().f(), new F().m());
    "#
        ),
        ["1 2 3 function 5 6"]
    );
}

#[test]
fn static_initializer_arrows_see_the_inner_class_name_through_eval() {
    // An inline static initializer compiles in the function around the class,
    // whose own `C` (the outer binding, made a cell by the eval) is outside
    // the class element: the inner name still wins, even once the outer one
    // is reassigned (test262 staging/sm/class/fields-static-class-name-binding-eval.js).
    // A local of the initializer's own arrows still shadows it.
    assert_eq!(
        output(
            r#"
        class C { static field = () => eval("C"); }
        const D = C; C = null;
        let E = class Inner { static field = () => eval("Inner"); };
        const F = E; E = null;
        class G { static field = () => { let G = 7; return () => eval("G"); }; }
        class H { static field = () => () => eval("H"); }
        const I = H; H = null;
        console.log(D.field() === D, F.field() === F, G.field()(), I.field()() === I);
    "#
        ),
        ["true true 7 true"]
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
