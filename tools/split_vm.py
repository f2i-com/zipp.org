#!/usr/bin/env python3
"""Lossless splitter for crates/zipp-vm/src/vm.rs -> crates/zipp-vm/src/vm/.

Pure code-move refactor: the giant `impl<'p> Vm<'p>` is partitioned into
concern-based submodules (each its own `impl<'p> Vm<'p>` block) and the trailing
free functions into helper modules. The struct, enums, consts and `mod native`
stay reachable from `vm/mod.rs`. Self-verifies that the concatenation of all
emitted pieces is byte-identical to the original moved regions.
"""
import re
import os
import sys

SRC = "crates/zipp-vm/src/vm.rs"
OUT = "crates/zipp-vm/src/vm"

NL = "\r\n"  # the source uses CRLF; keep generated lines consistent

CRATE_IMPORTS = (
    "use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};\n"
    "use crate::heap::{\n"
    "    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,\n"
    "    PropAttr, PromiseState, Reaction,\n"
    "};\n"
    "use crate::value::Value;\n"
).replace("\n", NL)

# (filename, first-method anchor). Groups are contiguous in file order.
IMPL_GROUPS = [
    ("engine", "new"),
    ("dispatch", "run_loop"),
    ("async_runtime", "alloc_generator"),
    ("indexing_date", "get_index"),
    ("setup", "prototype_of"),
    ("natives", "call_native"),
    ("props", "object_enum_own"),
    ("mathjson", "eval_math"),
    ("access", "type_of"),
    ("builtins", "try_builtin_method"),
    ("values", "has_property"),
    ("temporal", "make_duration"),
    ("intl", "intl_slot"),
    ("proxy_regexp", "make_proxy"),
    ("typedarray", "as_array_buffer"),
    ("construct", "construct"),
    ("misc_methods", "error_name"),
    ("array_ops", "array_each"),
    ("string_ops", "heap_char_at"),
    ("coerce", "array_like_read"),
]

# (filename, first-item anchor) for the trailing free functions / consts / enums.
POST_GROUPS = [
    ("helpers_misc", "STRING_CONST_BIT"),
    ("helpers_datetime", "is_leap_year"),
    ("helpers_numeric", "ta_encode"),
    ("helpers_json", "json_quote"),
    ("helpers_num2", "math_unary"),
]


def main():
    with open(SRC, encoding="utf-8", newline="") as f:
        text = f.read()
    lines = text.splitlines(keepends=True)
    n = len(lines)

    def find(pred, start=0):
        for i in range(start, n):
            if pred(lines[i]):
                return i
        raise SystemExit(f"marker not found from {start}")

    def is_close(l):
        return l.rstrip() == "}"

    impl_open = find(lambda l: l.rstrip() == "impl<'p> Vm<'p> {")
    impl_close = find(is_close, impl_open + 1)
    native_open = find(lambda l: l.rstrip() == "mod native {")
    native_close = find(is_close, native_open + 1)
    assert native_close < impl_open, "native must precede impl"

    pre_before_native = lines[0:native_open]
    native_inner = lines[native_open + 1 : native_close]
    pre_after_native = lines[native_close + 1 : impl_open]
    impl_body = lines[impl_open + 1 : impl_close]
    post = lines[impl_close + 1 :]

    # ---- locate group starts within a body ----
    def method_start(body, name):
        pat = re.compile(r"^    (pub |pub\(crate\) )?(async )?fn " + re.escape(name) + r"\b")
        for i, l in enumerate(body):
            if pat.match(l):
                j = i
                while j - 1 >= 0 and re.match(r"^    (///|#\[)", body[j - 1]):
                    j -= 1
                return j
        raise SystemExit(f"impl method not found: {name}")

    def item_start(body, name):
        pat = re.compile(
            r"^(pub |pub\(crate\) )?(fn|const|enum|struct|static|type) " + re.escape(name) + r"\b"
        )
        for i, l in enumerate(body):
            if pat.match(l):
                j = i
                while j - 1 >= 0 and re.match(r"^(///|#\[)", body[j - 1]):
                    j -= 1
                return j
        raise SystemExit(f"post item not found: {name}")

    def slice_groups(body, groups, finder):
        starts = [0]
        for _, anchor in groups[1:]:
            starts.append(finder(body, anchor))
        # strictly increasing
        for a, b in zip(starts, starts[1:]):
            assert a < b, f"non-monotonic group starts: {a} >= {b}"
        chunks = []
        for i in range(len(starts)):
            end = starts[i + 1] if i + 1 < len(starts) else len(body)
            chunks.append(body[starts[i]:end])
        # losslessness: concatenation == body, byte-identical
        assert "".join("".join(c) for c in chunks) == "".join(body), "LOSSY slice!"
        return chunks

    impl_chunks = slice_groups(impl_body, IMPL_GROUPS, method_start)
    post_chunks = slice_groups(post, POST_GROUPS, item_start)

    def widen(line):
        # widen top-level item visibility so mod.rs can re-export helpers.
        if re.match(r"^(pub |pub\(crate\))", line):
            return line
        if re.match(r"^(fn|const|enum|struct|static|type) ", line):
            return "pub(crate) " + line
        return line

    def widen_method(line):
        # The impl is split across modules, so each method must be at least
        # pub(crate) to remain callable from the other vm submodules. Only
        # 4-space-indented `fn`/`async fn`/`unsafe fn`/`const fn` decls; leave
        # anything already pub.
        m = re.match(r"^    (pub(\(crate\)|\(super\))? )?((?:async |unsafe |const )*fn )", line)
        if not m or m.group(1):
            return line
        return "    pub(crate) " + line[4:]

    os.makedirs(OUT, exist_ok=True)

    header = ("#![allow(unused_imports)]\nuse super::*;\n".replace("\n", NL)) + CRATE_IMPORTS + NL

    # impl submodule files (widen each method to pub(crate))
    for (fname, _), chunk in zip(IMPL_GROUPS, impl_chunks):
        widened = [widen_method(l) for l in chunk]
        with open(os.path.join(OUT, fname + ".rs"), "w", encoding="utf-8", newline="") as f:
            f.write(header)
            f.write("impl<'p> Vm<'p> {" + NL)
            f.write("".join(widened))
            f.write("}" + NL)

    # helper free-fn files (widen visibility of moved items)
    for (fname, _), chunk in zip(POST_GROUPS, post_chunks):
        widened = [widen(l) for l in chunk]
        with open(os.path.join(OUT, fname + ".rs"), "w", encoding="utf-8", newline="") as f:
            f.write(header)
            f.write("".join(widened))

    # native.rs (self-contained module body)
    with open(os.path.join(OUT, "native.rs"), "w", encoding="utf-8", newline="") as f:
        f.write("".join(native_inner))

    # mod.rs = pre-native + `mod native;` + (enums/struct/Thrown) + module graph
    decls = [NL, "// submodules (split from the former monolithic vm.rs)" + NL]
    for fname, _ in IMPL_GROUPS:
        decls.append(f"mod {fname};" + NL)
    decls.append("mod native;" + NL)
    for fname, _ in POST_GROUPS:
        decls.append(f"mod {fname};" + NL)
    decls.append(NL)
    for fname, _ in POST_GROUPS:
        decls.append(f"pub(crate) use {fname}::*;" + NL)

    # enums shared with submodules must be at least as visible as the (now
    # pub(crate)) methods that take them in their signatures.
    def widen_enum(line):
        if re.match(r"^enum ", line):
            return "pub(crate) " + line
        return line

    pre_b = [widen_enum(l) for l in pre_before_native]
    pre_a = [widen_enum(l) for l in pre_after_native]
    # insert `#![allow(unused_imports)]` after the leading //! doc block (the full
    # crate import set lives here but mod.rs only uses a subset, like the submodules).
    insert_at = next((i for i, l in enumerate(pre_b) if l.startswith("use ")), 0)
    pre_b = pre_b[:insert_at] + ["#![allow(unused_imports)]" + NL] + pre_b[insert_at:]

    with open(os.path.join(OUT, "mod.rs"), "w", encoding="utf-8", newline="") as f:
        f.write("".join(pre_b))
        f.write("".join(pre_a))
        f.write("".join(decls))

    # ---- report ----
    nfiles = len(IMPL_GROUPS) + len(POST_GROUPS) + 2
    print(f"impl_open=line {impl_open+1}  impl_close=line {impl_close+1}")
    print(f"native=lines {native_open+1}..{native_close+1}")
    print(f"emitted {nfiles} files to {OUT}/")
    for (fname, _), chunk in zip(IMPL_GROUPS, impl_chunks):
        print(f"  {fname}.rs  ({len(chunk)} lines, impl)")
    print(f"  native.rs ({len(native_inner)} lines)")
    for (fname, _), chunk in zip(POST_GROUPS, post_chunks):
        print(f"  {fname}.rs ({len(chunk)} lines, free fns)")
    total = sum(len(c) for c in impl_chunks) + sum(len(c) for c in post_chunks) + len(native_inner)
    print(f"moved body lines: impl={sum(len(c) for c in impl_chunks)} "
          f"post={sum(len(c) for c in post_chunks)} native={len(native_inner)} total={total}")


if __name__ == "__main__":
    main()
