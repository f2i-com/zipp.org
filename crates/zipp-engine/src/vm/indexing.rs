//! Property / index access — `obj[key]`, `obj.prop`, and their write
//! counterparts, along with `delete obj[key]`.
//!
//! The register VM's hot path for known-shape reads/writes lives in
//! `rvm.rs` (the `ROp::GetProp` / `ROp::SetProp` fast-path arms with
//! inline property caches). Anything the fast path can't handle —
//! accessors, SuperRef traversals, built-in string / array methods,
//! numeric index coercions, Map / Set / Promise method lookups — funnels
//! through [`VM::execute_index_expression`] (reads) and
//! [`VM::execute_set_index`] (writes) here.
//!
//! Extracted from `vm/mod.rs` in 0.4 so the monolith can continue to
//! shrink without behavioural change. `impl VM` in a sibling submodule
//! is fine because the two live in the same `vm` module.

use std::borrow::Cow;
use std::rc::Rc;

use crate::object::{undefined_object, BuiltinFunction, BuiltinFunctionObject, Object};
use crate::value::{obj_into_val, val_to_obj, Value};

use super::{VMError, ERR_ARRAY_SIZE, MAX_ARRAY_SIZE, SPARSE_ARRAY_THRESHOLD, VM};

impl VM {
    pub(crate) fn execute_index_expression(
        &mut self,
        left: Object,
        index: Object,
    ) -> Result<(), VMError> {
        match left {
            Object::Array(items) => {
                if let Some(idx) = Self::numeric_array_index(&index) {
                    let borrowed = items.borrow();
                    if idx >= borrowed.len() {
                        self.push_val(Value::UNDEFINED)?;
                    } else {
                        // Array stores Values — push directly, no conversion
                        self.push_val(borrowed[idx])?;
                    }
                    return Ok(());
                }

                if let Object::String(s) = &index {
                    if &**s == "length" {
                        self.push(Object::Integer(items.borrow().len() as i64))?;
                        return Ok(());
                    }

                    let builtin = match &**s {
                        "map" => Some(BuiltinFunction::ArrayMap),
                        "forEach" => Some(BuiltinFunction::ArrayForEach),
                        "flatMap" => Some(BuiltinFunction::ArrayFlatMap),
                        "flat" => Some(BuiltinFunction::ArrayFlat),
                        "reverse" => Some(BuiltinFunction::ArrayReverse),
                        "some" => Some(BuiltinFunction::ArraySome),
                        "every" => Some(BuiltinFunction::ArrayEvery),
                        "findIndex" => Some(BuiltinFunction::ArrayFindIndex),
                        "indexOf" => Some(BuiltinFunction::ArrayIndexOf),
                        "lastIndexOf" => Some(BuiltinFunction::ArrayLastIndexOf),
                        "pop" => Some(BuiltinFunction::ArrayPop),
                        "push" => Some(BuiltinFunction::ArrayPush),
                        "sort" => Some(BuiltinFunction::ArraySort),
                        "filter" => Some(BuiltinFunction::ArrayFilter),
                        "reduce" => Some(BuiltinFunction::ArrayReduce),
                        "reduceRight" => Some(BuiltinFunction::ArrayReduceRight),
                        "find" => Some(BuiltinFunction::ArrayFind),
                        "includes" => Some(BuiltinFunction::ArrayIncludes),
                        "join" => Some(BuiltinFunction::ArrayJoin),
                        "toString" => Some(BuiltinFunction::ArrayToString),
                        "valueOf" => Some(BuiltinFunction::ArrayValueOf),
                        "slice" => Some(BuiltinFunction::ArraySlice),
                        "at" => Some(BuiltinFunction::ArrayAt),
                        "toSorted" => Some(BuiltinFunction::ArrayToSorted),
                        "with" => Some(BuiltinFunction::ArrayWith),
                        "keys" => Some(BuiltinFunction::ArrayKeys),
                        "values" => Some(BuiltinFunction::ArrayValues),
                        "entries" => Some(BuiltinFunction::ArrayEntries),
                        "shift" => Some(BuiltinFunction::ArrayShift),
                        "unshift" => Some(BuiltinFunction::ArrayUnshift),
                        "splice" => Some(BuiltinFunction::ArraySplice),
                        "concat" => Some(BuiltinFunction::ArrayConcat),
                        "fill" => Some(BuiltinFunction::ArrayFill),
                        "copyWithin" => Some(BuiltinFunction::ArrayCopyWithin),
                        "findLast" => Some(BuiltinFunction::ArrayFindLast),
                        "findLastIndex" => Some(BuiltinFunction::ArrayFindLastIndex),
                        "toReversed" => Some(BuiltinFunction::ArrayToReversed),
                        _ => None,
                    };
                    if let Some(function) = builtin {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function,
                            receiver: Some(Object::Array(items)),
                        })))?;
                        return Ok(());
                    }

                    if let Some(idx) = Self::parse_non_negative_usize(s) {
                        let borrowed = items.borrow();
                        if idx >= borrowed.len() {
                            self.push_val(Value::UNDEFINED)?;
                        } else {
                            self.push_val(borrowed[idx])?;
                        }
                        return Ok(());
                    }
                }
                self.push(undefined_object())?;
                Ok(())
            }
            Object::Hash(hash) => {
                let hash_b = hash.borrow();

                // Check for getter accessor first (if the hash has any accessors).
                if hash_b.has_accessors() {
                    if let Object::String(s) = &index {
                        if let Some(getter) = hash_b.get_getter(s) {
                            let getter_func = getter.clone();
                            let _ = hash_b; // end borrow before calling accessor
                            let receiver_val =
                                obj_into_val(Object::Hash(Rc::clone(&hash)), &mut self.heap);
                            let (result, _) = self.execute_compiled_function_slice(
                                getter_func,
                                &[],
                                Some(receiver_val),
                            )?;
                            self.push_val(result)?;
                            return Ok(());
                        }
                    }
                }

                let value_ref = match &index {
                    Object::String(s) => hash_b.get_by_str(s),
                    _ => {
                        let key = self.hash_key_from_object(&index);
                        hash_b
                            .pairs
                            .get_index_of(&key)
                            .and_then(|slot| hash_b.get_value_at_slot(slot))
                    }
                };
                match value_ref {
                    Some(val) => {
                        let obj = val_to_obj(val, &self.heap);
                        if let Object::CompiledFunction(func) = &obj {
                            self.push(Object::BoundMethod(Box::new(
                                crate::object::BoundMethodObject {
                                    function: (**func).clone(),
                                    receiver: Box::new(Object::Hash(Rc::clone(&hash))),
                                },
                            )))?;
                        } else {
                            self.push(obj)?;
                        }
                    }
                    None => {
                        // Built-in methods on hash/object literals
                        if let Object::String(s) = &index {
                            if &**s == "hasOwnProperty" {
                                self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                    function: BuiltinFunction::HashHasOwnProperty,
                                    receiver: Some(Object::Hash(Rc::clone(&hash))),
                                })))?;
                                return Ok(());
                            }
                        }
                        self.push(Object::Undefined)?;
                    }
                }
                Ok(())
            }
            Object::Instance(instance) => {
                let prop: Cow<str> = match &index {
                    Object::String(s) => Cow::Borrowed(s),
                    Object::Integer(v) => Cow::Owned(v.to_string()),
                    Object::Float(v) => Cow::Owned(v.to_string()),
                    Object::Boolean(v) => Cow::Owned(v.to_string()),
                    _ => {
                        self.push(undefined_object())?;
                        return Ok(());
                    }
                };

                if let Some(value) = instance.fields.get(prop.as_ref()) {
                    self.push_val(*value)?;
                    return Ok(());
                }

                if let Some(getter) = instance.getters.get(prop.as_ref()) {
                    let receiver_val = obj_into_val(
                        Object::Instance(Box::new((*instance).clone())),
                        &mut self.heap,
                    );
                    let (result, _) = self.execute_compiled_function_slice(
                        getter.clone(),
                        &[],
                        Some(receiver_val),
                    )?;
                    self.push_val(result)?;
                    return Ok(());
                }

                if let Some(method) = instance.methods.get(prop.as_ref()) {
                    self.push(Object::BoundMethod(Box::new(
                        crate::object::BoundMethodObject {
                            function: method.clone(),
                            receiver: Box::new(Object::Instance(Box::new((*instance).clone()))),
                        },
                    )))?;
                    return Ok(());
                }

                self.push(undefined_object())?;
                Ok(())
            }
            Object::Error(err) => {
                if let Object::String(s) = &index {
                    match &**s {
                        "message" => {
                            self.push(Object::String(err.message.clone()))?;
                        }
                        "name" => {
                            self.push(Object::String(err.name.clone()))?;
                        }
                        "stack" => {
                            self.push(Object::String(Rc::from("")))?;
                        }
                        _ => {
                            self.push(undefined_object())?;
                        }
                    }
                } else {
                    self.push(undefined_object())?;
                }
                Ok(())
            }
            Object::Proxy(proxy) => {
                // Look for a `get` trap on the handler. If present, call it
                // with (target, key, receiver) and use the result. Otherwise
                // fall through to a normal property read on the target.
                if let Object::Hash(handler_rc) = &*proxy.handler {
                    let trap = {
                        let hb = handler_rc.borrow();
                        hb.get_by_str("get")
                    };
                    if let Some(trap_val) = trap {
                        if !trap_val.is_undefined() && !trap_val.is_null() {
                            let target_val = obj_into_val(
                                proxy.target.as_ref().clone(),
                                &mut self.heap,
                            );
                            let key_val = obj_into_val(index, &mut self.heap);
                            let recv_val = obj_into_val(
                                Object::Proxy(proxy.clone()),
                                &mut self.heap,
                            );
                            let result =
                                self.call_value_slice(trap_val, &[target_val, key_val, recv_val])?;
                            self.push_val(result)?;
                            return Ok(());
                        }
                    }
                }
                // No trap → forward to the underlying target.
                let target = (*proxy.target).clone();
                self.execute_index_expression(target, index)
            }
            Object::ArrayBuffer(buf_rc) => {
                if let Object::String(s) = &index {
                    match &**s {
                        "byteLength" => {
                            self.push(Object::Integer(buf_rc.borrow().len() as i64))?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                self.push(undefined_object())?;
                Ok(())
            }
            Object::TypedArray(ta) => {
                // Numeric indices read elements through the kind decoder.
                if let Some(idx) = Self::numeric_array_index(&index) {
                    if idx < ta.length {
                        let bpe = ta.kind.bytes_per_element();
                        let off = ta.byte_offset + idx * bpe;
                        let buf = ta.buffer.borrow();
                        if off + bpe <= buf.len() {
                            let n = ta.kind.read(&buf, off);
                            if n.is_finite() && n.fract() == 0.0
                                && !matches!(
                                    ta.kind,
                                    crate::object::TypedArrayKind::Float32
                                        | crate::object::TypedArrayKind::Float64
                                )
                            {
                                self.push_val(Value::from_i64(n as i64))?;
                            } else {
                                self.push_val(Value::from_f64(n))?;
                            }
                            return Ok(());
                        }
                    }
                    self.push_val(Value::UNDEFINED)?;
                    return Ok(());
                }
                if let Object::String(s) = &index {
                    match &**s {
                        "length" => {
                            self.push(Object::Integer(ta.length as i64))?;
                            return Ok(());
                        }
                        "byteLength" => {
                            self.push(Object::Integer(
                                (ta.length * ta.kind.bytes_per_element()) as i64,
                            ))?;
                            return Ok(());
                        }
                        "byteOffset" => {
                            self.push(Object::Integer(ta.byte_offset as i64))?;
                            return Ok(());
                        }
                        "buffer" => {
                            self.push(Object::ArrayBuffer(ta.buffer.clone()))?;
                            return Ok(());
                        }
                        "BYTES_PER_ELEMENT" => {
                            self.push(Object::Integer(ta.kind.bytes_per_element() as i64))?;
                            return Ok(());
                        }
                        "set" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::TypedArraySet,
                                receiver: Some(Object::TypedArray(ta)),
                            })))?;
                            return Ok(());
                        }
                        "subarray" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::TypedArraySubarray,
                                receiver: Some(Object::TypedArray(ta)),
                            })))?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                self.push(undefined_object())?;
                Ok(())
            }
            Object::DataView(dv) => {
                if let Object::String(s) = &index {
                    let fun = match &**s {
                        "byteLength" => {
                            self.push(Object::Integer(dv.byte_length as i64))?;
                            return Ok(());
                        }
                        "byteOffset" => {
                            self.push(Object::Integer(dv.byte_offset as i64))?;
                            return Ok(());
                        }
                        "buffer" => {
                            self.push(Object::ArrayBuffer(dv.buffer.clone()))?;
                            return Ok(());
                        }
                        "getInt8" => Some(BuiltinFunction::DataViewGetInt8),
                        "setInt8" => Some(BuiltinFunction::DataViewSetInt8),
                        "getUint8" => Some(BuiltinFunction::DataViewGetUint8),
                        "setUint8" => Some(BuiltinFunction::DataViewSetUint8),
                        "getInt16" => Some(BuiltinFunction::DataViewGetInt16),
                        "setInt16" => Some(BuiltinFunction::DataViewSetInt16),
                        "getUint16" => Some(BuiltinFunction::DataViewGetUint16),
                        "setUint16" => Some(BuiltinFunction::DataViewSetUint16),
                        "getInt32" => Some(BuiltinFunction::DataViewGetInt32),
                        "setInt32" => Some(BuiltinFunction::DataViewSetInt32),
                        "getUint32" => Some(BuiltinFunction::DataViewGetUint32),
                        "setUint32" => Some(BuiltinFunction::DataViewSetUint32),
                        "getFloat32" => Some(BuiltinFunction::DataViewGetFloat32),
                        "setFloat32" => Some(BuiltinFunction::DataViewSetFloat32),
                        "getFloat64" => Some(BuiltinFunction::DataViewGetFloat64),
                        "setFloat64" => Some(BuiltinFunction::DataViewSetFloat64),
                        _ => None,
                    };
                    if let Some(f) = fun {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: f,
                            receiver: Some(Object::DataView(dv)),
                        })))?;
                        return Ok(());
                    }
                }
                self.push(undefined_object())?;
                Ok(())
            }
            Object::Promise(prom) => {
                // .then / .catch / .finally — bound to this promise so the
                // builtin handler reads `builtin.receiver` correctly.
                if let Object::String(s) = &index {
                    let fun = match &**s {
                        "then" => Some(BuiltinFunction::PromiseThen),
                        "catch" => Some(BuiltinFunction::PromiseCatch),
                        "finally" => Some(BuiltinFunction::PromiseFinally),
                        _ => None,
                    };
                    if let Some(f) = fun {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: f,
                            receiver: Some(Object::Promise(prom)),
                        })))?;
                        return Ok(());
                    }
                }
                self.push(undefined_object())?;
                Ok(())
            }
            Object::Class(class_obj) => {
                let prop: Cow<str> = match &index {
                    Object::String(s) => Cow::Borrowed(s),
                    Object::Integer(v) => Cow::Owned(v.to_string()),
                    Object::Float(v) => Cow::Owned(v.to_string()),
                    Object::Boolean(v) => Cow::Owned(v.to_string()),
                    _ => {
                        self.push(undefined_object())?;
                        return Ok(());
                    }
                };

                if let Some(method) = class_obj.static_methods.get(prop.as_ref()) {
                    self.push(Object::BoundMethod(Box::new(
                        crate::object::BoundMethodObject {
                            function: method.clone(),
                            receiver: Box::new(Object::Class(Box::new((*class_obj).clone()))),
                        },
                    )))?;
                    return Ok(());
                }

                if let Some(getter) = class_obj.getters.get(prop.as_ref()) {
                    let receiver_val = obj_into_val(
                        Object::Class(Box::new((*class_obj).clone())),
                        &mut self.heap,
                    );
                    let (result, _) = self.execute_compiled_function_slice(
                        getter.clone(),
                        &[],
                        Some(receiver_val),
                    )?;
                    self.push_val(result)?;
                    return Ok(());
                }

                if let Some(field_val) = class_obj.static_fields.get(prop.as_ref()) {
                    self.push(field_val.clone())?;
                    return Ok(());
                }

                self.push(undefined_object())?;
                Ok(())
            }
            Object::SuperRef(super_ref) => {
                let prop: Cow<str> = match &index {
                    Object::String(s) => Cow::Borrowed(s),
                    Object::Integer(v) => Cow::Owned(v.to_string()),
                    Object::Float(v) => Cow::Owned(v.to_string()),
                    Object::Boolean(v) => Cow::Owned(v.to_string()),
                    _ => {
                        self.push(undefined_object())?;
                        return Ok(());
                    }
                };

                // Helper: shift the receiver's super_methods to the next
                // ancestor level so that super.X() inside the parent method
                // correctly resolves to the grandparent's methods.
                let shift_receiver = |recv: &Object, chain: &[crate::object::SuperLevel]| -> Object {
                    let mut r = recv.clone();
                    if let Object::Instance(inst) = &mut r {
                        if let Some((next_m, next_g, next_s)) = chain.first() {
                            inst.super_methods = next_m.clone();
                            inst.super_getters = next_g.clone();
                            inst.super_setters = next_s.clone();
                            inst.super_constructor_chain = chain[1..].to_vec();
                        } else {
                            inst.super_methods.clear();
                            inst.super_getters.clear();
                            inst.super_setters.clear();
                            inst.super_constructor_chain.clear();
                        }
                    }
                    r
                };

                if let Some(getter) = super_ref.getters.get(prop.as_ref()) {
                    let shifted = shift_receiver(&super_ref.receiver, &super_ref.constructor_chain);
                    let receiver_val = obj_into_val(shifted, &mut self.heap);
                    let (result, _) = self.execute_compiled_function_slice(
                        getter.clone(),
                        &[],
                        Some(receiver_val),
                    )?;
                    self.push_val(result)?;
                    return Ok(());
                }

                if let Some(method) = super_ref.methods.get(prop.as_ref()) {
                    let shifted = shift_receiver(&super_ref.receiver, &super_ref.constructor_chain);
                    self.push(Object::BoundMethod(Box::new(
                        crate::object::BoundMethodObject {
                            function: method.clone(),
                            receiver: Box::new(shifted),
                        },
                    )))?;
                    return Ok(());
                }

                self.push(undefined_object())?;
                Ok(())
            }
            Object::String(text) => {
                match index {
                    Object::String(s) => match &*s {
                        "length" => {
                            self.push(Object::Integer(Self::string_char_len(&text) as i64))?;
                        }
                        "charAt" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringCharAt,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "split" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringSplit,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "includes" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringIncludes,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "slice" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringSlice,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "toUpperCase" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringToUpperCase,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "toLowerCase" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringToLowerCase,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "trim" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringTrim,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "startsWith" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringStartsWith,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "endsWith" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringEndsWith,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "indexOf" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringIndexOf,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "lastIndexOf" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringLastIndexOf,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "substring" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringSubstring,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "repeat" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringRepeat,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "padStart" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringPadStart,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "padEnd" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringPadEnd,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "charCodeAt" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringCharCodeAt,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "replace" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringReplace,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "replaceAll" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringReplaceAll,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "match" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringMatch,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "matchAll" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringMatchAll,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "search" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringSearch,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "concat" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringConcat,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "trimStart" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringTrimStart,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "trimEnd" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringTrimEnd,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "at" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringAt,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "codePointAt" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringCodePointAt,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        "normalize" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringNormalize,
                                receiver: Some(Object::String(text)),
                            })))?;
                        }
                        _ => {
                            self.push(undefined_object())?;
                        }
                    },
                    Object::Integer(v) => {
                        if v < 0 {
                            self.push(undefined_object())?;
                        } else {
                            let idx = v as usize;
                            let ch = Self::string_nth_char(&text, idx)
                                .map(|c| Object::String(c.to_string().into()));
                            self.push(ch.unwrap_or_else(undefined_object))?;
                        }
                    }
                    _ => {
                        self.push(undefined_object())?;
                    }
                }
                Ok(())
            }
            Object::Integer(v) => {
                let key = self.object_key_cow(&index);
                match key.as_ref() {
                    "toFixed" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::NumberToFixed,
                            receiver: Some(Object::Integer(v)),
                        })))?
                    }
                    "toPrecision" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::NumberToPrecision,
                            receiver: Some(Object::Integer(v)),
                        })))?
                    }
                    "toString" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::NumberToString,
                            receiver: Some(Object::Integer(v)),
                        })))?
                    }
                    _ => self.push(undefined_object())?,
                }
                Ok(())
            }
            Object::Float(v) => {
                let key = self.object_key_cow(&index);
                match key.as_ref() {
                    "toFixed" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::NumberToFixed,
                            receiver: Some(Object::Float(v)),
                        })))?
                    }
                    "toPrecision" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::NumberToPrecision,
                            receiver: Some(Object::Float(v)),
                        })))?
                    }
                    "toString" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::NumberToString,
                            receiver: Some(Object::Float(v)),
                        })))?
                    }
                    _ => self.push(undefined_object())?,
                }
                Ok(())
            }
            Object::BuiltinFunction(builtin) => {
                let key = self.object_key_cow(&index);
                match builtin.function {
                    BuiltinFunction::StringCtor => match key.as_ref() {
                        "fromCharCode" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringFromCharCode,
                                receiver: None,
                            })))?;
                        }
                        "fromCodePoint" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringFromCodePoint,
                                receiver: None,
                            })))?;
                        }
                        "raw" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::StringRaw,
                                receiver: None,
                            })))?;
                        }
                        _ => self.push(undefined_object())?,
                    },
                    BuiltinFunction::NumberCtor => match key.as_ref() {
                        "isNaN" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::NumberIsNaN,
                                receiver: None,
                            })))?
                        }
                        "isFinite" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::NumberIsFinite,
                                receiver: None,
                            })))?
                        }
                        "parseInt" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::ParseInt,
                                receiver: None,
                            })))?
                        }
                        "parseFloat" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::ParseFloat,
                                receiver: None,
                            })))?
                        }
                        "isInteger" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::NumberIsInteger,
                                receiver: None,
                            })))?
                        }
                        "isSafeInteger" => {
                            self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                                function: BuiltinFunction::NumberIsSafeInteger,
                                receiver: None,
                            })))?
                        }
                        "MAX_SAFE_INTEGER" => self.push(Object::Integer(9007199254740991))?,
                        "MIN_SAFE_INTEGER" => self.push(Object::Integer(-9007199254740991))?,
                        "EPSILON" => self.push(Object::Float(2f64.powi(-52)))?,
                        "MAX_VALUE" => self.push(Object::Float(f64::MAX))?,
                        "MIN_VALUE" => self.push(Object::Float(5e-324_f64))?,
                        "POSITIVE_INFINITY" => self.push(Object::Float(f64::INFINITY))?,
                        "NEGATIVE_INFINITY" => self.push(Object::Float(f64::NEG_INFINITY))?,
                        _ => self.push(undefined_object())?,
                    },
                    _ => self.push(undefined_object())?,
                }
                Ok(())
            }
            Object::RegExp(re) => {
                let key = self.object_key_cow(&index);
                match key.as_ref() {
                    "test" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::RegExpTest,
                            receiver: Some(Object::RegExp(re)),
                        })))?
                    }
                    "exec" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::RegExpExec,
                            receiver: Some(Object::RegExp(re)),
                        })))?
                    }
                    _ => self.push(undefined_object())?,
                }
                Ok(())
            }
            Object::Map(map_obj) => {
                let key = self.object_key_cow(&index);
                match key.as_ref() {
                    "size" => self.push(Object::Integer(map_obj.entries.borrow().len() as i64))?,
                    "set" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::MapSet,
                            receiver: Some(Object::Map(map_obj)),
                        })))?
                    }
                    "get" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::MapGet,
                            receiver: Some(Object::Map(map_obj)),
                        })))?
                    }
                    "has" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::MapHas,
                            receiver: Some(Object::Map(map_obj)),
                        })))?
                    }
                    "delete" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::MapDelete,
                            receiver: Some(Object::Map(map_obj)),
                        })))?
                    }
                    "clear" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::MapClear,
                            receiver: Some(Object::Map(map_obj)),
                        })))?
                    }
                    "keys" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::MapKeys,
                            receiver: Some(Object::Map(map_obj)),
                        })))?
                    }
                    "values" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::MapValues,
                            receiver: Some(Object::Map(map_obj)),
                        })))?
                    }
                    "entries" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::MapEntries,
                            receiver: Some(Object::Map(map_obj)),
                        })))?
                    }
                    "forEach" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::MapForEach,
                            receiver: Some(Object::Map(map_obj)),
                        })))?
                    }
                    _ => self.push(undefined_object())?,
                }
                Ok(())
            }
            Object::Set(set_obj) => {
                let key = self.object_key_cow(&index);
                match key.as_ref() {
                    "size" => self.push(Object::Integer(set_obj.entries.borrow().len() as i64))?,
                    "add" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::SetAdd,
                            receiver: Some(Object::Set(set_obj)),
                        })))?
                    }
                    "has" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::SetHas,
                            receiver: Some(Object::Set(set_obj)),
                        })))?
                    }
                    "delete" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::SetDelete,
                            receiver: Some(Object::Set(set_obj)),
                        })))?
                    }
                    "clear" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::SetClear,
                            receiver: Some(Object::Set(set_obj)),
                        })))?
                    }
                    "keys" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::SetKeys,
                            receiver: Some(Object::Set(set_obj)),
                        })))?
                    }
                    "values" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::SetValues,
                            receiver: Some(Object::Set(set_obj)),
                        })))?
                    }
                    "entries" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::SetEntries,
                            receiver: Some(Object::Set(set_obj)),
                        })))?
                    }
                    "forEach" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::SetForEach,
                            receiver: Some(Object::Set(set_obj)),
                        })))?
                    }
                    _ => self.push(undefined_object())?,
                }
                Ok(())
            }
            Object::Generator(gen_rc) => {
                let key = self.object_key_cow(&index);
                match key.as_ref() {
                    "next" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::GeneratorNext,
                            receiver: Some(Object::Generator(gen_rc)),
                        })))?
                    }
                    "return" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::GeneratorReturn,
                            receiver: Some(Object::Generator(gen_rc)),
                        })))?
                    }
                    "throw" => {
                        self.push(Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                            function: BuiltinFunction::GeneratorThrow,
                            receiver: Some(Object::Generator(gen_rc)),
                        })))?
                    }
                    _ => self.push(undefined_object())?,
                }
                Ok(())
            }
            _ => {
                self.push(undefined_object())?;
                Ok(())
            }
        }
    }

    pub(crate) fn execute_set_index(
        &mut self,
        left: Object,
        index: Object,
        value: Object,
    ) -> Result<(), VMError> {
        match left {
            Object::Array(items) => {
                if let Some(uidx) = Self::object_to_array_index(&index) {
                    if uidx > MAX_ARRAY_SIZE {
                        return Err(VMError::TypeError(ERR_ARRAY_SIZE.to_string()));
                    }
                    let borrowed = unsafe { items.borrow_mut() };
                    if uidx >= borrowed.len() {
                        if uidx > borrowed.len() + SPARSE_ARRAY_THRESHOLD {
                            // Sparse-array guard: dense Vec storage cannot
                            // back V8-style sparse arrays, so writes beyond
                            // the threshold are silently dropped. Documented
                            // spec divergence; identical to the rvm hot path.
                            self.push(Object::Array(items))?;
                            return Ok(());
                        }
                        borrowed.resize(uidx + 1, Value::UNDEFINED);
                    }
                    borrowed[uidx] = obj_into_val(value, &mut self.heap);
                }

                self.push(Object::Array(items))?;
                Ok(())
            }
            Object::Proxy(proxy) => {
                // `set` trap: handler.set(target, key, value, receiver).
                // Convention: return the (possibly modified) value or true.
                if let Object::Hash(handler_rc) = &*proxy.handler {
                    let trap = {
                        let hb = handler_rc.borrow();
                        hb.get_by_str("set")
                    };
                    if let Some(trap_val) = trap {
                        if !trap_val.is_undefined() && !trap_val.is_null() {
                            let target_val = obj_into_val(
                                proxy.target.as_ref().clone(),
                                &mut self.heap,
                            );
                            let key_val = obj_into_val(index, &mut self.heap);
                            let value_val = obj_into_val(value, &mut self.heap);
                            let recv_val = obj_into_val(
                                Object::Proxy(proxy.clone()),
                                &mut self.heap,
                            );
                            let _ = self.call_value_slice(
                                trap_val,
                                &[target_val, key_val, value_val, recv_val],
                            )?;
                            self.push(Object::Proxy(proxy))?;
                            return Ok(());
                        }
                    }
                }
                let target = (*proxy.target).clone();
                self.execute_set_index(target, index, value)
            }
            Object::TypedArray(ta) => {
                // arr[i] = v — clamp to length, otherwise silently drop
                // (matches V8's TypedArray write-out-of-bounds semantics).
                if let Some(idx) = Self::numeric_array_index(&index) {
                    if idx < ta.length {
                        let bpe = ta.kind.bytes_per_element();
                        let off = ta.byte_offset + idx * bpe;
                        let value_val = obj_into_val(value, &mut self.heap);
                        let n = self.to_number_val(value_val).unwrap_or(0.0);
                        let mut buf = unsafe { ta.buffer.borrow_mut() };
                        if off + bpe <= buf.len() {
                            ta.kind.write(&mut buf, off, n);
                        }
                    }
                }
                self.push(Object::TypedArray(ta))?;
                Ok(())
            }
            Object::Hash(hash) => {
                // Check for setter accessor first (if the hash has any accessors).
                {
                    let hash_b = hash.borrow();
                    if hash_b.has_accessors() {
                        if let Object::String(s) = &index {
                            if let Some(setter) = hash_b.get_setter(s) {
                                let setter_func = setter.clone();
                                let _ = hash_b; // end borrow before calling accessor
                                let value_val = obj_into_val(value, &mut self.heap);
                                let receiver_val =
                                    obj_into_val(Object::Hash(Rc::clone(&hash)), &mut self.heap);
                                self.execute_compiled_function_slice(
                                    setter_func,
                                    std::slice::from_ref(&value_val),
                                    Some(receiver_val),
                                )?;
                                self.push(Object::Hash(hash))?;
                                return Ok(());
                            }
                        }
                    }
                }
                {
                    let hash_m = unsafe { hash.borrow_mut() };
                    let val = obj_into_val(value, &mut self.heap);
                    match &index {
                        Object::String(s) => {
                            hash_m.set_by_str(Rc::clone(s), val);
                        }
                        _ => {
                            let key = self.hash_key_from_object(&index);
                            hash_m.insert_pair(key, val);
                        }
                    }
                }
                self.push(Object::Hash(hash))?;
                Ok(())
            }
            Object::Instance(mut instance) => {
                let prop: Cow<str> = match &index {
                    Object::String(s) => Cow::Borrowed(s),
                    Object::Integer(v) => Cow::Owned(v.to_string()),
                    Object::Float(v) => Cow::Owned(v.to_string()),
                    Object::Boolean(v) => Cow::Owned(v.to_string()),
                    _ => {
                        self.push(Object::Instance(instance))?;
                        return Ok(());
                    }
                };
                if let Some(setter) = instance.setters.get(prop.as_ref()).cloned() {
                    let value_val = obj_into_val(value, &mut self.heap);
                    let receiver_val = obj_into_val(
                        Object::Instance(Box::new((*instance).clone())),
                        &mut self.heap,
                    );
                    let (_, receiver_after) = self.execute_compiled_function_slice(
                        setter,
                        std::slice::from_ref(&value_val),
                        Some(receiver_val),
                    )?;
                    if let Some(ra) = receiver_after {
                        if let Object::Instance(updated) = val_to_obj(ra, &self.heap) {
                            self.push(Object::Instance(updated))?;
                        } else {
                            self.push(Object::Instance(instance))?;
                        }
                    } else {
                        self.push(Object::Instance(instance))?;
                    }
                } else {
                    let val = obj_into_val(value, &mut self.heap);
                    instance.fields.insert(prop.into_owned(), val);
                    self.push(Object::Instance(instance))?;
                }
                Ok(())
            }
            Object::Class(mut class_obj) => {
                let prop: Cow<str> = match &index {
                    Object::String(s) => Cow::Borrowed(s),
                    Object::Integer(v) => Cow::Owned(v.to_string()),
                    Object::Float(v) => Cow::Owned(v.to_string()),
                    Object::Boolean(v) => Cow::Owned(v.to_string()),
                    _ => {
                        self.push(Object::Class(class_obj))?;
                        return Ok(());
                    }
                };
                // Check for setter accessor first
                if let Some(setter) = class_obj.setters.get(prop.as_ref()).cloned() {
                    let value_val = obj_into_val(value, &mut self.heap);
                    let receiver_val = obj_into_val(
                        Object::Class(Box::new((*class_obj).clone())),
                        &mut self.heap,
                    );
                    self.execute_compiled_function_slice(
                        setter,
                        std::slice::from_ref(&value_val),
                        Some(receiver_val),
                    )?;
                    self.push(Object::Class(class_obj))?;
                } else {
                    class_obj.static_fields.insert(prop.into_owned(), value);
                    self.push(Object::Class(class_obj))?;
                }
                Ok(())
            }
            Object::SuperRef(mut super_ref) => {
                let prop: Cow<str> = match &index {
                    Object::String(s) => Cow::Borrowed(s),
                    Object::Integer(v) => Cow::Owned(v.to_string()),
                    Object::Float(v) => Cow::Owned(v.to_string()),
                    Object::Boolean(v) => Cow::Owned(v.to_string()),
                    _ => {
                        self.push(Object::SuperRef(super_ref))?;
                        return Ok(());
                    }
                };

                if let Some(setter) = super_ref.setters.get(prop.as_ref()).cloned() {
                    let value_val = obj_into_val(value, &mut self.heap);
                    let receiver_val = obj_into_val((*super_ref.receiver).clone(), &mut self.heap);
                    let (_, receiver_after) = self.execute_compiled_function_slice(
                        setter,
                        std::slice::from_ref(&value_val),
                        Some(receiver_val),
                    )?;
                    if let Some(ra) = receiver_after {
                        super_ref.receiver = Box::new(val_to_obj(ra, &self.heap));
                    }
                    self.push(Object::SuperRef(super_ref))?;
                    Ok(())
                } else {
                    self.push(Object::SuperRef(super_ref))?;
                    Ok(())
                }
            }
            _ => {
                self.push(left)?;
                Ok(())
            }
        }
    }
}
