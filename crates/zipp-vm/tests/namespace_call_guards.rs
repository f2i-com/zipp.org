//! Guarded JSON/Math calls and captured namespace fallbacks must preserve
//! EvaluateCall ordering under interpretation, JIT execution and GC stress.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const SEMANTICS: &str = r#"
  function mark() { return this.tag; }
  var J = {tag: "J", parse: mark, stringify: mark};
  var M = {
    tag: "M", abs: mark, floor: mark, ceil: mark, round: mark, trunc: mark,
    sign: mark, sqrt: mark, cbrt: mark, exp: mark, log: mark, log2: mark,
    log10: mark, expm1: mark, log1p: mark, sin: mark, cos: mark, tan: mark,
    asin: mark, acos: mark, atan: mark, sinh: mark, cosh: mark, tanh: mark,
    asinh: mark, acosh: mark, atanh: mark, clz32: mark, fround: mark,
    pow: mark, atan2: mark, imul: mark, min: mark, max: mark, hypot: mark
  };
  var D = {tag: "D", now: mark, UTC: mark, parse: mark};
  var S = {tag: "S", raw: mark};

  function shadow(JSON, Math, Date, String) {
    var ms = [
      Math.abs(1), Math.floor(1), Math.ceil(1), Math.round(1), Math.trunc(1),
      Math.sign(1), Math.sqrt(1), Math.cbrt(1), Math.exp(1), Math.log(1),
      Math.log2(1), Math.log10(1), Math.expm1(1), Math.log1p(1), Math.sin(1),
      Math.cos(1), Math.tan(1), Math.asin(1), Math.acos(1), Math.atan(1),
      Math.sinh(1), Math.cosh(1), Math.tanh(1), Math.asinh(1), Math.acosh(1),
      Math.atanh(1), Math.clz32(1), Math.fround(1), Math.pow(1, 2),
      Math.atan2(1, 2), Math.imul(1, 2), Math.min(1, 2), Math.max(1, 2),
      Math.hypot(1, 2)
    ].join("");
    return JSON.parse(1) + JSON.stringify(1) + "|" + ms + "|" +
      Date.now() + Date.UTC(1) + Date.parse(1) + "|" + String.raw`x${1}y`;
  }
  console.log("shadow:" + shadow(J, M, D, S));

  var BJ = JSON, BM = Math, BD = Date, BS = String;
  JSON = J; var gj = JSON.parse(1) + JSON.stringify(1); JSON = BJ;
  Math = M; var gm = Math.max(1, 2); Math = BM;
  Date = D; var gd = Date.now() + Date.UTC(1) + Date.parse(1); Date = BD;
  String = S; var gs = String.raw`x`; String = BS;
  console.log("global:" + gj + gm + gd + gs);

  function tagged(tag, recv) {
    return function () { return this === recv ? tag : "bad-this"; };
  }
  var old;
  old = JSON.parse; JSON.parse = tagged("p", JSON); var rp = JSON.parse(1); JSON.parse = old;
  old = JSON.stringify; JSON.stringify = tagged("s", JSON); var rs = JSON.stringify(1); JSON.stringify = old;
  old = Math.floor; Math.floor = tagged("f", Math); var rm = Math.floor(1); Math.floor = old;
  old = Math.max; Math.max = tagged("m", Math); var rms = Math.max(...[1, 2]); Math.max = old;
  old = Date.UTC; Date.UTC = tagged("d", Date); var rd = Date.UTC(1); Date.UTC = old;
  old = String.raw; String.raw = tagged("r", String); var rr = String.raw`x`; String.raw = old;
  console.log("replace:" + rp + rs + rm + rms + rd + rr);

  function restore(ns, name, value) {
    Reflect.defineProperty(ns, name, {
      value: value, writable: true, enumerable: false, configurable: true
    });
  }
  var order = [];
  function arg(label, value) { order.push(label + "a"); return value; }
  function accessor(ns, name, label, result) {
    Reflect.defineProperty(ns, name, {
      configurable: true,
      get: function () {
        order.push(label + "g");
        return function () {
          order.push(label + "c:" + (this === ns));
          return result;
        };
      }
    });
  }
  old = JSON.parse; accessor(JSON, "parse", "P", "p"); var ap = JSON.parse(arg("P", "1")); restore(JSON, "parse", old);
  old = JSON.stringify; accessor(JSON, "stringify", "S", "s"); var as = JSON.stringify(arg("S", 1)); restore(JSON, "stringify", old);
  old = Math.max; accessor(Math, "max", "M", "m"); var am = Math.max(...arg("M", [1, 2])); restore(Math, "max", old);
  old = Date.UTC; accessor(Date, "UTC", "D", "d"); var ad = Date.UTC(arg("D", 1)); restore(Date, "UTC", old);
  old = String.raw; accessor(String, "raw", "R", "r"); var ar = String.raw`a${arg("R", 1)}b`; restore(String, "raw", old);
  console.log("order:" + order.join("|") + ":" + ap + as + am + ad + ar);

  var captured = [];
  old = JSON.parse;
  function mutateParse() { JSON.parse = function () { return "wrong"; }; return '"ok"'; }
  captured.push(JSON.parse(mutateParse()) === "ok"); restore(JSON, "parse", old);
  old = JSON.stringify;
  function mutateStringify() { JSON.stringify = function () { return "wrong"; }; return 3; }
  captured.push(JSON.stringify(mutateStringify()) === "3"); restore(JSON, "stringify", old);
  old = Math.imul;
  function mutateImul() { Math.imul = function () { return 99; }; return 2; }
  captured.push(Math.imul(mutateImul(), 3) === 6); restore(Math, "imul", old);
  old = Math.max;
  function mutateMax() { Math.max = function () { return 99; }; return [2, 8]; }
  captured.push(Math.max(...mutateMax()) === 8); restore(Math, "max", old);
  old = Date.UTC;
  function mutateUTC() { Date.UTC = function () { return 99; }; return 1970; }
  captured.push(Date.UTC(mutateUTC(), 0) === 0); restore(Date, "UTC", old);
  old = String.raw;
  function mutateRaw() { String.raw = function () { return "wrong"; }; return 7; }
  captured.push(String.raw`a${mutateRaw()}b` === "a7b"); restore(String, "raw", old);
  console.log("captured:" + captured.join(""));

  function readPi(Math) { return Math.PI; }
  var PM = {PI: 7};
  var piOrder = [];
  Reflect.defineProperty(PM, "PI", {
    configurable: true, get: function () { piOrder.push("get"); return 8; }
  });
  var piLex = readPi(PM);
  BM = Math; Math = {PI: 9}; var piGlobal = Math.PI; Math = BM;
  console.log("constants:" + piLex + ":" + piGlobal + ":" + piOrder.join(""));
"#;

#[test]
fn namespace_semantics_child() {
    if std::env::var_os("ZIPP_NAMESPACE_CHILD").is_none() {
        return;
    }
    let expected_math = "M".repeat(34);
    assert_eq!(
        run_ok(SEMANTICS),
        vec![
            format!("shadow:JJ|{expected_math}|DDD|S"),
            "global:JJMDDDS".to_string(),
            "replace:psfmdr".to_string(),
            "order:Pg|Pa|Pc:true|Sg|Sa|Sc:true|Mg|Ma|Mc:true|Dg|Da|Dc:true|Rg|Ra|Rc:true:psmdr"
                .to_string(),
            "captured:truetruetruetruetruetrue".to_string(),
            "constants:8:9:get".to_string(),
        ]
    );
}

const HOT: &str = r#"
  function hot(n) {
    var s = 0;
    for (var i = 0; i < n; i++) {
      s += Math.imul(i, 3) + Math.floor(i + 0.5);
    }
    return s;
  }
  var n = 6000;
  var before = hot(n);
  var imul = Math.imul, floor = Math.floor;
  Math.imul = function () { return 7; };
  Math.floor = function () { return 2; };
  var after = hot(n);
  Math.imul = imul; Math.floor = floor;
  console.log(before + "|" + after);
"#;

#[test]
fn namespace_jit_guard_child() {
    if std::env::var_os("ZIPP_NAMESPACE_JIT_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(HOT), ["71988000|54000"]);
}

#[test]
fn namespace_modes_match() {
    if std::env::var_os("ZIPP_NAMESPACE_CHILD").is_some()
        || std::env::var_os("ZIPP_NAMESPACE_JIT_CHILD").is_some()
    {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (test, marker) in [
        ("namespace_semantics_child", "ZIPP_NAMESPACE_CHILD"),
        ("namespace_jit_guard_child", "ZIPP_NAMESPACE_JIT_CHILD"),
    ] {
        for (mode, env) in [
            ("default", None),
            ("interpreter", Some(("ZIPP_NOJIT", "1"))),
            ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
            ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ] {
            let mut cmd = std::process::Command::new(&exe);
            cmd.args(["--exact", test, "--nocapture"])
                .env(marker, "1")
                .env_remove("ZIPP_NOJIT")
                .env_remove("ZIPP_JIT_THRESHOLD")
                .env_remove("ZIPP_GC_STRESS");
            if let Some((key, value)) = env {
                cmd.env(key, value);
            }
            let out = cmd.output().expect("spawn mode child");
            assert!(
                out.status.success(),
                "{test}/{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
