//! `new` of a class that declares private methods or accessors brands the
//! instance, also when the instance is made from the constructor's stores
//! with no frame (`ctor_store_new`): a class with public fields and a
//! private accessor made instances without the brand, so `this.#g` threw.
//! Each class runs once and then hot enough for compiled code; the expected
//! lines are Node's.

const SOURCE: &str = r##"// Instances of classes that declare private methods or accessors carry the
// brand however `new` makes them: once, and hot in a loop (compiled code).
var out = [];
function t(name, f) { try { out.push(name + " " + f()); } catch (e) { out.push(name + " threw " + e.constructor.name + ": " + e.message); } }
function doNotCall() { throw new Error("rhs evaluated"); }
class A { f = 1; get #g() { return 5; } read() { return this.#g + this.f; } }
class B { get #g() { return 5; } read() { return this.#g; } }
class D { f = 1; #m() { return 6; } read() { return this.#m() + this.f; } }
class E { f = 1; #x = 2; get #g() { return this.#x; } read() { return this.#g; } }
class F { f = 1; get #g() { return 5; } constructor(z) { this.z = z; } read() { return this.#g + this.z; } }
class G { constructor(a, b) { this.a = a; this.b = b; } #sum() { return this.a + this.b; } read() { return this.#sum(); } }
class H { constructor(a) { this.a = a * 2 + 1; } static #s() { return 9; } read() { return this.a + H.#s(); } }
class I { setterWasCalled = false; get #field() { return false; } set #field(v) { this.setterWasCalled = true; }
  andA() { return this.#field &&= doNotCall(); } orA() { return (this.#field ||= 3, this.setterWasCalled); } }
class J { constructor(p) { this.p = p; } has(o) { return #k in o; } #k() {} }
t("A", () => new A().read());
t("B", () => new B().read());
t("D", () => new D().read());
t("E", () => new E().read());
t("F", () => new F(4).read());
t("G", () => new G(2, 3).read());
t("H", () => new H(5).read());
t("I and", () => new I().andA());
t("I or", () => new I().orA());
t("J", () => [new J(1).has(new J(2)), new J(1).has({})].join());
var acc = 0;
for (var r = 0; r < 3000; r++) {
  acc += new A().read() + new D().read() + new F(r & 3).read() + new G(r, 1).read() + new H(r & 7).read();
  if (new J(r).has(new J(r + 1))) acc++;
}
t("hot", () => acc);
var objs = [];
for (var r = 0; r < 2000; r++) objs.push(new G(r, 2));
t("kept", () => objs.reduce((s, o) => s + o.read(), 0));
t("foreign", () => { try { return G.prototype.read.call({ a: 1, b: 2 }); } catch (e) { return e.constructor.name; } });
console.log(out.join("\n"));
"##;

const NODE: &str = r##"A 6
B 5
D 7
E 2
F 9
G 5
H 20
I and false
I or true
J true,false
hot 4614000
kept 2003000
foreign TypeError
"##;

#[test]
fn class_private_brand_new_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    let want: Vec<String> = NODE.trim_start_matches('\n').lines().map(str::to_owned).collect();
    assert_eq!(got, want);
}

/// The corpus again with eager JIT, GC stress and the interpreter, each in a
/// fresh process.
#[test]
fn class_private_brand_new_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")] {
        let out = std::process::Command::new(&exe)
            .args(["class_private_brand_new_parity", "--exact"])
            .env(mode.0, mode.1)
            .output()
            .expect("spawn test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("running 0 tests"), "mode filter matched nothing: {stdout}");
    }
}