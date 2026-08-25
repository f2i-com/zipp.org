//! Compile the AST directly to register bytecode.
//!
//! Two passes over the source:
//!
//! 1. **Hoist**: every top-level `function f(...)` and `var/let f = function`
//!    name is assigned a global slot, so calls resolve regardless of textual
//!    order (matching JS function hoisting) and recursion works.
//! 2. **Emit**: each function body compiles to a `FuncProto`. Locals and
//!    parameters live in registers, tracked by a `Scope` (name → register).
//!    Expression results flow into caller-chosen destination registers, which
//!    is what lets a value stay in one place across a basic block.
//!
//! The supported subset (v1): numbers/strings/bools/null, `let`/`const`/`var`,
//! function declarations + calls + recursion, `if/else`, `while`, C-style
//! `for`, `return`, the arithmetic/comparison/logical operators, and
//! `console.log`. Anything else is a clear compile error — coverage grows over
//! time, the same way the old engine did.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

// The AST, under both spellings: `ast::Expr` for code that wants the prefix and
// a bare `Expr` for code that does not. `Program` deliberately still names
// `crate::bytecode::Program` — an explicit import shadows a glob one, exactly as
// it did when the AST type was `ox::Program`; the AST's is `ast::Program`.
use crate::parse::ast::{self, *};
// Not re-exported by `ast` (it imports them privately), so they come from the
// token module: `StrVal` is every string literal's value.
use crate::parse::token::StrVal;

use crate::bytecode::{
    BitwiseOp, ClassDef, FuncProto, InstanceCtor, Instr, Program, Reg, UpvalSource, KEY_WRITEBACK,
};
use crate::capture;
use crate::value::Value;
use crate::vm::STRING_CONST_BIT;

type R<T> = Result<T, String>;

/// What `compile_class_fn` needs to know about a DECORATED class to emit the
/// instance-element part of its constructor / field thunk. `None` for every
/// undecorated class, which is the only shape existing code takes.
pub(crate) struct DecFieldPlan<'x> {
    /// The class whose runtime `DecState` the ops resolve through.
    pub class_id: u32,
    /// Parallel to the `fields` slice: the decorated-element index of that
    /// field, when it has decorators. (An auto-accessor's entry names its
    /// PRIVATE BACKING SLOT — that is the storage a returned `init` feeds.)
    pub named: &'x [Option<u16>],
    /// Parallel to the `computed_inits` slice: same, for `@dec [k] = v`.
    pub computed: &'x [Option<u16>],
    /// Whether the class has any decorated instance METHOD / getter / setter,
    /// i.e. whether `instanceMethodExtraInitializers` can be non-empty. A
    /// decorated field or auto-accessor does NOT count: its `addInitializer`
    /// callbacks are per-element and run right after that element is defined.
    pub run_inits: bool,
}

/// Which class-definition-time key evaluation a [`ClassStep`] performs.
pub(crate) enum StepKey {
    /// The element's key is a compile-time name; nothing to evaluate.
    None,
    /// Index into the `computed` (computed-key member) list.
    Member(usize),
    /// Index into the `computed_fields` list.
    Field(usize),
}

/// One class element that needs work at class-definition time, in DOCUMENT
/// order: its decorator expressions and/or its computed ClassElementName.
///
/// The two used to be driven by two SEPARATE loops (all computed member keys,
/// then all computed field keys), which is not document order —
/// `class C { [a] = 1; [b]() {} }` evaluated `b` before `a`. Decorators make
/// that observable in a second way (each element's decorators must be evaluated
/// immediately before its own key), so both now come off this one ordered list.
pub(crate) struct ClassStep<'b> {
    pub decorators: &'b [ast::Expr],
    /// Index into `DecPlan::elements`, when the element is decorated.
    pub dec_elem: Option<u16>,
    /// Which of ClassDefinitionEvaluation's four decoration loops this element
    /// belongs to: 0 = static non-field, 1 = instance non-field, 2 = static
    /// field, 3 = instance field. Elements are DECORATED in that group order
    /// (document order within a group), which is not document order overall.
    pub dec_group: u8,
    pub key: StepKey,
}

/// Everything `build_class_into` needs from `compile_class`.
pub(crate) struct CompiledClass<'b> {
    pub class_id: u32,
    pub static_fields: Vec<(String, Option<&'b ast::Expr>)>,
    /// (key expr, func id, kind, auto-accessor setter func id) per computed member.
    pub computed: Vec<(&'b ast::Expr, u32, u8, Option<u32>)>,
    /// (key expr, initializer, is_static) per computed-key FIELD.
    pub computed_fields: Vec<(&'b ast::Expr, Option<&'b ast::Expr>, bool)>,
    pub static_block_fns: Vec<u32>,
    /// Source order of STATIC elements: (0=named field, 1=computed field,
    /// 2=static block) -> index into its vec.
    pub static_order: Vec<(u8, usize)>,
    pub steps: Vec<ClassStep<'b>>,
    /// Parallel to `static_fields`: the decorated-element index of that field.
    pub dec_static_named: Vec<Option<u16>>,
    /// Parallel to `computed_fields`: same.
    pub dec_computed: Vec<Option<u16>>,
    /// Whether the class carries any decorator at all (class or element).
    pub has_dec: bool,
}

/// Shared, mutable upvalue list of a function: (name, where-the-cell-comes-from).
/// Shared via `Rc<RefCell>` so a deeply-nested function can append upvalues to
/// an ancestor (transitive capture): an intermediate function that only *passes
/// through* a variable still needs to capture it so its child can re-source it.
type UpvalList = Rc<RefCell<Vec<(String, UpvalSource)>>>;

/// A snapshot of an enclosing function's binding environment, used by a nested
/// function to resolve free variables to upvalues. The chain is ordered
/// outermost → innermost-parent; the last entry is the direct parent. The
/// `upvalues` handle is SHARED with the live ancestor `FnCompiler`, so resolving
/// a variable through this snapshot also records the upvalue on the ancestor.
#[derive(Clone)]
struct EnclosingFn {
    /// name → register holding that binding's CELL in the parent frame. Only
    /// captured bindings appear here (non-captured locals can't be upvalues).
    cell_locals: Vec<(String, Reg)>,
    /// The ancestor's own upvalue list (shared handle).
    upvalues: UpvalList,
}

struct Compiler {
    functions: Vec<FuncProto>,
    /// Program-wide source caps for compiler-prepared object-literal keys.
    /// These count retained plan sites/bytes, not runtime objects.
    static_key_plan_sites: usize,
    static_key_plan_retained_bytes: usize,
    /// Global name → slot. Ordered: the index IS the slot, and the VM indexes
    /// this directly, so entries are only ever appended.
    globals: Vec<String>,
    /// Reverse of `globals`, kept in step with it. Resolving a name used to be
    /// a linear scan with a string compare per entry, which is quadratic in the
    /// number of distinct globals — a program with 12k top-level functions spent
    /// ~70M string compares here, and compile time per megabyte tripled between
    /// a 0.4 MB and a 3.2 MB source. (Not to be confused with the property-name
    /// interning that PERF_ROADMAP records as a measured loss: that one sat on
    /// the runtime hot path, where the hash costs more than the small malloc it
    /// replaced. This is a compile-time lookup that was O(n).)
    global_index: rustc_hash::FxHashMap<String, u32>,
    /// Compiled class descriptors, indexed by the `MakeClass` class_id.
    classes: Vec<ClassDef>,
    /// Class name → class_id, for resolving `extends <Name>` and `super`.
    class_names: Vec<(String, u32)>,
    /// Slots for top-level `var` names — pre-initialized to `undefined` at VM
    /// startup (var hoisting), so a read before the textual decl isn't a
    /// never-declared ReferenceError.
    hoisted_globals: Vec<u32>,
    /// Membership index for `hoisted_globals`, which is an ORDERED dedup list
    /// (the VM replays it in declaration order). Same quadratic as
    /// `global_index` guarded against: one `contains` scan per top-level `var`.
    hoisted_set: rustc_hash::FxHashSet<u32>,
    /// The full program source, kept so each function can record its exact
    /// source slice (by span) for `Function.prototype.toString`.
    source: String,
    /// True when compiling an `eval` code string: the top-level script returns
    /// its *completion value* (the value of the last evaluated expression
    /// statement) instead of `undefined`.
    eval_mode: bool,
    /// Set for a DIRECT eval from a class-member context: the eval's top-level
    /// script (and its arrows) inherits the caller's home class for `super`
    /// (compiled against the u32::MAX sentinel).
    eval_inherit_super: Option<EvalClassCtx>,
    /// True while compiling a class FIELD INITIALIZER expression (or an eval
    /// program invoked from one): `arguments` is an early SyntaxError there.
    /// Arrows inherit (Compiler-level state); function/method bodies reset it.
    in_field_init: bool,
    /// True when compiling a MODULE as the program entry (not a dynamic import):
    /// the top-level body is an ASYNC context (top-level `await` is allowed), so
    /// func 0 is compiled with `in_async` and the VM runs it as an async activation.
    module_mode: bool,
    /// True only for a REAL eval program (do_eval): top-level lexicals are
    /// frame-locals (the spec's discarded eval lexEnv). False for modules,
    /// which also compile through compile_eval but whose top-level lexicals
    /// are live global-slot export bindings.
    eval_locals: bool,
    /// False for a STRICT eval program: its top-level var/function declarations
    /// live in the eval's own (discarded) variable environment — frame locals —
    /// instead of the realm's globals. True everywhere else.
    script_binds_globals: bool,
    /// Direct eval from an OBJECT-literal method/accessor: the eval top level
    /// (and arrows) resolve `super.x` via the caller's runtime [[HomeObject]].
    eval_inherit_super_obj: bool,
    /// For a DIRECT eval program: the caller bindings (ordered) the runtime
    /// supplies as the eval closure's upvalue cells. Free names in the eval
    /// resolve to these as UpvalGet/UpvalSet before falling back to globals.
    eval_caller_scope: Vec<String>,
    /// The subset of `eval_caller_scope` that lives in an ENCLOSING function of
    /// the caller, not in the caller's own activation. Those bindings are
    /// READABLE (they are ordinary upvalue cells) but they are not the eval's
    /// variable environment, so a top-level `var`/`function` of the same name
    /// creates a fresh binding that SHADOWS them rather than assigning through
    /// — `function o(){ var a=1; (function(){ eval("var a=5") })(); return a }`
    /// must still see 1.
    eval_outer_scope: Vec<String>,
    /// The subset of `eval_caller_scope` that is a CATCH PARAMETER of the
    /// caller's activation. Such a name is ALSO in `eval_outer_scope` (so a
    /// top-level `var` of it declares into the caller's varEnv — Annex B.3.5),
    /// but the eval body's reads/writes resolve to the param CELL (lexically
    /// nearer than the new varEnv binding), never to the dynamic EvalScope —
    /// their upvalue accesses must be the PLAIN ops, not the Dyn ones.
    eval_catch_params: Vec<String>,
    /// FUNCTION-context sloppy eval: the eval's own var/function names that
    /// live in the caller's dynamic EvalScope (the Dyn global ops find them).
    eval_dynamic_names: std::collections::HashSet<String>,
    /// True for a sloppy FUNCTION-context eval program: its global accesses
    /// must be dynamic-first (the caller activation may carry an EvalScope).
    eval_fn_context: bool,
    /// True while compiling code lexically inside a contains-direct-eval
    /// function: global accesses at ANY nesting depth compile to the Dyn ops.
    dyn_global_zone: bool,
    /// Force strict mode for the whole compilation, regardless of a `"use strict"`
    /// directive — set for a DIRECT eval invoked from strict-mode code (the
    /// evaluated string inherits the caller's strictness).
    force_strict: bool,
    /// Allow `new.target` at the eval script top level — set for a DIRECT eval
    /// invoked from inside a function/method/class-field initializer (the eval
    /// inherits the caller's new.target validity).
    force_new_target_ok: bool,
    /// Strictness of the lexical scope currently being compiled. Set on entry to
    /// a function/arrow body (inherited from the enclosing scope, OR'd with the
    /// body's own `"use strict"` directive), forced `true` inside class bodies,
    /// and seeded from module-ness for the top-level script. A function records
    /// this into `FuncProto.is_strict`; the VM uses it to decide `this`
    /// substitution at the call site.
    in_strict: bool,
    /// >0 while compiling a strict ClassTail region (heritage expression,
    /// computed member/field keys, static field initializers) INLINE into an
    /// enclosing function that may itself be sloppy. SetProp emissions consult
    /// it for their `strict` operand (the proto-level `is_strict` flag can't
    /// see these regions).
    strict_expr_region: u32,
    /// The enclosing-function chain that the methods of the class CURRENTLY being
    /// compiled close over (set by `compile_class`, read by `compile_class_fn`).
    /// Empty at script level, so script-level class methods keep free vars as
    /// globals. Saved/restored around each class to handle nesting.
    class_enclosing: Vec<EnclosingFn>,
    /// True while compiling the methods of a DERIVED class (`class X extends …`):
    /// gates `super(...)` (a base class's constructor calling `super()` is an early
    /// SyntaxError). `super.x` property access is allowed in ANY class method, so it
    /// uses the per-method home-class id instead. Saved/restored around each class.
    class_derived: bool,
    /// Set by the class compiler around the CONSTRUCTOR's `compile_class_fn`
    /// call only (consumed at its entry), so the ctor body — and nothing else —
    /// gets derived-ctor this-TDZ checks.
    compiling_ctor: bool,
    /// Private names declared by each enclosing class body (innermost last),
    /// pushed by compile_class / popped by build_class_into. A private access
    /// whose name no enclosing class declares is an early SyntaxError.
    private_names_stack: Vec<Vec<String>>,
    /// Class names whose HERITAGE expression is currently compiling (innermost
    /// last): the spec classScope makes a named class's own binding visible
    /// (in TDZ, immutable) throughout `extends ...`, including functions
    /// created inside it.
    heritage_classes: Vec<(String, u32)>,
    /// For DIRECT eval programs: private names lexically visible at the eval
    /// call site (the caller's brand-chain names). Empty otherwise.
    eval_visible_privates: std::collections::HashSet<String>,
    /// True while compiling a derived class's constructor BODY (and read by
    /// arrows lexically inside it). Gates `ThisCheck` emission on `this` reads
    /// and super-property references.
    in_derived_ctor: bool,
    /// One-shot, set for a CreateDynamicFunction (`new Function` & kin) eval
    /// compile: the FIRST function named "anonymous" (the wrapper — compilation
    /// is depth-first, so it precedes any same-named function nested in its
    /// parameter defaults or body) gets NO self-name binding. The created
    /// function's params/body are parsed and instantiated separately, so
    /// `typeof anonymous` inside it is "undefined" (constructor-binding.js).
    fn_ctor_no_self_name: bool,
    /// Global slots bound by a top-level `const` (immutable): assignment to one
    /// is a runtime TypeError.
    const_globals: HashSet<u32>,
    /// Global slots bound by a top-level `let`/`const` (lexical) declaration. Such a
    /// binding is in its TDZ — the slot is UNINITIALIZED — until its declaration runs,
    /// so an assignment to it before then is a ReferenceError even in sloppy mode
    /// (where an *undeclared* name would instead create a global). Registered by a
    /// hoisting pre-pass so a forward reference (`for([x] of …){} let x;`) is known.
    lexical_globals: HashSet<u32>,
    /// Global slots bound by a top-level FUNCTION or CLASS declaration. Like a
    /// global `var`/`let`/`const`, these are non-configurable bindings, so
    /// `delete <name>` on one yields `false`.
    decl_globals: HashSet<u32>,
    /// True while compiling code where `new.target` is syntactically allowed —
    /// inside an ordinary function/method/constructor/class-field body, and
    /// inherited by nested arrows. False at the top level of a script or eval (a
    /// `new.target` there is an early SyntaxError). Saved/restored per function.
    new_target_ok: bool,
    /// For a MODULE compile: the (exported name, local name) pairs collected from
    /// `export` declarations, in source order. The loader reads each local's
    /// top-level (eval-global) binding after the module runs to build its
    /// namespace. Empty for scripts/eval (which have no `export`).
    module_exports: Vec<(String, String)>,
    /// `export {imported as exported} from 'spec'` re-exports: (exported, imported,
    /// specifier). Resolved by the loader against the dependency module.
    module_reexports: Vec<(String, String, String)>,
    /// `export * from 'spec'` star re-exports: the specifier string.
    module_star_reexports: Vec<String>,
    module_ns_reexports: Vec<(String, String)>,
    module_imports: Vec<crate::bytecode::ImportEntry>,
    /// Monotonic counter giving every `with`-object hidden local a UNIQUE name
    /// (" with-object-N"), so a nested closure capturing two different with
    /// scopes resolves each to the right cell across the enclosing chain.
    with_name_counter: u32,
    /// Transient (consumed by `FnCompiler::new`): for each free name of the
    /// function about to be compiled, the ordered (innermost-first) chain of
    /// enclosing with-object binding names that may shadow it at runtime.
    /// Stashed by `stash_child_with_shadows` right before the nested compile.
    pending_with_shadows: std::collections::HashMap<String, Vec<String>>,
    /// Transient: set by the object-literal compiler right before compiling a concise
    /// method / get-set accessor value, so that function's body compiles with
    /// `super_home_obj = true` (object-method super). Consumed (taken) when the
    /// function body starts compiling.
    obj_method_super: bool,
}

/// Resolve `name` to an `UpvalSource` for the INNERMOST function whose enclosing
/// chain is `chain` (outermost → direct parent). The direct parent is
/// `chain.last()`. If the variable is a cell-local of the direct parent, the
/// source is `ParentLocal`. Otherwise the parent must itself capture it as one
/// of its upvalues — found, or recursively created by capturing from the
/// grandparent — and the source is `ParentUpval(parent's upvalue index)`. This
/// is the standard Lua "find/create upvalue" walk, threading a capture through
/// every intermediate level. `None` if no enclosing function binds `name`.
fn capture_source(chain: &[EnclosingFn], name: &str) -> Option<UpvalSource> {
    let (parent, outer) = chain.split_last()?;
    // Direct parent holds the cell as a local. Scanned BACKWARDS: `cell_locals`
    // is weakest-binding-first, so the last match is the innermost one in scope
    // at the child's creation point — the binding `resolve` would pick in the
    // parent. Taking the first match handed a closure inside `{ let a; }` the
    // OUTER `a`'s cell.
    if let Some((_, reg)) = parent.cell_locals.iter().rfind(|(n, _)| n == name) {
        return Some(UpvalSource::ParentLocal(*reg));
    }
    // Parent already captured it as an upvalue.
    if let Some(i) = parent.upvalues.borrow().iter().position(|(n, _)| n == name) {
        return Some(UpvalSource::ParentUpval(i as u16));
    }
    // Otherwise make the parent capture it from ITS enclosing chain, then point
    // at the parent's freshly-added upvalue.
    let src_for_parent = capture_source(outer, name)?;
    let mut pups = parent.upvalues.borrow_mut();
    let idx = pups.len() as u16;
    pups.push((name.to_string(), src_for_parent));
    Some(UpvalSource::ParentUpval(idx))
}

/// A method's `Function.prototype.toString` source. For a `static` member the
/// `static` keyword is part of the ClassElement, not the MethodDefinition, so
/// it is excluded from the method's [[SourceText]] (e.g. `static s(){}` →
/// `s(){}`, `static get x(){}` → `get x(){}`). When `is_static` the slice begins
/// with the `static` keyword (the parser flagged it), so strip it + trailing ws.
fn method_source(text: String, is_static: bool) -> String {
    if is_static {
        if let Some(rest) = text.strip_prefix("static") {
            // The MethodDefinition's [[SourceText]] starts at its first real token
            // (name / get / set / async / * / [). Whitespace AND comments between
            // `static` and that token are trivia, not part of the method source.
            return skip_leading_trivia(rest).to_string();
        }
    }
    text
}

/// Drop leading whitespace and comments (`// …` line + `/* … */` block) — the
/// trivia between a `static` keyword and the start of the MethodDefinition.
fn skip_leading_trivia(s: &str) -> &str {
    let mut t = s.trim_start();
    loop {
        if let Some(rest) = t.strip_prefix("//") {
            t = match rest.find(|c| c == '\n' || c == '\r') {
                Some(i) => rest[i..].trim_start(),
                None => "",
            };
        } else if let Some(rest) = t.strip_prefix("/*") {
            t = match rest.find("*/") {
                Some(i) => rest[i + 2..].trim_start(),
                None => "",
            };
        } else {
            return t;
        }
    }
}

fn placeholder(name: &str) -> FuncProto {
    FuncProto {
        name: name.to_string(),
        code: Vec::new(),
        reg_count: 0,
        param_count: 0,
        length: 0,
        rest_reg: None,
        arguments_reg: None,
        is_generator: false,
        is_async: false,
        non_constructable: false,
        lexical_this: false,
        super_static: false,
        is_strict: false,
        simple_params: false,
        constants: Vec::new(),
        string_constants: Vec::new(),
        static_key_plans: Vec::new(),
        bigint_consts: Vec::new(),
        wtf8_consts: Vec::new(),
        name_global: None,
        upvalues: Vec::new(),
        eval_sites: Vec::new(),
        source: String::new(),
    }
}

/// True if a directive prologue opens with `"use strict"`. Per spec the match is
/// against the directive's RAW source (so an escaped `"use strict"` does NOT
/// count); `Directive.raw` holds exactly that unescaped-but-raw text, which is
/// why the cooked `Directive.value` is not what is compared.
fn has_use_strict(directives: &[ast::Directive]) -> bool {
    directives.iter().any(|d| &*d.raw == "use strict")
}

/// A function's directive prologue, used to detect its own `"use strict"`.
///
/// Every caller wants the body's own prologue OR'd with `cx.in_strict`; inherited
/// strictness is tracked separately, so the prologue scan stays.
fn fn_directives(f: &ast::Function) -> &[ast::Directive] {
    // A bodiless function (TypeScript declaration / overload signature) is not
    // representable, so the `unwrap_or(&[])` the oxc shape needed is gone.
    &f.body.directives
}

/// Per-function compilation state.
struct FnCompiler<'a> {
    cx: &'a mut Compiler,
    code: Vec<Instr>,
    constants: Vec<Value>,
    string_constants: Vec<String>,
    static_key_plans: Vec<crate::bytecode::StaticKeyPlan>,
    /// BigInt literal constants beyond i128 (see `FuncProto::bigint_consts`).
    bigint_consts: Vec<num_bigint::BigInt>,
    /// `string_constants` indices holding the lone-surrogate MARKER form
    /// (see `Function::wtf8_consts`).
    wtf8_consts: Vec<u32>,
    /// Lexical scope chain: each entry is (name, register).
    scopes: Vec<Vec<(String, Reg)>>,
    /// Parallel to `scopes` (same length, same indices): the lexically-declared
    /// names of each BLOCK-like scope, recorded at block ENTRY.
    ///
    /// `block_fn_conflicts` used to read `scopes`, which only holds bindings
    /// already declared — so a `let` written AFTER a nested block did not block
    /// that block's Annex B B.3.3 promotion (`{ { function x(){} } let x; }`
    /// promoted, and the later declaration clobbered an earlier legitimate one).
    /// B.3.3.1's "would not produce any Early Errors" test spans the whole
    /// enclosing block, not the part textually above the declaration.
    scope_lex_names: Vec<HashSet<String>>,
    /// Next free register / high-water mark.
    next_reg: Reg,
    max_reg: Reg,
    /// Set when `alloc_reg` ran out of the u16 register space. `Reg` is a `u16`
    /// and `FuncProto::reg_count` is a `u16`, so a frame simply cannot address
    /// more than `u16::MAX` registers; before this flag existed the counter
    /// wrapped and the function silently miscompiled (a 70k-element array
    /// literal produced `length === 4464` AND clobbered unrelated locals,
    /// because `next_reg` wrapped back over live registers). Allocation now
    /// saturates instead of wrapping, and every site that finalises a
    /// `FuncProto` refuses to emit one when this is set.
    reg_overflow: bool,
    /// Register holding the rest-parameter array, if this function has one.
    rest_reg: Option<Reg>,
    /// Register reserved for the `arguments` object (non-arrow functions), and
    /// whether the body actually referenced `arguments` (gates building it).
    arguments_reg: Option<Reg>,
    uses_arguments: bool,
    /// When compiling a class method/constructor, THIS class's class_id — the home
    /// for `super.x`/`super.m()`, which resolve at runtime via the home prototype's
    /// [[Prototype]] (so they work in base classes too, reaching %Object.prototype%).
    super_class: Option<u32>,
    /// True when compiling an OBJECT-LITERAL method/accessor (or an arrow lexically
    /// inside one). `super.x` then resolves via the runtime [[HomeObject]] (the object
    /// the method was defined in) — emitted as the Super*Obj ops — rather than the
    /// compile-time class home. Mutually distinct from `super_class` (a fresh function
    /// has neither unless set for its kind).
    super_home_obj: bool,
    /// True when compiling a STATIC class element (static method/getter/setter or
    /// `static { … }` block, and arrows lexically inside one). Selects the static
    /// super base (the class's [[Prototype]] = parent class) at runtime. Baked into
    /// the emitted function's `FuncProto::super_static`.
    super_static: bool,
    /// True only inside a DERIVED class's methods — gates `super(...)` (calling it in
    /// a base class constructor is an early SyntaxError). `super.x` is not gated.
    derived_class: bool,
    /// True only inside a derived class's CONSTRUCTOR body (and arrows
    /// lexically inside it): `this` reads and super-property references emit a
    /// `ThisCheck` (ReferenceError until `super()` has completed).
    in_derived_ctor: bool,
    /// Set while THIS FnCompiler compiles a named class's heritage expression:
    /// the inner class binding shadows even this function's locals/params for
    /// the duration (classScope nests inside the function scope). Nested
    /// functions inside the heritage use the cx-level `heritage_classes`
    /// instead (after their own scopes, so their params still shadow).
    heritage_class: Option<(String, u32)>,
    /// True when this function's body references `eval` (a possible direct
    /// eval): EVERY local is boxed into a cell so the eval program can close
    /// over the caller scope; DirectEval sites record the visible bindings.
    box_all_locals: bool,
    /// SCRIPT top level whose program references `eval`: BLOCK-level lexicals
    /// (which are true locals even at script level) are boxed into cells and
    /// each direct-eval site records its visible-bindings map, so the eval
    /// program can close over them (`{ let x; eval('x') }` at script level).
    /// Never set for non-script bodies (those use `box_all_locals`).
    script_eval_lexicals: bool,
    /// Scope maps for this function's DirectEval call sites (see
    /// FuncProto::eval_sites).
    eval_sites: Vec<(Vec<(String, u8, u16)>, Option<Vec<String>>, Vec<String>)>,
    /// True while this function's parameter defaults compile: a direct eval
    /// there sits in the PARAM scope (its sloppy var/function declarations
    /// collide with parameter names / the implicit `arguments`).
    in_param_init: bool,
    /// This function's parameter names (incl. rest) for that collision check.
    param_names: Vec<String>,
    /// When set, `this` resolves to this register instead of reg 0. Used while
    /// evaluating static field initializers inline at class-definition time,
    /// where `this` must be the class value (not the enclosing `this`) — without
    /// moving the init into a non-capturing thunk (which would lose closure over
    /// the enclosing scope).
    this_override: Option<Reg>,
    /// Set transiently around a block-nested lexical destructuring declaration at
    /// SCRIPT level so `declare_pattern` binds the pattern's leaves as block-locals
    /// (not globals), mirroring the simple-identifier `let`/`const` path — block
    /// `let {a} = …` must not leak to the global scope. Off everywhere else.
    pattern_block_local: bool,
    /// True while compiling a `function*` body, so `yield` is allowed.
    in_generator: bool,
    /// True while compiling an `async` body, so `await` is allowed.
    in_async: bool,
    /// A label seen on a `LabeledStatement`, consumed by the immediately-following
    /// loop/switch so `break label` / `continue label` can target it.
    pending_label: Option<String>,
    /// True for the top-level script body: declarations (functions AND
    /// let/const/var) bind to globals rather than registers, so only genuinely
    /// nested functions ever capture.
    is_script: bool,
    /// For an `eval` top-level script: a persistent register that accumulates the
    /// completion value (last evaluated expression statement). `Return`ed at the
    /// end instead of `undefined`. `None` outside eval mode / in nested functions.
    completion_reg: Option<Reg>,
    /// A named function expression's own name bound to itself (the running
    /// function value): `(name, reg)`. Sits OUTSIDE the parameter/var scope, so
    /// `resolve` consults it only after the scope stack (params/locals shadow it).
    /// `None` for declarations, arrows, methods, and anonymous expressions.
    self_name: Option<(String, Reg)>,
    /// This function's own bindings that some nested function captures; these
    /// are boxed into heap cells at declaration so the closure shares the slot.
    captured: HashSet<String>,
    /// Registers currently holding a CELL (a boxed captured binding), so reads/
    /// writes of them go through CellGet/CellSet.
    cell_regs: HashSet<Reg>,
    /// Registers bound by a `const` (immutable local): assignment to one is a
    /// runtime TypeError. Each const local has a unique register, so reassignment
    /// is detected by register identity (no name-shadowing ambiguity).
    const_regs: HashSet<Reg>,
    /// Registers bound by LEXICAL local declarations (let/const/class) —
    /// visible-lexical collection for direct-eval site maps.
    lexical_regs: HashSet<Reg>,
    /// Parameter names currently in the Temporal Dead Zone while a parameter
    /// default initializer is being compiled: a default that references the
    /// parameter itself (`(x = x)`) or a later parameter (`(x = y, y)`) reads a
    /// name in this set and throws a ReferenceError. Empty except inside
    /// `bind_params`.
    param_tdz: HashSet<String>,
    /// Upvalues this function captures, built lazily as free vars are resolved:
    /// (name, source-in-parent). Index in this Vec is the runtime upvalue index.
    /// Shared (`Rc<RefCell>`) so nested functions can append transitively.
    upvalues: UpvalList,
    /// Enclosing functions' binding snapshots (outermost → direct parent).
    enclosing: Vec<EnclosingFn>,
    /// Active `with` scopes (innermost last). Each records the register holding
    /// the (ToObject'd) with-object and the lexical-scope depth at which the
    /// `with` was entered: an identifier whose static binding lives in a scope
    /// SHALLOWER than this `floor` (or is a global/upvalue) can be shadowed by
    /// the with-object and so resolves dynamically; a binding declared INSIDE
    /// the with body (depth ≥ floor) is not shadowed. Empty except inside a
    /// `with` body, so non-`with` code compiles identically (zero regression).
    with_stack: Vec<WithScope>,
    /// `with` scopes of ENCLOSING functions that may shadow this function's
    /// free names: free name → ordered (innermost-first) chain of with-object
    /// binding names (each resolvable as an upvalue). Computed by the parent
    /// (`stash_child_with_shadows`) with its floor-vs-depth logic, so a name
    /// bound INSIDE a with body never appears. Empty for code not nested in a
    /// `with` (zero cost on the common path).
    inherited_with_shadows: std::collections::HashMap<String, Vec<String>>,
    /// Annex B B.3.3: names of functions declared inside BLOCKS of this (sloppy,
    /// non-script) function body that also get a function-scoped `var` binding,
    /// synced to the function value when the block declaration executes. Excludes
    /// names with a lexical conflict (which would be an early error → B.3.3 skipped).
    b33_names: std::collections::HashSet<String>,
    /// Per-function counter assigning a stable site index to each tagged-template
    /// literal, so the VM can memoize one canonical template object per (func, site).
    template_site_count: u32,
    /// Annex B B.3.3: function-body-level names that a same-named block function
    /// must NOT overwrite — formal parameters, lexical (`let`/`const`) bindings,
    /// and class declarations. A block function with one of these names stays
    /// purely block-local (the "skip" cases), unlike a name matching an existing
    /// `var`/function (which gets the function-scoped update via `b33_names`).
    protect_names: std::collections::HashSet<String>,
    /// Function-body-level lexical (`let`/`const`/`class`) names that were
    /// pre-created as cells at entry because a nested function captures them, so a
    /// function materialised at entry (forward reference) can bind their cell. The
    /// textual declaration REUSES this cell instead of allocating a fresh binding.
    /// Empty unless a body has a captured forward-referenced lexical.
    entry_lexicals: std::collections::HashSet<String>,
    /// Registers of BLOCK-level lexical (`let`/`const`/`class`) bindings
    /// pre-created as TDZ cells at block entry (because a nested function
    /// captures them), whose textual declaration has not yet been compiled.
    /// The declaration REUSES the register (ending its TDZ); an ASSIGNMENT
    /// through `store_binding` while still in this set emits the checked
    /// `CellSetChecked` (a write during the TDZ is a ReferenceError).
    /// Cleaned per-register in `pop_scope`.
    block_tdz_cells: HashSet<Reg>,
    /// Registers of ARROW-BODY-level lexical cells pre-created in TDZ at entry
    /// (so a hoisted nested function can capture them) whose textual
    /// declaration has not been compiled yet. Gates ONLY the choice of
    /// `CellSetChecked` over `CellSet` in `store_binding`, so that an
    /// ASSIGNMENT reaching one during its TDZ is the ReferenceError it should
    /// be — `(() => { b = 0; let b; })()`.
    ///
    /// Deliberately NOT `block_tdz_cells`: that set has a second job (deciding
    /// whether a textual declaration REUSES the pre-created register), and
    /// entry cells must not participate in that.
    ///
    /// A register LEAVES this set at its declaration — including the
    /// destructuring path — because `store_binding` also emits the
    /// declaration's OWN initializing store, and a checked store there would
    /// throw on the very cell it is initializing.
    entry_tdz_cells: HashSet<Reg>,
    /// Registers holding CATCH PARAMETERS (simple-identifier `catch (e)`
    /// bindings). Annex B B.3.5 allows a same-named `var` / promoted block
    /// function alongside one (no early error), so the B.3.3-applicability
    /// check skips these scope entries. Cleaned per-register in `pop_scope`.
    catch_param_regs: HashSet<Reg>,
    /// Active optional-chain short-circuit targets: a stack (chains can nest),
    /// each entry collecting the ip of every `?.` nullish-bail jump in that chain.
    /// On exit the chain patches them to a "load undefined" block.
    chain_bails: Vec<Vec<u32>>,
    /// Enclosing loop contexts (innermost last) for `break`/`continue`: each
    /// collects the jump ips to patch to the loop's end (break) and continue
    /// point (continue).
    loop_ctx: Vec<LoopCtx>,
    /// Count of `try` handlers (catch and finally) statically active at the
    /// current compilation point — mirrors the runtime handler-stack depth here.
    /// A `break`/`continue` whose target loop has a SMALLER handler depth must
    /// unwind the difference (running any intervening `finally`), so it compiles
    /// to `JumpFinally` instead of `Jump`. Maintained across try/catch/finally
    /// regions (see `try_with_finally` / `try_catch_only`).
    handler_depth: usize,
    /// Register holding the runtime resource-scope id of the innermost enclosing
    /// `using` block currently being compiled (set by `compile_using_block`), so a
    /// `using` declaration's `RegisterDisposable` knows which scope to push onto.
    /// `None` outside any `using` block; saved/restored across block nesting.
    using_scope_reg: Option<Reg>,
}

/// Pending `break`/`continue` jumps for one enclosing breakable construct. A
/// `switch` is a break target but NOT a continue target, so `continue` skips
/// switch frames to the innermost loop (`is_loop`).
struct LoopCtx {
    break_jumps: Vec<u32>,
    continue_jumps: Vec<u32>,
    is_loop: bool,
    /// Whether a BARE `break` may target this frame. True for iteration
    /// statements and `switch` (the only two the grammar lets an unlabelled
    /// `break` leave), false for the frame a labelled non-loop statement pushes
    /// — `L: if (x) { break; }` is a SyntaxError even though `break L` is fine.
    bare_breakable: bool,
    /// The label attached to this loop/switch (`outer: for …`), for labeled
    /// `break outer` / `continue outer`.
    label: Option<String>,
    /// The `handler_depth` in effect where this loop/switch was entered — the
    /// `floor` a `break`/`continue` targeting it must unwind the handler stack to.
    handler_depth: usize,
    /// For a `for-of` / `for-await-of` frame: the register holding the live
    /// iterator, so an abrupt `break`/`continue` LEAVING this loop from inside a
    /// nested one runs IteratorClose on it (a `break` targeting this loop closes
    /// via its own block). `None` for other loops.
    iter_close: Option<Reg>,
    /// Set when this for-of's body runs under the `IterCloseFinally` handler, which
    /// already closes on a `return` completion — `return` must then NOT also emit an
    /// inline `IterClose` for this frame or the iterator closes twice. False for
    /// `for await`, whose inline close is still the only one.
    close_via_finally: bool,
}

/// A pre-evaluated destructuring member-target key (see `pre_member_ref`).
enum PreKey {
    Static(u32),
    Computed(Reg),
    Private(u32),
    /// `super.x` as a destructuring target. A SuperProperty reference has no
    /// object register — its base is `[[HomeObject]].[[Prototype]]`, resolved by
    /// the Super* opcodes at store time — so `pre_member_ref` pairs this with a
    /// dummy `obj` reg that `store_pre_ref` ignores.
    Super(u32),
    /// `super[k]`: the key expression is evaluated (and pinned) when the
    /// reference is taken, ahead of the source read.
    SuperComputed(Reg),
}

/// One active `with` scope (see `FnCompiler::with_stack`).
struct WithScope {
    /// Register holding the ToObject'd with-object, kept live across the body
    /// (allocated as a hidden scope-local so per-statement temp resets preserve it).
    obj_reg: Reg,
    /// Lexical-scope depth (`scopes.len()`) at the point the `with` was entered.
    floor: usize,
}

impl LoopCtx {
    fn loop_frame(label: Option<String>, handler_depth: usize) -> LoopCtx {
        LoopCtx {
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            is_loop: true,
            bare_breakable: true,
            label,
            handler_depth,
            iter_close: None,
            close_via_finally: false,
        }
    }
    fn switch_frame(label: Option<String>, handler_depth: usize) -> LoopCtx {
        LoopCtx {
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            is_loop: false,
            bare_breakable: true,
            label,
            handler_depth,
            iter_close: None,
            close_via_finally: false,
        }
    }
    /// The frame a LABELLED non-loop/non-switch statement pushes so `break
    /// label` can complete it (14.13.3). Reachable only by its own label.
    fn label_frame(label: String, handler_depth: usize) -> LoopCtx {
        LoopCtx {
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            is_loop: false,
            bare_breakable: false,
            label: Some(label),
            handler_depth,
            iter_close: None,
            close_via_finally: false,
        }
    }
}

/// The CLASS context a direct eval inherits from its caller (PerformEval: the
/// eval's lexical environment is the caller's, so the caller's `super` binding,
/// its derived-constructor `super()` right, and its class inner-name binding are
/// all in scope for the eval'd code).
///
/// This used to be a bare `Option<bool>` carrying only `super_static`, which
/// dropped the other two: `eval("super()")` in a derived constructor was a
/// SyntaxError, and `eval("C")` in a class element missed the inner name (it
/// hit the outer binding — still in TDZ for a static field initializer).
#[derive(Clone, Debug, Default)]
pub struct EvalClassCtx {
    /// The caller is a STATIC class element, so `super.x` starts at the class's
    /// own [[Prototype]] rather than at `prototype`.
    pub static_ctx: bool,
    /// The caller is a DERIVED-class constructor, so `super(...)` is legal.
    pub derived_ctor: bool,
    /// The caller class's inner NAME binding, if the class has a name.
    pub name: Option<String>,
}

enum Binding {
    /// Plain register-resident local (the fast path; no capture).
    Local(Reg),
    /// Local that has been boxed into a heap cell because a nested function
    /// captures it; the register holds the cell reference.
    LocalCell(Reg),
    /// A variable captured from an enclosing function: index into this
    /// function's upvalue list.
    Upvalue(u16),
    Global(u32),
    /// The inner immutable class-name binding inside a class body (resolved at
    /// runtime from `class_values[class_id]`); read-only — assignment is a
    /// TypeError. Visible to methods/ctor/static-blocks and arrows within them.
    ClassName(u32),
}

// submodules (split out of the former monolithic compile.rs)
mod assign;
mod bindings;
mod calls;
mod compiler;
mod control_flow;
mod decls;
mod entry;
mod exprs;
mod funcs;
mod helpers;
mod scopes;
mod string_accum;
mod with_stmt;

pub(crate) use entry::*;
pub(crate) use helpers::*;
pub(crate) use string_accum::*;
