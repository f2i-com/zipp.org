//! A Proxy handler's trap is read where it lies when the handler is an
//! ordinary object whose chain answers the read without running code
//! (`plain_handler_trap`); any other handler takes the generic Get. The
//! results must be Node's for every trap kind through a plain handler, a trap
//! inherited, replaced, deleted, null, undefined and non-callable, an
//! accessor trap, a class-instance handler, a Proxy handler, a function
//! handler, a null-prototype handler, a trap on Object.prototype added and
//! removed, a revoked proxy, the get invariant on a frozen target, and a hot
//! loop.

const SOURCE: &str = r##"
(function () {
  var out = [];
  function log(label, v) { out.push(label + ': ' + v); }
  function err(e) { return e instanceof TypeError ? 'TypeError' : 'other:' + e; }
  var target = { a: 1, b: 2 };

  // Every trap kind through a plain handler.
  var seen = [];
  var h = {
    get: function (t, k, r) { seen.push('get:' + String(k)); return k in t ? t[k] : 'dflt'; },
    set: function (t, k, v) { seen.push('set:' + k); t[k] = v * 10; return true; },
    has: function (t, k) { seen.push('has:' + k); return k === 'zz' || k in t; },
    deleteProperty: function (t, k) { seen.push('del:' + k); return delete t[k]; },
    ownKeys: function (t) { seen.push('keys'); return Reflect.ownKeys(t); },
    getOwnPropertyDescriptor: function (t, k) { seen.push('gopd:' + k); return Reflect.getOwnPropertyDescriptor(t, k); },
    defineProperty: function (t, k, d) { seen.push('def:' + k); return Reflect.defineProperty(t, k, d); },
    getPrototypeOf: function (t) { seen.push('gpo'); return Object.prototype; },
  };
  var p = new Proxy(target, h);
  p.c = 3;
  log('traps', [p.a, p.missing, 'zz' in p, 'a' in p, delete p.b, Object.keys(p).join('|'), Object.getPrototypeOf(p) === Object.prototype].join(','));
  Object.defineProperty(p, 'd', { value: 4, configurable: true, enumerable: true, writable: true });
  log('seen', seen.join(' '));

  // Trap found on the handler's prototype, changed, deleted, set to null.
  var base = { get: function (t, k) { return 'base:' + k; } };
  var h2 = Object.create(base);
  var p2 = new Proxy({}, h2);
  log('inherited trap', p2.x);
  h2.get = function (t, k) { return 'own:' + k; };
  log('own trap', p2.x);
  delete h2.get;
  log('deleted own', p2.x);
  base.get = null;
  log('null trap', String(p2.x));
  base.get = undefined;
  log('undefined trap', String(p2.x));
  base.get = 42;
  try { log('non-callable trap', p2.x); } catch (e) { log('non-callable trap', err(e)); }
  delete base.get;

  // Accessor trap, class-instance handler, Proxy handler, function handler.
  var calls = 0;
  var h3 = {};
  Object.defineProperty(h3, 'get', { get: function () { calls++; return function (t, k) { return 'acc:' + k; }; } });
  var p3 = new Proxy({}, h3);
  log('accessor trap', p3.y + ',' + p3.y + ',' + calls);
  class Handler { get(t, k) { return 'cls:' + k; } has() { return true; } }
  var p4 = new Proxy({}, new Handler());
  log('class handler', p4.q + ',' + ('anything' in p4));
  var meta = new Proxy({}, { get: function (t, name) { return function (tt, k) { return 'meta-' + name + ':' + k; }; } });
  var p5 = new Proxy({}, meta);
  log('proxy handler', p5.w);
  function fnHandler() {}
  fnHandler.get = function (t, k) { return 'fn:' + k; };
  var p6 = new Proxy({}, fnHandler);
  log('function handler', p6.v);
  var hProtoNull = Object.create(null);
  hProtoNull.get = function (t, k) { return 'nullproto:' + k; };
  log('null-proto handler', new Proxy({}, hProtoNull).k);
  Object.prototype.get = function (t, k) { return 'objproto:' + k; };
  log('trap on Object.prototype', new Proxy({}, {}).zz);
  delete Object.prototype.get;
  log('removed from Object.prototype', String(new Proxy({ zz: 'target' }, {}).zz));

  // Revocation, invariants, a hot loop.
  var r = Proxy.revocable({}, { get: function () { return 1; } });
  log('before revoke', r.proxy.x);
  r.revoke();
  try { r.proxy.x; log('revoked', 'none'); } catch (e) { log('revoked', err(e)); }
  var frozen = Object.freeze({ k: 'real' });
  try { log('invariant', new Proxy(frozen, { get: function () { return 'fake'; } }).k); } catch (e) { log('invariant', err(e)); }
  var hot = new Proxy({ n: 1 }, { get: function (t, k) { return t[k] + 1; } });
  var s = 0;
  for (var i = 0; i < 500; i++) s += hot.n;
  log('loop', s);

  out.forEach(function (l) { console.log(l); });
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
fn proxy_handler_trap_worker() {
    if std::env::var_os("ZIPP_PROXY_HANDLER_TRAP_WORKER").is_none() {
        return;
    }
    // The harness has printed `test proxy_handler_trap_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn proxy_handler_trap_match_node() {
    if std::env::var_os("ZIPP_PROXY_HANDLER_TRAP_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["proxy_handler_trap_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_PROXY_HANDLER_TRAP_WORKER", "1")
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
