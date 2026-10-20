//! A Proxy has [[Construct]] only when its target does (ProxyCreate step 7),
//! so `new` on a Proxy around an arrow, a concise method, a generator, an
//! async function or a bound one of those throws a TypeError, after the
//! arguments are evaluated and without reading the handler's `construct`.
//! zipp ran the trap (f2i-com/zipp.org#71). A Proxy whose target is a revoked
//! Proxy of a constructor keeps [[Construct]] and runs its trap. Expected
//! output is Node's (v24).

#[test]
fn new_on_a_proxy_follows_its_targets_construct() {
    let src = r###"const r = [];
const t = (label, f) => { try { r.push(label + ": " + f()); } catch (e) { r.push(label + ": " + e.name); } };
const trap = { construct() { r.push("  trap ran"); return { made: 1 }; } };
t("the issue's arrow with a construct trap", () => new (new Proxy(() => 1, trap))());
t("the issue's arrow without a trap", () => new (new Proxy(() => 1, {}))());
t("the issue's Reflect.construct newTarget", () => Reflect.construct(Object, [], () => {}));
t("a Proxy of a Proxy of an arrow", () => new (new Proxy(new Proxy(() => 1, {}), trap))());
t("a Proxy of a bound arrow", () => new (new Proxy((() => 1).bind(null), trap))());
t("a Proxy of a concise method", () => new (new Proxy({ m() {} }.m, trap))());
t("a Proxy of a generator", () => new (new Proxy(function* () {}, trap))());
t("a Proxy of an async function", () => new (new Proxy(async function () {}, trap))());
t("class extends a Proxy of an arrow", () => { class C extends new Proxy(() => {}, {}) {} return typeof C; });
t("Reflect.construct on a Proxy of an arrow", () => Reflect.construct(new Proxy(() => 1, trap), []));
t("the handler's construct is never read", () => {
  const h = { get construct() { r.push("  construct read"); return trap.construct; } };
  return new (new Proxy(() => 1, h))();
});
t("arguments are evaluated first", () => new (new Proxy(() => 1, trap))(r.push("  argument evaluated")));
t("a Proxy of an arrow still calls", () => new Proxy(() => 41, {})() + 1);
t("a Proxy of a function runs its trap", () => new (new Proxy(function () {}, trap))().made);
t("a Proxy of a class runs its trap", () => new (new Proxy(class {}, trap))().made);
t("a Proxy of a built-in constructor runs its trap", () => new (new Proxy(Date, trap))().made);
t("a Proxy of a bound function runs its trap", () => new (new Proxy(function () {}.bind(null), trap))().made);
t("a Proxy of a revoked Proxy of a function runs its trap", () => {
  const { proxy, revoke } = Proxy.revocable(function () {}, {});
  revoke();
  return new (new Proxy(proxy, trap))().made;
});
t("a Proxy of a revoked Proxy of an arrow", () => {
  const { proxy, revoke } = Proxy.revocable(() => 1, {});
  revoke();
  return new (new Proxy(proxy, trap))();
});
t("a revoked Proxy of a function", () => {
  const { proxy, revoke } = Proxy.revocable(function () {}, trap);
  revoke();
  return new proxy();
});
t("without a trap, a Proxy of a function constructs it", () => { function F() { this.v = 3; } return new (new Proxy(F, {}))().v; });
t("Array.from with an arrow as this", () => JSON.stringify(Array.from.call(() => {}, [1, 2])));
t("Array.of with an arrow as this", () => JSON.stringify(Array.of.call(() => {}, 1, 2)));
t("Array.from with a Proxy of an arrow as this", () => JSON.stringify(Array.from.call(new Proxy(() => {}, trap), [3])));
t("Reflect.construct on an arrow", () => Reflect.construct(() => {}, []));
t("class extends an arrow", () => { class C extends (() => {}) {} return typeof C; });
t("new on a bound arrow", () => new ((() => {}).bind(null))());
t("an arrow as a Reflect.construct newTarget through a Proxy", () => Reflect.construct(Object, [], new Proxy(() => {}, {})));
console.log(r.join("\n"));
"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###"the issue's arrow with a construct trap: TypeError
the issue's arrow without a trap: TypeError
the issue's Reflect.construct newTarget: TypeError
a Proxy of a Proxy of an arrow: TypeError
a Proxy of a bound arrow: TypeError
a Proxy of a concise method: TypeError
a Proxy of a generator: TypeError
a Proxy of an async function: TypeError
class extends a Proxy of an arrow: TypeError
Reflect.construct on a Proxy of an arrow: TypeError
the handler's construct is never read: TypeError
  argument evaluated
arguments are evaluated first: TypeError
a Proxy of an arrow still calls: 42
  trap ran
a Proxy of a function runs its trap: 1
  trap ran
a Proxy of a class runs its trap: 1
  trap ran
a Proxy of a built-in constructor runs its trap: 1
  trap ran
a Proxy of a bound function runs its trap: 1
  trap ran
a Proxy of a revoked Proxy of a function runs its trap: 1
a Proxy of a revoked Proxy of an arrow: TypeError
a revoked Proxy of a function: TypeError
without a trap, a Proxy of a function constructs it: 3
Array.from with an arrow as this: [1,2]
Array.of with an arrow as this: [1,2]
Array.from with a Proxy of an arrow as this: [3]
Reflect.construct on an arrow: TypeError
class extends an arrow: TypeError
new on a bound arrow: TypeError
an arrow as a Reflect.construct newTarget through a Proxy: TypeError"###);
}
