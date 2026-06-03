#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// The bare name of a callable value, for the `function <name>() { [native
    /// code] }` form of `toString`. Synthetic names (`<arrow>`, `<anonymous>`)
    /// and `Class.method` qualifiers are stripped; unknown → empty.
    pub(crate) fn callable_name(&self, v: Value) -> String {
        if !v.is_heap() {
            return String::new();
        }
        let raw: String = match self.heap.get(v.heap_index()) {
            HeapObj::Func(id) => self.func(*id as usize).name.clone(),
            HeapObj::Closure { func, .. } => self.func(*func as usize).name.clone(),
            HeapObj::Class(c) => c.name.clone(),
            HeapObj::Native(nid) => native::static_name_length(*nid)
                .map(|(n, _)| n.to_string())
                .or_else(|| native::proto_method(*nid).map(|(n, _, _)| n.to_string()))
                .unwrap_or_default(),
            _ => String::new(),
        };
        if raw.is_empty() || raw.starts_with('<') {
            String::new()
        } else {
            raw.rsplit('.').next().unwrap_or(&raw).to_string()
        }
    }

    /// Invoke a native (built-in) function by id with `this` and `args`. Backs
    /// first-class builtin values (`Object.defineProperty`, `Array.isArray`,
    /// `Object.prototype.hasOwnProperty`, `Function.prototype.call`, …).
    pub(crate) fn call_native(&mut self, id: u16, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        use native::*;
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        // Temporal prototype field getter: brand-check `this` is a Temporal
        // instance, then read the field (the fast get_member path computes it).
        if (native::TEMPORAL_GETTER_BASE
            ..native::TEMPORAL_GETTER_BASE + native::TEMPORAL_GETTER_FIELDS.len() as u16)
            .contains(&id)
        {
            if !matches!(
                this.is_heap().then(|| self.heap.get(this.heap_index())),
                Some(HeapObj::Temporal { .. })
            ) {
                return Err(Thrown(
                    "TypeError: Temporal field getter called on a non-Temporal receiver".into(),
                ));
            }
            let field = native::TEMPORAL_GETTER_FIELDS[(id - native::TEMPORAL_GETTER_BASE) as usize];
            return self.get_prop(this, field);
        }
        Ok(match id {
            OBJ_DEFINE_PROPERTY => {
                let key = self.to_property_key(a1)?;
                self.object_define_property(a0, &key, args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
                a0
            }
            OBJ_DEFINE_PROPERTIES => {
                self.object_define_properties(a0, a1)?;
                a0
            }
            OBJ_GET_OWN_DESC => {
                let key = self.to_property_key(a1)?;
                self.object_get_own_property_descriptor(a0, &key)
            }
            OBJ_GET_OWN_NAMES => self.object_own_property_names(a0),
            OBJ_GET_PROTO => self.object_get_prototype_of(a0),
            OBJ_KEYS => self.object_enum_own(a0, EnumWhat::Keys),
            OBJ_VALUES => self.object_enum_own(a0, EnumWhat::Values),
            OBJ_ENTRIES => self.object_enum_own(a0, EnumWhat::Entries),
            OBJ_ASSIGN => self.object_assign(args)?,
            OBJ_CREATE => {
                let o = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                if a0 != Value::UNDEFINED {
                    self.proto_of.insert(o.heap_index(), a0);
                }
                if a1 != Value::UNDEFINED {
                    self.object_define_properties(o, a1)?;
                }
                o
            }
            PROTO_HAS_OWN => {
                let k = self.to_property_key(a0)?;
                Value::bool(self.has_own_property(this, &k))
            }
            PROTO_PROP_ENUM => {
                let k = self.to_property_key(a0)?;
                Value::bool(self.own_is_enumerable(this, &k))
            }
            PROTO_IS_PROTO_OF => Value::bool(self.is_prototype_of(this, a0)),
            PROTO_VALUE_OF => this,
            PROTO_TO_STRING => {
                let tag = self.object_to_string_tag(this)?;
                self.alloc_str(format!("[object {tag}]"))
            }
            ERROR_TO_STRING => {
                // `name` (default "Error") + ": " + `message` (default ""), dropping
                // the separator when either part is empty.
                let nv = self.get_prop(this, "name")?;
                let name =
                    if nv == Value::UNDEFINED { "Error".to_string() } else { self.to_js_string(nv)? };
                let mv = self.get_prop(this, "message")?;
                let msg = if mv == Value::UNDEFINED { String::new() } else { self.to_js_string(mv)? };
                let s = if name.is_empty() {
                    msg
                } else if msg.is_empty() {
                    name
                } else {
                    format!("{name}: {msg}")
                };
                self.alloc_str(s)
            }
            SYMBOL_TO_STRING => {
                // `Symbol.prototype.toString` → "Symbol(description)".
                let desc = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::Symbol { desc, .. }) => *desc,
                    _ => {
                        return Err(Thrown(
                            "TypeError: Symbol.prototype.toString requires that 'this' be a Symbol"
                                .into(),
                        ))
                    }
                };
                let d = if desc == Value::UNDEFINED { String::new() } else { self.display(desc) };
                self.alloc_str(format!("Symbol({d})"))
            }
            SYMBOL_VALUE_OF => {
                // `Symbol.prototype.valueOf` → the Symbol primitive itself.
                if matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Symbol { .. })
                ) {
                    this
                } else {
                    return Err(Thrown(
                        "TypeError: Symbol.prototype.valueOf requires that 'this' be a Symbol".into(),
                    ));
                }
            }
            SYMBOL_TO_PRIMITIVE => {
                // `Symbol.prototype[Symbol.toPrimitive](hint)` → the Symbol itself.
                if matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Symbol { .. })
                ) {
                    this
                } else {
                    return Err(Thrown(
                        "TypeError: Symbol.prototype[Symbol.toPrimitive] requires that 'this' be a Symbol"
                            .into(),
                    ));
                }
            }
            SYMBOL_DESCRIPTION_GET => {
                // `get Symbol.prototype.description` → the symbol's description.
                match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::Symbol { desc, .. }) => *desc,
                    _ => {
                        return Err(Thrown(
                            "TypeError: Symbol.prototype.description getter requires that 'this' be a Symbol"
                                .into(),
                        ))
                    }
                }
            }
            STR_ITERATOR => {
                // `String.prototype[Symbol.iterator]()` — RequireObjectCoercible +
                // ToString, then a String Iterator yielding one code POINT at a time.
                if this.is_nullish() {
                    return Err(Thrown(
                        "TypeError: String.prototype[Symbol.iterator] called on null or undefined"
                            .into(),
                    ));
                }
                let s = self.to_js_string(this)?;
                let cps: Vec<Value> = s.chars().map(|c| self.alloc_str(c.to_string())).collect();
                self.make_iterator(cps, self.string_iter_proto)
            }
            SYMBOL_FOR => {
                // `Symbol.for(key)`: shared registry symbol for the ToString(key).
                let key = self.to_js_string(a0)?;
                if let Some(&sym) = self.symbol_registry.get(&key) {
                    sym
                } else {
                    let desc = self.alloc_str(key.clone());
                    let prop_key = format!("@@for:{key}");
                    let sym = self.make_named_symbol(desc, &prop_key);
                    self.symbol_registry.insert(key, sym);
                    sym
                }
            }
            SYMBOL_KEY_FOR => {
                // `Symbol.keyFor(sym)`: the registry key for a registered symbol, else undefined.
                if !matches!(
                    a0.is_heap().then(|| self.heap.get(a0.heap_index())),
                    Some(HeapObj::Symbol { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: Symbol.keyFor requires that the argument be a Symbol".into(),
                    ));
                }
                let key =
                    self.symbol_registry.iter().find(|(_, v)| v.bits() == a0.bits()).map(|(k, _)| k.clone());
                match key {
                    Some(k) => self.alloc_str(k),
                    None => Value::UNDEFINED,
                }
            }
            BIGINT_TO_STRING => {
                let n = match self.bigint_value(this) {
                    Some(n) => n,
                    None => {
                        return Err(Thrown(
                            "TypeError: BigInt.prototype.toString requires that 'this' be a BigInt".into(),
                        ))
                    }
                };
                let radix = if a0 == Value::UNDEFINED { 10 } else { self.to_number(a0)? as i64 };
                if !(2..=36).contains(&radix) {
                    return Err(Thrown("RangeError: toString() radix must be between 2 and 36".into()));
                }
                self.alloc_str(bigint_to_radix(n, radix as u32))
            }
            BIGINT_VALUE_OF => {
                if self.bigint_value(this).is_some() {
                    this
                } else {
                    return Err(Thrown(
                        "TypeError: BigInt.prototype.valueOf requires that 'this' be a BigInt".into(),
                    ));
                }
            }
            BIGINT_AS_INTN => {
                let bits = self.to_number(a0)?;
                if !bits.is_finite() || bits < 0.0 {
                    return Err(Thrown("RangeError: Invalid bits for BigInt.asIntN".into()));
                }
                let x = self.to_bigint(a1)?;
                self.make_bigint(bigint_as_intn(bits as u32, x))
            }
            BIGINT_AS_UINTN => {
                let bits = self.to_number(a0)?;
                if !bits.is_finite() || bits < 0.0 {
                    return Err(Thrown("RangeError: Invalid bits for BigInt.asUintN".into()));
                }
                let x = self.to_bigint(a1)?;
                self.make_bigint(bigint_as_uintn(bits as u32, x))
            }
            REGEXP_EXEC => {
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::RegExp { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.exec called on a non-RegExp".into(),
                    ));
                }
                self.regexp_exec(this.heap_index(), a0)?
            }
            REGEXP_TEST => {
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::RegExp { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.test called on a non-RegExp".into(),
                    ));
                }
                let r = self.regexp_exec(this.heap_index(), a0)?;
                Value::bool(r != Value::NULL)
            }
            REGEXP_COMPILE => {
                // RegExp.prototype.compile(pattern, flags): recompile in place.
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::RegExp { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.compile called on a non-RegExp".into(),
                    ));
                }
                // Reuse the constructor path (validates flags, builds the matcher),
                // then move the freshly built fields into the receiver.
                let built = self.build_regexp(a0, a1)?;
                let (source, flags) = match self.heap.get(built.heap_index()) {
                    HeapObj::RegExp { source, flags, .. } => (source.clone(), flags.clone()),
                    _ => unreachable!(),
                };
                // Rebuild the matcher from the validated source/flags.
                let mut rflags = String::new();
                for c in flags.chars() {
                    match c {
                        'i' | 'm' | 's' => rflags.push(c),
                        'u' | 'v' if !rflags.contains('u') => rflags.push('u'),
                        _ => {}
                    }
                }
                let regex = regress::Regex::with_flags(&source, rflags.as_str()).map_err(|e| {
                    Thrown(format!("SyntaxError: Invalid regular expression: /{source}/: {e}"))
                })?;
                if let HeapObj::RegExp { regex: r, source: s, flags: fl, last_index } =
                    self.heap.get_mut(this.heap_index())
                {
                    *r = Box::new(regex);
                    *s = source;
                    *fl = flags;
                    *last_index = 0;
                }
                this
            }
            REGEXP_ESCAPE => {
                // RegExp.escape(S): escape S so it matches itself literally. Throws
                // TypeError unless S is a String (no coercion).
                if !(a0.is_heap() && self.heap.is_str_like(a0.heap_index())) {
                    return Err(Thrown(
                        "TypeError: RegExp.escape called with a non-string argument".into(),
                    ));
                }
                let s = self.to_js_string(a0)?;
                // EncodeForRegExpEscape's "other punctuators" / WhiteSpace /
                // LineTerminator / lone-surrogate set: hex-escaped (\xNN if <=0xFF,
                // else \uNNNN per UTF-16 code unit). Tab/VT/FF/LF/CR use the control
                // escapes below, so they are excluded here.
                let other = |u: u32| -> bool {
                    matches!(
                        u,
                        // ,-=<>#&!%:;@~'`"
                        0x2c | 0x2d | 0x3d | 0x3c | 0x3e | 0x23 | 0x26 | 0x21 | 0x25 | 0x3a
                            | 0x3b | 0x40 | 0x7e | 0x27 | 0x60 | 0x22
                        // WhiteSpace (minus tab/VT/FF) + ZWNBSP
                            | 0x20 | 0xA0 | 0x1680 | 0x202F | 0x205F | 0x3000 | 0xFEFF
                        // LineTerminator (minus LF/CR)
                            | 0x2028 | 0x2029
                    ) || (0x2000..=0x200A).contains(&u)
                        || (0xD800..=0xDFFF).contains(&u)
                };
                let mut out = String::new();
                for c in s.chars() {
                    let u = c as u32;
                    if out.is_empty() && (c.is_ascii_digit() || c.is_ascii_alphabetic()) {
                        // A leading digit/letter is hex-escaped so the escape can't
                        // fuse with a preceding regex token (e.g. \0, a quantifier).
                        out.push_str(&format!("\\x{u:02x}"));
                        continue;
                    }
                    match c {
                        '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']'
                        | '{' | '}' | '|' | '/' => {
                            out.push('\\');
                            out.push(c);
                        }
                        '\t' => out.push_str("\\t"),
                        '\n' => out.push_str("\\n"),
                        '\u{0b}' => out.push_str("\\v"),
                        '\u{0c}' => out.push_str("\\f"),
                        '\r' => out.push_str("\\r"),
                        _ if other(u) => {
                            if u <= 0xFF {
                                out.push_str(&format!("\\x{u:02x}"));
                            } else {
                                let mut buf = [0u16; 2];
                                for cu in c.encode_utf16(&mut buf) {
                                    out.push_str(&format!("\\u{cu:04x}"));
                                }
                            }
                        }
                        _ => out.push(c),
                    }
                }
                self.alloc_str(out)
            }
            REGEXP_TO_STRING => {
                let (src, flg) = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::RegExp { source, flags, .. }) => {
                        let (s, f) = (source.clone(), flags.clone());
                        (self.escaped_source(&s), f)
                    }
                    _ => {
                        let s = self.get_prop(this, "source")?;
                        let f = self.get_prop(this, "flags")?;
                        (self.to_js_string(s)?, self.to_js_string(f)?)
                    }
                };
                self.alloc_str(format!("/{src}/{flg}"))
            }
            REGEXP_GET_SOURCE => {
                if !this.is_heap() {
                    return Err(Thrown(
                        "TypeError: get source called on a non-object".into(),
                    ));
                }
                let idx = this.heap_index();
                if idx == self.regexp_proto {
                    self.alloc_str("(?:)".to_string())
                } else if let HeapObj::RegExp { source, .. } = self.heap.get(idx) {
                    let s = source.clone();
                    let esc = self.escaped_source(&s);
                    self.alloc_str(esc)
                } else {
                    return Err(Thrown(
                        "TypeError: get source called on a non-RegExp object".into(),
                    ));
                }
            }
            REGEXP_GET_FLAGS => {
                // Generic getter: Type(R) must be Object; reads each flag property.
                let is_obj = this.is_heap()
                    && !matches!(
                        self.heap.get(this.heap_index()),
                        HeapObj::Str(_)
                            | HeapObj::Cons { .. }
                            | HeapObj::BigInt(_)
                            | HeapObj::Symbol { .. }
                    );
                if !is_obj {
                    return Err(Thrown(
                        "TypeError: get flags called on a non-object".into(),
                    ));
                }
                let mut out = String::new();
                for (prop, ch) in [
                    ("hasIndices", 'd'),
                    ("global", 'g'),
                    ("ignoreCase", 'i'),
                    ("multiline", 'm'),
                    ("dotAll", 's'),
                    ("unicode", 'u'),
                    ("unicodeSets", 'v'),
                    ("sticky", 'y'),
                ] {
                    let v = self.get_prop(this, prop)?;
                    if self.truthy(v) {
                        out.push(ch);
                    }
                }
                self.alloc_str(out)
            }
            REGEXP_SYM_SEARCH => {
                if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::RegExp { .. })
                {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype[Symbol.search] called on a non-RegExp".into(),
                    ));
                }
                self.regexp_search_impl(this.heap_index(), a0)?
            }
            REGEXP_SYM_MATCH => {
                if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::RegExp { .. })
                {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype[Symbol.match] called on a non-RegExp".into(),
                    ));
                }
                self.regexp_match_impl(this.heap_index(), a0)?
            }
            REGEXP_SYM_SPLIT => {
                if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::RegExp { .. })
                {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype[Symbol.split] called on a non-RegExp".into(),
                    ));
                }
                self.regexp_split_impl(this.heap_index(), a0, a1)?
            }
            REGEXP_SYM_REPLACE => {
                if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::RegExp { .. })
                {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype[Symbol.replace] called on a non-RegExp".into(),
                    ));
                }
                let s = self.to_js_string(a0)?;
                let re = this.heap_index();
                let global =
                    matches!(self.heap.get(re), HeapObj::RegExp { flags, .. } if flags.contains('g'));
                if global {
                    self.set_regexp_last_index(re, 0);
                }
                let out = self.regex_replace(&s, re, a1, global)?;
                self.alloc_str(out)
            }
            REGEXP_SYM_MATCHALL => {
                // RegExp.prototype[Symbol.matchAll](string): an iterator over all
                // matches. Eagerly computed (no user-overridable exec).
                if !this.is_heap() {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype[Symbol.matchAll] called on a non-object".into(),
                    ));
                }
                let s = self.to_js_string(a0)?;
                let s_val = self.alloc_str(s.clone());
                let flags_v = self.get_prop(this, "flags")?;
                let flags = self.to_js_string(flags_v)?;
                let global = flags.contains('g');
                // Clone the regex so iteration doesn't disturb the receiver.
                let matcher = self.build_regexp(this, flags_v)?;
                let matcher_idx = matcher.heap_index();
                let li = self.get_prop(this, "lastIndex")?;
                let li = self.to_number(li)?;
                let li = if li.is_finite() && li > 0.0 { li as usize } else { 0 };
                self.set_regexp_last_index(matcher_idx, li);
                let mut items: Vec<Value> = Vec::new();
                let mut guard = 0u32;
                loop {
                    guard += 1;
                    if guard > 1_000_000 {
                        break;
                    }
                    let r = self.regexp_exec(matcher_idx, s_val)?;
                    if r == Value::NULL {
                        break;
                    }
                    items.push(r);
                    if !global {
                        break;
                    }
                    // Empty match: advance one char so the loop terminates.
                    let empty = matches!(
                        self.heap.get(r.heap_index()),
                        HeapObj::Array(a) if a.first().is_some_and(|v| {
                            matches!(self.heap.get(v.heap_index()), HeapObj::Str(s) if s.char_len == 0)
                        })
                    );
                    if empty {
                        let cur = match self.heap.get(matcher_idx) {
                            HeapObj::RegExp { last_index, .. } => *last_index,
                            _ => 0,
                        };
                        self.set_regexp_last_index(matcher_idx, cur + 1);
                    }
                }
                let proto = self.regexp_string_iter_proto;
                Value::heap(self.heap.alloc(HeapObj::Iterator { items, index: 0, proto }))
            }
            REGEXP_GET_GLOBAL
            | REGEXP_GET_IGNORECASE
            | REGEXP_GET_MULTILINE
            | REGEXP_GET_DOTALL
            | REGEXP_GET_UNICODE
            | REGEXP_GET_UNICODESETS
            | REGEXP_GET_STICKY
            | REGEXP_GET_HASINDICES => {
                let ch = match id {
                    REGEXP_GET_GLOBAL => 'g',
                    REGEXP_GET_IGNORECASE => 'i',
                    REGEXP_GET_MULTILINE => 'm',
                    REGEXP_GET_DOTALL => 's',
                    REGEXP_GET_UNICODE => 'u',
                    REGEXP_GET_UNICODESETS => 'v',
                    REGEXP_GET_STICKY => 'y',
                    _ => 'd', // REGEXP_GET_HASINDICES
                };
                if !this.is_heap() {
                    return Err(Thrown(
                        "TypeError: RegExp flag getter called on a non-object".into(),
                    ));
                }
                let idx = this.heap_index();
                if idx == self.regexp_proto {
                    Value::UNDEFINED
                } else if let HeapObj::RegExp { flags, .. } = self.heap.get(idx) {
                    Value::bool(flags.contains(ch))
                } else {
                    return Err(Thrown(
                        "TypeError: RegExp flag getter called on a non-RegExp object".into(),
                    ));
                }
            }
            FN_CALL => {
                let rest: &[Value] = if args.len() > 1 { &args[1..] } else { &[] };
                self.call_value(this, a0, rest)?
            }
            FN_APPLY => {
                let callargs = if a1.is_heap() { self.iterate_to_vec(a1)? } else { Vec::new() };
                self.call_value(this, a0, &callargs)?
            }
            FN_BIND => {
                let bound: Vec<Value> = if args.len() > 1 { args[1..].to_vec() } else { Vec::new() };
                Value::heap(self.heap.alloc(HeapObj::Bound { target: this, this: a0, args: bound }))
            }
            FN_TO_STRING => {
                if !this.is_heap() {
                    return Err(Thrown(
                        "TypeError: Function.prototype.toString requires that 'this' be a Function"
                            .into(),
                    ));
                }
                // User functions carry their exact source slice; everything else
                // (natives, bound, classes) renders in the `[native code]` form.
                let stored: Option<String> = match self.heap.get(this.heap_index()) {
                    HeapObj::Func(id) => {
                        let s = &self.func(*id as usize).source;
                        (!s.is_empty()).then(|| s.clone())
                    }
                    HeapObj::Closure { func, .. } => {
                        let s = &self.func(*func as usize).source;
                        (!s.is_empty()).then(|| s.clone())
                    }
                    // A class value renders as its whole `class … { … }` source.
                    HeapObj::Class(c) => (!c.source.is_empty()).then(|| c.source.clone()),
                    HeapObj::Native(_) | HeapObj::Bound { .. } => None,
                    _ => {
                        return Err(Thrown(
                            "TypeError: Function.prototype.toString requires that 'this' be a Function"
                                .into(),
                        ))
                    }
                };
                let out = match stored {
                    Some(s) => s,
                    None => {
                        let name = self.callable_name(this);
                        format!("function {name}() {{ [native code] }}")
                    }
                };
                self.alloc_str(out)
            }
            ARR_IS_ARRAY => {
                Value::bool(a0.is_heap() && matches!(self.heap.get(a0.heap_index()), HeapObj::Array(_)))
            }
            ARR_FROM => self.array_from(this, a0, a1, args.get(2).copied().unwrap_or(Value::UNDEFINED))?,
            ARR_OF => Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec()))),
            // `%TypedArray%.from(src, mapFn?)` / `.of(...items)` — `this` is the
            // concrete kind constructor (Int8Array, …); collect the values into a
            // plain Array, then materialize a typed array of that kind.
            TA_FROM | TA_OF => {
                let kind = self
                    .ta_ctors
                    .iter()
                    .position(|&c| this.is_heap() && c == this.heap_index());
                let kind = match kind {
                    Some(k) => k as u8,
                    None => {
                        return Err(Thrown(
                            "TypeError: this is not a TypedArray constructor".into(),
                        ))
                    }
                };
                let arr = if id == TA_FROM {
                    self.array_from(Value::UNDEFINED, a0, a1, args.get(2).copied().unwrap_or(Value::UNDEFINED))?
                } else {
                    Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec())))
                };
                self.build_typed_array(kind, &[arr])?
            }
            // %TypedArray%.prototype accessor getters. The data accessors throw on a
            // non-TypedArray receiver; @@toStringTag returns undefined instead.
            TA_GET_BUFFER | TA_GET_BYTELENGTH | TA_GET_BYTEOFFSET | TA_GET_LENGTH => {
                let (buffer, kind, byte_offset, length) =
                    match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                        Some(HeapObj::TypedArray { buffer, kind, byte_offset, length }) => {
                            (*buffer, *kind, *byte_offset, *length)
                        }
                        _ => {
                            return Err(Thrown(
                                "TypeError: TypedArray accessor called on a non-TypedArray".into(),
                            ))
                        }
                    };
                let size = native::TA_KINDS[kind as usize].1;
                let detached =
                    matches!(self.heap.get(buffer), HeapObj::ArrayBuffer { detached: true, .. });
                match id {
                    TA_GET_BUFFER => Value::heap(buffer),
                    TA_GET_BYTELENGTH => Value::num(if detached { 0.0 } else { (length * size) as f64 }),
                    TA_GET_BYTEOFFSET => Value::num(if detached { 0.0 } else { byte_offset as f64 }),
                    _ => Value::num(if detached { 0.0 } else { length as f64 }), // TA_GET_LENGTH
                }
            }
            // `get [Symbol.species]` — returns the receiver constructor unchanged.
            SPECIES_GET => this,
            TA_GET_TOSTRINGTAG => {
                match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::TypedArray { kind, .. }) => {
                        let name = native::TA_KINDS[*kind as usize].0.to_string();
                        self.alloc_str(name)
                    }
                    _ => Value::UNDEFINED,
                }
            }
            // `Array.prototype.{join,push}` as values: `this` is the receiver array.
            // join is generic over array-likes (array_method materializes a
            // non-array receiver); push mutates, so it still requires a real array.
            ARR_JOIN => {
                if this.is_heap() {
                    self.array_method(this.heap_index(), "join", args)?.unwrap_or(Value::UNDEFINED)
                } else {
                    Value::UNDEFINED
                }
            }
            ARR_PUSH => {
                if this.is_heap() && matches!(self.heap.get(this.heap_index()), HeapObj::Array(_)) {
                    self.array_method(this.heap_index(), "push", args)?.unwrap_or(Value::UNDEFINED)
                } else {
                    Value::UNDEFINED
                }
            }
            // More Object statics as values.
            OBJ_IS => {
                let a = args.first().copied().unwrap_or(Value::UNDEFINED);
                let b = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                Value::bool(self.same_value(a, b))
            }
            OBJ_HAS_OWN => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                let k = self.to_property_key(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                Value::bool(self.has_own_property(o, &k))
            }
            OBJ_SET_PROTO_OF => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                let proto = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if o.is_heap() {
                    self.proto_of.insert(o.heap_index(), proto);
                }
                o
            }
            OBJ_GET_OWN_SYMBOLS => {
                // Own symbol-keyed properties: the `@@`-prefixed own keys, mapped
                // back to their Symbol values via the prop_key registry.
                let mut syms: Vec<Value> = Vec::new();
                if a0.is_heap() {
                    if let HeapObj::Object(m) = self.heap.get(a0.heap_index()) {
                        let keys: Vec<String> =
                            m.keys.iter().filter(|k| k.starts_with("@@")).cloned().collect();
                        for k in keys {
                            if let Some(&sym) = self.symbol_keys.get(&k) {
                                syms.push(sym);
                            }
                        }
                    }
                }
                Value::heap(self.heap.alloc(HeapObj::Array(syms)))
            }
            OBJ_FROM_ENTRIES => {
                let src = args.first().copied().unwrap_or(Value::UNDEFINED);
                let entries = if src.is_heap() { self.iterate_to_vec(src)? } else { Vec::new() };
                let mut map = ObjMap::new();
                for e in entries {
                    let k = self.get_index(e, Value::int(0))?;
                    let v = self.get_index(e, Value::int(1))?;
                    let ks = self.display(k);
                    map.set(&ks, v);
                }
                Value::heap(self.heap.alloc(HeapObj::Object(map)))
            }
            OBJ_GET_OWN_DESCS => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                let names = self.object_own_property_names(o);
                let keys: Vec<Value> = match self.heap.get(names.heap_index()) {
                    HeapObj::Array(items) => items.clone(),
                    _ => Vec::new(),
                };
                let mut map = ObjMap::new();
                for kv in keys {
                    let ks = self.display(kv);
                    let desc = self.object_get_own_property_descriptor(o, &ks);
                    map.set(&ks, desc);
                }
                Value::heap(self.heap.alloc(HeapObj::Object(map)))
            }
            // Integrity traits. Non-object arguments pass through unchanged
            // (freeze/seal/preventExtensions) or report as already-locked
            // (isFrozen/isSealed -> true, isExtensible -> false), per ES2015+.
            // Extensibility for an exotic (non-Object) heap value — array, function,
            // Temporal instance, Map/Set/Date/… — is tracked in the `arr_props` side
            // table (its ObjMap carries the `extensible` flag, default true). A fresh
            // exotic is therefore extensible / not-frozen / not-sealed (per spec),
            // and preventExtensions/seal/freeze record it consistently. Plain
            // Objects keep their own `extensible` flag; primitives are immutable.
            OBJ_FREEZE | OBJ_SEAL | OBJ_PREVENT_EXT => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                if o.is_heap() {
                    let idx = o.heap_index();
                    match self.heap.get(idx) {
                        // Heap-but-primitive (string/symbol/bigint): a no-op.
                        HeapObj::Str(_)
                        | HeapObj::Cons { .. }
                        | HeapObj::Symbol { .. }
                        | HeapObj::BigInt(_) => {}
                        HeapObj::Object(_) => {
                            if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                                match id {
                                    OBJ_FREEZE => m.freeze(),
                                    OBJ_SEAL => m.seal(),
                                    _ => m.extensible = false,
                                }
                            }
                        }
                        _ => {
                            let m = self.arr_props.entry(idx).or_insert_with(ObjMap::new);
                            match id {
                                OBJ_FREEZE => m.freeze(),
                                OBJ_SEAL => m.seal(),
                                _ => m.extensible = false,
                            }
                        }
                    }
                }
                o
            }
            OBJ_IS_FROZEN | OBJ_IS_SEALED | OBJ_IS_EXT => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                // A non-object (primitive, incl. heap string/symbol/bigint) is
                // non-extensible and vacuously frozen/sealed. An exotic object's
                // flags live in `arr_props` (default: extensible, not frozen/sealed).
                let (frozen, sealed, ext) = if o.is_heap() {
                    match self.heap.get(o.heap_index()) {
                        HeapObj::Object(m) => (m.is_frozen(), m.is_sealed(), m.extensible),
                        HeapObj::Str(_)
                        | HeapObj::Cons { .. }
                        | HeapObj::Symbol { .. }
                        | HeapObj::BigInt(_) => (true, true, false),
                        _ => self.arr_props.get(&o.heap_index()).map_or((false, false, true), |m| {
                            (m.is_frozen(), m.is_sealed(), m.extensible)
                        }),
                    }
                } else {
                    (true, true, false)
                };
                Value::bool(match id {
                    OBJ_IS_FROZEN => frozen,
                    OBJ_IS_SEALED => sealed,
                    _ => ext,
                })
            }
            // Object.groupBy(items, cb) -> null-proto object of arrays keyed by cb's
            // (string) return; Map.groupBy -> a Map keyed by cb's value (SameValueZero).
            OBJ_GROUP_BY | MAP_GROUP_BY => {
                // The accumulating group arrays / keys live in Rust locals (not
                // reachable from the GC roots) while the callback re-enters the
                // interpreter — suspend GC for the scope.
                let _gc = self.gc_lock_guard();
                let src = args.first().copied().unwrap_or(Value::UNDEFINED);
                let cb = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if !(cb.is_heap() && self.heap.as_callable(cb.heap_index()).is_some()) {
                    return Err(Thrown("TypeError: groupBy callback is not callable".into()));
                }
                if !src.is_heap() {
                    return Err(Thrown("TypeError: groupBy items is not iterable".into()));
                }
                let items = self.iterate_to_vec(src)?;
                if id == OBJ_GROUP_BY {
                    let mut map = ObjMap::new();
                    for (i, item) in items.into_iter().enumerate() {
                        let key = self.call_value(cb, Value::UNDEFINED, &[item, Value::int(i as i32)])?;
                        let ks = self.display(key);
                        match map.get(&ks) {
                            Some(arr) => {
                                if let HeapObj::Array(a) = self.heap.get_mut(arr.heap_index()) {
                                    a.push(item);
                                }
                            }
                            None => {
                                let arr = Value::heap(self.heap.alloc(HeapObj::Array(vec![item])));
                                map.set(&ks, arr);
                            }
                        }
                    }
                    let result = self.heap.alloc(HeapObj::Object(map));
                    self.proto_of.insert(result, Value::NULL); // null prototype per spec
                    Value::heap(result)
                } else {
                    let mut keys: Vec<Value> = Vec::new();
                    let mut vals: Vec<Value> = Vec::new();
                    for (i, item) in items.into_iter().enumerate() {
                        let mut key = self.call_value(cb, Value::UNDEFINED, &[item, Value::int(i as i32)])?;
                        if key.is_number() && key.as_f64() == 0.0 {
                            key = Value::int(0); // Map normalizes -0 to +0
                        }
                        match keys.iter().position(|k| self.same_value_zero(*k, key)) {
                            Some(pos) => {
                                if let HeapObj::Array(a) = self.heap.get_mut(vals[pos].heap_index()) {
                                    a.push(item);
                                }
                            }
                            None => {
                                keys.push(key);
                                vals.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![item]))));
                            }
                        }
                    }
                    Value::heap(self.heap.alloc(HeapObj::Map { keys, vals }))
                }
            }
            // Promise.withResolvers() -> { promise, resolve, reject }.
            PROMISE_WITH_RESOLVERS => {
                if !self.is_constructor(this) {
                    return Err(Thrown(
                        "TypeError: Promise.withResolvers called on a non-constructor".into(),
                    ));
                }
                let p = self.alloc_promise();
                let resolve = Value::heap(
                    self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: false }),
                );
                let reject = Value::heap(
                    self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: true }),
                );
                let mut map = ObjMap::new();
                map.set("promise", Value::heap(p));
                map.set("resolve", resolve);
                map.set("reject", reject);
                Value::heap(self.heap.alloc(HeapObj::Object(map)))
            }
            PROMISE_TRY => {
                if !self.is_constructor(this) {
                    return Err(Thrown("TypeError: Promise.try called on a non-constructor".into()));
                }
                let p = self.alloc_promise();
                let rest: Vec<Value> = if args.len() > 1 { args[1..].to_vec() } else { Vec::new() };
                match self.call_value(a0, Value::UNDEFINED, &rest) {
                    Ok(v) => self.resolve(p, v),
                    Err(Thrown(msg)) => {
                        let e = self.alloc_error_from_message(&msg);
                        self.reject(p, e);
                    }
                }
                Value::heap(p)
            }
            // Reflect namespace. apply/construct accept any callable target; the
            // property-reflecting methods require Type(target) === Object (else TypeError).
            REFLECT_APPLY => {
                let target = a0;
                let this_arg = a1;
                let args_list = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let arg_vec =
                    if args_list.is_heap() { self.array_snapshot(args_list.heap_index()) } else { Vec::new() };
                self.call_value(target, this_arg, &arg_vec)?
            }
            REFLECT_CONSTRUCT => {
                let target = a0;
                if !self.is_constructor(target) {
                    return Err(Thrown("TypeError: Reflect.construct target is not a constructor".into()));
                }
                // An explicit newTarget (3rd arg) must also be a constructor. We
                // don't model newTarget-driven prototype selection, but the throw is
                // what test262's isConstructor relies on.
                if let Some(nt) = args.get(2) {
                    if !self.is_constructor(*nt) {
                        return Err(Thrown(
                            "TypeError: Reflect.construct newTarget is not a constructor".into(),
                        ));
                    }
                }
                let arg_vec = if a1.is_heap() { self.array_snapshot(a1.heap_index()) } else { Vec::new() };
                self.construct(target, &arg_vec)?
            }
            REFLECT_GET => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.get called on non-object".into()));
                }
                // Reflect.get(target, key, receiver?): an explicit receiver is the
                // `this` for an accessor getter (else the target). Use the index
                // path when there's no distinct receiver (it also reads array
                // elements for numeric keys).
                let receiver = args.get(2).copied().unwrap_or(a0);
                if receiver == a0 {
                    self.get_index(a0, a1)?
                } else {
                    let key = self.to_property_key(a1)?;
                    self.get_member(a0, &key, receiver)?
                }
            }
            REFLECT_SET => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.set called on non-object".into()));
                }
                let value = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                // ToPropertyKey once (an object key may have a side-effecting
                // toString); reuse the coerced key Value for set_index so it isn't
                // coerced a second time.
                let kv = self.coerce_index_key(a1)?;
                let key = self.key_of(kv);
                // success = not blocked by a non-writable own data property, an
                // accessor without a setter, or a new key on a non-extensible object.
                let ok = match self.heap.get(a0.heap_index()) {
                    HeapObj::Object(m) => match m.pos(&key) {
                        Some(i) => {
                            if m.attrs[i].accessor {
                                m.attrs[i].setter != Value::UNDEFINED
                            } else {
                                m.attrs[i].writable
                            }
                        }
                        None => m.extensible,
                    },
                    _ => true,
                };
                if ok {
                    self.set_index(a0, kv, value)?;
                }
                Value::bool(ok)
            }
            REFLECT_HAS => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.has called on non-object".into()));
                }
                let kv = self.coerce_index_key(a1)?;
                Value::bool(self.has_property(a0, kv))
            }
            REFLECT_DELETE => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.deleteProperty called on non-object".into()));
                }
                let key = self.to_property_key(a1)?;
                self.delete_prop(a0, &key)
            }
            REFLECT_OWN_KEYS => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.ownKeys called on non-object".into()));
                }
                self.object_own_property_names(a0)
            }
            REFLECT_GET_PROTO => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.getPrototypeOf called on non-object".into()));
                }
                self.object_get_prototype_of(a0)
            }
            REFLECT_SET_PROTO => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.setPrototypeOf called on non-object".into()));
                }
                if a1 != Value::NULL && !self.is_object_value(a1) {
                    return Err(Thrown(
                        "TypeError: Reflect.setPrototypeOf prototype must be an object or null".into(),
                    ));
                }
                self.proto_of.insert(a0.heap_index(), a1);
                Value::bool(true)
            }
            REFLECT_DEFINE => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.defineProperty called on non-object".into()));
                }
                let desc = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                if !self.is_object_value(desc) {
                    return Err(Thrown("TypeError: Property description must be an object".into()));
                }
                let key = self.to_property_key(a1)?;
                // Reflect.defineProperty returns false (not throw) when the definition
                // is rejected (non-configurable redefine, non-extensible new key).
                match self.object_define_property(a0, &key, desc) {
                    Ok(()) => Value::bool(true),
                    Err(_) => Value::bool(false),
                }
            }
            REFLECT_GET_OWN_DESC => {
                if !self.is_object_value(a0) {
                    return Err(Thrown(
                        "TypeError: Reflect.getOwnPropertyDescriptor called on non-object".into(),
                    ));
                }
                let key = self.to_property_key(a1)?;
                self.object_get_own_property_descriptor(a0, &key)
            }
            REFLECT_IS_EXT => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.isExtensible called on non-object".into()));
                }
                let ext = match self.heap.get(a0.heap_index()) {
                    HeapObj::Object(m) => m.extensible,
                    _ => self.arr_props.get(&a0.heap_index()).map_or(true, |m| m.extensible),
                };
                Value::bool(ext)
            }
            REFLECT_PREVENT_EXT => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.preventExtensions called on non-object".into()));
                }
                let idx = a0.heap_index();
                if matches!(self.heap.get(idx), HeapObj::Object(_)) {
                    if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                        m.extensible = false;
                    }
                } else {
                    self.arr_props.entry(idx).or_insert_with(ObjMap::new).extensible = false;
                }
                Value::bool(true)
            }
            // JSON namespace methods as values (`JSON.parse`/`JSON.stringify`).
            // (The direct `JSON.parse(x)` call form is compile-lowered to a JSON op;
            // these back the value form + reflection.)
            JSON_PARSE => {
                let s = self.to_js_string(a0)?;
                let reviver = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if self.is_callable(reviver) {
                    let _gc = self.gc_lock_guard();
                    let (parsed, srctree) = self.json_parse_with_src(&s)?;
                    let mut m = crate::heap::ObjMap::new();
                    m.set("", parsed);
                    let wrapper = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                    self.internalize_json(wrapper, "", reviver, Some(&srctree))?
                } else {
                    self.json_parse(&s)?
                }
            }
            JSON_STRINGIFY => {
                let space = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let indent = self.json_indent(space);
                let replacer = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (replacer_fn, allowlist) = self.json_resolve_replacer(replacer)?;
                // Hold un-rooted Values across toJSON/replacer re-entry; suspend GC.
                let _gc = self.gc_lock_guard();
                let mut m = crate::heap::ObjMap::new();
                m.set("", a0);
                let wrapper = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                let mut visited = Vec::new();
                match self.json_value(
                    wrapper,
                    "",
                    a0,
                    &indent,
                    0,
                    &mut visited,
                    replacer_fn,
                    allowlist.as_deref(),
                )? {
                    Some(s) => self.alloc_str(s),
                    None => Value::UNDEFINED,
                }
            }
            JSON_RAW_JSON => {
                // JSON.rawJSON(text): ToString (throws TypeError for a Symbol),
                // then validate the text is a single non-empty JSON value with no
                // leading/trailing JSON whitespace. The result is a frozen,
                // null-prototype object whose sole own property "rawJSON" holds the
                // text, tagged [[IsRawJSON]] so stringify emits it verbatim.
                let s = self.to_js_string(a0)?;
                let bytes = s.as_bytes();
                let ws = |c: u8| matches!(c, b'\t' | b'\n' | b'\r' | b' ');
                if s.is_empty() || ws(bytes[0]) || ws(bytes[bytes.len() - 1]) {
                    return Err(Thrown(
                        "SyntaxError: JSON.rawJSON text must be non-empty without leading/trailing whitespace".into(),
                    ));
                }
                // Validate it parses as one complete JSON value (checks trailing).
                self.json_parse(&s)?;
                let _gc = self.gc_lock_guard();
                let sval = self.alloc_str(s);
                let mut m = crate::heap::ObjMap::new();
                m.is_raw_json = true;
                m.extensible = false;
                m.keys.push("rawJSON".to_string());
                m.vals.push(sval);
                m.attrs.push(crate::heap::PropAttr {
                    writable: false,
                    enumerable: true,
                    configurable: false,
                    accessor: false,
                    setter: Value::UNDEFINED,
                });
                let idx = self.heap.alloc(HeapObj::Object(m));
                self.proto_of.insert(idx, Value::NULL); // OrdinaryObjectCreate(null)
                Value::heap(idx)
            }
            JSON_IS_RAW_JSON => {
                let v = args.first().copied().unwrap_or(Value::UNDEFINED);
                let is = v.is_heap()
                    && matches!(self.heap.get(v.heap_index()), HeapObj::Object(m) if m.is_raw_json);
                Value::bool(is)
            }
            // `Math.random` as a value (the call form uses the Random op). xorshift64*.
            MATH_RANDOM => {
                let mut x = self.rng_state;
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                self.rng_state = x;
                let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
                Value::num((r >> 11) as f64 / (1u64 << 53) as f64)
            }
            // WeakMap/WeakSet methods (brand-checked + object-key validated inside).
            WM_GET => self.weakmap_method(this, "get", args)?,
            WM_SET => self.weakmap_method(this, "set", args)?,
            WM_HAS => self.weakmap_method(this, "has", args)?,
            WM_DELETE => self.weakmap_method(this, "delete", args)?,
            WS_ADD => self.weakset_method(this, "add", args)?,
            WS_HAS => self.weakset_method(this, "has", args)?,
            WS_DELETE => self.weakset_method(this, "delete", args)?,
            WR_DEREF => {
                match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::WeakRef(t)) => *t, // no GC → target always live
                    _ => {
                        return Err(Thrown(
                            "TypeError: WeakRef.prototype.deref called on incompatible receiver".into(),
                        ))
                    }
                }
            }
            FR_REGISTER => self.finreg_method(this, "register", args)?,
            FR_UNREGISTER => self.finreg_method(this, "unregister", args)?,
            ITER_NEXT => {
                let (val, done) = match this.is_heap().then(|| self.heap.get_mut(this.heap_index())) {
                    Some(HeapObj::Iterator { items, index, .. }) => {
                        if *index < items.len() {
                            let v = items[*index];
                            *index += 1;
                            (v, false)
                        } else {
                            (Value::UNDEFINED, true)
                        }
                    }
                    _ => {
                        return Err(Thrown(
                            "TypeError: Iterator.prototype.next called on incompatible receiver".into(),
                        ))
                    }
                };
                let mut m = ObjMap::new();
                m.set("value", val);
                m.set("done", Value::bool(done));
                Value::heap(self.heap.alloc(HeapObj::Object(m)))
            }
            ITER_SELF => this, // `iter[Symbol.iterator]()` returns the iterator itself
            // ES2025 Iterator Helpers (%Iterator.prototype%).
            ITER_MAP | ITER_FILTER | ITER_TAKE | ITER_DROP | ITER_FLATMAP | ITER_REDUCE
            | ITER_TOARRAY | ITER_FOREACH | ITER_SOME | ITER_EVERY | ITER_FIND => {
                self.iter_helper_method(id, this, args)?
            }
            ITER_HELPER_NEXT => {
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::IterHelper { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: Iterator Helper next called on an incompatible receiver".into(),
                    ));
                }
                self.iter_helper_next(this.heap_index())?
            }
            ITER_HELPER_RETURN => {
                if this.is_heap() {
                    if let HeapObj::IterHelper { done, .. } = self.heap.get_mut(this.heap_index()) {
                        *done = true;
                    }
                }
                self.iter_result(Value::UNDEFINED, true)
            }
            ITER_FROM => self.iterator_from(a0)?,
            // test262 `$262.detachArrayBuffer(ab)` / `$262.gc()`.
            DOLLAR262_DETACH => {
                if let Some(buf) = self.as_array_buffer(a0) {
                    if let HeapObj::ArrayBuffer { data, detached } = self.heap.get_mut(buf) {
                        *detached = true;
                        data.clear();
                    }
                }
                Value::NULL
            }
            DOLLAR262_GC => Value::UNDEFINED,
            // Object.prototype Annex-B accessor helpers.
            OBJPROTO_DEFINE_GETTER | OBJPROTO_DEFINE_SETTER => {
                if !self.is_callable(a1) {
                    return Err(Thrown(
                        "TypeError: Object.prototype.__define[GS]etter__: expecting a function".into(),
                    ));
                }
                let key = self.to_property_key(a0)?;
                let mut d = ObjMap::new();
                d.set(if id == OBJPROTO_DEFINE_GETTER { "get" } else { "set" }, a1);
                d.set("enumerable", Value::bool(true));
                d.set("configurable", Value::bool(true));
                let desc = Value::heap(self.heap.alloc(HeapObj::Object(d)));
                self.object_define_property(this, &key, desc)?;
                Value::UNDEFINED
            }
            OBJPROTO_LOOKUP_GETTER | OBJPROTO_LOOKUP_SETTER => {
                let key = self.to_property_key(a0)?;
                self.lookup_accessor(this, &key, id == OBJPROTO_LOOKUP_SETTER)
            }
            OBJPROTO_PROTO_GET => self.object_get_prototype_of(this),
            OBJPROTO_PROTO_SET => {
                // Only an object or null changes the prototype; primitives are ignored.
                if this.is_heap() && (self.is_object_value(a0) || a0 == Value::NULL) {
                    self.proto_of.insert(this.heap_index(), a0);
                }
                Value::UNDEFINED
            }
            ITER_TAG_GET => self.alloc_str("Iterator".to_string()),
            ITER_TAG_SET => {
                if this.is_heap() && this.heap_index() == self.iterator_proto_root {
                    return Err(Thrown(
                        "TypeError: Cannot assign to read only property 'Symbol(Symbol.toStringTag)'"
                            .into(),
                    ));
                }
                if self.is_object_value(this) {
                    self.set_prop(this, "@@toStringTag", a0)?;
                }
                Value::UNDEFINED
            }
            ITER_CTOR_GET => {
                if self.iterator_ctor != 0 {
                    Value::heap(self.iterator_ctor)
                } else {
                    Value::UNDEFINED
                }
            }
            ITER_CTOR_SET => {
                if this.is_heap() && this.heap_index() == self.iterator_proto_root {
                    return Err(Thrown(
                        "TypeError: Cannot assign to read only property 'constructor'".into(),
                    ));
                }
                if self.is_object_value(this) {
                    self.set_prop(this, "constructor", a0)?;
                }
                Value::UNDEFINED
            }
            // Number static methods as values (no coercion, per spec).
            NUM_IS_INTEGER => Value::bool(num_is_integer(a0)),
            NUM_IS_NAN => Value::bool(a0.is_double() && a0.as_f64().is_nan()),
            NUM_IS_FINITE => Value::bool(num_is_finite(a0)),
            NUM_IS_SAFE_INTEGER => Value::bool(num_is_safe_integer(a0)),
            // Global functions as values.
            GLOBAL_PARSE_INT => {
                let s = self.display(a0);
                let radix = if args.len() >= 2 { self.to_number(a1)? as i32 } else { 0 };
                Value::num(parse_int(&s, radix))
            }
            GLOBAL_PARSE_FLOAT => Value::num(parse_float(&self.display(a0))),
            GLOBAL_IS_NAN => Value::bool(self.to_number(a0).unwrap_or(f64::NAN).is_nan()),
            GLOBAL_IS_FINITE => Value::bool(self.to_number(a0).unwrap_or(f64::NAN).is_finite()),
            GLOBAL_EVAL => {
                // eval(x): if x is not a String, return it unchanged (spec 19.2.1).
                let is_str = a0.is_heap()
                    && matches!(self.heap.get(a0.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. });
                if !is_str {
                    a0
                } else {
                    let code = self.display(a0);
                    return self.do_eval(&code);
                }
            }
            // String static methods.
            STR_FROM_CHAR_CODE => {
                let mut s = String::new();
                for &v in args {
                    let u = to_uint32(self.to_number(v).unwrap_or(0.0)) as u16;
                    s.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}'));
                }
                self.alloc_str(s)
            }
            STR_FROM_CODE_POINT => {
                let mut s = String::new();
                for &v in args {
                    let n = self.to_number(v)?;
                    if !n.is_finite() || n < 0.0 || n > 0x10FFFF as f64 || n.fract() != 0.0 {
                        return Err(Thrown(format!("RangeError: Invalid code point {n}")));
                    }
                    // A lone-surrogate code point can't be a Rust char → replacement.
                    s.push(char::from_u32(n as u32).unwrap_or('\u{FFFD}'));
                }
                self.alloc_str(s)
            }
            // Date static methods as values.
            DATE_NOW => Value::num(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as f64)
                    .unwrap_or(0.0),
            ),
            DATE_PARSE => Value::num(parse_date(&self.display(a0))),
            DATE_UTC => Value::num(self.date_utc_ms(args)?),
            STR_RAW => {
                // String.raw(template, ...subs): interleave template.raw[i] with subs[i].
                let raw = self.get_prop(a0, "raw")?;
                if !raw.is_heap() {
                    return Ok(self.alloc_str(String::new()));
                }
                let len_v = self.get_prop(raw, "length")?;
                let n = self.to_number(len_v)?;
                let raw_len = if n.is_finite() && n > 0.0 { n as usize } else { 0 };
                let subs = args.get(1..).unwrap_or(&[]);
                let mut out = String::new();
                for i in 0..raw_len {
                    let seg = self.get_index(raw, Value::int(i as i32))?;
                    out.push_str(&self.display(seg));
                    if i + 1 == raw_len {
                        break;
                    }
                    if let Some(sub) = subs.get(i) {
                        out.push_str(&self.display(*sub));
                    }
                }
                self.alloc_str(out)
            }
            // Object.prototype.toLocaleString() → this.toString().
            PROTO_TO_LOCALE_STRING => {
                let ts = self.get_prop(this, "toString")?;
                if self.is_callable(ts) {
                    self.call_value(ts, this, &[])?
                } else {
                    return Err(Thrown("TypeError: toString is not callable".into()));
                }
            }
            // `Math.<op>` as a value (`Math.abs`, `Math.max`, …). The direct call
            // form is compile-lowered to MathOp; these back the value form.
            _ if native::math_method(id).is_some() => {
                let (_, op, _) = native::math_method(id).unwrap();
                Value::num(self.eval_math_args(op, args)?)
            }
            // Promise static methods invoked as values (`Promise.resolve`, …).
            PROMISE_RESOLVE => {
                let p = self.to_promise(args.first().copied().unwrap_or(Value::UNDEFINED));
                Value::heap(p)
            }
            PROMISE_REJECT => {
                let p = self.alloc_promise();
                self.reject(p, args.first().copied().unwrap_or(Value::UNDEFINED));
                Value::heap(p)
            }
            PROMISE_ALL => self.promise_combine(crate::heap::CombKind::All, args.first().copied().unwrap_or(Value::UNDEFINED))?,
            PROMISE_ALLSETTLED => self.promise_combine(crate::heap::CombKind::AllSettled, args.first().copied().unwrap_or(Value::UNDEFINED))?,
            PROMISE_RACE => self.promise_combine(crate::heap::CombKind::Race, args.first().copied().unwrap_or(Value::UNDEFINED))?,
            PROMISE_ANY => self.promise_combine(crate::heap::CombKind::Any, args.first().copied().unwrap_or(Value::UNDEFINED))?,
            // `%TypedArray%.prototype.<m>` invoked as a value (`.map.call(ta, …)`).
            _ if (TA_METHOD_BASE..TA_METHOD_BASE + TA_PROTO_METHODS.len() as u16).contains(&id) => {
                let m = TA_PROTO_METHODS[(id - TA_METHOD_BASE) as usize];
                if !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::TypedArray { .. })) {
                    return Err(Thrown(format!(
                        "TypeError: TypedArray.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.typed_array_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            _ if (DV_METHOD_BASE..DV_METHOD_BASE + DV_PROTO_METHODS.len() as u16).contains(&id) => {
                let m = DV_PROTO_METHODS[(id - DV_METHOD_BASE) as usize];
                if !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::DataView { .. })) {
                    return Err(Thrown(format!(
                        "TypeError: DataView.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.dataview_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            ARRAYBUFFER_SLICE => {
                if !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::ArrayBuffer { .. })) {
                    return Err(Thrown(
                        "TypeError: ArrayBuffer.prototype.slice called on incompatible receiver".into(),
                    ));
                }
                self.arraybuffer_method(this.heap_index(), "slice", args)?.unwrap_or(Value::UNDEFINED)
            }
            ARRAYBUFFER_RESIZE => {
                if !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::ArrayBuffer { .. })) {
                    return Err(Thrown(
                        "TypeError: ArrayBuffer.prototype.resize called on incompatible receiver".into(),
                    ));
                }
                self.arraybuffer_method(this.heap_index(), "resize", args)?.unwrap_or(Value::UNDEFINED)
            }
            ARRAYBUFFER_ISVIEW => Value::bool(
                a0.is_heap()
                    && matches!(
                        self.heap.get(a0.heap_index()),
                        HeapObj::TypedArray { .. } | HeapObj::DataView { .. }
                    ),
            ),
            _ if (BUFFER_GETTER_BASE..BUFFER_GETTER_BASE + BUFFER_GETTERS.len() as u16)
                .contains(&id) =>
            {
                let (name, kind) = BUFFER_GETTERS[(id - BUFFER_GETTER_BASE) as usize];
                let ok = this.is_heap()
                    && matches!(
                        (kind, self.heap.get(this.heap_index())),
                        (0, HeapObj::ArrayBuffer { .. })
                            | (1, HeapObj::TypedArray { .. })
                            | (2, HeapObj::DataView { .. })
                    );
                if !ok {
                    return Err(Thrown(format!(
                        "TypeError: get {name} called on an incompatible receiver"
                    )));
                }
                // The instance arm of get_member computes the value directly (it
                // never consults this proto accessor, so there's no recursion).
                self.get_member(this, name, this)?
            }
            SAB_GROW => {
                let ok = this.is_heap() && self.shared_buffers.contains(&this.heap_index());
                if !ok {
                    return Err(Thrown(
                        "TypeError: SharedArrayBuffer.prototype.grow called on incompatible receiver".into(),
                    ));
                }
                self.arraybuffer_method(this.heap_index(), "grow", args)?.unwrap_or(Value::UNDEFINED)
            }
            _ if (ATOMICS_BASE..ATOMICS_BASE + ATOMICS_METHODS.len() as u16).contains(&id) => {
                let (name, _) = ATOMICS_METHODS[(id - ATOMICS_BASE) as usize];
                self.atomics_op(name, args)?
            }
            DISPOSABLE_USE | DISPOSABLE_ADOPT | DISPOSABLE_DEFER | DISPOSABLE_DISPOSE
            | DISPOSABLE_MOVE | DISPOSABLE_DISPOSED_GET => self.disposable_op(id, this, args)?,
            _ if (SAB_GETTER_BASE..SAB_GETTER_BASE + SAB_GETTERS.len() as u16).contains(&id) => {
                let name = SAB_GETTERS[(id - SAB_GETTER_BASE) as usize];
                if !(this.is_heap() && self.shared_buffers.contains(&this.heap_index())) {
                    return Err(Thrown(format!(
                        "TypeError: get SharedArrayBuffer.prototype.{name} called on incompatible receiver"
                    )));
                }
                // The shared-buffer arm of get_member computes the value directly.
                self.get_member(this, name, this)?
            }
            PROXY_REVOCABLE => {
                // Proxy.revocable(target, handler) → { proxy, revoke }.
                let p = self.make_proxy(a0, a1)?;
                let revoke_fn = self.heap.alloc(HeapObj::Native(PROXY_REVOKE));
                let revoke = Value::heap(self.heap.alloc(HeapObj::Bound {
                    target: Value::heap(revoke_fn),
                    this: p,
                    args: Vec::new(),
                }));
                let mut m = ObjMap::new();
                m.set("proxy", p);
                m.set("revoke", revoke);
                Value::heap(self.heap.alloc(HeapObj::Object(m)))
            }
            PROXY_REVOKE => {
                if this.is_heap() {
                    if let HeapObj::Proxy { revoked, .. } = self.heap.get_mut(this.heap_index()) {
                        *revoked = true;
                    }
                }
                Value::UNDEFINED
            }
            _ if (TEMPORAL_M_BASE..TEMPORAL_M_BASE + TEMPORAL_DURATION_METHODS.len() as u16)
                .contains(&id) =>
            {
                let m = TEMPORAL_DURATION_METHODS[(id - TEMPORAL_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 0, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.Duration.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            TEMPORAL_DURATION_FROM => {
                let f = self.to_duration(a0)?;
                self.make_duration(f)
            }
            TEMPORAL_DURATION_COMPARE => {
                let fa = self.to_duration(a0)?;
                let fb = self.to_duration(a1)?;
                let opts = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                Value::num(self.duration_compare(fa, fb, opts)?)
            }
            _ if (PD_M_BASE..PD_M_BASE + PLAINDATE_METHODS.len() as u16).contains(&id) => {
                let m = PLAINDATE_METHODS[(id - PD_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 1, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainDate.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINDATE_FROM => {
                let reject = self.read_overflow(a1)?;
                let (y, m, d) = self.to_plain_date_overflow(a0, reject)?;
                self.make_plain_date(y, m, d)?
            }
            PLAINDATE_COMPARE => {
                let a = self.to_plain_date(a0)?;
                let b = self.to_plain_date(a1)?;
                let ea = iso_to_epoch_days(a.0, a.1, a.2);
                let eb = iso_to_epoch_days(b.0, b.1, b.2);
                Value::num(if ea < eb { -1.0 } else if ea > eb { 1.0 } else { 0.0 })
            }
            _ if (PT_M_BASE..PT_M_BASE + PLAINTIME_METHODS.len() as u16).contains(&id) => {
                let m = PLAINTIME_METHODS[(id - PT_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 2, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainTime.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINTIME_FROM => {
                let reject = self.read_overflow(a1)?;
                let f = self.to_plain_time_overflow(a0, reject)?;
                self.make_plain_time(f)?
            }
            PLAINTIME_COMPARE => {
                let a = self.to_plain_time(a0)?;
                let b = self.to_plain_time(a1)?;
                let (ta, tb) = (time_to_ns(&a), time_to_ns(&b));
                Value::num(if ta < tb { -1.0 } else if ta > tb { 1.0 } else { 0.0 })
            }
            _ if (PDT_M_BASE..PDT_M_BASE + PLAINDATETIME_METHODS.len() as u16).contains(&id) => {
                let m = PLAINDATETIME_METHODS[(id - PDT_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 3, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainDateTime.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINDATETIME_FROM => {
                let reject = self.read_overflow(a1)?;
                let f = self.to_plain_date_time_overflow(a0, reject)?;
                self.make_plain_date_time(f)?
            }
            PLAINDATETIME_COMPARE => {
                let a = self.to_plain_date_time(a0)?;
                let b = self.to_plain_date_time(a1)?;
                let an = iso_to_epoch_days(a[0], a[1], a[2]) as i128 * 86_400_000_000_000
                    + time_to_ns(&[a[3], a[4], a[5], a[6], a[7], a[8]]);
                let bn = iso_to_epoch_days(b[0], b[1], b[2]) as i128 * 86_400_000_000_000
                    + time_to_ns(&[b[3], b[4], b[5], b[6], b[7], b[8]]);
                Value::num(if an < bn { -1.0 } else if an > bn { 1.0 } else { 0.0 })
            }
            _ if (INST_M_BASE..INST_M_BASE + INSTANT_METHODS.len() as u16).contains(&id) => {
                let m = INSTANT_METHODS[(id - INST_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 4, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.Instant.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            INST_FROM => {
                let ns = self.to_instant_ns(a0)?;
                self.make_instant(ns)?
            }
            INST_FROM_EPOCH_MS => {
                let ns = (self.to_number(a0)? as i128) * 1_000_000;
                self.make_instant(ns)?
            }
            INST_FROM_EPOCH_SEC => {
                let ns = (self.to_number(a0)? as i128) * 1_000_000_000;
                self.make_instant(ns)?
            }
            INST_FROM_EPOCH_NS => {
                let ns = self.to_bigint(a0)?;
                self.make_instant(ns)?
            }
            INST_FROM_EPOCH_US => {
                let ns = self.to_bigint(a0)? * 1_000;
                self.make_instant(ns)?
            }
            INST_COMPARE => {
                let a = self.to_instant_ns(a0)?;
                let b = self.to_instant_ns(a1)?;
                Value::num(if a < b { -1.0 } else if a > b { 1.0 } else { 0.0 })
            }
            _ if (PYM_M_BASE..PYM_M_BASE + PLAINYEARMONTH_METHODS.len() as u16).contains(&id) => {
                let m = PLAINYEARMONTH_METHODS[(id - PYM_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 5, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainYearMonth.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINYEARMONTH_FROM => {
                let reject = self.read_overflow(a1)?;
                let (y, m, rd) = self.to_plain_year_month_overflow(a0, reject)?;
                self.make_plain_year_month(y, m, rd)?
            }
            PLAINYEARMONTH_COMPARE => {
                let a = self.to_plain_year_month(a0)?;
                let b = self.to_plain_year_month(a1)?;
                let ka = a.0 * 12 + a.1;
                let kb = b.0 * 12 + b.1;
                Value::num(if ka < kb { -1.0 } else if ka > kb { 1.0 } else { 0.0 })
            }
            _ if (PMD_M_BASE..PMD_M_BASE + PLAINMONTHDAY_METHODS.len() as u16).contains(&id) => {
                let m = PLAINMONTHDAY_METHODS[(id - PMD_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 6, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainMonthDay.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            _ if (ZDT_M_BASE..ZDT_M_BASE + ZONEDDATETIME_METHODS.len() as u16).contains(&id) => {
                let m = ZONEDDATETIME_METHODS[(id - ZDT_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 7, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.ZonedDateTime.prototype.{m} called on incompatible receiver"
                    )));
                }
                match self.temporal_method(this.heap_index(), m, args)? {
                    Some(v) => v,
                    None => {
                        return Err(Thrown(format!(
                            "TypeError: Temporal.ZonedDateTime.prototype.{m} is not yet supported"
                        )))
                    }
                }
            }
            ZDT_FROM => self.zoned_date_time_from(a0, a1)?,
            ZDT_COMPARE => {
                let za = self.zoned_date_time_from(a0, Value::UNDEFINED)?;
                let zb = self.zoned_date_time_from(a1, Value::UNDEFINED)?;
                let na = self.zdt_epoch_ns(za.heap_index()).unwrap_or(0);
                let nb = self.zdt_epoch_ns(zb.heap_index()).unwrap_or(0);
                Value::num(match na.cmp(&nb) {
                    std::cmp::Ordering::Less => -1.0,
                    std::cmp::Ordering::Equal => 0.0,
                    std::cmp::Ordering::Greater => 1.0,
                })
            }
            PLAINMONTHDAY_FROM => {
                let reject = self.read_overflow(a1)?;
                let (ry, m, d) = self.to_plain_month_day_overflow(a0, reject)?;
                self.make_plain_month_day(m, d, ry)?
            }
            // Temporal.Now — no timezone DB, so everything reports UTC.
            NOW_INSTANT => {
                let ns = Self::now_epoch_ns();
                self.make_instant(ns)?
            }
            NOW_PLAINDATETIME_ISO => {
                let ns = Self::now_epoch_ns();
                let days = ns.div_euclid(DAY_NS);
                let t = ns_to_time(ns.rem_euclid(DAY_NS));
                let (y, mo, d) = epoch_days_to_iso(days as i64);
                self.make_plain_date_time([y, mo, d, t[0], t[1], t[2], t[3], t[4], t[5]])?
            }
            NOW_PLAINDATE_ISO => {
                let ns = Self::now_epoch_ns();
                let (y, mo, d) = epoch_days_to_iso(ns.div_euclid(DAY_NS) as i64);
                self.make_plain_date(y, mo, d)?
            }
            NOW_PLAINTIME_ISO => {
                let ns = Self::now_epoch_ns();
                self.make_plain_time(ns_to_time(ns.rem_euclid(DAY_NS)))?
            }
            NOW_TIMEZONE_ID => self.alloc_str("UTC".to_string()),
            // ── Intl ──
            INTL_GET_CANONICAL_LOCALES => {
                let list = self.canonicalize_locale_list(a0)?;
                let items: Vec<Value> = list.into_iter().map(|s| self.alloc_str(s)).collect();
                Value::heap(self.heap.alloc(HeapObj::Array(items)))
            }
            INTL_SUPPORTED_VALUES_OF => {
                let key = self.to_js_string(a0)?;
                let vals: &[&str] = match key.as_str() {
                    "calendar" => &["gregory", "iso8601"],
                    "collation" => &["default"],
                    "currency" => &["USD", "EUR", "GBP", "JPY"],
                    "numberingSystem" => &["latn"],
                    "timeZone" => &["UTC"],
                    "unit" => &["meter", "second", "byte"],
                    _ => {
                        return Err(Thrown(format!(
                            "RangeError: invalid key for supportedValuesOf: {key}"
                        )))
                    }
                };
                let items: Vec<Value> = vals.iter().map(|s| self.alloc_str(s.to_string())).collect();
                Value::heap(self.heap.alloc(HeapObj::Array(items)))
            }
            INTL_SUPPORTED_LOCALES_OF => {
                let list = self.canonicalize_locale_list(a0)?;
                let items: Vec<Value> = list.into_iter().map(|s| self.alloc_str(s)).collect();
                Value::heap(self.heap.alloc(HeapObj::Array(items)))
            }
            INTL_RESOLVED_OPTIONS => {
                let resolved = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::Intl { resolved, .. }) => *resolved,
                    _ => {
                        return Err(Thrown(
                            "TypeError: resolvedOptions called on an incompatible receiver".into(),
                        ))
                    }
                };
                self.clone_plain_object(resolved)
            }
            INTL_NF_FORMAT => {
                let resolved = self.intl_this(this, INTL_NUMBERFORMAT, "format")?;
                self.intl_number_format(resolved, a0)?
            }
            INTL_NF_FORMAT_TO_PARTS => {
                let resolved = self.intl_this(this, INTL_NUMBERFORMAT, "formatToParts")?;
                let formatted = self.intl_number_format(resolved, a0)?;
                let mut part = ObjMap::new();
                let ty = self.alloc_str("integer".to_string());
                part.set("type", ty);
                part.set("value", formatted);
                let p = Value::heap(self.heap.alloc(HeapObj::Object(part)));
                Value::heap(self.heap.alloc(HeapObj::Array(vec![p])))
            }
            INTL_DTF_FORMAT => {
                let resolved = self.intl_this(this, INTL_DATETIMEFORMAT, "format")?;
                let ms = if a0 == Value::UNDEFINED {
                    (Self::now_epoch_ns() / 1_000_000) as f64
                } else {
                    self.to_number(a0)?
                };
                let s = self.dtf_format(resolved, ms);
                self.alloc_str(s)
            }
            INTL_DTF_FORMAT_TO_PARTS => {
                let resolved = self.intl_this(this, INTL_DATETIMEFORMAT, "formatToParts")?;
                let ms = if a0 == Value::UNDEFINED {
                    (Self::now_epoch_ns() / 1_000_000) as f64
                } else {
                    self.to_number(a0)?
                };
                let s = self.dtf_format(resolved, ms);
                let mut part = ObjMap::new();
                let ty = self.alloc_str("literal".to_string());
                part.set("type", ty);
                let sv = self.alloc_str(s);
                part.set("value", sv);
                let p = Value::heap(self.heap.alloc(HeapObj::Object(part)));
                Value::heap(self.heap.alloc(HeapObj::Array(vec![p])))
            }
            INTL_COLLATOR_COMPARE => {
                let _ = self.intl_this(this, INTL_COLLATOR, "compare")?;
                let a = self.to_js_string(a0)?;
                let b = self.to_js_string(a1)?;
                Value::num(if a < b { -1.0 } else if a > b { 1.0 } else { 0.0 })
            }
            INTL_PLURAL_SELECT => {
                let _ = self.intl_this(this, INTL_PLURALRULES, "select")?;
                let n = self.to_number(a0)?;
                let cat = if n == 1.0 { "one" } else { "other" };
                self.alloc_str(cat.to_string())
            }
            INTL_PLURAL_SELECT_RANGE => {
                let _ = self.intl_this(this, INTL_PLURALRULES, "selectRange")?;
                self.alloc_str("other".to_string())
            }
            INTL_LIST_FORMAT => {
                let resolved = self.intl_this(this, INTL_LISTFORMAT, "format")?;
                let items = self.iterate_to_vec(a0)?;
                let mut strs: Vec<String> = Vec::with_capacity(items.len());
                for v in items {
                    strs.push(self.to_js_string(v)?);
                }
                let t = self.display(self.intl_slot(resolved, "type"));
                let conj = if t == "disjunction" { "or" } else { "and" };
                let s = format_list_en(&strs, conj);
                self.alloc_str(s)
            }
            INTL_LIST_FORMAT_TO_PARTS => {
                let resolved = self.intl_this(this, INTL_LISTFORMAT, "formatToParts")?;
                let items = self.iterate_to_vec(a0)?;
                let mut strs: Vec<String> = Vec::with_capacity(items.len());
                for v in items {
                    strs.push(self.to_js_string(v)?);
                }
                let t = self.display(self.intl_slot(resolved, "type"));
                let conj = if t == "disjunction" { "or" } else { "and" };
                let s = format_list_en(&strs, conj);
                let mut part = ObjMap::new();
                let ty = self.alloc_str("literal".to_string());
                part.set("type", ty);
                let sv = self.alloc_str(s);
                part.set("value", sv);
                let p = Value::heap(self.heap.alloc(HeapObj::Object(part)));
                Value::heap(self.heap.alloc(HeapObj::Array(vec![p])))
            }
            INTL_RTF_FORMAT | INTL_RTF_FORMAT_TO_PARTS => {
                let _ = self.intl_this(this, INTL_RELATIVETIMEFORMAT, "format")?;
                let v = self.to_number(a0)?;
                let unit = self.to_js_string(a1)?;
                let s = format_relative_time_en(v, &unit);
                if id == INTL_RTF_FORMAT {
                    self.alloc_str(s)
                } else {
                    let mut part = ObjMap::new();
                    let ty = self.alloc_str("literal".to_string());
                    part.set("type", ty);
                    let sv = self.alloc_str(s);
                    part.set("value", sv);
                    let p = Value::heap(self.heap.alloc(HeapObj::Object(part)));
                    Value::heap(self.heap.alloc(HeapObj::Array(vec![p])))
                }
            }
            INTL_DISPLAYNAMES_OF => {
                let resolved = self.intl_this(this, INTL_DISPLAYNAMES, "of")?;
                let code = self.to_js_string(a0)?;
                let fb = self.display(self.intl_slot(resolved, "fallback"));
                if fb == "none" {
                    Value::UNDEFINED
                } else {
                    self.alloc_str(code)
                }
            }
            INTL_LOCALE_TOSTRING => {
                let resolved = self.intl_this(this, INTL_LOCALE, "toString")?;
                self.intl_slot(resolved, "baseName")
            }
            INTL_LOCALE_MAXIMIZE | INTL_LOCALE_MINIMIZE => {
                let resolved = self.intl_this(this, INTL_LOCALE, "maximize")?;
                let bn = self.intl_slot(resolved, "baseName");
                self.make_locale(bn, Value::UNDEFINED)?
            }
            INTL_SEGMENTER_SEGMENT => {
                let _ = self.intl_this(this, INTL_SEGMENTER, "segment")?;
                // Minimal Segments object (full grapheme/word segmentation TBD).
                let s = self.to_js_string(a0)?;
                let mut o = ObjMap::new();
                let sv = self.alloc_str(s);
                o.set("@@seginput", sv);
                Value::heap(self.heap.alloc(HeapObj::Object(o)))
            }
            INTL_DURATION_FORMAT => {
                let _ = self.intl_this(this, INTL_DURATIONFORMAT, "format")?;
                let dur = self.to_duration(a0)?;
                let s = format_duration_en(&dur);
                self.alloc_str(s)
            }
            _ if (INTL_LOCALE_GET_BASE..INTL_LOCALE_GET_BASE + LOCALE_ACCESSORS.len() as u16)
                .contains(&id) =>
            {
                let field = LOCALE_ACCESSORS[(id - INTL_LOCALE_GET_BASE) as usize];
                let resolved = self.intl_this(this, INTL_LOCALE, field)?;
                self.intl_slot(resolved, field)
            }
            // The format/compare bound-function getters: return (and cache) a
            // function bound to the instance, so `nf.format === nf.format`.
            INTL_NF_FORMAT_GET | INTL_DTF_FORMAT_GET | INTL_COLLATOR_COMPARE_GET => {
                let (kind, target_id, svc) = match id {
                    INTL_NF_FORMAT_GET => (INTL_NUMBERFORMAT, INTL_NF_FORMAT, "format"),
                    INTL_DTF_FORMAT_GET => (INTL_DATETIMEFORMAT, INTL_DTF_FORMAT, "format"),
                    _ => (INTL_COLLATOR, INTL_COLLATOR_COMPARE, "compare"),
                };
                let resolved = self.intl_this(this, kind, svc)?;
                let cached = self.intl_slot(resolved, "@@boundfn");
                if cached != Value::UNDEFINED {
                    cached
                } else {
                    let nat = Value::heap(self.heap.alloc(HeapObj::Native(target_id)));
                    let b = Value::heap(self.heap.alloc(HeapObj::Bound {
                        target: nat,
                        this,
                        args: vec![],
                    }));
                    if let HeapObj::Object(m) = self.heap.get_mut(resolved) {
                        m.set("@@boundfn", b);
                    }
                    b
                }
            }
            // `Array.prototype.<m>` / `String.prototype.<m>` invoked as a value
            // (`.call`/`.apply`/`.bind` or `m()`): dispatch on the `this` receiver.
            _ if native::proto_method(id).is_some() => {
                let (m, kind, _len) = native::proto_method(id).unwrap();
                // A boxed primitive receiver unwraps to its [[PrimitiveValue]] so the
                // method runs on the primitive (`new Number(5).toFixed(2)`).
                let this = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::Boxed { value, .. }) => *value,
                    _ => this,
                };
                // Number/Boolean receivers are primitive values; the rest are heap.
                if kind == 2 {
                    self.number_method(this, m, args)?.unwrap_or(Value::UNDEFINED)
                } else if kind == 5 {
                    self.boolean_method(this, m)
                } else if kind == 1 {
                    // String methods are generic: RequireObjectCoercible(this) then
                    // ToString(this), so `String.prototype.slice.call(123, …)` works.
                    let s_idx = if this.is_heap() && self.heap.is_str_like(this.heap_index()) {
                        this.heap_index()
                    } else if this == Value::UNDEFINED || this == Value::NULL {
                        return Err(Thrown(format!(
                            "TypeError: String.prototype.{m} called on null or undefined"
                        )));
                    } else {
                        let s = self.to_js_string(this)?;
                        self.alloc_str(s).heap_index()
                    };
                    self.string_method(s_idx, m, args)?.unwrap_or(Value::UNDEFINED)
                } else if !this.is_heap() {
                    return Err(Thrown(format!(
                        "TypeError: prototype method {m} called on {}",
                        self.display(this)
                    )));
                } else {
                    let r = match kind {
                        0 => self.array_method(this.heap_index(), m, args)?,
                        1 => self.string_method(this.heap_index(), m, args)?,
                        3 => self.set_method(this.heap_index(), m, args)?,
                        4 => self.map_method(this.heap_index(), m, args)?,
                        6 => self.date_method(this.heap_index(), m, args)?,
                        _ => self.promise_method(this.heap_index(), m, args)?, // kind 7
                    };
                    r.unwrap_or(Value::UNDEFINED)
                }
            }
            _ => Value::UNDEFINED,
        })
    }

}
