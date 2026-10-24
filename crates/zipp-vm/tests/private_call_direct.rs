//! `this.#m(x)` whose brand check resolves the private member to a plain
//! function enters it as a frame of the dispatch loop, as the general call
//! tail would; generators, async functions, getters and other values keep that
//! tail. The results must be Node's for: methods that read and write private
//! fields, return `this`, read `arguments`, take rest and default parameters,
//! throw, and recurse; private generator and async methods, a private getter
//! called as a method (TypeError), private fields holding a function or an
//! arrow, static private methods, another instance, wrong and null receivers,
//! subclass instances, the same class source evaluated twice, a brand added
//! by a return override, and an inner class shadowing the name.

const SOURCE: &str = r##"
(function () {
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }
  function err(e) { return e instanceof TypeError ? 'TypeError' : 'other:' + e; }

  class P {
    #n = 0;
    #inc(x) { this.#n += x; return this.#n; }
    #self() { return this; }
    #args() { return arguments.length + ':' + Array.prototype.join.call(arguments, '|'); }
    #rest(...xs) { return xs.length; }
    #def(a, b = a * 2) { return a + b; }
    *#gen() { yield 1; yield 2; }
    async #later(x) { return x + 1; }
    get #g() { return 'getter:' + this.#n; }
    #fieldFn = function (y) { return 'field:' + y + ':' + (this instanceof P); };
    #arrow = (z) => 'arrow:' + z + ':' + (this instanceof P);
    #throws() { throw new RangeError('inside'); }
    #recur(k) { return k <= 0 ? 0 : k + this.#recur(k - 1); }
    static #sm(x) { return 'static:' + x; }
    static callStatic(x) { return P.#sm(x); }
    run(x) { return this.#inc(x); }
    self() { return this.#self() === this; }
    args() { return this.#args(1, 'b', 3); }
    rest() { return this.#rest(1, 2, 3, 4); }
    def() { return this.#def(5) + ',' + this.#def(5, 1); }
    gen() { return Array.from(this.#gen()).join(','); }
    later() { return this.#later(41); }
    getterCall() { try { return this.#g(); } catch (e) { return err(e); } }
    fieldFn() { return this.#fieldFn(7); }
    arrow() { return this.#arrow(8); }
    throws() { try { this.#throws(); return 'none'; } catch (e) { return e instanceof RangeError ? 'RangeError' : 'other'; } }
    recur() { return this.#recur(50); }
    on(o, x) { try { return o.#inc(x); } catch (e) { return err(e); } }
    loop(n) { var s = 0; for (var i = 0; i < n; i++) s = (s + this.#inc(1)) | 0; return s; }
  }

  var p = new P();
  log('run', p.run(1) + ',' + p.run(2));
  log('self', p.self());
  log('args', p.args());
  log('rest', p.rest());
  log('def', p.def());
  log('gen', p.gen());
  log('getter called', p.getterCall());
  log('field fn', p.fieldFn());
  log('arrow field', p.arrow());
  log('throws', p.throws());
  log('recur', p.recur());
  log('static', P.callStatic(3));
  log('other instance', p.on(new P(), 5));
  log('wrong receiver', p.on({}, 5));
  log('null receiver', p.on(null, 5));
  log('loop', p.loop(1000));

  // A subclass instance carries the brand; a separate class evaluation's
  // instance does not.
  class Q extends P { more() { return this.run(10); } }
  var q = new Q();
  log('subclass', q.more() + ',' + p.on(q, 1));
  function make() { return class { #m() { return 'm'; } call(o) { try { return o.#m(); } catch (e) { return err(e); } } }; }
  var A = make(), B = make();
  var a = new A(), b = new B();
  log('same source, other evaluation', a.call(a) + ',' + a.call(b) + ',' + b.call(b));

  // Return-override: an object branded by a base constructor's return.
  class Base { constructor(o) { return o; } }
  class Stamp extends Base { #tag() { return 'stamped'; } static read(o) { try { return o.#tag(); } catch (e) { return err(e); } } }
  var plain = {};
  new Stamp(plain);
  log('return override', Stamp.read(plain) + ',' + Stamp.read({}));

  // Shadowing: an inner class declaring the same private name.
  class Outer {
    #x() { return 'outer'; }
    test() {
      var self = this;
      class Inner { #x() { return 'inner'; } go(o) { try { return o.#x(); } catch (e) { return err(e); } } }
      var i = new Inner();
      return i.go(i) + ',' + i.go(self) + ',' + this.#x();
    }
  }
  log('shadowing', new Outer().test());

  // The async method's promise.
  p.later().then(function (v) {
    log('async', v);
    out.forEach(function (l) { console.log(l); });
  });
})();
"##;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn zipp_lines() -> Vec<String> {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.join("\n").lines().map(str::to_owned).collect()
}

#[test]
fn private_call_direct_worker() {
    if std::env::var_os("ZIPP_PRIVATE_CALL_DIRECT_WORKER").is_none() {
        return;
    }
    // The harness has printed `test private_call_direct_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn private_call_direct_match_node() {
    if std::env::var_os("ZIPP_PRIVATE_CALL_DIRECT_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["private_call_direct_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_PRIVATE_CALL_DIRECT_WORKER", "1")
        .env("ZIPP_NOJIT", "1")
        .output()
        .expect("spawn the worker");
    assert!(out.status.success(), "worker failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("LINE ").map(str::to_owned))
        .collect();
    assert_eq!(got, want, "interpreter-only run");
}
