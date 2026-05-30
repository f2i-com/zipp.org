//! Built-in global namespaces (`Math`, `JSON`, `Object`, `Array`, `String`,
//! `Number`, `Promise`, `Symbol`, `RegExp`, `Error`, `Date`, `Map`, `Set`,
//! `console`, …).
//!
//! This module is a single big table of what methods each built-in
//! namespace exposes — `Math.floor` becomes `BuiltinFunction::MathFloor`,
//! `JSON.stringify` becomes `BuiltinFunction::JsonStringify`, and so on.
//! The register compiler calls [`builtin_global_object`] when it encounters
//! a known global identifier and splices the resulting `Object::Hash` into
//! the script's constants.
//!
//! Nothing here does any compilation itself; the module is pure data
//! construction. It used to live as a 1.5 k-line method on the legacy
//! stack `Compiler`; extracting it here means the stack compiler can be
//! deleted without losing the built-in surface.

use std::rc::Rc;

use crate::object::{
    make_hash, BuiltinFunction, BuiltinFunctionObject, HashKey, HashObject, Object,
};

/// Convenience: build a string-typed [`HashKey`]. All 200-odd
/// namespace entries below use this, so factoring it out keeps the
/// table below readable.
fn hash_key_string(name: &str) -> HashKey {
    HashKey::from_string(name)
}

/// Return the pre-built namespace object for a known built-in global
/// (`Math`, `JSON`, `Object`, `Array`, …) or `None` for anything else.
///
/// Callers (currently just
/// [`crate::backend::rcompiler::RCompiler`]) embed the returned value
/// directly in a script's constants table; the VM then sees the usual
/// `Object::Hash` and can dispatch through it like any other object.
pub fn builtin_global_object(name: &str) -> Option<Object> {
    match name {
        "Math" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("abs"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathAbs,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("floor"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathFloor,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("ceil"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathCeil,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("round"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathRound,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("min"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathMin,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("max"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathMax,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("pow"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathPow,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("sqrt"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathSqrt,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("trunc"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathTrunc,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("sign"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathSign,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("random"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathRandom,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("log"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathLog,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("log2"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathLog2,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("cbrt"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathCbrt,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("sin"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathSin,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("cos"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathCos,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("tan"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathTan,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("exp"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathExp,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("log10"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathLog10,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("atan2"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathAtan2,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("hypot"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathHypot,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("imul"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathImul,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("clz32"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathClz32,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("fround"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathFround,
                    receiver: None,
                })),
            );
            for (name, fun) in [
                ("acos", BuiltinFunction::MathAcos),
                ("asin", BuiltinFunction::MathAsin),
                ("atan", BuiltinFunction::MathAtan),
                ("acosh", BuiltinFunction::MathAcosh),
                ("asinh", BuiltinFunction::MathAsinh),
                ("atanh", BuiltinFunction::MathAtanh),
                ("sinh", BuiltinFunction::MathSinh),
                ("cosh", BuiltinFunction::MathCosh),
                ("tanh", BuiltinFunction::MathTanh),
                ("expm1", BuiltinFunction::MathExpm1),
                ("log1p", BuiltinFunction::MathLog1p),
            ] {
                hash.insert_pair_obj(
                    hash_key_string(name),
                    Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                        function: fun,
                        receiver: None,
                    })),
                );
            }
            hash.insert_pair_obj(
                hash_key_string("PI"),
                Object::Float(std::f64::consts::PI),
            );
            hash.insert_pair_obj(
                hash_key_string("E"),
                Object::Float(std::f64::consts::E),
            );
            hash.insert_pair_obj(
                hash_key_string("LN2"),
                Object::Float(std::f64::consts::LN_2),
            );
            hash.insert_pair_obj(
                hash_key_string("LN10"),
                Object::Float(std::f64::consts::LN_10),
            );
            hash.insert_pair_obj(
                hash_key_string("SQRT2"),
                Object::Float(std::f64::consts::SQRT_2),
            );
            Some(make_hash(hash))
        }
        "String" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::StringCtor,
            receiver: None,
        }))),
        "parseInt" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::ParseInt,
            receiver: None,
        }))),
        "parseFloat" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::ParseFloat,
            receiver: None,
        }))),
        "isNaN" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::IsNaN,
            receiver: None,
        }))),
        "isFinite" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::IsFinite,
            receiver: None,
        }))),
        "encodeURIComponent" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::EncodeURIComponent,
            receiver: None,
        }))),
        "decodeURIComponent" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::DecodeURIComponent,
            receiver: None,
        }))),
        "encodeURI" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::EncodeURI,
            receiver: None,
        }))),
        "decodeURI" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::DecodeURI,
            receiver: None,
        }))),
        "structuredClone" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::StructuredClone,
            receiver: None,
        }))),
        "Infinity" => Some(Object::Float(f64::INFINITY)),
        "NaN" => Some(Object::Float(f64::NAN)),
        "undefined" => Some(Object::Undefined),
        "Number" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::NumberCtor,
            receiver: None,
        }))),
        "Array" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("from"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ArrayFrom,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("isArray"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ArrayIsArray,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("of"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ArrayOf,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }
        "RegExp" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::RegExpCtor,
            receiver: None,
        }))),
        "Map" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::MapCtor,
            receiver: None,
        }))),
        "Set" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::SetCtor,
            receiver: None,
        }))),
        "globalThis" => {
            let mut hash = HashObject::default();

            let mut math = HashObject::default();
            math.insert_pair_obj(
                hash_key_string("abs"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathAbs,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("floor"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathFloor,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("ceil"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathCeil,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("round"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathRound,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("min"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathMin,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("max"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathMax,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("pow"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathPow,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("sqrt"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathSqrt,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("trunc"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathTrunc,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("sign"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathSign,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("random"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathRandom,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("log"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathLog,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("log2"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathLog2,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("cbrt"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathCbrt,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("sin"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathSin,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("cos"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathCos,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("tan"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathTan,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("exp"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathExp,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("log10"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathLog10,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("atan2"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathAtan2,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("hypot"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathHypot,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("imul"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathImul,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("clz32"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathClz32,
                    receiver: None,
                })),
            );
            math.insert_pair_obj(
                hash_key_string("fround"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MathFround,
                    receiver: None,
                })),
            );
            for (name, fun) in [
                ("acos", BuiltinFunction::MathAcos),
                ("asin", BuiltinFunction::MathAsin),
                ("atan", BuiltinFunction::MathAtan),
                ("acosh", BuiltinFunction::MathAcosh),
                ("asinh", BuiltinFunction::MathAsinh),
                ("atanh", BuiltinFunction::MathAtanh),
                ("sinh", BuiltinFunction::MathSinh),
                ("cosh", BuiltinFunction::MathCosh),
                ("tanh", BuiltinFunction::MathTanh),
                ("expm1", BuiltinFunction::MathExpm1),
                ("log1p", BuiltinFunction::MathLog1p),
            ] {
                math.insert_pair_obj(
                    hash_key_string(name),
                    Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                        function: fun,
                        receiver: None,
                    })),
                );
            }
            math.insert_pair_obj(
                hash_key_string("PI"),
                Object::Float(std::f64::consts::PI),
            );
            math.insert_pair_obj(
                hash_key_string("E"),
                Object::Float(std::f64::consts::E),
            );
            math.insert_pair_obj(
                hash_key_string("LN2"),
                Object::Float(std::f64::consts::LN_2),
            );
            math.insert_pair_obj(
                hash_key_string("LN10"),
                Object::Float(std::f64::consts::LN_10),
            );
            math.insert_pair_obj(
                hash_key_string("SQRT2"),
                Object::Float(std::f64::consts::SQRT_2),
            );

            hash.insert_pair_obj(hash_key_string("Math"), make_hash(math));
            hash.insert_pair_obj(
                hash_key_string("String"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::StringCtor,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("parseInt"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ParseInt,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("parseFloat"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ParseFloat,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("isNaN"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::IsNaN,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("isFinite"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::IsFinite,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("encodeURIComponent"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::EncodeURIComponent,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("decodeURIComponent"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DecodeURIComponent,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("encodeURI"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::EncodeURI,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("decodeURI"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DecodeURI,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("Infinity"),
                Object::Float(f64::INFINITY),
            );
            hash.insert_pair_obj(hash_key_string("NaN"), Object::Float(f64::NAN));
            hash.insert_pair_obj(hash_key_string("undefined"), Object::Undefined);

            hash.insert_pair_obj(
                hash_key_string("Number"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::NumberCtor,
                    receiver: None,
                })),
            );
            let mut array_ns = HashObject::default();
            array_ns.insert_pair_obj(
                hash_key_string("from"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ArrayFrom,
                    receiver: None,
                })),
            );
            array_ns.insert_pair_obj(
                hash_key_string("isArray"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ArrayIsArray,
                    receiver: None,
                })),
            );
            array_ns.insert_pair_obj(
                hash_key_string("of"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ArrayOf,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(hash_key_string("Array"), make_hash(array_ns));
            hash.insert_pair_obj(
                hash_key_string("RegExp"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::RegExpCtor,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("Map"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::MapCtor,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("Set"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::SetCtor,
                    receiver: None,
                })),
            );

            let mut json_ns = HashObject::default();
            json_ns.insert_pair_obj(
                hash_key_string("stringify"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::JsonStringify,
                    receiver: None,
                })),
            );
            json_ns.insert_pair_obj(
                hash_key_string("parse"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::JsonParse,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(hash_key_string("JSON"), make_hash(json_ns));

            let mut promise_ns = HashObject::default();
            for (name, fun) in [
                ("resolve", BuiltinFunction::PromiseResolve),
                ("reject", BuiltinFunction::PromiseReject),
                ("all", BuiltinFunction::PromiseAll),
                ("race", BuiltinFunction::PromiseRace),
                ("allSettled", BuiltinFunction::PromiseAllSettled),
                ("any", BuiltinFunction::PromiseAny),
            ] {
                promise_ns.insert_pair_obj(
                    hash_key_string(name),
                    Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                        function: fun,
                        receiver: None,
                    })),
                );
            }
            hash.insert_pair_obj(hash_key_string("Promise"), make_hash(promise_ns));

            let mut object_ns = HashObject::default();
            object_ns.insert_pair_obj(
                hash_key_string("keys"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectKeys,
                    receiver: None,
                })),
            );
            object_ns.insert_pair_obj(
                hash_key_string("values"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectValues,
                    receiver: None,
                })),
            );
            object_ns.insert_pair_obj(
                hash_key_string("entries"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectEntries,
                    receiver: None,
                })),
            );
            object_ns.insert_pair_obj(
                hash_key_string("fromEntries"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectFromEntries,
                    receiver: None,
                })),
            );
            object_ns.insert_pair_obj(
                hash_key_string("hasOwn"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectHasOwn,
                    receiver: None,
                })),
            );
            object_ns.insert_pair_obj(
                hash_key_string("is"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectIs,
                    receiver: None,
                })),
            );
            object_ns.insert_pair_obj(
                hash_key_string("assign"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectAssign,
                    receiver: None,
                })),
            );
            object_ns.insert_pair_obj(
                hash_key_string("freeze"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectFreeze,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(hash_key_string("Object"), make_hash(object_ns));

            Some(make_hash(hash))
        }
        "Object" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("keys"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectKeys,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("values"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectValues,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("entries"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectEntries,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("fromEntries"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectFromEntries,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("hasOwn"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectHasOwn,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("is"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectIs,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("assign"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectAssign,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("freeze"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectFreeze,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("create"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectCreate,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("defineProperty"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectDefineProperty,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getPrototypeOf"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectGetPrototypeOf,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getOwnPropertyDescriptor"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectGetOwnPropertyDescriptor,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getOwnPropertyNames"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectGetOwnPropertyNames,
                    receiver: None,
                })),
            );
            // prototype with hasOwnProperty and toString
            let mut proto = HashObject::default();
            proto.insert_pair_obj(
                hash_key_string("hasOwnProperty"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::HashHasOwnProperty,
                    receiver: None,
                })),
            );
            proto.insert_pair_obj(
                hash_key_string("toString"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectPrototypeToString,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("prototype"),
                make_hash(proto),
            );
            Some(make_hash(hash))
        }
        "Error" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::ErrorConstructor,
            receiver: None,
        }))),
        "JSON" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("stringify"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::JsonStringify,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("parse"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::JsonParse,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }
        "Promise" => {
            // Promise needs to be both indexable (Promise.resolve etc.)
            // *and* invokable via `new Promise(executor)`. We expose
            // static methods as properties of a Hash and tag the same
            // hash with a `__construct` sentinel pointing at
            // PromiseExecutorCtor. The New opcode handler picks that
            // sentinel up and routes the call there.
            let mut hash = HashObject::default();
            for (name, fun) in [
                ("resolve", BuiltinFunction::PromiseResolve),
                ("reject", BuiltinFunction::PromiseReject),
                ("all", BuiltinFunction::PromiseAll),
                ("race", BuiltinFunction::PromiseRace),
                ("allSettled", BuiltinFunction::PromiseAllSettled),
                ("any", BuiltinFunction::PromiseAny),
            ] {
                hash.insert_pair_obj(
                    hash_key_string(name),
                    Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                        function: fun,
                        receiver: None,
                    })),
                );
            }
            hash.insert_pair_obj(
                hash_key_string("__construct"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::PromiseExecutorCtor,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }
        "ArrayBuffer" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::ArrayBufferCtor,
            receiver: None,
        }))),
        "DataView" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::DataViewCtor,
            receiver: None,
        }))),
        "Int8Array" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::Int8ArrayCtor,
            receiver: None,
        }))),
        "Uint8Array" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::Uint8ArrayCtor,
            receiver: None,
        }))),
        "Uint8ClampedArray" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::Uint8ClampedArrayCtor,
            receiver: None,
        }))),
        "Int16Array" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::Int16ArrayCtor,
            receiver: None,
        }))),
        "Uint16Array" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::Uint16ArrayCtor,
            receiver: None,
        }))),
        "Int32Array" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::Int32ArrayCtor,
            receiver: None,
        }))),
        "Uint32Array" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::Uint32ArrayCtor,
            receiver: None,
        }))),
        "Float32Array" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::Float32ArrayCtor,
            receiver: None,
        }))),
        "Float64Array" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::Float64ArrayCtor,
            receiver: None,
        }))),
        "BigInt" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::BigIntCtor,
            receiver: None,
        }))),
        "Proxy" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::ProxyCtor,
            receiver: None,
        }))),
        "queueMicrotask" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::QueueMicrotask,
            receiver: None,
        }))),
        "Reflect" => {
            let mut hash = HashObject::default();
            for (name, fun) in [
                ("get", BuiltinFunction::ReflectGet),
                ("set", BuiltinFunction::ReflectSet),
                ("has", BuiltinFunction::ReflectHas),
                ("deleteProperty", BuiltinFunction::ReflectDeleteProperty),
                ("ownKeys", BuiltinFunction::ReflectOwnKeys),
                ("getPrototypeOf", BuiltinFunction::ReflectGetPrototypeOf),
                ("setPrototypeOf", BuiltinFunction::ReflectSetPrototypeOf),
                ("isExtensible", BuiltinFunction::ReflectIsExtensible),
                ("preventExtensions", BuiltinFunction::ReflectPreventExtensions),
                ("defineProperty", BuiltinFunction::ReflectDefineProperty),
                ("getOwnPropertyDescriptor", BuiltinFunction::ReflectGetOwnPropertyDescriptor),
                ("apply", BuiltinFunction::ReflectApply),
                ("construct", BuiltinFunction::ReflectConstruct),
            ] {
                hash.insert_pair_obj(
                    hash_key_string(name),
                    Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                        function: fun,
                        receiver: None,
                    })),
                );
            }
            Some(make_hash(hash))
        }
        "TypeError" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::TypeErrorConstructor,
            receiver: None,
        }))),
        "RangeError" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::RangeErrorConstructor,
            receiver: None,
        }))),
        "SyntaxError" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::SyntaxErrorConstructor,
            receiver: None,
        }))),
        "ReferenceError" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::ReferenceErrorConstructor,
            receiver: None,
        }))),
        // WeakMap / WeakSet alias to Map / Set. Real "weak" semantics
        // require a tracing GC: a WeakMap entry should disappear once
        // nothing else strongly references the key. The engine's heap
        // (`crate::value::Heap`) keeps strong refs to every object it
        // allocates until its own GC pass runs, and that GC isn't
        // weak-aware, so a Weak<...>-backed WeakMap couldn't see a key
        // become collectable. Until the heap learns weak handles,
        // WeakMap is a Map and WeakSet is a Set — code that depends on
        // entries vanishing on key collection is the documented failure
        // mode here.
        "WeakMap" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::MapCtor,
            receiver: None,
        }))),
        "WeakSet" => Some(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
            function: BuiltinFunction::SetCtor,
            receiver: None,
        }))),
        "Date" => {
            let mut hash = HashObject::default();
            for (name, fun) in [
                ("now", BuiltinFunction::DateNow),
                ("parse", BuiltinFunction::DateParse),
                ("UTC", BuiltinFunction::DateUtc),
            ] {
                hash.insert_pair_obj(
                    hash_key_string(name),
                    Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                        function: fun,
                        receiver: None,
                    })),
                );
            }
            Some(make_hash(hash))
        }
        "localStorage" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("getItem"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LocalStorageGetItem,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("setItem"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LocalStorageSetItem,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("removeItem"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LocalStorageRemoveItem,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("clear"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LocalStorageClear,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }
        "db" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("query"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DbQuery,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("create"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DbCreate,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("update"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DbUpdate,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("delete"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DbDelete,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("hardDelete"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DbHardDelete,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("get"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DbGet,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("startSync"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DbStartSync,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("stopSync"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DbStopSync,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getSyncStatus"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DbGetSyncStatus,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getSavedSyncRoom"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DbGetSavedSyncRoom,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }
        // ── http ──
        "http" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("get"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::HttpGet,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("post"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::HttpPost,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("put"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::HttpPut,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("delete"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::HttpDelete,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }
        // ── fs ──
        "fs" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("readFile"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::FsReadFile,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("writeFile"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::FsWriteFile,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("appendFile"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::FsAppendFile,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("exists"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::FsExists,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("listDir"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::FsListDir,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("deleteFile"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::FsDeleteFile,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("mkdir"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::FsMkdir,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }
        // ── env ──
        "env" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("get"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::EnvGet,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("keys"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::EnvKeys,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("log"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::EnvLog,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }
        // ── draw ──
        "draw" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("rect"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawRect,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("roundedRect"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawRoundedRect,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("circle"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawCircle,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("ellipse"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawEllipse,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("line"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawLine,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("path"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawPath,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("text"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawText,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("image"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawImage,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("linearGradient"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawLinearGradient,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("radialGradient"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawRadialGradient,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("shadow"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawShadow,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("pushClip"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawPushClip,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("popClip"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawPopClip,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("pushTransform"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawPushTransform,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("popTransform"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawPopTransform,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("pushOpacity"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawPushOpacity,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("popOpacity"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawPopOpacity,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("arc"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawArc,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("measureText"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawMeasureText,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getViewportWidth"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawGetViewportWidth,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getViewportHeight"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::DrawGetViewportHeight,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }

        // ── layout ──
        "layout" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("createNode"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LayoutCreateNode,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("updateStyle"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LayoutUpdateStyle,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("setChildren"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LayoutSetChildren,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("computeLayout"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LayoutComputeLayout,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getLayout"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LayoutGetLayout,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("removeNode"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LayoutRemoveNode,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("clear"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::LayoutClear,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }

        // ── input ──
        "input" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("getMouseX"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputGetMouseX,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getMouseY"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputGetMouseY,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("isMouseDown"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputIsMouseDown,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("isMousePressed"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputIsMousePressed,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("isMouseReleased"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputIsMouseReleased,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getScrollY"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputGetScrollY,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("setCursor"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputSetCursor,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getTextInput"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputGetTextInput,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("isBackspacePressed"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputIsBackspacePressed,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("isEscapePressed"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputIsEscapePressed,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("requestRedraw"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputRequestRedraw,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getElapsedSecs"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputGetElapsedSecs,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getPageElapsedSecs"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputGetPageElapsedSecs,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getDeltaTime"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputGetDeltaTime,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("getFocusedInput"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputGetFocusedInput,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("setFocusedInput"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputSetFocusedInput,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("isKeyDown"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::InputIsKeyDown,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }

        "host" => {
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("call"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::HostCall,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(
                hash_key_string("callSync"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::HostCallSync,
                    receiver: None,
                })),
            );
            Some(make_hash(hash))
        }

        "Symbol" => {
            // Symbol is both callable (creates unique symbols) and has well-known
            // symbol properties (e.g. Symbol.iterator). We represent it as a Hash
            // with a __call__ entry plus static symbol properties.
            let mut hash = HashObject::default();
            hash.insert_pair_obj(
                hash_key_string("__call__"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::SymbolCtor,
                    receiver: None,
                })),
            );
            // Well-known symbol: Symbol.iterator (id=1)
            hash.insert_pair_obj(
                hash_key_string("iterator"),
                Object::Symbol(1, Some(Rc::from("Symbol.iterator"))),
            );
            Some(make_hash(hash))
        }

        _ => None,
    }
}
