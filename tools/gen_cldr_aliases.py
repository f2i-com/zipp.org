#!/usr/bin/env python3
"""Generate `crates/zipp-vm/src/vm/cldr_alias_data.rs` from upstream CLDR XML.

The tables this emits are the *locale-independent* half of CLDR: the alias
registries UTS #35 §3.2.1 (Canonical Unicode Locale Identifiers) names, and the
likely-subtags table UTS #35 §4.3 names. They carry no translated content and no
per-locale formatting data, so they are the same bytes for every engine that
ships them.

Inputs (a CLDR release checkout, or the four files fetched individually):

    common/supplemental/supplementalMetadata.xml   language/script/territory/
                                                   variant/subdivision aliases
    common/supplemental/likelySubtags.xml          Add/RemoveLikelySubtags
    common/bcp47/*.xml                             -u-/-t- keyword type aliases

Usage:

    python tools/gen_cldr_aliases.py <cldr-root> [-o out.rs]

`<cldr-root>` is the directory holding `common/`; the flat layout produced by
downloading the files side by side is accepted too.
"""

import argparse
import os
import re
import sys

# ── locating the inputs ─────────────────────────────────────────────────────


def find(root, *candidates):
    for c in candidates:
        p = os.path.join(root, c)
        if os.path.exists(p):
            return p
    sys.exit("missing input: none of %s under %s" % (", ".join(candidates), root))


def find_bcp47(root):
    for c in ("common/bcp47", "bcp47", "."):
        p = os.path.join(root, c)
        if os.path.isdir(p) and any(f.endswith(".xml") for f in os.listdir(p)):
            names = sorted(f for f in os.listdir(p) if f.endswith(".xml"))
            # A flat dir also holds the supplemental files; those carry no <key>.
            if any("<key " in open(os.path.join(p, f), encoding="utf-8").read() for f in names):
                return p, names
    sys.exit("missing input: common/bcp47/*.xml under %s" % root)


# ── parsing ─────────────────────────────────────────────────────────────────


def uncomment(src):
    """Drop XML comments.

    Load-bearing, not cosmetic: supplementalMetadata.xml keeps retracted rules
    commented out in place — 478 of its 625 `subdivisionAlias` rows, and
    `<!-- Special case <languageAlias type="sr" replacement="sh"…` — so a naive
    `findall` over the raw text would ship rules CLDR has explicitly withdrawn
    (`sr` → `sh` would break every Serbian tag). The withdrawn rows nest their
    own comment as `<!- -`, so a non-greedy strip is exact.
    """
    return re.sub(r"<!--.*?-->", "", src, flags=re.S)


def alias_rows(src, kind):
    """`<{kind} type="X" replacement="Y" .../>` in document order."""
    return re.findall(r'<%s\s+type="([^"]+)"\s+replacement="([^"]*)"' % kind, src)


def dash(s):
    return s.replace("_", "-")


UVALUE = re.compile(r"^[0-9A-Za-z]{3,8}(-[0-9A-Za-z]{3,8})*$")


def bcp47_type_aliases(path, names):
    """(key, alias, canonical) for every `-u-`/`-t-` type replacement.

    Two shapes carry a replacement, and UTS #35 §3.6.4 treats them alike:
    `<type name="N" alias="A B"/>` (A and B are legacy spellings of N) and
    `<type name="N" deprecated="true" preferred="P"/>` (N itself is retired).

    When a type is BOTH, `preferred` wins for every spelling it lists — that is
    the `ca` calendar case test262 calls out: `<type name="islamicc"
    deprecated="true" preferred="islamic-civil" alias="islamic-civil"/>` reads
    under §3.2.1's letter as "islamic-civil is an alias of islamicc", but the
    Unicode Locale Extension Data Files section makes `islamicc` the retired
    spelling. Reading `alias` on a deprecated type would build the cycle
    islamicc → islamic-civil → islamicc.

    Only spellings that match the `uvalue` production can ever appear inside a
    `-u-` extension, so anything else is dropped rather than shipped dead.
    """
    out = []
    for fname in names:
        src = uncomment(open(os.path.join(path, fname), encoding="utf-8").read())
        # `<key name="ca" …>` and `<key extension="t" name="m0" …>` both occur:
        # the attributes are unordered, so parse them rather than positionally
        # matching. The types belong to the enclosing key.
        for kmatch in re.finditer(r"<key\s([^>]*)>(.*?)(?=<key\s|</keyword>)", src, re.S):
            kattrs = dict(re.findall(r'(\w+)="([^"]*)"', kmatch.group(1)))
            key = kattrs.get("name", "").lower()
            if not key:
                continue
            body = kmatch.group(2)
            for tmatch in re.finditer(r"<type\s([^>]*)/?>", body):
                attrs = dict(re.findall(r'(\w+)="([^"]*)"', tmatch.group(1)))
                name = attrs.get("name")
                if not name:
                    continue
                name = name.lower()
                pref = attrs.get("preferred", "").lower()
                if attrs.get("deprecated") == "true" and pref:
                    for a in [name] + attrs.get("alias", "").lower().split():
                        if a != pref and UVALUE.match(a):
                            out.append((key, a, pref))
                    continue
                for a in attrs.get("alias", "").lower().split():
                    if a != name and UVALUE.match(a):
                        out.append((key, a, name))
    # Deterministic order, and the same alias must not map two ways.
    seen = {}
    rows = []
    for key, a, n in sorted(set(out)):
        if seen.setdefault((key, a), n) != n:
            sys.exit("conflicting bcp47 alias %s/%s -> %s and %s" % (key, a, seen[(key, a)], n))
        rows.append((key, a, n))
    return rows


# ── emitting ────────────────────────────────────────────────────────────────


def blob(name, doc, rows):
    """A `&str` of `\n`-separated records, one record per source line."""
    out = ["%s\npub(crate) static %s: &str = \"\\" % (doc, name)]
    for r in rows:
        out.append("    %s\\n\\" % r)
    out.append('    ";')
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cldr_root")
    ap.add_argument("-o", "--out", default="-")
    ap.add_argument("--cldr-version", default="47",
                    help="release tag the inputs came from; recorded in the header")
    a = ap.parse_args()

    meta = uncomment(open(find(a.cldr_root, "common/supplemental/supplementalMetadata.xml",
                               "supplementalMetadata.xml"), encoding="utf-8").read())
    likely = uncomment(open(find(a.cldr_root, "common/supplemental/likelySubtags.xml",
                                 "likelySubtags.xml"), encoding="utf-8").read())
    bcp47_dir, bcp47_names = find_bcp47(a.cldr_root)
    version = a.cldr_version

    lang = [(dash(t), dash(r)) for t, r in alias_rows(meta, "languageAlias")]
    script = [(t, r) for t, r in alias_rows(meta, "scriptAlias")]
    variant = [(t.lower(), r.lower()) for t, r in alias_rows(meta, "variantAlias")]
    # A multi-valued territory replacement is disambiguated with likelySubtags
    # at lookup time, so the whole list is kept (space separated).
    territory = [(t.upper(), r.upper()) for t, r in alias_rows(meta, "territoryAlias")]
    # `sd`/`rg` take the FIRST replacement only (UTS #35 §3.2.1), so the tail of
    # a multi-valued subdivision row is dead weight and is dropped here. The
    # replacement's CASE is preserved: a row may retire a subdivision in favour
    # of a whole REGION (`fi01` → `AX`, uppercase), which the consumer has to
    # respell as `axzzzz`, and only the case tells the two apart.
    subdiv = [(t.lower(), r.split()[0]) for t, r in alias_rows(meta, "subdivisionAlias")
              if r.split()]

    likely_rows = re.findall(r'<likelySubtag from="([^"]+)" to="([^"]+)"', likely)
    by_shape = {"lang": [], "lang_script": [], "lang_region": [], "und_script": [],
                "und_region": [], "und_script_region": [], "und": []}
    for f, t in likely_rows:
        fp, tp = f.split("_"), t.split("_")
        if len(tp) != 3:
            sys.exit("unexpected likelySubtag target %s -> %s" % (f, t))
        und = fp[0] == "und"
        if len(fp) == 1:
            shape = "und" if und else "lang"
        elif len(fp) == 3:
            shape = "und_script_region"
            if not und:
                sys.exit("unexpected 3-part likelySubtag source %s" % f)
        else:
            second = "script" if (len(fp[1]) == 4 and fp[1].isalpha()) else "region"
            shape = ("und_" if und else "lang_") + second
        by_shape[shape].append((fp, tp))

    # Every non-`und` source repeats its own language in the target, so the
    # target language is elided from those rows (asserted, not assumed).
    for shape in ("lang", "lang_script", "lang_region"):
        for fp, tp in by_shape[shape]:
            if fp[0] != tp[0]:
                sys.exit("likelySubtag %s -> %s changes the language" % ("_".join(fp), "_".join(tp)))
    if len(by_shape["und"]) != 1:
        sys.exit("expected exactly one `und` likelySubtag row")

    parts = []
    parts.append('''//! CLDR alias registries and likely subtags — GENERATED, DO NOT EDIT.
//!
//! Regenerate with `python tools/gen_cldr_aliases.py <cldr-root>`; that script
//! documents the exact upstream files. This is the locale-INDEPENDENT half of
//! CLDR: the registries UTS #35 §3.2.1 uses to canonicalize a
//! `unicode_locale_id`, plus the §4.3 likely-subtags table. No translated
//! content, no per-locale patterns — the same bytes in every engine that ships
//! them.
//!
//! PROVENANCE: CLDR %s (tag `release-%s` of unicode-org/cldr) —
//!   `common/supplemental/supplementalMetadata.xml`  (the five alias registries)
//!   `common/supplemental/likelySubtags.xml`         (likely subtags)
//!   `common/bcp47/*.xml`                            (`-u-`/`-t-` type aliases)
//! The same CLDR version node 24.12 / ICU 77.1 carries, so
//! `tools/gen_cldr_aliases.py`'s output can be checked value-by-value against
//! node's ICU (see `cldr_alias.rs`'s tests for the sampled results).
//!
//! Every table is a `&str` of newline-separated records so the rows stay
//! diffable against the XML they came from; `cldr_alias.rs` indexes them once
//! behind a `OnceLock`.
''' % (version, version))

    parts.append(blob(
        "LANGUAGE_ALIAS",
        "/// `<languageAlias type=\"…\" replacement=\"…\"/>`, `type|replacement`.\n"
        "/// A type may name a whole `unicode_language_id` (`hy-arevmda`,\n"
        "/// `und-hepburn-heploc`); `und` in a type matches any language.",
        ["%s|%s" % r for r in lang]))
    parts.append("")
    parts.append(blob(
        "SCRIPT_ALIAS",
        "/// `<scriptAlias type=\"…\" replacement=\"…\"/>`, `type|replacement`.",
        ["%s|%s" % r for r in script]))
    parts.append("")
    parts.append(blob(
        "TERRITORY_ALIAS",
        "/// `<territoryAlias type=\"…\" replacement=\"…\"/>`, `type|replacement`.\n"
        "/// A replacement holding several regions is resolved with likely\n"
        "/// subtags (UTS #35 §3.2.1); the whole space-separated list is kept.",
        ["%s|%s" % r for r in territory]))
    parts.append("")
    parts.append(blob(
        "VARIANT_ALIAS",
        "/// `<variantAlias type=\"…\" replacement=\"…\"/>`, `type|replacement`.",
        ["%s|%s" % r for r in variant]))
    parts.append("")
    parts.append(blob(
        "SUBDIVISION_ALIAS",
        "/// `<subdivisionAlias type=\"…\" replacement=\"…\"/>`, `type|replacement`,\n"
        "/// reduced to the first replacement — the only one `-u-sd-`/`-u-rg-`\n"
        "/// canonicalization can use. An UPPERCASE replacement is a region, not\n"
        "/// a subdivision (see `cldr_alias.rs`).",
        ["%s|%s" % r for r in subdiv]))
    parts.append("")
    parts.append(blob(
        "BCP47_TYPE_ALIAS",
        "/// `-u-`/`-t-` keyword type aliases from `common/bcp47/*.xml`:\n"
        "/// `key|alias|canonical`. Covers both `<type name=N alias=\"A\"/>` and\n"
        "/// `<type name=N deprecated=\"true\" preferred=P/>`; spellings that\n"
        "/// cannot occur in an extension (not a `uvalue`) are omitted.",
        ["%s|%s|%s" % r for r in bcp47_type_aliases(bcp47_dir, bcp47_names)]))
    parts.append("")

    ls = by_shape["und"][0][1]
    parts.append(
        "/// `<likelySubtag from=\"und\" to=\"%s\"/>` — the root of §4.3.\n"
        "pub(crate) static LIKELY_UND: (&str, &str, &str) = (\"%s\", \"%s\", \"%s\");"
        % ("_".join(ls), ls[0], ls[1], ls[2]))
    parts.append("")
    parts.append(blob(
        "LIKELY_LANG",
        "/// `<likelySubtag from=\"L\" to=\"L_S_R\"/>` as `L S R`. The target\n"
        "/// language always repeats the source, so it is not stored twice.",
        ["%s %s %s" % (fp[0], tp[1], tp[2]) for fp, tp in by_shape["lang"]]))
    parts.append("")
    parts.append(blob(
        "LIKELY_LANG_SCRIPT",
        "/// `<likelySubtag from=\"L_S\" to=\"L_S_R\"/>` as `L S R`.",
        ["%s %s %s" % (fp[0], fp[1], tp[2]) for fp, tp in by_shape["lang_script"]]))
    parts.append("")
    parts.append(blob(
        "LIKELY_LANG_REGION",
        "/// `<likelySubtag from=\"L_R\" to=\"L_S_R\"/>` as `L R S`.",
        ["%s %s %s" % (fp[0], fp[1], tp[1]) for fp, tp in by_shape["lang_region"]]))
    parts.append("")
    parts.append(blob(
        "LIKELY_UND_SCRIPT",
        "/// `<likelySubtag from=\"und_S\" to=\"L_S_R\"/>` as `S L R`.",
        ["%s %s %s" % (fp[1], tp[0], tp[2]) for fp, tp in by_shape["und_script"]]))
    parts.append("")
    parts.append(blob(
        "LIKELY_UND_REGION",
        "/// `<likelySubtag from=\"und_R\" to=\"L_S_R\"/>` as `R L S`.",
        ["%s %s %s" % (fp[1], tp[0], tp[1]) for fp, tp in by_shape["und_region"]]))
    parts.append("")
    parts.append(blob(
        "LIKELY_UND_SCRIPT_REGION",
        "/// `<likelySubtag from=\"und_S_R\" to=\"L_S_R\"/>` as `S R L`.",
        ["%s %s %s" % (fp[1], fp[2], tp[0]) for fp, tp in by_shape["und_script_region"]]))
    parts.append("")

    text = "\n".join(parts)
    if a.out == "-":
        sys.stdout.write(text)
    else:
        with open(a.out, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(text)
        counts = dict(
            language=len(lang), script=len(script), territory=len(territory),
            variant=len(variant), subdivision=len(subdiv),
            likely=len(likely_rows))
        print("wrote %s (CLDR %s) %s" % (a.out, version, counts), file=sys.stderr)


if __name__ == "__main__":
    main()
