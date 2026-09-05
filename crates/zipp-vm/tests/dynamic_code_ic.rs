//! Runtime-installed code (`eval`, `new Function`) now owns interpreter inline
//! cache state like main code does. The guards are shared, so the risk is in
//! eligibility alone: every shape transition, prototype change, callee
//! replacement and re-installation a main-code site survives must be survived
//! by a dynamically installed site too, with identical results.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// One body, installed four ways; every installation must agree with the
/// main-code result at every step of a shape-changing workload.
const BODY: &str = r#"
  var acc = [];
  function P(x) { this.x = x; this.y = x + 1; }
  P.prototype.z = 100;
  var objs = [new P(1), new P(2), {x: 3, y: 4}, Object.create(P.prototype)];
  objs[3].x = 5;
  for (var round = 0; round < 6; round++) {
    var s = 0;
    for (var i = 0; i < objs.length; i++) {
      var o = objs[i];
      s += o.x + (o.y === undefined ? 0 : o.y) + o.z;
      s += (o.get === undefined ? 0 : o.get());
      s += helper(i) + (i % 2 ? helper2(i) : 0);
    }
    acc.push(s);
    // Shape transitions between rounds: add, delete, re-add, accessorize,
    // change the prototype value and swap the callee behind `helper`.
    if (round === 0) { objs[0].w = 1; delete objs[1].y; }
    if (round === 1) { objs[1].y = 20; P.prototype.z = 200; }
    if (round === 2) {
      Object.defineProperty(objs[2], 'x', { get: function () { return 30; }, configurable: true });
      helper = function (i) { return i * 10; };
    }
    if (round === 3) { delete objs[2].x; objs[2].x = 300; Object.setPrototypeOf(objs[3], { z: 7, get: function () { return 1; } }); }
    if (round === 4) { P.prototype.get = function () { return this.x; }; helper2 = function (i) { return -i; }; }
  }
  return acc.join(',');
"#;

#[test]
fn installed_code_matches_main_code_across_shape_transitions() {
    let src = format!(
        r#"
  var helper = function (i) {{ return i; }};
  var helper2 = function (i) {{ return i * 2; }};
  function reset() {{
    helper = function (i) {{ return i; }};
    helper2 = function (i) {{ return i * 2; }};
  }}
  var body = {body:?};
  var mainFn = function () {{ {body} }};
  reset(); var expected = mainFn();
  reset(); var viaFunction = new Function(body)();
  reset(); var viaIndirect = (0, eval)('(function () {{ ' + body + ' }})')();
  reset(); var viaDirect = (function () {{ return eval('(function () {{ ' + body + ' }})'); }})()();
  reset(); var viaNested = new Function('return new Function(' + JSON.stringify(body) + ')')()();
  function same(label, got) {{ if (got !== expected) throw new Error(label + ': ' + got + ' != ' + expected); }}
  same('Function', viaFunction);
  same('indirect eval', viaIndirect);
  same('direct eval', viaDirect);
  same('nested Function', viaNested);
  // Re-run each installed function: the second execution runs on filled caches.
  reset(); same('Function again', new Function(body)());
  var f = new Function(body);
  reset(); same('same Function twice a', f());
  reset(); same('same Function twice b', f());
  console.log(expected);
"#,
        body = BODY,
    );
    let out = run_ok(&src);
    assert_eq!(out.len(), 1);
    assert!(out[0].split(',').count() == 6, "six rounds: {}", out[0]);
}

#[test]
fn many_installed_functions_stay_correct_past_any_cache_budget() {
    // Thousands of distinct dynamic functions with property and call sites:
    // whether or not the per-VM dynamic cache budget admits them, every
    // result must equal the main-code computation.
    let src = r#"
      function mk(i) {
        return new Function('o', 'f', 'var s = 0; for (var k = 0; k < 4; k++) s += o.a + o.b + f(k) + ' + i + '; return s;');
      }
      var fns = [];
      for (var i = 0; i < 3000; i++) fns.push(mk(i));
      var o1 = {a: 1, b: 2}, o2 = {b: 5, a: 4, c: 9};
      var f1 = function (k) { return k; }, f2 = function (k) { return k * 3; };
      var t = 0;
      for (var r = 0; r < 3; r++) {
        for (var j = 0; j < fns.length; j++) {
          var o = (j + r) % 2 ? o1 : o2, f = (j * r) % 3 ? f1 : f2;
          var got = fns[j](o, f);
          var want = 0;
          for (var k = 0; k < 4; k++) want += o.a + o.b + f(k) + j;
          if (got !== want) throw new Error('fn ' + j + ' round ' + r + ': ' + got + ' != ' + want);
          t += got;
        }
      }
      console.log(t);
    "#;
    let out = run_ok(src);
    assert_eq!(out.len(), 1);
}

#[test]
fn installed_code_sees_live_prototype_and_global_changes() {
    let src = r#"
      var log = [];
      var target = { v: 1 };
      var proto = { m: function () { return 'proto1'; } };
      Object.setPrototypeOf(target, proto);
      var probe = new Function('t', 'return t.v + ":" + t.m();');
      log.push(probe(target));
      proto.m = function () { return 'proto2'; };
      log.push(probe(target));
      target.m = function () { return 'own'; };
      log.push(probe(target));
      delete target.m;
      log.push(probe(target));
      Object.setPrototypeOf(target, { m: function () { return 'proto3'; } });
      log.push(probe(target));
      Object.defineProperty(target, 'v', { get: function () { return 42; } });
      log.push(probe(target));
      var callee = new Function('return g();');
      var g = function () { return 'g1'; };
      log.push(callee());
      g = function () { return 'g2'; };
      log.push(callee());
      g = function* () { yield 'gen'; };
      log.push(typeof callee().next);
      console.log(log.join('|'));
    "#;
    assert_eq!(
        run_ok(src),
        vec!["1:proto1|1:proto2|1:own|1:proto2|1:proto3|42:proto3|g1|g2|function"]
    );
}
