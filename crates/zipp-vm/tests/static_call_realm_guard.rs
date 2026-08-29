//! Static namespace fast paths must distinguish same-id natives from another
//! realm.  Calling a child-realm intrinsic through a main-realm namespace is
//! still a call to the captured child function: its result allocations and
//! abrupt completions belong to that function's realm.

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
  var foreign = $262.createRealm().global;
  var ownKeys = Object.keys;
  Object.keys = foreign.Object.keys;

  var result = Object.keys({ x: 1 });
  console.log("array:" + [
    Object.getPrototypeOf(result) === foreign.Array.prototype,
    Object.getPrototypeOf(result) === Array.prototype
  ].join("|"));

  try {
    Object.keys(null);
  } catch (error) {
    console.log("error:" + [
      error.constructor === foreign.TypeError,
      error.constructor === TypeError
    ].join("|"));
  }

  Object.keys = ownKeys;
"#;

const DIRECT_FOREIGN: &str = r#"
  var foreign = $262.createRealm().global;
  var source = { x: 1 };
  var keys = foreign.Object.keys(source);
  var values = foreign.Object.values(source);
  var entries = foreign.Object.entries(source);

  function isForeignArray(value) {
    return Object.getPrototypeOf(value) === foreign.Array.prototype &&
           Object.getPrototypeOf(value) !== Array.prototype;
  }

  console.log("direct:" + [
    isForeignArray(keys),
    isForeignArray(values),
    isForeignArray(entries),
    isForeignArray(entries[0])
  ].join("|"));

  function isForeignObject(value) {
    return Object.getPrototypeOf(value) === foreign.Object.prototype &&
           Object.getPrototypeOf(value) !== Object.prototype;
  }
  function throwsFrom(perform, foreignConstructor, mainConstructor) {
    try {
      perform();
      return false;
    } catch (error) {
      return error.constructor === foreignConstructor &&
             error.constructor !== mainConstructor;
    }
  }

  var names = foreign.Object.getOwnPropertyNames(source);
  var descriptor = foreign.Object.getOwnPropertyDescriptor(source, "x");
  var fromEntries = foreign.Object.fromEntries([["x", 1]]);
  var assignedPrimitive = foreign.Object.assign(1);
  var parsed = foreign.JSON.parse('{"nested":[1]}');
  var stringifyHolder = false;
  foreign.JSON.stringify({ x: 1 }, function (key, value) {
    if (key === "") stringifyHolder = isForeignObject(this);
    return value;
  });
  var cyclic = {};
  cyclic.self = cyclic;
  console.log("direct-more:" + [
    isForeignArray(names),
    isForeignObject(descriptor),
    isForeignObject(fromEntries),
    Object.getPrototypeOf(assignedPrimitive) === foreign.Number.prototype,
    isForeignObject(parsed),
    isForeignArray(parsed.nested),
    stringifyHolder,
    throwsFrom(function () { foreign.JSON.stringify(cyclic); }, foreign.TypeError, TypeError),
    throwsFrom(function () { foreign.Math.max(Symbol()); }, foreign.TypeError, TypeError),
    throwsFrom(function () { foreign.JSON.parse("{"); }, foreign.SyntaxError, SyntaxError)
  ].join("|"));
"#;

const TRANSPLANTED_FOREIGN: &str = r#"
  var foreign = $262.createRealm().global;
  function isForeignArray(value) {
    return Object.getPrototypeOf(value) === foreign.Array.prototype &&
           Object.getPrototypeOf(value) !== Array.prototype;
  }
  function isForeignObject(value) {
    return Object.getPrototypeOf(value) === foreign.Object.prototype &&
           Object.getPrototypeOf(value) !== Object.prototype;
  }

  var saved;
  saved = Object.values;
  Object.values = foreign.Object.values;
  var values = Object.values({ x: 1 });
  Object.values = saved;

  saved = Object.entries;
  Object.entries = foreign.Object.entries;
  var entries = Object.entries({ x: 1 });
  Object.entries = saved;

  saved = Object.getOwnPropertyNames;
  Object.getOwnPropertyNames = foreign.Object.getOwnPropertyNames;
  var names = Object.getOwnPropertyNames({ x: 1 });
  Object.getOwnPropertyNames = saved;

  saved = Object.getOwnPropertyDescriptor;
  Object.getOwnPropertyDescriptor = foreign.Object.getOwnPropertyDescriptor;
  var descriptor = Object.getOwnPropertyDescriptor({ x: 1 }, "x");
  Object.getOwnPropertyDescriptor = saved;

  saved = Object.fromEntries;
  Object.fromEntries = foreign.Object.fromEntries;
  var fromEntries = Object.fromEntries([["x", 1]]);
  Object.fromEntries = saved;

  saved = Object.assign;
  Object.assign = foreign.Object.assign;
  var assignedPrimitive = Object.assign(1);
  Object.assign = saved;

  saved = JSON.parse;
  JSON.parse = foreign.JSON.parse;
  var parsed = JSON.parse('{"nested":[1]}');
  JSON.parse = saved;

  var stringifyHolder = false;
  var stringifyError = false;
  var cyclic = {};
  cyclic.self = cyclic;
  saved = JSON.stringify;
  JSON.stringify = foreign.JSON.stringify;
  JSON.stringify({ x: 1 }, function (key, value) {
    if (key === "") stringifyHolder = isForeignObject(this);
    return value;
  });
  try {
    JSON.stringify(cyclic);
  } catch (error) {
    stringifyError = error.constructor === foreign.TypeError &&
                     error.constructor !== TypeError;
  }
  JSON.stringify = saved;

  var mathError = false;
  saved = Math.max;
  Math.max = foreign.Math.max;
  try {
    Math.max(Symbol());
  } catch (error) {
    mathError = error.constructor === foreign.TypeError &&
                error.constructor !== TypeError;
  }
  Math.max = saved;

  console.log("transplanted:" + [
    isForeignArray(values),
    isForeignArray(entries),
    isForeignArray(entries[0]),
    isForeignArray(names),
    isForeignObject(descriptor),
    isForeignObject(fromEntries),
    Object.getPrototypeOf(assignedPrimitive) === foreign.Number.prototype,
    isForeignObject(parsed),
    isForeignArray(parsed.nested),
    stringifyHolder,
    stringifyError,
    mathError
  ].join("|"));
"#;

#[test]
fn foreign_same_id_native_is_not_a_main_realm_intrinsic() {
    assert_eq!(run_ok(SEMANTICS), ["array:true|false", "error:true|false"]);
}

#[test]
fn direct_foreign_object_enumeration_uses_its_own_array_intrinsic() {
    assert_eq!(
        run_ok(DIRECT_FOREIGN),
        [
            "direct:true|true|true|true",
            "direct-more:true|true|true|true|true|true|true|true|true|true",
        ]
    );
}

#[test]
fn transplanted_foreign_intrinsics_do_not_borrow_main_realm_fast_paths() {
    assert_eq!(
        run_ok(TRANSPLANTED_FOREIGN),
        ["transplanted:true|true|true|true|true|true|true|true|true|true|true|true"]
    );
}
