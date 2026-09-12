//! Regression coverage for strict ECMAScript ToNumber sites. BigInt remains
//! accepted by Number/ToNumeric/Intl.NumberFormat paths, while operations whose
//! algorithms say ToNumber reject it after any observable ToPrimitive step.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const STRICT_TONUMBER: &str = r#"
var failures = [];
var checks = 0;

function mustTypeError(label, op) {
  try {
    op();
    failures.push(label + ":accepted");
  } catch (e) {
    if (!(e instanceof TypeError)) failures.push(label + ":" + e.constructor.name);
  }
  checks++;
}

function mustEqual(label, actual, expected) {
  if (!Object.is(actual, expected)) failures.push(label + ":" + String(actual));
  checks++;
}

function big(kind) {
  if (kind === "primitive") return 1n;
  if (kind === "boxed") return Object(1n);
  return { [Symbol.toPrimitive]: function () { return 1n; } };
}

["primitive", "boxed", "object-result"].forEach(function (kind) {
  var v = big(kind);
  function strict(label, op) { mustTypeError(label + "/" + kind, op); }

  strict("Math.abs", function () { Math.abs(v); });
  strict("Math.max", function () { Math.max(0, v); });
  strict("Math.max-spread", function () { Math.max(...[v]); });
  strict("Math.pow", function () { Math.pow(v, 1); });
  strict("Math.imul", function () { Math.imul(v, 1); });
  if (typeof Math.f16round === "function") {
    strict("Math.f16round", function () { Math.f16round(v); });
  }

  strict("array-length-set", function () { var a = []; a.length = v; });
  strict("array-length-define", function () {
    Object.defineProperty([], "length", { value: v });
  });
  strict("Array.from-length", function () { Array.from({ length: v }); });
  strict("array-like-length", function () {
    Array.prototype.forEach.call({ length: v }, function () {});
  });
  strict("array-slice-index", function () { [0, 1].slice(v); });
  strict("array-indexOf-index", function () { [0, 1].indexOf(0, v); });
  strict("TypedArray-array-like-length", function () {
    new Uint8Array({ length: v });
  });

  strict("Date-constructor", function () { new Date(v); });
  strict("Date.UTC", function () { Date.UTC(v, 0); });
  strict("Date.setTime", function () { new Date(0).setTime(v); });
  strict("Date.setUTCFullYear", function () { new Date(0).setUTCFullYear(v); });

  strict("RegExp-symbol-split-limit", function () {
    RegExp.prototype[Symbol.split].call(/a/, "a", v);
  });
  strict("String-regexp-split-limit", function () { "a".split(/a/, v); });
  strict("String-string-split-limit", function () { "a-a".split("-", v); });
  strict("String-repeat-count", function () { "a".repeat(v); });
  strict("String-lastIndexOf-position", function () { "a".lastIndexOf("a", v); });

  strict("global-isNaN", function () { isNaN(v); });
  strict("global-isFinite", function () { isFinite(v); });
  strict("parseInt-radix", function () { parseInt("10", v); });

  strict("Intl.NumberFormat-option", function () {
    new Intl.NumberFormat("en", { minimumFractionDigits: v });
  });
  strict("Intl.getCanonicalLocales-length", function () {
    Intl.getCanonicalLocales({ length: v });
  });
  strict("Intl.PluralRules.select", function () {
    new Intl.PluralRules("en").select(v);
  });
  strict("Intl.RelativeTimeFormat.format", function () {
    new Intl.RelativeTimeFormat("en").format(v, "day");
  });
  strict("Intl.DateTimeFormat.format", function () {
    new Intl.DateTimeFormat("en").format(v);
  });

  if (typeof Temporal === "object") {
    strict("Temporal.Duration-constructor", function () {
      new Temporal.Duration(v);
    });
    strict("Temporal.Duration.from-field", function () {
      Temporal.Duration.from({ days: v });
    });
    strict("Temporal.PlainDate.from-field", function () {
      Temporal.PlainDate.from({ year: v, month: 1, day: 1 });
    });
    strict("Temporal-rounding-increment", function () {
      new Temporal.PlainDate(2020, 1, 1).until(
        new Temporal.PlainDate(2020, 1, 2),
        { roundingIncrement: v }
      );
    });
  }

  if (typeof Set.prototype.union === "function") {
    strict("Set-like-size", function () {
      new Set([1]).union({
        size: v,
        has: function () { return false; },
        keys: function () { return [][Symbol.iterator](); }
      });
    });
  }

  var proxiedArray = new Proxy([], {
    get: function (target, key) {
      if (key === "length") return v;
      return Reflect.get(target, key);
    }
  });
  strict("JSON-array-length", function () { JSON.stringify(proxiedArray); });
  strict("String.raw-length", function () {
    String.raw({ raw: { length: v } });
  });

  var numberBox = new Number(2);
  if (kind === "primitive") {
    numberBox.valueOf = function () { return 1n; };
  } else if (kind === "boxed") {
    numberBox.valueOf = function () { return Object(1n); };
    numberBox.toString = function () { return 1n; };
  } else {
    numberBox[Symbol.toPrimitive] = function () { return 1n; };
  }
  strict("Math.abs-number-wrapper-hook", function () { Math.abs(numberBox); });
  strict("JSON-space-number-wrapper", function () {
    JSON.stringify({ a: 1 }, null, numberBox);
  });
  strict("JSON-number-wrapper", function () { JSON.stringify(numberBox); });

  // Intentional BigInt-accepting paths: Number explicitly converts BigInt;
  // relational comparison and same-kind arithmetic use ToNumeric; parseInt
  // stringifies its first argument; NumberFormat uses ToIntlMathematicalValue.
  mustEqual("Number/" + kind, Number(v), 1);
  mustEqual("new-Number/" + kind, new Number(v).valueOf(), 1);
  mustEqual("relational/" + kind, v < 2, true);
  mustTypeError("mixed-arithmetic/" + kind, function () { return v - 1; });
  mustEqual("bigint-arithmetic/" + kind, String(v - 1n), "0");
  mustEqual("parseInt-input/" + kind, parseInt(v), 1);

  var nf = new Intl.NumberFormat("en");
  mustEqual("NumberFormat-format/" + kind, nf.format(v), "1");
  mustEqual(
    "NumberFormat-formatToParts/" + kind,
    nf.formatToParts(v).map(function (part) { return part.value; }).join(""),
    "1"
  );
  if (typeof nf.formatRange === "function") {
    try { nf.formatRange(v, 2n); checks++; }
    catch (e) { failures.push("NumberFormat-formatRange/" + kind + ":" + e.constructor.name); }
  }
});

[
  new Number(1),
  new String("1"),
  new Boolean(false),
  Object(1n)
].forEach(function (box, index) {
  box[Symbol.toPrimitive] = function () { return 3; };
  mustEqual("boxed-Symbol.toPrimitive/" + index, Math.abs(box), 3);
});

// Date is an object even though its default numeric primitive is stored in a
// dedicated heap variant. Number and NumberFormat must still run its inherited
// @@toPrimitive and therefore observe an overridden valueOf.
var hookedDate = new Date(1);
hookedDate.valueOf = function () { return 7n; };
var hookedDateNf = new Intl.NumberFormat("en");
mustEqual("Number/date-valueOf", Number(hookedDate), 7);
mustEqual("new-Number/date-valueOf", new Number(hookedDate).valueOf(), 7);
mustEqual("ToNumeric/date-valueOf", String(hookedDate - 2n), "5");
mustTypeError("ToNumeric/date-mixed", function () { return hookedDate - 2; });
mustEqual("NumberFormat/date-valueOf", hookedDateNf.format(hookedDate), "7");
mustEqual(
  "NumberFormatToParts/date-valueOf",
  hookedDateNf.formatToParts(hookedDate).map(function (part) { return part.value; }).join(""),
  "7"
);

console.log(failures.length ? "FAIL:" + failures.join(",") : "strict-tonumber-bigint-ok:" + checks);
"#;

#[test]
fn audit_20260912_strict_tonumber_bigint_paths() {
    let output = run_ok(STRICT_TONUMBER);
    assert_eq!(output.len(), 1, "unexpected output: {output:?}");
    assert!(
        output[0].starts_with("strict-tonumber-bigint-ok:"),
        "{}",
        output[0]
    );
}

#[cfg(not(feature = "safe-sandbox"))]
#[test]
fn audit_20260912_atomics_timeouts_use_strict_tonumber() {
    let output = run_ok(
        r#"
        var failures = [];
        function big(kind) {
          if (kind === "primitive") return 1n;
          if (kind === "boxed") return Object(1n);
          return { [Symbol.toPrimitive]: function () { return 1n; } };
        }
        ["primitive", "boxed", "object-result"].forEach(function (kind) {
          var sab = new SharedArrayBuffer(4);
          var ta = new Int32Array(sab);
          ["wait", "waitAsync"].forEach(function (name) {
            try {
              Atomics[name](ta, 0, 0, big(kind));
              failures.push(name + "/" + kind + ":accepted");
            } catch (e) {
              if (!(e instanceof TypeError)) failures.push(name + "/" + kind + ":" + e.constructor.name);
            }
          });
        });
        console.log(failures.length ? "FAIL:" + failures.join(",") : "atomics-timeout-bigint-ok");
        "#,
    );
    assert_eq!(output, ["atomics-timeout-bigint-ok"]);
}
