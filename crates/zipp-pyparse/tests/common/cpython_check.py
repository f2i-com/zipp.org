"""Compare ZIPP's arena AST dumps with CPython's parser.

    python cpython_check.py cases.jsonl

Each line: {"label", "source", "ours"} where "ours" is the arena dump (see
cpython.rs) or {"error": message}. Prints one line per disagreement: CPython
rejects what ZIPP accepts, or the trees differ (positions are not compared;
number literals are evaluated from their text; adjacent string constants of
a JoinedStr are merged and empty ones dropped on both sides, since ZIPP keeps
the RustPython-compatible split).
"""
import ast
import json
import sys
import unicodedata
import warnings

warnings.simplefilter("ignore")


def norm_joined(values):
    out = []
    for v in values:
        if isinstance(v, list) and v[0] == "Constant" and isinstance(v[1]["value"], str):
            if out and isinstance(out[-1], list) and out[-1][0] == "Constant" and isinstance(out[-1][1]["value"], str):
                out[-1] = ["Constant", {"value": out[-1][1]["value"] + v[1]["value"]}]
                continue
            if v[1]["value"] == "":
                continue
        out.append(v)
    return out


def norm(x):
    """Our dump, normalized: numbers evaluated, JoinedStr constants merged."""
    if isinstance(x, list) and len(x) == 2 and isinstance(x[0], str) and isinstance(x[1], dict):
        name, fields = x
        fields = {k: norm(v) for k, v in fields.items()}
        if name == "JoinedStr":
            fields["values"] = norm_joined(fields["values"])
        if name == "Constant":
            fields = {"value": fields["value"]}
        return [name, fields]
    if isinstance(x, list):
        return [norm(v) for v in x]
    if isinstance(x, dict):
        if "int_text" in x:
            text = x["int_text"].replace("_", "")
            return {"int": str(int(text, 0) if x["radix"] != 10 else int(text))}
        if "float_text" in x:
            return {"float": repr(float(x["float_text"].replace("_", "")))}
        if "complex_text" in x:
            return {"complex": repr(float(x["complex_text"][:-1].replace("_", "")))}
        return {k: norm(v) for k, v in x.items()}
    return x


TARGET_CHECKS = (
    "cannot assign to",
    "cannot delete",
    "illegal expression for augmented assignment",
    "cannot use assignment expressions with",
)

SKIP = {"type_comment", "kind", "lineno", "col_offset", "end_lineno", "end_col_offset", "type_ignores"}


def dump(node):
    if isinstance(node, ast.AST):
        name = type(node).__name__
        fields = {}
        for f in node._fields:
            if f in SKIP:
                continue
            fields[f] = dump(getattr(node, f, None))
        if name == "Constant":
            fields = {"value": const(node.value)}
        if name == "JoinedStr":
            fields["values"] = norm_joined(fields["values"])
        if name == "Module":
            fields["type_ignores"] = []
        return [name, fields]
    if isinstance(node, list):
        return [dump(v) for v in node]
    return node


def const(v):
    if isinstance(v, str):
        # Rust strings cannot hold lone surrogates: ZIPP (like RustPython)
        # reads `\ud800` as U+FFFD.
        if any(0xD800 <= ord(c) <= 0xDFFF for c in v):
            v = "".join("�" if 0xD800 <= ord(c) <= 0xDFFF else c for c in v)
        return v
    if v is None or isinstance(v, bool):
        return v
    if v is Ellipsis:
        return {"ellipsis": 1}
    if isinstance(v, int):
        return {"int": str(v)}
    if isinstance(v, float):
        return {"float": repr(v)}
    if isinstance(v, complex):
        return {"complex": repr(v.imag)}
    if isinstance(v, bytes):
        return {"bytes": v.decode("latin-1")}
    raise TypeError(v)


def quirks(x):
    """Fold the RustPython-compatible readings CPython does not share, on
    both trees: `AnnAssign.simple` (RustPython computed it after dropping a
    target's parentheses), a one-element `match x,:` subject (RustPython
    used the element), a lone starred subscript `a[*b]` (RustPython keeps
    the `Starred` unwrapped) and identifiers, which CPython NFKC-normalizes
    (RustPython, and so ZIPP, keeps them as written: `µ` and `μ` differ)."""
    if isinstance(x, list) and len(x) == 2 and isinstance(x[0], str) and isinstance(x[1], dict):
        name, fields = x
        fields = {k: identifiers(v) if k in IDENTIFIERS else quirks(v) for k, v in fields.items()}
        if name == "AnnAssign":
            fields.pop("simple", None)
        if name == "Match":
            subject = fields["subject"]
            if subject[0] == "Tuple" and len(subject[1]["elts"]) == 1:
                fields["subject"] = subject[1]["elts"][0]
        if name == "Subscript":
            s = fields["slice"]
            if s[0] == "Tuple" and len(s[1]["elts"]) == 1 and s[1]["elts"][0][0] == "Starred":
                fields["slice"] = s[1]["elts"][0]
        return [name, fields]
    if isinstance(x, list):
        return [quirks(v) for v in x]
    return x


IDENTIFIERS = {"id", "arg", "attr", "name", "names", "module", "asname", "rest", "kwd_attrs"}


def identifiers(v):
    if isinstance(v, str):
        return unicodedata.normalize("NFKC", v)
    if isinstance(v, list) and all(isinstance(s, str) for s in v):
        return [unicodedata.normalize("NFKC", s) for s in v]
    return quirks(v)


def main():
    for line in open(sys.argv[1], encoding="utf-8"):
        try:
            case = json.loads(line)
        except json.JSONDecodeError as e:
            print(f"bad dump near ...{line[max(0, e.pos - 150):e.pos + 50]}...")
            continue
        ours = case["ours"]
        label = case["label"]
        if isinstance(ours, dict) and "error" in ours:
            if label.endswith("[if valid]"):
                try:
                    ast.parse(case["source"])
                except (SyntaxError, ValueError, UnicodeError):
                    continue
                if "knows common names only" in ours["error"]:
                    continue  # the compact \N{...} table, by design
                print(f"{label}: CPython accepts what ZIPP rejects ({ours['error']})")
            continue
        try:
            tree = ast.parse(case["source"])
        except (SyntaxError, ValueError, UnicodeError) as e:
            msg = getattr(e, "msg", str(e))
            # Assignment targets are checked by ZIPP's compiler, as they were
            # with the RustPython parser; CPython's parser checks them itself.
            if ours != "reject" and not any(t in msg for t in TARGET_CHECKS):
                print(f"{label}: CPython rejects ({msg}, line {getattr(e, 'lineno', '?')}) what ZIPP accepts")
            continue
        if ours == "reject":
            print(f"{label}: CPython accepts what ZIPP rejects")
            continue
        if ours is None:
            continue
        theirs = quirks(dump(tree))
        mine = quirks(norm(ours))
        mine[1]["type_ignores"] = []
        if theirs != mine:
            a = json.dumps(theirs)
            b = json.dumps(mine)
            i = next((k for k in range(min(len(a), len(b))) if a[k] != b[k]), min(len(a), len(b)))
            print(f"{label}: trees differ near ...{a[max(0, i - 80):i + 80]}... vs ...{b[max(0, i - 80):i + 80]}...")


main()
