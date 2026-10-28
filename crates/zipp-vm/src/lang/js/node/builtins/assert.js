// node:assert — assertions, deep equality as util's.
(function (host, builtin) {
  "use strict";
  const util = builtin("util");
  class AssertionError extends Error {
    constructor(options) {
      const { message, actual, expected, operator } = options || {};
      super(message !== undefined ? String(message)
        : util.inspect(actual) + " " + (operator || "==") + " " + util.inspect(expected));
      this.name = "AssertionError";
      this.code = "ERR_ASSERTION";
      this.actual = actual;
      this.expected = expected;
      this.operator = operator;
      this.generatedMessage = message === undefined;
    }
  }
  function fail(actual, expected, message, operator, defaultMessage) {
    if (message instanceof Error) throw message;
    throw new AssertionError({ message: message !== undefined ? message : defaultMessage, actual, expected, operator });
  }
  function ok(...args) {
    const [value, message] = args;
    if (!value) {
      fail(value, true, message, "==", args.length === 0
        ? "No value argument passed to `assert.ok()`"
        : "The expression evaluated to a falsy value:\n\n  assert.ok(" + util.inspect(value) + ")\n");
    }
  }
  function matches(err, expected) {
    if (expected === undefined) return true;
    if (typeof expected === "function") {
      if (expected.prototype !== undefined && err instanceof expected) return true;
      if (Error.isPrototypeOf(expected) || expected === Error) return false;
      return expected.call({}, err) === true;
    }
    if (expected instanceof RegExp) return expected.test(String(err));
    if (typeof expected === "object") {
      return Object.keys(expected).every((k) => (expected[k] instanceof RegExp && typeof err[k] === "string"
        ? expected[k].test(err[k])
        : util.isDeepStrictEqual(err[k], expected[k])));
    }
    return true;
  }
  function make(strict) {
    const assert = (...args) => ok(...args);
    const eq = strict ? (a, b) => Object.is(a, b) : (a, b) => a == b || (a !== a && b !== b);
    Object.assign(assert, {
      ok,
      fail(message = "Failed") {
        if (message instanceof Error) throw message;
        throw new AssertionError({ message, operator: "fail" });
      },
      equal(a, b, m) {
        if (!eq(a, b)) fail(a, b, m, strict ? "strictEqual" : "==");
      },
      notEqual(a, b, m) {
        if (eq(a, b)) fail(a, b, m, strict ? "notStrictEqual" : "!=");
      },
      strictEqual(a, b, m) {
        if (!Object.is(a, b)) {
          fail(a, b, m, "strictEqual", "Expected values to be strictly equal:\n\n" + util.inspect(a) + " !== " + util.inspect(b) + "\n");
        }
      },
      notStrictEqual(a, b, m) {
        if (Object.is(a, b)) fail(a, b, m, "notStrictEqual", 'Expected "actual" to be strictly unequal to: ' + util.inspect(b));
      },
      deepEqual(a, b, m) {
        if (!util._deepEqual(a, b, strict)) {
          fail(a, b, m, strict ? "deepStrictEqual" : "deepEqual", "Expected values to be " + (strict ? "strictly" : "loosely") + " deep-equal");
        }
      },
      notDeepEqual(a, b, m) {
        if (util._deepEqual(a, b, strict)) fail(a, b, m, "notDeepEqual");
      },
      deepStrictEqual(a, b, m) {
        if (!util._deepEqual(a, b, true)) {
          fail(a, b, m, "deepStrictEqual", "Expected values to be strictly deep-equal:\n" + util.inspect(a) + "\n\nshould equal\n\n" + util.inspect(b));
        }
      },
      notDeepStrictEqual(a, b, m) {
        if (util._deepEqual(a, b, true)) fail(a, b, m, "notDeepStrictEqual");
      },
      throws(fn, expected, message) {
        if (typeof expected === "string") {
          message = expected;
          expected = undefined;
        }
        try {
          fn();
        } catch (e) {
          if (!matches(e, expected)) throw e;
          return;
        }
        fail(undefined, expected, message, "throws", "Missing expected exception" + (expected && expected.name ? " (" + expected.name + ")." : "."));
      },
      doesNotThrow(fn, expected, message) {
        try {
          fn();
        } catch (e) {
          fail(e, expected, typeof expected === "string" ? expected : message, "doesNotThrow", 'Got unwanted exception.\nActual message: "' + (e && e.message) + '"');
        }
      },
      async rejects(p, expected, message) {
        if (typeof p === "function") p = p();
        try {
          await p;
        } catch (e) {
          if (!matches(e, expected)) throw e;
          return;
        }
        fail(undefined, expected, message, "rejects", "Missing expected rejection.");
      },
      async doesNotReject(p, expected, message) {
        if (typeof p === "function") p = p();
        try {
          await p;
        } catch (e) {
          fail(e, expected, message, "doesNotReject", "Got unwanted rejection.");
        }
      },
      match(s, re, m) {
        if (!re.test(s)) fail(s, re, m, "match", "The input did not match the regular expression " + re);
      },
      doesNotMatch(s, re, m) {
        if (re.test(s)) fail(s, re, m, "doesNotMatch");
      },
      ifError(e) {
        if (e !== null && e !== undefined) throw e;
      },
      AssertionError,
    });
    return assert;
  }
  const assert = make(false);
  assert.strict = make(true);
  assert.strict.strict = assert.strict;
  return assert;
})
