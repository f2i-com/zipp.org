#!/usr/bin/env python3
"""Lossless splitter for oversized Rust source files in this workspace.

Generalises `tools/split_vm.py` (which split the former monolithic `vm.rs`)
into a config-driven tool: a giant `foo.rs` becomes a `foo/` directory whose
`mod.rs` keeps the module preamble (doc comment, imports, type/struct/enum/const
definitions) and whose submodules carry the bodies.

It is a PURE CODE MOVE. Nothing is rewritten except:
  * item visibility is widened to `pub(crate)` so the pieces can still see each
    other (they used to be one module), and
  * each submodule gets `use super::*;` so the parent's imports stay in scope
    (a child module can see its parent's private `use` bindings).

Every split self-verifies that the concatenation of all emitted regions is
BYTE-IDENTICAL to the region it replaced, so a split can never silently drop or
duplicate code.

Usage:
  python tools/split_rs.py --list                # show configured targets
  python tools/split_rs.py codegen               # split one target
  python tools/split_rs.py codegen compile       # split several
  python tools/split_rs.py --all
"""
import argparse
import os
import re
import sys

# The workspace sources are CRLF; generated lines must match so the file stays
# consistent under `core.autocrlf` and so diffs stay clean.
NL = "\r\n"

# --------------------------------------------------------------------------
# Split plans.
#
# Each plan describes ONE oversized file:
#   src / out    : source file, and the directory it becomes
#   move_impls   : whole `impl` blocks lifted verbatim into their own module,
#                  keyed by the exact text of the opening line.
#   split_impls  : ONE giant `impl` block partitioned into several modules at
#                  method boundaries; `groups` is [(module, first_method), ...]
#                  in source order (the first group starts at the impl body).
#   free_groups  : the trailing top-level free items (fns/consts/enums/structs)
#                  partitioned into modules; [(module, first_item), ...] in
#                  source order. `free_start` names the first item that moves;
#                  everything above it stays in mod.rs.
# --------------------------------------------------------------------------
PLANS = {
    # ---------------------------------------------------------------- codegen
    # 9.9k lines: a small type/driver preamble followed by ~93 free functions
    # that make up the x64 emitter. mod.rs keeps the types + the `Jit` driver.
    "codegen": {
        "src": "crates/zipp-vm/src/codegen.rs",
        "out": "crates/zipp-vm/src/codegen",
        "move_impls": [],
        "split_impls": [],
        "free_regions": [{"start": "can_compile", "groups": [
            ("fn_int", "can_compile"),          # whole-function int JIT + guards
            ("self_call", "SELF_CALL_DEOPT"),   # self-recursive direct calls
            ("kernels", "can_kernel_body"),     # fused map/filter/reduce kernels
            ("region_admit", "BOOL_TAG"),       # region admission + leaf-inline gating
            ("plan", "VTy"),                    # home/regalloc plan types
            ("absint", "IV_FULL"),              # abstract interval analysis (i53 guards)
            ("plan_region", "plan_region"),     # the region planner + field promotion
            ("regalloc", "compile_region_regalloc"),
            ("region_int", "TWO_POW_53"),       # the unboxed i64 region path
            ("emit", "emit_int_entry_load"),    # low-level emit helpers
            ("inline", "emit_region_call_ic"),  # call ICs + leaf/method/accessor inlining
            ("region_mem", "compile_region_mem"),
            ("proto_mem", "mem_can_compile"),   # Tier-C whole-function mem path
            ("emit_misc", "region_target"),     # tail emit helpers + DOp
        ]}],
    },
    # ---------------------------------------------------------------- compile
    # 12.2k lines: AST -> bytecode. Dominated by one 9k-line
    # `impl<'a> FnCompiler<'a>`; that is the block we partition.
    "compile": {
        "src": "crates/zipp-vm/src/compile.rs",
        "out": "crates/zipp-vm/src/compile",
        "move_impls": [
            # the 1.2k-line program/module-level compiler (the SECOND
            # `impl Compiler` block; the first is a 7-line ctor kept in mod.rs)
            ("compiler", "impl Compiler {", 1),
        ],
        "split_impls": [
            {
                "open": "impl<'a> FnCompiler<'a> {",
                "occurrence": 0,
                "groups": [
                    ("scopes", "new"),              # regs, scopes, locals, consts
                    ("decls", "stmt"),              # statements + var/pattern decls
                    ("funcs", "func_decl"),         # functions, classes, arrows
                    ("control_flow", "branch_stmt"),# if/while/for/try/switch/for-of
                    ("exprs", "expr"),              # expressions, literals, operators
                    ("with_stmt", "with_objs_for"), # `with` scope machinery
                    ("bindings", "load_binding"),   # binding load/store, params, tail calls
                    ("assign", "assign_target"),    # assignment + destructuring targets
                    ("calls", "yield_expr"),        # calls, yield/await, eval, spread
                ],
            }
        ],
        "free_regions": [
            # the pre-pass that rewrites hot `s = s + x` accumulators
            {"start": "is_string_const", "until": "compile_program", "groups": [
                ("string_accum", "is_string_const"),
            ]},
            # the public entry points: compile_program / compile_module /
            # compile_eval (carries the small `impl Compiler` ctor with it)
            {"start": "compile_program", "until": "Compiler", "groups": [
                ("entry", "compile_program"),
            ]},
            # trailing AST/name/param helpers
            {"start": "compound_assign_instr", "groups": [
                ("helpers", "compound_assign_instr"),
            ]},
        ],
    },
    # ------------------------------------------------------------- vm/engine
    "engine": {
        "src": "crates/zipp-vm/src/vm/engine.rs",
        "out": "crates/zipp-vm/src/vm/engine",
        "split_impls": [{"open": "impl<'p> Vm<'p> {", "occurrence": 0, "groups": [
            ("boot", "func"),                       # ctor + jit toggle
            ("jit_plans", "build_ta_pin_plan"),     # TA-pin / leaf / method inline plans
            ("jit_calls", "jit_self_call_impl"),    # native<->interp call bridges
            ("method_inline", "try_method_inline"), # in-region method inlining
            ("jit_frame", "jit_frame_call"),        # frame/prop slow paths
            ("run", "run"),                         # program + eval entry points
            ("modules", "alloc_module_shared_slot"),# ES module graph
            ("eval_prog", "prepare_eval_program"),
        ]}],
    },
    # -------------------------------------------------------------- vm/props
    "props": {
        "src": "crates/zipp-vm/src/vm/props.rs",
        "out": "crates/zipp-vm/src/vm/props",
        "split_impls": [{"open": "impl<'p> Vm<'p> {", "occurrence": 0, "groups": [
            ("proxy_ops", "proxy_target_desc"),
            ("enumerate", "object_enum_own"),
            ("descriptors", "proxy_gopd"),
            ("define", "define_field"),
            ("array_len", "is_extensible"),
            ("member", "proto_member_get"),
        ]}],
    },
    # ---------------------------------------------------------- vm/construct
    "construct": {
        "src": "crates/zipp-vm/src/vm/construct.rs",
        "out": "crates/zipp-vm/src/vm/construct",
        "split_impls": [{"open": "impl<'p> Vm<'p> {", "occurrence": 0, "groups": [
            ("modules_dispose", "build_function"),  # namespaces + explicit resource mgmt
            ("construct", "construct"),             # [[Construct]] + new.target
            ("inherit", "ctor_value"),              # instanceof / super / class ctor
            ("iterate", "object_assign"),           # copy-props + iterator protocol
        ]}],
    },
    # ----------------------------------------------------------- vm/temporal
    "temporal": {
        "src": "crates/zipp-vm/src/vm/temporal.rs",
        "out": "crates/zipp-vm/src/vm/temporal",
        "split_impls": [{"open": "impl<'p> Vm<'p> {", "occurrence": 0, "groups": [
            ("duration", "make_duration"),
            ("plain_date", "make_plain_date"),
            ("plain_time", "make_plain_time"),
            ("plain_date_time", "make_plain_date_time"),
            ("instant_zdt", "now_epoch_ns"),
            ("year_month_day", "make_plain_year_month"),
        ]}],
    },
}


# --------------------------------------------------------------------------
# generic machinery
# --------------------------------------------------------------------------
ITEM_RE = re.compile(
    r"^(pub(\([a-z]+\))? )?((?:async |unsafe |const |extern \"[A-Za-z0-9_-]+\" )*)"
    r"(fn|struct|enum|const|static|type|trait|impl|union) "
)
METHOD_RE_TMPL = r"^    (pub(\([a-z]+\))? )?((?:async |unsafe |const )*)fn {name}\b"
ITEM_RE_TMPL = (
    r"^(pub(\([a-z]+\))? )?((?:async |unsafe |const |extern \"[A-Za-z0-9_-]+\" )*)"
    r"(fn|struct|enum|const|static|type|trait|union) {name}\b"
)
# lines that belong to the item BELOW them (doc comments, attributes, plain
# comment blocks describing the next item)
ATTACH_RE = re.compile(r"^\s*(///|//!|//|#\[)")


def _walk_back(lines, i):
    """Extend a start index upward over the item's doc comments/attributes."""
    while i - 1 >= 0 and ATTACH_RE.match(lines[i - 1]) and lines[i - 1].strip():
        i -= 1
    return i


def _find(lines, pattern, start=0, what="item"):
    pat = re.compile(pattern)
    for i in range(start, len(lines)):
        if pat.match(lines[i]):
            return _walk_back(lines, i)
    raise SystemExit(f"split_rs: {what} not found: /{pattern}/")


def _block_span(lines, open_idx):
    """Return (open_idx, close_idx) for a brace block opening on `open_idx`."""
    depth = 0
    for i in range(open_idx, len(lines)):
        depth += lines[i].count("{") - lines[i].count("}")
        if i > open_idx and depth == 0:
            return open_idx, i
        if i == open_idx and depth == 0:
            raise SystemExit(f"split_rs: no brace opened at line {open_idx+1}")
    raise SystemExit(f"split_rs: unterminated block from line {open_idx+1}")


def _find_impl(lines, open_text, occurrence=0):
    seen = 0
    for i, l in enumerate(lines):
        if l.rstrip("\r\n") == open_text:
            if seen == occurrence:
                return _block_span(lines, i)
            seen += 1
    raise SystemExit(f"split_rs: impl not found: {open_text!r} (occurrence {occurrence})")


def _slice_by_anchors(body, groups, finder, label):
    """Partition `body` into len(groups) contiguous chunks at the anchors."""
    starts = [0]
    for _, anchor in groups[1:]:
        starts.append(finder(body, anchor))
    for a, b in zip(starts, starts[1:]):
        if a >= b:
            raise SystemExit(f"split_rs: {label} anchors out of order ({a} >= {b})")
    chunks = []
    for i, s in enumerate(starts):
        e = starts[i + 1] if i + 1 < len(starts) else len(body)
        chunks.append(body[s:e])
    joined = "".join("".join(c) for c in chunks)
    if joined != "".join(body):
        raise SystemExit(f"split_rs: LOSSY slice in {label}")
    return chunks


def _widen_method(line):
    """An associated item of a split impl must be >= pub(crate) to stay
    reachable: methods, but also associated consts/types, which sibling
    modules reach through `Self::NAME`."""
    m = re.match(
        r"^    (pub(\([a-z]+\))? )?"
        r"((?:async |unsafe |const )*fn |const \w+\s*:|type \w+\s*=)",
        line,
    )
    if not m or m.group(1):
        return line
    return "    pub(crate) " + line[4:]


def _widen_item(line):
    """A moved top-level item must be >= pub(crate) to stay reachable."""
    if re.match(r"^pub(\([a-z]+\))? ", line):
        return line
    if ITEM_RE.match(line) and not line.startswith("impl"):
        return "pub(crate) " + line
    return line


_STRUCT_OPEN_RE = re.compile(r"^(pub(\([a-z]+\))? )?struct \w+.*\{\s*$")
_FIELD_RE = re.compile(r"^    (pub(\([a-z]+\))? )?\w+\s*:")


def _widen_chunk(chunk):
    """Widen a moved free region: column-0 items, the methods of any inline
    `impl` block, and the FIELDS of any moved struct.

    A struct's private fields are only visible inside the module that defines
    it (and its descendants). Once the struct lives in a submodule, its sibling
    modules — which used to be the same module — can no longer read them, so
    every field must become at least `pub(crate)`. Enum variants are left alone
    (a variant is already as visible as its enum)."""
    out = []
    depth = 0
    in_struct = False
    for line in chunk:
        if depth == 0 and _STRUCT_OPEN_RE.match(line):
            in_struct = True
        if in_struct and depth == 1 and _FIELD_RE.match(line) and not re.match(
            r"^    pub", line
        ):
            line = "    pub(crate) " + line[4:]
        elif line.startswith("    "):
            line = _widen_method(line)
        elif depth == 0:
            line = _widen_item(line)
        depth += line.count("{") - line.count("}")
        if depth == 0:
            in_struct = False
        out.append(line)
    return out


def _header():
    return (
        "// Split out of the former monolithic parent file by tools/split_rs.py."
        + NL
        + "// Pure code move: `use super::*` keeps the parent module's imports in"
        + NL
        + "// scope, and items are widened to pub(crate) so the pieces still see"
        + NL
        + "// each other. No logic changed."
        + NL
        + "#![allow(unused_imports)]"
        + NL
        + "use super::*;"
        + NL
        + NL
    )


def split(plan_name, plan, root, dry_run=False):
    src = os.path.join(root, plan["src"])
    out = os.path.join(root, plan["out"])
    with open(src, encoding="utf-8", newline="") as f:
        text = f.read()
    lines = text.splitlines(keepends=True)

    emitted = {}          # module name -> list of lines (final file body)
    moved_regions = []    # (start, end_exclusive) regions removed from the parent
    order = []            # module declaration order
    reexport = []         # modules that export free items (need `use x::*;`);
                          # impl-only modules export nothing, so re-exporting
                          # them would just raise `unused import` warnings.

    # ---- whole-impl moves ------------------------------------------------
    for mod_name, open_text, occurrence in plan.get("move_impls", []):
        o, c = _find_impl(lines, open_text, occurrence)
        o = _walk_back(lines, o)
        body = lines[o : c + 1]
        emitted[mod_name] = [_widen_method(l) for l in body]
        moved_regions.append((o, c + 1))
        order.append(mod_name)

    # ---- one impl partitioned across modules ------------------------------
    for spec in plan.get("split_impls", []):
        o, c = _find_impl(lines, spec["open"], spec.get("occurrence", 0))
        open_line = lines[o]
        body = lines[o + 1 : c]

        def method_finder(b, name):
            return _find(b, METHOD_RE_TMPL.format(name=re.escape(name)), 0,
                         f"method {name}")

        chunks = _slice_by_anchors(body, spec["groups"], method_finder,
                                   f"{plan_name}:{spec['open']}")
        for (mod_name, _), chunk in zip(spec["groups"], chunks):
            widened = [_widen_method(l) for l in chunk]
            emitted[mod_name] = [open_line] + widened + ["}" + NL]
            order.append(mod_name)
        moved_regions.append((o, c + 1))

    # ---- free-item regions -----------------------------------------------
    def item_finder(b, name):
        return _find(b, ITEM_RE_TMPL.format(name=re.escape(name)), 0,
                     f"free item {name}")

    for region in plan.get("free_regions", []):
        rstart = _find(lines, ITEM_RE_TMPL.format(name=re.escape(region["start"])),
                       0, f"region start {region['start']}")
        if region.get("until"):
            rend = _find(lines, ITEM_RE_TMPL.format(name=re.escape(region["until"])),
                         rstart + 1, f"region end {region['until']}")
        else:
            rend = len(lines)
        body = lines[rstart:rend]
        chunks = _slice_by_anchors(body, region["groups"], item_finder,
                                   f"{plan_name}:free@{region['start']}")
        for (mod_name, _), chunk in zip(region["groups"], chunks):
            emitted[mod_name] = _widen_chunk(chunk)
            reexport.append(mod_name)
            order.append(mod_name)
        moved_regions.append((rstart, rend))

    # ---- verify the moved regions are disjoint + rebuild mod.rs ----------
    moved_regions.sort()
    for (a1, b1), (a2, b2) in zip(moved_regions, moved_regions[1:]):
        if b1 > a2:
            raise SystemExit(
                f"split_rs: {plan_name}: overlapping moved regions "
                f"[{a1},{b1}) and [{a2},{b2})"
            )

    kept = []
    cursor = 0
    for a, b in moved_regions:
        kept.extend(lines[cursor:a])
        cursor = b
    kept.extend(lines[cursor:])

    # LOSSLESSNESS: kept + every moved region, in original order == original.
    recomposed = []
    cursor = 0
    for a, b in moved_regions:
        recomposed.extend(lines[cursor:a])
        recomposed.extend(lines[a:b])
        cursor = b
    recomposed.extend(lines[cursor:])
    if "".join(recomposed) != text:
        raise SystemExit(f"split_rs: {plan_name}: LOSSY region partition")

    # `#![...]` inner attributes must stay at the top of mod.rs; they already do
    # because they live in the preamble, which is never moved.
    decls = [NL, "// submodules (split out of the former monolithic "
             + os.path.basename(plan["src"]) + ")" + NL]
    for m in order:
        decls.append(f"mod {m};" + NL)
    if reexport:
        decls.append(NL)
        for m in reexport:
            decls.append(f"pub(crate) use {m}::*;" + NL)

    mod_body = "".join(kept)
    # keep exactly one trailing newline before the decls block
    mod_text = mod_body.rstrip("\r\n") + NL + "".join(decls)

    if dry_run:
        print(f"[dry-run] {plan_name}: mod.rs {len(mod_text.splitlines())} lines, "
              f"{len(emitted)} submodules")
        for m in order:
            print(f"    {m}.rs  {len(emitted[m])} lines")
        return

    os.makedirs(out, exist_ok=True)
    for m in order:
        with open(os.path.join(out, m + ".rs"), "w", encoding="utf-8", newline="") as f:
            f.write(_header())
            f.write("".join(emitted[m]))
    with open(os.path.join(out, "mod.rs"), "w", encoding="utf-8", newline="") as f:
        f.write(mod_text)
    os.remove(src)

    total_moved = sum(len(v) for v in emitted.values())
    print(f"{plan_name}: {len(lines)} lines -> mod.rs "
          f"{len(mod_text.splitlines())} + {len(emitted)} modules "
          f"({total_moved} moved lines)")
    for m in order:
        print(f"    {m}.rs  {len(emitted[m])} lines")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("targets", nargs="*", help="plan names (see --list)")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--root", default=".")
    a = ap.parse_args()
    if a.list:
        for k, v in PLANS.items():
            print(f"{k:12s} {v['src']}")
        return
    names = list(PLANS) if a.all else a.targets
    if not names:
        ap.error("give a target name, --all, or --list")
    for n in names:
        if n not in PLANS:
            sys.exit(f"unknown target {n!r}; try --list")
        split(n, PLANS[n], a.root, a.dry_run)


if __name__ == "__main__":
    main()
